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
//!
//! Each login is also an account on every host the vault gives a client key to: the vault
//! sends hosts its logins ([`crate::VaultMessage::ProviderLogins`]), and a host runs the
//! provider's CLI on one through the proxy with its model names prefixed by the login's `pin`,
//! which the proxy routes to that login only. Its label, fallback mark and usage come from the
//! vault, where owners set them with `set_provider_login`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Provider, Timestamp, UsageWindow};

/// A provider login a vault's CLIProxyAPI holds, without its tokens.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderLogin {
    /// The proxy's name for it, the file it is kept in; for `remove_provider_login`.
    pub name: String,
    /// The provider it signs in to.
    pub provider: Provider,
    /// The owner's name for it; else its email, or the proxy's label for it when it has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The email it is signed in as, when the proxy knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Whether it is fallback-only, as `Account.fallback` is: hosts and the proxy's own pool
    /// pick it only while every other login of its provider is unavailable.
    #[serde(default)]
    pub fallback: bool,
    /// Every limit window it last reported, from the provider's answers to requests the proxy
    /// made with it; empty until it made one.
    #[serde(default)]
    pub usage: Vec<UsageWindow>,
    /// What a host puts before a model name, `<pin>/<model>`, for the proxy to run the request
    /// on this login only.
    pub pin: String,
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
