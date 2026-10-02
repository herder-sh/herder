//! What the user can ask for, and the keys that ask for it.
//!
//! Every key goes through [`for_key`] to become an [`Action`], and only actions change the app
//! ([`crate::app::App::act`]). A new view adds its actions here and its keys to [`for_key`],
//! usually behind a check of what has focus.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use herder_protocol::ApprovalDecision;

use crate::app::{App, Focus};
use crate::compose::{self, Act};
use crate::prs::{self, PrAction};

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
    /// Write to, answer or control a session; see [`crate::compose`].
    Compose(Act),
    /// Something about pull requests.
    Pr(PrAction),
    /// Show the machines panel.
    OpenMachines,
    /// Open the add-machine dialog.
    AddMachine,
    /// Input to the machines panel or its add dialog.
    Machines(crate::machines::Input),
    /// Open the selected session's terminal picker, or close it.
    Terminals,
}

/// The action a key asks for in the app's current state, if any.
pub fn for_key(key: KeyEvent, app: &App) -> Option<Action> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl && key.code == KeyCode::Char('c') {
        return Some(Action::Compose(Act::CtrlC));
    }
    if app.help {
        // Any key closes the help.
        return Some(Action::ToggleHelp);
    }
    if let Some(panel) = &app.machine_panel {
        return crate::machines::for_key(key, panel);
    }
    if app.terminals.is_some() {
        let action = match key.code {
            KeyCode::Char('k') | KeyCode::Up => Action::Up,
            KeyCode::Char('j') | KeyCode::Down => Action::Down,
            KeyCode::Char('g') | KeyCode::Home => Action::Top,
            KeyCode::Char('G') | KeyCode::End => Action::Bottom,
            KeyCode::Enter => Action::Open,
            KeyCode::Esc | KeyCode::Char('q' | 't') => Action::Back,
            _ => return None,
        };
        return Some(action);
    }
    if let Some(action) = compose::for_key(key, app) {
        return action;
    }
    if let Some(action) = prs::for_key(key, app) {
        return Some(Action::Pr(action));
    }
    if app.prs.prompt.is_some() {
        // The link prompt takes every key.
        return None;
    }
    let in_transcript = app.focus == Focus::Transcript;
    let action = match key.code {
        KeyCode::Char('i') | KeyCode::Enter if in_transcript => Action::Compose(Act::Write),
        KeyCode::Char('y') if in_transcript => {
            Action::Compose(Act::Approve(ApprovalDecision::Allow))
        }
        KeyCode::Char('n') if in_transcript => {
            Action::Compose(Act::Approve(ApprovalDecision::Deny))
        }
        KeyCode::Char(digit @ '1'..='9') if in_transcript => {
            Action::Compose(Act::Choose(u32::from(digit) - u32::from('1')))
        }
        KeyCode::Char(':') => Action::Compose(Act::Palette),
        KeyCode::Char('n') => Action::Compose(Act::NewSession),
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
        KeyCode::Char('m') => Action::OpenMachines,
        KeyCode::Char('a') => Action::AddMachine,
        KeyCode::Char('t') => Action::Terminals,
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
    ("i, Enter", "write in the open session"),
    ("Enter / Alt-Enter", "send / new line, in the composer"),
    ("Esc", "leave the composer or close a dialog"),
    ("y / n", "allow / deny the pending approval"),
    ("1-9", "pick an answer to the pending question"),
    ("Ctrl-c", "interrupt the running turn"),
    (":", "commands: model, mode, archive, new"),
    ("n", "new session"),
    ("p", "focus the session's pull requests"),
    ("P", "every session's pull requests"),
    ("Enter, o", "open the selected pull request in the browser"),
    ("L", "link a pull request by number or URL"),
    ("x", "unlink the selected pull request"),
    ("r", "reconnect now"),
    ("m", "machines: connections and fingerprints"),
    ("a", "add a machine (or paste its link)"),
    ("t", "terminals of the selected session (owners)"),
    ("Ctrl-] d", "detach from an attached terminal"),
    ("?", "show or hide this help"),
    ("q, Ctrl-c twice", "quit"),
];
