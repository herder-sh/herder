//! The accounts view, in the main pane: each machine and its accounts, which ones sessions
//! fail over to, and how much of each usage window they used, with a bar and when it resets.
//!
//! ```text
//!  accounts                                              rotates on a limit only
//!
//!  ● box                                                          sessions pinned
//! ▶  claude-main  claude                                                3 sessions
//!      Session         ███████░░░░░░░░░░░░░  38%   resets in 2h 13m
//!      Weekly          ██░░░░░░░░░░░░░░░░░░  12%   resets in 5d 3h
//! ```

use herder_client_core::{Machine, provider_hints};
use herder_protocol::{Account, HostId, SessionStatus, Timestamp, UsageWindow};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::account_screen::{self, AccountScreen, Pick};
use crate::app::App;
use crate::mouse::{Click, Hits, List as Rows, Wheel};
use crate::ui::list::{ListView, Row};
use crate::ui::{GAP, Ui, usage};

/// The widest a usage bar gets.
const BAR: u16 = 24;

/// The narrowest a usage bar gets beside its reset time; narrower, the reset goes under it.
const MIN_BAR: u16 = 8;

/// How failover works, and where pinning is set.
const FAILOVER: &str = "A session whose account hits a limit rotates to the account of the same \
                        provider with the most room left, on the same model, unless sessions \
                        are pinned. Pinning is set in the machine's daemon config \
                        ([failover] pin).";

pub(super) fn draw(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    screen: &AccountScreen,
    hits: &mut Hits,
) {
    let ui = app.ui();
    let compact = app.width < super::NARROW;
    super::clear(frame, area, ui);
    let area = super::heading(
        frame,
        area,
        ui,
        "accounts",
        "",
        "rotates on a limit only",
        compact,
    );

    let picks = account_screen::rows(&app.machines);
    let selected = screen.selected(&picks);
    // Past the pointer and the body's indent, and the right inset.
    let width = area.width.saturating_sub(5);
    let now = app.now();
    // Every window's label and reset take the widest one's room, so the bars line up.
    let windows = app
        .machines
        .iter()
        .flat_map(|machine| &machine.accounts)
        .flat_map(|account| &account.usage);
    let columns = Columns {
        label: windows
            .clone()
            .map(|usage| crate::ui::width(&account_screen::window_label(&usage.window)))
            .max()
            .unwrap_or(0),
        resets: windows
            .filter_map(|usage| usage.resets_at)
            .map(|at| crate::ui::width(&resets(at, now, compact)))
            .max()
            .unwrap_or(0),
        compact,
    };
    let mut rows = Vec::new();
    // The pick of each row; gaps have none.
    let mut of_row = Vec::new();
    let mut selected_row = None;
    let mut hinted: Option<HostId> = None;
    for (at, pick) in picks.iter().enumerate() {
        let Some(machine) = app.machines.iter().find(|m| m.host_id == *pick.host_id()) else {
            continue;
        };
        if matches!(pick, Pick::Machine(_))
            && let Some(host_id) = hinted.take()
            && let Some(previous) = app.machines.iter().find(|m| m.host_id == host_id)
        {
            push_hints(ui, previous, &app.machines, &mut rows, &mut of_row);
        }
        let row = match pick {
            Pick::Machine(_) => {
                if !rows.is_empty() {
                    rows.push(Row::Gap);
                    of_row.push(None);
                }
                hinted = Some(machine.host_id.clone());
                machine_row(ui, machine)
            }
            Pick::Account(_, account_id) => {
                let Some(account) = machine
                    .accounts
                    .iter()
                    .find(|account| account.account_id == *account_id)
                else {
                    continue;
                };
                account_row(app, ui, machine, account, width, now, columns)
            }
        };
        if selected == Some(at) {
            selected_row = Some(rows.len());
        }
        rows.push(row);
        of_row.push(Some(at));
    }
    if let Some(host_id) = hinted
        && let Some(previous) = app.machines.iter().find(|m| m.host_id == host_id)
    {
        push_hints(ui, previous, &app.machines, &mut rows, &mut of_row);
    }

    // The note on failover follows the list, or sits at the bottom once the list fills the
    // pane.
    let note: Vec<Line> =
        textwrap::wrap(FAILOVER, usize::from(area.width.saturating_sub(2)).max(8))
            .into_iter()
            .map(|part| Line::styled(part.into_owned(), ui.muted()))
            .collect();
    let note_height = u16::try_from(note.len()).unwrap_or(u16::MAX);
    let total = u16::try_from(rows.iter().map(Row::height).sum::<usize>()).unwrap_or(u16::MAX);
    let room = area.height.saturating_sub(note_height + 1);
    let list = Rect {
        height: total.min(room),
        ..area
    };
    let mut offset = 0;
    let placed = ListView::new(ui, rows)
        .select(selected_row)
        .focused(screen.adding.is_none())
        .render(list, frame.buffer_mut(), &mut offset);
    if screen.adding.is_none() {
        hits.wheel(area, Wheel::Keys);
        for (row, rect) in placed {
            if let Some(Some(at)) = of_row.get(row) {
                hits.click(rect, Click::Row(Rows::Accounts, *at));
            }
        }
    }
    if area.height > list.height + 1 {
        let note_area = Rect {
            x: area.x + crate::ui::INSET,
            y: list.bottom() + 1,
            width: area.width.saturating_sub(2 * crate::ui::INSET),
            height: area.bottom() - list.bottom() - 1,
        };
        frame.render_widget(Paragraph::new(note), note_area);
    }

    if let Some(adding) = &screen.adding {
        super::machines::account_dialog(frame, area, app, adding);
    }
}

/// A machine: its connection and name, whether it pins sessions to their account, and why it
/// is not connected.
fn machine_row(ui: Ui, machine: &Machine) -> Row<'static> {
    let (mark, color) = super::add_machine::connection_mark(ui, machine);
    let left = Line::from(vec![
        Span::styled(mark, ratatui::style::Style::new().fg(color)),
        Span::raw(" "),
        Span::styled(machine.name.clone(), ui.strong()),
    ]);
    let mut right = Vec::new();
    if let herder_client_core::ConnectionState::Disconnected { error } = &machine.connection {
        right.push(Span::styled(error.clone(), ui.muted()));
    }
    if machine.failover.pin {
        right.push(Span::styled("sessions pinned", ui.muted()));
    }
    let mut row = Row::item(left).right(Line::from(ui.joined(right)));
    if machine.accounts.is_empty() {
        row = row.body(vec![Line::styled("no accounts: n adds one", ui.muted())]);
    }
    row
}

/// The widths the usage windows share, and whether they are drawn on a phone.
#[derive(Clone, Copy)]
struct Columns {
    label: usize,
    resets: usize,
    compact: bool,
}

fn account_row(
    app: &App,
    ui: Ui,
    machine: &Machine,
    account: &Account,
    width: u16,
    now: Timestamp,
    columns: Columns,
) -> Row<'static> {
    let sessions = app
        .sessions
        .iter()
        .filter(|(key, session)| {
            key.host_id == machine.host_id
                && session.account_id.as_ref() == Some(&account.account_id)
                && session.status != SessionStatus::Archived
        })
        .count();
    let mut facts = vec![Span::styled(
        account.provider.as_str().to_owned(),
        ui.muted(),
    )];
    if account.label != account.account_id.as_str() && !columns.compact {
        facts.push(Span::styled(account.account_id.to_string(), ui.muted()));
    }
    let mut left = vec![
        Span::raw("  "),
        Span::styled(account.label.clone(), ui.text()),
        Span::raw(" ".repeat(GAP)),
    ];
    left.extend(ui.joined(facts));
    let right = match sessions {
        0 => String::new(),
        1 => "1 session".to_owned(),
        n => format!("{n} sessions"),
    };
    let mut body = Vec::new();
    if account.usage.is_empty() {
        body.push(Line::styled("  no usage reported yet", ui.muted()));
    }
    for window in &account.usage {
        body.extend(window_lines(ui, window, columns, width, now));
    }
    Row::item(Line::from(left))
        .right(Span::styled(right, ui.muted()))
        .body(body)
}

/// When a window resets, from `now`: `resets in 2h 13m`, or on a phone `2h 13m`.
fn resets(at: Timestamp, now: Timestamp, compact: bool) -> String {
    let until = account_screen::until(at.duration_since(now).as_secs());
    if compact {
        until
    } else {
        format!("resets in {until}")
    }
}

/// A usage window: its name, a bar with the share used, and when it resets; the reset on a
/// line of its own when a bar beside it would be too short.
fn window_lines(
    ui: Ui,
    window: &UsageWindow,
    columns: Columns,
    width: u16,
    now: Timestamp,
) -> Vec<Line<'static>> {
    let label = account_screen::window_label(&window.window);
    let label_width = columns.label;
    // Clamped to 0..=100 first, so the cast cannot wrap.
    let percent = window.used_percent.clamp(0.0, 100.0).round() as u8;
    let resets = window.resets_at.map(|at| resets(at, now, columns.compact));
    let label_span = Span::styled(format!("  {label:<label_width$}  "), ui.muted());
    // The indent and gap round the label, and the share after the bar.
    let fixed = u16::try_from(2 + label_width + 2 + 5).unwrap_or(u16::MAX);
    let resets_width = u16::try_from(GAP + columns.resets).unwrap_or(u16::MAX);
    let beside = width.saturating_sub(fixed + resets_width);
    let mut first = vec![label_span];
    if beside >= MIN_BAR {
        first.extend(usage::bar(ui, percent, beside.min(BAR)).spans);
        if let Some(resets) = resets {
            first.push(Span::styled(
                format!("{}{resets}", " ".repeat(GAP)),
                ui.muted(),
            ));
        }
        return vec![Line::from(first)];
    }
    let bar = width.saturating_sub(fixed).clamp(1, BAR);
    first.extend(usage::bar(ui, percent, bar).spans);
    let mut lines = vec![Line::from(first)];
    if let Some(resets) = resets {
        lines.push(Line::styled(
            format!("  {:<label_width$}  {resets}", ""),
            ui.muted(),
        ));
    }
    lines
}

fn push_hints(
    ui: Ui,
    machine: &Machine,
    machines: &[Machine],
    rows: &mut Vec<Row<'static>>,
    of_row: &mut Vec<Option<usize>>,
) {
    for hint in provider_hints(machine, machines) {
        rows.push(Row::item(Line::styled(hint.text(), ui.muted())));
        of_row.push(None);
    }
}
