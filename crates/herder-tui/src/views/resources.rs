//! What loads a machine and a session: the host figures in the machines panel, and the open
//! session's usage, wait for capacity and leftovers in a strip above its transcript.
//!
//! The figures are the latest the daemon pushed, so every draw shows them live; while a
//! machine is not connected it has none.

use herder_client_core::Machine;
use herder_protocol::{Constraint, ContainerState, HostResources, SessionStatus, SessionUsage};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::session::Session;
use crate::ui::Ui;
use crate::ui::state::State;

/// Containers listed in the strip; the rest are counted.
const CONTAINER_ROWS: usize = 3;

/// The share of the last 10 seconds above which pressure shows in yellow.
const PRESSURE_WARN: f64 = 10.0;

/// The machine row's load figures: CPU, memory, the worst pressure and turns of the cap; in
/// `compact` rows CPU and memory only.
pub(super) fn row(ui: Ui, machine: &Machine, compact: bool) -> Vec<Span<'static>> {
    let Some(host) = &machine.resources else {
        return Vec::new();
    };
    let mut spans = vec![
        Span::styled(format!("cpu {:>3.0}%  ", host.cpu_percent), ui.muted()),
        Span::styled(format!("mem {:>3.0}%", memory_percent(host)), ui.muted()),
    ];
    if !compact {
        if let Some(pressure) = &host.pressure {
            let worst = pressure
                .cpu_some
                .max(pressure.memory_some)
                .max(pressure.io_some);
            spans.push(Span::styled(
                format!("  psi {worst:>3.0}%"),
                warn_above(ui, worst, PRESSURE_WARN),
            ));
        }
        let turns = format!("  {}/{} turns", host.running_turns, host.max_turns);
        let style = if host.constraint.is_some() {
            Style::new().fg(ui.theme.warning)
        } else {
            ui.muted()
        };
        spans.push(Span::styled(turns, style));
    }
    spans
}

/// The machine details' resource fields: label, value and its style.
pub(super) fn facts(ui: Ui, machine: &Machine) -> Vec<(&'static str, String, Style)> {
    let Some(host) = &machine.resources else {
        return vec![(
            "resources",
            "none while not connected".to_owned(),
            ui.muted(),
        )];
    };
    let plain = ui.text();
    let used = host
        .memory_total_bytes
        .saturating_sub(host.memory_available_bytes);
    let mut facts = vec![
        (
            "cpu",
            format!("{:.0}% of {} cores", host.cpu_percent, host.cpu_cores),
            plain,
        ),
        ("load", format!("{:.2} over 1 min", host.load_1m), plain),
        (
            "memory",
            format!(
                "{} / {} GiB, {:.0}%",
                gib(used),
                gib(host.memory_total_bytes),
                memory_percent(host)
            ),
            plain,
        ),
    ];
    match &host.pressure {
        Some(p) => {
            let worst = p.cpu_some.max(p.memory_some).max(p.io_some);
            facts.push((
                "pressure",
                format!(
                    "cpu {:.0}% mem {:.0}% io {:.0}%",
                    p.cpu_some, p.memory_some, p.io_some
                ),
                warn_above(ui, worst, PRESSURE_WARN),
            ));
            // Every task stalled at once: the machine is close to freezing.
            if p.memory_full >= 1.0 {
                facts.push((
                    "memory stall",
                    format!("{:.0}% of the last 10 s", p.memory_full),
                    Style::new().fg(ui.theme.error),
                ));
            }
        }
        None => facts.push(("pressure", "not reported".to_owned(), ui.muted())),
    }
    let mut turns = format!("{}/{} running", host.running_turns, host.max_turns);
    if host.waiting_turns > 0 {
        turns.push_str(&format!(", {} waiting", host.waiting_turns));
    }
    facts.push(("turns", turns, plain));
    let (binds, style) = match host.constraint {
        Some(constraint) => (
            constraint_name(constraint),
            Style::new().fg(ui.theme.warning),
        ),
        None => ("nothing: room for another turn", ui.muted()),
    };
    facts.push(("binds", binds.to_owned(), style));
    facts
}

/// What a constraint means, for people.
fn constraint_name(constraint: Constraint) -> &'static str {
    match constraint {
        Constraint::MaxTurns => "the turn cap",
        Constraint::Memory => "too little memory",
        Constraint::Load => "load too high",
        Constraint::Pressure => "too much pressure",
    }
}

/// Rows of the open session's resource strip, its padding included; 0 when it has nothing
/// to say.
pub(super) fn strip_height(app: &App, compact: bool) -> u16 {
    match lines(app, compact).len() {
        0 => 0,
        n => u16::try_from(n + 3).unwrap_or(u16::MAX),
    }
}

/// The open session's usage, wait for capacity and leftovers, on a panel with a row of padding
/// round it and a blank row under it; `compact` on a narrow screen.
pub(super) fn strip(frame: &mut Frame, area: Rect, app: &App, compact: bool) {
    let lines = lines(app, compact);
    if area.height < 2 || lines.is_empty() {
        return;
    }
    let ui = app.ui();
    // In line with the transcript's blocks, a column in on either side.
    let panel = Rect {
        x: area.x + 1,
        width: area.width.saturating_sub(2),
        height: area.height - 1,
        ..area
    };
    crate::ui::fill(frame.buffer_mut(), panel, ui.panel());
    let text = Rect {
        x: panel.x + 2,
        y: panel.y + 1,
        width: panel.width.saturating_sub(4),
        height: panel.height.saturating_sub(2),
    };
    let lines: Vec<Line> = lines
        .into_iter()
        .map(|line| crate::ui::fit(line, usize::from(text.width), ui.glyphs))
        .collect();
    frame.render_widget(Paragraph::new(lines), text);
}

/// The strip's lines for the open session.
pub(super) fn lines(app: &App, compact: bool) -> Vec<Line<'static>> {
    let (Some(key), Some(session)) = (&app.open, app.open_session()) else {
        return Vec::new();
    };
    let ui = app.ui();
    let machine = app.machines.iter().find(|m| m.host_id == key.host_id);
    let usage = machine.and_then(|m| m.session_usage.get(&key.session_id));
    let mut lines = Vec::new();
    if session.status == SessionStatus::WaitingForCapacity {
        lines.push(waiting(
            ui,
            machine.and_then(|m| m.resources.as_ref()),
            compact,
        ));
    }
    if let Some(usage) = usage {
        usage_lines(ui, &mut lines, session, usage, compact);
    }
    lines
}

/// Why the session's turn has not started.
fn waiting(ui: Ui, host: Option<&HostResources>, compact: bool) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            format!("{} ", ui.glyphs.state(State::Waiting)),
            Style::new().fg(ui.theme.state_waiting),
        ),
        Span::styled("waiting for capacity", ui.text()),
    ];
    if let Some(host) = host {
        let mut why = format!(
            "{}{}/{} turns",
            ui.glyphs.separator, host.running_turns, host.max_turns
        );
        if let Some(constraint) = host.constraint
            && !compact
        {
            why.push_str(&format!(", {} binds", constraint_name(constraint)));
        }
        spans.push(Span::styled(why, ui.muted()));
    }
    Line::from(spans)
}

fn usage_lines(
    ui: Ui,
    lines: &mut Vec<Line<'static>>,
    session: &Session,
    usage: &SessionUsage,
    compact: bool,
) {
    let theme = ui.theme;
    if usage.processes > 0 {
        let processes = match usage.processes {
            1 => "1 process".to_owned(),
            n => format!("{n} processes"),
        };
        let mut spans = ui.joined([
            Span::styled(format!("cpu {:.0}%", usage.cpu_percent), ui.text()),
            Span::styled(format!("mem {}", bytes(usage.memory_bytes)), ui.text()),
            Span::styled(processes, ui.text()),
        ]);
        if session.status == SessionStatus::Archived {
            spans.push(Span::styled(
                "  left running",
                Style::new().fg(theme.warning),
            ));
        }
        lines.push(Line::from(spans));
    }
    for container in usage.containers.iter().take(CONTAINER_ROWS) {
        let (mark, color) = match container.state {
            ContainerState::Running => (ui.glyphs.connected, theme.success),
            ContainerState::Restarting | ContainerState::Paused | ContainerState::Created => {
                (ui.glyphs.connecting, theme.warning)
            }
            ContainerState::Removing | ContainerState::Exited => {
                (ui.glyphs.state(State::Idle), theme.state_idle)
            }
            ContainerState::Dead => (ui.glyphs.disconnected, theme.error),
        };
        let mut spans = vec![
            Span::styled(format!("{mark} "), Style::new().fg(color)),
            Span::styled(container.name.clone(), ui.text()),
            Span::styled(format!("  {}", state_name(container.state)), ui.muted()),
        ];
        if !compact {
            spans.push(Span::styled(
                format!("{}{}", ui.glyphs.separator, container.image),
                ui.muted(),
            ));
        }
        lines.push(Line::from(spans));
    }
    if let Some(more) = usage.containers.len().checked_sub(CONTAINER_ROWS)
        && more > 0
    {
        lines.push(Line::styled(format!("+{more} more containers"), ui.muted()));
    }
    let mut projects: Vec<&str> = usage
        .containers
        .iter()
        .filter_map(|container| container.compose_project.as_deref())
        .collect();
    projects.sort_unstable();
    projects.dedup();
    if let [project] = projects.as_slice() {
        lines.push(Line::styled(
            format!(":down brings {project} down"),
            ui.muted(),
        ));
    } else if !projects.is_empty() {
        lines.push(Line::styled(
            format!(
                ":down <project>{}{}",
                ui.glyphs.separator,
                projects.join(", ")
            ),
            ui.muted(),
        ));
    }
}

fn state_name(state: ContainerState) -> &'static str {
    match state {
        ContainerState::Created => "created",
        ContainerState::Running => "running",
        ContainerState::Paused => "paused",
        ContainerState::Restarting => "restarting",
        ContainerState::Removing => "removing",
        ContainerState::Exited => "exited",
        ContainerState::Dead => "dead",
    }
}

fn memory_percent(host: &HostResources) -> f64 {
    if host.memory_total_bytes == 0 {
        return 0.0;
    }
    let used = host
        .memory_total_bytes
        .saturating_sub(host.memory_available_bytes);
    // Lossy above 2^53 bytes, far beyond any machine.
    let share = used as f64 / host.memory_total_bytes as f64;
    share * 100.0
}

const GIB: u64 = 1 << 30;

/// `bytes` in MiB below a GiB, else in GiB.
fn bytes(bytes: u64) -> String {
    if bytes < GIB {
        format!("{} MiB", bytes >> 20)
    } else {
        format!("{} GiB", gib(bytes))
    }
}

/// `bytes` in GiB with one decimal, without the unit.
fn gib(bytes: u64) -> String {
    format!("{}.{}", bytes / GIB, bytes % GIB * 10 / GIB)
}

fn warn_above(ui: Ui, value: f64, limit: f64) -> Style {
    if value > limit {
        Style::new().fg(ui.theme.warning)
    } else {
        ui.muted()
    }
}
