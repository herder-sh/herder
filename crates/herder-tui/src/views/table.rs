//! GitHub-flavoured pipe tables in the transcript: a header row, a `|---|:-:|` delimiter row,
//! then body rows. Columns take their widest cell's width; a table wider than the transcript
//! narrows its widest columns first, wrapping their cells, and one too wide even for that shows
//! each row as `header: value` lines instead.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::markdown::{Run, inline, wrap};
use super::transcript::Row;
use crate::ui::{Ui, line_width, width};

/// How narrow a column gets, unless all its cells are narrower, before the table shows its
/// rows as blocks instead.
const NARROWEST: usize = 6;

/// How a column's cells line up, from the colons of its delimiter cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Align {
    Left,
    Center,
    Right,
}

/// A table's cells, as Markdown text.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Table {
    header: Vec<String>,
    aligns: Vec<Align>,
    body: Vec<Vec<String>>,
}

impl Table {
    /// The table `header` and `delimiter` start, if they start one. A delimiter with fewer
    /// cells than the header is still streaming in: its missing columns align left.
    pub fn start(header: &str, delimiter: &str) -> Option<Self> {
        if !header.contains('|') || !delimiter.contains('|') {
            return None;
        }
        let header = cells(header);
        let mut aligns = cells(delimiter)
            .iter()
            .map(|cell| align(cell))
            .collect::<Option<Vec<_>>>()?;
        if aligns.len() > header.len() {
            return None;
        }
        aligns.resize(header.len(), Align::Left);
        Some(Self {
            header,
            aligns,
            body: Vec::new(),
        })
    }

    /// Adds a body row, padded or cut to the header's columns, as GitHub does.
    pub fn push(&mut self, line: &str) {
        let mut row = cells(line);
        row.resize(self.header.len(), String::new());
        self.body.push(row);
    }

    /// The table's rows in a transcript `indent` columns in, with `room` columns after it.
    pub fn rows(&self, ui: Ui, indent: usize, room: usize) -> Vec<Row> {
        let base = Style::new().fg(ui.theme.markdown_text);
        let bold = |text: &str| -> Vec<Run> {
            inline(ui, text, base.add_modifier(Modifier::BOLD))
                .into_iter()
                .map(|(text, style)| (text, style.add_modifier(Modifier::BOLD)))
                .collect()
        };
        let header: Vec<Vec<Run>> = self.header.iter().map(|cell| bold(cell)).collect();
        let body: Vec<Vec<Vec<Run>>> = self
            .body
            .iter()
            .map(|row| row.iter().map(|cell| inline(ui, cell, base)).collect())
            .collect();
        let Some(widths) = widths(&header, &body, room) else {
            return blocks(&header, &body, indent, room);
        };
        let pad = " ".repeat(indent);
        let border = Style::new().fg(ui.theme.border);
        let rule = |left: &str, mid: &str, right: &str| {
            let mut text = String::from(left);
            for (i, w) in widths.iter().enumerate() {
                if i > 0 {
                    text.push_str(mid);
                }
                text.push_str(&"─".repeat(w + 2));
            }
            text.push_str(right);
            Row::new(Line::from(vec![
                Span::raw(pad.clone()),
                Span::styled(text, border),
            ]))
        };
        let lines = |cells: &[Vec<Run>]| -> Vec<Row> {
            let wrapped: Vec<Vec<Line<'static>>> = cells
                .iter()
                .zip(&widths)
                .map(|(runs, &w)| wrap(runs, w, &[], &[]))
                .collect();
            let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
            (0..height)
                .map(|k| {
                    let mut spans = vec![Span::raw(pad.clone()), Span::styled("│", border)];
                    for ((cell, &w), &align) in wrapped.iter().zip(&widths).zip(&self.aligns) {
                        let line = cell.get(k).cloned().unwrap_or_default();
                        let gap = w.saturating_sub(line_width(&line));
                        let left = match align {
                            Align::Left => 0,
                            Align::Center => gap / 2,
                            Align::Right => gap,
                        };
                        spans.push(Span::raw(" ".repeat(left + 1)));
                        spans.extend(line.spans);
                        spans.push(Span::raw(" ".repeat(gap - left + 1)));
                        spans.push(Span::styled("│", border));
                    }
                    Row::new(Line::from(spans))
                })
                .collect()
        };
        let body: Vec<Vec<Row>> = body.iter().map(|row| lines(row)).collect();
        // Wrapped cells get a rule between rows, so a row's lines read as one.
        let ruled = body.iter().any(|row| row.len() > 1);
        let mut rows = vec![rule("┌", "┬", "┐")];
        rows.extend(lines(&header));
        rows.push(rule("├", "┼", "┤"));
        for (i, row) in body.into_iter().enumerate() {
            if ruled && i > 0 {
                rows.push(rule("├", "┼", "┤"));
            }
            rows.extend(row);
        }
        rows.push(rule("└", "┴", "┘"));
        rows
    }
}

/// Each column's width for a table drawn in `room` columns: its widest cell's, narrowing the
/// widest columns first while the table is too wide. `None` if it cannot fit.
fn widths(header: &[Vec<Run>], body: &[Vec<Vec<Run>>], room: usize) -> Option<Vec<usize>> {
    let runs_width = |runs: &Vec<Run>| runs.iter().map(|(text, _)| width(text)).sum::<usize>();
    let mut widths: Vec<usize> = header.iter().map(|cell| runs_width(cell).max(1)).collect();
    for row in body {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(runs_width(cell));
        }
    }
    // A border, then each column's text between a space each side and a border.
    let room = room.checked_sub(1 + 3 * widths.len())?;
    if widths.iter().map(|&w| w.min(NARROWEST)).sum::<usize>() > room {
        return None;
    }
    let mut total: usize = widths.iter().sum();
    while total > room {
        // Narrows the first widest column; it stays wider than the narrowest a column gets,
        // as the table would fit with every column that narrow.
        let widest = (0..widths.len()).max_by_key(|&i| (widths[i], std::cmp::Reverse(i)))?;
        widths[widest] -= 1;
        total -= 1;
    }
    Some(widths)
}

/// A table too wide for the transcript, as a block of `header: value` lines per row.
fn blocks(header: &[Vec<Run>], body: &[Vec<Vec<Run>>], indent: usize, room: usize) -> Vec<Row> {
    let pad = " ".repeat(indent);
    let hang = [Span::raw("  ")];
    let mut rows = Vec::new();
    for (i, row) in body.iter().enumerate() {
        if i > 0 {
            rows.push(Row::new(Line::raw("")));
        }
        for (label, value) in header.iter().zip(row) {
            if value.iter().all(|(text, _)| text.trim().is_empty()) {
                continue;
            }
            let mut runs = label.clone();
            if runs.iter().any(|(text, _)| !text.trim().is_empty()) {
                let style = runs.last().map_or_else(Style::new, |(_, style)| *style);
                runs.push((": ".to_owned(), style));
            }
            runs.extend(value.iter().cloned());
            for line in wrap(&runs, room.max(1), &[], &hang) {
                let mut spans = vec![Span::raw(pad.clone())];
                spans.extend(line.spans);
                rows.push(Row::new(Line::from(spans)));
            }
        }
    }
    rows
}

/// A row's cells between unescaped pipes, trimmed. The outer pipes are optional and `\|` is
/// a literal pipe.
fn cells(line: &str) -> Vec<String> {
    let mut text = line.trim();
    text = text.strip_prefix('|').unwrap_or(text);
    if !text.ends_with("\\|") {
        text = text.strip_suffix('|').unwrap_or(text);
    }
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('|') => cell.push('|'),
                Some(next) => {
                    cell.push('\\');
                    cell.push(next);
                }
                None => cell.push('\\'),
            },
            '|' => cells.push(std::mem::take(&mut cell)),
            c => cell.push(c),
        }
    }
    cells.push(cell);
    cells
        .into_iter()
        .map(|cell| cell.trim().to_owned())
        .collect()
}

/// A delimiter cell's alignment: `---`, `:--`, `:-:` or `--:`.
fn align(cell: &str) -> Option<Align> {
    let dashes = cell.trim_matches(':');
    if dashes.is_empty() || !dashes.chars().all(|c| c == '-') {
        return None;
    }
    Some(match (cell.starts_with(':'), cell.ends_with(':')) {
        (true, true) => Align::Center,
        (false, true) => Align::Right,
        _ => Align::Left,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_split_on_unescaped_pipes() {
        assert_eq!(cells("| | a | b\\|c |"), ["", "a", "b|c"]);
        assert_eq!(cells("a | b"), ["a", "b"]);
        assert_eq!(align(":-:"), Some(Align::Center));
        assert_eq!(align("--:"), Some(Align::Right));
        assert_eq!(align(":--"), Some(Align::Left));
        assert_eq!(align("x"), None);
    }

    #[test]
    fn a_delimiter_starts_a_table_only_with_no_more_cells_than_the_header() {
        assert!(Table::start("| a | b |", "|---|---|").is_some());
        // Still streaming in.
        assert!(Table::start("| a | b |", "|---|").is_some());
        assert!(Table::start("| a | b |", "|---|---|---|").is_none());
        assert!(Table::start("a | b", "---").is_none());
        assert!(Table::start("plain", "|---|").is_none());
    }

    #[test]
    fn the_widest_columns_narrow_first() {
        let run = |n: usize| vec![("x".repeat(n), Style::new())];
        let header = vec![run(2), run(20), run(10)];
        // 1 + 3 * 3 borders and padding, then 22 for text.
        assert_eq!(widths(&header, &[], 32), Some(vec![2, 10, 10]));
        assert_eq!(widths(&header, &[], 31), Some(vec![2, 9, 10]));
        assert_eq!(widths(&header, &[], 100), Some(vec![2, 20, 10]));
        // Two columns of six and the two-wide one need 14.
        assert_eq!(widths(&header, &[], 24), Some(vec![2, 6, 6]));
        assert_eq!(widths(&header, &[], 23), None);
    }
}
