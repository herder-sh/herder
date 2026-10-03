//! Badges: a word or two set off from the text around it, one space of padding a side.
//!
//! - [`solid`]: background-coloured text on a colour, bold. The mode badge (` PROMPT `), and
//!   anything that must be seen at a glance.
//! - [`subtle`]: coloured text on the element background. ` queued `, a PR's state, counts.
//!
//! Casing: mode badges are uppercase (` PROMPT `), every status badge lowercase (` queued `).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

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

#[cfg(test)]
mod tests {
    use ratatui::text::Line;

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
            snapshot::lines(variant, 40, lines)
        });
    }
}
