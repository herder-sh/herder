//! Taps and swipes: phone SSH apps send a tap as a mouse click and a swipe as wheel events,
//! once the TUI asks for mouse reporting.
//!
//! Every draw records where the tappable and scrollable things of the frame are, as [`Hits`].
//! A click or a wheel event is matched against the last frame's hits, topmost first, and does
//! what the matching key would: a button names its key or action, a row selects itself, and a
//! tap on the selected row opens it. With `:mouse off` the TUI stops asking, so the terminal
//! selects text again; Shift-drag (Option-drag on macOS) selects text either way.

use std::path::Path;

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};

use crate::action::{self, Action};
use crate::app::{App, Effect, Focus};
use crate::inbox::InboxAction;
use crate::prs::PrAction;
use crate::switch;

/// Lines a wheel step scrolls the transcript.
const WHEEL_LINES: isize = 3;

/// The client profile's file of TUI settings, next to its machines.
const SETTINGS: &str = "tui.json";

/// What a tap on a spot does.
#[derive(Clone, Debug, PartialEq)]
pub enum Click {
    /// Nothing: covers what a dialog hides.
    Nothing,
    /// What this key does.
    Key(KeyEvent),
    /// This action.
    Act(Action),
    /// Show the open session's transcript.
    Open,
    /// A row of a list, by index; see [`List`].
    Row(List, usize),
}

/// A list whose rows a tap selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum List {
    /// The session list, by [`App::rows`] index; a tap opens a session.
    Sessions,
    /// The inbox, by [`App::waiting`] index; a second tap opens the request's session.
    Inbox,
    /// The open session's PR strip; a second tap opens the PR.
    Strip,
    /// Every session's PRs, by [`App::all_prs`] index; a second tap opens the PR.
    AllPrs,
    /// The terminal picker; a second tap attaches.
    Picker,
    /// The accounts screen's rows.
    Accounts,
    /// The machines panel's machines.
    Machines,
    /// The switch dialog's accounts; a second tap switches.
    Switch,
}

/// What a wheel step over a spot scrolls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wheel {
    /// Moves the session list's selection.
    Sessions,
    /// Scrolls the open transcript.
    Transcript,
    /// Moves the PR strip's selection.
    Strip,
    /// Does what ↑ and ↓ do: for the view or dialog that has the keys.
    Keys,
}

/// The last frame's tappable and scrollable spots, in drawing order.
#[derive(Clone, Debug, Default)]
pub struct Hits {
    clicks: Vec<(Rect, Click)>,
    wheels: Vec<(Rect, Wheel)>,
}

impl Hits {
    /// Makes a tap in `area` do `click`, over what was recorded before.
    pub fn click(&mut self, area: Rect, click: Click) {
        self.clicks.push((area, click));
    }

    /// Makes the wheel in `area` scroll `wheel`, over what was recorded before.
    pub fn wheel(&mut self, area: Rect, wheel: Wheel) {
        self.wheels.push((area, wheel));
    }

    /// Covers `area` with a dialog: taps and the wheel there only reach what is recorded
    /// after.
    pub fn cover(&mut self, area: Rect) {
        self.click(area, Click::Nothing);
        self.wheel(area, Wheel::Keys);
    }

    /// Records a tap on each shown row of a list drawn in `area`, inside its borders: rows
    /// from `offset`, the first shown, each as tall as `heights` says. `row` names the row of
    /// an item, or `None` for an item that is not one, such as a heading.
    pub fn list(
        &mut self,
        area: Rect,
        offset: usize,
        heights: &[usize],
        row: impl Fn(usize) -> Option<Click>,
    ) {
        let mut y = area.y;
        for (at, height) in heights.iter().enumerate().skip(offset) {
            if y >= area.bottom() {
                break;
            }
            let height = u16::try_from(*height)
                .unwrap_or(u16::MAX)
                .min(area.bottom() - y);
            if let Some(click) = row(at) {
                self.click(Rect::new(area.x, y, area.width, height), click);
            }
            y += height;
        }
    }

    /// Puts `top`'s spots over these.
    pub fn append(&mut self, mut top: Hits) {
        self.clicks.append(&mut top.clicks);
        self.wheels.append(&mut top.wheels);
    }

    /// What a tap at `x`, `y` does: the topmost spot there.
    pub fn click_at(&self, x: u16, y: u16) -> Option<&Click> {
        let at = Position::new(x, y);
        self.clicks
            .iter()
            .rev()
            .find(|(area, _)| area.contains(at))
            .map(|(_, click)| click)
    }

    /// What the wheel at `x`, `y` scrolls: the topmost spot there.
    pub fn wheel_at(&self, x: u16, y: u16) -> Option<Wheel> {
        let at = Position::new(x, y);
        self.wheels
            .iter()
            .rev()
            .find(|(area, _)| area.contains(at))
            .map(|(_, wheel)| *wheel)
    }
}

/// A key without modifiers, as a button presses it.
pub fn key(code: KeyCode) -> Click {
    Click::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// Whether the profile in `config_dir` asks for mouse reporting: yes unless `:mouse off` was
/// saved.
pub fn load(config_dir: &Path) -> bool {
    std::fs::read(config_dir.join(SETTINGS))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|settings| settings.get("mouse")?.as_bool())
        .unwrap_or(true)
}

/// Saves whether to ask for mouse reporting in the profile in `config_dir`.
pub fn save(config_dir: &Path, on: bool) -> std::io::Result<()> {
    let settings = serde_json::json!({ "mouse": on });
    std::fs::write(config_dir.join(SETTINGS), format!("{settings}\n"))
}

/// Turns the terminal's mouse reporting on or off: clicks with their release, and the wheel,
/// in SGR encoding. Motion is not asked for, so a phone over SSH is not sent a report for
/// every move.
pub fn report(on: bool) -> std::io::Result<()> {
    use std::io::Write;
    let codes: &[u8] = if on {
        b"\x1b[?1000h\x1b[?1006h"
    } else {
        b"\x1b[?1006l\x1b[?1000l"
    };
    let mut out = std::io::stdout();
    out.write_all(codes)?;
    out.flush()
}

impl App {
    /// Folds in a mouse event: a tap where it lands, a wheel step where the pointer is.
    pub(crate) fn on_mouse(&mut self, event: MouseEvent) -> Vec<Effect> {
        if !self.mouse {
            return Vec::new();
        }
        let (x, y) = (event.column, event.row);
        match event.kind {
            MouseEventKind::ScrollUp => self.wheel(x, y, -1),
            MouseEventKind::ScrollDown => self.wheel(x, y, 1),
            // A tap is a press and a release on the same spot; a press that slides off is
            // dropped.
            MouseEventKind::Down(MouseButton::Left) => {
                self.pressed = self.hits.click_at(x, y).cloned();
                Vec::new()
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let pressed = self.pressed.take();
                match self.hits.click_at(x, y).cloned() {
                    Some(click) if pressed.as_ref() == Some(&click) => {
                        self.notice = None;
                        self.click(click)
                    }
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    fn wheel(&mut self, x: u16, y: u16, step: isize) -> Vec<Effect> {
        match self.hits.wheel_at(x, y) {
            Some(Wheel::Sessions) => self.select_by(step),
            Some(Wheel::Transcript) => self.scroll.by(step * WHEEL_LINES),
            Some(Wheel::Strip) => {
                let last = self.strip_prs().len().saturating_sub(1);
                self.prs.strip = self
                    .prs
                    .strip
                    .min(last)
                    .saturating_add_signed(step)
                    .min(last);
            }
            Some(Wheel::Keys) => {
                let code = if step < 0 { KeyCode::Up } else { KeyCode::Down };
                if let Some(action) = action::for_key(KeyEvent::new(code, KeyModifiers::NONE), self)
                {
                    return self.act(action);
                }
            }
            None => {}
        }
        Vec::new()
    }

    fn click(&mut self, click: Click) -> Vec<Effect> {
        match click {
            Click::Nothing => Vec::new(),
            Click::Key(key) => match action::for_key(key, self) {
                Some(action) => self.act(action),
                None => Vec::new(),
            },
            Click::Act(action) => self.act(action),
            Click::Open => {
                if self.open.is_some() {
                    self.inbox.answer = None;
                    self.focus = Focus::Transcript;
                }
                Vec::new()
            }
            Click::Row(list, at) => self.tap_row(list, at),
        }
    }

    /// Selects row `at` of `list`; on the selected row, opens it.
    fn tap_row(&mut self, list: List, at: usize) -> Vec<Effect> {
        match list {
            List::Sessions => {
                let Some(row) = self.rows().into_iter().nth(at) else {
                    return Vec::new();
                };
                let session = row.session().is_some();
                self.choose_row(row);
                if session {
                    return self.act(Action::Open);
                }
                self.focus = Focus::Sessions;
            }
            List::Inbox => {
                if self.inbox_index(&self.waiting()) == at {
                    return self.act_inbox(InboxAction::OpenSession);
                }
                self.select_request(at);
            }
            List::Strip => {
                if self.focus == Focus::Prs && self.pr_index() == at {
                    return self.act_pr(PrAction::Browse);
                }
                self.focus = Focus::Prs;
                self.prs.strip = at;
            }
            List::AllPrs => {
                if self.pr_index() == at {
                    return self.act_pr(PrAction::Browse);
                }
                self.prs.all = at;
            }
            List::Picker => {
                if let Some(picker) = &mut self.terminals {
                    if picker.selected == at {
                        return self.act(Action::Open);
                    }
                    picker.selected = at;
                }
            }
            List::Accounts => {
                let rows = crate::account_screen::rows(&self.machines);
                if let (Some(screen), Some(row)) = (&mut self.account_screen, rows.get(at)) {
                    screen.chosen = Some(row.clone());
                }
            }
            List::Machines => {
                let host_id = self.machines.get(at).map(|machine| machine.host_id.clone());
                if let (Some(panel), Some(host_id)) = (&mut self.machine_panel, host_id) {
                    panel.chosen = Some(host_id);
                }
            }
            List::Switch => {
                if let Some(dialog) = &mut self.switch {
                    if dialog.selected == at && !dialog.editing {
                        return self.switch_input(switch::Input::Submit);
                    }
                    dialog.selected = at;
                    dialog.editing = false;
                }
            }
        }
        Vec::new()
    }

    /// Whether a dialog or overlay has the keys.
    pub fn dialog_open(&self) -> bool {
        self.help
            || self.machine_panel.is_some()
            || self.account_screen.is_some()
            || self.switch.is_some()
            || self.terminals.is_some()
            || self.compose.dialog.is_some()
            || self.compose.palette.is_some()
            || self.prs.prompt.is_some()
    }
}

#[cfg(test)]
mod tests;
