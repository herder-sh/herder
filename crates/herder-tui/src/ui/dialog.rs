//! Dialogs: one frame for all of them.
//!
//! A centred card on the panel background: the title bold at the top left, `esc` at the top
//! right, the body padded [`PAD_X`] columns and one row inside the border, and footer hints
//! at the bottom. What it covers is dimmed. Widths are fixed ([`Size`]); a screen narrower
//! than the dialog gets it full width, as on a phone.
//!
//! ```text
//! ┌─ go to ─────────────────────── esc ─┐
//! │                                     │
//! │  body                               │
//! │                                     │
//! │  enter open  esc close              │
//! └─────────────────────────────────────┘
//! ```

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Widget};

use super::Ui;
use super::hints::{Hint, ModeBar};

/// Columns between the border and the body.
pub const PAD_X: u16 = 2;

/// A dialog's width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    /// Confirmations.
    Small,
    /// Forms and pickers.
    Medium,
    /// The command palette and help.
    Large,
}

impl Size {
    /// Columns, border included.
    pub fn width(self) -> u16 {
        match self {
            Self::Small => 44,
            Self::Medium => 60,
            Self::Large => 88,
        }
    }
}

/// Where a drawn dialog's parts are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Areas {
    /// The whole box: a tap outside it closes the dialog.
    pub outer: Rect,
    /// Where the body goes.
    pub body: Rect,
    /// The `esc` mark: a tap there closes the dialog.
    pub close: Rect,
}

/// A dialog frame; the caller draws the body into [`Areas::body`].
pub struct Dialog<'a> {
    ui: Ui<'a>,
    title: Line<'a>,
    size: Size,
    hints: &'a [Hint],
}

impl<'a> Dialog<'a> {
    pub fn new(ui: Ui<'a>, title: impl Into<Line<'a>>, size: Size) -> Self {
        Self {
            ui,
            title: title.into(),
            size,
            hints: &[],
        }
    }

    /// Hints under the body.
    pub fn hints(mut self, hints: &'a [Hint]) -> Self {
        self.hints = hints;
        self
    }

    /// Rows the frame adds to a body: border, padding and hints.
    pub fn chrome(&self) -> u16 {
        4 + if self.hints.is_empty() { 0 } else { 2 }
    }

    /// Draws the frame for a body `body_height` rows tall, centred in `screen` and clipped to
    /// it, and dims the rest of `screen`.
    pub fn render(self, screen: Rect, body_height: u16, buf: &mut Buffer) -> Areas {
        let ui = self.ui;
        let screen = screen.intersection(buf.area);
        let width = self.size.width().min(screen.width);
        let height = body_height.saturating_add(self.chrome()).min(screen.height);
        let outer = Rect::new(
            screen.x + (screen.width - width) / 2,
            screen.y + (screen.height - height) / 2,
            width,
            height,
        );
        dim(ui, screen, outer, buf);
        Clear.render(outer, buf);
        let title = Line::from(
            [Span::raw(" ")]
                .into_iter()
                .chain(
                    self.title
                        .spans
                        .into_iter()
                        .map(|span| span.patch_style(ui.strong())),
                )
                .chain([Span::raw(" ")])
                .collect::<Vec<_>>(),
        );
        // On a panel background the dialog is a card, its edge the panel's own; on the
        // terminal's background (`ansi`) a line draws the edge.
        let edge = if ui.theme.background_panel == Color::Reset {
            ui.border(false)
        } else {
            Style::new().fg(ui.theme.background_panel)
        };
        let block = Block::bordered()
            .border_style(edge)
            .style(ui.panel())
            .title(title)
            .title(Line::styled(" esc ", ui.muted()).right_aligned());
        let inner = block.inner(outer);
        block.render(outer, buf);
        let padded = Rect {
            x: inner.x + PAD_X.min(inner.width / 2),
            y: inner.y + 1.min(inner.height),
            width: inner.width.saturating_sub(2 * PAD_X),
            height: inner.height.saturating_sub(2),
        };
        let footer = if self.hints.is_empty() { 0 } else { 2 };
        let body = Rect {
            height: padded.height.saturating_sub(footer),
            ..padded
        };
        if footer > 0 && padded.height > footer {
            let at = Rect {
                y: padded.bottom() - 1,
                height: 1,
                // The bar insets itself by a column; the body's edge lines it up.
                x: padded.x.saturating_sub(super::INSET),
                width: padded.width + super::INSET,
            };
            ModeBar::new(ui, self.hints).render(at, buf);
        }
        let close = Rect::new(outer.right().saturating_sub(6), outer.y, 5.min(width), 1);
        Areas { outer, body, close }
    }
}

/// Mutes everything in `screen` outside `keep`: what a dialog covers recedes.
fn dim(ui: Ui, screen: Rect, keep: Rect, buf: &mut Buffer) {
    for y in screen.top()..screen.bottom() {
        for x in screen.left()..screen.right() {
            if keep.contains((x, y).into()) {
                continue;
            }
            let cell = &mut buf[(x, y)];
            // Muted text on the plain background: coloured blocks would keep their pull, and
            // block glyphs (bars, the prompt's cap) fade to the faintest line.
            let block = cell
                .symbol()
                .chars()
                .next()
                .is_some_and(|c| ('\u{2580}'..='\u{259f}').contains(&c));
            cell.fg = if block {
                ui.theme.border_subtle
            } else {
                ui.theme.text_muted
            };
            cell.bg = ui.theme.background;
            cell.modifier.remove(Modifier::BOLD | Modifier::REVERSED);
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::text::Text;

    use super::super::snapshot;
    use super::*;

    #[test]
    fn dialogs() {
        let hints = [Hint::new("enter", "open"), Hint::new("esc", "close")];
        for width in [45, 100] {
            snapshot::each(&format!("dialog-{width}"), |variant| {
                snapshot::render(variant, width, 12, |ui, area, buf| {
                    Text::styled(
                        "behind the dialog: what it covers dims\n".repeat(12),
                        ui.strong(),
                    )
                    .render(area, buf);
                    let areas = Dialog::new(ui, "go to", Size::Medium)
                        .hints(&hints)
                        .render(area, 2, buf);
                    assert_eq!(areas.body.height, 2);
                    Text::styled("the body\ngoes here", ui.text()).render(areas.body, buf);
                    assert_eq!(buf[(areas.close.x + 1, areas.close.y)].symbol(), "e");
                })
            });
        }
    }
}
