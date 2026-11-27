//! Remote access: the routes a hub offers devices away from home, besides its own addresses
//! (`docs/remote-access.md`).
//!
//! - **herder's WireGuard on the hub**: a userspace WireGuard endpoint on one UDP port. A
//!   device enrolls the public half of a key it generated with
//!   [`CommandBody::EnrollWireGuard`](crate::CommandBody::EnrollWireGuard) on its ordinary,
//!   authenticated connection to the hub; the private half never leaves it. Inside the
//!   tunnel the device reaches the hub's herder port only, and every machine through the hub's
//!   relay ([`crate::relay`]). The key is kept on the device's record, so revoking the device
//!   drops it.
//! - **A Durable Object relay** in the owner's Cloudflare account, which the hub deploys: hosts
//!   and the hub keep a WebSocket out to it, devices connect out to it, and it pairs streams
//!   and forwards their ciphertext. It refuses any upgrade without a token the hub signed
//!   ([`RelayGrant`]).
//! - **A managed Cloudflare Tunnel** for the hub, which the hub creates and runs; its
//!   hostname becomes one of the hub's public addresses.
//!
//! Every route carries the same connection inside: TLS pinning the machine's certificate and
//! presenting the device's, then the WebSocket and the hello. None of these sees more than
//! ciphertext.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{SecretRef, Timestamp};

/// The UDP port a hub's WireGuard endpoint binds unless set otherwise, WireGuard's usual.
pub const DEFAULT_WIREGUARD_PORT: u16 = 51820;

/// What a hub offers devices away from home, answering `get_remote_access` and
/// `set_remote_access`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RemoteAccess {
    /// The hub's settings; sent to owners only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<RemoteAccessSettings>,
    /// herder's WireGuard on the hub, when it is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wireguard: Option<WireGuardOffer>,
    /// The Durable Object relay, when it is set up or being set up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<RouteStatus>,
    /// The managed Cloudflare Tunnel, when it is set up or being set up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunnel: Option<RouteStatus>,
}

/// How a hub is reached from outside, as its owners set it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RemoteAccessSettings {
    /// herder's WireGuard endpoint; off when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wireguard: Option<WireGuardSettings>,
    /// What the hub sets up in the owner's Cloudflare account; nothing when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloudflare: Option<CloudflareSettings>,
}

/// herder's WireGuard endpoint on a hub.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WireGuardSettings {
    /// The UDP port the endpoint binds, on every interface; [`DEFAULT_WIREGUARD_PORT`] as a
    /// rule. The router forwards it to the hub.
    pub port: u16,
    /// Where devices send to from outside, as `host:port`: the router's public name or IP
    /// and the port it forwards, such as `home.example.net:51820`.
    pub endpoint: String,
}

/// What a hub sets up in its owner's Cloudflare account, through the API.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CloudflareSettings {
    /// The Cloudflare account.
    pub account_id: String,
    /// The hub secret holding the API token, put with `put_secret`: the hub reads it, and it
    /// never goes back to a client nor to a host.
    pub api_token: SecretRef,
    /// Whether the hub deploys and runs the Durable Object relay, on the account's
    /// `workers.dev` subdomain.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub relay: bool,
    /// The hostname of the managed tunnel to the hub, in one of the account's zones, such as
    /// `hub.example.com`; no tunnel when absent. Once up it is one of the hub's public
    /// addresses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunnel_hostname: Option<String>,
}

/// herder's WireGuard on a hub, as a device sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WireGuardOffer {
    /// Where devices send to, as `host:port`.
    pub endpoint: String,
    /// Whether the device asking has a key enrolled.
    pub enrolled: bool,
}

/// Something a hub set up in its owner's Cloudflare account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RouteStatus {
    /// Where devices reach it: the relay's `https://` URL, or the tunnel's `wss://` one.
    /// Empty until it is known.
    pub url: String,
    /// How far it got.
    pub state: RouteState,
    /// Why it failed, in Cloudflare's or the hub's words, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// How far something a hub sets up got.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RouteState {
    /// The hub is creating, updating or repairing it.
    SettingUp,
    /// It is up and serving.
    Up,
    /// It is not up; `error` says why. The hub tries again on its own.
    Failed,
}

/// A device's enrollment with a hub's WireGuard endpoint, answering `enroll_wireguard`: with
/// the private key it kept, a complete WireGuard peer config.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WireGuardEnrollment {
    /// Where the device sends to, as `host:port`.
    pub endpoint: String,
    /// The hub's WireGuard public key, base64 as WireGuard writes keys.
    pub hub_public_key: String,
    /// The device's address inside the tunnel, such as `10.247.0.2`.
    pub address: String,
    /// The hub's herder port inside the tunnel, as `ip:port`, such as `10.247.0.1:7447`:
    /// the only thing the tunnel reaches.
    pub hub_address: String,
}

/// A token for the Durable Object relay, answering `get_relay_token`, or sent to a host on
/// its replication connection.
///
/// The token is sent as `Authorization: Bearer <token>` on every upgrade to the relay. It is
/// `v1.<claims>.<mac>`: `claims` is base64url (no padding) of the JSON object
/// `{"sub": <device or host id>, "kind": "client" | "host", "exp": <unix seconds>}`, and
/// `mac` is base64url (no padding) of HMAC-SHA256, keyed with the relay's secret, over
/// `v1.<claims>`. The relay refuses with 401 a token that is malformed, expired or wrongly
/// signed, a `client` token anywhere but `/machines/<host id>`, and a `host` token whose
/// `sub` is not the host its path names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RelayGrant {
    /// The relay, as `https://<worker host>`.
    pub url: String,
    /// The token.
    pub token: String,
    /// When the token expires; renew it before.
    #[schemars(with = "crate::TimestampSchema")]
    pub expires_at: Timestamp,
}
