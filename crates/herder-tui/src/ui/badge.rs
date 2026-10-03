//! Badges: a word or two set off from the text around it, one space of padding a side.
//!
//! - [`solid`]: background-coloured text on a colour, bold. The mode badge (` PROMPT `), and
//!   anything that must be seen at a glance.
//! - [`subtle`]: coloured text on the element background. ` queued `, a PR's state, counts.
//! - [`chip`]: an attachment, ` image 1 · 340 KB `, on the element background; in brackets
//!   where the theme has none. [`chip_rows`] lays chips out in rows.
//!
//! Casing: mode badges are uppercase (` PROMPT `), every status badge lowercase (` queued `).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::Ui;

/// ` text ` in `color`'s solid block.
pub fn solid(ui: Ui, text: &str, color: Color) -> Span<'static> {
    Span::styled(
        format!(" {text} "),
        Style::new()
            .fg(ui.theme.selected_list_item_text)
            .bg(color)
            .add_modifier(Modifier::BOLD),
    )
}

/// ` text ` in `color`, on the element background.
pub fn subtle(ui: Ui, text: &str, color: Color) -> Span<'static> {
    Span::styled(
        format!(" {text} "),
        Style::new().fg(color).bg(ui.theme.background_element),
    )
}

/// An attachment's chip: `label` on the element background, or `[label]` on a theme
/// without one, as `ansi`.
pub fn chip(ui: Ui, label: &str, style: Style) -> Span<'static> {
    let background = ui.theme.background_element;
    if background == Color::Reset {
        Span::styled(format!("[{label}]"), style)
    } else {
        Span::styled(format!(" {label} "), style.bg(background))
    }
}

/// `chips` in rows at most `width` wide, a space between them; a chip wider than a row has
/// one of its own.
pub fn chip_rows(chips: Vec<Span<'static>>, width: usize) -> Vec<Line<'static>> {
    let mut rows: Vec<Line<'static>> = Vec::new();
    for chip in chips {
        match rows.last_mut() {
            Some(row) if row.width() + 1 + chip.width() <= width => {
                row.push_span(Span::raw(" "));
                row.push_span(chip);
            }
            _ => rows.push(Line::from(chip)),
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::super::snapshot;
    use super::*;

    #[test]
    fn badges() {
        snapshot::each("badges", |variant| {
            let ui = variant.ui();
            let theme = ui.theme;
            let line = |spans: Vec<Span<'static>>| Line::from(spans);
            let lines = vec![
                line(vec![
                    Span::raw(" "),
                    solid(ui, "PROMPT", theme.primary),
                    Span::raw(" "),
                    solid(ui, "NAVIGATE", theme.secondary),
                    Span::raw(" "),
                    solid(ui, "APPROVAL", theme.attention),
                ]),
                line(vec![
                    Span::raw(" "),
                    subtle(ui, "queued", theme.warning),
                    Span::raw(" "),
                    subtle(ui, "open", theme.pr_open),
                    Span::raw(" "),
                    subtle(ui, "merged", theme.pr_merged),
                    Span::raw(" "),
                    subtle(ui, "draft", theme.pr_draft),
                ]),
            ];
            let chips = vec![
                chip(ui, "image 1 · 340 KB", ui.text()),
                chip(ui, "image 2 · 1.2 MB", ui.text()),
                chip(ui, "image 3 · loading…", ui.muted()),
            ];
            let mut lines = lines;
            for row in chip_rows(chips, 39) {
                let mut spans = vec![Span::raw(" ")];
                spans.extend(row.spans);
                lines.push(line(spans));
            }
            snapshot::lines(variant, 40, lines)
        });
    }
}
