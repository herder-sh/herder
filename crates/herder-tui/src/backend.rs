//! The terminal backend: crossterm's, with every cell after a non-ASCII one placed by an
//! explicit cursor move.
//!
//! ratatui writes a run of adjacent cells without moving the cursor, trusting the terminal to
//! advance one column per cell. A terminal that draws a symbol wider than ratatui counted, as
//! a phone SSH app over mosh may for `●` or `…`, then shifts the rest of the row right. Placing
//! the cell after any non-ASCII symbol keeps such a mistake to that one cell. (Herdr's renderer,
//! Apache-2.0, writes inline only after one-column ASCII for the same reason.)

use std::io;

use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};

/// [`CrosstermBackend`] that moves the cursor after every non-ASCII cell.
pub struct Anchored<W: io::Write>(pub CrosstermBackend<W>);

impl<W: io::Write> Backend for Anchored<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let cells: Vec<_> = content.collect();
        // Each draw starts with a cursor move, so every run ends at a non-ASCII cell.
        for run in cells.split_inclusive(|(_, _, cell)| !cell.symbol().is_ascii()) {
            self.0.draw(run.iter().copied())?;
        }
        Ok(())
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.0.append_lines(n)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.0.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.0.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.0.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.0.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.0.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.0.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        self.0.size()
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.0.window_size()
    }

    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.0)
    }
}

/// The TUI's terminal.
pub type Tui = ratatui::Terminal<Anchored<io::Stdout>>;

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

    /// A writer whose bytes the test can read back.
    #[derive(Clone, Default)]
    struct Shared(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

    impl io::Write for Shared {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().write(bytes)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// What drawing `line` on row 0 writes.
    fn written(line: &str) -> String {
        let buffer = Buffer::with_lines([line]);
        let cells = buffer
            .content
            .iter()
            .enumerate()
            .map(|(x, cell)| (u16::try_from(x).unwrap(), 0, cell));
        let out = Shared::default();
        Anchored(CrosstermBackend::new(out.clone()))
            .draw(cells)
            .unwrap();
        String::from_utf8(out.0.take()).unwrap()
    }

    #[test]
    fn the_cell_after_a_symbol_is_placed_and_ascii_runs_are_not() {
        let out = written("ab● c…d");
        // Rows and columns from 1: the cursor moves to the start, after `●` and after `…`.
        assert!(out.contains("\x1b[1;1Ha"), "{out:?}");
        assert!(!out.contains("\x1b[1;2H"), "{out:?}");
        assert!(out.contains("●"), "{out:?}");
        assert!(out.contains("\x1b[1;4H "), "{out:?}");
        assert!(out.contains("\x1b[1;7Hd"), "{out:?}");
    }
}
