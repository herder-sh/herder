//! Opaque identifier newtypes.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

macro_rules! id {
    ($(#[$doc:meta] $name:ident),+ $(,)?) => {$(
        #[$doc]
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wraps an existing identifier string.
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            /// The identifier as a string.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    )+};
}

id! {
    /// Identifies a session; a ULID string, opaque to receivers.
    SessionId,
    /// Identifies a turn within a session; a ULID string, opaque to receivers.
    TurnId,
    /// Identifies an item within a session; a ULID string, opaque to receivers.
    ItemId,
    /// Identifies an approval request within a session; a ULID string, opaque to receivers.
    ApprovalId,
    /// Identifies a host running a daemon; a ULID string, opaque to receivers.
    HostId,
    /// Identifies a provider account on a host; a ULID string, opaque to receivers.
    AccountId,
    /// Identifies a named user of a daemon; a ULID string, opaque to receivers.
    UserId,
    /// Identifies a paired client device; a ULID string, opaque to receivers.
    DeviceId,
    /// Identifies a terminal on a host; a ULID string, opaque to receivers.
    TerminalId,
    /// Client-chosen idempotency key of a command; a ULID string, opaque to the daemon.
    CommandId,
}
