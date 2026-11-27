//! One connection to the hub, direct or delivered by `cloudflared` inside a tunnel's
//! frames: TLS with the peer's device certificate, then the WebSocket upgrade, whose path
//! tells an end of a relay stream ([`relay`]) from the rest, and on `/` the first message,
//! which tells a host from a client. A client is served by the daemon's client server over
//! the fleet view; a host gets the hellos, then its messages are
//! handled one at a time: an image is durable before the next is read, and a batch is
//! acknowledged once durable. An image the host's cap leaves no room for is dropped, never
//! failing the connection.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use herder_protocol::{
    Cursor, DeviceId, HostId, HostMessage, HubHello, HubMessage, ProviderLogin,
    REPLICATION_VERSION, RelayRoute, ReplicationError, ReplicationErrorCode,
};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::store::{self, Kept, Outcome, SecretUse};
use super::upgrade::{self, Upgrade as Upgrading};
use super::{BUILD, Shared, blocking, cliproxyapi, relay, secrets};
use crate::auth::DeviceRole;
use crate::ws::{self, Transport, Ws, tunnel};

/// A connection whose upgrade request is read and not answered yet.
pub(super) type Upgrade = Upgrading<tokio_rustls::server::TlsStream<tunnel::Metered>>;

/// Time a peer gets for the TLS and WebSocket handshakes, and again for its hello.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Serves a connection accepted on a listen address, or, with `tunneled`, on the tunnel
/// listener, where it first takes `cloudflared`'s WebSocket upgrade.
pub(super) async fn run(
    stream: TcpStream,
    peer: SocketAddr,
    tunneled: bool,
    shared: Arc<Shared>,
    cancel: CancellationToken,
) {
    let _ = stream.set_nodelay(true);
    // A relay stream's peer that is gone must not hold the other end's connection open.
    if let Err(err) = ws::notice_a_gone_peer(&stream) {
        debug!(%peer, "cannot set the socket's keepalive: {err}");
    }
    #[cfg(target_os = "linux")]
    if let Err(err) = ws::hold_back_unsent(&stream) {
        debug!(%peer, "cannot limit the socket's unsent bytes: {err}");
    }
    let opened = tokio::select! {
        () = cancel.cancelled() => return,
        opened = tokio::time::timeout(HANDSHAKE_TIMEOUT, open(&shared, stream, peer, tunneled)) => opened,
    };
    serve_opened(opened, peer, shared, cancel).await;
}

/// Serves a connection the relay Worker delivered in `frames`, counted by `pass` until it
/// authenticated, exactly as one from the tunnel listener.
pub(super) async fn run_relayed(
    frames: Box<dyn tunnel::Io>,
    pass: tunnel::Pass,
    peer: SocketAddr,
    shared: Arc<Shared>,
    cancel: CancellationToken,
) {
    let opening = secure(&shared, Transport::Relay(frames), pass);
    let opened = tokio::select! {
        () = cancel.cancelled() => return,
        opened = tokio::time::timeout(HANDSHAKE_TIMEOUT, opening) => opened,
    };
    serve_opened(opened, peer, shared, cancel).await;
}

/// Serves a connection whose handshakes up to its upgrade request ended as `opened`.
async fn serve_opened(
    opened: Result<Result<(Upgrade, String, tunnel::Pass)>, tokio::time::error::Elapsed>,
    peer: SocketAddr,
    shared: Arc<Shared>,
    cancel: CancellationToken,
) {
    let (upgrade, device, pass) = match opened {
        Ok(Ok(opened)) => opened,
        Ok(Err(err)) => return info!(%peer, "handshake failed: {err:#}"),
        Err(_) => return info!(%peer, "handshake timed out"),
    };
    if upgrade.path() != "/" {
        let Some(route) = RelayRoute::parse(upgrade.path()) else {
            debug!(%peer, path = upgrade.path(), "refused an upgrade to an unknown path");
            let _ = tokio::time::timeout(Duration::from_secs(1), upgrade.refuse(404, None)).await;
            return;
        };
        return relay::serve(upgrade, route, &shared, &device, pass, peer, cancel).await;
    }
    let ws = tokio::select! {
        () = cancel.cancelled() => return,
        ws = tokio::time::timeout(HANDSHAKE_TIMEOUT, upgrade.accept(ws::ws_config())) => ws,
    };
    let mut ws = match ws {
        Ok(Ok(ws)) => ws,
        Ok(Err(err)) => return info!(%peer, "upgrade failed: {err:#}"),
        Err(_) => return info!(%peer, "upgrade timed out"),
    };
    let first = tokio::select! {
        () = cancel.cancelled() => return,
        first = tokio::time::timeout(HANDSHAKE_TIMEOUT, first_text(&mut ws)) => first,
    };
    let first = match first {
        Ok(Ok(Some(first))) => first,
        Ok(Ok(None)) => return,
        Ok(Err(err)) => return info!(%peer, "no hello: {err:#}"),
        Err(_) => return info!(%peer, "no hello within {HANDSHAKE_TIMEOUT:?}"),
    };
    if !is_host(&first) {
        return shared
            .clients
            .serve(ws, device, first, pass, peer, cancel)
            .await;
    }
    let result = tokio::select! {
        () = cancel.cancelled() => Ok(()),
        result = serve(&mut ws, &first, &shared, &device, pass, &cancel) => result,
    };
    if let Err(err) = result {
        info!(%peer, "host connection failed: {err:#}");
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), ws.close(None)).await;
}

/// The handshakes of a connection up to its upgrade request, which is read and not answered
/// yet, once a gate lets it in: on a tunneled one, the upgrade from `cloudflared` first.
async fn open(
    shared: &Shared,
    stream: TcpStream,
    peer: SocketAddr,
    tunneled: bool,
) -> Result<(Upgrade, String, tunnel::Pass)> {
    let (transport, pass) = if tunneled {
        let (frames, pass, _) = tunnel::accept(stream, &shared.gates.tunnel)
            .await
            .context("the tunnel's WebSocket upgrade")?;
        (Transport::Tunnel(Box::new(frames)), pass)
    } else {
        (Transport::Tcp(stream), shared.gates.direct(peer)?)
    };
    secure(shared, transport, pass).await
}

/// The TLS handshake on `transport` and the upgrade request after it, read and not answered
/// yet.
async fn secure(
    shared: &Shared,
    transport: Transport,
    pass: tunnel::Pass,
) -> Result<(Upgrade, String, tunnel::Pass)> {
    let (tls, device) = ws::secure(&shared.tls, pass.meter(transport)).await?;
    let upgrade = upgrade::read(tls).await?;
    Ok((upgrade, device, pass))
}

/// The first text frame, or `None` when the peer closes first.
async fn first_text(ws: &mut Ws) -> Result<Option<String>> {
    loop {
        let Some(frame) = ws.next().await else {
            return Ok(None);
        };
        match frame.context("reading the first message")? {
            Message::Text(text) => return Ok(Some(text.as_str().to_owned())),
            Message::Binary(_) => bail!("messages must be JSON text frames"),
            Message::Close(_) => return Ok(None),
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

/// Whether a first message is a host's hello: only that one carries a replication version.
/// Read without building the message: it comes from a peer not authenticated yet, and JSON
/// parsed into a tree takes many times its size in memory.
fn is_host(first: &str) -> bool {
    #[derive(serde::Deserialize)]
    struct Hello {
        replication_version: Option<serde::de::IgnoredAny>,
    }
    serde_json::from_str::<Hello>(first).is_ok_and(|hello| hello.replication_version.is_some())
}

/// Handles the host's messages after its hello, `first`, until it closes the connection,
/// breaks the protocol or falls silent; `pass` counts a tunneled one until it authenticated.
async fn serve(
    ws: &mut Ws,
    first: &str,
    shared: &Shared,
    device: &str,
    pass: tunnel::Pass,
    cancel: &CancellationToken,
) -> Result<()> {
    let hello = match serde_json::from_str(first) {
        Ok(HostMessage::Hello(hello)) => hello,
        Ok(_) => return fail(ws, bad("the first message must be a hello")).await,
        Err(err) => return fail(ws, bad(&format!("invalid message: {err}"))).await,
    };
    let identity = match shared.auth.authenticate(
        device,
        hello.pairing_code.as_deref(),
        &hello.build,
        DeviceRole::Host,
        cancel,
    ) {
        Ok(identity) => identity,
        Err(error) => {
            let error = ReplicationError {
                code: ReplicationErrorCode::Forbidden,
                message: error.message,
            };
            return fail(ws, error).await;
        }
    };
    drop(pass);
    if hello.replication_version != REPLICATION_VERSION {
        send(ws, hello_message(Vec::new(), None, false)).await?;
        bail!(
            "host {} speaks replication {}, this hub {REPLICATION_VERSION}",
            hello.host_id,
            hello.replication_version
        );
    }
    let paired: Vec<DeviceId> = shared
        .auth
        .devices()
        .into_iter()
        .map(|(device, _)| device.device_id)
        .collect();
    let host = hello.host_id.clone();
    let cap = hello.attachments_cap;
    let bound = {
        let (device, host, name) = (identity.device_id.clone(), host.clone(), hello.host_name);
        blocking(&shared.store, move |store| {
            store.bind(&device, &host, &name, cap, &paired)
        })
        .await?
    };
    if !bound {
        let error = ReplicationError {
            code: ReplicationErrorCode::Forbidden,
            message: format!(
                "this device replicates as another host, or another paired device replicates \
                 as {host}; revoke that device on the hub with `herder pair --revoke`"
            ),
        };
        return fail(ws, error).await;
    }
    // Before the hello, so the host finds its key as soon as it may ask for it, and is told
    // where the proxy is only once it holds one.
    let mut proxy = None;
    // Whether the host reaches the proxy through this hub, which names no address for it.
    let mut proxy_relayed = false;
    if let (Some(keys), Some(key)) = (shared.cliproxyapi.get(), &shared.secrets_key) {
        match keys.ensure_host(&shared.store, key, &host).await {
            Ok(minted) => {
                if minted {
                    info!(host_id = %host, "CLIProxyAPI key minted for the host");
                }
                if cliproxyapi::set_name(&host).is_some() {
                    proxy = keys.endpoint().map(str::to_owned);
                    proxy_relayed = proxy.is_none();
                }
            }
            Err(err) => warn!(host_id = %host, "cannot give the host a CLIProxyAPI key: {err:#}"),
        }
    }
    let cursors = {
        let host = host.clone();
        blocking(&shared.store, move |store| store.cursors(&host)).await?
    };
    info!(
        host_id = %host,
        device_id = %identity.device_id,
        build = %hello.build,
        sessions = cursors.len(),
        "host connected"
    );
    // A host told where the proxy is runs its logins as accounts.
    let logins = match shared.cliproxyapi.get() {
        Some(keys) if proxy.is_some() || proxy_relayed => Some(keys.management().subscribe()),
        _ => None,
    };
    send(ws, hello_message(cursors, proxy, proxy_relayed)).await?;

    // Before the host is shown online, so it is never online without its relay.
    let mut relay = hello.relay.then(|| shared.relays.host_connected(&host));
    let presence = shared.fleet.presence();
    presence.connected(&host);
    shared.fleet.refresh_hosts().await;
    let result = receive(ws, shared, &host, cap, logins, &mut relay).await;
    drop(relay);
    let seen = presence.disconnected(&host);
    let device = identity.device_id.clone();
    blocking(&shared.store, move |store| store.seen(&device, seen)).await?;
    shared.fleet.refresh_hosts().await;
    info!(host_id = %host, online = presence.online(&host), "host disconnected");
    result
}

/// Handles the host's messages once it is connected, keeping at most `cap` bytes of its
/// images, until another host takes over one of its sessions: then the connection is
/// dropped, so the host reconnects and stops that session. Sends the host `logins` as they
/// are listed, the current ones first, and asks it to open its end of each stream `relay`
/// gives.
async fn receive(
    ws: &mut Ws,
    shared: &Shared,
    host: &HostId,
    cap: Option<u64>,
    mut logins: Option<watch::Receiver<Option<Vec<ProviderLogin>>>>,
    relay: &mut Option<relay::HostEnd>,
) -> Result<()> {
    let mut superseded = shared.superseded.subscribe();
    if let Some(logins) = &mut logins {
        logins.mark_changed();
    }
    // Sending the host its logins is no sign of life from it.
    let mut deadline = tokio::time::Instant::now() + shared.liveness;
    loop {
        let frame = tokio::select! {
            listed = next_logins(&mut logins) => {
                send(ws, HubMessage::ProviderLogins { logins: listed }).await?;
                continue;
            }
            open = next_open(relay) => {
                let relay::Open { stream_id, device_id } = open;
                send(ws, HubMessage::RelayOpen { stream_id, device_id }).await?;
                continue;
            }
            frame = tokio::time::timeout_at(deadline, ws.next()) => frame,
            other = superseded.recv() => match other {
                Ok(other) if other != *host => continue,
                _ => bail!("another host took over one of this host's sessions"),
            },
        };
        let frame = match frame {
            Err(_) => bail!("the host was silent for {:?}", shared.liveness),
            Ok(None) => return Ok(()),
            Ok(Some(frame)) => frame.context("reading from the host")?,
        };
        deadline = tokio::time::Instant::now() + shared.liveness;
        shared.fleet.presence().heard(host);
        let message = match frame {
            Message::Text(text) => {
                serde_json::from_str(&text).map_err(|err| format!("invalid message: {err}"))
            }
            Message::Binary(_) => Err("messages must be JSON text frames".to_owned()),
            Message::Close(_) => return Ok(()),
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
        };
        let reply = match message {
            Ok(HostMessage::Hello(_)) => return fail(ws, bad("already said hello")).await,
            Ok(HostMessage::Session(summary)) => {
                let host = host.clone();
                let superseded = blocking(&shared.store, move |store| {
                    let superseded = store.claim(&host, &summary.session_id)?;
                    store.put_summary(&host, &summary)?;
                    Ok(superseded)
                })
                .await?;
                shared.supersede(superseded);
                shared.fleet.refresh().await;
                shared.fleet.refresh_hosts().await;
                continue;
            }
            Ok(HostMessage::Batch(batch)) => {
                let session_id = batch.session_id.clone();
                let owner = host.clone();
                let stored = blocking(&shared.store, move |store| {
                    let superseded = store.claim(&owner, &batch.session_id)?;
                    let outcome = store.append(&owner, &batch)?;
                    let current = store.recovered_to(&owner, &batch.session_id)?.is_none();
                    Ok((outcome, batch, superseded, current))
                })
                .await;
                match stored {
                    Ok((Outcome::Acked(after_seq), batch, superseded, current)) => {
                        shared.supersede(superseded);
                        if let Some(newest) = batch.events.last() {
                            shared.fleet.presence().stored(host, newest.at);
                        }
                        // A copy another host took over is kept but never shown: its session goes on
                        // elsewhere at the same seqs.
                        if current {
                            shared.fleet.publish(&batch);
                        }
                        shared.fleet.refresh().await;
                        HubMessage::Ack(Cursor {
                            session_id,
                            after_seq,
                        })
                    }
                    Ok((Outcome::Rejected { held, reason }, _, superseded, _)) => {
                        shared.supersede(superseded);
                        warn!(host_id = %host, %session_id, ?reason, "batch rejected");
                        HubMessage::Rejected {
                            cursor: Cursor {
                                session_id,
                                after_seq: held,
                            },
                            reason,
                        }
                    }
                    Err(err) => match err.downcast::<store::BadBatch>() {
                        Ok(bad_batch) => return fail(ws, bad(&bad_batch.to_string())).await,
                        Err(err) => return Err(err),
                    },
                }
            }
            Ok(HostMessage::Attachment(image)) => {
                let (owner, session_id) = (host.clone(), image.session_id.clone());
                let attachment_id = image.attachment.attachment_id.clone();
                let stored = blocking(&shared.store, move |store| {
                    store.put_attachment(&owner, &image, cap)
                })
                .await;
                match stored {
                    Ok(Kept::Stored { evicted }) => {
                        if evicted > 0 {
                            debug!(host_id = %host, evicted, "oldest images evicted for a new one");
                        }
                        shared.fleet.refresh_hosts().await;
                        continue;
                    }
                    Ok(Kept::Off) => {
                        debug!(host_id = %host, %session_id, %attachment_id, "image dropped: this host backs up no images");
                        continue;
                    }
                    Ok(Kept::OverCap) => {
                        warn!(host_id = %host, %session_id, %attachment_id, "image dropped: bigger than this host's cap");
                        continue;
                    }
                    Err(err) => match err.downcast::<store::BadBatch>() {
                        Ok(bad_batch) => return fail(ws, bad(&bad_batch.to_string())).await,
                        Err(err) => return Err(err),
                    },
                }
            }
            Ok(HostMessage::ListSecrets { request_id }) => {
                secrets::list(&shared.store, host, request_id).await?
            }
            Ok(HostMessage::FetchSecrets(fetch)) => {
                let key = shared.secrets_key.as_ref();
                let used = SecretUse::Session(fetch.session_id);
                secrets::fetch(
                    &shared.store,
                    key,
                    host,
                    fetch.request_id,
                    used,
                    fetch.secrets,
                )
                .await?
            }
            Ok(HostMessage::FetchHostSecrets(fetch)) => {
                let key = shared.secrets_key.as_ref();
                let used = SecretUse::Host(fetch.purpose);
                secrets::fetch(
                    &shared.store,
                    key,
                    host,
                    fetch.request_id,
                    used,
                    fetch.secrets,
                )
                .await?
            }
            Ok(HostMessage::Unknown) => continue,
            Err(err) => return fail(ws, bad(&err)).await,
        };
        send(ws, reply).await?;
    }
}

/// The next stream the host is to open its end of; never, for a host that takes none.
async fn next_open(relay: &mut Option<relay::HostEnd>) -> relay::Open {
    match relay {
        Some(relay) => relay.next().await,
        None => std::future::pending().await,
    }
}

/// The logins once they change and have been listed; never, without a receiver or once its
/// sender is gone.
async fn next_logins(
    logins: &mut Option<watch::Receiver<Option<Vec<ProviderLogin>>>>,
) -> Vec<ProviderLogin> {
    let Some(receiver) = logins else {
        return std::future::pending().await;
    };
    loop {
        if receiver.changed().await.is_err() {
            return std::future::pending().await;
        }
        if let Some(listed) = receiver.borrow_and_update().clone() {
            return listed;
        }
    }
}

fn hello_message(
    acked: Vec<Cursor>,
    cliproxyapi: Option<String>,
    cliproxyapi_relay: bool,
) -> HubMessage {
    HubMessage::Hello(HubHello {
        replication_version: REPLICATION_VERSION,
        build: BUILD.to_owned(),
        acked,
        cliproxyapi,
        cliproxyapi_relay,
    })
}

async fn send(ws: &mut Ws, message: HubMessage) -> Result<()> {
    let text = serde_json::to_string(&message)?;
    ws.send(Message::text(text))
        .await
        .context("writing to the host")
}

fn bad(message: &str) -> ReplicationError {
    ReplicationError {
        code: ReplicationErrorCode::BadRequest,
        message: message.to_owned(),
    }
}

/// Reports `error` to the host; the connection then closes.
async fn fail(ws: &mut Ws, error: ReplicationError) -> Result<()> {
    let message = error.message.clone();
    send(ws, HubMessage::Error { error }).await?;
    bail!("{message}")
}

#[cfg(test)]
mod tests {
    use super::is_host;

    #[test]
    fn a_host_is_told_by_its_replication_version_alone() {
        assert!(is_host(r#"{"type":"hello","replication_version":3}"#));
        assert!(!is_host(r#"{"type":"hello","protocol_version":3}"#));
        assert!(!is_host(r#"{"replication_version":null}"#));
        assert!(!is_host("not json"));
    }

    /// Fuzz targets, run briefly by `cargo test`, at length with `PROPTEST_CASES`: the hub
    /// reads the first message of a peer it has not authenticated to tell a host from a client.
    mod fuzz {
        use herder_protocol::{HostHello, HostId, HostMessage, REPLICATION_VERSION};
        use proptest::prelude::*;
        use serde_json::Value;

        use super::is_host;

        /// JSON of any shape, a few levels deep.
        fn json() -> impl Strategy<Value = Value> {
            let leaf = prop_oneof![
                Just(Value::Null),
                any::<bool>().prop_map(Value::Bool),
                any::<i64>().prop_map(Value::from),
                "\\PC{0,16}".prop_map(Value::String),
            ];
            leaf.prop_recursive(4, 64, 8, |inner| {
                prop_oneof![
                    prop::collection::vec(inner.clone(), 0..8).prop_map(Value::Array),
                    prop::collection::btree_map("\\PC{0,12}", inner, 0..8)
                        .prop_map(|map| Value::Object(map.into_iter().collect())),
                ]
            })
        }

        proptest! {
            /// Any object is a host's exactly when it names a replication version.
            #[test]
            fn fuzz_any_object(
                fields in prop::collection::btree_map("\\PC{0,12}", json(), 0..8),
                version in proptest::option::of(json()),
            ) {
                let mut object: serde_json::Map<String, Value> = fields.into_iter().collect();
                object.remove("replication_version");
                if let Some(version) = &version {
                    object.insert("replication_version".into(), version.clone());
                }
                let named = version.is_some_and(|version| !version.is_null());
                prop_assert_eq!(is_host(&Value::Object(object).to_string()), named);
            }

            /// Any text: a host or not, and a host's hello decoded or refused, never a panic.
            #[test]
            fn fuzz_any_text(text in "\\PC{0,512}") {
                if is_host(&text) {
                    let _ = serde_json::from_str::<HostMessage>(&text);
                }
            }

            /// A host's hello with bytes of it changed: never a panic.
            #[test]
            fn fuzz_a_hello_changed(
                edits in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..8),
            ) {
                let hello = HostMessage::Hello(HostHello {
                    replication_version: REPLICATION_VERSION,
                    host_id: HostId::new("01JABCDEFGHJKMNPQRSTVWXYZ0"),
                    host_name: "box".into(),
                    build: "1.0".into(),
                    pairing_code: Some("ABCDE-FGHJK".into()),
                    attachments_cap: Some(1 << 30),
                    relay: true,
                });
                let mut bytes = serde_json::to_vec(&hello).unwrap();
                for (at, byte) in edits {
                    let at = at.index(bytes.len());
                    bytes[at] = byte;
                }
                let text = String::from_utf8_lossy(&bytes);
                if is_host(&text) {
                    let _ = serde_json::from_str::<HostMessage>(&text);
                }
            }
        }
    }
}
