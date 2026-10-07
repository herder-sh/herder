//! The switch picker of a session: move it to another account of its machine, of its own
//! provider or another one, or change its model (OpenCode's model picker, reshaped around
//! herder's switches).
//!
//! An account of the session's provider takes over the conversation as it is; another
//! provider's replays the transcript from herder's log, so the picker lists them apart. The
//! search filters the accounts as it is typed; Tab moves to the model, which is typed, with
//! the models the machine's sessions use offered. The daemon applies a switch between turns
//! and journals it, so the transcript shows it once it happened.

use herder_protocol::{Account, CommandBody, SessionStatus};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::Action;
use crate::app::{App, Effect, Focus, Row};
use crate::compose::Origin;
use crate::session::{Session, SessionKey};

/// The switch picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Switch {
    /// The session to switch.
    pub session: SessionKey,
    /// The search over the accounts.
    pub search: String,
    /// The cursor, by index into [`App::switch_rows`].
    pub selected: usize,
    /// The list's scroll, kept between draws.
    pub offset: usize,
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

/// Input to the picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// Close the picker.
    Close,
    /// The cursor to the previous account.
    Up,
    /// The cursor to the next account.
    Down,
    /// The cursor to the first account.
    Top,
    /// The cursor to the last account.
    Bottom,
    /// Type into the model.
    EditModel,
    /// Stop typing into the model.
    Accounts,
    /// Switch.
    Submit,
    /// Type a character into the search or the model.
    Char(char),
    /// Delete the last character of the search or the model.
    Backspace,
    /// Clear the search or the model.
    Clear,
    /// Take the model the chosen account's machine uses: by index into
    /// [`App::recent_models`].
    Recent(usize),
}

/// The action a key asks for while the picker is open.
pub fn for_key(key: KeyEvent, switch: &Switch) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let input = match key.code {
        KeyCode::Enter => Input::Submit,
        KeyCode::Esc if switch.editing => Input::Accounts,
        KeyCode::Esc => Input::Close,
        KeyCode::Tab | KeyCode::BackTab if switch.editing => Input::Accounts,
        KeyCode::Tab | KeyCode::BackTab => Input::EditModel,
        KeyCode::Backspace if switch.editing && switch.model.is_empty() => Input::Accounts,
        KeyCode::Backspace if !switch.editing && switch.search.is_empty() => Input::Close,
        KeyCode::Backspace => Input::Backspace,
        KeyCode::Char('u') if ctrl => Input::Clear,
        KeyCode::Up if switch.editing => Input::Accounts,
        KeyCode::Up => Input::Up,
        KeyCode::Down => Input::Down,
        KeyCode::Char('p' | 'k') if ctrl => Input::Up,
        KeyCode::Char('n' | 'j') if ctrl => Input::Down,
        KeyCode::Home | KeyCode::PageUp => Input::Top,
        KeyCode::End | KeyCode::PageDown => Input::Bottom,
        KeyCode::Char(c) if !ctrl => Input::Char(c),
        _ => return None,
    };
    Some(Action::Switch(input))
}

impl App {
    /// Opens the switch picker for the open session, or the selected one in the session list.
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
        self.switch = Some(Switch {
            session: key,
            search: String::new(),
            selected: 0,
            offset: 0,
            model: String::new(),
            editing: false,
            error: None,
        });
        // The cursor starts on the session's account.
        let accounts = self.switch.as_ref().map(|s| self.accounts_of(&s.session));
        let current = self.switch_rows().iter().position(|at| {
            accounts
                .and_then(|accounts| accounts.get(*at))
                .is_some_and(|account| self.open_switch_kind(account) == Some(Kind::Current))
        });
        if let Some(switch) = &mut self.switch {
            switch.selected = current.unwrap_or(0);
        }
    }

    /// What switching the picker's session to `account` does.
    fn open_switch_kind(&self, account: &Account) -> Option<Kind> {
        let switch = self.switch.as_ref()?;
        let session = self.sessions.get(&switch.session)?;
        Some(kind(session, account))
    }

    /// The accounts of `key`'s machine.
    pub(crate) fn accounts_of(&self, key: &SessionKey) -> &[Account] {
        self.machines
            .iter()
            .find(|machine| machine.host_id == key.host_id)
            .map_or(&[], |machine| machine.accounts.as_slice())
    }

    /// The accounts the picker lists, by index into its machine's: those its search keeps,
    /// the session's provider's first, the session's own leading them.
    pub fn switch_rows(&self) -> Vec<usize> {
        let Some(switch) = &self.switch else {
            return Vec::new();
        };
        let Some(session) = self.sessions.get(&switch.session) else {
            return Vec::new();
        };
        let accounts = self.accounts_of(&switch.session);
        let mut kept = crate::fuzzy::filter(&switch.search, accounts, |account| {
            format!(
                "{} {} {}",
                account.label,
                account.account_id.as_str(),
                account.provider.as_str()
            )
        });
        // With no search, by kind; with one, best match first within each group.
        let group = |at: &usize| match kind(session, &accounts[*at]) {
            Kind::Current if switch.search.is_empty() => 0,
            Kind::Current | Kind::Account => 1,
            Kind::Provider => 2,
        };
        kept.sort_by_key(group);
        kept
    }

    /// The models to offer for `account`: those the sessions of its machine on its provider
    /// use, the newest session's first.
    pub fn recent_models(&self, key: &SessionKey, account: &Account) -> Vec<String> {
        let mut sessions: Vec<(&SessionKey, &Session)> = self
            .sessions
            .iter()
            .filter(|(other, session)| {
                other.host_id == key.host_id
                    && session.provider.as_ref() == Some(&account.provider)
                    && !session.model.is_empty()
            })
            .collect();
        sessions.sort_by(|a, b| b.0.session_id.cmp(&a.0.session_id));
        let mut models: Vec<String> = Vec::new();
        for (_, session) in sessions {
            if !models.contains(&session.model) {
                models.push(session.model.clone());
            }
        }
        models.truncate(3);
        models
    }

    /// Catalog models for `account`'s provider, then the ones this machine has used.
    pub fn catalog_models(&self, key: &SessionKey, account: &Account) -> Vec<String> {
        let mut models: Vec<String> = herder_client_core::catalog_entry(&account.provider)
            .map(|entry| {
                entry
                    .models
                    .iter()
                    .map(|model| model.id.to_owned())
                    .collect()
            })
            .unwrap_or_default();
        for recent in self.recent_models(key, account) {
            if !models.contains(&recent) {
                models.push(recent);
            }
        }
        models
    }

    /// The models offered for the account under the picker's cursor.
    pub fn switch_recent(&self) -> Vec<String> {
        let Some(switch) = &self.switch else {
            return Vec::new();
        };
        let rows = self.switch_rows();
        rows.get(switch.selected.min(rows.len().saturating_sub(1)))
            .and_then(|at| self.accounts_of(&switch.session).get(*at))
            .map_or_else(Vec::new, |account| {
                self.catalog_models(&switch.session, account)
            })
    }

    /// Carries out one input to the switch picker.
    pub(crate) fn switch_input(&mut self, input: Input) -> Vec<Effect> {
        let rows = self.switch_rows();
        let last = rows.len().saturating_sub(1);
        let recent = self.switch_recent();
        let Some(switch) = &mut self.switch else {
            return Vec::new();
        };
        switch.error = None;
        fn typing(switch: &mut Switch) -> &mut String {
            if switch.editing {
                &mut switch.model
            } else {
                switch.selected = 0;
                &mut switch.search
            }
        }
        match input {
            Input::Close => self.switch = None,
            Input::Up => switch.selected = switch.selected.min(last).saturating_sub(1),
            Input::Down => switch.selected = (switch.selected + 1).min(last),
            Input::Top => switch.selected = 0,
            Input::Bottom => switch.selected = last,
            Input::EditModel => switch.editing = true,
            Input::Accounts => switch.editing = false,
            Input::Char(c) => typing(switch).push(c),
            Input::Backspace => {
                typing(switch).pop();
            }
            Input::Clear => typing(switch).clear(),
            Input::Recent(at) => {
                if let Some(model) = recent.get(at) {
                    switch.model.clone_from(model);
                    switch.editing = true;
                }
            }
            Input::Submit => return self.switch_submit(),
        }
        Vec::new()
    }

    /// The commands the picker asks for, or why there are none.
    fn switch_submit(&mut self) -> Vec<Effect> {
        let rows = self.switch_rows();
        let Some(switch) = self.switch.take() else {
            return Vec::new();
        };
        let key = switch.session.clone();
        let chosen = rows.get(switch.selected.min(rows.len().saturating_sub(1)));
        let (Some(session), Some(account)) = (
            self.sessions.get(&key),
            chosen.and_then(|at| self.accounts_of(&key).get(*at)),
        ) else {
            let error = if self.accounts_of(&key).is_empty() {
                "this machine has no accounts"
            } else {
                "no account matches"
            };
            self.switch = Some(Switch {
                error: Some(error.to_owned()),
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
        // Opening lands in the prompt: leave it for NAVIGATE.
        press(&mut app, KeyCode::Esc);
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
        assert_eq!(app.switch_rows(), [0, 1, 2]);
        press(&mut app, KeyCode::Down);
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
        // The search keeps codex alone.
        fake::type_text(&mut app, "codex");
        assert_eq!(app.switch_rows(), [2]);
        press(&mut app, KeyCode::Tab);
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
    fn backspace_leaves_the_model_then_the_search_then_closes() {
        let mut app = app();
        press(&mut app, KeyCode::Char('s'));
        fake::type_text(&mut app, "w");
        press(&mut app, KeyCode::Tab);
        fake::type_text(&mut app, "jq");
        assert_eq!(app.switch.as_ref().unwrap().model, "jq");
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        assert!(!app.switch.as_ref().unwrap().editing);
        assert_eq!(app.switch.as_ref().unwrap().search, "w");
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.switch, None);
        assert_eq!(app.focus, Focus::Transcript);
    }

    #[test]
    fn the_machines_models_are_offered_and_a_tapped_one_is_taken() {
        let mut app = app();
        let accounts = app.machines[0].accounts.clone();
        fake::feed(
            &mut app,
            "h1",
            "s1",
            update(
                "s1",
                9,
                vec![EventBody::ModelSwitched {
                    model: "claude-sonnet".into(),
                }],
                vec![],
            ),
        );
        let models = app.recent_models(&key("h1", "s2"), &accounts[0]);
        assert!(models.contains(&"claude-sonnet".to_owned()), "{models:?}");
        assert!(app.recent_models(&key("h1", "s2"), &accounts[2]).is_empty());
        press(&mut app, KeyCode::Char('s'));
        let at = app
            .switch_recent()
            .iter()
            .position(|m| m == "claude-sonnet")
            .unwrap();
        app.act(Action::Switch(Input::Recent(at)));
        let switch = app.switch.as_ref().unwrap();
        assert_eq!(
            (switch.model.as_str(), switch.editing),
            ("claude-sonnet", true)
        );
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
        // The picker now starts on the new account, and offers Claude by replay.
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.switch_rows(), [2, 0, 1]);
        let switch = app.switch.as_ref().unwrap();
        assert_eq!(switch.selected, 0);
        let accounts = app.accounts_of(&switch.session);
        let session = &app.sessions[&key("h1", "s2")];
        assert_eq!(kind(session, &accounts[0]), Kind::Provider);
    }
}
