//! The one list widget: the sidebar, the inbox, PRs, accounts, the fleet and dialogs.
//!
//! A list is [`Row`]s: group headers, items and gaps. An item is a left line (usually a
//! status dot and a title), a right line (meta: machine, counts) and optional body lines
//! under it. The right line always shows; the left gives way, cut with an ellipsis.
//!
//! The selected item carries the pointer (`▶`) and, while the list has focus, the element
//! background across its full width. The list scrolls to keep it in view.
//!
//! ```text
//!  attention              priority
//! ▶ ◉ docs · app               box
//!   ✓ fix-login · app          box
//!     escalated: timeout
//! ```

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use super::{GAP, INSET, Ui, fill, fit, line_width};

/// A list row.
#[derive(Clone, Debug)]
pub enum Row<'a> {
    /// A group's heading, with meta at the right.
    Header { title: Line<'a>, right: Line<'a> },
    /// What the cursor moves over.
    Item {
        left: Line<'a>,
        right: Line<'a>,
        body: Vec<Line<'a>>,
    },
    /// A blank row between groups.
    Gap,
}

impl<'a> Row<'a> {
    pub fn header(title: impl Into<Line<'a>>) -> Self {
        Self::Header {
            title: title.into(),
            right: Line::default(),
        }
    }

    pub fn item(left: impl Into<Line<'a>>) -> Self {
        Self::Item {
            left: left.into(),
            right: Line::default(),
            body: Vec::new(),
        }
    }

    /// Meta at the right end of a header or an item.
    pub fn right(mut self, line: impl Into<Line<'a>>) -> Self {
        if let Self::Header { right, .. } | Self::Item { right, .. } = &mut self {
            *right = line.into();
        }
        self
    }

    /// Lines under an item, indented to its text.
    pub fn body(mut self, lines: Vec<Line<'a>>) -> Self {
        if let Self::Item { body, .. } = &mut self {
            *body = lines;
        }
        self
    }

    /// Rows the row takes.
    pub fn height(&self) -> usize {
        match self {
            Self::Item { body, .. } => 1 + body.len(),
            _ => 1,
        }
    }
}

/// A list ready to draw.
pub struct ListView<'a> {
    ui: Ui<'a>,
    rows: Vec<Row<'a>>,
    selected: Option<usize>,
    focused: bool,
}

impl<'a> ListView<'a> {
    pub fn new(ui: Ui<'a>, rows: Vec<Row<'a>>) -> Self {
        Self {
            ui,
            rows,
            selected: None,
            focused: false,
        }
    }

    /// The selected row, by index into the rows.
    pub fn select(mut self, selected: Option<usize>) -> Self {
        self.selected = selected;
        self
    }

    /// Whether the list has the keys: the selection is drawn solid.
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Draws the rows from `offset`, a row of the drawn lines, moved so the selection shows.
    /// Returns each item drawn, by row index, and where.
    pub fn render(self, area: Rect, buf: &mut Buffer, offset: &mut usize) -> Vec<(usize, Rect)> {
        let ui = self.ui;
        let area = area.intersection(buf.area);
        let height = usize::from(area.height);
        // Lines above each row.
        let tops: Vec<usize> = self
            .rows
            .iter()
            .scan(0, |top, row| {
                let at = *top;
                *top += row.height();
                Some(at)
            })
            .collect();
        let total = self
            .rows
            .last()
            .map_or(0, |row| tops[tops.len() - 1] + row.height());
        if let Some(selected) = self.selected.filter(|at| *at < self.rows.len()) {
            let top = tops[selected];
            let bottom = top + self.rows[selected].height();
            if top < *offset {
                *offset = top;
            } else if bottom > *offset + height {
                *offset = bottom.saturating_sub(height);
            }
        }
        *offset = (*offset).min(total.saturating_sub(height));

        let mut placed = Vec::new();
        for (at, (row, top)) in self.rows.into_iter().zip(tops).enumerate() {
            let rows = row.height();
            if top + rows <= *offset || top >= *offset + height {
                continue;
            }
            let selected = self.selected == Some(at);
            // Lines of the row in view, and where the first lands.
            let skip = offset.saturating_sub(top);
            let y = area.y + u16::try_from(top + skip - *offset).unwrap_or(u16::MAX);
            let shown = (rows - skip).min(height - (top + skip - *offset));
            let rect = Rect::new(
                area.x,
                y,
                area.width,
                u16::try_from(shown).unwrap_or(u16::MAX),
            );
            match row {
                Row::Gap => {}
                Row::Header { title, right } => {
                    if skip == 0 {
                        let title = Line::from(
                            title
                                .spans
                                .into_iter()
                                .map(|span| span.patch_style(ui.strong()))
                                .collect::<Vec<_>>(),
                        );
                        let right = Line::from(
                            right
                                .spans
                                .into_iter()
                                .map(|span| span.patch_style(ui.muted()))
                                .collect::<Vec<_>>(),
                        );
                        line(ui, rect, buf, INSET, title, right);
                    }
                }
                Row::Item { left, right, body } => {
                    if selected && self.focused {
                        fill(buf, rect, ui.text().bg(ui.theme.background_element));
                    }
                    let mut lines = Vec::with_capacity(rows);
                    lines.push((left, right));
                    lines.extend(body.into_iter().map(|line| (line, Line::default())));
                    for (index, (left, right)) in lines.into_iter().enumerate().skip(skip) {
                        let y = rect.y + u16::try_from(index - skip).unwrap_or(u16::MAX);
                        if y >= rect.bottom() {
                            break;
                        }
                        let row = Rect {
                            y,
                            height: 1,
                            ..rect
                        };
                        if index == 0 {
                            let pointer = if selected { ui.glyphs.pointer } else { " " };
                            let style = if self.focused {
                                ui.accent()
                            } else {
                                ui.muted()
                            };
                            Span::styled(pointer, style).render(row, buf);
                            // Bold, keeping each span's colour: a status dot stays its
                            // state's.
                            let left = if selected {
                                Line::from(
                                    left.spans
                                        .into_iter()
                                        .map(|span| {
                                            span.patch_style(
                                                Style::new().add_modifier(Modifier::BOLD),
                                            )
                                        })
                                        .collect::<Vec<_>>(),
                                )
                            } else {
                                left
                            };
                            line(ui, row, buf, 2, left, right);
                        } else {
                            // Body lines line up with the title, past the status dot.
                            line(ui, row, buf, 4, left, right);
                        }
                    }
                    placed.push((at, rect));
                }
            }
        }
        placed
    }
}

/// `left` from `indent` columns in and `right` at the right end, one column in; `left` cut to
/// leave [`GAP`] columns between them.
fn line(ui: Ui, row: Rect, buf: &mut Buffer, indent: u16, left: Line, right: Line) {
    let start = row.x + indent.min(row.width);
    let end = row.right().saturating_sub(INSET);
    let right_width = u16::try_from(line_width(&right)).unwrap_or(u16::MAX);
    let room = end.saturating_sub(start);
    let (right_width, left_room) = if right_width == 0 {
        (0, room)
    } else if right_width + u16::try_from(GAP).unwrap_or(0) < room {
        (
            right_width,
            room - right_width - u16::try_from(GAP).unwrap_or(0),
        )
    } else {
        // No room for both: the right side goes.
        (0, room)
    };
    if right_width > 0 {
        right.render(Rect::new(end - right_width, row.y, right_width, 1), buf);
    }
    fit(left, usize::from(left_room), ui.glyphs).render(Rect::new(start, row.y, left_room, 1), buf);
}

#[cfg(test)]
mod tests {
    use super::super::snapshot;
    use super::super::state::{self, State};
    use super::*;

    fn rows(ui: Ui) -> Vec<Row<'static>> {
        let item = |state, title: &str, project: &str, machine: &str| {
            let mut spans = vec![state::dot(ui, state), Span::raw(" ")];
            spans.extend(ui.joined([
                Span::styled(title.to_owned(), ui.text()),
                Span::styled(project.to_owned(), ui.muted()),
            ]));
            Row::item(Line::from(spans)).right(Span::styled(machine.to_owned(), ui.muted()))
        };
        vec![
            Row::header("attention").right("priority"),
            item(State::NeedsYou, "docs", "app", "box").body(vec![
                Line::styled("escalated: exceeds authority", ui.muted()),
                Line::styled("Which heading level should the API page use?", ui.text()),
            ]),
            item(State::Error, "deploy-preview", "infra", "box"),
            item(State::Done, "fix-login", "app", "box"),
            item(
                State::Running,
                "a rather long session title that has to give way",
                "herder",
                "m2",
            ),
            Row::Gap,
            Row::header("idle"),
            item(State::Waiting, "queued", "app", "m2"),
            item(State::Idle, "write tests", "app", "box"),
        ]
    }

    #[test]
    fn lists() {
        for width in [45, 100] {
            snapshot::each(&format!("list-{width}"), |variant| {
                snapshot::render(variant, width, 12, |ui, area, buf| {
                    let mut offset = 0;
                    let placed = ListView::new(ui, rows(ui))
                        .select(Some(1))
                        .focused(true)
                        .render(area, buf, &mut offset);
                    assert_eq!(placed.first().map(|(at, r)| (*at, r.height)), Some((1, 3)));
                })
            });
        }
        // Unfocused: the pointer stays, the background goes.
        snapshot::each("list-unfocused", |variant| {
            snapshot::render(variant, 45, 4, |ui, area, buf| {
                let mut offset = 0;
                ListView::new(ui, rows(ui))
                    .select(Some(3))
                    .render(area, buf, &mut offset);
            })
        });
    }

    #[test]
    fn the_list_scrolls_to_the_selection() {
        let variant = snapshot::variants().remove(0);
        let ui = variant.ui();
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 3));
        let mut offset = 0;
        let placed =
            ListView::new(ui, rows(ui))
                .select(Some(7))
                .render(buf.area, &mut buf, &mut offset);
        assert_eq!(offset, 7);
        assert_eq!(placed.last().map(|(at, r)| (*at, r.y)), Some((7, 2)));
        // Back up: the offset follows the selection to the top.
        let placed =
            ListView::new(ui, rows(ui))
                .select(Some(1))
                .render(buf.area, &mut buf, &mut offset);
        assert_eq!(offset, 1);
        assert_eq!(placed[0], (1, Rect::new(0, 0, 30, 3)));
    }
}
