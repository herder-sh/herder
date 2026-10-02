//! The screen shown while no machine is paired: how to pair one.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::Paragraph;

pub(super) fn draw(frame: &mut Frame, area: Rect) {
    let command = Style::new().fg(Color::Cyan);
    let text = Text::from(vec![
        Line::styled("No machines paired yet.", super::bold()),
        Line::raw(""),
        Line::raw("On a machine running the herder daemon, run"),
        Line::raw(""),
        Line::styled("herder pair", command),
        Line::raw(""),
        Line::raw("It prints a one-time pairing link and QR code for this device."),
    ])
    .centered();
    let height = u16::try_from(text.height()).unwrap_or(u16::MAX);
    let width = u16::try_from(text.width()).unwrap_or(u16::MAX);
    frame.render_widget(Paragraph::new(text), super::centered(area, width, height));
}
