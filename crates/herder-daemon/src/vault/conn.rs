//! One connection to the vault: TLS with the peer's device certificate, the WebSocket upgrade
//! and the first message, which tells a host from a client. A client is served by the
//! daemon's client server over the fleet view; a host gets the hellos, then its batches are
//! handled one at a time, each acknowledged once durable.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use herder_protocol::{
    Cursor, DeviceId, HostId, HostMessage, REPLICATION_VERSION, ReplicationError,
    ReplicationErrorCode, VaultHello, VaultMessage,
};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::store::{self, Outcome};
use super::{BUILD, Shared, blocking};
use crate::auth::DeviceRole;
use crate::ws::{self, Ws};

/// Time a peer gets for the TLS and WebSocket handshakes, and again for its hello.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) async fn run(
    stream: TcpStream,
    peer: SocketAddr,
    shared: Arc<Shared>,
    cancel: CancellationToken,
) {
    let _ = stream.set_nodelay(true);
    let ws = tokio::select! {
        () = cancel.cancelled() => return,
        ws = tokio::time::timeout(HANDSHAKE_TIMEOUT, ws::handshake(&shared.tls, stream)) => ws,
    };
    let (mut ws, device) = match ws {
        Ok(Ok(ws)) => ws,
        Ok(Err(err)) => return debug!(%peer, "handshake failed: {err:#}"),
        Err(_) => return debug!(%peer, "handshake timed out"),
    };
    let first = tokio::select! {
        () = cancel.cancelled() => return,
        first = tokio::time::timeout(HANDSHAKE_TIMEOUT, first_text(&mut ws)) => first,
    };
    let first = match first {
        Ok(Ok(Some(first))) => first,
        Ok(Ok(None)) => return,
        Ok(Err(err)) => return debug!(%peer, "no hello: {err:#}"),
        Err(_) => return debug!(%peer, "no hello within {HANDSHAKE_TIMEOUT:?}"),
    };
    if !is_host(&first) {
        return shared.clients.serve(ws, device, first, peer, cancel).await;
    }
    let result = tokio::select! {
        () = cancel.cancelled() => Ok(()),
        result = serve(&mut ws, &first, &shared, &device, &cancel) => result,
    };
    if let Err(err) = result {
        debug!(%peer, "host connection failed: {err:#}");
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), ws.close(None)).await;
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
fn is_host(first: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(first)
        .is_ok_and(|hello| hello.get("replication_version").is_some())
}

/// Handles the host's messages after its hello, `first`, until it closes the connection,
/// breaks the protocol or falls silent.
async fn serve(
    ws: &mut Ws,
    first: &str,
    shared: &Shared,
    device: &str,
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
    if hello.replication_version != REPLICATION_VERSION {
        send(ws, hello_message(Vec::new())).await?;
        bail!(
            "host {} speaks replication {}, this vault {REPLICATION_VERSION}",
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
    let bound = {
        let (device, host, name) = (identity.device_id.clone(), host.clone(), hello.host_name);
        blocking(&shared.store, move |store| {
            store.bind(&device, &host, &name, &paired)
        })
        .await?
    };
    if !bound {
        let error = ReplicationError {
            code: ReplicationErrorCode::Forbidden,
            message: format!(
                "this device replicates as another host, or another paired device replicates \
                 as {host}; revoke that device on the vault with `herder pair --revoke`"
            ),
        };
        return fail(ws, error).await;
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
    send(ws, hello_message(cursors)).await?;

    let presence = shared.fleet.presence();
    presence.connected(&host);
    shared.fleet.refresh_hosts().await;
    let result = receive(ws, shared, &host).await;
    let seen = presence.disconnected(&host);
    let device = identity.device_id.clone();
    blocking(&shared.store, move |store| store.seen(&device, seen)).await?;
    shared.fleet.refresh_hosts().await;
    info!(host_id = %host, online = presence.online(&host), "host disconnected");
    result
}

/// Handles the host's messages once it is connected, until another host recovers one of its
/// sessions: then the connection is dropped, so the host reconnects and stops that session.
async fn receive(ws: &mut Ws, shared: &Shared, host: &HostId) -> Result<()> {
    let mut superseded = shared.superseded.subscribe();
    loop {
        let frame = tokio::select! {
            frame = tokio::time::timeout(shared.liveness, ws.next()) => frame,
            other = superseded.recv() => match other {
                Ok(other) if other != *host => continue,
                _ => bail!("another host recovered one of this host's sessions"),
            },
        };
        let frame = match frame {
            Err(_) => bail!("the host was silent for {:?}", shared.liveness),
            Ok(None) => return Ok(()),
            Ok(Some(frame)) => frame.context("reading from the host")?,
        };
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
                        // A recovered copy is kept but never shown: its session goes on
                        // elsewhere at the same seqs.
                        if current {
                            shared.fleet.publish(&batch);
                        }
                        shared.fleet.refresh().await;
                        VaultMessage::Ack(Cursor {
                            session_id,
                            after_seq,
                        })
                    }
                    Ok((Outcome::Rejected { held, reason }, _, superseded, _)) => {
                        shared.supersede(superseded);
                        warn!(host_id = %host, %session_id, ?reason, "batch rejected");
                        VaultMessage::Rejected {
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
            Ok(HostMessage::Unknown) => continue,
            Err(err) => return fail(ws, bad(&err)).await,
        };
        send(ws, reply).await?;
    }
}

fn hello_message(acked: Vec<Cursor>) -> VaultMessage {
    VaultMessage::Hello(VaultHello {
        replication_version: REPLICATION_VERSION,
        build: BUILD.to_owned(),
        acked,
    })
}

async fn send(ws: &mut Ws, message: VaultMessage) -> Result<()> {
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
    send(ws, VaultMessage::Error { error }).await?;
    bail!("{message}")
}
