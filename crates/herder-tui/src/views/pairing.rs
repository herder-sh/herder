//! The screen shown while this terminal is signed in to no machine: how to sign in to one.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Text};
use ratatui::widgets::Paragraph;

use crate::ui::Ui;

pub(super) fn draw(frame: &mut Frame, area: Rect, ui: Ui) {
    let command = ui.accent();
    let text = Text::from(vec![
        Line::styled("No machines yet.", ui.strong()),
        Line::raw(""),
        Line::raw("Sign this terminal in to a machine with"),
        Line::raw(""),
        Line::styled("herder login", command),
        Line::raw(""),
        Line::raw("or press a here and paste the link a herder app of yours shares,"),
        Line::raw("then approve this terminal in that app."),
    ])
    .centered();
    let height = u16::try_from(text.height()).unwrap_or(u16::MAX);
    let width = u16::try_from(text.width()).unwrap_or(u16::MAX);
    frame.render_widget(Paragraph::new(text), super::centered(area, width, height));
}
