//! The look every view shares: the [`theme`], the [`glyphs`], and the widgets built on them.
//!
//! Views style nothing themselves. They take a [`Ui`] (the theme and the glyph set the
//! screen draws with) and draw with its styles and widgets, so a theme or a glyph set changes
//! every screen at once:
//!
//! | widget | module | draws |
//! |---|---|---|
//! | status dot | [`state`] | a [`state::State`] as glyph + colour, and [`state::rollup`] |
//! | badge | [`badge`] | ` PROMPT ` solid, ` QUEUED ` subtle |
//! | key-hint bar | [`hints`] | [`hints::ModeBar`] on a desktop, [`hints::ButtonBar`] on a phone, from one hint list |
//! | dialog | [`dialog`] | a centred box: title, `esc`, body, footer hints, dimmed backdrop |
//! | list | [`list`] | headers, items with a status dot, right-aligned meta, cursor, scrolling |
//! | input editor | [`input`] | the prompt (`┃` bar, meta line, cap) and one-line fields |
//! | usage bar | [`usage`] | `█████░░░ 38%` coloured by how full |
//!
//! Spacing is fixed here too: rows start one column in ([`INSET`]), groups sit [`GAP`]
//! columns apart, and dialogs pad [`dialog::PAD_X`] columns inside their border.
//!
//! [`gallery`] draws every widget at once: the screenshots and `cargo run --example
//! gallery` show it.

pub mod badge;
pub mod dialog;
pub mod gallery;
pub mod glyphs;
pub mod hints;
pub mod input;
pub mod list;
pub mod state;
pub mod theme;
pub mod usage;

#[cfg(test)]
mod snapshot;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use glyphs::{GlyphSet, Glyphs};
use theme::Theme;

/// Columns between a row's edge and its text.
pub const INSET: u16 = 1;

/// Columns between groups on a row: hints, a title and its meta.
pub const GAP: usize = 2;

/// What a screen draws with: its theme and glyph set.
#[derive(Clone, Copy, Debug)]
pub struct Ui<'a> {
    pub theme: &'a Theme,
    pub glyphs: &'static GlyphSet,
}

impl<'a> Ui<'a> {
    pub fn new(theme: &'a Theme, glyphs: Glyphs) -> Self {
        Self {
            theme,
            glyphs: glyphs.set(),
        }
    }

    /// The screen's own: text on the background.
    pub fn base(self) -> Style {
        Style::new().fg(self.theme.text).bg(self.theme.background)
    }

    /// A raised surface: dialogs, the prompt, user messages.
    pub fn panel(self) -> Style {
        Style::new()
            .fg(self.theme.text)
            .bg(self.theme.background_panel)
    }

    /// Body text.
    pub fn text(self) -> Style {
        Style::new().fg(self.theme.text)
    }

    /// Titles, keys and the selected row.
    pub fn strong(self) -> Style {
        self.text().add_modifier(Modifier::BOLD)
    }

    /// Secondary text: meta, labels, hints.
    pub fn muted(self) -> Style {
        Style::new().fg(self.theme.text_muted)
    }

    /// The one accent: focus, the cursor, what can be tapped.
    pub fn accent(self) -> Style {
        Style::new().fg(self.theme.primary)
    }

    /// A border, highlighted while what it frames has focus.
    pub fn border(self, focused: bool) -> Style {
        Style::new().fg(if focused {
            self.theme.border_active
        } else {
            self.theme.border
        })
    }

    /// `parts` joined by the glyph set's separator, in muted text.
    pub fn joined<'b>(self, parts: impl IntoIterator<Item = Span<'b>>) -> Vec<Span<'b>> {
        let mut spans = Vec::new();
        for part in parts {
            if !spans.is_empty() {
                spans.push(Span::styled(self.glyphs.separator, self.muted()));
            }
            spans.push(part);
        }
        spans
    }
}

/// Columns `text` takes on screen.
pub fn width(text: &str) -> usize {
    textwrap::core::display_width(text)
}

/// Columns `line` takes on screen.
pub fn line_width(line: &Line) -> usize {
    line.spans.iter().map(|span| width(&span.content)).sum()
}

/// `line` cut to `max` columns, ending in the glyph set's ellipsis when cut.
pub fn fit<'a>(line: Line<'a>, max: usize, glyphs: &GlyphSet) -> Line<'a> {
    if line_width(&line) <= max {
        return line;
    }
    let ellipsis = width(glyphs.ellipsis);
    let mut room = max.saturating_sub(ellipsis);
    let mut spans = Vec::new();
    let mut last_style = line.style;
    for span in line.spans {
        if room == 0 {
            break;
        }
        let mut kept = String::new();
        for c in span.content.chars() {
            let mut buf = [0; 4];
            let w = width(c.encode_utf8(&mut buf));
            if w > room {
                room = 0;
                break;
            }
            room -= w;
            kept.push(c);
        }
        last_style = span.style;
        spans.push(Span::styled(kept, span.style));
    }
    if max >= ellipsis {
        spans.push(Span::styled(glyphs.ellipsis, last_style));
    }
    Line::from(spans).style(line.style)
}

/// Sets `style` on every cell of `area`, clipped to `buf`.
pub fn fill(buf: &mut Buffer, area: Rect, style: Style) {
    buf.set_style(area.intersection(buf.area), style);
}
