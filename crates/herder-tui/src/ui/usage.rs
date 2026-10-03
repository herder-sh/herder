//! Usage bars: how full an account's window is, `█░` (`#-` in ASCII), coloured by how close
//! to its limit: [`super::theme::Theme::usage`].
//!
//! ```text
//! ███████░░░░░░░░░░░░░ 38%
//! ```

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::Ui;

/// A bar `width` cells long, `percent` full, then the percentage right-aligned in four
/// columns.
pub fn bar(ui: Ui, percent: u8, width: u16) -> Line<'static> {
    let percent = percent.min(100);
    let cells = usize::from(width);
    let full = (cells * usize::from(percent) + 50) / 100;
    // Any use at all shows.
    let full = if percent > 0 { full.max(1) } else { 0 };
    let color = ui.theme.usage(percent);
    Line::from(vec![
        Span::styled(ui.glyphs.usage_full.repeat(full), Style::new().fg(color)),
        Span::styled(
            ui.glyphs.usage_empty.repeat(cells - full),
            Style::new().fg(ui.theme.border),
        ),
        Span::styled(format!(" {percent:>3}%"), Style::new().fg(color)),
    ])
}

#[cfg(test)]
mod tests {
    use super::super::snapshot;
    use super::*;

    #[test]
    fn usage_bars() {
        snapshot::each("usage", |variant| {
            let ui = variant.ui();
            let row = |label: &str, percent| {
                let mut line = bar(ui, percent, 20);
                line.spans
                    .insert(0, Span::styled(format!(" {label:<5} "), ui.muted()));
                line
            };
            snapshot::lines(
                variant,
                34,
                vec![
                    row("5h", 38),
                    row("week", 74),
                    row("day", 91),
                    row("idle", 0),
                ],
            )
        });
        let variant = snapshot::variants().remove(0);
        let ui = variant.ui();
        assert_eq!(super::super::line_width(&bar(ui, 100, 10)), 15);
        assert_eq!(bar(ui, 1, 10).spans[0].content, "█");
    }
}
