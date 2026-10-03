//! The bottom line, a [`ModeBar`]: a notice or what waits on the user, the essential keys, and
//! each machine's connection at the right end; on a narrow screen, only each machine's mark
//! and name.

use herder_client_core::ConnectionState;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::App;
use crate::ui::hints::{Hint, ModeBar};

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App, narrow: bool) {
    let ui = app.ui();
    let theme = ui.theme;
    let mut lead = Vec::new();
    if let Some(notice) = &app.notice {
        lead.push(Span::styled(notice.clone(), Style::new().fg(theme.warning)));
    }
    let waiting = app.waiting().len();
    if waiting > 0 && app.notice.is_none() {
        let text = if narrow {
            format!("{waiting} waiting · I")
        } else {
            format!("{waiting} waiting on you · I inbox")
        };
        lead.push(Span::styled(text, Style::new().fg(theme.attention)));
    }
    let mut right = Vec::new();
    for machine in app.machines.iter().filter(|_| app.notice.is_none()) {
        let (mark, color, text) = match &machine.connection {
            ConnectionState::Connected => (ui.glyphs.connected, theme.success, "connected"),
            ConnectionState::Connecting => (ui.glyphs.connecting, theme.warning, "connecting"),
            ConnectionState::Disconnected { error } => {
                (ui.glyphs.disconnected, theme.error, error.as_str())
            }
        };
        if !right.is_empty() {
            right.push(Span::raw("  "));
        }
        right.push(Span::styled(mark, Style::new().fg(color)));
        right.push(Span::styled(format!(" {}", machine.name), ui.text()));
        if !narrow {
            right.push(Span::styled(format!(" {text}"), ui.muted()));
        }
    }
    let hints = if narrow {
        vec![Hint::new("?", "help")]
    } else {
        vec![Hint::new("?", "help"), Hint::new("q", "quit")]
    };
    ModeBar::new(ui, &hints)
        .lead(Line::from(lead))
        .right(Line::from(right))
        .render(area, frame.buffer_mut());
}
