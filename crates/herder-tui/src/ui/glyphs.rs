//! Which glyphs the screen may show, and the marks each set draws ([`GlyphSet`]).
//!
//! Widgets ask the set for every mark they draw, so each set keeps its marks distinct: no two
//! states share an ASCII glyph. [`fold`] stays as a safety net for text the TUI does not
//! choose, such as a transcript's.
//!
//! A terminal and the program behind it each decide how many columns a character takes. For
//! East Asian Ambiguous characters (`·`, `…`, `●`, `◆`, `→`, ...) and symbols with an emoji
//! form (`▪`, `⚙`) they disagree: mosh and ratatui count one column where a phone SSH app may
//! draw two, which shifts the rest of the row right and leaves stale cells behind. With
//! [`Glyphs::Ascii`] the finished frame swaps each such symbol for an ASCII one before it is
//! written, wherever it came from: the TUI's own marks or a transcript's text. Box drawing
//! is folded too (`+ - |`), as it is East Asian Ambiguous as well; views draw panels without
//! lines in the ASCII set where they can.
//!
//! Unless `:glyphs` chose, narrow screens, as on a phone, get ASCII.

use ratatui::buffer::Buffer;

use super::state::State;
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

    /// The marks the set draws.
    pub fn set(self) -> &'static GlyphSet {
        match self {
            Self::Unicode => &UNICODE,
            Self::Ascii => &ASCII,
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

/// The marks of one glyph set, each one column wide. Strings, as some ASCII marks take two
/// characters.
#[derive(Debug, PartialEq, Eq)]
pub struct GlyphSet {
    /// [`State`]s, in their declared order; see [`GlyphSet::state`].
    pub states: [&'static str; 9],
    /// A tree row with siblings below it.
    pub branch: &'static str,
    /// The last row of a tree level.
    pub last: &'static str,
    /// A tree level continuing past a deeper row.
    pub pipe: &'static str,
    /// A folded subtree.
    pub folded: &'static str,
    /// An unfolded subtree.
    pub unfolded: &'static str,
    /// The bar left of a user message, tool block, prompt or request.
    pub bar: &'static str,
    /// The end of the bar under a prompt.
    pub cap_end: &'static str,
    /// The cap under a prompt, after its end.
    pub cap_fill: &'static str,
    /// The text cursor in place of a terminal one.
    pub cursor: &'static str,
    /// The cursor on a list's selected row.
    pub pointer: &'static str,
    /// A collapsed section, toggled open.
    pub collapsed: &'static str,
    /// An expanded section.
    pub expanded: &'static str,
    /// Text cut short.
    pub ellipsis: &'static str,
    /// The used part of a usage bar.
    pub usage_full: &'static str,
    /// The rest of a usage bar.
    pub usage_empty: &'static str,
    /// A CI, review or merge check that passed.
    pub check_pass: &'static str,
    /// A check that failed.
    pub check_fail: &'static str,
    /// A check still running.
    pub check_pending: &'static str,
    /// No check.
    pub check_none: &'static str,
    /// A connected machine.
    pub connected: &'static str,
    /// A machine being connected.
    pub connecting: &'static str,
    /// A disconnected machine.
    pub disconnected: &'static str,
    /// Spinner frames, one each [`GlyphSet::spinner_ms`].
    pub spinner: &'static [&'static str],
    /// Milliseconds per spinner frame.
    pub spinner_ms: u64,
    /// Back, in a header.
    pub back: &'static str,
    /// The menu button.
    pub menu: &'static str,
    /// Collapse the sidebar.
    pub collapse: &'static str,
    /// Expand the sidebar.
    pub expand: &'static str,
    /// Between the parts of a row: `title · project · machine`.
    pub separator: &'static str,
    /// Tools, by [`GlyphSet::tool`]: shell, read, write, search, web, todo, task, other.
    pub tools: [&'static str; 8],
}

/// The Unicode set: one column wide on desktop terminals.
pub const UNICODE: GlyphSet = GlyphSet {
    states: ["◉", "✗", "✓", "●", "◌", "○", "▪", "→", "·"],
    branch: "├",
    last: "└",
    pipe: "│",
    folded: "▸",
    unfolded: "▾",
    bar: "┃",
    cap_end: "╹",
    cap_fill: "▀",
    cursor: "▌",
    pointer: "▶",
    collapsed: "▶",
    expanded: "▼",
    ellipsis: "…",
    usage_full: "█",
    usage_empty: "░",
    check_pass: "✓",
    check_fail: "✗",
    check_pending: "…",
    check_none: "–",
    connected: "●",
    connecting: "◌",
    disconnected: "✗",
    spinner: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
    spinner_ms: 80,
    back: "‹ back",
    menu: "≡ menu",
    collapse: "«",
    expand: "»",
    separator: " · ",
    // Not ⚙ for other tools: it has an emoji form, drawn two columns wide.
    tools: ["$", "→", "←", "✱", "◈", "☐", "◇", "◆"],
};

/// The ASCII set: what any terminal draws one column wide. Box drawing goes too: it is East
/// Asian Ambiguous, which some phone fonts draw two columns wide.
pub const ASCII: GlyphSet = GlyphSet {
    states: ["!", "x", "v", "*", "~", "o", "_", ">", "."],
    branch: "|",
    last: "`",
    pipe: "|",
    folded: "+",
    unfolded: "-",
    bar: "|",
    cap_end: "'",
    cap_fill: "-",
    cursor: "|",
    pointer: ">",
    collapsed: ">",
    expanded: "v",
    ellipsis: "...",
    usage_full: "#",
    usage_empty: "-",
    check_pass: "v",
    check_fail: "x",
    check_pending: ".",
    check_none: "-",
    connected: "*",
    connecting: "~",
    disconnected: "x",
    spinner: &["|", "/", "-", "\\"],
    spinner_ms: 120,
    back: "< back",
    menu: "= menu",
    collapse: "<<",
    expand: ">>",
    separator: " - ",
    tools: ["$", ">", "<", "*", "@", "[]", "+", ">"],
};

impl GlyphSet {
    /// The mark of `state`.
    pub fn state(&self, state: State) -> &'static str {
        self.states[state as usize]
    }

    /// The mark of the tool a vendor CLI calls `name`: Claude's names, Codex's, and herder's
    /// task tools; any other gets the generic mark.
    pub fn tool(&self, name: &str) -> &'static str {
        let at = match name {
            "Bash" | "shell" | "exec_command" => 0,
            "Read" | "NotebookRead" => 1,
            "Write" | "Edit" | "MultiEdit" | "NotebookEdit" | "apply_patch" => 2,
            "Grep" | "Glob" | "LS" => 3,
            "WebFetch" | "WebSearch" | "web_search" => 4,
            "TodoWrite" | "TodoRead" | "update_plan" => 5,
            name if name.starts_with("mcp__herder__") => 6,
            _ => 7,
        };
        self.tools[at]
    }

    /// The spinner's frame `elapsed_ms` into a turn.
    pub fn spinner_frame(&self, elapsed_ms: u64) -> &'static str {
        let frames = self.spinner.len() as u64;
        let at = (elapsed_ms / self.spinner_ms) % frames.max(1);
        self.spinner
            .get(usize::try_from(at).unwrap_or(0))
            .copied()
            .unwrap_or(" ")
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
    let ascii =
        match c {
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
            // Box drawing is East Asian Ambiguous too: lines become `-` and `|`, the rest `+`.
            '─' | '━' | '┄' | '┅' | '┈' | '┉' | '╌' | '╍' | '═' | '╴' | '╶' | '╸' | '╺' | '╼'
            | '╾' => '-',
            '│' | '┃' | '┆' | '┇' | '┊' | '┋' | '╎' | '╏' | '║' | '╵' | '╷' | '╹' | '╻' | '╽'
            | '╿' => '|',
            '\u{2500}'..='\u{257f}' => '+',
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
    fn ascii_folds_symbols_and_box_drawing() {
        let mut buffer = Buffer::with_lines(["┌ a · b … ● ◆ ▪ → ⚙ ✓ ✗ ¿ ą 日 ┐", "│ ├─┴ ═ ║ ┘"]);
        fold(&mut buffer);
        assert_eq!(
            buffer,
            Buffer::with_lines(["+ a - b . * + _ > > v x ? ą 日 +", "| +-+ - | +"])
        );
    }

    #[test]
    fn sets_keep_states_apart_and_one_column_wide() {
        use ratatui::text::Span;
        for set in [&UNICODE, &ASCII] {
            let mut states = set.states.to_vec();
            states.sort_unstable();
            states.dedup();
            assert_eq!(states.len(), set.states.len(), "{:?}", set.states);
            for mark in set.states.iter().chain(set.spinner).chain([
                &set.branch,
                &set.pointer,
                &set.bar,
                &set.cap_end,
                &set.cap_fill,
                &set.cursor,
                &set.connected,
            ]) {
                assert_eq!(Span::raw(*mark).width(), 1, "{mark:?}");
            }
        }
        // ASCII needs no fold.
        for mark in ASCII.states.iter().chain(ASCII.spinner).chain(&ASCII.tools) {
            assert!(mark.is_ascii(), "{mark:?}");
        }
        assert_eq!(UNICODE.state(State::NeedsYou), "◉");
        assert_eq!(ASCII.state(State::Unknown), ".");
        assert_eq!(UNICODE.tool("Bash"), "$");
        assert_eq!(ASCII.tool("mcp__herder__spawn"), "+");
        assert_eq!(UNICODE.tool("mcp__github__search"), "◆");
        assert_eq!(UNICODE.spinner_frame(0), "⠋");
        assert_eq!(UNICODE.spinner_frame(85), "⠙");
        assert_eq!(ASCII.spinner_frame(120 * 5), "/");
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
