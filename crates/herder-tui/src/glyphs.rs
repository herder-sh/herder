//! Which glyphs the screen may show.
//!
//! A terminal and the program behind it each decide how many columns a character takes. For
//! East Asian Ambiguous characters (`·`, `…`, `●`, `◆`, `→`, ...) and symbols with an emoji
//! form (`▪`, `⚙`) they disagree: mosh and ratatui count one column where a phone SSH app may
//! draw two, which shifts the rest of the row right and leaves stale cells behind. With
//! [`Glyphs::Ascii`] the finished frame swaps each such symbol for an ASCII one before it is
//! written, wherever it came from: the TUI's own marks or a transcript's text. Box-drawing
//! borders stay; every terminal draws them one column wide.
//!
//! Unless `:glyphs` chose, narrow screens, as on a phone, get ASCII.

use ratatui::buffer::Buffer;

use crate::views::NARROW;

/// A glyph set, as `:glyphs` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyphs {
    /// Any symbol.
    Unicode,
    /// ASCII in place of the symbols a terminal may draw wider than one column.
    Ascii,
}

impl Glyphs {
    /// The set `name` names.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "unicode" => Some(Self::Unicode),
            "ascii" => Some(Self::Ascii),
            _ => None,
        }
    }

    /// The set's name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Unicode => "unicode",
            Self::Ascii => "ascii",
        }
    }

    /// The set for a screen drawn `width` columns wide: `chosen`, else ASCII on a narrow
    /// screen.
    pub fn for_width(chosen: Option<Self>, width: u16) -> Self {
        chosen.unwrap_or(if width < NARROW {
            Self::Ascii
        } else {
            Self::Unicode
        })
    }
}

/// Swaps every one-column symbol in `buffer` that may not be one column wide for its ASCII
/// stand-in.
pub fn fold(buffer: &mut Buffer) {
    for cell in &mut buffer.content {
        let mut chars = cell.symbol().chars();
        if let (Some(c), None) = (chars.next(), chars.next())
            && let Some(ascii) = ascii(c)
        {
            cell.set_char(ascii);
        }
    }
}

/// `text` as [`fold`] shows it.
#[cfg(test)]
pub fn folded(text: &str) -> String {
    text.chars().map(|c| ascii(c).unwrap_or(c)).collect()
}

/// The ASCII stand-in for `c`, if it needs one.
fn ascii(c: char) -> Option<char> {
    let ascii = match c {
        '·' | '–' | '—' | '‒' | '―' | '−' | '░' => '-',
        '…' => '.',
        '●' | '•' | '◉' | '★' => '*',
        '○' | '◌' | '◯' | '◦' => 'o',
        '◆' | '◇' => '+',
        '▪' | '■' | '□' => '_',
        '✗' | '✘' | '×' => 'x',
        '✓' | '✔' => 'v',
        '‹' | '«' | '←' | '⌫' | '◀' | '◂' => '<',
        '›' | '»' | '→' | '⏎' | '▶' | '▸' | '⚙' => '>',
        '↑' | '▲' | '▴' => '^',
        '↓' | '▼' | '▾' => 'v',
        '▌' | '▏' | '▎' | '▍' | '¦' => '|',
        '█' | '▓' | '▒' => '#',
        '⎿' => 'L',
        '‘' | '’' | '‚' | '′' => '\'',
        '“' | '”' | '„' | '″' => '"',
        '\u{a0}' | '\u{2000}'..='\u{200a}' | '\u{202f}' | '\u{205f}' => ' ',
        // Box drawing: one column everywhere.
        '\u{2500}'..='\u{257f}' => return None,
        // Latin-1's symbols, punctuation to arrows, shapes, dingbats and technical symbols:
        // wide on some terminals, one column on others.
        '\u{a1}'..='\u{bf}' | '\u{2010}'..='\u{2bff}' => '?',
        _ => return None,
    };
    Some(ascii)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_leaves_one_column_ascii_and_borders() {
        let mut buffer = Buffer::with_lines(["┌ a · b … ● ◆ ▪ → ⚙ ✓ ✗ ¿ ą 日 ┐"]);
        fold(&mut buffer);
        assert_eq!(
            buffer,
            Buffer::with_lines(["┌ a - b . * + _ > > v x ? ą 日 ┐"])
        );
    }

    #[test]
    fn narrow_screens_default_to_ascii() {
        assert_eq!(Glyphs::for_width(None, 45), Glyphs::Ascii);
        assert_eq!(Glyphs::for_width(None, 120), Glyphs::Unicode);
        assert_eq!(
            Glyphs::for_width(Some(Glyphs::Unicode), 45),
            Glyphs::Unicode
        );
        assert_eq!(Glyphs::for_width(Some(Glyphs::Ascii), 120), Glyphs::Ascii);
        assert_eq!(Glyphs::parse("ascii").map(Glyphs::name), Some("ascii"));
        assert_eq!(Glyphs::parse("emoji"), None);
    }
}
