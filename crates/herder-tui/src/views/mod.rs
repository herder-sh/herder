//! Drawing: one module per area of the screen, each a `draw` function over the app state.
//!
//! [`draw`] lays the screen out and hands each area to its module. Views read the app and
//! write back only what layout decides, such as how many transcript lines fit.
//!
//! At 64 columns or fewer ([`NARROW`]), as on a phone, the screen shows one pane at a time: the session
//! list, or what it opened, full width. Rows and titles there are compact.
//!
//! A tappable header tops the screen; on a narrow screen a bar of buttons for what can be done
//! now sits over the status line ([`touch`]). Each view records where its taps and swipes land
//! ([`crate::mouse::Hits`]) as it draws; dialogs cover what they hide.
//!
//! The last column stays blank: a terminal that has just written there may wrap at the next
//! character or not, and a phone SSH app over mosh does not always agree with mosh, which
//! shifts the rows below. With the ASCII glyph set the finished frame goes through
//! [`crate::ui::glyphs::fold`].

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
mod recover;
mod resources;
mod sessions;
mod status;
mod switch;
mod terminals;
mod touch;
mod transcript;

use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Block;
use ratatui::{Frame, Terminal};

use crate::app::{App, Focus};
use crate::mouse::Hits;
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
    let [header, body, bar, status_line] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(u16::from(narrow)),
        Constraint::Length(1),
    ])
    .areas(area);
    if app.machines.is_empty() {
        pairing::draw(frame, body);
    } else if narrow && app.focus == Focus::Sessions {
        sessions::draw(frame, body, app, true, &mut hits);
    } else if narrow {
        main(frame, body, app, true, &mut hits);
    } else {
        let list_width = (body.width / 3).clamp(30, 44);
        let [list, rest] =
            Layout::horizontal([Constraint::Length(list_width), Constraint::Fill(1)]).areas(body);
        sessions::draw(frame, list, app, false, &mut hits);
        main(frame, rest, app, false, &mut hits);
    }
    if !palette::draw(frame, status_line, app) {
        status::draw(frame, status_line, app, narrow);
    }
    // Under the dialogs, but their taps over everything.
    let mut touch = Hits::default();
    touch::header(frame, header, app, narrow, &mut touch);
    let clicks = if narrow {
        touch::clicks(app)
    } else {
        Vec::new()
    };
    // Tab's focus stays on a button only while the bar stays the same.
    if clicks != app.bar {
        app.bar = clicks;
        app.bar_focus = None;
    }
    if narrow {
        touch::bar(frame, bar, app, &mut touch);
    }
    // A dialog takes taps for itself and what it hides; the header and the bar stay.
    if app.dialog_open() {
        hits.cover(area);
    }
    new_session::draw(frame, area, app);
    if let Some(screen) = &app.account_screen {
        accounts::draw(frame, body, app, screen, &mut hits);
    }
    switch::draw(frame, body, app, &mut hits);
    recover::draw(frame, body, app, &mut hits);
    if let Some(panel) = &app.machine_panel {
        machines::draw(frame, body, app, panel, &mut hits);
    }
    terminals::draw(frame, body, app, &mut hits);
    if app.help {
        help::draw(frame, area, app, &mut hits);
    }
    prs::prompt(frame, area, app);
    hits.append(touch);
    app.hits = hits;
    if Glyphs::for_width(app.glyphs, area.width) == Glyphs::Ascii {
        glyphs::fold(frame.buffer_mut());
    }
}

/// The main pane: every session's PRs, the inbox, or the open session; `compact` on a narrow
/// screen.
fn main(frame: &mut Frame, area: Rect, app: &mut App, compact: bool, hits: &mut Hits) {
    if app.focus == Focus::AllPrs {
        prs::all(frame, area, app, compact, hits);
    } else if app.focus == Focus::Inbox {
        inbox::draw(frame, area, app, compact, hits);
    } else {
        let (area, controls) = composer::split(area, app);
        let [strip, usage, area] = Layout::vertical([
            Constraint::Length(prs::strip_height(app)),
            Constraint::Length(resources::strip_height(app, compact)),
            Constraint::Fill(1),
        ])
        .areas(area);
        prs::strip(frame, strip, app, compact, hits);
        resources::strip(frame, usage, app, compact);
        transcript::draw(frame, area, app, compact, hits);
        if let Some(controls) = controls {
            composer::draw(frame, controls, app, compact, hits);
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
