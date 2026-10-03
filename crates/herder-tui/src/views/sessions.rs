//! The left pane: each project, its sessions with their status and machine, or with `v`
//! each machine and its sessions ([`crate::projects`]); children under their parent.
//! A primary shows its child count and, when any child waits on the user, how many; `z`
//! folds its children away. A vault's sessions come under the host they run on, marked
//! offline with when the vault last heard from it; a `moved` copy names the host it went to
//! ([`crate::recover`]). Compact rows, on a narrow screen, show the
//! status as one glyph, the branch's last part and one PR.

use herder_client_core::ConnectionState;
use herder_protocol::{FleetHost, SessionStatus, Timestamp};
use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem};

use crate::account_screen;
use crate::app::{App, Focus, Row};
use crate::mouse::{Click, Hits, List as Rows, Wheel};
use crate::projects::Grouping;

/// Width of the status label column.
const BADGE: usize = 9;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App, compact: bool, hits: &mut Hits) {
    let rows = app.rows();
    // Inside the borders.
    let width = usize::from(area.width.saturating_sub(2));
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| item(app, row, width, compact))
        .collect();
    let highlight = if app.focus == Focus::Sessions {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new().add_modifier(Modifier::BOLD)
    };
    let list = List::new(items)
        .block(
            Block::bordered()
                .title(match app.grouping {
                    Grouping::Projects => " sessions · by project ",
                    Grouping::Machines => " sessions ",
                })
                .border_style(super::border(app, Focus::Sessions)),
        )
        .highlight_style(highlight);
    let selected = app.selected_index(&rows);
    app.list.select(selected);
    frame.render_stateful_widget(list, area, &mut app.list);
    hits.wheel(area, Wheel::Sessions);
    hits.list(
        area.inner(Margin::new(1, 1)),
        app.list.offset(),
        &vec![1; rows.len()],
        |at| Some(Click::Row(Rows::Sessions, at)),
    );
}

fn item<'a>(app: &App, row: &Row, width: usize, compact: bool) -> ListItem<'a> {
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
            let mut spans = vec![
                Span::styled(mark, Style::new().fg(color)),
                Span::styled(format!(" {}", machine.name), super::bold()),
                Span::styled(format!(" ({}) ", machine.sessions.len()), super::dim()),
            ];
            spans.extend(super::resources::row(machine, true));
            ListItem::new(Line::from(spans))
        }
        Row::Host { vault, host } => {
            let Some(machine) = app.machines.iter().find(|m| m.host_id == *vault) else {
                return ListItem::new("");
            };
            let Some(host) = machine.hosts.iter().find(|h| h.host_id == *host) else {
                return ListItem::new("");
            };
            let count = machine
                .sessions
                .iter()
                .filter(|head| head.host_id.as_ref() == Some(&host.host_id))
                .count();
            let (mark, color) = if host.online {
                ("●", Color::Green)
            } else {
                ("✗", Color::Red)
            };
            let mut spans = vec![
                Span::styled(format!("  {mark}"), Style::new().fg(color)),
                Span::styled(format!(" {}", host.host_name), super::bold()),
                Span::styled(format!(" ({count})"), super::dim()),
            ];
            if !host.online {
                spans.push(Span::styled(" offline", Style::new().fg(Color::Red)));
                spans.push(Span::styled(format!(" · {} ago", ago(host)), super::dim()));
            }
            ListItem::new(Line::from(spans))
        }
        Row::Project(project) => super::projects::heading(app, project.as_ref(), width, compact),
        Row::Session { key, depth } => {
            let Some(session) = app.sessions.get(key) else {
                return ListItem::new("");
            };
            let children = app.children(key);
            let indent = match depth {
                0 if children.is_empty() => String::new(),
                0 if app.folded.contains(key) => "▸ ".to_owned(),
                0 => "▾ ".to_owned(),
                depth => format!("{}└ ", "  ".repeat(depth - 1)),
            };
            // A primary counts its children, and how many of them wait on the user.
            let mut tree = Vec::new();
            if !children.is_empty() {
                tree.push(Span::styled(format!(" ({})", children.len()), super::dim()));
                let waiting = children
                    .iter()
                    .filter_map(|child| app.sessions.get(*child))
                    .filter(|child| child.needs_user())
                    .count();
                if waiting > 0 {
                    tree.push(Span::styled(
                        format!(" !{waiting}"),
                        Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                    ));
                }
            }
            let tree_width: usize = tree.iter().map(Span::width).sum();
            let (label, style) = if !session.loaded {
                ("…", super::dim())
            } else if compact {
                super::composer::waiting_glyph(session).unwrap_or_else(|| glyph(session.status))
            } else {
                super::composer::waiting(session).unwrap_or_else(|| badge(session.status))
            };
            let label_width = if compact { 1 } else { BADGE };
            let mut title = Style::new();
            if app.open.as_ref() == Some(key) {
                title = title.add_modifier(Modifier::UNDERLINED);
            }
            // The PR badge stays in view: the title gives way to it.
            let prs = super::prs::badge(session, compact);
            let prs_width: usize = prs.iter().map(Span::width).sum();
            // Grouped by project, each session says which machine it runs on.
            let machine = (app.grouping == Grouping::Projects)
                .then(|| super::projects::machine_label(app, key));
            // A moved copy says where the session went.
            let moved = app
                .moved_to(key)
                .map(|to| Span::styled(format!(" → {to}"), Style::new().fg(Color::Blue)));
            let machine_width =
                machine.as_ref().map_or(0, Span::width) + moved.as_ref().map_or(0, Span::width);
            let room = width.saturating_sub(
                label_width + 2 + indent.chars().count() + tree_width + prs_width + machine_width,
            );
            let name = if machine.is_some() {
                super::projects::session_name(session, compact)
            } else if compact {
                session.short_title()
            } else {
                session.title()
            };
            let mut spans = vec![
                Span::styled(format!(" {label:<label_width$} "), style),
                Span::styled(indent, super::dim()),
                Span::styled(clip(&name, room), title),
            ];
            spans.extend(tree);
            spans.extend(prs);
            spans.extend(machine);
            spans.extend(moved);
            ListItem::new(Line::from(spans))
        }
    }
}

/// How long ago the vault last heard from `host`.
pub(super) fn ago(host: &FleetHost) -> String {
    let secs = Timestamp::now().duration_since(host.last_seen).as_secs();
    account_screen::until(secs)
}

/// `text` cut to `width` characters, with an ellipsis when cut.
pub(super) fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let mut clipped: String = text.chars().take(width.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
}

/// A status as one glyph, in its label's colour, for compact rows.
pub(super) fn glyph(status: SessionStatus) -> (&'static str, Style) {
    let glyph = match status {
        SessionStatus::Idle => "·",
        SessionStatus::Running => "●",
        SessionStatus::WaitingForCapacity => "◌",
        SessionStatus::NeedsYou => "!",
        SessionStatus::Error => "✗",
        SessionStatus::Archived => "▪",
        SessionStatus::Moved => "→",
        SessionStatus::Unknown => "?",
    };
    (glyph, badge(status).1)
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
