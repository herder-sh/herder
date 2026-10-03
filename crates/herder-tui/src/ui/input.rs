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
use ratatui_textarea::TextArea;

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
    editor: &'a TextArea<'b>,
    meta: Option<Line<'a>>,
    focused: bool,
    accent: Option<Color>,
    placeholder: Option<&'a str>,
}

impl<'a, 'b> Prompt<'a, 'b> {
    pub fn new(ui: Ui<'a>, editor: &'a TextArea<'b>) -> Self {
        Self {
            ui,
            editor,
            meta: None,
            focused: false,
            accent: None,
            placeholder: None,
        }
    }

    /// What the empty prompt shows in place of the editor's placeholder.
    pub fn placeholder(mut self, placeholder: &'a str) -> Self {
        self.placeholder = Some(placeholder);
        self
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
        let lines = Wrapped::new(editor, room).rows.len();
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
        if self.editor.is_empty() {
            // The placeholder starts where the text will, under the cursor.
            let placeholder = self
                .placeholder
                .unwrap_or_else(|| self.editor.placeholder_text());
            Line::styled(placeholder.to_owned(), ui.muted()).render(text, buf);
            if self.focused {
                buf[(text.x, text.y)].modifier.insert(Modifier::REVERSED);
            }
        } else {
            Wrapped::new(self.editor, usize::from(text.width)).render(
                text,
                buf,
                ui.text(),
                self.focused,
            );
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

/// An editor's text wrapped at word boundaries, as the prompt shows it. Unlike the text
/// area's own wrap, spaces hang past the end of a row rather than start the next one, so a
/// word that just fits never leaves the next row starting with a space.
struct Wrapped<'e> {
    lines: &'e [String],
    /// Each row: its line, and the byte range of that line it shows.
    rows: Vec<(usize, usize, usize)>,
    /// The cursor's row and column.
    cursor: (usize, usize),
}

impl<'e> Wrapped<'e> {
    fn new(editor: &'e TextArea, width: usize) -> Self {
        let width = width.max(1);
        let lines = editor.lines();
        let ratatui_textarea::DataCursor(cursor_line, cursor_char) = editor.cursor();
        let mut rows = Vec::new();
        let mut cursor = (0, 0);
        for (at, line) in lines.iter().enumerate() {
            let first = rows.len();
            for (start, end) in wrap(line, width) {
                rows.push((at, start, end));
            }
            if at == cursor_line {
                let byte = line
                    .char_indices()
                    .nth(cursor_char)
                    .map_or(line.len(), |(byte, _)| byte);
                // The last row that starts at or before the cursor.
                let row = (first..rows.len())
                    .rev()
                    .find(|&row| rows[row].1 <= byte)
                    .unwrap_or(first);
                let column = super::width(&line[rows[row].1..byte]);
                cursor = if column >= width {
                    // Past a full row's hanging spaces: the start of a row of its own.
                    rows.insert(row + 1, (at, byte, byte));
                    (row + 1, 0)
                } else {
                    (row, column)
                };
            }
        }
        Self {
            lines,
            rows,
            cursor,
        }
    }

    /// Draws the rows around the cursor that fit in `area`.
    fn render(&self, area: Rect, buf: &mut Buffer, style: Style, focused: bool) {
        let height = usize::from(area.height);
        let first = (self.cursor.0 + 1).saturating_sub(height);
        for (y, &(line, start, end)) in (area.y..area.bottom()).zip(&self.rows[first..]) {
            let row = Rect::new(area.x, y, area.width, 1);
            Line::styled(&self.lines[line][start..end], style).render(row, buf);
        }
        if focused {
            let y = area.y + u16::try_from(self.cursor.0 - first).unwrap_or(0);
            let x = area.x + u16::try_from(self.cursor.1).unwrap_or(0);
            if let Some(cell) = buf.cell_mut((x.min(area.right() - 1), y)) {
                cell.modifier.insert(Modifier::REVERSED);
            }
        }
    }
}

/// The byte ranges of `line`'s rows, `width` columns wide: words move to the next row whole
/// unless longer than a row, and the spaces after a word stay on its row.
fn wrap(line: &str, width: usize) -> Vec<(usize, usize)> {
    let mut rows = Vec::new();
    let mut start = 0;
    let mut used = 0;
    let mut at = 0;
    while at < line.len() {
        let rest = &line[at..];
        let word = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let spaces = rest[word..]
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(rest.len() - word);
        let word_width = super::width(&rest[..word]);
        if used > 0 && used + word_width > width {
            rows.push((start, at));
            start = at;
            used = 0;
        }
        if word_width > width {
            // Longer than a row: split it at the row's edge.
            for (byte, c) in rest[..word].char_indices() {
                let mut buf = [0; 4];
                let w = super::width(c.encode_utf8(&mut buf));
                if used + w > width && used > 0 {
                    rows.push((start, at + byte));
                    start = at + byte;
                    used = 0;
                }
                used += w;
            }
        } else {
            used += word_width;
        }
        used += super::width(&rest[word..word + spaces]);
        at += word + spaces;
    }
    rows.push((start, line.len()));
    rows
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
        (&*self.editor).render(inner, buf);
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
                    let focused = editor("write the docs page too, and link it from @README.md");
                    let height = Prompt::height(&focused, width, 6, true);
                    assert_eq!(height, if width < 50 { 5 } else { 4 });
                    Prompt::new(ui, &focused)
                        .meta(meta(ui))
                        .focused(true)
                        .render(Rect { height, ..area }, buf);
                    // Unfocused and empty: the placeholder, the bar in the border colour.
                    let empty = editor("");
                    let below = Rect {
                        y: area.y + height + 1,
                        height: Prompt::height(&empty, width, 6, true),
                        ..area
                    };
                    Prompt::new(ui, &empty).meta(meta(ui)).render(below, buf);
                    let answer = editor("h2 under Reference");
                    let last = Rect {
                        y: below.bottom() + 1,
                        height: 2,
                        ..area
                    };
                    Prompt::new(ui, &answer)
                        .accent(ui.theme.attention)
                        .focused(true)
                        .render(last, buf);
                })
            });
        }
    }

    #[test]
    fn a_wrapped_row_never_starts_with_a_space() {
        let text = "write the docs page too, and link it from @README.md";
        let shown = |width| -> Vec<&str> {
            wrap(text, width)
                .into_iter()
                .map(|(start, end)| &text[start..end])
                .collect()
        };
        // "from" ends the row exactly: its space hangs, "@README.md" starts the next.
        assert_eq!(
            shown(41),
            ["write the docs page too, and link it from ", "@README.md"]
        );
        assert_eq!(shown(100), [text]);
        assert_eq!(
            shown(4)[..6],
            ["writ", "e ", "the ", "docs ", "page ", "too, "]
        );
        // The cursor after a full row's space starts a row of its own.
        let mut editor = editor("abcd ");
        editor.move_cursor(ratatui_textarea::CursorMove::End);
        let wrapped = Wrapped::new(&editor, 4);
        assert_eq!(wrapped.rows.len(), 2);
        assert_eq!(wrapped.cursor, (1, 0));
        assert_eq!(Prompt::height(&editor, 7, 6, false), 3);
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
