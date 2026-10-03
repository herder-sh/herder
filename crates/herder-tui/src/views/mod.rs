//! Drawing: one module per area of the screen, each a `draw` function over the app state.
//!
//! [`draw`] lays the frame out, as Herdr's, and hands each area to its module. Views read the
//! app and write back only what layout decides, such as how many transcript lines fit.
//!
//! ```text
//!  herder         inbox 2 «│  chat   tasks 2   prs 1          claude-main · opus · ask │ api
//!                          │                                                          │ ...
//!  projects   (sidebar)    │  (main pane: the open session's tab, or a view)          │ (details,
//!  ...                     │                                                          │  ≥ 120)
//! ─────────────────────────┴──────────────────────────────────────────────────────────┴────────
//!  PROMPT  enter send  esc navigate  ctrl+x leader                            ● box  ● m2
//! ```
//!
//! - From 65 columns: the sidebar ([`sessions`]), the main pane, and from
//!   [`crate::nav::WIDE`] columns the details panel ([`details`]); the mode bar
//!   ([`status`]) at the bottom.
//! - At 64 columns or fewer ([`NARROW`]), as on a phone, Herdr's mobile layout: a two-row
//!   header with the `switch` button ([`phone`]), one pane full width (the switcher, or what
//!   it opened), and a bar of buttons ([`touch`]) as the last row.
//!
//! Each view records where its taps and swipes land ([`crate::mouse::Hits`]) as it draws;
//! dialogs cover what they hide.
//!
//! The last column stays blank: a terminal that has just written there may wrap at the next
//! character or not, and a phone SSH app over mosh does not always agree with mosh, which
//! shifts the rows below. With the ASCII glyph set the finished frame goes through
//! [`crate::ui::glyphs::fold`].

mod accounts;
mod add_machine;
mod composer;
mod details;
mod help;
mod inbox;
mod machines;
mod markdown;
mod new_session;
mod pairing;
mod palette;
mod phone;
mod projects;
mod prs;
mod recover;
mod resources;
mod sessions;
mod status;
mod switch;
mod tabs;
mod terminals;
mod tools;
mod touch;
mod transcript;

use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::{Frame, Terminal};

use crate::app::{App, Focus};
use crate::mouse::{Click, Hits};
use crate::nav::{self, Tab};
use crate::ui::glyphs::{self, Glyphs};

/// Screens drawn narrower than this show one pane at a time: 64 columns or fewer, as Herdr's
/// mobile layout, since the last column stays blank.
pub const NARROW: u16 = 64;

/// Draws the screen; with `full`, onto a cleared screen with nothing assumed of the last
/// frame.
///
/// While resizing, the terminal may reflow or scroll what it shows, and a resize that ends at
/// the size of the last draw, as a phone keyboard opening and closing between two draws, does
/// not set off ratatui's own clear. Either leaves stale rows a diff against the last frame
/// never touches, so every resize repaints every cell. So does the first frame, over what the
/// shell left on a terminal without an alternate screen, as under mosh, and a frame now and
/// then, over whatever a terminal got wrong since.
pub fn paint<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    full: bool,
) -> Result<(), B::Error> {
    if full {
        let size = terminal.size()?;
        terminal.resize(Rect::new(0, 0, size.width, size.height))?;
    }
    terminal.draw(|frame| draw(frame, app))?;
    Ok(())
}

/// Draws the whole screen.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let mut hits = Hits::default();
    let screen = frame.area();
    let area = Rect {
        width: screen.width.saturating_sub(1),
        ..screen
    };
    let narrow = area.width < NARROW;
    app.width = area.width;
    frame.render_widget(Block::new().style(app.ui().base()), screen);
    // The header's and the bar's taps, which stay over the dialogs.
    let mut touch = Hits::default();
    // Dialogs go over the body; the accounts and the fleet, views, over the main pane.
    let (body, main) = if narrow {
        let body = phone_frame(frame, area, app, &mut hits, &mut touch);
        (body, body)
    } else {
        desktop_frame(frame, area, app, &mut hits, &mut touch)
    };
    // A dialog takes taps for itself and what it hides; the header and the bar stay.
    if app.dialog_open() {
        hits.cover(area);
    }
    if let Some(screen) = &app.account_screen {
        accounts::draw(frame, main, app, screen, &mut hits);
    }
    switch::draw(frame, body, app, &mut hits);
    recover::draw(frame, body, app, &mut hits);
    if let Some(panel) = &app.machine_panel {
        machines::draw(frame, main, app, panel, &mut hits);
    }
    terminals::draw(frame, body, app, &mut hits);
    new_session::draw(frame, area, app, &mut hits);
    palette::draw(frame, area, app, &mut hits);
    if app.help {
        help::draw(frame, area, app, &mut hits);
    }
    prs::prompt(frame, area, app);
    status::leader_popup(frame, body, app);
    hits.append(touch);
    app.hits = hits;
    if Glyphs::for_width(app.glyphs, area.width) == Glyphs::Ascii {
        glyphs::fold(frame.buffer_mut());
    }
}

/// The desktop frame: the sidebar, the main pane and on a wide screen the details panel, a
/// rule, and the mode bar. Returns the body, where dialogs go, and the main pane.
fn desktop_frame(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    hits: &mut Hits,
    touch: &mut Hits,
) -> (Rect, Rect) {
    let [body, rule, bar] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    // No button bar: Tab moves between panes.
    if !app.bar.is_empty() {
        app.bar.clear();
        app.bar_focus = None;
    }
    if !palette::quit_hint(frame, bar, app) {
        status::draw(frame, bar, app, touch);
    }
    if app.machines.is_empty() {
        pairing::draw(frame, body);
        rule_with_joints(frame, rule, &[], app.ui());
        return (body, body);
    }
    let sidebar = if app.layout.collapsed {
        nav::STRIP
    } else {
        app.layout.sidebar.min(body.width / 2)
    };
    let [side, side_edge, rest] = Layout::horizontal([
        Constraint::Length(sidebar.saturating_sub(1)),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(body);
    // Counting the last column, which stays blank.
    let wide = area.width + 1 >= nav::WIDE;
    // On a wide screen the details sit beside the main pane unless toggled away; on a
    // narrower one, `ctrl+x d` lays them over it.
    let details = app.open.is_some() && wide != app.layout.details;
    let (main, details_area) = if details && wide {
        let [main, panel] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(nav::DETAILS)]).areas(rest);
        (main, Some(panel))
    } else {
        (rest, None)
    };
    sessions::draw(frame, side, app, hits);
    edge(frame, side_edge, app.ui());
    let mut joints = vec![side_edge.x];
    if !app.layout.collapsed {
        hits.click(side_edge, Click::Resize);
    }
    main_pane(frame, inset(main), app, false, details, hits);
    if let Some(panel) = details_area {
        details::draw(frame, panel, app, false);
        joints.push(panel.x);
    } else if details {
        let width = nav::DETAILS.min(main.width);
        let panel = Rect {
            x: main.right() - width,
            width,
            ..main
        };
        details::draw(frame, panel, app, true);
        joints.push(panel.x);
    }
    rule_with_joints(frame, rule, &joints, app.ui());
    (body, inset(main))
}

/// The rule over the bar, joining the separators above it at `joints`.
fn rule_with_joints(frame: &mut Frame, rule: Rect, joints: &[u16], ui: crate::ui::Ui) {
    let buf = frame.buffer_mut();
    for x in rule.left()..rule.right() {
        let symbol = if joints.contains(&x) { "┴" } else { "─" };
        if let Some(cell) = buf.cell_mut((x, rule.y)) {
            cell.set_symbol(symbol).set_fg(ui.theme.border);
        }
    }
}

/// The phone frame: the two-row header and its rule, one pane full width, a notice, and the
/// button bar. Returns the body, where dialogs go.
fn phone_frame(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    hits: &mut Hits,
    touch: &mut Hits,
) -> Rect {
    let [header, rule, body, notice, bar] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(u16::from(app.notice.is_some())),
        Constraint::Length(1),
    ])
    .areas(area);
    // The bar's buttons, then the header's switch, which Shift-Tab reaches first.
    let mut clicks = touch::clicks(app);
    let switch = phone::has_switch(app);
    if switch {
        clicks.push(Click::Act(crate::action::Action::GoTo));
    }
    // Tab's focus stays on a button only while the bar stays the same.
    if clicks != app.bar {
        app.bar = clicks;
        app.bar_focus = None;
    }
    let switch_focused = switch && app.bar_focus == Some(app.bar.len() - 1);
    phone::header(frame, header, app, switch_focused, touch);
    let border = Style::new().fg(app.theme.border);
    frame.render_widget(
        Span::styled("─".repeat(usize::from(rule.width)), border),
        rule,
    );
    if app.machines.is_empty() {
        pairing::draw(frame, body);
    } else if app.focus == Focus::Sessions {
        sessions::switcher(frame, body, app, hits);
    } else {
        main_pane(frame, body, app, true, false, hits);
    }
    let ui = app.ui();
    if let Some(text) = &app.notice {
        let line = Line::styled(text.clone(), Style::new().fg(ui.theme.warning));
        frame.render_widget(
            crate::ui::fit(line, usize::from(notice.width.saturating_sub(2)), ui.glyphs),
            Rect {
                x: notice.x + 1,
                width: notice.width.saturating_sub(2),
                ..notice
            },
        );
    }
    if !palette::quit_hint(frame, bar, app) {
        touch::bar(frame, bar, app, touch);
    }
    body
}

/// The main pane: every session's PRs, the inbox, or the open session's tab under the tab
/// row; `compact` on a phone, which has no tabs. `details` says whether the details panel
/// shows the session's resources, so the chat need not.
fn main_pane(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    compact: bool,
    details: bool,
    hits: &mut Hits,
) {
    if app.focus == Focus::AllPrs {
        prs::all(frame, area, app, compact, hits);
        return;
    }
    if app.focus == Focus::Inbox {
        inbox::draw(frame, area, app, compact, hits);
        return;
    }
    if app.open_session().is_none() {
        empty(frame, area, app);
        return;
    }
    // Under 20 rows the tab row goes; `p` and `t` still reach the tabs.
    let area = if compact || area.height < 20 {
        area
    } else {
        let [tab_row, _, rest] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(area);
        tabs::row(frame, tab_row, app, hits);
        rest
    };
    match app.tab() {
        Tab::Tasks => tabs::tasks(frame, area, app, hits),
        // Compact rows where the full ones would cut the titles.
        Tab::Prs => prs::strip(frame, area, app, compact || area.width < 80, hits),
        Tab::Chat | Tab::Term => chat(frame, area, app, compact, details, hits),
    }
}

/// The chat tab: the transcript, the controls under it, and the session's resources over it
/// while the details panel does not show them.
fn chat(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    compact: bool,
    details: bool,
    hits: &mut Hits,
) {
    let (area, controls) = composer::split(area, app, compact, frame.area().height);
    let usage = if details {
        0
    } else {
        resources::strip_height(app, compact)
    };
    let [usage_area, area] =
        Layout::vertical([Constraint::Length(usage), Constraint::Fill(1)]).areas(area);
    resources::strip(frame, usage_area, app, compact);
    transcript::draw(frame, area, app, hits);
    if let Some((controls, heights)) = controls {
        composer::draw(frame, controls, heights, app, compact, hits);
    }
}

/// The main pane with no session open: what to do next.
fn empty(frame: &mut Frame, area: Rect, app: &App) {
    let ui = app.ui();
    let key = |key: &'static str, what: &'static str| {
        Line::from(vec![
            Span::styled(format!("{key:>7}"), ui.strong()),
            Span::styled(format!("  {what}"), ui.muted()),
        ])
    };
    let lines = vec![
        Line::styled("No session open", ui.strong()),
        Line::default(),
        key("enter", "open the selected session"),
        key("n", "start a new session"),
        key("I", "inbox: what waits on you"),
        key("ctrl+x", "leader: every view, from anywhere"),
        key("?", "every key"),
    ];
    let width = 44.min(area.width);
    let height = u16::try_from(lines.len()).unwrap_or(0);
    frame.render_widget(Paragraph::new(lines), centered(area, width, height));
}

/// `area` one column in on either side.
fn inset(area: Rect) -> Rect {
    Rect {
        x: area.x + crate::ui::INSET,
        width: area.width.saturating_sub(2 * crate::ui::INSET),
        ..area
    }
}

/// A vertical separator down `area`'s first column.
fn edge(frame: &mut Frame, area: Rect, ui: crate::ui::Ui) {
    let buf = frame.buffer_mut();
    for y in area.top()..area.bottom() {
        if let Some(cell) = buf.cell_mut((area.x, y)) {
            cell.set_symbol("│").set_fg(ui.theme.border);
        }
    }
}

/// The border style of a pane: highlighted while it has focus.
fn border(app: &App, pane: Focus) -> Style {
    app.ui().border(app.focus == pane)
}

/// Style for secondary text.
fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

/// Style for headings.
fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// A `width` by `height` rect centred in `area`, clipped to it.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [area] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    area
}

#[cfg(test)]
mod screenshots;
#[cfg(test)]
mod tests;
