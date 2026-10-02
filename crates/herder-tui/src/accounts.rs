//! The add-account dialog, opened from the machines panel: pick a provider and an id, then the
//! provider's own login runs on the machine in a terminal attached here.
//!
//! herder only relays that terminal. The account is added once the login exits successfully,
//! and shows in the machine's account list.

use herder_client_core::NewAccount;
use herder_protocol::{AccountId, HostId, Provider};

/// The providers whose logins the daemon runs, in the dialog's order.
pub const PROVIDERS: [Provider; 3] = [Provider::Claude, Provider::Codex, Provider::Cursor];

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
        }
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
        let len = PROVIDERS.len();
        self.provider = if forward {
            (self.provider + 1) % len
        } else {
            (self.provider + len - 1) % len
        };
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

/// What to do in the login terminal, for the dialog.
pub fn login_hint(provider: &Provider) -> &'static str {
    match provider {
        Provider::Claude => "claude starts: log in with /login, then leave with /exit.",
        Provider::Codex => "codex shows a device code: open the link and enter it.",
        _ => "the provider's login shows a link: open it and finish there.",
    }
}
