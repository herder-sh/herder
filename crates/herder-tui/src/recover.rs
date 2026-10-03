//! Recovering a session whose host is offline, and where a moved session went.
//!
//! A vault lists every host's sessions, with which hosts are online. A session of an offline
//! host can be taken over by another host of the vault: `herder recover <session>` run on
//! that host, through its daemon's local control socket. The TUI cannot run it there, so the
//! recover dialog picks the host and shows the command to run on it. The session keeps its
//! id; the vault then lists it under its new host. Its old host, back online, makes its own
//! copy `moved`, read-only, and a client paired with it sees that copy go to the new host.

use herder_protocol::{FleetHost, SessionId, SessionStatus};
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::action::Action;
use crate::app::{App, Focus, Row};
use crate::session::SessionKey;

/// The recover dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recover {
    /// The session to recover, as its vault lists it.
    pub session: SessionKey,
    /// Index of the chosen host in [`App::recover_targets`].
    pub selected: usize,
}

/// Input to the dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// Close the dialog.
    Close,
    /// Choose the previous host.
    Up,
    /// Choose the next host.
    Down,
    /// Choose the first host.
    Top,
    /// Choose the last host.
    Bottom,
}

/// The action a key asks for while the dialog is open.
pub fn for_key(key: KeyEvent) -> Option<Action> {
    let input = match key.code {
        KeyCode::Esc | KeyCode::Backspace | KeyCode::Enter | KeyCode::Char('q' | 'R') => {
            Input::Close
        }
        KeyCode::Char('k') | KeyCode::Up => Input::Up,
        KeyCode::Char('j') | KeyCode::Down => Input::Down,
        KeyCode::Home | KeyCode::PageUp => Input::Top,
        KeyCode::End | KeyCode::PageDown => Input::Bottom,
        _ => return None,
    };
    Some(Action::Recover(input))
}

/// The command that recovers `session` on the host it is run on.
pub fn command(session: &SessionId) -> String {
    format!("herder recover {session}")
}

impl App {
    /// The host `key`'s session runs on, when a vault lists it with its host.
    pub fn fleet_host(&self, key: &SessionKey) -> Option<&FleetHost> {
        let machine = self.machines.iter().find(|m| m.host_id == key.host_id)?;
        let host = machine
            .sessions
            .iter()
            .find(|head| head.session_id == key.session_id)?
            .host_id
            .as_ref()?;
        machine.hosts.iter().find(|h| h.host_id == *host)
    }

    /// The offline host of `key`'s session, when the session can be recovered from it: a
    /// vault lists it on that host, and it is a top-level session that is not archived.
    /// Recovery does not take over task children.
    pub fn recoverable(&self, key: &SessionKey) -> Option<&FleetHost> {
        let host = self.fleet_host(key).filter(|host| !host.online)?;
        let session = self.sessions.get(key)?;
        let ended = matches!(
            session.status,
            SessionStatus::Archived | SessionStatus::Moved
        );
        (session.loaded && !ended && session.parent.is_none()).then_some(host)
    }

    /// The hosts that can take over `key`'s session: the online hosts of its vault.
    pub fn recover_targets(&self, key: &SessionKey) -> Vec<&FleetHost> {
        self.machines
            .iter()
            .find(|m| m.host_id == key.host_id)
            .map(|machine| machine.hosts.iter().filter(|h| h.online).collect())
            .unwrap_or_default()
    }

    /// Where `key`'s session went, when it is `moved`: the host of another listed copy of it
    /// that is not moved, by name, if one is listed.
    pub fn moved_to(&self, key: &SessionKey) -> Option<String> {
        if self.sessions.get(key)?.status != SessionStatus::Moved {
            return None;
        }
        let copy = self.sessions.iter().find(|(other, session)| {
            *other != key
                && other.session_id == key.session_id
                && session.loaded
                && session.status != SessionStatus::Moved
        })?;
        let copy = copy.0;
        if let Some(host) = self.fleet_host(copy) {
            return Some(host.host_name.clone());
        }
        self.machines
            .iter()
            .find(|m| m.host_id == copy.host_id)
            .map(|machine| machine.name.clone())
    }

    /// Why `key`'s session takes no prompts here, in place of its composer, and whether it
    /// can be recovered: its host is offline, or it moved to another host.
    pub fn read_only(&self, key: &SessionKey) -> Option<(String, bool)> {
        let session = self.sessions.get(key)?;
        if session.status == SessionStatus::Moved {
            let to = self
                .moved_to(key)
                .unwrap_or_else(|| "another host".to_owned());
            return Some((format!("moved to {to} · read-only here"), false));
        }
        if let Some(host) = self.recoverable(key) {
            return Some((format!("{} is offline · R recover", host.host_name), true));
        }
        let host = self.fleet_host(key).filter(|host| !host.online)?;
        Some((format!("{} is offline · read-only", host.host_name), false))
    }

    /// Opens the recover dialog for the open session, or the selected one in the session list,
    /// or says why it cannot be recovered.
    pub(crate) fn open_recover(&mut self) {
        let key = match self.focus {
            Focus::Sessions => self.selected().as_ref().and_then(Row::session).cloned(),
            _ => self.open.clone(),
        };
        let Some(key) = key else {
            return;
        };
        if self.recoverable(&key).is_some() {
            self.recover = Some(Recover {
                session: key,
                selected: 0,
            });
            return;
        }
        let notice = match (self.fleet_host(&key), self.sessions.get(&key)) {
            (None, _) => "only a vault's sessions of an offline host can be recovered",
            (Some(host), _) if host.online => "its host is online: drive it there",
            (_, Some(session)) if session.parent.is_some() => {
                "a task's child cannot be recovered; recover its primary"
            }
            _ => "an archived or moved session cannot be recovered",
        };
        self.notice = Some(notice.to_owned());
    }

    /// Carries out one input to the recover dialog.
    pub(crate) fn recover_input(&mut self, input: Input) {
        let Some(dialog) = &self.recover else {
            return;
        };
        let last = self
            .recover_targets(&dialog.session)
            .len()
            .saturating_sub(1);
        let Some(dialog) = &mut self.recover else {
            return;
        };
        match input {
            Input::Close => self.recover = None,
            Input::Up => dialog.selected = dialog.selected.saturating_sub(1),
            Input::Down => dialog.selected = (dialog.selected + 1).min(last),
            Input::Top => dialog.selected = 0,
            Input::Bottom => dialog.selected = last,
        }
    }
}

#[cfg(test)]
mod tests {
    use herder_protocol::HostId;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::Msg;
    use crate::fake::{self, key};

    fn press(app: &mut App, code: KeyCode) {
        app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    /// Selects `id` of [`fake::vault`].
    fn select(app: &mut App, id: &str) {
        app.choose_row(Row::Session {
            key: key("v", id),
            depth: 0,
        });
    }

    #[test]
    fn only_a_session_of_an_offline_host_is_recoverable() {
        let mut app = fake::vault();
        let laptop = app.recoverable(&key("v", "s2")).map(|h| h.host_id.clone());
        assert_eq!(laptop, Some(HostId::new("laptop")));
        assert!(app.recoverable(&key("v", "s1")).is_none());

        select(&mut app, "s1");
        press(&mut app, KeyCode::Char('R'));
        assert!(app.recover.is_none());
        assert_eq!(
            app.notice.as_deref(),
            Some("its host is online: drive it there")
        );

        select(&mut app, "s2");
        press(&mut app, KeyCode::Char('R'));
        assert_eq!(
            app.recover,
            Some(Recover {
                session: key("v", "s2"),
                selected: 0,
            })
        );
        let targets: Vec<_> = app
            .recover_targets(&key("v", "s2"))
            .iter()
            .map(|h| h.host_name.clone())
            .collect();
        assert_eq!(targets, ["devbox"]);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.recover.as_ref().map(|r| r.selected), Some(0));
        press(&mut app, KeyCode::Esc);
        assert!(app.recover.is_none());

        // Its composer is not offered: the vault takes no prompts for it.
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(app.focus, Focus::Transcript);
        let read_only = app.read_only(&key("v", "s2"));
        assert_eq!(
            read_only,
            Some(("laptop is offline · R recover".to_owned(), true))
        );
    }

    #[test]
    fn an_archived_session_or_a_child_is_not_recoverable() {
        let mut app = fake::vault();
        fake::feed(
            &mut app,
            "v",
            "s2",
            fake::update("s2", 3, vec![fake::status(SessionStatus::Archived)], vec![]),
        );
        assert!(app.recoverable(&key("v", "s2")).is_none());

        let mut app = fake::vault();
        fake::feed(
            &mut app,
            "v",
            "s2",
            fake::update(
                "s2",
                3,
                vec![fake::created("b", Some("s9"), Some("t"))],
                vec![],
            ),
        );
        assert!(app.recoverable(&key("v", "s2")).is_none());
    }

    #[test]
    fn a_moved_session_names_the_host_it_went_to() {
        let mut app = fake::recovered();
        assert_eq!(
            app.moved_to(&key("laptop", "s2")).as_deref(),
            Some("devbox")
        );
        // The live copy has not moved.
        assert_eq!(app.moved_to(&key("v", "s2")), None);
        // Without a live copy listed, where it went is not known.
        let machines = app.machines[1..].to_vec();
        app.update(Msg::Machines(machines));
        assert_eq!(app.moved_to(&key("laptop", "s2")), None);
    }
}
