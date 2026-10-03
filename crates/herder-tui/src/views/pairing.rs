//! The screen shown while no machine is paired: how to pair one, through the add-machine dialog.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Text};
use ratatui::widgets::Paragraph;

use crate::ui::Ui;

pub(super) fn draw(frame: &mut Frame, area: Rect, ui: Ui) {
    let command = ui.accent();
    let text = Text::from(vec![
        Line::styled("No machines paired yet.", ui.strong()),
        Line::raw(""),
        Line::raw("On a machine running the herder daemon, run"),
        Line::raw(""),
        Line::styled("herder pair", command),
        Line::raw(""),
        Line::raw("It prints a one-time pairing link. Press a here and paste it,"),
        Line::raw("or paste it anywhere in herder."),
    ])
    .centered();
    let height = u16::try_from(text.height()).unwrap_or(u16::MAX);
    let width = u16::try_from(text.width()).unwrap_or(u16::MAX);
    frame.render_widget(Paragraph::new(text), super::centered(area, width, height));
}
