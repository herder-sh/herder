//! The left pane: each machine, its sessions with their status, children under their parent.

use herder_client_core::ConnectionState;
use herder_protocol::SessionStatus;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem};

use crate::app::{App, Focus, Row};

/// Width of the status label column.
const BADGE: usize = 9;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App) {
    let rows = app.rows();
    // Inside the borders.
    let width = usize::from(area.width.saturating_sub(2));
    let items: Vec<ListItem> = rows.iter().map(|row| item(app, row, width)).collect();
    let highlight = if app.focus == Focus::Sessions {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new().add_modifier(Modifier::BOLD)
    };
    let list = List::new(items)
        .block(
            Block::bordered()
                .title(" sessions ")
                .border_style(super::border(app, Focus::Sessions)),
        )
        .highlight_style(highlight);
    let selected = app.selected_index(&rows);
    app.list.select(selected);
    frame.render_stateful_widget(list, area, &mut app.list);
}

fn item<'a>(app: &App, row: &Row, width: usize) -> ListItem<'a> {
    match row {
        Row::Machine(host_id) => {
            let Some(machine) = app.machines.iter().find(|m| m.host_id == *host_id) else {
                return ListItem::new("");
            };
            let (mark, color) = match machine.connection {
                ConnectionState::Connected => ("●", Color::Green),
                ConnectionState::Connecting => ("◌", Color::Yellow),
                ConnectionState::Disconnected { .. } => ("✗", Color::Red),
            };
            ListItem::new(Line::from(vec![
                Span::styled(mark, Style::new().fg(color)),
                Span::styled(format!(" {}", machine.name), super::bold()),
                Span::styled(format!(" ({})", machine.sessions.len()), super::dim()),
            ]))
        }
        Row::Session { key, depth } => {
            let Some(session) = app.sessions.get(key) else {
                return ListItem::new("");
            };
            let indent = match depth {
                0 => String::new(),
                depth => format!("{}└ ", "  ".repeat(depth - 1)),
            };
            let (label, style) = if session.loaded {
                super::composer::waiting(session).unwrap_or_else(|| badge(session.status))
            } else {
                ("…", super::dim())
            };
            let mut title = Style::new();
            if app.open.as_ref() == Some(key) {
                title = title.add_modifier(Modifier::UNDERLINED);
            }
            // The PR badge stays in view: the title gives way to it.
            let prs = super::prs::badge(session);
            let prs_width: usize = prs.iter().map(Span::width).sum();
            let room = width.saturating_sub(BADGE + 2 + indent.chars().count() + prs_width);
            let mut spans = vec![
                Span::styled(format!(" {label:<BADGE$} "), style),
                Span::styled(indent, super::dim()),
                Span::styled(clip(&session.title(), room), title),
            ];
            spans.extend(prs);
            ListItem::new(Line::from(spans))
        }
    }
}

/// `text` cut to `width` characters, with an ellipsis when cut.
fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let mut clipped: String = text.chars().take(width.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
}

/// A status's label and colour.
pub(super) fn badge(status: SessionStatus) -> (&'static str, Style) {
    let color = |color| Style::new().fg(color);
    match status {
        SessionStatus::Idle => ("idle", super::dim()),
        SessionStatus::Running => ("running", color(Color::Yellow)),
        SessionStatus::WaitingForCapacity => ("waiting", color(Color::Blue)),
        SessionStatus::NeedsYou => (
            "needs you",
            color(Color::Magenta).add_modifier(Modifier::BOLD),
        ),
        SessionStatus::Error => ("error", color(Color::Red)),
        SessionStatus::Archived => ("archived", super::dim().add_modifier(Modifier::DIM)),
        SessionStatus::Moved => ("moved", super::dim()),
        SessionStatus::Unknown => ("?", super::dim()),
    }
}
