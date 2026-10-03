//! Autocomplete: the popup the prompt opens for `/` commands and `@` mentions, as OpenCode's.
//! A box on the menu background, one row per match: what is inserted, then what it does in
//! muted text; the selected row solid in the accent.
//!
//! ```text
//! ┌──────────────────────────────────────┐
//! │ /model <name>        switch the model│
//! │ /mode <mode>      set the permission…│
//! └──────────────────────────────────────┘
//! ```

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Widget};

use super::{Ui, fill, spread};

/// Most rows a popup shows.
pub const MAX_ROWS: usize = 10;

pub struct Popup<'a> {
    ui: Ui<'a>,
    rows: Vec<(Line<'a>, Line<'a>)>,
    selected: usize,
}

impl<'a> Popup<'a> {
    /// A popup of `rows`: each what is inserted, and what it does.
    pub fn new(ui: Ui<'a>, rows: Vec<(Line<'a>, Line<'a>)>) -> Self {
        Self {
            ui,
            rows,
            selected: 0,
        }
    }

    pub fn select(mut self, selected: usize) -> Self {
        self.selected = selected;
        self
    }

    /// Rows a popup of `rows` matches takes, border included.
    pub fn height(rows: usize) -> u16 {
        u16::try_from(rows.min(MAX_ROWS)).unwrap_or(0) + 2
    }

    pub fn render(self, area: Rect, buf: &mut Buffer) {
        let ui = self.ui;
        let theme = ui.theme;
        let area = area.intersection(buf.area);
        if area.height < 3 || area.width < 8 {
            return;
        }
        Clear.render(area, buf);
        let menu = Style::new().fg(theme.text).bg(theme.background_menu);
        Block::bordered()
            .border_style(ui.border(false).bg(theme.background_menu))
            .style(menu)
            .render(area, buf);
        let inner = Rect {
            x: area.x + 2,
            y: area.y + 1,
            width: area.width - 4,
            height: area.height - 2,
        };
        let shown = usize::from(inner.height);
        // The selection stays in view.
        let first = (self.selected + 1).saturating_sub(shown);
        for (at, (left, right)) in self.rows.into_iter().enumerate().skip(first).take(shown) {
            let y = inner.y + u16::try_from(at - first).unwrap_or(0);
            let row = Rect::new(inner.x, y, inner.width, 1);
            spread(left, right, usize::from(row.width), ui.glyphs).render(row, buf);
            if at == self.selected {
                let solid = Style::new()
                    .fg(theme.selected_list_item_text)
                    .bg(theme.primary);
                fill(buf, Rect::new(area.x + 1, y, area.width - 2, 1), solid);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::text::Span;

    use super::super::snapshot;
    use super::*;

    #[test]
    fn popups() {
        snapshot::each("popup", |variant| {
            snapshot::render(variant, 45, 6, |ui, area, buf| {
                let row = |name: &'static str, does: &'static str| {
                    (
                        Line::from(Span::styled(name, ui.text())),
                        Line::from(Span::styled(does, ui.muted())),
                    )
                };
                let rows = vec![
                    row("/model <name>", "switch the model"),
                    row("/mode <mode>", "set the permission mode"),
                    row("/stop", "interrupt the turn"),
                    row("/switch", "account, provider or model"),
                ];
                assert_eq!(Popup::height(rows.len()), 6);
                Popup::new(ui, rows).select(1).render(area, buf);
            })
        });
    }
}
