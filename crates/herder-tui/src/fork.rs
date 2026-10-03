//! Forking a session onto a host, and where a moved session went.
//!
//! Any top-level session can be forked: its history goes on in a new session on a host this
//! device is paired with as an owner, which reads the session from itself, or from its vault
//! when the session is another host's, whether that host is up or gone. The original stays as
//! it is. The fork dialog picks the host, sends it `fork_session`, then opens the new session
//! there. Without such a host it shows the command to run on one, `herder fork <session>`.
//! A vault's copy of a session whose host is offline offers the fork in place of its composer.
//!
//! A host that took over a session under its id, as the vault shows it, leaves the old copy
//! `moved`, read-only; a client paired with both sees that copy go to the new host.

use herder_client_core::{ConnectionState, Machine};
use herder_protocol::{CommandBody, FleetHost, Role, SessionId, SessionStatus};
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::action::Action;
use crate::app::{App, Effect, Focus, Row};
use crate::compose::Origin;
use crate::session::SessionKey;

/// The fork dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fork {
    /// The session to fork, as its machine lists it.
    pub session: SessionKey,
    /// Index of the chosen host in [`App::fork_targets`].
    pub selected: usize,
    /// Whether the fork was sent and its answer is awaited.
    pub sending: bool,
    /// Why the last fork failed.
    pub error: Option<String>,
}

/// Input to the dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// Close the dialog.
    Close,
    /// Fork the session onto the chosen host.
    Fork,
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
        KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('q' | 'F') => Input::Close,
        KeyCode::Enter => Input::Fork,
        KeyCode::Char('k') | KeyCode::Up => Input::Up,
        KeyCode::Char('j') | KeyCode::Down => Input::Down,
        KeyCode::Home | KeyCode::PageUp => Input::Top,
        KeyCode::End | KeyCode::PageDown => Input::Bottom,
        _ => return None,
    };
    Some(Action::Fork(input))
}

/// The command that forks `session` onto the host it is run on.
pub fn command(session: &SessionId) -> String {
    format!("herder fork {session}")
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

    /// The name of the host `key`'s session runs on: the vault's name for it, or its machine's.
    pub fn host_name(&self, key: &SessionKey) -> Option<String> {
        if let Some(host) = self.fleet_host(key) {
            return Some(host.host_name.clone());
        }
        self.machines
            .iter()
            .find(|m| m.host_id == key.host_id)
            .map(|machine| machine.name.clone())
    }

    /// Whether `key`'s session can be forked: it is loaded, and not a task's child.
    pub fn forkable(&self, key: &SessionKey) -> bool {
        self.sessions
            .get(key)
            .is_some_and(|session| session.loaded && session.parent.is_none())
    }

    /// The machines a session can be forked onto: hosts, not vaults, connected, with this
    /// device's user an owner there.
    pub fn fork_targets(&self) -> Vec<&Machine> {
        self.machines
            .iter()
            .filter(|machine| {
                machine.hosts.is_empty()
                    && machine.vault.is_none()
                    && machine.connection == ConnectionState::Connected
                    && machine.role == Some(Role::Owner)
            })
            .collect()
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
        self.host_name(copy.0)
    }

    /// Why `key`'s session takes no prompts here, in place of its composer, and whether that
    /// offers to fork it: its host is offline, or it moved to another host.
    pub fn read_only(&self, key: &SessionKey) -> Option<(String, bool)> {
        let session = self.sessions.get(key)?;
        if session.status == SessionStatus::Moved {
            let to = self
                .moved_to(key)
                .unwrap_or_else(|| "another host".to_owned());
            return Some((format!("moved to {to} · read-only here"), false));
        }
        let host = self.fleet_host(key).filter(|host| !host.online)?;
        if self.forkable(key) {
            return Some((format!("{} is offline · F fork", host.host_name), true));
        }
        Some((format!("{} is offline · read-only", host.host_name), false))
    }

    /// Opens the fork dialog for the open session, or the selected one in the session list, or
    /// says why it cannot be forked.
    pub(crate) fn open_fork(&mut self) {
        let key = match self.focus {
            Focus::Sessions => self.selected().as_ref().and_then(Row::session).cloned(),
            _ => self.open.clone(),
        };
        let Some(key) = key else {
            return;
        };
        if !self.forkable(&key) {
            self.notice = Some("a task's child cannot be forked; fork its primary".to_owned());
            return;
        }
        self.fork = Some(Fork {
            session: key,
            selected: 0,
            sending: false,
            error: None,
        });
    }

    /// Carries out one input to the fork dialog.
    pub(crate) fn fork_input(&mut self, input: Input) -> Vec<Effect> {
        let targets = self.fork_targets();
        let last = targets.len().saturating_sub(1);
        let Some(dialog) = &self.fork else {
            return Vec::new();
        };
        let target = targets
            .get(dialog.selected.min(last))
            .map(|machine| machine.host_id.clone());
        let Some(dialog) = &mut self.fork else {
            return Vec::new();
        };
        match input {
            Input::Close => self.fork = None,
            Input::Up => dialog.selected = dialog.selected.saturating_sub(1),
            Input::Down => dialog.selected = (dialog.selected + 1).min(last),
            Input::Top => dialog.selected = 0,
            Input::Bottom => dialog.selected = last,
            Input::Fork => {
                // Without a host to fork onto, the dialog shows the command to run on one.
                let Some(host_id) = target.filter(|_| !dialog.sending) else {
                    return Vec::new();
                };
                dialog.sending = true;
                dialog.error = None;
                return vec![Effect::Send {
                    host_id: host_id.clone(),
                    command: CommandBody::ForkSession {
                        session_id: dialog.session.session_id.clone(),
                        account_id: None,
                    },
                    origin: Origin::Fork(host_id),
                }];
            }
        }
        Vec::new()
    }

    /// The answer to a fork the dialog sent to `host_id`: opens the new session there once
    /// the host lists it, or shows why it failed.
    pub(crate) fn forked(
        &mut self,
        host_id: herder_protocol::HostId,
        result: Result<SessionId, String>,
    ) {
        match result {
            Ok(session_id) => {
                self.fork = None;
                self.compose.pending_open = Some(SessionKey {
                    host_id,
                    session_id,
                });
                self.open_pending();
            }
            Err(error) => {
                if let Some(dialog) = &mut self.fork {
                    dialog.sending = false;
                    dialog.error = Some(error);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use herder_protocol::{CommandResult, HostId};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::Msg;
    use crate::fake::{self, key};

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    /// Selects `id` of [`fake::vault`].
    fn select(app: &mut App, id: &str) {
        app.choose_row(Row::Session {
            key: key("v", id),
            depth: 0,
        });
    }

    /// [`fake::vault`], paired with `devbox` too, as an owner.
    fn with_devbox() -> (App, Vec<Machine>) {
        let mut app = fake::vault();
        let mut machines = app.machines.clone();
        machines.push(fake::machine("devbox", "devbox", &[]));
        app.update(Msg::Machines(machines.clone()));
        (app, machines)
    }

    #[test]
    fn any_session_forks_onto_a_paired_owner_host_and_opens_there() {
        let (mut app, mut machines) = with_devbox();
        // devbox is online: its session forks all the same.
        select(&mut app, "s1");
        press(&mut app, KeyCode::Char('F'));
        let targets: Vec<_> = app.fork_targets().iter().map(|m| m.name.clone()).collect();
        assert_eq!(targets, ["devbox"]);
        let sent = press(&mut app, KeyCode::Enter);
        assert_eq!(
            sent,
            [Effect::Send {
                host_id: HostId::new("devbox"),
                command: CommandBody::ForkSession {
                    session_id: SessionId::new("s1"),
                    account_id: None,
                },
                origin: Origin::Fork(HostId::new("devbox")),
            }]
        );
        // One at a time.
        assert!(press(&mut app, KeyCode::Enter).is_empty());

        // A refusal shows in the dialog, which may try again.
        app.update(Msg::Sent {
            origin: Origin::Fork(HostId::new("devbox")),
            result: Err("no account of claude here".into()),
        });
        let dialog = app.fork.as_ref().unwrap();
        assert_eq!(
            (dialog.sending, dialog.error.as_deref()),
            (false, Some("no account of claude here"))
        );
        assert_eq!(press(&mut app, KeyCode::Enter).len(), 1);

        // Forked: the new session opens on devbox once devbox lists it.
        app.update(Msg::Sent {
            origin: Origin::Fork(HostId::new("devbox")),
            result: Ok(CommandResult::SessionForked {
                session_id: SessionId::new("s9"),
                account_id: herder_protocol::AccountId::new("claude-main"),
                forked_from: SessionId::new("s1"),
                from_host_id: HostId::new("devbox"),
            }),
        });
        assert!(app.fork.is_none());
        let fork = key("devbox", "s9");
        assert_eq!(app.compose.pending_open.as_ref(), Some(&fork));
        machines[1] = fake::machine("devbox", "devbox", &["s9"]);
        app.update(Msg::Machines(machines));
        assert_eq!(app.open.as_ref(), Some(&fork));
    }

    #[test]
    fn without_a_paired_owner_host_the_dialog_shows_the_command() {
        let mut app = fake::vault();
        select(&mut app, "s2");
        press(&mut app, KeyCode::Char('F'));
        assert!(app.fork.is_some());
        // The vault itself takes no fork.
        assert!(app.fork_targets().is_empty());
        assert!(press(&mut app, KeyCode::Enter).is_empty());
        assert!(app.fork.is_some());
        press(&mut app, KeyCode::Esc);
        assert!(app.fork.is_none());

        let mut devbox = fake::machine("devbox", "devbox", &[]);
        devbox.role = Some(Role::Member);
        let mut machines = app.machines.clone();
        machines.push(devbox.clone());
        app.update(Msg::Machines(machines.clone()));
        assert!(app.fork_targets().is_empty());
        devbox.role = Some(Role::Owner);
        devbox.connection = ConnectionState::Connecting;
        machines[1] = devbox;
        app.update(Msg::Machines(machines));
        assert!(app.fork_targets().is_empty());
    }

    #[test]
    fn an_offline_hosts_session_offers_the_fork_in_place_of_its_composer() {
        let mut app = fake::vault();
        assert_eq!(
            app.read_only(&key("v", "s2")),
            Some(("laptop is offline · F fork".to_owned(), true))
        );
        assert_eq!(app.read_only(&key("v", "s1")), None);
        // Its composer is not offered: the vault takes no prompts for it.
        select(&mut app, "s2");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(app.focus, Focus::Transcript);
    }

    #[test]
    fn a_tasks_child_is_not_forkable() {
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
        assert!(!app.forkable(&key("v", "s2")));
        select(&mut app, "s2");
        press(&mut app, KeyCode::Char('F'));
        assert!(app.fork.is_none());
        assert_eq!(
            app.notice.as_deref(),
            Some("a task's child cannot be forked; fork its primary")
        );
        assert_eq!(
            app.read_only(&key("v", "s2")),
            Some(("laptop is offline · read-only".to_owned(), false))
        );
    }

    #[test]
    fn a_moved_session_names_the_host_it_went_to() {
        let mut app = fake::moved();
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
