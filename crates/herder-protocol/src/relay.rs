//! Relay: the vault forwards a byte stream between a client and a host, so a client reaches a
//! host it cannot dial, through the one machine both can.
//!
//! A relay connection to the vault starts like any other: TLS with the vault's pinned
//! certificate and the peer's device certificate, then the WebSocket upgrade. Its first
//! message is a [`RelayHello`], which only it carries a `relay_version` in, as a JSON text
//! frame. The vault answers with one [`RelayAnswer`], also a text frame. After
//! [`RelayAnswer::Ready`] the connection carries binary frames only, in both directions: the
//! bytes of the stream, in order, split into frames however the sender likes. The vault
//! forwards each frame's bytes as they are and reads nothing of them. Closing the connection
//! ends the stream, and the vault then closes the other end.
//!
//! A stream between a client and a host takes three steps:
//! 1. A client device paired with the vault sends [`RelayHello::Connect`] naming the host.
//! 2. The vault sends the host [`crate::VaultMessage::RelayOpen`] over its replication
//!    connection. The host opens a connection of its own to the vault and sends
//!    [`RelayHello::Accept`] with the stream's id.
//! 3. The vault answers both with [`RelayAnswer::Ready`] and forwards between them.
//!
//! Inside the stream the client runs the connection it would run on a socket to the host: TLS
//! pinning the host's certificate and presenting its device certificate, the WebSocket
//! upgrade, and the hello. The host serves it exactly as a direct connection, so the vault
//! sees ciphertext only, and it is the host that authenticates the device and authorizes
//! every command. The vault decides only whether to forward: for a device paired with it as a
//! client, to a host that replicates to it and said in its hello that it takes relayed
//! connections ([`crate::HostHello::relay`]).
//!
//! A host reaches the vault's CLIProxyAPI the same way, with [`RelayHello::Proxy`]: after
//! [`RelayAnswer::Ready`] the stream is a connection to the proxy, carrying the plain HTTP
//! the host's CLI speaks to it, with the host's client key; the vault speaks the proxy's TLS
//! when it has one.
//!
//! Evolution follows the client protocol's rules, under its own [`RELAY_VERSION`].

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::HostId;

/// Relay protocol version, sent in every [`RelayHello`]; the vault refuses another one as
/// [`RelayErrorCode::Unsupported`].
pub const RELAY_VERSION: u32 = 1;

/// First message of a relay connection to the vault.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RelayHello {
    /// From a client device: open a stream to a host.
    Connect {
        /// Must equal the vault's [`RELAY_VERSION`].
        relay_version: u32,
        /// The host to reach; one that replicates to this vault.
        host_id: HostId,
    },
    /// From a host: this connection is its end of the stream the vault asked it to open.
    Accept {
        /// Must equal the vault's [`RELAY_VERSION`].
        relay_version: u32,
        /// The stream, as [`crate::VaultMessage::RelayOpen`] named it.
        stream_id: String,
    },
    /// From a host: open a stream to the vault's CLIProxyAPI. Only a host the vault told it
    /// forwards to the proxy ([`crate::VaultHello::cliproxyapi_relay`]) may.
    Proxy {
        /// Must equal the vault's [`RELAY_VERSION`].
        relay_version: u32,
    },
}

/// The vault's one answer to a [`RelayHello`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RelayAnswer {
    /// The stream is open: from here on the connection carries its bytes as binary frames.
    Ready,
    /// The vault opened no stream, and closes the connection.
    Refused {
        /// Why.
        error: RelayError,
    },
    /// An answer newer than this build; taken as a refusal.
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Unknown,
}

/// Why the vault opened no stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RelayError {
    /// What kind of refusal.
    pub code: RelayErrorCode,
    /// Human-readable detail.
    pub message: String,
}

/// Kind of refusal of a relay stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelayErrorCode {
    /// The hello was malformed, or names a stream the vault did not ask for or no longer
    /// waits for.
    BadRequest,
    /// The hello's `relay_version` is not the vault's.
    Unsupported,
    /// The device is not paired with the vault, or not as what this hello needs: a client for
    /// [`RelayHello::Connect`], the host itself for the others.
    Forbidden,
    /// No host with that id replicates to this vault, or it takes no relayed connections.
    UnknownHost,
    /// The host is not connected to the vault, or did not open its end in time; try again.
    HostOffline,
    /// The device or the host has as many streams open as the vault allows; try again.
    Busy,
    /// The vault failed, or cannot reach its CLIProxyAPI.
    Internal,
}
