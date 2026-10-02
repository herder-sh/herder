//! The bottom line while the command palette is open, or while Ctrl-C waits for a second press.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

use crate::app::App;
use crate::compose::COMMANDS;

/// Draws the palette or the quit hint; `false` when neither is up, so the status line shows.
pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App) -> bool {
    let Some(palette) = &mut app.compose.palette else {
        if app.compose.quit_armed {
            let hint = " press Ctrl-c again to quit ";
            frame.render_widget(Line::styled(hint, Style::new().fg(Color::Yellow)), area);
            return true;
        }
        return false;
    };
    let (note, style) = match &palette.error {
        Some(error) => (format!(" {error} "), Style::new().fg(Color::Red)),
        None => (format!(" {COMMANDS} "), super::dim()),
    };
    let note_width = u16::try_from(note.chars().count()).unwrap_or(u16::MAX);
    let [prompt, input, note_area] = Layout::horizontal([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(note_width.min(area.width / 2)),
    ])
    .areas(area);
    frame.render_widget(
        Line::styled(":", Style::new().add_modifier(Modifier::BOLD)),
        prompt,
    );
    frame.render_widget(&palette.input, input);
    frame.render_widget(Line::styled(note, style).right_aligned(), note_area);
    true
}
