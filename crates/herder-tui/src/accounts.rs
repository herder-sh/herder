//! The add-account dialog, opened from the machines panel: pick a provider and an id, then
//! either the provider's own login runs on the machine in a terminal attached here, or, for a
//! provider that can run against an API server, the account points at an endpoint and its key,
//! or, on a machine whose vault runs a CLIProxyAPI, at the vault's logins with the key the vault
//! keeps for the machine.
//!
//! herder only relays the login terminal. A login account is added once the login exits
//! successfully; an endpoint account once the machine takes it. Either then shows in the
//! machine's account list. The accounts screen opens the same dialog.

use herder_client_core::{Machine, NewAccount, next_account_id};
use herder_protocol::{AccountId, CommandBody, CommandResult, HostId, Provider, SecretValue};
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
    /// How it signs in.
    pub kind: Kind,
    /// Where the machine reaches its vault's CLIProxyAPI, as it says once asked; `None` until
    /// then, and when its vault runs none.
    pub vault: Option<String>,
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

/// How an account signs in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    /// The provider's own login, in a terminal.
    #[default]
    Login,
    /// An API server, with a typed key.
    Endpoint,
    /// The logins in the CLIProxyAPI next to the machine's vault, with the key the vault keeps
    /// for the machine: nothing to type.
    Vault,
}

impl Kind {
    /// How the dialog names it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Login => "log in",
            Self::Endpoint => "endpoint",
            Self::Vault => "the vault's logins",
        }
    }
}

/// The secret on a machine's vault holding its CLIProxyAPI client key.
pub const VAULT_KEY_SECRET: &str = "CLIPROXYAPI_KEY";

/// A field of the add-account form, in tab order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Field {
    /// The provider, picked with the arrow keys.
    #[default]
    Provider,
    /// How the account runs: a login, an endpoint, or the vault's logins; picked with the
    /// arrow keys.
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
            kind: Kind::Login,
            vault: None,
            url: String::new(),
            key: Masked::default(),
            focus: Field::default(),
            error: None,
            sending: false,
        }
    }

    /// The form's fields, in tab order: the kind only for a provider that can use an
    /// endpoint, then the config dir for a login, the URL and key for an endpoint, and
    /// nothing more on the vault's logins.
    pub fn fields(&self) -> Vec<Field> {
        let mut fields = vec![Field::Provider];
        if supports_endpoint(self.provider()) {
            fields.push(Field::Kind);
        }
        fields.extend([Field::Id, Field::Label]);
        match self.kind {
            Kind::Login => fields.push(Field::ConfigDir),
            Kind::Endpoint => fields.extend([Field::Url, Field::Key]),
            Kind::Vault => {}
        }
        fields
    }

    /// The ways the account can sign in: the vault's logins only where the machine reaches
    /// them.
    pub fn kinds(&self) -> Vec<Kind> {
        let mut kinds = vec![Kind::Login];
        if supports_endpoint(self.provider()) {
            kinds.push(Kind::Endpoint);
            if self.vault.is_some() {
                kinds.push(Kind::Vault);
            }
        }
        kinds
    }

    /// Picks the next or previous way to sign in.
    fn cycle_kind(&mut self, forward: bool) {
        let kinds = self.kinds();
        let len = kinds.len();
        let at = kinds
            .iter()
            .position(|kind| *kind == self.kind)
            .unwrap_or(0);
        self.kind = kinds[if forward { at + 1 } else { at + len - 1 } % len];
    }

    /// Whether the machine sends it as an endpoint account, rather than running a login.
    pub fn sends_endpoint(&self) -> bool {
        self.kind != Kind::Login
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
            self.kind = Kind::Login;
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
        match (self.kind, &self.vault) {
            (Kind::Login, _) => {
                return Some(Outcome::Login(NewAccount {
                    account_id,
                    provider: self.provider().clone(),
                    label,
                    config_dir: optional(&self.config_dir),
                }));
            }
            (Kind::Vault, Some(vault)) => {
                return Some(Outcome::AddEndpoint(CommandBody::AddEndpointAccount {
                    account_id,
                    provider: self.provider().clone(),
                    label,
                    endpoint: vault.clone(),
                    key: None,
                    key_secret: VAULT_KEY_SECRET.parse().ok(),
                }));
            }
            (Kind::Vault, None) => {
                return refuse(self, "this machine's vault has no logins", Field::Kind);
            }
            (Kind::Endpoint, _) => {}
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
                self.cycle_kind(input == Input::Right);
            }
            Input::Char(' ') if self.focus == Field::Kind => self.cycle_kind(true),
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

/// Asks `host_id` where it backs up, for whether it reaches its vault's logins.
pub(crate) fn ask_vault(host_id: HostId) -> Effect {
    Effect::Send {
        host_id: host_id.clone(),
        command: CommandBody::GetVaultLink,
        origin: Origin::AccountVault(host_id),
    }
}

impl App {
    /// The machine's answer to where it backs up: the open dialog for it offers the vault's
    /// logins where its vault runs a CLIProxyAPI and it fetches secrets from it.
    pub(crate) fn account_vault(
        &mut self,
        host_id: &HostId,
        result: Result<CommandResult, String>,
    ) {
        let vault = match result {
            Ok(CommandResult::VaultLink {
                vault: Some(linked),
                ..
            }) if linked.secrets => linked.cliproxyapi,
            _ => None,
        };
        let screen = self
            .account_screen
            .as_mut()
            .map(|screen| &mut screen.adding);
        let panel = self.machine_panel.as_mut().map(|panel| &mut panel.account);
        for form in [screen, panel].into_iter().flatten().flatten() {
            if form.host_id == *host_id {
                form.vault.clone_from(&vault);
            }
        }
    }

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
