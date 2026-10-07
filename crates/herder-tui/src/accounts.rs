//! The add-account dialog, opened from the machines panel: pick a provider and an id, then the
//! provider's own login runs on the machine in a terminal attached here.
//!
//! herder only relays that terminal. The account is added once the login exits successfully,
//! and shows in the machine's account list. The accounts screen opens the same dialog.

use herder_client_core::NewAccount;
use herder_protocol::{AccountId, HostId, Provider};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::machines::Input;

/// The providers whose logins the daemon runs, in the dialog's order.
pub const PROVIDERS: [Provider; 4] = [
    Provider::Claude,
    Provider::Codex,
    Provider::Cursor,
    Provider::Opencode,
];

/// The add-account dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddAccount {
    /// The machine to add the account to.
    pub host_id: HostId,
    /// Index into [`PROVIDERS`].
    pub provider: usize,
    /// The new account's id.
    pub id: String,
    /// Its label; the id when empty.
    pub label: String,
    /// Its config dir on the machine; the daemon's default when empty.
    pub config_dir: String,
    /// The field being edited.
    pub focus: Field,
    /// Why the last submit was refused.
    pub error: Option<String>,
    /// Suggested id for the current provider, when the id field is still empty.
    pub suggested_id: String,
}

/// A field of the add-account form, in tab order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Field {
    /// The provider, picked with the arrow keys.
    #[default]
    Provider,
    /// The id.
    Id,
    /// The label.
    Label,
    /// The config dir.
    ConfigDir,
}

impl Field {
    const ALL: [Field; 4] = [Field::Provider, Field::Id, Field::Label, Field::ConfigDir];

    pub(crate) fn next(self, forward: bool) -> Field {
        let len = Self::ALL.len();
        let at = Self::ALL.iter().position(|f| *f == self).unwrap_or(0);
        let to = if forward { at + 1 } else { at + len - 1 };
        Self::ALL[to % len]
    }
}

impl AddAccount {
    /// An empty form for `host_id`.
    pub fn new(host_id: HostId) -> Self {
        Self {
            host_id,
            provider: 0,
            id: String::new(),
            label: String::new(),
            config_dir: String::new(),
            focus: Field::default(),
            error: None,
            suggested_id: String::new(),
        }
    }

    /// Suggests an unused id for the current provider, without filling the field.
    pub fn suggest_id(&mut self, accounts: &[herder_protocol::Account]) {
        self.suggested_id = herder_client_core::next_account_id(accounts, self.provider());
    }

    /// The chosen provider.
    pub fn provider(&self) -> &Provider {
        &PROVIDERS[self.provider % PROVIDERS.len()]
    }

    /// The config dir the daemon picks when none is entered.
    pub fn default_config_dir(&self) -> String {
        let id = if self.id.is_empty() { "<id>" } else { &self.id };
        format!("~/.{}-{id}", self.provider().as_str())
    }

    /// Picks the next or previous provider.
    pub(crate) fn cycle(&mut self, forward: bool) {
        let previous = self.provider().as_str().to_owned();
        let len = PROVIDERS.len();
        self.provider = if forward {
            (self.provider + 1) % len
        } else {
            (self.provider + len - 1) % len
        };
        if self.id.is_empty() || self.id == previous || self.id == self.suggested_id {
            self.suggested_id.clear();
            self.id.clear();
        }
    }

    /// The text field being edited; `None` on the provider.
    pub(crate) fn field(&mut self) -> Option<&mut String> {
        match self.focus {
            Field::Provider => None,
            Field::Id => Some(&mut self.id),
            Field::Label => Some(&mut self.label),
            Field::ConfigDir => Some(&mut self.config_dir),
        }
    }

    /// The account the form describes, or why it is not complete; the daemon checks the rest.
    pub(crate) fn submit(&mut self) -> Option<NewAccount> {
        if self.id.trim().is_empty() && !self.suggested_id.is_empty() {
            self.id = self.suggested_id.clone();
        }
        let id = self.id.trim();
        if id.is_empty() {
            self.error = Some("enter an id for the account".to_owned());
            self.focus = Field::Id;
            return None;
        }
        let optional = |text: &str| Some(text.trim().to_owned()).filter(|text| !text.is_empty());
        Some(NewAccount {
            account_id: AccountId::new(id),
            provider: self.provider().clone(),
            label: optional(&self.label),
            config_dir: optional(&self.config_dir),
        })
    }
}

/// The input a key is to the dialog.
pub(crate) fn input_for_key(key: KeyEvent) -> Option<Input> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let input = match key.code {
        KeyCode::Esc => Input::Close,
        KeyCode::Enter => Input::Submit,
        KeyCode::Tab | KeyCode::Down => Input::Down,
        KeyCode::BackTab | KeyCode::Up => Input::Up,
        KeyCode::Left => Input::Left,
        KeyCode::Right => Input::Right,
        KeyCode::Backspace => Input::Backspace,
        KeyCode::Char('u') if ctrl => Input::Clear,
        KeyCode::Char(c) if !ctrl => Input::Char(c),
        _ => return None,
    };
    Some(input)
}

/// What an input did to the dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The dialog stays open.
    Open,
    /// The user cancelled it.
    Closed,
    /// The form is complete: run this account's login.
    Login(NewAccount),
}

impl AddAccount {
    /// Carries out one input.
    pub(crate) fn input(&mut self, input: Input) -> Outcome {
        match input {
            Input::Close => return Outcome::Closed,
            Input::Submit => {
                if let Some(new) = self.submit() {
                    return Outcome::Login(new);
                }
            }
            Input::Up | Input::Down => self.focus = self.focus.next(input == Input::Down),
            Input::Left | Input::Right if self.focus == Field::Provider => {
                self.cycle(input == Input::Right);
            }
            Input::Char(c) => match self.field() {
                Some(field) => field.push(c),
                None if c == ' ' => self.cycle(true),
                None => {}
            },
            Input::Backspace => {
                if let Some(field) = self.field() {
                    field.pop();
                }
            }
            Input::Clear => {
                if let Some(field) = self.field() {
                    field.clear();
                }
            }
            _ => {}
        }
        Outcome::Open
    }
}

/// An add-account form for `host_id`, with an unused id for the first provider.
pub fn start(machines: &[herder_client_core::Machine], host_id: HostId) -> AddAccount {
    let mut adding = AddAccount::new(host_id.clone());
    if let Some(machine) = machines.iter().find(|machine| machine.host_id == host_id) {
        adding.suggest_id(&machine.accounts);
    }
    adding
}

/// After a dialog key, refill an empty id from the machine's accounts.
pub fn refresh_id(machines: &[herder_client_core::Machine], adding: &mut AddAccount) {
    if adding.id.is_empty()
        && let Some(machine) = machines
            .iter()
            .find(|machine| machine.host_id == adding.host_id)
    {
        adding.suggest_id(&machine.accounts);
    }
}

/// Login, or install first when this machine does not have the CLI and herder can install it.
pub fn after_submit(
    machines: &[herder_client_core::Machine],
    adding: &mut AddAccount,
    new: NewAccount,
) -> Result<crate::terminal::Target, String> {
    let status = herder_client_core::status_on(machines, &adding.host_id, &new.provider);
    if let Some(status) = status
        && !status.installed
    {
        if status.can_install {
            return Ok(crate::terminal::Target::Install(new.provider));
        }
        return Err(format!(
            "install {} on this machine first",
            new.provider.as_str()
        ));
    }
    Ok(crate::terminal::Target::Login(new))
}

/// Providers on other machines this one does not have, as one quiet line.
pub fn elsewhere_line(
    machines: &[herder_client_core::Machine],
    host_id: &HostId,
) -> Option<String> {
    let missing = herder_client_core::used_elsewhere(machines, host_id);
    if missing.is_empty() {
        return None;
    }
    Some(format!(
        "also used elsewhere: {}",
        missing
            .iter()
            .map(Provider::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// A later CLI version of `provider` on another machine, as one quiet line.
pub fn update_line(
    machines: &[herder_client_core::Machine],
    host_id: &HostId,
    provider: &Provider,
) -> Option<String> {
    herder_client_core::newer_elsewhere(machines, host_id, provider)
        .map(|machine| format!("{}: newer on {}", provider.as_str(), machine.name))
}

/// Status of the chosen provider on this machine, for the add-account dialog.
pub fn dialog_notes(
    machines: &[herder_client_core::Machine],
    host_id: &HostId,
    provider: &Provider,
) -> Vec<String> {
    let mut notes = Vec::new();
    let on = herder_client_core::used_on(machines, host_id, provider);
    if !on.is_empty() {
        notes.push(format!("also on {}", on.join(", ")));
    }
    if let Some(status) = herder_client_core::status_on(machines, host_id, provider) {
        if status.installed {
            if let Some(version) = &status.version {
                notes.push(version.clone());
            }
            if let Some(line) = update_line(machines, host_id, provider) {
                notes.push(line);
            }
        } else if status.can_install {
            notes.push("not installed here; enter installs it".into());
        } else {
            notes.push("not installed on this machine".into());
        }
    }
    notes
}

/// Whether submit should install this provider instead of logging in.
pub fn will_install(
    machines: &[herder_client_core::Machine],
    host_id: &HostId,
    provider: &Provider,
) -> bool {
    herder_client_core::status_on(machines, host_id, provider)
        .is_some_and(|status| !status.installed && status.can_install)
}

/// What to do in the login terminal, for the dialog.
pub fn login_hint(provider: &Provider) -> &'static str {
    match provider {
        Provider::Claude => "claude starts: log in with /login, then leave with /exit.",
        Provider::Codex => "codex shows a device code: open the link and enter it.",
        Provider::Cursor => "agent login shows a link: open it and finish there.",
        Provider::Opencode => "opencode asks for a provider, then a key or device login.",
        _ => "the provider's login shows a link: open it and finish there.",
    }
}
