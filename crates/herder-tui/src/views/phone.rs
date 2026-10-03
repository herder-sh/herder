//! The phone's header, as Herdr's mobile layout: two rows over a rule, with the `switch`
//! button at the right of both.
//!
//! ```text
//!  * api - app - box              2/6 │ switch
//!  ! 2 need you - 3 running - 1 done  │   !
//! ────────────────────────────────────────────
//! ```
//!
//! The first row names what is in view: the open session, its project and machine, and where
//! it is among all sessions; or a view, with `< back`. The second sums up every session's
//! state, and any machine that is offline. `switch` opens the switcher, carrying the
//! needs-you mark while anything needs the user; Shift-Tab from an unfocused button bar
//! reaches it first. In the switcher, the header reads `switch` and `close x`.

use herder_client_core::ConnectionState;
use ratatui::Frame;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::action::Action;
use crate::app::{App, Focus};
use crate::mouse::{self, Click, Hits};
use crate::ui::state::{self, State};
use crate::ui::{INSET, Ui, fit, line_width};

/// Columns of the `│ switch` button.
const SWITCH: u16 = 8;

/// Whether the header shows the `switch` button: everywhere but the switcher itself.
pub(super) fn has_switch(app: &App) -> bool {
    app.focus != Focus::Sessions && !app.machines.is_empty() && !app.dialog_open()
}

/// The header in `area`'s two rows; `focused` draws the switch button as Tab's focus.
pub(super) fn header(frame: &mut Frame, area: Rect, app: &App, focused: bool, hits: &mut Hits) {
    let ui = app.ui();
    let rows = |y| Rect::new(area.x, area.y + y, area.width, 1);
    let (top, bottom) = (rows(0), rows(1));
    if app.machines.is_empty() {
        put(
            frame,
            top,
            ui,
            Line::styled("herder", ui.strong()),
            Line::default(),
        );
        return;
    }
    let (top, bottom) = if has_switch(app) {
        let left = area.width.saturating_sub(SWITCH + 1);
        let button = Rect::new(area.x + left + 1, area.y, SWITCH, 2);
        switch(frame, button, app, focused);
        hits.click(
            Rect {
                height: button.height + 1,
                ..button
            },
            Click::Act(Action::GoTo),
        );
        (
            Rect { width: left, ..top },
            Rect {
                width: left,
                ..bottom
            },
        )
    } else {
        (top, bottom)
    };

    let (title, right, back) = match app.focus {
        // A dialog's back closes it.
        _ if app.dialog_open() => (
            Line::styled(dialog_title(app), ui.strong()),
            Some(Span::styled(ui.glyphs.back, ui.accent())),
            true,
        ),
        Focus::Sessions => {
            let close = app
                .open
                .is_some()
                .then(|| Span::styled("close x", ui.accent()));
            (Line::styled("switch", ui.strong()), close, false)
        }
        Focus::Inbox => (
            Line::styled(format!("inbox {}", app.waiting().len()), ui.strong()),
            Some(Span::styled(ui.glyphs.back, ui.accent())),
            true,
        ),
        Focus::AllPrs => (
            Line::styled(format!("prs {}", app.all_prs().len()), ui.strong()),
            Some(Span::styled(ui.glyphs.back, ui.accent())),
            true,
        ),
        _ => {
            let position = app
                .position()
                .map(|(at, of)| Span::styled(format!("{at}/{of}"), ui.muted()));
            (session_title(app, ui), position, false)
        }
    };
    let right_area = put(
        frame,
        top,
        ui,
        title,
        Line::from(right.clone().into_iter().collect::<Vec<_>>()),
    );
    if right.is_some() {
        let click = if back {
            mouse::key(KeyCode::Esc)
        } else if app.focus == Focus::Sessions {
            Click::Act(Action::Resume)
        } else {
            Click::Nothing
        };
        hits.click(
            Rect {
                height: 2,
                ..right_area
            },
            click,
        );
    }
    let room = bottom.width.saturating_sub(2 * INSET);
    put(frame, bottom, ui, summary(app, ui, room), Line::default());
}

/// What the open dialog is, in a word.
fn dialog_title(app: &App) -> &'static str {
    if app.help {
        "keys"
    } else if app.machine_panel.is_some() {
        "fleet"
    } else if app.account_screen.is_some() {
        "accounts"
    } else if app.switch.is_some() {
        "switch account"
    } else if app.recover.is_some() {
        "recover"
    } else if app.terminals.is_some() {
        "terminals"
    } else if app.compose.dialog.is_some() {
        "new session"
    } else {
        "herder"
    }
}

/// `left` from one column in and `right` at the end of `area`'s row, `left` cut to leave a
/// gap; returns where `right` went.
fn put(frame: &mut Frame, area: Rect, ui: Ui, left: Line<'static>, right: Line<'static>) -> Rect {
    let start = area.x + INSET.min(area.width);
    let end = area.right().saturating_sub(INSET);
    let right_width = u16::try_from(line_width(&right))
        .unwrap_or(0)
        .min(end - start);
    let right_area = Rect::new(end - right_width, area.y, right_width, 1);
    frame.render_widget(right, right_area);
    let room = (end - right_width).saturating_sub(start + if right_width > 0 { 2 } else { 0 });
    frame.render_widget(
        fit(left, usize::from(room), ui.glyphs),
        Rect::new(start, area.y, room, 1),
    );
    right_area
}

/// The open session as `dot name - project - machine`.
fn session_title(app: &App, ui: Ui) -> Line<'static> {
    let (Some(key), Some(session)) = (&app.open, app.open_session()) else {
        return Line::styled("herder", ui.strong());
    };
    let mut parts = vec![Span::styled(
        super::projects::session_name(session, true),
        ui.strong(),
    )];
    if let Some(project) = app.project_of(key) {
        parts.push(Span::styled(app.project_name(&project), ui.text()));
    }
    if let Some(machine) = app.machines.iter().find(|m| m.host_id == key.host_id) {
        parts.push(Span::styled(machine.name.clone(), ui.muted()));
    }
    let mut spans = vec![state::dot(ui, app.state(key)), Span::raw(" ")];
    spans.extend(ui.joined(parts));
    Line::from(spans)
}

/// Every session's states in a few words, `room` columns at most: `! 2 need you  * 3
/// running`, then the machines that are offline. Where the words do not fit, only the first
/// state keeps its label.
fn summary(app: &App, ui: Ui, room: u16) -> Line<'static> {
    let states = app.summary();
    let offline: Vec<Span<'static>> = app
        .machines
        .iter()
        .filter(|m| matches!(m.connection, ConnectionState::Disconnected { .. }))
        .map(|m| {
            Span::styled(
                format!("{} {} offline", ui.glyphs.disconnected, m.name),
                Style::new().fg(ui.theme.error),
            )
        })
        .collect();
    if states.is_empty() && offline.is_empty() {
        return Line::styled("all quiet", ui.muted());
    }
    let line = |labelled: usize| {
        let mut spans = Vec::new();
        for (at, (state, n)) in states.iter().enumerate() {
            let label = match state {
                State::NeedsYou => " need you",
                _ if at >= labelled => "",
                other => match other {
                    State::Error => " error",
                    State::Done => " done",
                    State::Running => " running",
                    _ => " waiting",
                },
            };
            if !spans.is_empty() {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(
                format!("{} {n}{label}", ui.glyphs.state(*state)),
                state::style(ui, *state),
            ));
        }
        for part in &offline {
            if !spans.is_empty() {
                spans.push(Span::raw("  "));
            }
            spans.push(part.clone());
        }
        Line::from(spans)
    };
    let full = line(states.len());
    if line_width(&full) <= usize::from(room) {
        return full;
    }
    line(1)
}

/// The `│ switch` button over two rows, the needs-you mark under it.
fn switch(frame: &mut Frame, area: Rect, app: &App, focused: bool) {
    let ui = app.ui();
    let theme = ui.theme;
    let edge = Style::new().fg(theme.border);
    let label = if focused {
        Style::new()
            .fg(theme.selected_list_item_text)
            .bg(theme.primary)
            .add_modifier(Modifier::BOLD)
    } else {
        ui.accent().add_modifier(Modifier::BOLD)
    };
    let needs_you = app
        .summary()
        .first()
        .is_some_and(|(s, _)| *s == State::NeedsYou);
    let mark = if needs_you {
        ui.glyphs.state(State::NeedsYou)
    } else {
        " "
    };
    let lines = [
        Line::from(vec![
            Span::styled(ui.glyphs.pipe, edge),
            Span::raw(" "),
            Span::styled("switch", label),
        ]),
        Line::from(vec![
            Span::styled(ui.glyphs.pipe, edge),
            Span::raw("    "),
            Span::styled(mark, state::style(ui, State::NeedsYou)),
        ]),
    ];
    for (y, line) in (area.y..area.bottom()).zip(lines) {
        frame.render_widget(
            line,
            Rect {
                y,
                height: 1,
                ..area
            },
        );
    }
}
