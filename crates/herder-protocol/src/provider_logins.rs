//! Provider logins: the logins a vault's CLIProxyAPI holds, which hosts reach through endpoint
//! accounts.
//!
//! The proxy keeps each login's tokens on the vault's volume; herder never reads, keeps or
//! relays them. An owner adds a login from an app: `start_provider_login` has the proxy begin
//! the provider's OAuth sign-in and hands back its URL; the owner signs in in a browser, which
//! the provider then sends to a localhost address that does not load, as the proxy is not on
//! the owner's machine; the owner pastes that address into `complete_provider_login`, and the
//! proxy trades the code in it for the login. Only the URL and that one-time code, useless
//! without what the proxy keeps, pass through herder.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Provider, Timestamp};

/// A provider login a vault's CLIProxyAPI holds, without its tokens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderLogin {
    /// The proxy's name for it, the file it is kept in; for `remove_provider_login`.
    pub name: String,
    /// The provider it signs in to.
    pub provider: Provider,
    /// The account's email, or the proxy's label for it when it has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Whether the proxy can use it now.
    pub state: ProviderLoginState,
    /// When a login in `cooling_down` is tried again, as far as the proxy knows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<Timestamp>,
    /// What the proxy says about it, such as why it is cooling down or failing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Whether a vault's CLIProxyAPI can use a provider login now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderLoginState {
    /// In use.
    Active,
    /// Out of quota or rate-limited, until `until`.
    CoolingDown,
    /// The provider no longer takes it: sign in again, or remove it.
    NeedsSignIn,
    /// Failing for another reason, which `message` gives.
    Error,
    /// Turned off on the proxy.
    Disabled,
}
