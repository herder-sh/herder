//! The accounts screen, over the main screen: each machine with its failover settings and its
//! accounts, and how much of each usage window they used, with a bar and when it resets.

use herder_client_core::Machine;
use herder_protocol::{Account, SessionStatus, Timestamp, UsageWindow};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Padding, Paragraph};

use crate::account_screen::{self, AccountScreen, Pick};
use crate::app::App;
use crate::mouse::{Click, Hits, List as Rows};

/// The widest a usage bar gets.
const BAR: usize = 30;

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
    let narrow = area.width < super::NARROW;
    let keys = if narrow {
        " n add  r reconnect  Esc close "
    } else {
        " j/k move  n add an account  r reconnect  Esc close "
    };
    let block = Block::bordered()
        .title(Line::styled(" accounts ", super::bold()))
        .title_bottom(Line::styled(keys, super::dim()).centered())
        .border_style(Style::new().fg(Color::Cyan))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let footer: Vec<Line> = textwrap::wrap(FAILOVER, usize::from(inner.width).max(8))
        .into_iter()
        .map(|part| Line::styled(part.into_owned(), super::dim()))
        .collect();
    let footer_height = u16::try_from(footer.len())
        .unwrap_or(u16::MAX)
        .min(inner.height / 3);
    let [list, _, footer_area] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(footer_height),
    ])
    .areas(inner);

    let rows = account_screen::rows(&app.machines);
    let selected = screen.selected(&rows);
    let width = usize::from(list.width);
    let now = Timestamp::now();
    // Each item with its row.
    let items: Vec<(usize, ListItem)> = rows
        .iter()
        .enumerate()
        .filter_map(|(at, pick)| {
            let chosen = selected == Some(at);
            let machine = app
                .machines
                .iter()
                .find(|machine| machine.host_id == *pick.host_id())?;
            let text = match pick {
                Pick::Machine(_) => machine_text(machine, chosen),
                Pick::Account(_, account_id) => {
                    let account = machine
                        .accounts
                        .iter()
                        .find(|account| account.account_id == *account_id)?;
                    account_text(app, machine, account, chosen, width, now)
                }
            };
            Some((at, ListItem::new(text)))
        })
        .collect();
    let heights: Vec<usize> = items.iter().map(|(_, item)| item.height()).collect();
    let (picks, items): (Vec<usize>, Vec<ListItem>) = items.into_iter().unzip();
    let selected = selected.and_then(|at| picks.iter().position(|pick| *pick == at));
    let mut state = ListState::default().with_selected(selected);
    frame.render_stateful_widget(List::new(items), list, &mut state);
    if screen.adding.is_none() {
        hits.list(list, state.offset(), &heights, |at| {
            Some(Click::Row(Rows::Accounts, picks[at]))
        });
    }
    frame.render_widget(Paragraph::new(footer), footer_area);

    if let Some(adding) = &screen.adding {
        super::machines::account_dialog(frame, area, app, adding);
    }
}

/// The style of a row's name: reversed while it is selected.
fn name_style(chosen: bool) -> Style {
    if chosen {
        super::bold().reversed()
    } else {
        super::bold()
    }
}

fn machine_text(machine: &Machine, chosen: bool) -> Text<'static> {
    let (mark, color, state) = super::machines::connection(machine);
    let mut facts = format!("  {state}");
    if machine.failover.pin {
        facts.push_str(" · pinned");
    }
    let mut lines = vec![Line::from(vec![
        Span::styled(mark, Style::new().fg(color)),
        Span::raw(" "),
        Span::styled(machine.name.clone(), name_style(chosen)),
        Span::styled(facts, super::dim()),
    ])];
    if machine.accounts.is_empty() {
        lines.push(Line::styled("  no accounts: n adds one", super::dim()));
    }
    Text::from(lines)
}

fn account_text(
    app: &App,
    machine: &Machine,
    account: &Account,
    chosen: bool,
    width: usize,
    now: Timestamp,
) -> Text<'static> {
    let sessions = app
        .sessions
        .iter()
        .filter(|(key, session)| {
            key.host_id == machine.host_id
                && session.account_id.as_ref() == Some(&account.account_id)
                && session.status != SessionStatus::Archived
        })
        .count();
    let mut facts = account.provider.as_str().to_owned();
    if account.label != account.account_id.as_str() {
        facts.push_str(&format!(" · {}", account.account_id));
    }
    match sessions {
        0 => {}
        1 => facts.push_str(" · 1 session"),
        n => facts.push_str(&format!(" · {n} sessions")),
    }
    let mut lines = vec![Line::from(vec![
        Span::raw("  "),
        Span::styled(account.label.clone(), name_style(chosen)),
        Span::styled(format!("  {facts}"), super::dim()),
    ])];
    if account.usage.is_empty() {
        lines.push(Line::styled("    no usage reported yet", super::dim()));
    }
    let label_width = account
        .usage
        .iter()
        .map(|usage| account_screen::window_label(&usage.window).chars().count())
        .max()
        .unwrap_or(0);
    for usage in &account.usage {
        lines.extend(window_lines(usage, label_width, width, now));
    }
    Text::from(lines)
}

/// A usage window: its name, a bar, the share used and when it resets; on two lines when one
/// leaves the bar too little room.
fn window_lines(
    usage: &UsageWindow,
    label_width: usize,
    width: usize,
    now: Timestamp,
) -> Vec<Line<'static>> {
    let label = account_screen::window_label(&usage.window);
    let percent = usage.used_percent.clamp(0.0, 100.0);
    let color = if percent >= 90.0 {
        Color::Red
    } else if percent >= 70.0 {
        Color::Yellow
    } else {
        Color::Cyan
    };
    let used = Span::styled(format!("{percent:>3.0}%"), Style::new().fg(color));
    let resets = usage.resets_at.map_or_else(String::new, |at| {
        let secs = at.duration_since(now).as_secs();
        format!("resets in {}", account_screen::until(secs))
    });
    let label = Span::raw(format!("    {label:<label_width$}  "));
    // The indent, the label, the gaps, the share and the reset.
    let fixed = 4 + label_width + 2 + 2 + 4 + 2 + resets.chars().count();
    let room = width.saturating_sub(fixed).min(BAR);
    if room >= 10 {
        let mut spans = vec![label];
        spans.extend(bar(percent, room, color));
        spans.extend([
            Span::raw("  "),
            used,
            Span::styled(format!("  {resets}"), super::dim()),
        ]);
        return vec![Line::from(spans)];
    }
    // Two lines: the name and share, then the bar and the reset.
    // Bars line up whatever the reset says: `resets in 23h 59m` is the longest.
    let room = width.saturating_sub(4 + 2 + 17).min(BAR);
    let mut second = vec![Span::raw("    ")];
    second.extend(bar(percent, room, color));
    second.push(Span::styled(format!("  {resets}"), super::dim()));
    vec![Line::from(vec![label, used]), Line::from(second)]
}

/// A bar `width` cells wide, filled to `percent`.
fn bar(percent: f64, width: usize, color: Color) -> [Span<'static>; 2] {
    // `as` saturates, and `percent` is clamped to 0..=100.
    let filled = ((percent / 100.0 * width as f64).round() as usize).min(width);
    [
        Span::styled("█".repeat(filled), Style::new().fg(color)),
        Span::styled("░".repeat(width - filled), super::dim()),
    ]
}
