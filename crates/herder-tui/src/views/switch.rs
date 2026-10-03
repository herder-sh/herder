//! The switch picker, over the main screen: the session's machine's accounts, those of its
//! provider apart from those that replay the transcript, each with its busiest usage window;
//! then the model, with the models the machine's sessions use; then what the switch does.

use herder_protocol::Account;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::account_screen;
use crate::action::Action;
use crate::app::App;
use crate::mouse::{Click, Hits, List as Rows};
use crate::palette::search_line;
use crate::session::Session;
use crate::switch::{self, Input, Kind};
use crate::ui::dialog::Size;
use crate::ui::glyphs::Glyphs;
use crate::ui::hints::Hint;
use crate::ui::input::Field;
use crate::ui::list::Row;
use crate::ui::select::{Select, label_width};
use crate::ui::{GAP, Ui};

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App, hits: &mut Hits) {
    let Some(switch) = &app.switch else {
        return;
    };
    let Some(session) = app.sessions.get(&switch.session) else {
        return;
    };
    let theme = app.theme.clone();
    let ui = Ui::new(&theme, Glyphs::for_width(app.glyphs, app.width));
    let accounts = app.accounts_of(&switch.session);
    let order = app.switch_rows();
    let cursor = switch.selected.min(order.len().saturating_sub(1));
    let chosen = order.get(cursor).and_then(|at| accounts.get(*at));
    let narrow = area.width < super::NARROW;

    // The list: by kind, a header over each.
    let mut rows = Vec::new();
    let mut items = Vec::new();
    let mut last = None;
    for at in &order {
        let account = &accounts[*at];
        let group = switch::kind(session, account) == Kind::Provider;
        if last != Some(group) {
            if last.is_some() {
                rows.push(Row::Gap);
            }
            let (title, note) = if group {
                ("other provider", "replays the transcript")
            } else {
                ("same provider", "conversation continues")
            };
            rows.push(Row::header(title).right(note));
            last = Some(group);
        }
        items.push(rows.len());
        rows.push(row(ui, session, account, narrow));
    }
    let selected = items.get(cursor).copied();

    // Under it: the model, the models in use, what the switch does, and why it was refused.
    let recent = app.switch_recent();
    let placeholder = match chosen.map(|account| switch::kind(session, account)) {
        Some(Kind::Provider) => "the provider's default".to_owned(),
        _ if session.model.is_empty() => "keep the current model".to_owned(),
        _ => format!("keep {}", session.model),
    };
    let mut footer = vec![Line::default()];
    if !recent.is_empty() {
        let mut spans = vec![Span::styled(
            // Under the model field's text, past its label and padding.
            format!(
                "{:<w$}",
                "recent",
                w = usize::from(label_width("search")) + 1
            ),
            ui.muted(),
        )];
        spans.extend(
            ui.joined(
                recent
                    .iter()
                    .map(|model| Span::styled(model.clone(), ui.text())),
            ),
        );
        footer.push(Line::from(spans));
    }
    let mut notes = Vec::new();
    let room = usize::from(
        Size::Medium
            .width()
            .min(area.width)
            .saturating_sub(2 + 2 * crate::ui::dialog::PAD_X),
    )
    .max(8);
    let mut wrapped = |text: &str, style: Style| {
        for line in textwrap::wrap(text, room) {
            notes.push(Line::styled(line.into_owned(), style));
        }
    };
    if let Some(account) = chosen {
        wrapped(&what(session, account), ui.muted());
    }
    if let Some(error) = &switch.error {
        wrapped(error, Style::new().fg(theme.error));
    }
    if !notes.is_empty() {
        footer.push(Line::default());
        footer.extend(notes);
    }

    let editing = switch.editing;
    let hints = if editing {
        vec![
            Hint::new("enter", "switch"),
            Hint::new("tab", "accounts"),
            Hint::new("esc", "back"),
        ]
    } else {
        vec![
            Hint::new("enter", "switch"),
            Hint::new("tab", "model"),
            Hint::new("esc", "close"),
        ]
    };
    let title = Line::from(ui.joined([Span::raw("switch"), Span::raw(session.short_title())]));
    let empty = if accounts.is_empty() {
        "this machine has no accounts: A shows them"
    } else {
        "no account matches"
    };
    let Some(switch) = &mut app.switch else {
        return;
    };
    let mut search = search_line(&switch.search, "type to filter");
    let mut model = search_line(&switch.model, &placeholder);
    let placed = Select::new(ui, title, Size::Medium, &mut search)
        .searching(!editing)
        .rows(rows, selected)
        .empty(empty)
        .footer(footer)
        .hints(if narrow { &hints[..2] } else { &hints })
        .render(area, frame.buffer_mut(), &mut switch.offset);
    super::palette::dialog_taps(hits, area, &placed.dialog);
    hits.click(placed.search, Click::Act(Action::Switch(Input::Accounts)));
    if let Some((_, model_row)) = placed.footer.first() {
        Field::new(ui, "model", &mut model).focused(editing).render(
            *model_row,
            frame.buffer_mut(),
            label_width("search"),
        );
        hits.click(*model_row, Click::Act(Action::Switch(Input::EditModel)));
    }
    // Each recent model, tapped, is taken.
    if let Some((_, recent_row)) = placed.footer.get(1).filter(|_| !recent.is_empty()) {
        let mut x = recent_row.x + label_width("search") + 1;
        let separator = u16::try_from(crate::ui::width(ui.glyphs.separator)).unwrap_or(3);
        for (at, model) in recent.iter().enumerate() {
            let width = u16::try_from(crate::ui::width(model)).unwrap_or(u16::MAX);
            let rect = Rect::new(x, recent_row.y, width, 1).intersection(*recent_row);
            hits.click(rect, Click::Act(Action::Switch(Input::Recent(at))));
            x = x.saturating_add(width + separator);
        }
    }
    for (row, rect) in placed.rows {
        if let Some(at) = items.iter().position(|item| *item == row) {
            hits.click(rect, Click::Row(Rows::Switch, at));
        }
    }
}

/// An account to pick: marked if the session is on it, its label, and its busiest window.
fn row<'a>(ui: Ui, session: &Session, account: &Account, narrow: bool) -> Row<'a> {
    let current = switch::kind(session, account) == Kind::Current;
    let mark = if current {
        Span::styled(ui.glyphs.connected, ui.accent())
    } else {
        Span::raw(" ")
    };
    let mut left = vec![
        mark,
        Span::raw(" "),
        Span::styled(account.label.clone(), ui.text()),
    ];
    if !narrow && account.label != account.account_id.as_str() {
        left.push(Span::raw("  "));
        left.push(Span::styled(account.account_id.to_string(), ui.muted()));
    }
    let mut right = Vec::new();
    if let Some(usage) = account
        .usage
        .iter()
        .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
    {
        // Rounded, then held to 0–100, so the cast cannot truncate.
        let percent = usage.used_percent.round().clamp(0.0, 100.0) as u8;
        right.push(Span::styled(
            format!("{} ", account_screen::window_label(&usage.window)),
            ui.muted(),
        ));
        right.push(Span::styled(
            format!("{percent:>3}%"),
            Style::new().fg(ui.theme.usage(percent)),
        ));
    }
    if current && !narrow {
        right.push(Span::styled(
            format!("{}current", " ".repeat(GAP)),
            ui.muted(),
        ));
    }
    Row::item(Line::from(left)).right(Line::from(right))
}

/// What switching `session` to `account` does.
fn what(session: &Session, account: &Account) -> String {
    match switch::kind(session, account) {
        Kind::Current => "The session is on this account: a model changes only the model.".into(),
        Kind::Account => format!(
            "Moves to {} between turns; the conversation carries on.",
            account.label
        ),
        Kind::Provider => format!(
            "Moves to {} between turns, replaying the transcript there.",
            account.provider.as_str()
        ),
    }
}
