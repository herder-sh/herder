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
//!
//! A hub shows how full its disk is, as it said when the fleet view opened, in place of
//! accounts and load, warning above [`HubVolume::WARN_RATIO`], and what each host it
//! backs up takes there:
//!
//! ```text
//!  disk         412.8 / 480.0 GiB, 86% · nearly full
//!  hosts        devbox  12 sessions · images 340 MiB of 1.0 GiB
//!               laptop   3 sessions · images off
//! ```

use herder_client_core::{ConnectionState, Machine, RouteChoice, RouteKind};
use herder_protocol::{FleetHost, HubVolume, Role, Timestamp};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use ratatui_textarea::TextArea;

use crate::accounts::{self, AddAccount};
use crate::app::App;
use crate::backup::{self, Known};
use crate::machines::{self, MachinePanel, PanelEdit};
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
    if let Some(secrets) = &panel.secrets {
        return super::secrets::draw(frame, area, app, secrets, hits);
    }
    if let Some(usage) = &panel.usage {
        return super::hub_usage::draw(frame, area, app, usage, hits);
    }
    if let Some(remote) = &panel.remote {
        return super::remote_access::draw(frame, area, app, remote, hits);
    }
    list(frame, area, app, panel, hits);
    match (&panel.account, &panel.add, &panel.backup) {
        (Some(account), _, _) => account_dialog(frame, area, app, account),
        (None, Some(add), _) => super::add_machine::draw(frame, area, app, add, hits),
        (None, None, Some(backup)) => super::backup::draw(frame, area, app, backup),
        (None, None, None) => {}
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
    let mut details = selected.map_or_else(Vec::new, |at| {
        details(ui, app, panel, &app.machines[at], width)
    });
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
        Some(PanelEdit::Public(text)) => {
            // The addresses being typed replace the details' heading.
            if let Some(heading) = details.first_mut() {
                *heading = match text {
                    Some(text) => Line::from(vec![
                        Span::styled(format!("{:<LABEL$}", "public"), ui.accent()),
                        Span::styled(text.clone(), ui.strong()),
                        Span::styled(ui.glyphs.cursor, ui.accent()),
                    ]),
                    None => Line::styled("Asking for its public addresses…", ui.muted()),
                };
                details.insert(
                    1,
                    Line::styled(
                        "each a host:port or a tunnel's wss:// URL, spaces between",
                        ui.muted(),
                    ),
                );
            }
        }
        Some(PanelEdit::Routes(at)) => {
            // The routes, in the order they are tried, replace the details.
            if let Some(at_machine) = selected {
                details = routes(ui, app, &app.machines[at_machine], *at);
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
        .map(|machine| row(ui, app, panel, machine, compact))
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
        .focused(
            panel.edit.is_none()
                && panel.add.is_none()
                && panel.account.is_none()
                && panel.backup.is_none(),
        )
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
fn row<'a>(ui: Ui, app: &App, panel: &MachinePanel, machine: &Machine, compact: bool) -> Row<'a> {
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
    // The route in use while connected, else the one tried first.
    let address = machine.address.as_ref().or(machine.addresses.first());
    if let Some(address) = address.filter(|_| !compact) {
        left.push(Span::raw("  "));
        left.push(Span::styled(route(app, address), ui.muted()));
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
        if is_hub(panel, machine) {
            facts.push(Span::styled(count(machine.hosts.len(), "host"), ui.muted()));
        } else {
            facts.push(Span::styled(
                count(machine.accounts.len(), "account"),
                ui.muted(),
            ));
        }
        // A phone has room for a hub's hosts or its sessions, not both.
        if !(compact && is_hub(panel, machine)) {
            facts.push(Span::styled(count(sessions, "session"), ui.muted()));
        }
        if let Some(volume) = panel.volumes.get(&machine.host_id) {
            let percent = format!("disk {}", percent(volume));
            facts.push(Span::styled(percent, disk_style(ui, volume)));
        }
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

/// Whether `machine` is a hub: it lists the hosts it backs up, or said it is one.
fn is_hub(panel: &MachinePanel, machine: &Machine) -> bool {
    !machine.hosts.is_empty() || panel.links.get(&machine.host_id) == Some(&Known::Hub)
}

/// How much of the hub's disk is in use, as `86%`.
fn percent(volume: &HubVolume) -> String {
    format!("{:.0}%", volume.used_ratio() * 100.0)
}

/// The warning colour once the hub's disk is nearly full, else muted.
fn disk_style(ui: Ui, volume: &HubVolume) -> Style {
    if volume.nearly_full() {
        Style::new().fg(ui.theme.warning)
    } else {
        ui.muted()
    }
}

/// The hub's disk and what each host takes on it, as details rows `width` wide.
fn hub_facts(ui: Ui, panel: &MachinePanel, machine: &Machine, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match panel.volumes.get(&machine.host_id) {
        Some(volume) => {
            let mut disk = format!(
                "{} / {} GiB, {}",
                super::resources::gib(volume.used_bytes),
                super::resources::gib(volume.total_bytes),
                percent(volume)
            );
            let style = if volume.nearly_full() {
                disk.push_str(&format!("{}nearly full", ui.glyphs.separator));
                Style::new().fg(ui.theme.warning)
            } else {
                ui.text()
            };
            lines.extend(field(ui, "disk", &disk, style, width));
        }
        None => lines.extend(field(ui, "disk", "not known", ui.muted(), width)),
    }
    if machine.hosts.is_empty() {
        lines.extend(field(
            ui,
            "hosts",
            "none backs up here yet",
            ui.muted(),
            width,
        ));
        return lines;
    }
    let name_width = machine
        .hosts
        .iter()
        .map(|host| host.host_name.chars().count())
        .max()
        .unwrap_or(0);
    let sessions_width = machine
        .hosts
        .iter()
        .filter_map(|host| host.usage.as_ref())
        .map(|usage| usage.sessions.to_string().len())
        .max()
        .unwrap_or(1);
    for (at, host) in machine.hosts.iter().enumerate() {
        let label = if at == 0 { "hosts" } else { "" };
        lines.extend(host_usage(
            ui,
            label,
            host,
            name_width,
            sessions_width,
            width,
        ));
    }
    lines
}

/// One host's row under `hosts`: its name, then its sessions and images against its cap,
/// in columns `name_width` and `sessions_width` wide; where the row has no room for that,
/// the images go on a line of their own under the name.
fn host_usage(
    ui: Ui,
    label: &str,
    host: &FleetHost,
    name_width: usize,
    sessions_width: usize,
    width: usize,
) -> Vec<Line<'static>> {
    let separator = ui.glyphs.separator;
    let usage = host.usage.as_ref().map(|usage| {
        let sessions = match usage.sessions {
            1 => "session",
            _ => "sessions",
        };
        let images = match usage.attachments_cap {
            Some(cap) => format!(
                "images {} of {}",
                super::resources::bytes(usage.attachment_bytes),
                super::resources::bytes(cap)
            ),
            None if usage.attachment_bytes > 0 => format!(
                "images off, {} kept",
                super::resources::bytes(usage.attachment_bytes)
            ),
            None => "images off".to_owned(),
        };
        (
            format!("{:>sessions_width$} {sessions:<8}", usage.sessions),
            images,
        )
    });
    let Some((sessions, images)) = usage else {
        return field(ui, label, &host.host_name, ui.text(), width);
    };
    let fits = name_width
        + 2
        + sessions.chars().count()
        + separator.chars().count()
        + images.chars().count()
        <= width;
    if !fits {
        let sessions = sessions.split_whitespace().collect::<Vec<_>>().join(" ");
        let name = format!("{}{separator}{sessions}", host.host_name);
        let mut lines = field(ui, label, &name, ui.text(), width);
        if images.chars().count() + 2 <= width {
            lines.push(Line::from(vec![
                Span::raw(" ".repeat(LABEL + 2)),
                Span::styled(images, ui.muted()),
            ]));
        } else {
            lines.extend(field(ui, "", &images, ui.muted(), width));
        }
        return lines;
    }
    vec![Line::from(vec![
        Span::styled(format!("{label:<LABEL$}"), ui.muted()),
        Span::styled(format!("{:<name_width$}  ", host.host_name), ui.text()),
        Span::styled(sessions, ui.text()),
        Span::styled(separator.to_owned(), ui.muted()),
        Span::styled(images, ui.muted()),
    ])]
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

/// `address` as shown: the route through a hub names the hub, as the user knows it,
/// in place of its host id; any other address is shown as it is.
fn route(app: &App, address: &str) -> String {
    let Some(hub) = address.strip_prefix("hub://") else {
        return address.to_owned();
    };
    match app.machines.iter().find(|m| m.host_id.as_str() == hub) {
        Some(hub) => format!("through {}", hub.name),
        None => "through a hub".to_owned(),
    }
}

/// What a route's choice says beside it; nothing while it is in the race.
fn choice_word(choice: RouteChoice) -> Option<&'static str> {
    match choice {
        RouteChoice::Auto => None,
        RouteChoice::Pinned => Some("only this"),
        RouteChoice::Disabled => Some("never"),
    }
}

/// What carries a connection to `address`: "home network", "through herder-hub".
pub(super) fn carrier(app: &App, address: &str) -> String {
    match herder_client_core::route_kind(address.to_owned()) {
        RouteKind::HomeNetwork => "home network".to_owned(),
        RouteKind::OwnVpn { .. } => "your VPN".to_owned(),
        RouteKind::Direct => "internet".to_owned(),
        RouteKind::SystemVpn => "system VPN".to_owned(),
        RouteKind::HubWireGuard => "hub WireGuard".to_owned(),
        RouteKind::Tunnel => "tunnel".to_owned(),
        RouteKind::Hub { .. } => route(app, address),
        RouteKind::CloudflareRelay => "Cloudflare relay".to_owned(),
    }
}

/// `machine`'s routes in the order they are tried, `at` selected: each numbered, named by
/// what carries it, its address, the user's choice and whether it is in use.
fn routes(ui: Ui, app: &App, machine: &Machine, at: usize) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::styled(
            format!("{} · routes on this device", machine.name),
            ui.strong(),
        ),
        Line::styled(
            "tried first to last when a connection starts; never moved mid-use",
            ui.muted(),
        ),
    ];
    let in_use = match machine.connection {
        ConnectionState::Connected => machine.address.as_deref(),
        _ => None,
    };
    for (index, address) in machine.addresses.iter().enumerate() {
        let choice = machines::choice(machine, address);
        let marker = if index == at { ui.glyphs.pointer } else { " " };
        let style = match (index == at, choice) {
            (true, _) => ui.accent(),
            (false, RouteChoice::Disabled) => ui.muted(),
            (false, _) => ui.text(),
        };
        let mut spans = vec![
            Span::styled(format!("{marker} {}. ", index + 1), style),
            Span::styled(format!("{:<18}", carrier(app, address)), style),
            // A route through a hub is named by the hub; its id says nothing more.
            Span::styled(
                if address.starts_with("hub://") {
                    String::new()
                } else {
                    address.clone()
                },
                ui.muted(),
            ),
        ];
        if let Some(word) = choice_word(choice) {
            spans.push(Span::styled(format!("  {word}"), style));
        }
        if in_use == Some(address.as_str()) {
            spans.push(Span::styled("  in use", Style::new().fg(ui.theme.success)));
        }
        if let Some(door) = machines::front_door(machine, address) {
            let style = if door == "signed in" {
                ui.muted()
            } else {
                Style::new().fg(ui.theme.warning)
            };
            spans.push(Span::styled(format!("  {door}"), style));
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// The selected machine in full: its name, then each fact, the values `width` wide.
fn details(
    ui: Ui,
    app: &App,
    panel: &MachinePanel,
    machine: &Machine,
    width: usize,
) -> Vec<Line<'static>> {
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
    if let Some(known) = panel.links.get(&machine.host_id) {
        let hub = match known {
            Known::Linked(linked) => app.paired_hub(linked),
            _ => None,
        };
        let style = match known {
            Known::Unlinked => Style::new().fg(ui.theme.warning),
            Known::Asking | Known::Unknown(_) => ui.muted(),
            Known::Hub | Known::Linked(_) => ui.text(),
        };
        let backup = backup::describe(known, hub);
        lines.extend(field(ui, "backup", &backup, style, width));
    }
    if is_hub(panel, machine) {
        lines.extend(hub_facts(ui, panel, machine, width));
    } else {
        lines.extend(machine_facts(ui, machine, width));
    }
    for (label, value) in hub_status(machine, app.now()) {
        lines.extend(field(ui, &label, &value, ui.text(), width));
    }
    lines.extend(field(
        ui,
        "host id",
        machine.host_id.as_str(),
        ui.text(),
        width,
    ));
    let addresses: Vec<String> = machine
        .addresses
        .iter()
        .map(
            |address| match choice_word(machines::choice(machine, address)) {
                Some(word) => format!("{} ({word})", route(app, address)),
                None => route(app, address),
            },
        )
        .collect();
    let addresses = addresses.join(", ");
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

/// A daemon's accounts and load, as details rows `width` wide.
fn machine_facts(ui: Ui, machine: &Machine, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
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

/// A hub's status as of `now`: what it holds, then each host's replication, labelled with
/// the host's name.
fn hub_status(machine: &Machine, now: Timestamp) -> Vec<(String, String)> {
    let Some(hub) = &machine.hub else {
        return Vec::new();
    };
    let count = |n: u64, one: &str| match n {
        1 => format!("1 {one}"),
        n => format!("{n} {one}s"),
    };
    let mut facts = vec![(
        "hub".to_owned(),
        format!(
            "{} · {} · {}",
            count(hub.sessions, "session"),
            count(hub.events, "event"),
            storage(hub.storage_bytes)
        ),
    )];
    for host in &hub.hosts {
        let name = machine
            .hosts
            .iter()
            .find(|fleet| fleet.host_id == host.host_id)
            .map_or_else(|| host.host_id.to_string(), |fleet| fleet.host_name.clone());
        let mut value = format!(
            "{} · {}",
            count(host.sessions, "session"),
            count(host.events, "event")
        );
        if let Some(at) = host.last_event_at {
            let ago = crate::account_screen::until(now.duration_since(at).as_secs());
            value.push_str(&format!(" · last event {ago} ago"));
        }
        if let Some(lag) = host.lag_ms {
            value.push_str(&format!(" · lag {lag} ms"));
        }
        facts.push((super::sessions::clip(&name, LABEL - 1), value));
    }
    facts
}

/// `bytes` in KiB below a MiB, else as host memory shows.
fn storage(bytes: u64) -> String {
    if bytes < 1 << 20 {
        format!("{} KiB", bytes >> 10)
    } else {
        super::resources::bytes(bytes)
    }
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
    let provider = account.provider();
    let mut top = if account.kind == accounts::Kind::Hub {
        wrapped(
            &format!(
                "Runs {} on {machine} on the provider logins in the CLIProxyAPI next to its \
                 hub, with the key the hub keeps for {machine}. Nothing to type.",
                provider.as_str()
            ),
            ui.muted(),
        )
    } else if account.kind == accounts::Kind::Endpoint {
        wrapped(
            &format!(
                "Runs {} on {machine} against an API server, such as a CLIProxyAPI or a local \
                 model server, with its client key. No login.",
                provider.as_str()
            ),
            ui.muted(),
        )
    } else {
        wrapped(
            &format!("Runs the provider's own login on {machine}, in a terminal here."),
            ui.muted(),
        )
    };
    top.push(Line::default());
    let mut bottom = vec![Line::default()];
    if account.kind == accounts::Kind::Hub {
        bottom.extend(wrapped(
            &format!("{machine} fetches its key from the hub when a session starts."),
            ui.muted(),
        ));
    } else if account.kind == accounts::Kind::Endpoint {
        bottom.extend(wrapped(
            &format!("The key is kept on {machine} and never shown again."),
            ui.muted(),
        ));
    } else {
        bottom.extend(wrapped(accounts::login_hint(provider), ui.muted()));
        bottom.extend(wrapped(
            "The account is added once the login succeeds. ctrl+] d detaches.",
            ui.muted(),
        ));
    }
    if let Some(error) = &account.error {
        bottom.push(Line::default());
        bottom.extend(super::failure(ui, error, body_width));
    }
    if account.sending {
        bottom.push(Line::default());
        bottom.push(Line::styled(
            format!("Adding{}", ui.glyphs.ellipsis),
            ui.muted(),
        ));
    }
    let fields = account.fields();
    let height = u16::try_from(top.len() + fields.len() + bottom.len()).unwrap_or(u16::MAX);
    let hints = [
        Hint::new(
            "enter",
            if account.sends_endpoint() {
                "add"
            } else {
                "log in"
            },
        ),
        Hint::new("tab", "field"),
        Hint::new("←/→", "choose"),
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
    let status = app
        .machines
        .iter()
        .find(|machine| machine.host_id == account.host_id)
        .and_then(|machine| {
            machine
                .providers
                .iter()
                .find(|status| status.provider == *provider)
        });
    let shown = match status {
        Some(status) if status.installed => match &status.version {
            Some(version) => format!("{}  {version}", provider.as_str()),
            None => format!("{}  installed", provider.as_str()),
        },
        Some(status) if status.can_install => {
            format!("{}  not installed", provider.as_str())
        }
        _ => provider.as_str().to_owned(),
    };
    let default_dir = account.default_config_dir();
    for field in fields {
        if y >= body.bottom() {
            break;
        }
        let at = Rect::new(body.x, y, body.width, 1);
        y += 1;
        let focused = account.focus == field;
        let (name, value, placeholder) = match field {
            accounts::Field::Provider => {
                Choice::new(ui, "provider", Span::styled(shown.clone(), ui.text()))
                    .focused(focused)
                    .render(at, buf, label);
                continue;
            }
            accounts::Field::Kind => {
                Choice::new(ui, "how", Span::styled(account.kind.name(), ui.text()))
                    .focused(focused)
                    .render(at, buf, label);
                continue;
            }
            accounts::Field::Id => ("id", account.id.clone(), "e.g. work"),
            accounts::Field::Label => ("label", account.label.clone(), "the id"),
            accounts::Field::ConfigDir => (
                "config dir",
                account.config_dir.clone(),
                default_dir.as_str(),
            ),
            accounts::Field::Url => ("url", account.url.clone(), "http://127.0.0.1:8317"),
            accounts::Field::Key => (
                "key",
                super::secrets::masked(ui, account.key.len()),
                "typed hidden, never shown again",
            ),
        };
        let mut editor: TextArea = search_line(&value, placeholder);
        TextField::new(ui, name, &mut editor)
            .focused(focused)
            .render(at, buf, label);
    }
    for text in bottom {
        line(text, &mut y, buf);
    }
}
