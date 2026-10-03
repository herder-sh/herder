//! The details panel, OpenCode's sidebar: on a screen at least [`crate::nav::WIDE`] columns
//! wide it sits right of the main pane, on a narrower one `ctrl+x d` lays it over the main
//! pane's right side. It tells about the open session, in sections:
//!
//! ```text
//!  api
//!  herder/api · box
//!  claude-main · opus · ask
//!
//!  Usage · claude-main
//!  5h    ███████░░░░░░░░░  38%
//!  failover on
//!
//!  Tasks 2
//!  ● write tests
//! ```
//!
//! then the session's pull requests, resources and terminals.

use herder_protocol::TerminalPurpose;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::account_screen;
use crate::app::App;
use crate::ui::state::{self, State};
use crate::ui::{INSET, Ui, usage};

/// The panel in `area`, a separator in its first column; `over` clears what it covers.
pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App, over: bool) {
    let ui = app.ui();
    if over {
        frame.render_widget(Clear, area);
        crate::ui::fill(frame.buffer_mut(), area, ui.base());
    }
    super::edge(frame, Rect { width: 1, ..area }, ui);
    let inner = Rect {
        x: area.x + 1 + INSET,
        width: area.width.saturating_sub(1 + 2 * INSET),
        y: area.y,
        height: area.height,
    };
    let lines = lines(app, inner.width);
    let lines: Vec<Line> = lines
        .into_iter()
        .map(|line| crate::ui::fit(line, usize::from(inner.width), ui.glyphs))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The panel's lines, `width` columns wide.
fn lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let ui = app.ui();
    let (Some(key), Some(session)) = (&app.open, app.open_session()) else {
        return vec![Line::styled("No session open.", ui.muted())];
    };
    let machine = app.machines.iter().find(|m| m.host_id == key.host_id);
    let state = app.state(key);
    let mut lines = vec![
        Line::from(vec![
            state::dot(ui, state),
            Span::raw(" "),
            Span::styled(super::projects::session_name(session, true), ui.strong()),
            Span::styled(format!("  {}", state.label()), state::style(ui, state)),
        ]),
        Line::from(
            ui.joined(
                [
                    Some(session.branch.clone()),
                    machine.map(|m| m.name.clone()),
                    Some(home(&session.repo)),
                ]
                .into_iter()
                .flatten()
                .filter(|part| !part.is_empty())
                .map(|part| Span::styled(part, ui.muted())),
            ),
        ),
        Line::from(
            ui.joined(
                super::tabs::facts(app)
                    .into_iter()
                    .map(|fact| Span::styled(fact, ui.muted())),
            ),
        ),
    ];

    // The account's busiest windows, and whether it fails over.
    let account = session
        .account_id
        .as_ref()
        .and_then(|id| account_screen::find(&app.machines, &key.host_id, id));
    if let Some(account) = account {
        section(&mut lines, ui, format!("Usage · {}", account.label));
        let labels: Vec<String> = account
            .usage
            .iter()
            .map(|w| account_screen::window_label(&w.window))
            .collect();
        let label_width = labels.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        let bar_width = width.saturating_sub(u16::try_from(label_width + 2 + 5).unwrap_or(0));
        for (window, label) in account.usage.iter().zip(&labels) {
            // Clamped to 0..=100 first, so the cast cannot wrap.
            let percent = window.used_percent.clamp(0.0, 100.0).round() as u8;
            let mut spans = vec![Span::styled(format!("{label:<label_width$}  "), ui.muted())];
            spans.extend(usage::bar(ui, percent, bar_width.min(20)).spans);
            lines.push(Line::from(spans));
        }
        if account.usage.is_empty() {
            lines.push(Line::styled("no usage reported yet", ui.muted()));
        }
        let failover = if account.failover {
            "failover on"
        } else {
            "failover off"
        };
        lines.push(Line::styled(failover, ui.muted()));
    }

    let tasks = app.tasks();
    if !tasks.is_empty() {
        section(&mut lines, ui, format!("Tasks {}", tasks.len()));
        for task in &tasks {
            let Some(child) = app.sessions.get(task) else {
                continue;
            };
            let state = app.state(task);
            let mut spans = vec![
                state::dot(ui, state),
                Span::raw(" "),
                Span::styled(child.title(), ui.text()),
            ];
            if state == State::NeedsYou
                && let Some((mark, _)) = super::composer::waiting_glyph(child)
            {
                spans.push(Span::styled(format!(" {mark}"), state::style(ui, state)));
            }
            lines.push(Line::from(spans));
        }
    }

    if !session.prs.is_empty() {
        section(&mut lines, ui, "Pull requests".to_owned());
        for pr in &session.prs {
            lines.push(super::prs::line(ui, pr));
        }
    }

    // The machine's load, then the session's own.
    let host = machine
        .map(|m| super::resources::row(ui, m, true))
        .unwrap_or_default();
    let resources = super::resources::lines(app, true);
    if !host.is_empty() || !resources.is_empty() {
        section(&mut lines, ui, "Resources".to_owned());
        if let Some(machine) = machine.filter(|_| !host.is_empty()) {
            let mut spans = vec![Span::styled(format!("{}  ", machine.name), ui.muted())];
            spans.extend(host);
            lines.push(Line::from(spans));
        }
        lines.extend(resources);
    }

    let terminals = machine.map_or(0, |m| {
        m.terminals
            .iter()
            .filter(|t| {
                matches!(&t.purpose, TerminalPurpose::Shell { session_id }
                    if *session_id == key.session_id)
            })
            .count()
    });
    if terminals > 0 {
        section(&mut lines, ui, format!("Terminals {terminals}"));
        lines.push(Line::styled("t opens one", ui.muted()));
    }
    lines
}

/// A blank line and a section's heading.
fn section(lines: &mut Vec<Line<'static>>, ui: Ui, title: String) {
    lines.push(Line::default());
    lines.push(Line::styled(title, ui.strong()));
}

/// `path` with the home directory as `~`.
fn home(path: &str) -> String {
    let mut parts = path.splitn(4, '/');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(""), Some("home" | "Users"), Some(_), Some(rest)) => format!("~/{rest}"),
        _ => path.to_owned(),
    }
}
