//! Drawing: one module per area of the screen, each a `draw` function over the app state.
//!
//! [`draw`] lays the screen out and hands each area to its module. Views read the app and
//! write back only what layout decides, such as how many transcript lines fit.
//!
//! Below [`NARROW`] columns, as on a phone, the screen shows one pane at a time: the session
//! list, or what it opened, full width. Rows and titles there are compact.

mod accounts;
mod composer;
mod help;
mod inbox;
mod machines;
mod new_session;
mod pairing;
mod palette;
mod projects;
mod prs;
mod resources;
mod sessions;
mod status;
mod switch;
mod terminals;
mod transcript;

use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::{Frame, Terminal};

use crate::app::{App, Focus};

/// Screens narrower than this show one pane at a time.
pub const NARROW: u16 = 80;

/// Draws the screen; with `resized`, onto a cleared screen with nothing assumed of the last
/// frame.
///
/// While resizing, the terminal may reflow or scroll what it shows, and a resize that ends at
/// the size of the last draw, as a phone keyboard opening and closing between two draws, does
/// not set off ratatui's own clear. Either leaves stale rows a diff against the last frame
/// never touches, so every resize repaints every cell.
pub fn paint<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    resized: bool,
) -> Result<(), B::Error> {
    if resized {
        let size = terminal.size()?;
        terminal.resize(Rect::new(0, 0, size.width, size.height))?;
    }
    terminal.draw(|frame| draw(frame, app))?;
    Ok(())
}

/// Draws the whole screen.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let narrow = area.width < NARROW;
    let [body, status_line] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    if app.machines.is_empty() {
        pairing::draw(frame, body);
    } else if narrow && app.focus == Focus::Sessions {
        sessions::draw(frame, body, app, true);
    } else if narrow {
        main(frame, body, app, true);
    } else {
        let list_width = (body.width / 3).clamp(30, 44);
        let [list, rest] =
            Layout::horizontal([Constraint::Length(list_width), Constraint::Fill(1)]).areas(body);
        sessions::draw(frame, list, app, false);
        main(frame, rest, app, false);
    }
    if !palette::draw(frame, status_line, app) {
        status::draw(frame, status_line, app, narrow);
    }
    new_session::draw(frame, area, app);
    if let Some(screen) = &app.account_screen {
        accounts::draw(frame, body, app, screen);
    }
    switch::draw(frame, body, app);
    if let Some(panel) = &app.machine_panel {
        machines::draw(frame, body, app, panel);
    }
    terminals::draw(frame, body, app);
    if app.help {
        help::draw(frame, area, app);
    }
    prs::prompt(frame, area, app);
}

/// The main pane: every session's PRs, the inbox, or the open session; `compact` on a narrow
/// screen.
fn main(frame: &mut Frame, area: Rect, app: &mut App, compact: bool) {
    if app.focus == Focus::AllPrs {
        prs::all(frame, area, app, compact);
    } else if app.focus == Focus::Inbox {
        inbox::draw(frame, area, app, compact);
    } else {
        let (area, controls) = composer::split(area, app);
        let [strip, usage, area] = Layout::vertical([
            Constraint::Length(prs::strip_height(app)),
            Constraint::Length(resources::strip_height(app, compact)),
            Constraint::Fill(1),
        ])
        .areas(area);
        prs::strip(frame, strip, app, compact);
        resources::strip(frame, usage, app, compact);
        transcript::draw(frame, area, app, compact);
        if let Some(controls) = controls {
            composer::draw(frame, controls, app, compact);
        }
    }
}

/// The border style of a pane: highlighted while it has focus.
fn border(app: &App, pane: Focus) -> Style {
    if app.focus == pane {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    }
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
mod tests;
