//! Input editors, over [`ratatui_textarea::TextArea`]: the editing stays the text area's, the
//! look is the theme's.
//!
//! - [`Prompt`]: OpenCode's prompt. A bar down the left in the accent while it has focus, the
//!   text on the panel background, a meta line under it (`account · model · mode`) and a
//!   row of padding; the panel is solid to its edges.
//! - [`Field`]: a one-line field with a label, for dialog searches and forms.
//!
//! ```text
//! ┃ write the docs page too▌
//! ┃
//! ┃ claude-main · opus · ask
//! ┃
//! ```

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use ratatui_textarea::{TextArea, WrapMode};

use super::{Ui, fill, fit};

/// Styles `editor` for `ui`: its text, placeholder and, with focus, its cursor.
fn style(ui: Ui, editor: &mut TextArea, focused: bool, background: Color) {
    editor.remove_block();
    editor.set_style(ui.text().bg(background));
    editor.set_cursor_line_style(Style::new());
    editor.set_placeholder_style(ui.muted().bg(background));
    editor.set_cursor_style(if focused {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new()
    });
}

/// The prompt: the editor in its panel.
pub struct Prompt<'a, 'b> {
    ui: Ui<'a>,
    editor: &'a mut TextArea<'b>,
    meta: Option<Line<'a>>,
    focused: bool,
    accent: Option<Color>,
}

impl<'a, 'b> Prompt<'a, 'b> {
    pub fn new(ui: Ui<'a>, editor: &'a mut TextArea<'b>) -> Self {
        Self {
            ui,
            editor,
            meta: None,
            focused: false,
            accent: None,
        }
    }

    /// The line under the text: `account · model · mode`.
    pub fn meta(mut self, meta: impl Into<Line<'a>>) -> Self {
        self.meta = Some(meta.into());
        self
    }

    /// Whether the prompt has the keys: the bar takes the accent and the cursor shows.
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// The bar's colour in place of the accent: `attention` for an answer, say.
    pub fn accent(mut self, color: Color) -> Self {
        self.accent = Some(color);
        self
    }

    /// Rows a prompt takes `width` columns wide with `editor`'s text, which shows at most
    /// `max_lines` lines before it scrolls.
    pub fn height(editor: &TextArea, width: u16, max_lines: u16, meta: bool) -> u16 {
        let room = usize::from(width.saturating_sub(3)).max(1);
        let lines: usize = editor
            .lines()
            .iter()
            .map(|line| super::width(line).div_ceil(room).max(1))
            .sum();
        let lines = u16::try_from(lines).unwrap_or(u16::MAX);
        lines.clamp(1, max_lines.max(1)) + if meta { 3 } else { 1 }
    }

    pub fn render(self, area: Rect, buf: &mut Buffer) {
        let ui = self.ui;
        let theme = ui.theme;
        let area = area.intersection(buf.area);
        if area.height < 2 || area.width < 4 {
            return;
        }
        let bar_color = match (self.focused, self.accent) {
            (true, Some(color)) => color,
            (true, None) => theme.primary,
            (false, _) => theme.border,
        };
        let panel = Rect {
            height: area.height - 1,
            ..area
        };
        fill(buf, panel, ui.panel());
        for y in panel.top()..panel.bottom() {
            buf[(panel.x, y)]
                .set_symbol(ui.glyphs.bar)
                .set_fg(bar_color);
        }
        let meta_rows = if self.meta.is_some() { 2 } else { 0 };
        let text = Rect {
            x: panel.x + 2,
            width: panel.width - 3,
            height: panel.height.saturating_sub(meta_rows).max(1),
            ..panel
        };
        style(ui, self.editor, self.focused, theme.background_panel);
        // The height counts wrapped lines: the text wraps, never scrolls sideways.
        self.editor.set_wrap_mode(WrapMode::WordOrGlyph);
        if self.editor.is_empty() {
            // The placeholder starts where the text will, under the cursor.
            Line::styled(self.editor.placeholder_text().to_owned(), ui.muted()).render(text, buf);
            if self.focused {
                buf[(text.x, text.y)].modifier.insert(Modifier::REVERSED);
            }
        } else {
            (&*self.editor).render(text, buf);
        }
        if let Some(meta) = self.meta
            && panel.height > meta_rows
        {
            let row = Rect::new(text.x, panel.bottom() - 1, text.width, 1);
            fit(meta, usize::from(row.width), ui.glyphs).render(row, buf);
        }
        // The bottom row: solid panel to the edge, the bar running on. On the terminal's own
        // background (`ansi`) there is no panel to show it, so the bar ends in a cap.
        let cap = Rect::new(area.x, area.bottom() - 1, area.width, 1);
        if theme.background_panel == Color::Reset {
            buf[(cap.x, cap.y)]
                .set_symbol(ui.glyphs.cap_end)
                .set_style(Style::new().fg(bar_color));
            if ui.glyphs.cap_fill != "▀" {
                for x in cap.x + 1..cap.right() {
                    buf[(x, cap.y)]
                        .set_symbol(ui.glyphs.cap_fill)
                        .set_style(Style::new().fg(theme.border));
                }
            }
        } else {
            fill(buf, cap, ui.panel());
            buf[(cap.x, cap.y)]
                .set_symbol(ui.glyphs.bar)
                .set_fg(bar_color);
        }
    }
}

/// A one-line field: `label` then the editor, on the element background while focused.
pub struct Field<'a, 'b> {
    ui: Ui<'a>,
    label: &'a str,
    editor: &'a mut TextArea<'b>,
    focused: bool,
}

impl<'a, 'b> Field<'a, 'b> {
    pub fn new(ui: Ui<'a>, label: &'a str, editor: &'a mut TextArea<'b>) -> Self {
        Self {
            ui,
            label,
            editor,
            focused: false,
        }
    }

    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Draws on the first row of `area`; the label takes `label_width` columns, so the fields
    /// of a form line up.
    pub fn render(self, area: Rect, buf: &mut Buffer, label_width: u16) {
        let ui = self.ui;
        let area = Rect { height: 1, ..area }.intersection(buf.area);
        let label_style = if self.focused {
            ui.accent().add_modifier(Modifier::BOLD)
        } else {
            ui.muted()
        };
        let label_width = label_width.min(area.width);
        fit(
            Line::from(Span::styled(self.label, label_style)),
            usize::from(label_width),
            ui.glyphs,
        )
        .render(
            Rect {
                width: label_width,
                ..area
            },
            buf,
        );
        let value = Rect {
            x: area.x + label_width,
            width: area.width - label_width,
            ..area
        };
        let background = if self.focused {
            ui.theme.background_element
        } else {
            buf.cell((value.x, value.y))
                .map(|cell| cell.bg)
                .unwrap_or(Color::Reset)
        };
        fill(buf, value, Style::new().bg(background));
        let inner = Rect {
            x: value.x + 1,
            width: value.width.saturating_sub(2),
            ..value
        };
        style(ui, self.editor, self.focused, background);
        if self.editor.is_empty() {
            // The placeholder starts where the text will, under the cursor.
            Line::styled(self.editor.placeholder_text().to_owned(), ui.muted()).render(inner, buf);
            if self.focused && inner.width > 0 {
                buf[(inner.x, inner.y)].modifier.insert(Modifier::REVERSED);
            }
        } else {
            (&*self.editor).render(inner, buf);
        }
    }
}

/// A form's choice: `label`, the value where a [`Field`]'s text would be, then `‹ ›` to
/// change it, tappable; on the element background while focused.
pub struct Choice<'a> {
    ui: Ui<'a>,
    label: &'a str,
    value: Line<'a>,
    focused: bool,
}

impl<'a> Choice<'a> {
    pub fn new(ui: Ui<'a>, label: &'a str, value: impl Into<Line<'a>>) -> Self {
        Self {
            ui,
            label,
            value: value.into(),
            focused: false,
        }
    }

    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Draws on the first row of `area`, the label `label_width` columns wide as a
    /// [`Field`]'s; returns where the previous arrow is, and the value, which a tap moves
    /// forward.
    pub fn render(self, area: Rect, buf: &mut Buffer, label_width: u16) -> [Rect; 2] {
        let ui = self.ui;
        let area = Rect { height: 1, ..area }.intersection(buf.area);
        let label_width = label_width.min(area.width);
        let label_style = if self.focused {
            ui.accent().add_modifier(Modifier::BOLD)
        } else {
            ui.muted()
        };
        fit(
            Line::from(Span::styled(self.label, label_style)),
            usize::from(label_width),
            ui.glyphs,
        )
        .render(
            Rect {
                width: label_width,
                ..area
            },
            buf,
        );
        let value = Rect {
            x: area.x + label_width,
            width: area.width - label_width,
            ..area
        };
        if self.focused {
            fill(buf, value, Style::new().bg(ui.theme.background_element));
        }
        let [prev, next] = ui.glyphs.choice;
        let arrows = u16::try_from(super::width(prev) + 1 + super::width(next)).unwrap_or(3);
        // The value as a field's text sits, then the arrows after a gap.
        let room = usize::from(value.width.saturating_sub(arrows + 4));
        let shown = fit(self.value, room, ui.glyphs);
        let shown_width = u16::try_from(super::line_width(&shown)).unwrap_or(u16::MAX);
        shown.render(
            Rect::new(value.x + 1, value.y, shown_width, 1).intersection(value),
            buf,
        );
        let arrow = if self.focused {
            ui.accent()
        } else {
            ui.muted()
        };
        let x = value.x + 1 + shown_width + 2;
        let prev_width = u16::try_from(super::width(prev)).unwrap_or(1);
        let prev_at = Rect::new(x, value.y, prev_width, 1).intersection(value);
        Span::styled(prev, arrow).render(prev_at, buf);
        let next_at =
            Rect::new(x + prev_width + 1, value.y, arrows - prev_width - 1, 1).intersection(value);
        Span::styled(next, arrow).render(next_at, buf);
        // A tap on the left arrow goes back; anywhere else on the value, forward. The arrow
        // is inside the value: record it after, so it is on top.
        [prev_at, value]
    }
}

#[cfg(test)]
mod tests {
    use super::super::snapshot;
    use super::*;

    fn editor(text: &str) -> TextArea<'static> {
        let mut editor = TextArea::from(text.lines());
        editor.set_placeholder_text("Write a prompt…");
        editor
    }

    fn meta(ui: Ui) -> Line<'static> {
        Line::from(ui.joined([
            Span::styled("claude-main", ui.text()),
            Span::styled("opus", ui.muted()),
            Span::styled("ask", ui.muted()),
        ]))
    }

    #[test]
    fn prompts() {
        for width in [45, 100] {
            snapshot::each(&format!("prompt-{width}"), |variant| {
                snapshot::render(variant, width, 11, |ui, area, buf| {
                    let mut focused =
                        editor("write the docs page too, and link it from @README.md");
                    let height = Prompt::height(&focused, width, 6, true);
                    assert_eq!(height, if width < 50 { 5 } else { 4 });
                    Prompt::new(ui, &mut focused)
                        .meta(meta(ui))
                        .focused(true)
                        .render(Rect { height, ..area }, buf);
                    // Unfocused and empty: the placeholder, the bar in the border colour.
                    let mut empty = editor("");
                    let below = Rect {
                        y: area.y + height + 1,
                        height: Prompt::height(&empty, width, 6, true),
                        ..area
                    };
                    Prompt::new(ui, &mut empty)
                        .meta(meta(ui))
                        .render(below, buf);
                    let mut answer = editor("h2 under Reference");
                    let last = Rect {
                        y: below.bottom() + 1,
                        height: 2,
                        ..area
                    };
                    Prompt::new(ui, &mut answer)
                        .accent(ui.theme.attention)
                        .focused(true)
                        .render(last, buf);
                })
            });
        }
    }

    #[test]
    fn choices() {
        snapshot::each("choice", |variant| {
            snapshot::render(variant, 45, 3, |ui, area, buf| {
                let [prev, value] = Choice::new(ui, "account", "claude-main")
                    .focused(true)
                    .render(area, buf, 9);
                assert!(value.contains(prev.as_position()));
                Choice::new(ui, "mode", "ask").render(
                    Rect {
                        y: area.y + 2,
                        ..area
                    },
                    buf,
                    9,
                );
            })
        });
    }

    #[test]
    fn fields() {
        snapshot::each("field", |variant| {
            snapshot::render(variant, 45, 3, |ui, area, buf| {
                let mut search = editor("docs");
                Field::new(ui, "search", &mut search)
                    .focused(true)
                    .render(area, buf, 9);
                let mut model = editor("opus");
                Field::new(ui, "model", &mut model).render(
                    Rect {
                        y: area.y + 2,
                        ..area
                    },
                    buf,
                    9,
                );
            })
        });
    }
}
