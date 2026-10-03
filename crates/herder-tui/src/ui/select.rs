//! Pickers: a [`Dialog`] with a search field over a [`ListView`], the list filtered as the
//! search is typed, and lines under the list for what the choice does.
//!
//! ```text
//! ┌─ switch · api ─────────────────── esc ─┐
//! │                                        │
//! │  search  clau▌                         │
//! │                                        │
//! │  same provider · conversation continues│
//! │▶ ● claude-main            5h 38%       │
//! │                                        │
//! │  model: opus                           │
//! │                                        │
//! │  enter switch  tab model  esc close    │
//! └────────────────────────────────────────┘
//! ```
//!
//! The caller filters (see [`crate::fuzzy`]) and keeps the cursor; the picker draws, scrolls
//! the list to the cursor and says where each row landed, for taps.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Widget;
use ratatui_textarea::TextArea;

use super::dialog::{Areas, Dialog, Size};
use super::hints::Hint;
use super::input::Field;
use super::list::{ListView, Row};
use super::{Ui, fit};

/// Most list rows a picker shows before it scrolls.
pub const MAX_ROWS: u16 = 12;

/// Where a drawn picker's parts are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    /// The dialog's frame.
    pub dialog: Areas,
    /// The search field's row.
    pub search: Rect,
    /// Each list row drawn, by index into the rows, and where.
    pub rows: Vec<(usize, Rect)>,
    /// Each line under the list, by index, and where.
    pub footer: Vec<(usize, Rect)>,
}

/// A picker ready to draw.
pub struct Select<'a, 'b> {
    ui: Ui<'a>,
    title: Line<'a>,
    size: Size,
    label: &'a str,
    search: &'a mut TextArea<'b>,
    searching: bool,
    rows: Vec<Row<'a>>,
    selected: Option<usize>,
    empty: &'a str,
    footer: Vec<Line<'a>>,
    hints: &'a [Hint],
}

impl<'a, 'b> Select<'a, 'b> {
    pub fn new(
        ui: Ui<'a>,
        title: impl Into<Line<'a>>,
        size: Size,
        search: &'a mut TextArea<'b>,
    ) -> Self {
        Self {
            ui,
            title: title.into(),
            size,
            label: "search",
            search,
            searching: true,
            rows: Vec::new(),
            selected: None,
            empty: "nothing matches",
            footer: Vec::new(),
            hints: &[],
        }
    }

    /// The search field's label.
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = label;
        self
    }

    /// Whether keys type into the search: its field and the list's cursor are drawn solid.
    /// Off while a field under the list has them.
    pub fn searching(mut self, searching: bool) -> Self {
        self.searching = searching;
        self
    }

    /// The list, filtered, and its cursor by index into it.
    pub fn rows(mut self, rows: Vec<Row<'a>>, selected: Option<usize>) -> Self {
        self.rows = rows;
        self.selected = selected;
        self
    }

    /// What shows in place of an empty list.
    pub fn empty(mut self, empty: &'a str) -> Self {
        self.empty = empty;
        self
    }

    /// Lines under the list, past a blank row: a field, what the choice does, an error.
    pub fn footer(mut self, footer: Vec<Line<'a>>) -> Self {
        self.footer = footer;
        self
    }

    /// Hints at the dialog's foot.
    pub fn hints(mut self, hints: &'a [Hint]) -> Self {
        self.hints = hints;
        self
    }

    /// Draws the picker centred in `screen`, the list from `offset`, moved so the cursor
    /// shows.
    pub fn render(self, screen: Rect, buf: &mut Buffer, offset: &mut usize) -> Placed {
        let ui = self.ui;
        let dialog = Dialog::new(ui, self.title, self.size).hints(self.hints);
        let footer = u16::try_from(self.footer.len()).unwrap_or(u16::MAX);
        // The search and the blank row under it; the footer and the blank row over it.
        let fixed = 2 + if footer > 0 { footer + 1 } else { 0 };
        let room = screen
            .height
            .saturating_sub(dialog.chrome() + fixed)
            .clamp(1, MAX_ROWS);
        let total: usize = self.rows.iter().map(Row::height).sum();
        let list = u16::try_from(total).unwrap_or(u16::MAX).clamp(1, room);
        let areas = dialog.render(screen, fixed + list, buf);
        let body = areas.body;
        let row = |y: u16, height: u16| Rect::new(body.x, y, body.width, height).intersection(body);
        let search = row(body.y, 1);
        Field::new(ui, self.label, self.search)
            .focused(self.searching)
            .render(search, buf, label_width(self.label));
        // The pointer sits in the padding, so headers line up with the search and the
        // footer, and the cursor's row runs a column past the text either side.
        let list_area = Rect::new(body.x.saturating_sub(1), body.y + 2, body.width + 2, list)
            .intersection(areas.outer);
        let rows = if self.rows.is_empty() {
            Line::styled(self.empty, ui.muted()).render(row(list_area.y, 1), buf);
            Vec::new()
        } else {
            ListView::new(ui, self.rows)
                .select(self.selected)
                .focused(self.searching)
                .render(list_area, buf, offset)
        };
        let mut lines = Vec::new();
        let top = list_area.bottom() + 1;
        for (at, (line, y)) in self.footer.into_iter().zip(top..).enumerate() {
            let area = row(y, 1);
            if area.is_empty() {
                break;
            }
            fit(line, usize::from(area.width), ui.glyphs).render(area, buf);
            lines.push((at, area));
        }
        Placed {
            dialog: areas,
            search,
            rows,
            footer: lines,
        }
    }
}

/// Columns a field's label takes: the label and two spaces.
pub fn label_width(label: &str) -> u16 {
    u16::try_from(super::width(label) + 2).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use ratatui::text::{Span, Text};

    use super::super::snapshot;
    use super::super::state::{self, State};
    use super::*;

    fn rows(ui: Ui) -> Vec<Row<'static>> {
        let item = |state, title: &'static str, meta: &'static str| {
            Row::item(Line::from(vec![
                state::dot(ui, state),
                Span::styled(format!(" {title}"), ui.text()),
            ]))
            .right(Span::styled(meta, ui.muted()))
        };
        vec![
            Row::header("same provider · conversation continues"),
            item(State::Running, "claude-main", "5h 38%"),
            item(State::Idle, "claude-alt", "5h 4%"),
            Row::Gap,
            Row::header("other provider · replays the transcript"),
            item(State::Idle, "codex-work", "day 91%"),
        ]
    }

    #[test]
    fn pickers() {
        let hints = [Hint::new("enter", "switch"), Hint::new("esc", "close")];
        for width in [45, 100] {
            snapshot::each(&format!("select-{width}"), |variant| {
                snapshot::render(variant, width, 20, |ui, area, buf| {
                    Text::styled("behind\n".repeat(20), ui.text()).render(area, buf);
                    let mut search = TextArea::from(["cl"]);
                    let mut offset = 0;
                    let placed = Select::new(ui, "switch · api", Size::Medium, &mut search)
                        .rows(rows(ui), Some(2))
                        .footer(vec![Line::styled("model: opus", ui.muted())])
                        .hints(&hints)
                        .render(area, buf, &mut offset);
                    // Each item lands on its own row, the cursor's too.
                    assert_eq!(
                        placed.rows.iter().map(|(at, _)| *at).collect::<Vec<_>>(),
                        [1, 2, 5]
                    );
                    assert_eq!(placed.footer.len(), 1);
                })
            });
        }
    }

    #[test]
    fn a_long_list_scrolls_to_the_cursor_and_an_empty_one_says_so() {
        let variant = snapshot::variants().remove(0);
        let ui = variant.ui();
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 14));
        let mut search = TextArea::default();
        let many: Vec<Row> = (0..30).map(|at| Row::item(format!("row {at}"))).collect();
        let mut offset = 0;
        let placed = Select::new(ui, "go to", Size::Medium, &mut search)
            .rows(many, Some(29))
            .render(buf.area, &mut buf, &mut offset);
        assert_eq!(placed.rows.last().map(|(at, _)| *at), Some(29));
        assert!(offset > 0);

        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 14));
        let placed = Select::new(ui, "go to", Size::Medium, &mut search)
            .empty("no session matches")
            .render(buf.area, &mut buf, &mut 0);
        assert!(placed.rows.is_empty());
        let text: String = (0..60)
            .map(|x| buf[(x, placed.search.y + 2)].symbol().to_owned())
            .collect();
        assert!(text.contains("no session matches"), "{text}");
    }
}
