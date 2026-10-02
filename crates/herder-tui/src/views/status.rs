//! The bottom line: each machine's connection, and the essential keys.

use herder_client_core::ConnectionState;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;

const KEYS: &str = " ? help  q quit ";

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans = Vec::new();
    if let Some(notice) = &app.notice {
        spans.push(Span::styled(
            format!(" {notice} "),
            Style::new().fg(Color::Yellow),
        ));
    }
    let waiting = app.waiting().len();
    if waiting > 0 && app.notice.is_none() {
        spans.push(Span::styled(
            format!(" {waiting} waiting on you · I inbox "),
            Style::new().fg(Color::Magenta),
        ));
    }
    for machine in app.machines.iter().filter(|_| app.notice.is_none()) {
        let (mark, color, text) = match &machine.connection {
            ConnectionState::Connected => ("●", Color::Green, "connected".to_owned()),
            ConnectionState::Connecting => ("◌", Color::Yellow, "connecting".to_owned()),
            ConnectionState::Disconnected { error } => ("✗", Color::Red, error.clone()),
        };
        spans.push(Span::raw(" "));
        spans.push(Span::styled(mark, Style::new().fg(color)));
        spans.push(Span::raw(format!(" {} ", machine.name)));
        spans.push(Span::styled(text, super::dim()));
        spans.push(Span::raw(" "));
    }
    let keys_width = u16::try_from(KEYS.len()).unwrap_or(u16::MAX);
    let [machines, keys] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(keys_width)]).areas(area);
    frame.render_widget(Paragraph::new(Line::from(spans)), machines);
    frame.render_widget(Line::styled(KEYS, super::dim()).right_aligned(), keys);
}
