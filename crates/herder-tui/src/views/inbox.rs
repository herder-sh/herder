//! The inbox in the main pane: each request waiting on the user, newest first, with its
//! session, why it is the user's and what the primary session said; under it, the answer
//! being typed, in a prompt.
//!
//! ```text
//!  inbox · 2 waiting on you                         every machine · newest first
//!
//! ▶ ◉ question · docs  app › api                                      box · 2m
//!     escalated: the primary session left it to you
//!     primary's note: "I don't know the house style; ask."
//!     Which heading level should the API page use?
//!       1 h2 under Reference
//!       2 h1, its own page
//! ```

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::{App, Focus};
use crate::inbox::{Waiting, What};
use crate::mouse::{Click, Hits, List as Rows, Wheel};
use crate::session::reason_text;
use crate::ui::Ui;
use crate::ui::glyphs::Glyphs;
use crate::ui::input::Prompt;
use crate::ui::list::{ListView, Row};
use crate::ui::state::{self, State};

/// Lines an answer shows before it scrolls.
const ANSWER_LINES: u16 = 4;

/// `compact`, on a phone, leaves the heading to the phone's header.
pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App, compact: bool, hits: &mut Hits) {
    let theme = app.theme.clone();
    let ui = Ui::new(&theme, Glyphs::for_width(app.glyphs, app.width));
    let waiting = app.waiting();
    let area = super::heading(
        frame,
        area,
        ui,
        "inbox",
        &format!("{} waiting on you", waiting.len()),
        "every machine · newest first",
        compact,
    );
    if waiting.is_empty() {
        super::nothing(frame, area, ui, "Nothing is waiting on you.");
        return;
    }
    let answer_height = app.inbox.answer.as_ref().map_or(0, |answer| {
        Prompt::height(answer, area.width, ANSWER_LINES, true)
    });
    let [list_area, _, answer_area] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(u16::from(answer_height > 0)),
        Constraint::Length(answer_height),
    ])
    .areas(area);
    let selected = app.inbox_index(&waiting);
    // Past the pointer and the status dot, and the right inset.
    let width = usize::from(list_area.width.saturating_sub(5)).max(8);
    let mut rows = Vec::new();
    // The row of each request.
    let mut items = Vec::new();
    for waiting in &waiting {
        if !rows.is_empty() {
            rows.push(Row::Gap);
        }
        items.push(rows.len());
        rows.push(entry(app, ui, waiting, width, compact));
    }
    let task = waiting
        .get(selected)
        .map(|waiting| waiting.session.short_title())
        .unwrap_or_default();
    let mut offset = 0;
    let placed = ListView::new(ui, rows)
        .select(items.get(selected).copied())
        .focused(app.focus == Focus::Inbox && app.inbox.answer.is_none())
        .render(list_area, frame.buffer_mut(), &mut offset);
    hits.wheel(list_area, Wheel::Keys);
    for (row, rect) in placed {
        if let Some(at) = items.iter().position(|item| *item == row) {
            hits.click(rect, Click::Row(Rows::Inbox, at));
        }
    }
    if let Some(answer) = &app.inbox.answer {
        Prompt::new(ui, answer)
            .focused(true)
            .accent(theme.attention)
            .placeholder("Type an answer…")
            .meta(Line::from(ui.joined([
                Span::styled("answer", ui.muted()),
                Span::styled(task, ui.text()),
            ])))
            .render(answer_area, frame.buffer_mut());
    }
}

/// One request: what it is and whose, with its machine and age at the right; under it why it
/// is the user's, the primary's note, then the request itself.
fn entry(app: &App, ui: Ui, waiting: &Waiting<'_>, width: usize, compact: bool) -> Row<'static> {
    let (mark, kind) = match waiting.what {
        What::Approval(_) => (ui.glyphs.approval, "approval"),
        What::Question(_) => (ui.glyphs.question, "question"),
    };
    let attention = state::style(ui, State::NeedsYou);
    let mut left = vec![
        Span::styled(mark, attention),
        Span::raw(" "),
        Span::styled(kind, attention),
        Span::styled(ui.glyphs.separator, ui.muted()),
        Span::styled(waiting.session.short_title(), ui.text()),
    ];
    // The task path, where a row has room for it; on a phone it goes under.
    let path = app.primary(waiting.key).map(|(_, primary)| {
        format!(
            "{} {} {}",
            primary.short_title(),
            ui.glyphs.choice[1],
            waiting.session.short_title()
        )
    });
    if let Some(path) = &path
        && !compact
    {
        left.push(Span::styled(format!("  {path}"), ui.muted()));
    }
    let machine = app
        .machines
        .iter()
        .find(|machine| machine.host_id == waiting.key.host_id)
        .map_or_else(|| waiting.key.host_id.to_string(), |m| m.name.clone());
    let (since, reason, note) = match waiting.what {
        What::Approval(approval) => (approval.since, approval.reason, approval.note.as_deref()),
        What::Question(question) => (question.since, question.reason, question.note.as_deref()),
    };
    let right = Line::from(ui.joined([
        Span::styled(machine, ui.muted()),
        Span::styled(ago(app.now().as_second() - since.as_second()), ui.muted()),
    ]));

    let mut body = Vec::new();
    if let Some(path) = path.filter(|_| compact) {
        wrap(&mut body, &path, ui.muted(), width);
    }
    if let Some(reason) = reason {
        wrap(
            &mut body,
            &format!("escalated: {}", reason_text(reason)),
            ui.muted(),
            width,
        );
    }
    if let Some(note) = note {
        wrap(
            &mut body,
            &format!("primary's note: \u{201c}{note}\u{201d}"),
            ui.muted().add_modifier(Modifier::ITALIC),
            width,
        );
    }
    match waiting.what {
        What::Approval(approval) => wrap(&mut body, &approval.summary, ui.text(), width),
        What::Question(question) => {
            wrap(&mut body, &question.text, ui.text(), width);
            for (at, choice) in question.choices.iter().enumerate() {
                let mut lines = Vec::new();
                wrap(&mut lines, choice, ui.text(), width.saturating_sub(4));
                for (row, mut line) in lines.into_iter().enumerate() {
                    let number = if row == 0 {
                        format!("  {} ", at + 1)
                    } else {
                        "    ".to_owned()
                    };
                    line.spans.insert(0, Span::styled(number, ui.accent()));
                    body.push(line);
                }
            }
        }
    }
    Row::item(Line::from(left)).right(right).body(body)
}

/// `text`, wrapped `width` wide, one line per row.
fn wrap(out: &mut Vec<Line<'static>>, text: &str, style: Style, width: usize) {
    for line in text.lines() {
        for part in textwrap::wrap(line, width.max(8)) {
            out.push(Line::styled(part.into_owned(), style));
        }
    }
}

/// How long ago, `seconds` back, in its largest unit or two: `12s`, `2m`, `1h 5m`.
pub(super) fn ago(seconds: i64) -> String {
    if seconds < 60 {
        format!("{}s", seconds.max(0))
    } else {
        crate::account_screen::until(seconds)
    }
}
