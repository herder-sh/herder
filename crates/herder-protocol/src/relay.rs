//! Relay: the hub forwards a byte stream between a client and a host, so a client reaches a
//! host it cannot dial, through the one machine both can.
//!
//! A relay connection to the hub starts like any other: TLS with the hub's pinned
//! certificate and the peer's device certificate, then the WebSocket upgrade. The upgrade
//! request's path names what the connection is for, as a [`RelayRoute`]; `/` is a host's
//! replication connection or a client's, as before. The hub answers the upgrade only once
//! the stream is open: `101 Switching Protocols`, after which the connection carries binary
//! frames only, in both directions: the bytes of the stream, in order, split into frames
//! however the sender likes. The hub forwards each frame's bytes as they are and reads
//! nothing of them. Closing the connection ends the stream, and the hub then closes the
//! other end. A refusal is an HTTP error status ([`RelayErrorCode::status`]) whose body is
//! a [`RelayError`] as JSON, and the hub then closes the connection.
//!
//! A stream between a client and a host takes one request from each:
//! 1. A client device paired with the hub asks for [`RelayRoute::Machine`], naming the host.
//!    It need not wait for the answer: it may send the stream's first frames right behind
//!    the request, and the hub holds them, unread, until the stream is open.
//! 2. The hub sends the host [`crate::HubMessage::RelayOpen`] over its replication
//!    connection. The host opens a connection of its own to the hub and asks for
//!    [`RelayRoute::Stream`] with the stream's id.
//! 3. The hub answers both upgrades and forwards between them.
//!
//! So a relayed connect costs one round trip more than a direct one: the client's TLS to
//! the hub. The upgrade to the hub and the client's TLS to the host share a round trip.
//!
//! Inside the stream the client runs the connection it would run on a socket to the host: TLS
//! pinning the host's certificate and presenting its device certificate, the WebSocket
//! upgrade, and the hello. The host serves it exactly as a direct connection, so the hub
//! sees ciphertext only, and it is the host that authenticates the device and authorizes
//! every command. The hub decides only whether to forward: for a device paired with it as a
//! client, to a host that replicates to it and said in its hello that it takes relayed
//! connections ([`crate::HostHello::relay`]).
//!
//! A host reaches the hub's CLIProxyAPI the same way, with [`RelayRoute::Proxy`]: once the
//! upgrade is answered the stream is a connection to the proxy, carrying the plain HTTP the
//! host's CLI speaks to it, with the host's client key; the hub speaks the proxy's TLS when
//! it has one.
//!
//! The paths are the contract: a route that changes shape gets a new path, and a hub
//! answers one it does not know with `404`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::HostId;

/// What a relay connection to the hub is for, as its WebSocket upgrade request's path names
/// it. Every id in a path is percent-encoded, so any id makes one path segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelayRoute {
    /// From a client device: a stream to a host that replicates to this hub.
    /// `/machines/<host id>`.
    Machine(HostId),
    /// From a host: its end of the stream the hub asked it to open, as
    /// [`crate::HubMessage::RelayOpen`] named it. `/streams/<stream id>`.
    Stream(String),
    /// From a host: a stream to the hub's CLIProxyAPI. Only a host the hub told it forwards
    /// to the proxy ([`crate::HubHello::cliproxyapi_relay`]) may. `/proxy`.
    Proxy,
}

const MACHINES: &str = "/machines/";
const STREAMS: &str = "/streams/";
const PROXY: &str = "/proxy";

impl RelayRoute {
    /// The request path that names this route.
    pub fn path(&self) -> String {
        match self {
            Self::Machine(host) => format!("{MACHINES}{}", encode(host.as_str())),
            Self::Stream(stream) => format!("{STREAMS}{}", encode(stream)),
            Self::Proxy => PROXY.to_owned(),
        }
    }

    /// The route a request path names, ignoring a query; `None` for any other path, `/`
    /// included.
    pub fn parse(path: &str) -> Option<Self> {
        let path = path.split_once('?').map_or(path, |(path, _)| path);
        if path == PROXY {
            return Some(Self::Proxy);
        }
        if let Some(host) = path.strip_prefix(MACHINES) {
            return decode(host).map(|host| Self::Machine(HostId::new(host)));
        }
        path.strip_prefix(STREAMS)
            .and_then(decode)
            .map(Self::Stream)
    }
}

/// `id`, with every byte but an unreserved one (RFC 3986) percent-encoded.
fn encode(id: &str) -> String {
    let mut encoded = String::with_capacity(id.len());
    for byte in id.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// The id a path segment encodes; `None` for an empty one, one with a `/`, or a bad escape.
fn decode(segment: &str) -> Option<String> {
    if segment.is_empty() || segment.contains('/') {
        return None;
    }
    let mut bytes = Vec::with_capacity(segment.len());
    let mut rest = segment.as_bytes();
    while let Some((&byte, after)) = rest.split_first() {
        if byte == b'%' {
            let hex = after.get(..2)?;
            let hex = std::str::from_utf8(hex).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            rest = &after[2..];
        } else {
            bytes.push(byte);
            rest = after;
        }
    }
    String::from_utf8(bytes).ok()
}

/// Why the hub opened no stream: the body of its refusal, as JSON, under the code's HTTP
/// status.
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
    /// The route names a stream the hub did not ask for or no longer waits for.
    BadRequest,
    /// The device is not paired with the hub, or not as what the route needs: a client for
    /// [`RelayRoute::Machine`], the host itself for the others.
    Forbidden,
    /// No host with that id replicates to this hub, or it takes no relayed connections.
    UnknownHost,
    /// The host is not connected to the hub, or did not open its end in time; try again.
    HostOffline,
    /// The device or the host has as many streams open as the hub allows; try again.
    Busy,
    /// The hub failed, or cannot reach its CLIProxyAPI.
    Internal,
}

impl RelayErrorCode {
    /// The HTTP status the hub refuses an upgrade with for this code.
    pub fn status(self) -> u16 {
        match self {
            Self::BadRequest => 400,
            Self::Forbidden => 403,
            Self::UnknownHost => 404,
            Self::Busy => 429,
            Self::Internal => 500,
            Self::HostOffline => 503,
        }
    }
}
