//! The machines panel and the add-machine dialog, over the main screen.

use herder_client_core::auth::PairingUri;
use herder_client_core::{ConnectionState, Machine};
use herder_protocol::Role;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Padding, Paragraph};

use crate::app::App;
use crate::machines::{AddMachine, Field, Form, MachinePanel, Step};

/// Width of the label column of details and form fields.
const LABEL: usize = 13;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App, panel: &MachinePanel) {
    match &panel.add {
        Some(add) => dialog(frame, area, app, add),
        None => list(frame, area, app, panel),
    }
}

fn popup_width(area: Rect) -> u16 {
    area.width.saturating_sub(4).min(86)
}

/// Room for a detail or field value in a popup over `area`: inside borders, padding, the
/// indent and the label.
fn value_width(area: Rect) -> usize {
    usize::from(popup_width(area).saturating_sub(6)).saturating_sub(LABEL)
}

fn popup(area: Rect, height: usize) -> Rect {
    let width = popup_width(area);
    let height = u16::try_from(height).unwrap_or(u16::MAX);
    super::centered(area, width, height.min(area.height))
}

fn keys(text: &str) -> Line<'_> {
    Line::styled(format!(" {text} "), super::dim()).centered()
}

fn list(frame: &mut Frame, area: Rect, app: &App, panel: &MachinePanel) {
    let selected = panel.selected(&app.machines);
    let details =
        selected.map_or_else(Vec::new, |at| details(&app.machines[at], value_width(area)));
    let rows = app.machines.len().max(1);
    // Borders, the machines, a blank line, the details.
    let area = popup(area, rows + details.len() + 3);
    let block = Block::bordered()
        .title(" machines ")
        .title_bottom(keys("a add  r reconnect  Esc close"))
        .border_style(Style::new().fg(Color::Cyan))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let rows = u16::try_from(rows).unwrap_or(u16::MAX);
    let [top, _, bottom] = Layout::vertical([
        Constraint::Length(rows),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(inner);
    if app.machines.is_empty() {
        let hint = "No machines paired yet: press a to add one.";
        frame.render_widget(Line::styled(hint, super::dim()), top);
        return;
    }
    let items: Vec<ListItem> = app.machines.iter().map(row).collect();
    let mut state = ListState::default().with_selected(selected);
    let list = List::new(items).highlight_style(Style::new().reversed());
    frame.render_stateful_widget(list, top, &mut state);
    frame.render_widget(Paragraph::new(details), bottom);
}

/// A machine's connection mark, its colour, and what it says.
fn connection(machine: &Machine) -> (&'static str, Color, String) {
    match &machine.connection {
        ConnectionState::Connected => ("●", Color::Green, "connected".to_owned()),
        ConnectionState::Connecting => ("◌", Color::Yellow, "connecting".to_owned()),
        ConnectionState::Disconnected { error } => ("✗", Color::Red, error.clone()),
    }
}

fn row(machine: &Machine) -> ListItem<'static> {
    let (mark, color, state) = connection(machine);
    let sessions = match machine.sessions.len() {
        1 => "1 session".to_owned(),
        n => format!("{n} sessions"),
    };
    ListItem::new(Line::from(vec![
        Span::styled(mark, Style::new().fg(color)),
        Span::styled(format!(" {:<16} ", machine.name), super::bold()),
        Span::styled(format!("{sessions:<12} "), super::dim()),
        Span::styled(state, super::dim()),
    ]))
}

fn details(machine: &Machine, width: usize) -> Vec<Line<'static>> {
    let (_, color, state) = connection(machine);
    let role = match machine.role {
        Some(Role::Owner) => "owner",
        Some(Role::Member) => "member",
        None => "not known until connected",
    };
    let mut lines = vec![Line::styled(machine.name.clone(), super::bold())];
    lines.extend(field("state", &state, Style::new().fg(color), width));
    lines.extend(field("role", role, Style::new(), width));
    lines.extend(field(
        "host id",
        machine.host_id.as_str(),
        Style::new(),
        width,
    ));
    let addresses = machine.addresses.join(", ");
    lines.extend(field("addresses", &addresses, Style::new(), width));
    lines.extend(field(
        "fingerprint",
        &machine.fingerprint,
        Style::new(),
        width,
    ));
    lines
}

/// A label and its value, the value cut into lines `width` wide under each other.
fn field(label: &str, value: &str, style: Style, width: usize) -> Vec<Line<'static>> {
    let chars: Vec<char> = value.chars().collect();
    let chunks: Vec<String> = if chars.is_empty() {
        vec![String::new()]
    } else {
        chars
            .chunks(width.max(8))
            .map(|chunk| chunk.iter().collect())
            .collect()
    };
    chunks
        .into_iter()
        .enumerate()
        .map(|(at, chunk)| {
            let label = if at == 0 { label } else { "" };
            Line::from(vec![
                Span::styled(format!("  {label:<LABEL$}"), super::dim()),
                Span::styled(chunk, style),
            ])
        })
        .collect()
}

fn dialog(frame: &mut Frame, area: Rect, app: &App, add: &AddMachine) {
    let width = value_width(area);
    let (lines, keys_text) = match &add.step {
        Step::Edit => (
            form(&add.form, None, width),
            "Enter next  Tab field  Esc cancel",
        ),
        Step::Failed(error) => (
            form(&add.form, Some(error), width),
            "Enter next  Tab field  Esc cancel",
        ),
        Step::Confirm(uri) => (confirm(app, uri, width), "Enter pair  Esc back"),
        Step::Pairing(uri) => {
            let to = uri.hosts.join(", ");
            let lines = vec![Line::raw(format!("Pairing with {to}…"))];
            (lines, "Esc close")
        }
        Step::Paired(machine) => {
            let mut lines = vec![
                Line::styled(format!("Paired with {}.", machine.name), super::bold()),
                Line::raw(""),
            ];
            lines.extend(field(
                "host id",
                machine.host_id.as_str(),
                Style::new(),
                width,
            ));
            lines.extend(field(
                "fingerprint",
                &machine.fingerprint,
                Style::new(),
                width,
            ));
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "Its sessions show in the list as it connects.",
                super::dim(),
            ));
            (lines, "Enter done")
        }
    };
    let area = popup(area, lines.len() + 4);
    let block = Block::bordered()
        .title(" add a machine ")
        .title_bottom(keys(keys_text))
        .border_style(Style::new().fg(Color::Cyan))
        .padding(Padding::uniform(1));
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn form(form: &Form, error: Option<&String>, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::raw("On the machine to add, run `herder pair` and paste the link it prints:"),
        Line::raw(""),
        input(form, Field::Link, "link", &form.link, width),
        Line::raw(""),
        Line::styled("or enter what it prints:", super::dim()),
        input(form, Field::Host, "address", &form.host, width),
        input(
            form,
            Field::Fingerprint,
            "fingerprint",
            &form.fingerprint,
            width,
        ),
        input(form, Field::Code, "code", &form.code, width),
    ];
    if let Some(error) = error {
        lines.push(Line::raw(""));
        lines.push(Line::styled(error.clone(), Style::new().fg(Color::Red)));
    }
    lines
}

/// One form field: its label, then its value's end, with a cursor when it is being typed in.
fn input(form: &Form, which: Field, label: &str, value: &str, width: usize) -> Line<'static> {
    let focused = form.focus == which;
    let room = width.saturating_sub(1).max(4);
    let chars: Vec<char> = value.chars().collect();
    let shown: String = chars[chars.len().saturating_sub(room)..].iter().collect();
    let label_style = if focused {
        Style::new().fg(Color::Cyan)
    } else {
        super::dim()
    };
    let mut spans = vec![
        Span::styled(format!("  {label:<LABEL$}"), label_style),
        Span::raw(shown),
    ];
    if focused {
        spans.push(Span::styled("▌", Style::new().fg(Color::Cyan)));
    }
    Line::from(spans)
}

fn confirm(app: &App, uri: &PairingUri, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::styled("Pair with this daemon?", super::bold()),
        Line::raw(""),
    ];
    lines.extend(field("address", &uri.hosts.join(", "), Style::new(), width));
    let fingerprint = uri.fingerprint.to_ascii_lowercase();
    let yellow = Style::new().fg(Color::Yellow);
    lines.extend(field("fingerprint", &fingerprint, yellow, width));
    lines.push(Line::raw(""));
    lines.push(Line::raw(
        "Check that the fingerprint is the one `herder pair` printed.",
    ));
    if let Some(known) = app.machines.iter().find(|m| m.fingerprint == fingerprint) {
        lines.push(Line::styled(
            format!(
                "Already paired as {}: pairing again gives it a new key.",
                known.name
            ),
            super::dim(),
        ));
    }
    lines
}
