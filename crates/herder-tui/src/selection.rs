//! Selecting transcript text with the mouse, as in OpenCode's TUI: a drag selects, and the
//! release copies the selection with OSC 52 ([`Effect::Copy`]). A double-click selects a
//! word, a triple-click a line.
//!
//! A selection is kept in the transcript's rows as last built ([`crate::views`]), not in
//! screen cells, so what is copied is the text: no sidebar, bars, indents or hint bar, wrapped
//! lines joined back, and code with its own newlines and indentation. A drag past the top or
//! bottom edge scrolls. On a narrow screen a drag still scrolls, as a finger would.

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use crate::app::{App, Effect};
use crate::views::{Join, TranscriptRow};

/// The longest gap between the clicks of a double- or triple-click.
const MULTI_CLICK: Duration = Duration::from_millis(400);

/// A cell of the transcript: a row as built, and a column in it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Spot {
    pub row: usize,
    pub col: u16,
}

/// The selected text, from `anchor` (where the drag started) to `head`, both cells in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Selection {
    pub anchor: Spot,
    pub head: Spot,
    /// Whether it is more than a press: a drag, or a word or line a double- or triple-click
    /// picked. A press alone selects nothing and stays a tap.
    pub grown: bool,
}

impl Selection {
    /// The first and the last cell, in order.
    pub fn span(&self) -> (Spot, Spot) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }

    /// The columns of `row` selected, both in, if any.
    pub fn columns(&self, row: usize) -> Option<(u16, u16)> {
        let (from, to) = self.span();
        if !self.grown || row < from.row || row > to.row {
            return None;
        }
        let lo = if row == from.row { from.col } else { 0 };
        let hi = if row == to.row { to.col } else { u16::MAX };
        Some((lo, hi))
    }
}

/// The clicks of a multi-click so far: where, when, how many.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Clicks {
    at: Spot,
    when: Instant,
    count: u8,
}

/// `row`'s cells: each character with the column it starts in.
fn cells(row: &TranscriptRow) -> Vec<(u16, char)> {
    let mut col: u16 = 0;
    let mut cells = Vec::new();
    for span in &row.line.spans {
        for c in span.content.chars() {
            let mut buf = [0; 4];
            let width = crate::ui::width(c.encode_utf8(&mut buf));
            if width == 0 {
                continue;
            }
            cells.push((col, c));
            col = col.saturating_add(u16::try_from(width).unwrap_or(1));
        }
    }
    cells
}

/// `row`'s text from column `lo` to `hi`, both in, past the row's bars and indents.
fn slice(row: &TranscriptRow, lo: u16, hi: u16) -> String {
    let lo = lo.max(row.text);
    cells(row)
        .into_iter()
        .filter(|(col, _)| (lo..=hi).contains(col))
        .map(|(_, c)| c)
        .collect()
}

/// The text `selection` covers in `rows`: rows on lines of their own end in a newline,
/// wrapped ones join back into their line.
pub(crate) fn text(rows: &[TranscriptRow], selection: &Selection) -> String {
    let (from, to) = selection.span();
    let mut out = String::new();
    for (at, row) in rows.iter().enumerate().take(to.row + 1).skip(from.row) {
        let Some((lo, hi)) = selection.columns(at) else {
            continue;
        };
        let part = slice(row, lo, hi);
        let part = part.trim_end();
        if at == from.row {
            out.push_str(part);
            continue;
        }
        match row.join {
            Join::Line => {
                out.push('\n');
                out.push_str(part);
            }
            Join::Space => {
                out.push(' ');
                out.push_str(part.trim_start());
            }
            Join::Glued => out.push_str(part.trim_start()),
        }
    }
    out
}

/// Whether `c` belongs to a word a double-click selects: paths and identifiers whole.
fn wordy(c: char) -> bool {
    c.is_alphanumeric() || "_-./~".contains(c)
}

/// The word of `rows` at `at`, as a selection; none on a space.
fn word(rows: &[TranscriptRow], at: Spot) -> Option<Selection> {
    let row = rows.get(at.row)?;
    let cells: Vec<(u16, char)> = cells(row)
        .into_iter()
        .filter(|(col, _)| *col >= row.text)
        .collect();
    let hit = cells.iter().rposition(|(col, _)| *col <= at.col)?;
    if !wordy(cells[hit].1) {
        return None;
    }
    let start = cells[..hit]
        .iter()
        .rposition(|(_, c)| !wordy(*c))
        .map_or(0, |at| at + 1);
    let end = cells[hit..]
        .iter()
        .position(|(_, c)| !wordy(*c))
        .map_or(cells.len(), |len| hit + len);
    Some(Selection {
        anchor: Spot {
            row: at.row,
            col: cells[start].0,
        },
        head: Spot {
            row: at.row,
            col: cells[end - 1].0,
        },
        grown: true,
    })
}

/// The line of `rows` at `at`, with the rows it wrapped onto, as a selection.
fn line(rows: &[TranscriptRow], at: Spot) -> Option<Selection> {
    rows.get(at.row)?;
    let first = (0..=at.row)
        .rev()
        .find(|row| rows[*row].join == Join::Line)
        .unwrap_or(0);
    let last = (at.row + 1..rows.len())
        .find(|row| rows[*row].join == Join::Line)
        .map_or(rows.len() - 1, |next| next - 1);
    Some(Selection {
        anchor: Spot { row: first, col: 0 },
        head: Spot {
            row: last,
            col: u16::MAX,
        },
        grown: true,
    })
}

/// Marks the selected cells of the rows drawn in `area`, `first` the row at its top.
pub(crate) fn highlight(
    buf: &mut Buffer,
    area: Rect,
    rows: &[TranscriptRow],
    first: usize,
    selection: &Selection,
) {
    for (y, (at, row)) in (area.y..area.bottom()).zip(rows.iter().enumerate().skip(first)) {
        let Some((lo, hi)) = selection.columns(at) else {
            continue;
        };
        let end = u16::try_from(crate::ui::line_width(&row.line)).unwrap_or(u16::MAX);
        let lo = lo.max(row.text);
        for col in lo..hi.saturating_add(1).min(end).min(area.width) {
            buf[(area.x + col, y)].modifier.insert(Modifier::REVERSED);
        }
    }
}

impl App {
    /// The transcript cell at screen `x`, `y`, if a selection may start there: a wide screen,
    /// the transcript's text under the pointer.
    pub(crate) fn text_at(&self, x: u16, y: u16) -> Option<Spot> {
        let area = self.hits.text?;
        if self.width < crate::views::NARROW || !area.contains(ratatui::layout::Position::new(x, y))
        {
            return None;
        }
        Some(Spot {
            row: self.scroll.first_line() + usize::from(y - area.y),
            col: x - area.x,
        })
    }

    /// A press on the transcript's text at `at`: starts a selection, or on a second or third
    /// click picks the word or the line there.
    pub(crate) fn select_press(&mut self, at: Spot) {
        let now = Instant::now();
        let count = match self.clicks {
            Some(clicks) if clicks.at == at && now - clicks.when < MULTI_CLICK => {
                clicks.count % 3 + 1
            }
            _ => 1,
        };
        self.clicks = Some(Clicks {
            at,
            when: now,
            count,
        });
        let rows = &self.transcript.rows;
        self.selection = match count {
            2 => word(rows, at),
            3 => line(rows, at),
            _ => Some(Selection {
                anchor: at,
                head: at,
                grown: false,
            }),
        };
        if count > 1 {
            // A double- or triple-click is no tap.
            self.pressed = None;
        }
    }

    /// The selection's end dragged to screen `x`, `y`: past the top or the bottom it scrolls.
    pub(crate) fn select_drag(&mut self, x: u16, y: u16) {
        let Some(area) = self.hits.text else {
            return;
        };
        let Some(selection) = &mut self.selection else {
            return;
        };
        if y < area.y {
            self.scroll.by(-1);
        } else if y >= area.bottom() {
            self.scroll.by(1);
        }
        let first = self.scroll.first_line();
        let row = if y < area.y {
            first
        } else {
            first + usize::from(y.min(area.bottom() - 1) - area.y)
        };
        let col = x.clamp(area.x, area.right() - 1) - area.x;
        selection.head = Spot {
            row: row.min(self.transcript.rows.len().saturating_sub(1)),
            col,
        };
        selection.grown = true;
        self.pressed = None;
        self.clicks = None;
    }

    /// The release of a selection: copies it, if it is more than a press.
    pub(crate) fn select_release(&mut self) -> Vec<Effect> {
        let Some(selection) = self.selection.filter(|selection| selection.grown) else {
            self.selection = None;
            return Vec::new();
        };
        let copied = text(&self.transcript.rows, &selection);
        if copied.is_empty() {
            return Vec::new();
        }
        self.notice = Some("copied".to_owned());
        vec![Effect::Copy(copied)]
    }
}

#[cfg(test)]
mod tests;
