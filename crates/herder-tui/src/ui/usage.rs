//! Usage bars: how full an account's window is, coloured by how close to its limit
//! ([`super::theme::Theme::usage`]).
//!
//! The fill is one solid run of background colour on a `backgroundElement` track, in whole
//! cells: no glyph seams, no stipple. On a theme without backgrounds (`ansi`) the bar falls
//! back to the glyph set's `█░` / `#-`.
//!
//! ```text
//! ████████░░░░░░░░░░░░ 38%   (fill and track are background colours)
//! ```

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::Ui;

/// A bar `width` cells long, `percent` full, then the percentage right-aligned in four
/// columns.
pub fn bar(ui: Ui, percent: u8, width: u16) -> Line<'static> {
    let percent = percent.min(100);
    let cells = usize::from(width);
    let color = ui.theme.usage(percent);
    let track = ui.theme.background_element;
    let label = Span::styled(format!(" {percent:>3}%"), Style::new().fg(color));
    // Whole cells, and any use at all shows. (An eighth-block's glyph does not always fill
    // its cell, which would break the bar's edge.)
    let full = (cells * usize::from(percent) + 50) / 100;
    let full = if percent > 0 { full.max(1) } else { 0 };
    let spans = if track == Color::Reset {
        vec![
            Span::styled(ui.glyphs.usage_full.repeat(full), Style::new().fg(color)),
            Span::styled(
                ui.glyphs.usage_empty.repeat(cells - full),
                Style::new().fg(ui.theme.border),
            ),
            label,
        ]
    } else {
        vec![
            Span::styled(" ".repeat(full), Style::new().bg(color)),
            Span::styled(" ".repeat(cells - full), Style::new().bg(track)),
            label,
        ]
    };
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::super::glyphs::Glyphs;
    use super::super::snapshot;
    use super::super::theme::Theme;
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
        // Any use shows: a cell of fill.
        assert_eq!(bar(ui, 1, 10).spans[0].content, " ");
        assert_eq!(bar(ui, 50, 10).spans[0].content, "     ");
        assert_eq!(
            bar(Ui::new(&Theme::ansi(), Glyphs::Unicode), 1, 10).spans[0].content,
            "█"
        );
    }
}
