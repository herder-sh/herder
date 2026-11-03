//! The add-account dialog, opened from the machines panel: pick a provider and an id, then
//! either the provider's own login runs on the machine in a terminal attached here, or, for a
//! provider that can run against an API server, the account points at an endpoint and its key.
//!
//! herder only relays the login terminal. A login account is added once the login exits
//! successfully; an endpoint account once the machine takes it. Either then shows in the
//! machine's account list. The accounts screen opens the same dialog.

use herder_client_core::{Machine, NewAccount, next_account_id};
use herder_protocol::{AccountId, CommandBody, HostId, Provider, SecretValue};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, Effect};
use crate::compose::Origin;
use crate::machines::Input;
use crate::secrets::Masked;

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
    /// It runs against an API server rather than a login.
    pub endpoint: bool,
    /// The API server's base URL, for an endpoint account.
    pub url: String,
    /// The API server's client key, for an endpoint account; typed masked, never shown.
    pub key: Masked,
    /// The field being edited.
    pub focus: Field,
    /// Why the last submit was refused.
    pub error: Option<String>,
    /// An endpoint account was sent, and its answer is awaited.
    pub sending: bool,
}

/// A field of the add-account form, in tab order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Field {
    /// The provider, picked with the arrow keys.
    #[default]
    Provider,
    /// How the account runs: a login, or an endpoint; picked with the arrow keys.
    Kind,
    /// The id.
    Id,
    /// The label.
    Label,
    /// The config dir, for a login account.
    ConfigDir,
    /// The endpoint's URL.
    Url,
    /// The endpoint's key.
    Key,
}

/// Whether `provider`'s CLI can run against an endpoint instead of a login.
pub fn supports_endpoint(provider: &Provider) -> bool {
    matches!(provider, Provider::Claude | Provider::Codex)
}

impl AddAccount {
    /// A form for `host_id` on `provider`, prefilling an id that `machines` do not use.
    pub fn for_provider(host_id: HostId, provider: usize, machines: &[Machine]) -> Self {
        let provider = provider % PROVIDERS.len();
        let taken: Vec<&str> = machines
            .iter()
            .filter(|machine| machine.host_id == host_id)
            .flat_map(|machine| {
                machine
                    .accounts
                    .iter()
                    .map(|account| account.account_id.as_str())
            })
            .collect();
        Self {
            host_id,
            provider,
            id: next_account_id(&PROVIDERS[provider], taken),
            label: String::new(),
            config_dir: String::new(),
            endpoint: false,
            url: String::new(),
            key: Masked::default(),
            focus: Field::default(),
            error: None,
            sending: false,
        }
    }

    /// The form's fields, in tab order: the kind only for a provider that can use an
    /// endpoint, then the config dir for a login, or the URL and key for an endpoint.
    pub fn fields(&self) -> Vec<Field> {
        let mut fields = vec![Field::Provider];
        if supports_endpoint(self.provider()) {
            fields.push(Field::Kind);
        }
        fields.extend([Field::Id, Field::Label]);
        if self.endpoint {
            fields.extend([Field::Url, Field::Key]);
        } else {
            fields.push(Field::ConfigDir);
        }
        fields
    }

    fn step(&mut self, forward: bool) {
        let fields = self.fields();
        let len = fields.len();
        let at = fields.iter().position(|f| *f == self.focus).unwrap_or(0);
        let to = if forward { at + 1 } else { at + len - 1 };
        self.focus = fields[to % len];
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
        let old = self.provider().as_str().to_owned();
        let auto = self.id.is_empty() || self.id == old;
        let len = PROVIDERS.len();
        self.provider = if forward {
            (self.provider + 1) % len
        } else {
            (self.provider + len - 1) % len
        };
        if auto {
            self.id = self.provider().as_str().to_owned();
        }
        if !supports_endpoint(self.provider()) {
            self.endpoint = false;
        }
    }

    /// The text field being edited; `None` on a choice.
    pub(crate) fn field(&mut self) -> Option<&mut String> {
        match self.focus {
            Field::Provider | Field::Kind => None,
            Field::Id => Some(&mut self.id),
            Field::Label => Some(&mut self.label),
            Field::ConfigDir => Some(&mut self.config_dir),
            Field::Url => Some(&mut self.url),
            Field::Key => Some(&mut self.key.0),
        }
    }

    /// What the form is complete as, or why it is not; the daemon checks the rest.
    pub(crate) fn submit(&mut self) -> Option<Outcome> {
        let id = self.id.trim();
        let refuse = |form: &mut Self, error: &str, focus: Field| {
            form.error = Some(error.to_owned());
            form.focus = focus;
            None
        };
        if id.is_empty() {
            return refuse(self, "enter an id for the account", Field::Id);
        }
        let account_id = AccountId::new(id);
        let optional = |text: &str| Some(text.trim().to_owned()).filter(|text| !text.is_empty());
        let label = optional(&self.label);
        if !self.endpoint {
            return Some(Outcome::Login(NewAccount {
                account_id,
                provider: self.provider().clone(),
                label,
                config_dir: optional(&self.config_dir),
            }));
        }
        let url = self.url.trim();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return refuse(
                self,
                "enter the endpoint's http:// or https:// URL",
                Field::Url,
            );
        }
        let Some(key) = SecretValue::new(self.key.0.trim()) else {
            return refuse(self, "enter the endpoint's key", Field::Key);
        };
        Some(Outcome::AddEndpoint(CommandBody::AddEndpointAccount {
            account_id,
            provider: self.provider().clone(),
            label,
            endpoint: url.to_owned(),
            key: Some(key),
            key_secret: None,
        }))
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
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Outcome {
    /// The dialog stays open.
    Open,
    /// The user cancelled it.
    Closed,
    /// The form is complete: run this account's login.
    Login(NewAccount),
    /// The form is complete: send the machine this endpoint account.
    AddEndpoint(CommandBody),
}

impl AddAccount {
    /// Carries out one input.
    pub(crate) fn input(&mut self, input: Input) -> Outcome {
        match input {
            Input::Close => return Outcome::Closed,
            // One endpoint account at a time.
            _ if self.sending => {}
            Input::Submit => {
                if let Some(outcome) = self.submit() {
                    self.error = None;
                    self.sending = matches!(outcome, Outcome::AddEndpoint(_));
                    return outcome;
                }
            }
            Input::Up | Input::Down => self.step(input == Input::Down),
            Input::Left | Input::Right if self.focus == Field::Provider => {
                self.cycle(input == Input::Right);
            }
            Input::Left | Input::Right if self.focus == Field::Kind => {
                self.endpoint = !self.endpoint;
            }
            Input::Char(' ') if self.focus == Field::Kind => self.endpoint = !self.endpoint,
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

/// Sends `command`, an endpoint account the dialog describes, to `host_id`.
pub(crate) fn send(host_id: HostId, command: CommandBody) -> Effect {
    Effect::Send {
        host_id: host_id.clone(),
        command,
        origin: Origin::AddAccount(host_id),
    }
}

impl App {
    /// The answer to an endpoint account the dialog sent: the dialog closes once the machine
    /// took it, and the account shows in its list; a refusal shows in the dialog.
    pub(crate) fn account_added(&mut self, result: Result<(), String>) {
        let screen = self
            .account_screen
            .as_mut()
            .map(|screen| &mut screen.adding);
        let panel = self.machine_panel.as_mut().map(|panel| &mut panel.account);
        let Some(dialog) = [screen, panel]
            .into_iter()
            .flatten()
            .find(|dialog| dialog.as_ref().is_some_and(|form| form.sending))
        else {
            return;
        };
        match result {
            Ok(()) => *dialog = None,
            Err(error) => {
                if let Some(form) = dialog {
                    form.sending = false;
                    form.error = Some(error);
                }
            }
        }
    }
}

/// What to do in the login terminal, for the dialog.
pub fn login_hint(provider: &Provider) -> &'static str {
    match provider {
        Provider::Claude => "claude starts: log in with /login, then leave with /exit.",
        Provider::Codex => "codex shows a device code: open the link and enter it.",
        _ => "the provider's login shows a link: open it and finish there.",
    }
}
