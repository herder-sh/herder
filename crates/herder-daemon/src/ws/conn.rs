//! One client connection: handshake, then a reader handling client messages and a writer
//! draining the connection's [`Outbox`].

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use herder_protocol::{
    ClientMessage, Command, Cursor, ErrorCode, ErrorInfo, EventBody, PROTOCOL_VERSION, ServerHello,
    ServerMessage,
};
use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::{Backend, Shared, fingerprint};
use crate::auth::{self, PAIRING_CODE_HEADER};
use crate::hub::{Outbox, OutboxState};

/// Time a client gets for the TLS and WebSocket handshakes, and again for its hello.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Time the writer waits to deliver a close frame before dropping the socket.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

/// Events read from the journal per query during replay; replay also waits for the queue to
/// drain below this before each query, so a long journal never piles up in memory.
const REPLAY_PAGE: usize = 256;

type Ws = WebSocketStream<TlsStream<TcpStream>>;

/// What the client presented before its hello.
struct Credentials {
    /// Fingerprint of its device certificate.
    device: String,
    /// Pairing code from the upgrade request, sent by a device that is not paired yet.
    pairing_code: Option<String>,
}

pub(super) async fn run<B: Backend>(
    stream: TcpStream,
    peer: SocketAddr,
    shared: Arc<Shared<B>>,
    cancel: CancellationToken,
) {
    if let Err(err) = stream.set_nodelay(true) {
        debug!(%peer, "cannot disable Nagle's algorithm: {err}");
    }
    let ws = tokio::select! {
        () = cancel.cancelled() => return,
        ws = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake(stream, &shared)) => ws,
    };
    let (ws, credentials) = match ws {
        Ok(Ok(ws)) => ws,
        Ok(Err(err)) => {
            debug!(%peer, "handshake failed: {err:#}");
            return;
        }
        Err(_) => {
            debug!(%peer, "handshake timed out");
            return;
        }
    };
    let (sink, stream) = ws.split();
    let outbox = Arc::new(Outbox::default());
    let writer = tokio::spawn(write(sink, Arc::clone(&outbox), cancel.clone()));
    let result = tokio::select! {
        () = cancel.cancelled() => Ok(()),
        result = read(stream, &shared, &outbox, &credentials, &cancel) => result,
    };
    if let Err(err) = result {
        debug!(%peer, "connection failed: {err:#}");
    }
    shared.hub.disconnect(&outbox);
    outbox.finish();
    let _ = writer.await;
    debug!(%peer, "connection closed");
}

async fn handshake<B>(stream: TcpStream, shared: &Shared<B>) -> Result<(Ws, Credentials)> {
    let tls = shared.tls.acceptor().accept(stream).await?;
    // The verifier makes a client certificate mandatory; this only guards against a change there.
    let device = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|certs| certs.first())
        .map(|cert| fingerprint(cert))
        .context("the client sent no device certificate")?;
    let mut pairing_code = None;
    // tungstenite's handshake callback fixes the error type; this closure never fails.
    #[allow(clippy::result_large_err)]
    let ws = tokio_tungstenite::accept_hdr_async(tls, |request: &Request, response: Response| {
        pairing_code = request
            .headers()
            .get(PAIRING_CODE_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        Ok(response)
    })
    .await?;
    Ok((
        ws,
        Credentials {
            device,
            pairing_code,
        },
    ))
}

/// Handles the client's messages until it closes the connection.
async fn read<B: Backend>(
    mut stream: SplitStream<Ws>,
    shared: &Arc<Shared<B>>,
    outbox: &Arc<Outbox>,
    credentials: &Credentials,
    cancel: &CancellationToken,
) -> Result<()> {
    let first = tokio::time::timeout(HANDSHAKE_TIMEOUT, next(&mut stream))
        .await
        .map_err(|_| anyhow!("no hello within {HANDSHAKE_TIMEOUT:?}"))??;
    let hello = match first {
        Some(Ok(ClientMessage::Hello(hello))) => hello,
        Some(Ok(_)) => return reject(outbox, "the first message must be a hello"),
        Some(Err(err)) => return reject(outbox, &err),
        None => return Ok(()),
    };
    if hello.protocol_version != PROTOCOL_VERSION {
        return reject(
            outbox,
            &format!(
                "protocol version {} is not supported; this daemon speaks {PROTOCOL_VERSION}",
                hello.protocol_version
            ),
        );
    }
    let identity = match shared.auth.authenticate(
        &credentials.device,
        credentials.pairing_code.as_deref(),
        &hello.client,
        cancel,
    ) {
        Ok(identity) => identity,
        Err(error) => {
            outbox.push(ServerMessage::Error { error });
            bail!("device {} refused", credentials.device);
        }
    };
    info!(
        client = %hello.client,
        user_id = %identity.user_id,
        device_id = %identity.device_id,
        "client connected"
    );
    outbox.push(ServerMessage::Hello(ServerHello {
        protocol_version: PROTOCOL_VERSION,
        host_id: shared.host.id.clone(),
        host_name: shared.host.name.clone(),
        user_id: identity.user_id.clone(),
        device_id: identity.device_id.clone(),
        role: identity.role,
    }));
    shared.hub.connect(outbox);
    match shared.backend.sessions().await {
        Ok(sessions) => shared.hub.initial_sessions(outbox, sessions),
        Err(err) => {
            warn!("cannot list sessions: {err:#}");
            error(outbox, ErrorCode::Internal, "cannot list sessions");
        }
    }
    for cursor in hello.resume {
        subscribe(shared, outbox, cursor).await;
    }

    while let Some(message) = next(&mut stream).await? {
        match message {
            Ok(ClientMessage::Hello(_)) => {
                error(outbox, ErrorCode::BadRequest, "already said hello")
            }
            Ok(ClientMessage::Subscribe(cursor)) => subscribe(shared, outbox, cursor).await,
            Ok(ClientMessage::Unsubscribe { session_id }) => {
                shared.hub.unsubscribe(outbox, &session_id);
            }
            Ok(ClientMessage::Command(Command { id, body })) => {
                if let Err(error) = auth::authorize(&identity, &body) {
                    outbox.push(ServerMessage::CommandRejected {
                        command_id: id,
                        error,
                    });
                    continue;
                }
                let key = (identity.user_id.clone(), id.clone());
                let apply = {
                    let shared = Arc::clone(shared);
                    let identity = identity.clone();
                    async move { shared.backend.command(&identity, body).await }
                };
                outbox.push(match shared.commands.apply(key, apply).await {
                    Ok(result) => ServerMessage::CommandAccepted {
                        command_id: id,
                        result,
                    },
                    Err(error) => ServerMessage::CommandRejected {
                        command_id: id,
                        error,
                    },
                });
            }
            Err(err) => error(outbox, ErrorCode::BadRequest, &err),
        }
    }
    Ok(())
}

/// The next client message: `Some(Err)` describes a frame that is not one.
async fn next(stream: &mut SplitStream<Ws>) -> Result<Option<Result<ClientMessage, String>>> {
    loop {
        let Some(frame) = stream.next().await else {
            return Ok(None);
        };
        return Ok(Some(match frame.context("reading from the client")? {
            Message::Text(text) => {
                serde_json::from_str(&text).map_err(|err| format!("invalid message: {err}"))
            }
            Message::Binary(_) => Err("messages must be JSON text frames".to_owned()),
            Message::Close(_) => return Ok(None),
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
        }));
    }
}

/// Replays the session's journal after the cursor, then switches the subscription to live.
///
/// The hub holds live events back from the moment of subscribing, so an event appended
/// during replay arrives either from the journal or from the hub, and duplicates are dropped
/// by seq.
async fn subscribe<B: Backend>(shared: &Shared<B>, outbox: &Outbox, cursor: Cursor) {
    let Cursor {
        session_id,
        after_seq,
    } = cursor;
    match shared.backend.sessions().await {
        Ok(sessions) if sessions.iter().any(|s| s.session_id == session_id) => {}
        Ok(_) => {
            let message = format!("session {session_id} does not exist");
            return error(outbox, ErrorCode::NotFound, &message);
        }
        Err(err) => {
            warn!("cannot list sessions: {err:#}");
            return error(outbox, ErrorCode::Internal, "cannot list sessions");
        }
    }
    shared.hub.subscribe(outbox, &session_id);
    let mut last = after_seq;
    loop {
        outbox.wait_below(REPLAY_PAGE).await;
        if outbox.state() != OutboxState::Open {
            return;
        }
        let page = match shared
            .backend
            .read_since(&session_id, last, REPLAY_PAGE)
            .await
        {
            Ok(page) => page,
            Err(err) => {
                warn!(%session_id, "cannot replay the journal: {err:#}");
                shared.hub.unsubscribe(outbox, &session_id);
                return error(outbox, ErrorCode::Internal, "cannot read the journal");
            }
        };
        let full = page.len() == REPLAY_PAGE;
        if let Some(event) = page.last() {
            last = event.seq;
        }
        // Bodies from a newer build cannot be sent; their seqs still advance the cursor.
        outbox.push_all(
            page.into_iter()
                .filter(|event| !matches!(event.body, EventBody::Unknown))
                .map(ServerMessage::Event),
        );
        if !full {
            break;
        }
    }
    shared.hub.go_live(outbox, &session_id, last);
}

fn error(outbox: &Outbox, code: ErrorCode, message: &str) {
    outbox.push(ServerMessage::Error {
        error: ErrorInfo {
            code,
            message: message.to_owned(),
        },
    });
}

/// Answers a failed hello with an error; the connection then closes.
fn reject(outbox: &Outbox, message: &str) -> Result<()> {
    error(outbox, ErrorCode::BadRequest, message);
    bail!("hello refused: {message}")
}

/// Writes queued messages until the outbox stops, the socket fails, or `cancel`.
async fn write(mut sink: SplitSink<Ws, Message>, outbox: Arc<Outbox>, cancel: CancellationToken) {
    let close = loop {
        while let Some(message) = outbox.pop() {
            let text = match serde_json::to_string(&message) {
                Ok(text) => text,
                Err(err) => {
                    warn!("cannot encode a message for a client: {err}");
                    continue;
                }
            };
            let sent = tokio::select! {
                () = cancel.cancelled() => None,
                sent = sink.send(Message::text(text)) => Some(sent),
            };
            match sent {
                None => break,
                Some(Ok(())) => {}
                Some(Err(err)) => {
                    debug!("cannot write to a client: {err}");
                    cancel.cancel();
                    return;
                }
            }
        }
        if cancel.is_cancelled() {
            break (CloseCode::Away, "herder daemon is shutting down");
        }
        match outbox.state() {
            OutboxState::Open => {}
            OutboxState::Finished if outbox.is_empty() => break (CloseCode::Normal, ""),
            OutboxState::Finished => continue,
            OutboxState::Overflowed => {
                break (
                    CloseCode::Again,
                    "too far behind; reconnect and resume from your cursor",
                );
            }
        }
        tokio::select! {
            () = cancel.cancelled() => {}
            () = outbox.ready() => {}
        }
    };
    let frame = CloseFrame {
        code: close.0,
        reason: close.1.into(),
    };
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, sink.send(Message::Close(Some(frame)))).await;
    cancel.cancel();
}
