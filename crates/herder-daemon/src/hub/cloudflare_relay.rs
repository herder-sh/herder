//! The Durable Object relay in the owner's Cloudflare account (`deploy/cloudflare-relay/`,
//! `docs/remote-access.md`): what the hub and its hosts do with it once it is deployed.
//!
//! The hub signs tokens for it with the secret it set as the Worker's `RELAY_SECRET`: one
//! per client device on request ([`CLIENT_TTL`]), one per host on its replication connection
//! ([`HOST_TTL`]), each renewed before it runs out ([`herder_protocol::RelayGrant`]). The
//! Worker checks them and nothing else; who gets in is decided inside, by each machine's own
//! TLS and authentication, as on any route.
//!
//! Each machine that takes streams through the relay, a host or the hub itself, keeps one
//! WebSocket to it at `/hosts/<host id>` ([`machine`]). For each device that asks for it the
//! relay says `{"open": "<stream id>", "device": ..., "client": ...}`; the machine opens
//! `/streams/<stream id>` and serves the frames of that WebSocket as a direct connection.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_util::{SinkExt, StreamExt};
use herder_protocol::{HostId, RelayGrant, Timestamp};
use serde::Deserialize;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::client;
use crate::ws::tunnel::{Frames, Io};

/// How long a client device's token lasts; client-core renews it once half is used.
pub(crate) const CLIENT_TTL: Duration = Duration::from_secs(24 * 3600);

/// How long a host's token lasts; the hub sends a new one once half is used.
pub(crate) const HOST_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

/// How often a machine pings the relay, and the silence after which it takes the connection
/// for gone. Cloudflare answers pings without waking the Durable Object.
const PING_EVERY: Duration = Duration::from_secs(30);
const SILENCE: Duration = Duration::from_secs(90);

/// Time to open a WebSocket to the relay.
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);

/// Bounds of the wait before connecting to the relay again.
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// The relay as deployed: where it is, and the secret its tokens are signed with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Deployed {
    /// `https://<worker host>`, or `http://` for a local `workerd`.
    pub(crate) url: String,
    /// The Worker's `RELAY_SECRET`.
    pub(crate) secret: String,
}

/// Who a token is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A device, to reach any machine at `/machines/<host id>`.
    Client,
    /// A machine, to take streams at `/hosts/<its id>` and `/streams/<id>`.
    Host,
}

/// A token for `sub` of `kind`, signed with `deployed`'s secret, valid for `ttl` from `now`.
pub(crate) fn grant(
    deployed: &Deployed,
    sub: &str,
    kind: Kind,
    ttl: Duration,
    now: Timestamp,
) -> Result<RelayGrant> {
    let ttl = jiff::SignedDuration::try_from(ttl)?;
    let expires_at = now.checked_add(ttl)?;
    let kind = match kind {
        Kind::Client => "client",
        Kind::Host => "host",
    };
    let claims = serde_json::json!({ "sub": sub, "kind": kind, "exp": expires_at.as_second() });
    let claims = URL_SAFE_NO_PAD.encode(claims.to_string());
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, deployed.secret.as_bytes());
    let mac = ring::hmac::sign(&key, format!("v1.{claims}").as_bytes());
    Ok(RelayGrant {
        url: deployed.url.clone(),
        token: format!("v1.{claims}.{}", URL_SAFE_NO_PAD.encode(mac.as_ref())),
        expires_at,
    })
}

/// The relay as the hub has it deployed: watched by every connection that hands out tokens.
#[derive(Default)]
pub(crate) struct Relay {
    deployed: watch::Sender<Option<Deployed>>,
}

impl Relay {
    /// The relay as deployed now, if it is.
    pub(crate) fn deployed(&self) -> Option<Deployed> {
        self.deployed.borrow().clone()
    }

    /// Changes as the relay is deployed, moved or removed.
    pub(crate) fn subscribe(&self) -> watch::Receiver<Option<Deployed>> {
        self.deployed.subscribe()
    }

    /// Uses `deployed` as the relay from now on; `None` once it is removed.
    pub(crate) fn set(&self, deployed: Option<Deployed>) {
        self.deployed.send_if_modified(|current| {
            let changed = *current != deployed;
            *current = deployed;
            changed
        });
    }

    /// A token for `sub` of `kind` lasting `ttl`, while the relay is deployed.
    pub(crate) fn grant(&self, sub: &str, kind: Kind, ttl: Duration) -> Option<RelayGrant> {
        let deployed = self.deployed()?;
        match grant(&deployed, sub, kind, ttl, Timestamp::now()) {
            Ok(grant) => Some(grant),
            Err(err) => {
                warn!("cannot sign a relay token: {err:#}");
                None
            }
        }
    }
}

/// A stream a device opened to a machine through the relay.
pub(crate) struct Opened {
    /// Its frames, carrying the device's connection.
    pub(crate) stream: Box<dyn Io>,
    /// The socket's peer: Cloudflare's edge.
    pub(crate) peer: SocketAddr,
    /// The device, as the token the hub signed for it names it.
    pub(crate) device: String,
    /// The device's address, as Cloudflare saw it.
    pub(crate) client: Option<String>,
}

/// What the relay asks of a machine on its connection.
#[derive(Deserialize)]
struct Open {
    open: String,
    #[serde(default)]
    device: Option<String>,
    #[serde(default)]
    client: Option<String>,
}

/// Keeps `host` connected to the relay `grants` names while it names one, serving each
/// stream a device opens there with `serve`, until `cancel`.
pub(crate) async fn machine(
    host: HostId,
    mut grants: watch::Receiver<Option<RelayGrant>>,
    serve: Arc<dyn Fn(Opened) + Send + Sync>,
    cancel: CancellationToken,
) {
    let mut backoff = MIN_BACKOFF;
    loop {
        let current = grants.borrow_and_update().clone();
        let usable = current.filter(|grant| grant.expires_at > Timestamp::now());
        let Some(grant) = usable else {
            tokio::select! {
                () = cancel.cancelled() => return,
                changed = grants.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    continue;
                }
            }
        };
        let started = Instant::now();
        let result = tokio::select! {
            () = cancel.cancelled() => return,
            result = connected(&host, &grant, &mut grants, &serve, &cancel) => result,
        };
        match result {
            // The relay moved or went: connect to where it is now, at once.
            Ok(()) => {
                backoff = MIN_BACKOFF;
                continue;
            }
            Err(err) => {
                warn!(url = %grant.url, "connection to the Cloudflare relay failed: {err:#}")
            }
        }
        if started.elapsed() > MAX_BACKOFF {
            backoff = MIN_BACKOFF;
        }
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// One connection of `host` to the relay with `grant`: takes the streams the relay announces
/// until it fails, or returns once `grants` names another relay.
async fn connected(
    host: &HostId,
    grant: &RelayGrant,
    grants: &mut watch::Receiver<Option<RelayGrant>>,
    serve: &Arc<dyn Fn(Opened) + Send + Sync>,
    cancel: &CancellationToken,
) -> Result<()> {
    let (mut control, _) = dial(grant, &format!("/hosts/{host}")).await?;
    info!(url = %grant.url, "taking streams through the Cloudflare relay");
    let mut ping = tokio::time::interval_at(Instant::now() + PING_EVERY, PING_EVERY);
    let mut heard = Instant::now();
    loop {
        tokio::select! {
            changed = grants.changed() => {
                changed.context("the relay's grant is gone")?;
                let moved = grants.borrow().as_ref().map(|g| g.url.clone());
                if moved.as_deref() != Some(grant.url.as_str()) {
                    let _ = control.close(None).await;
                    return Ok(());
                }
            }
            _ = ping.tick() => {
                if heard.elapsed() > SILENCE {
                    bail!("the relay fell silent");
                }
                control.send(Message::Ping(Default::default())).await?;
            }
            message = control.next() => {
                let message = message.context("the relay closed the connection")??;
                heard = Instant::now();
                match message {
                    Message::Text(text) => match serde_json::from_str::<Open>(&text) {
                        Ok(open) => {
                            // The newest token: the stream outlives this one's renewal.
                            let grant = grants.borrow().clone().unwrap_or_else(|| grant.clone());
                            tokio::spawn(accept(grant, open, Arc::clone(serve), cancel.child_token()));
                        }
                        Err(err) => debug!("ignored a message from the relay: {err}"),
                    },
                    Message::Close(_) => bail!("the relay closed the connection"),
                    _ => {}
                }
            }
        }
    }
}

/// Opens the machine's end of the stream `open` announced, and serves it.
async fn accept(
    grant: RelayGrant,
    open: Open,
    serve: Arc<dyn Fn(Opened) + Send + Sync>,
    cancel: CancellationToken,
) {
    if open.open.len() != 32 || !open.open.bytes().all(|b| b.is_ascii_hexdigit()) {
        return debug!("ignored a relay stream with a malformed id");
    }
    let path = format!("/streams/{}", open.open);
    let opening = dial(&grant, &path);
    let (ws, peer) = tokio::select! {
        () = cancel.cancelled() => return,
        opened = opening => match opened {
            Ok(opened) => opened,
            Err(err) => return info!("cannot open a relay stream: {err:#}"),
        },
    };
    serve(Opened {
        stream: Box::new(Frames::new(ws)),
        peer,
        device: open.device.unwrap_or_default(),
        client: open.client,
    });
}

/// A WebSocket to `path` on the relay, with `grant`'s token.
async fn dial(grant: &RelayGrant, path: &str) -> Result<(client::Outer, SocketAddr)> {
    let base = grant
        .url
        .trim_end_matches('/')
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    let bearer = format!("Bearer {}", grant.token);
    let headers = [("authorization".to_owned(), bearer)];
    tokio::time::timeout(
        DIAL_TIMEOUT,
        client::outer(&format!("{base}{path}"), &headers),
    )
    .await
    .map_err(|_| anyhow!("the relay did not answer in {DIAL_TIMEOUT:?}"))?
}
