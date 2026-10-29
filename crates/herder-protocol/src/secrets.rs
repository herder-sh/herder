//! Secrets: values agents use to call services without ever seeing them.
//!
//! Secrets live only on a vault, encrypted at rest under a master key the vault reads at start;
//! a daemon not linked to a vault has none. They are grouped into named sets, such as
//! `personal` or `prod`; within its set, each secret has a name, as an environment variable
//! has, and a one-line description. The vault's owners manage them from clients (`put_secret`,
//! `list_secrets`, ...) and grant sets to hosts, the devices that replicate to the vault.
//!
//! Values are write-only: `put_secret` carries one to the vault, and no result, event or
//! client message ever carries one back. The only way out is a host's fetch over its
//! replication connection, for secrets in sets granted to that host: on behalf of one of its
//! sessions ([`HostMessage::FetchSecrets`](crate::HostMessage::FetchSecrets)), or for the
//! host's own use, such as a bot's token
//! ([`HostMessage::FetchHostSecrets`](crate::HostMessage::FetchHostSecrets)). A set is granted
//! to a host for one or the other: its sessions never get what is granted for host use. The
//! vault records each fetch ([`SecretAccess`]), never the value. Hosts keep fetched values in
//! memory only, and hand a session's to its command as environment variables.
//!
//! An agent stores a secret without seeing it with `store_secret`: the host reads a file on
//! its own machine and asks for approval ([`SecretStoreRequest`]). An owner allows it from an
//! app, which takes the value from the host once
//! ([`CommandBody::GetSecretToStore`](crate::CommandBody::GetSecretToStore)), puts it on the
//! vault with `put_secret`, as the owner, and tells the host
//! ([`CommandBody::ConfirmSecretStored`](crate::CommandBody::ConfirmSecretStored)). That is the
//! only result carrying a value, of a host's own file, to an owner of the host; it is never
//! journaled nor kept.
//!
//! On a host, a secret is named `NAME` when exactly one set granted to the host has it, and
//! `set/NAME` otherwise ([`SecretRef`]); the environment variable is `NAME` either way.

use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{HostId, SessionId, Timestamp};

/// Most bytes a secret name or set name may have.
pub const MAX_SECRET_NAME_BYTES: usize = 64;

/// Most characters a secret's description may have.
pub const MAX_SECRET_DESCRIPTION_CHARS: usize = 200;

/// Most bytes a secret's value may have.
pub const MAX_SECRET_VALUE_BYTES: usize = 64 * 1024;

/// Most entries one `secret_audit` answer holds.
pub const MAX_SECRET_AUDIT_ENTRIES: usize = 200;

/// Whether `name` may name a secret, as an environment variable: 1 to
/// [`MAX_SECRET_NAME_BYTES`] uppercase ASCII letters, digits and underscores, not starting with
/// a digit (`[A-Z_][A-Z0-9_]*`).
pub fn is_valid_secret_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_SECRET_NAME_BYTES
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// Whether `purpose` may say what a host fetches secrets for its own use for, such as
/// `bot:trip1-team`: 1 to [`MAX_SECRET_NAME_BYTES`] lowercase ASCII letters, digits, `-`, `_`,
/// `.` and `:`.
pub fn is_valid_secret_purpose(purpose: &str) -> bool {
    !purpose.is_empty()
        && purpose.len() <= MAX_SECRET_NAME_BYTES
        && purpose.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.' | b':')
        })
}

/// Whether `name` may name a set of secrets: 1 to [`MAX_SECRET_NAME_BYTES`] lowercase ASCII
/// letters, digits and hyphens, neither starting nor ending with a hyphen nor holding two in a
/// row, as a skill name.
pub fn is_valid_secret_set_name(name: &str) -> bool {
    name.len() <= MAX_SECRET_NAME_BYTES && crate::is_valid_skill_name(name)
}

/// A secret's value. Its `Debug` never shows it, so logging a message that carries one leaks
/// nothing; read it with [`Self::expose`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct SecretValue(String);

impl SecretValue {
    /// A value; refused unless it is 1 to [`MAX_SECRET_VALUE_BYTES`] bytes without a NUL,
    /// as an environment variable can hold.
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        let valid =
            !value.is_empty() && value.len() <= MAX_SECRET_VALUE_BYTES && !value.contains('\0');
        valid.then_some(Self(value))
    }

    /// The value itself.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue(‹redacted›)")
    }
}

/// How a host's session names a secret: `NAME`, or `set/NAME` when more than one set granted
/// to the host has `NAME`. On the wire, that string.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SecretRef {
    /// The set, when named.
    pub set: Option<String>,
    /// The secret's name, and the environment variable it is given as.
    pub name: String,
}

impl FromStr for SecretRef {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (set, name) = match text.split_once('/') {
            Some((set, name)) => (Some(set), name),
            None => (None, text),
        };
        if set.is_some_and(|set| !is_valid_secret_set_name(set)) || !is_valid_secret_name(name) {
            return Err(format!(
                "`{text}` is not a secret name: NAME or set/NAME, NAME as an environment \
                 variable, set in lowercase letters, digits and hyphens"
            ));
        }
        Ok(Self {
            set: set.map(str::to_owned),
            name: name.to_owned(),
        })
    }
}

impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.set {
            Some(set) => write!(f, "{set}/{}", self.name),
            None => f.write_str(&self.name),
        }
    }
}

impl TryFrom<String> for SecretRef {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        text.parse()
    }
}

impl From<SecretRef> for String {
    fn from(secret: SecretRef) -> Self {
        secret.to_string()
    }
}

impl JsonSchema for SecretRef {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SecretRef".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "A secret as a host's session names it: `NAME`, or `set/NAME` when \
                            more than one set granted to the host has `NAME`.",
            "type": "string",
            "pattern": "^([a-z0-9]+(-[a-z0-9]+)*/)?[A-Z_][A-Z0-9_]*$"
        })
    }
}

/// What an agent asked to store as a secret with `store_secret`, as its approval request
/// shows it: where the host read the value, and where it is to go. Never the value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SecretStoreRequest {
    /// The file on the host the value was read from, as the agent named it.
    pub path: String,
    /// The value's size in bytes.
    pub bytes: u64,
    /// The first 8 hex digits of the value's SHA-256, to tell values apart without them.
    pub fingerprint: String,
    /// The set the agent asked for; the owner who allows it may pick another.
    pub set: String,
    /// The secret's name, as [`is_valid_secret_name`] accepts it.
    pub name: String,
    /// What it is for, on one line.
    pub description: String,
    /// Whether the host deletes the file once the owner confirms the vault stored it.
    pub delete_file_after: bool,
}

/// A secret, without its value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SecretInfo {
    /// Its set.
    pub set: String,
    /// Its name, as [`is_valid_secret_name`] accepts it.
    pub name: String,
    /// What it is for, on one line.
    pub description: String,
    /// When its value was last set.
    pub updated_at: Timestamp,
}

/// A set of secrets on a vault, and the hosts it is granted to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SecretSet {
    /// The set's name, as [`is_valid_secret_set_name`] accepts it.
    pub name: String,
    /// The hosts it is granted to, ordered by host id; every session on them may use its
    /// secrets.
    pub hosts: Vec<HostId>,
    /// The hosts it is granted to for their own use, ordered by host id; their sessions do not
    /// get its secrets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub host_use: Vec<HostId>,
}

/// One fetch of a secret's value by a host, as the vault recorded it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SecretAccess {
    /// Position in the vault's audit log, increasing; for `list_secret_audit`'s `before`.
    pub id: u64,
    /// When the vault handed the value out.
    pub at: Timestamp,
    /// The host that fetched it.
    pub host_id: HostId,
    /// The session of that host it was fetched for; absent when the host fetched it for its
    /// own use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// What the host fetched it for, when for its own use, as [`is_valid_secret_purpose`]
    /// accepts it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// The secret's set.
    pub set: String,
    /// The secret's name.
    pub name: String,
}

/// A secret's value, as the vault hands it to a host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FetchedSecret {
    /// The secret, as the fetch named it.
    pub secret: SecretRef,
    /// The set it resolved to.
    pub set: String,
    /// The value.
    pub value: SecretValue,
}

/// Why secrets could not be handed out; nothing was.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SecretError {
    /// What kind of failure.
    pub code: SecretErrorCode,
    /// Human-readable detail.
    pub message: String,
    /// The secret it is about, as named; absent when it is about none in particular.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<SecretRef>,
    /// For `ambiguous`, every `set/NAME` the secret could be, ordered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<SecretRef>,
}

/// Kind of failure handing secrets out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SecretErrorCode {
    /// A bare `NAME` is in more than one set granted to the host; name it as `set/NAME`.
    Ambiguous,
    /// No set granted to the host has the secret, or the named set has no such secret.
    Unknown,
    /// The named set exists but is not granted to the host.
    NotGranted,
    /// The host is not linked to a vault, so it has no secrets.
    NoVault,
    /// The host is linked to a vault but cannot reach it now.
    VaultUnreachable,
}

/// Where a host stands with secrets it fetches for its own use, so a feature that needs them,
/// such as a bot, can say why it is not running.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HostSecretsState {
    /// The host holds every value.
    Ok,
    /// The host has no values yet, or no longer, because it cannot reach its vault; it keeps
    /// trying.
    WaitingForVault,
    /// The vault, or the host's lack of one, refuses them; the host asks again now and then,
    /// should an owner grant them.
    Denied {
        /// Why.
        error: SecretError,
    },
}
