//! The narrow layout's action bar, driven by keys: phone SSH apps such as Termius send their
//! gestures as Tab, Shift-Tab and arrows, never as mouse events. On a narrow screen Tab and
//! Shift-Tab move a focus across the bar's buttons, wrapping at either end, and Enter presses
//! the focused one, so approving is Tab, Enter. Any other key drops the focus and does what it
//! always does.
//!
//! Forms whose fields Tab already moves between keep Tab; their buttons are Enter and Esc.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

use crate::app::{App, Effect};

impl App {
    /// Folds in `key` if it drives the action bar; `None` hands it on.
    pub(crate) fn bar_key(&mut self, key: KeyEvent) -> Option<Vec<Effect>> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        let last = self.bar.len().checked_sub(1);
        match key.code {
            KeyCode::Tab | KeyCode::BackTab if !self.tab_moves_fields() => {
                let last = last?;
                self.bar_focus = Some(match (self.bar_focus, key.code) {
                    (Some(at), KeyCode::Tab) if at < last => at + 1,
                    (_, KeyCode::Tab) => 0,
                    (Some(at), _) if at > 0 => at - 1,
                    _ => last,
                });
                Some(Vec::new())
            }
            KeyCode::Enter if key.modifiers.is_empty() && self.bar_focus.is_some() => {
                let click = self
                    .bar_focus
                    .take()
                    .and_then(|at| self.bar.get(at))?
                    .clone();
                Some(self.click(click))
            }
            _ => {
                self.bar_focus = None;
                None
            }
        }
    }

    /// Whether a form is open whose fields Tab moves between.
    fn tab_moves_fields(&self) -> bool {
        self.compose.dialog.is_some()
            || self.switch.is_some()
            || self
                .machine_panel
                .as_ref()
                .is_some_and(|panel| panel.add.is_some() || panel.account.is_some())
            || self
                .account_screen
                .as_ref()
                .is_some_and(|screen| screen.adding.is_some())
    }
}

#[cfg(test)]
mod tests;
