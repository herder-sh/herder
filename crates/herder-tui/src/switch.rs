//! The switch dialog of a session: move it to another account of its machine, of its own
//! provider or another one, or change its model.
//!
//! An account of the session's provider takes over the conversation as it is; another
//! provider's replays the transcript from herder's log. The daemon applies a switch between
//! turns and journals it, so the transcript shows it once it happened.

use herder_protocol::{Account, CommandBody, SessionStatus};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::Action;
use crate::app::{App, Effect, Focus, Row};
use crate::compose::Origin;
use crate::session::{Session, SessionKey};

/// The switch dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Switch {
    /// The session to switch.
    pub session: SessionKey,
    /// Index of the chosen account in its machine's accounts.
    pub selected: usize,
    /// The model to switch to; empty keeps the current one, or takes the new provider's
    /// default.
    pub model: String,
    /// Whether keys type into the model.
    pub editing: bool,
    /// Why the last submit was refused.
    pub error: Option<String>,
}

/// What switching to an account does to a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// It is the session's account already: only the model can change.
    Current,
    /// Another account of the session's provider.
    Account,
    /// An account of another provider: the transcript is replayed.
    Provider,
}

/// What switching `session` to `account` does.
pub fn kind(session: &Session, account: &Account) -> Kind {
    if session.account_id.as_ref() == Some(&account.account_id) {
        Kind::Current
    } else if session.provider.as_ref() == Some(&account.provider) {
        Kind::Account
    } else {
        Kind::Provider
    }
}

/// Input to the dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// Close the dialog.
    Close,
    /// Choose the previous account.
    Up,
    /// Choose the next account.
    Down,
    /// Type into the model.
    EditModel,
    /// Stop typing into the model.
    Accounts,
    /// Switch.
    Submit,
    /// Type a character into the model.
    Char(char),
    /// Delete the model's last character.
    Backspace,
    /// Clear the model.
    Clear,
}

/// The action a key asks for while the dialog is open.
pub fn for_key(key: KeyEvent, switch: &Switch) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let input = if switch.editing {
        match key.code {
            KeyCode::Enter => Input::Submit,
            KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab | KeyCode::Up => Input::Accounts,
            KeyCode::Backspace if switch.model.is_empty() => Input::Accounts,
            KeyCode::Backspace => Input::Backspace,
            KeyCode::Char('u') if ctrl => Input::Clear,
            KeyCode::Char(c) if !ctrl => Input::Char(c),
            _ => return None,
        }
    } else {
        match key.code {
            KeyCode::Enter => Input::Submit,
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('q' | 's') => Input::Close,
            KeyCode::Char('k') | KeyCode::Up => Input::Up,
            KeyCode::Char('j') | KeyCode::Down => Input::Down,
            KeyCode::Char('m' | 'i') | KeyCode::Tab => Input::EditModel,
            _ => return None,
        }
    };
    Some(Action::Switch(input))
}

impl App {
    /// Opens the switch dialog for the open session, or the selected one in the session list.
    pub(crate) fn open_switch(&mut self) {
        let key = match self.focus {
            Focus::Sessions => self.selected().as_ref().and_then(Row::session).cloned(),
            _ => self.open.clone(),
        };
        let Some((key, session)) = key.and_then(|key| {
            let session = self.sessions.get(&key)?;
            Some((key, session))
        }) else {
            return;
        };
        if session.status == SessionStatus::Archived {
            self.notice = Some("an archived session cannot switch".to_owned());
            return;
        }
        let selected = self
            .accounts_of(&key)
            .iter()
            .position(|account| session.account_id.as_ref() == Some(&account.account_id))
            .unwrap_or(0);
        self.switch = Some(Switch {
            session: key,
            selected,
            model: String::new(),
            editing: false,
            error: None,
        });
    }

    /// The accounts of `key`'s machine.
    pub(crate) fn accounts_of(&self, key: &SessionKey) -> &[Account] {
        self.machines
            .iter()
            .find(|machine| machine.host_id == key.host_id)
            .map_or(&[], |machine| machine.accounts.as_slice())
    }

    /// Carries out one input to the switch dialog.
    pub(crate) fn switch_input(&mut self, input: Input) -> Vec<Effect> {
        let Some(switch) = &self.switch else {
            return Vec::new();
        };
        let last = self.accounts_of(&switch.session).len().saturating_sub(1);
        let Some(switch) = &mut self.switch else {
            return Vec::new();
        };
        switch.error = None;
        match input {
            Input::Close => self.switch = None,
            Input::Up => switch.selected = switch.selected.saturating_sub(1),
            Input::Down => switch.selected = (switch.selected + 1).min(last),
            Input::EditModel => switch.editing = true,
            Input::Accounts => switch.editing = false,
            Input::Char(c) => switch.model.push(c),
            Input::Backspace => {
                switch.model.pop();
            }
            Input::Clear => switch.model.clear(),
            Input::Submit => return self.switch_submit(),
        }
        Vec::new()
    }

    /// The commands the dialog asks for, or why there are none.
    fn switch_submit(&mut self) -> Vec<Effect> {
        let Some(switch) = self.switch.take() else {
            return Vec::new();
        };
        let key = switch.session.clone();
        let (Some(session), Some(account)) = (
            self.sessions.get(&key),
            self.accounts_of(&key).get(switch.selected),
        ) else {
            self.switch = Some(Switch {
                error: Some("this machine has no accounts".to_owned()),
                ..switch
            });
            return Vec::new();
        };
        let session_id = session.id.clone();
        let account_id = account.account_id.clone();
        let model = Some(switch.model.trim().to_owned()).filter(|model| !model.is_empty());
        let set_model = |model: String| CommandBody::SetModel {
            session_id: session_id.clone(),
            model,
        };
        let commands = match (kind(session, account), model) {
            (Kind::Current, Some(model)) if model != session.model => vec![set_model(model)],
            (Kind::Current, _) => {
                self.switch = Some(Switch {
                    error: Some("already on this account: enter a new model".to_owned()),
                    editing: true,
                    ..switch
                });
                return Vec::new();
            }
            (Kind::Account, model) => {
                let mut commands = vec![CommandBody::SwitchAccount {
                    session_id: session_id.clone(),
                    account_id,
                }];
                commands.extend(model.map(set_model));
                commands
            }
            (Kind::Provider, model) => vec![CommandBody::SwitchProvider {
                session_id: session_id.clone(),
                account_id,
                model,
            }],
        };
        commands
            .into_iter()
            .map(|command| Effect::Send {
                host_id: key.host_id.clone(),
                command,
                origin: Origin::Session(key.clone()),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use herder_protocol::{AccountId, EventBody, HostId, Provider, SessionId};

    use super::*;
    use crate::app::Msg;
    use crate::fake::{self, key, update};

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    /// [`fake::tree`] with `s2` open, on `claude-main`, beside `claude-work` and `codex`.
    fn app() -> App {
        let mut app = fake::tree();
        let mut machines = app.machines.clone();
        let mut codex = fake::account("codex", "Codex");
        codex.provider = Provider::Codex;
        machines[0].accounts = vec![
            fake::account("claude-main", "Main"),
            fake::account("claude-work", "Work"),
            codex,
        ];
        app.update(Msg::Machines(machines));
        press(&mut app, KeyCode::Enter);
        app
    }

    fn on_s2(command: CommandBody) -> Effect {
        Effect::Send {
            host_id: HostId::new("h1"),
            command,
            origin: Origin::Session(key("h1", "s2")),
        }
    }

    fn s2() -> SessionId {
        SessionId::new("s2")
    }

    #[test]
    fn s_switches_to_another_account_of_the_provider() {
        let mut app = app();
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.switch.as_ref().unwrap().selected, 0);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            [on_s2(CommandBody::SwitchAccount {
                session_id: s2(),
                account_id: AccountId::new("claude-work"),
            })]
        );
        assert_eq!(app.switch, None);
    }

    #[test]
    fn another_provider_replays_with_the_model_typed() {
        let mut app = app();
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('m'));
        fake::type_text(&mut app, "gpt-5");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            [on_s2(CommandBody::SwitchProvider {
                session_id: s2(),
                account_id: AccountId::new("codex"),
                model: Some("gpt-5".into()),
            })]
        );
    }

    #[test]
    fn the_current_account_changes_only_the_model() {
        let mut app = app();
        press(&mut app, KeyCode::Char('s'));
        // Nothing to switch to: the dialog asks for a model.
        assert_eq!(press(&mut app, KeyCode::Enter), []);
        let switch = app.switch.as_ref().unwrap();
        assert!(switch.editing);
        assert!(switch.error.is_some());
        fake::type_text(&mut app, "claude-sonnet");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            [on_s2(CommandBody::SetModel {
                session_id: s2(),
                model: "claude-sonnet".into(),
            })]
        );
    }

    #[test]
    fn backspace_leaves_the_model_then_closes() {
        let mut app = app();
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Char('m'));
        fake::type_text(&mut app, "jq");
        assert_eq!(app.switch.as_ref().unwrap().model, "jq");
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        assert!(!app.switch.as_ref().unwrap().editing);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.switch, None);
        assert_eq!(app.focus, Focus::Transcript);
    }

    #[test]
    fn the_session_list_switches_the_selected_session() {
        let mut app = app();
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('G'));
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.switch.as_ref().unwrap().session, key("h1", "s1"));
    }

    #[test]
    fn an_archived_session_cannot_switch() {
        let mut app = app();
        fake::feed(
            &mut app,
            "h1",
            "s2",
            update("s2", 3, vec![fake::status(SessionStatus::Archived)], vec![]),
        );
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.switch, None);
        assert_eq!(
            app.notice.as_deref(),
            Some("an archived session cannot switch")
        );
    }

    #[test]
    fn a_switch_moves_the_session_to_its_new_account() {
        let mut app = app();
        let mut switched = update(
            "s2",
            3,
            vec![EventBody::ProviderSwitched {
                provider: Provider::Codex,
                account_id: AccountId::new("codex"),
                model: "gpt-5".into(),
            }],
            vec![],
        );
        switched.events[0].by = Some(herder_protocol::UserId::new("ann"));
        fake::feed(&mut app, "h1", "s2", switched);
        let session = &app.sessions[&key("h1", "s2")];
        assert_eq!(session.provider, Some(Provider::Codex));
        assert_eq!(session.account_id, Some(AccountId::new("codex")));
        assert_eq!(session.model, "gpt-5");
        // The dialog now starts on the new account, and offers Claude by replay.
        press(&mut app, KeyCode::Char('s'));
        let switch = app.switch.as_ref().unwrap();
        assert_eq!(switch.selected, 2);
        let accounts = app.accounts_of(&switch.session);
        let session = &app.sessions[&key("h1", "s2")];
        assert_eq!(kind(session, &accounts[0]), Kind::Provider);
    }
}
