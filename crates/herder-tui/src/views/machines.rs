//! The fleet view, in the main pane: every machine with its role, address and load; under the
//! list, the selected machine in full, its host resources included. Over it go the
//! add-machine and add-account dialogs.
//!
//! ```text
//!  fleet · 2 machines
//!
//! ▶ ● box     owner   10.0.0.4:7447              cpu  23%  mem  41%  3/6 turns
//!     9f2c…41ab · 3 accounts · 5 sessions · 12 ms
//!   ✗ laptop  member  laptop.lan:7447
//!     01de…77c0 · connection refused
//!
//!  box
//!  state        connected
//!  link         up 2m · 1 reconnect · 0 missed pongs
//!  latency      12 ms · avg 14 ms · 9–31 ms
//!  cpu          23% of 8 cores
//! ```

use herder_client_core::{ConnectionState, Machine};
use herder_protocol::Role;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use ratatui_textarea::TextArea;

use crate::accounts::{self, AddAccount};
use crate::app::App;
use crate::machines::{MachinePanel, PanelEdit};
use crate::mouse::{Click, Hits, List as Rows, Wheel};
use crate::palette::search_line;
use crate::ui::dialog::{Dialog, Size};
use crate::ui::hints::Hint;
use crate::ui::input::{Choice, Field as TextField};
use crate::ui::list::{ListView, Row};
use crate::ui::select::label_width;
use crate::ui::{Ui, fit};

/// Width of the label column of the details.
const LABEL: usize = 14;

pub(super) fn draw(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    panel: &MachinePanel,
    hits: &mut Hits,
) {
    list(frame, area, app, panel, hits);
    match (&panel.account, &panel.add) {
        (Some(account), _) => account_dialog(frame, area, app, account),
        (None, Some(add)) => super::add_machine::draw(frame, area, app, add, hits),
        (None, None) => {}
    }
}

fn list(frame: &mut Frame, area: Rect, app: &App, panel: &MachinePanel, hits: &mut Hits) {
    let ui = app.ui();
    let compact = app.width < super::NARROW;
    super::clear(frame, area, ui);
    let count = match app.machines.len() {
        1 => "1 machine".to_owned(),
        n => format!("{n} machines"),
    };
    let area = super::heading(frame, area, ui, "fleet", &count, "", compact);
    if app.machines.is_empty() {
        super::nothing(frame, area, ui, "No machines paired yet: a adds one.");
        return;
    }
    let selected = panel.selected(&app.machines);
    let width = usize::from(area.width.saturating_sub(3)).saturating_sub(LABEL);
    let mut details =
        selected.map_or_else(Vec::new, |at| details(ui, app, &app.machines[at], width));
    match &panel.edit {
        Some(PanelEdit::Rename(name)) => {
            // The name being typed replaces the details' heading.
            if let Some(heading) = details.first_mut() {
                *heading = Line::from(vec![
                    Span::styled(format!("{:<LABEL$}", "name"), ui.accent()),
                    Span::styled(name.clone(), ui.strong()),
                    Span::styled(ui.glyphs.cursor, ui.accent()),
                ]);
            }
        }
        Some(PanelEdit::Forget) => {
            if let Some(heading) = details.first_mut() {
                let name = selected.map_or("", |at| app.machines[at].name.as_str());
                *heading = Line::styled(
                    format!("Forget {name} on this device? y forgets, n keeps it."),
                    Style::new().fg(ui.theme.warning),
                );
            }
        }
        None => {}
    }
    let rows: Vec<Row> = app
        .machines
        .iter()
        .map(|machine| row(ui, app, machine, compact))
        .collect();
    let total = u16::try_from(rows.iter().map(Row::height).sum::<usize>()).unwrap_or(u16::MAX);
    // The list first; the details take what is left.
    let list_height = total.min(area.height);
    let list_area = Rect {
        height: list_height,
        ..area
    };
    let mut offset = 0;
    let placed = ListView::new(ui, rows)
        .select(selected)
        .focused(panel.edit.is_none() && panel.add.is_none() && panel.account.is_none())
        .render(list_area, frame.buffer_mut(), &mut offset);
    if panel.edit.is_none() {
        hits.wheel(list_area, Wheel::Keys);
        for (at, rect) in placed {
            hits.click(rect, Click::Row(Rows::Machines, at));
        }
    }
    if area.height > list_height + 1 {
        let details_area = Rect {
            // In line with the rows' status dots.
            x: area.x + 2,
            y: list_area.bottom() + 1,
            width: area.width.saturating_sub(2 + crate::ui::INSET),
            height: area.bottom() - list_area.bottom() - 1,
        };
        let details: Vec<Line> = details
            .into_iter()
            .map(|line| fit(line, usize::from(details_area.width), ui.glyphs))
            .collect();
        frame.render_widget(Paragraph::new(details), details_area);
    }
}

/// A machine's connection mark, its colour, and what it says.
pub(super) fn connection(ui: Ui, machine: &Machine) -> (&'static str, Style, String) {
    let (mark, color) = super::add_machine::connection_mark(ui, machine);
    let state = match &machine.connection {
        ConnectionState::Connected => "connected".to_owned(),
        ConnectionState::Connecting => "connecting".to_owned(),
        ConnectionState::Disconnected { error } => error.clone(),
    };
    (mark, Style::new().fg(color), state)
}

/// A machine's row: its connection, name, role and address, its load at the right; under it
/// its fingerprint, accounts and sessions, or why it is not connected. `compact` on a phone,
/// where the address and the load's pressure and turns go.
fn row<'a>(ui: Ui, app: &App, machine: &Machine, compact: bool) -> Row<'a> {
    let (mark, style, state) = connection(ui, machine);
    let mut left = vec![
        Span::styled(mark, style),
        Span::raw(" "),
        Span::styled(machine.name.clone(), ui.text()),
    ];
    if let Some(role) = machine.role {
        left.push(Span::raw("  "));
        left.push(Span::styled(role_name(role), ui.muted()));
    }
    if let Some(address) = machine.addresses.first().filter(|_| !compact) {
        left.push(Span::raw("  "));
        left.push(Span::styled(address.clone(), ui.muted()));
    }
    let right = Line::from(super::resources::row(ui, machine, compact));
    let sessions = app
        .sessions
        .keys()
        .filter(|key| key.host_id == machine.host_id)
        .count();
    let count = |n: usize, one: &str| match n {
        1 => format!("1 {one}"),
        n => format!("{n} {one}s"),
    };
    let mut facts = vec![Span::styled(short(ui, &machine.fingerprint), ui.muted())];
    if matches!(machine.connection, ConnectionState::Connected) {
        facts.push(Span::styled(
            count(machine.accounts.len(), "account"),
            ui.muted(),
        ));
        facts.push(Span::styled(count(sessions, "session"), ui.muted()));
        if let Some(rtt) = machine.quality.last_rtt_ms {
            facts.push(Span::styled(format!("{rtt} ms"), ui.muted()));
        }
    } else {
        facts.push(Span::styled(state, style));
    }
    Row::item(Line::from(left))
        .right(right)
        .body(vec![Line::from(ui.joined(facts))])
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Owner => "owner",
        Role::Member => "member",
    }
}

/// A fingerprint's first and last four digits, as a glance compares them.
fn short(ui: Ui, fingerprint: &str) -> String {
    let digits: Vec<char> = fingerprint.chars().collect();
    if digits.len() <= 12 {
        return fingerprint.to_owned();
    }
    let head: String = digits[..4].iter().collect();
    let tail: String = digits[digits.len() - 4..].iter().collect();
    format!("{head}{}{tail}", ui.glyphs.ellipsis)
}

/// The selected machine in full: its name, then each fact, the values `width` wide.
fn details(ui: Ui, app: &App, machine: &Machine, width: usize) -> Vec<Line<'static>> {
    let (_, style, state) = connection(ui, machine);
    let role = machine.role.map_or("not known until connected", role_name);
    let mut lines = vec![Line::styled(machine.name.clone(), ui.strong())];
    lines.extend(field(ui, "state", &state, style, width));
    if let Some(link) = link(app, machine) {
        lines.extend(field(ui, "link", &link, ui.text(), width));
    }
    if let Some(latency) = latency(machine) {
        lines.extend(field(ui, "latency", &latency, ui.text(), width));
    }
    lines.extend(field(ui, "role", role, ui.text(), width));
    let accounts = machine
        .accounts
        .iter()
        .map(|account| format!("{} ({})", account.account_id, account.provider.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    let accounts = if accounts.is_empty() {
        "none".to_owned()
    } else {
        accounts
    };
    lines.extend(field(ui, "accounts", &accounts, ui.text(), width));
    for (label, value, style) in super::resources::facts(ui, machine) {
        lines.extend(field(ui, label, &value, style, width));
    }
    lines.extend(field(
        ui,
        "host id",
        machine.host_id.as_str(),
        ui.text(),
        width,
    ));
    let addresses = machine.addresses.join(", ");
    lines.extend(field(ui, "addresses", &addresses, ui.text(), width));
    lines.extend(field(
        ui,
        "fingerprint",
        &machine.fingerprint,
        ui.text(),
        width,
    ));
    lines
}

/// How long the connection has been up, and how often this client lost it or a pong; `None`
/// before the first connection.
fn link(app: &App, machine: &Machine) -> Option<String> {
    let quality = &machine.quality;
    if quality.connected_since.is_none() && quality.reconnects == 0 && quality.missed_pongs == 0 {
        return None;
    }
    let plural = |n: u32, one: &str| match n {
        1 => format!("1 {one}"),
        n => format!("{n} {one}s"),
    };
    let mut parts = Vec::new();
    if let Some(since) = quality.connected_since {
        let up = app.now().as_second() - since.as_second();
        parts.push(format!("up {}", super::inbox::ago(up)));
    }
    parts.push(plural(quality.reconnects, "reconnect"));
    parts.push(plural(quality.missed_pongs, "missed pong"));
    Some(parts.join(" · "))
}

/// The latest round trip, and the mean and range of the recent ones; `None` before the first.
fn latency(machine: &Machine) -> Option<String> {
    let quality = &machine.quality;
    let last = quality.last_rtt_ms?;
    let mut latency = format!("{last} ms");
    if let (Some(average), Some(min), Some(max)) = (
        quality.average_rtt_ms,
        quality.min_rtt_ms,
        quality.max_rtt_ms,
    ) {
        latency.push_str(&format!(" · avg {average} ms · {min}–{max} ms"));
    }
    Some(latency)
}

/// A label and its value, the value wrapped `width` wide under itself; a word longer than a
/// line, such as a fingerprint, is cut.
fn field(ui: Ui, label: &str, value: &str, style: Style, width: usize) -> Vec<Line<'static>> {
    let mut chunks: Vec<String> = textwrap::wrap(value, width.max(8))
        .into_iter()
        .map(|chunk| chunk.into_owned())
        .collect();
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
        .into_iter()
        .enumerate()
        .map(|(at, chunk)| {
            let label = if at == 0 { label } else { "" };
            Line::from(vec![
                Span::styled(format!("{label:<LABEL$}"), ui.muted()),
                Span::styled(chunk, style),
            ])
        })
        .collect()
}

/// The add-account dialog: the provider, the id, label and config dir, and how the login
/// runs.
pub(super) fn account_dialog(frame: &mut Frame, area: Rect, app: &App, account: &AddAccount) {
    let ui = app.ui();
    let machine = app
        .machines
        .iter()
        .find(|machine| machine.host_id == account.host_id)
        .map_or_else(
            || account.host_id.to_string(),
            |machine| machine.name.clone(),
        );
    let body_width = usize::from(
        Size::Medium
            .width()
            .min(area.width)
            .saturating_sub(2 + 2 * crate::ui::dialog::PAD_X),
    )
    .max(8);
    let wrapped = |text: &str, style: Style| -> Vec<Line<'static>> {
        textwrap::wrap(text, body_width)
            .into_iter()
            .map(|line| Line::styled(line.into_owned(), style))
            .collect()
    };
    let mut top = wrapped(
        &format!("Runs the provider's own login on {machine}, in a terminal here."),
        ui.muted(),
    );
    top.push(Line::default());
    let mut bottom = vec![Line::default()];
    bottom.extend(wrapped(
        accounts::login_hint(account.provider()),
        ui.muted(),
    ));
    bottom.extend(wrapped(
        "The account is added once the login succeeds. ctrl+] d detaches.",
        ui.muted(),
    ));
    if let Some(error) = &account.error {
        bottom.push(Line::default());
        bottom.extend(super::failure(ui, error, body_width));
    }
    let fields = 4;
    let height = u16::try_from(top.len() + fields + bottom.len()).unwrap_or(u16::MAX);
    let hints = [
        Hint::new("enter", "log in"),
        Hint::new("tab", "field"),
        Hint::new("←/→", "provider"),
        Hint::new("esc", "cancel"),
    ];
    let narrow = area.width < super::NARROW;
    let areas = Dialog::new(ui, "add account", Size::Medium)
        .hints(if narrow { &hints[..2] } else { &hints })
        .render(area, height, frame.buffer_mut());
    let body = areas.body;
    let buf = frame.buffer_mut();
    let mut y = body.y;
    let line = |line: Line<'static>, y: &mut u16, buf: &mut ratatui::buffer::Buffer| {
        if *y < body.bottom() {
            line.render(Rect::new(body.x, *y, body.width, 1), buf);
        }
        *y += 1;
    };
    for text in top {
        line(text, &mut y, buf);
    }
    let label = label_width("config dir");
    if y < body.bottom() {
        Choice::new(
            ui,
            "provider",
            Span::styled(account.provider().as_str().to_owned(), ui.text()),
        )
        .focused(account.focus == accounts::Field::Provider)
        .render(Rect::new(body.x, y, body.width, 1), buf, label);
    }
    y += 1;
    let default_dir = account.default_config_dir();
    for (field, name, value, placeholder) in [
        (accounts::Field::Id, "id", &account.id, "e.g. work"),
        (accounts::Field::Label, "label", &account.label, "the id"),
        (
            accounts::Field::ConfigDir,
            "config dir",
            &account.config_dir,
            default_dir.as_str(),
        ),
    ] {
        if y < body.bottom() {
            let mut editor: TextArea = search_line(value, placeholder);
            TextField::new(ui, name, &mut editor)
                .focused(account.focus == field)
                .render(Rect::new(body.x, y, body.width, 1), buf, label);
        }
        y += 1;
    }
    for text in bottom {
        line(text, &mut y, buf);
    }
}
