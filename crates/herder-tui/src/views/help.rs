//! The key help, over everything else.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Padding, Row, Table};

use crate::action::HELP;

pub(super) fn draw(frame: &mut Frame, area: Rect) {
    let key_width = HELP
        .iter()
        .map(|(key, _)| key.chars().count())
        .max()
        .unwrap_or(0);
    let text_width = HELP
        .iter()
        .map(|(_, text)| text.chars().count())
        .max()
        .unwrap_or(0);
    let width = u16::try_from(key_width + text_width + 7).unwrap_or(u16::MAX);
    let height = u16::try_from(HELP.len() + 4).unwrap_or(u16::MAX);
    let popup = super::centered(area, width, height);
    let rows = HELP
        .iter()
        .map(|(key, text)| Row::new([Line::styled(*key, super::bold()), Line::raw(*text)]));
    let key_width = u16::try_from(key_width).unwrap_or(u16::MAX);
    let table = Table::new(rows, [Constraint::Length(key_width), Constraint::Fill(1)])
        .column_spacing(2)
        .block(
            Block::bordered()
                .title(" keys ")
                .title_bottom(Line::styled(" any key closes ", super::dim()).centered())
                .padding(Padding::uniform(1)),
        );
    frame.render_widget(Clear, popup);
    frame.render_widget(table, popup);
}
