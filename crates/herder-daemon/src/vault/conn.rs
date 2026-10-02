//! One host connection to the vault: TLS with the host's device certificate, the WebSocket
//! upgrade, the hellos, then batches handled one at a time, each acknowledged once durable.

use std::net::SocketAddr;
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use herder_protocol::{
    Cursor, DeviceId, HostMessage, REPLICATION_VERSION, ReplicationError, ReplicationErrorCode,
    VaultHello, VaultMessage,
};
use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::store::{self, Outcome, VaultStore};
use super::{BUILD, Shared};
use crate::ws::fingerprint;

/// Time a host gets for the TLS and WebSocket handshakes, and again for its hello.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

type Ws = WebSocketStream<TlsStream<TcpStream>>;

pub(super) async fn run(
    stream: TcpStream,
    peer: SocketAddr,
    shared: Arc<Shared>,
    cancel: CancellationToken,
) {
    let _ = stream.set_nodelay(true);
    let ws = tokio::select! {
        () = cancel.cancelled() => return,
        ws = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake(stream, &shared)) => ws,
    };
    let (mut ws, device) = match ws {
        Ok(Ok(ws)) => ws,
        Ok(Err(err)) => return debug!(%peer, "handshake failed: {err:#}"),
        Err(_) => return debug!(%peer, "handshake timed out"),
    };
    let result = tokio::select! {
        () = cancel.cancelled() => Ok(()),
        result = serve(&mut ws, &shared, &device, &cancel) => result,
    };
    if let Err(err) = result {
        debug!(%peer, "host connection failed: {err:#}");
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), ws.close(None)).await;
}

async fn handshake(stream: TcpStream, shared: &Shared) -> Result<(Ws, String)> {
    let tls = shared.tls.acceptor().accept(stream).await?;
    // The verifier makes a client certificate mandatory; this only guards against a change there.
    let device = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|certs| certs.first())
        .map(|cert| fingerprint(cert))
        .context("the host sent no device certificate")?;
    Ok((tokio_tungstenite::accept_async(tls).await?, device))
}

/// Handles the host's messages until it closes the connection or breaks the protocol.
async fn serve(
    ws: &mut Ws,
    shared: &Shared,
    device: &str,
    cancel: &CancellationToken,
) -> Result<()> {
    let first = tokio::time::timeout(HANDSHAKE_TIMEOUT, next(ws))
        .await
        .map_err(|_| anyhow!("no hello within {HANDSHAKE_TIMEOUT:?}"))??;
    let hello = match first {
        Some(Ok(HostMessage::Hello(hello))) => hello,
        Some(Ok(_)) => return fail(ws, bad("the first message must be a hello")).await,
        Some(Err(err)) => return fail(ws, bad(&err)).await,
        None => return Ok(()),
    };
    let identity =
        match shared
            .auth
            .authenticate(device, hello.pairing_code.as_deref(), &hello.build, cancel)
        {
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
        blocking(shared, move |store| {
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
        blocking(shared, move |store| store.cursors(&host)).await?
    };
    info!(
        host_id = %host,
        device_id = %identity.device_id,
        build = %hello.build,
        sessions = cursors.len(),
        "host connected"
    );
    send(ws, hello_message(cursors)).await?;

    while let Some(message) = next(ws).await? {
        let reply = match message {
            Ok(HostMessage::Hello(_)) => return fail(ws, bad("already said hello")).await,
            Ok(HostMessage::Session(summary)) => {
                let host = host.clone();
                blocking(shared, move |store| store.put_summary(&host, &summary)).await?;
                continue;
            }
            Ok(HostMessage::Batch(batch)) => {
                let session_id = batch.session_id.clone();
                let host = host.clone();
                match blocking(shared, move |store| store.append(&host, &batch)).await {
                    Ok(Outcome::Acked(after_seq)) => VaultMessage::Ack(Cursor {
                        session_id,
                        after_seq,
                    }),
                    Ok(Outcome::Rejected { held, reason }) => {
                        warn!(host_id = %hello.host_id, %session_id, ?reason, "batch rejected");
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
    Ok(())
}

fn hello_message(acked: Vec<Cursor>) -> VaultMessage {
    VaultMessage::Hello(VaultHello {
        replication_version: REPLICATION_VERSION,
        build: BUILD.to_owned(),
        acked,
    })
}

/// Runs `call` on the store on the blocking pool; a malformed batch comes back as
/// [`store::BadBatch`].
async fn blocking<T: Send + 'static>(
    shared: &Shared,
    call: impl FnOnce(&mut VaultStore) -> store::Result<T> + Send + 'static,
) -> Result<T> {
    let store = Arc::clone(&shared.store);
    tokio::task::spawn_blocking(move || {
        // Every write is one transaction, so a poisoned store is consistent.
        let mut store = store.lock().unwrap_or_else(PoisonError::into_inner);
        call(&mut store).map_err(|err| match err {
            store::Error::BadBatch(bad) => anyhow::Error::new(bad),
            err => anyhow::Error::new(err).context("the vault database failed"),
        })
    })
    .await
    .context("the vault store task panicked")?
}

/// The next host message: `Some(Err)` describes a frame that is not one.
async fn next(ws: &mut Ws) -> Result<Option<Result<HostMessage, String>>> {
    loop {
        let Some(frame) = ws.next().await else {
            return Ok(None);
        };
        return Ok(Some(match frame.context("reading from the host")? {
            Message::Text(text) => {
                serde_json::from_str(&text).map_err(|err| format!("invalid message: {err}"))
            }
            Message::Binary(_) => Err("messages must be JSON text frames".to_owned()),
            Message::Close(_) => return Ok(None),
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
        }));
    }
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
