//! The inbox in the main pane: each request waiting on the user, newest first, with its
//! session, why it is the user's and what the primary session said; under it, the answer
//! being typed.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, List, ListItem, ListState};

use crate::app::{App, Focus};
use crate::inbox::{Waiting, What};
use crate::mouse::{Click, Hits, List as Rows, Wheel};
use crate::session::reason_text;

/// Marks the selected request.
const MARK: &str = "▶ ";

/// `compact`, on a narrow screen, keeps the hints short.
pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App, compact: bool, hits: &mut Hits) {
    let (list_area, answer_area) = match app.inbox.answer {
        Some(_) => {
            let [list, answer] =
                Layout::vertical([Constraint::Fill(1), Constraint::Length(3)]).areas(area);
            (list, Some(answer))
        }
        None => (area, None),
    };
    let waiting = app.waiting();
    let hint = match (app.inbox.answer.is_some(), compact) {
        (true, false) => " Enter send · Esc cancel ",
        (true, true) => " Enter send · ⌫ cancel ",
        (false, false) => " y allow · n deny · 1-9 pick · Enter answer · l session · Esc back ",
        (false, true) => " y/n · 1-9 · Enter answer · l open · ⌫ back ",
    };
    let block = Block::bordered()
        .title(Line::from(vec![
            Span::styled(" inbox ", super::bold()),
            Span::styled(format!("· {} waiting on you ", waiting.len()), super::dim()),
        ]))
        .title_bottom(Line::styled(hint, super::dim()).right_aligned())
        .border_style(super::border(app, Focus::Inbox));
    if waiting.is_empty() {
        let inner = block.inner(list_area);
        frame.render_widget(block, list_area);
        let hint = Line::styled("Nothing is waiting on you.", super::dim());
        frame.render_widget(hint.centered(), super::centered(inner, inner.width, 1));
        return;
    }
    let width = usize::from(list_area.width.saturating_sub(2 + 2)).max(8);
    let list_len = waiting.len();
    let items: Vec<ListItem> = waiting
        .iter()
        .enumerate()
        .map(|(at, waiting)| ListItem::new(entry(app, waiting, width, at + 1 < list_len)))
        .collect();
    let heights: Vec<usize> = items.iter().map(ListItem::height).collect();
    let mut state = ListState::default();
    state.select(Some(app.inbox_index(&waiting)));
    let task = waiting
        .get(app.inbox_index(&waiting))
        .map(|waiting| waiting.session.title())
        .unwrap_or_default();
    let list = List::new(items)
        .block(block)
        .highlight_symbol(MARK)
        .highlight_style(Style::new().add_modifier(Modifier::BOLD));
    frame.render_stateful_widget(list, list_area, &mut state);
    hits.wheel(list_area, Wheel::Keys);
    hits.list(
        list_area.inner(Margin::new(1, 1)),
        state.offset(),
        &heights,
        |at| Some(Click::Row(Rows::Inbox, at)),
    );
    if let (Some(area), Some(answer)) = (answer_area, &mut app.inbox.answer) {
        answer.set_cursor_style(Style::new().add_modifier(Modifier::REVERSED));
        answer.set_block(
            Block::bordered()
                .border_style(Style::new().fg(Color::Cyan))
                .title(Line::styled(format!(" answer · {task} "), attention())),
        );
        frame.render_widget(&*answer, area);
    }
}

/// One request: what it is and whose, the request itself, then why it is the user's; `gap`
/// adds a blank line after it.
fn entry(app: &App, waiting: &Waiting<'_>, width: usize, gap: bool) -> Text<'static> {
    let mut lines = Vec::new();
    let kind = match waiting.what {
        What::Approval(_) => "approval",
        What::Question(_) => "question",
    };
    let machine = app
        .machines
        .iter()
        .find(|machine| machine.host_id == waiting.key.host_id)
        .map_or_else(|| waiting.key.host_id.to_string(), |m| m.name.clone());
    let mut heading = vec![
        Span::styled(format!("{kind:<9}"), attention()),
        Span::styled(format!("{machine} · "), super::dim()),
    ];
    if let Some((_, primary)) = app.primary(waiting.key) {
        heading.push(Span::styled(
            format!("{} › ", primary.title()),
            super::dim(),
        ));
    }
    heading.push(Span::styled(waiting.session.title(), super::bold()));
    lines.push(Line::from(heading));
    let (reason, note) = match waiting.what {
        What::Approval(approval) => {
            wrap(&mut lines, &approval.summary, Style::new(), width);
            (approval.reason, approval.note.as_deref())
        }
        What::Question(question) => {
            wrap(&mut lines, &question.text, Style::new(), width);
            for (at, choice) in question.choices.iter().enumerate() {
                wrap(
                    &mut lines,
                    &format!("{}. {choice}", at + 1),
                    Style::new(),
                    width,
                );
            }
            (question.reason, question.note.as_deref())
        }
    };
    if let Some(reason) = reason {
        wrap(&mut lines, reason_text(reason), super::dim(), width);
    }
    if let Some(note) = note {
        let style = Style::new().fg(Color::Cyan).add_modifier(Modifier::ITALIC);
        wrap(
            &mut lines,
            &format!("the primary says: {note}"),
            style,
            width,
        );
    }
    if gap {
        lines.push(Line::raw(""));
    }
    Text::from(lines)
}

fn wrap(out: &mut Vec<Line<'static>>, text: &str, style: Style, width: usize) {
    let options = textwrap::Options::new(width)
        .initial_indent("  ")
        .subsequent_indent("  ");
    for line in text.lines() {
        for part in textwrap::wrap(line, &options) {
            out.push(Line::styled(part.into_owned(), style));
        }
    }
}

fn attention() -> Style {
    Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD)
}
