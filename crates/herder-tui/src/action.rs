//! What the user can ask for, and the keys that ask for it.
//!
//! Every key goes through [`for_key`] to become an [`Action`], and only actions change the app
//! ([`crate::app::App::act`]). A new view adds its actions here and its keys to [`for_key`],
//! usually behind a check of what has focus.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::app::{App, Focus};

/// Something the user asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Leave the TUI.
    Quit,
    /// Show or hide the key help.
    ToggleHelp,
    /// Select the previous row, or scroll the transcript up a line.
    Up,
    /// Select the next row, or scroll the transcript down a line.
    Down,
    /// Scroll the transcript up a page.
    PageUp,
    /// Scroll the transcript down a page.
    PageDown,
    /// Select the first row, or scroll to the transcript's start.
    Top,
    /// Select the last row, or scroll to the transcript's end and follow it.
    Bottom,
    /// Open the selected session in the main pane.
    Open,
    /// Move focus to the other pane.
    SwitchPane,
    /// Move focus back to the session list.
    Back,
    /// Reconnect every disconnected machine now.
    Reconnect,
}

/// The action a key asks for in the app's current state, if any.
pub fn for_key(key: KeyEvent, app: &App) -> Option<Action> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl && key.code == KeyCode::Char('c') {
        return Some(Action::Quit);
    }
    if app.help {
        // Any key closes the help.
        return Some(Action::ToggleHelp);
    }
    let action = match key.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('?') => Action::ToggleHelp,
        KeyCode::Char('k') | KeyCode::Up => Action::Up,
        KeyCode::Char('j') | KeyCode::Down => Action::Down,
        KeyCode::Char('u') if ctrl => Action::PageUp,
        KeyCode::Char('d') if ctrl => Action::PageDown,
        KeyCode::PageUp => Action::PageUp,
        KeyCode::PageDown | KeyCode::Char(' ') => Action::PageDown,
        KeyCode::Char('g') | KeyCode::Home => Action::Top,
        KeyCode::Char('G') | KeyCode::End => Action::Bottom,
        KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right if app.focus == Focus::Sessions => {
            Action::Open
        }
        KeyCode::Tab | KeyCode::BackTab => Action::SwitchPane,
        KeyCode::Esc | KeyCode::Char('h') | KeyCode::Left if app.focus == Focus::Transcript => {
            Action::Back
        }
        KeyCode::Char('r') => Action::Reconnect,
        _ => return None,
    };
    Some(action)
}

/// The keys [`for_key`] knows, for the help screen: key, then what it does.
pub const HELP: &[(&str, &str)] = &[
    ("j / k, ↓ / ↑", "move, or scroll the transcript"),
    ("Enter, l", "open the selected session"),
    ("Tab", "switch between sessions and transcript"),
    ("Esc, h", "back to the sessions"),
    ("PgUp / PgDn", "scroll a page (also Ctrl-u / Ctrl-d)"),
    ("g / G", "first / last; G follows the transcript"),
    ("r", "reconnect now"),
    ("?", "show or hide this help"),
    ("q, Ctrl-c", "quit"),
];
