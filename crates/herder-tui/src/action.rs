//! What the user can ask for, and the keys that ask for it.
//!
//! Every key goes through [`for_key`] to become an [`Action`], and only actions change the app
//! ([`crate::app::App::act`]). A new view adds its actions here and its keys to [`for_key`],
//! usually behind a check of what has focus. NAVIGATE's keys and the leader's are tables in
//! [`crate::keys`], which the help is drawn from too.
//!
//! Every action has a key a phone's on-screen keyboard has: a letter, a digit, Enter or
//! Backspace. Esc, Tab, arrows and Ctrl chords are alternatives, never the only way.
//!
//! Every view is usable with only the keys a phone SSH app's gestures send, too: arrows move
//! or scroll, PgUp / PgDn page, Home / End jump, Enter opens, Esc and Backspace go back, and on
//! a narrow screen Tab and Shift-Tab reach the action bar's buttons ([`crate::bar`]).

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::app::{App, Focus};
use crate::board::{self, BoardAction};
use crate::chat::ChatAct;
use crate::compose::{self, Act};
use crate::inbox::{self, InboxAction};
use crate::keys;
use crate::nav::Tab;
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
    /// Move through the open transcript; see [`crate::chat`].
    Chat(ChatAct),
    /// Something about pull requests.
    Pr(PrAction),
    /// Show the machines panel.
    OpenMachines,
    /// Open the add-machine dialog.
    AddMachine,
    /// Input to the machines panel or its add dialog.
    Machines(crate::machines::Input),
    /// Input to the machines panel's backup dialog.
    Backup(crate::backup::Input),
    /// Input to the fleet's secrets screen.
    Secrets(crate::secrets::Input),
    /// Open the selected session's terminal picker, or close it.
    Terminals,
    /// Something in the inbox.
    Inbox(InboxAction),
    /// Something on the board.
    Board(BoardAction),
    /// Fold or unfold the selected task's children.
    Fold,
    /// Show the accounts screen.
    OpenAccounts,
    /// Input to the accounts screen.
    Accounts(crate::account_screen::Input),
    /// Open the switch dialog of the open or selected session.
    OpenSwitch,
    /// Input to the switch dialog.
    Switch(crate::switch::Input),
    /// Group the session list by project or by machine.
    Group,
    /// Show or hide archived sessions in the session list.
    ToggleArchived,
    /// Open the fork dialog of the open or selected session.
    OpenFork,
    /// Input to the fork dialog.
    Fork(crate::fork::Input),
    /// Rename the selected or open session: the palette, with its `rename` line to edit.
    Rename,
    /// Ask AI to title the selected or open session again from its conversation so far.
    Retitle,
    /// Arm the leader: the next key is a NAVIGATE key, from any mode.
    Leader,
    /// Go to another session: the sidebar, or on a phone the switcher.
    GoTo,
    /// Back from the sidebar to the open session.
    Resume,
    /// Select the sidebar row one level up.
    Parent,
    /// Collapse the sidebar to a strip of state glyphs, or expand it.
    ToggleSidebar,
    /// Show or hide the details panel.
    ToggleDetails,
    /// Open the session of the attention list's row, from 0.
    Attention(usize),
    /// Show a tab of the open session.
    Tab(Tab),
    /// Input to the command palette.
    Palette(crate::palette::Input),
    /// Input to the new-session dialog.
    NewSession(crate::new_session::Input),
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
    if ctrl && key.code == KeyCode::Char('x') && !app.dialog_open() {
        return Some(Action::Leader);
    }
    if app.help {
        // Keys but the help's own do nothing.
        return keys::lookup(keys::HELP, key, app.focus);
    }
    if let Some(panel) = &app.machine_panel {
        return crate::machines::for_key(key, panel);
    }
    if let Some(screen) = &app.account_screen {
        return crate::account_screen::for_key(key, screen);
    }
    if let Some(switch) = &app.switch {
        return crate::switch::for_key(key, switch);
    }
    if app.fork.is_some() {
        return crate::fork::for_key(key);
    }
    if app.terminals.is_some() {
        return keys::lookup(keys::TERMINALS, key, app.focus);
    }
    let dialog = app.compose.palette.is_some() || app.compose.dialog.is_some();
    if ctrl && key.code == KeyCode::Char('p') && !dialog {
        return Some(Action::Compose(Act::Palette));
    }
    if let Some(action) = compose::for_key(key, app) {
        return action;
    }
    if let Some(action) = inbox::for_key(key, app) {
        return Some(Action::Inbox(action));
    }
    if let Some(action) = board::for_key(key, app) {
        return Some(Action::Board(action));
    }
    if let Some(action) = prs::for_key(key, app) {
        return Some(Action::Pr(action));
    }
    if app.prs.prompt.is_some() {
        // The link prompt takes every key.
        return None;
    }
    let in_transcript = app.focus == Focus::Transcript;
    match key.code {
        // On an item, Enter expands it, as `e` does.
        KeyCode::Enter if in_transcript && app.chat.cursor.is_some() => {
            return Some(Action::Chat(ChatAct::Toggle));
        }
        KeyCode::Char(digit @ '1'..='9') if in_transcript => {
            return Some(Action::Compose(Act::Choose(
                u32::from(digit) - u32::from('1'),
            )));
        }
        // In the sidebar, Esc returns to the open session; on a phone, so does ⌫, which
        // closes the switcher.
        KeyCode::Esc if app.focus == Focus::Sessions && app.open.is_some() => {
            return Some(Action::Resume);
        }
        KeyCode::Backspace
            if app.focus == Focus::Sessions
                && app.open.is_some()
                && app.width < crate::views::NARROW =>
        {
            return Some(Action::Resume);
        }
        _ => {}
    }
    keys::lookup(keys::NAVIGATE, key, app.focus)
}

/// The action `ctrl+x` then `key` asks for: what `key` does in NAVIGATE, plus `b` and `d`
/// for the sidebar and the details panel, and a digit for that attention row.
pub fn for_leader(key: KeyEvent) -> Option<Action> {
    if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
        return None;
    }
    if let KeyCode::Char(digit @ '1'..='9') = key.code {
        return Some(Action::Attention(usize::from(
            u8::try_from(digit).ok()? - b'1',
        )));
    }
    keys::lookup(keys::LEADER, key, Focus::Sessions)
}
