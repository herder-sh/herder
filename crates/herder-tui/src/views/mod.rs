//! Drawing: one module per area of the screen, each a `draw` function over the app state.
//!
//! [`draw`] lays the screen out and hands each area to its module. Views read the app and
//! write back only what layout decides, such as how many transcript lines fit.

mod composer;
mod help;
mod inbox;
mod machines;
mod new_session;
mod pairing;
mod palette;
mod prs;
mod sessions;
mod status;
mod terminals;
mod transcript;

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};

use crate::app::{App, Focus};

/// Draws the whole screen.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let [body, status_line] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    if app.machines.is_empty() {
        pairing::draw(frame, body);
    } else {
        let list_width = (body.width / 3).clamp(30, 44);
        let [list, main] =
            Layout::horizontal([Constraint::Length(list_width), Constraint::Fill(1)]).areas(body);
        sessions::draw(frame, list, app);
        if app.focus == Focus::AllPrs {
            prs::all(frame, main, app);
        } else if app.focus == Focus::Inbox {
            inbox::draw(frame, main, app);
        } else {
            let (main, controls) = composer::split(main, app);
            let [strip, main] = Layout::vertical([
                Constraint::Length(prs::strip_height(app)),
                Constraint::Fill(1),
            ])
            .areas(main);
            prs::strip(frame, strip, app);
            transcript::draw(frame, main, app);
            if let Some(controls) = controls {
                composer::draw(frame, controls, app);
            }
        }
    }
    if !palette::draw(frame, status_line, app) {
        status::draw(frame, status_line, app);
    }
    new_session::draw(frame, area, app);
    if let Some(panel) = &app.machine_panel {
        machines::draw(frame, body, app, panel);
    }
    terminals::draw(frame, body, app);
    if app.help {
        help::draw(frame, area);
    }
    prs::prompt(frame, area, app);
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
