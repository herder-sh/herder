//! One client connection: handshake, then a reader handling client messages and a writer
//! draining the connection's [`Outbox`].

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{FutureExt, SinkExt, StreamExt};
use herder_protocol::{
    ClientHello, ClientMessage, Command, CommandBody, CommandId, CommandResult, Cursor, DeviceId,
    ErrorCode, ErrorInfo, EventBody, LATEST_EVENTS, LATEST_EVENTS_BYTES, MAX_MESSAGE_BYTES,
    PASSKEY_CHALLENGE_SECS, PROTOCOL_VERSION, Seq, ServerHello, ServerMessage, SessionId, SignIn,
};
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::{Backend, Identity, Shared, Tls, Transport, Ws, fingerprint, tunnel};
use crate::auth::{self, Auth};
use crate::fanout::{Outbox, OutboxState};
use crate::session;

/// Time a client gets for the TLS and WebSocket handshakes, and again for its hello.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Time the writer waits to deliver a close frame before dropping the socket.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

/// Events read from the journal per query during replay; replay also waits for the queue to
/// drain below this before each query, so a long journal never piles up in memory.
const REPLAY_PAGE: usize = 256;

/// Messages read from a client ahead of the one being handled; with that many waiting, its
/// socket is left unread until the daemon catches up.
const WAITING: usize = 16;

/// How long a client's socket may be idle before the kernel asks the peer whether it is
/// still there, and how often it asks again; [`KEEPALIVE_PROBES`] unanswered close it.
const KEEPALIVE_IDLE: Duration = Duration::from_secs(30);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
const KEEPALIVE_PROBES: u32 = 3;

/// How long what the daemon sent may stay unacknowledged before the kernel closes the socket.
/// Left to itself it retransmits for a quarter of an hour, and the daemon goes on serving a
/// client that went to sleep or left its network for as long. Only Linux has the limit.
#[cfg(target_os = "linux")]
const UNACKNOWLEDGED: Duration = Duration::from_secs(60);

/// Most bytes of one message the daemon reads off a socket. It is well over
/// [`MAX_MESSAGE_BYTES`], so that a client sending a message over that limit is told which
/// command was refused and keeps its connection; a message over this one closes it.
const READ_LIMIT: usize = 4 * MAX_MESSAGE_BYTES;

/// Most bytes of JSON the writer gathers from what is queued into one compressed frame. Only
/// what is already queued goes in, so no message waits for a frame to fill; and one frame
/// holds back what is queued behind it for only as long as these bytes take to send.
const BATCH_BYTES: usize = 64 * 1024;

/// A lone message of fewer bytes goes as a text frame: compression gains little on it.
const DEFLATE_FROM: usize = 512;

/// Serves a connection accepted on a listen address, or, with `tunneled`, on the tunnel
/// listener, where it first takes `cloudflared`'s WebSocket upgrade.
pub(super) async fn run<B: Backend>(
    stream: TcpStream,
    peer: SocketAddr,
    tunneled: bool,
    shared: Arc<Shared<B>>,
    cancel: CancellationToken,
) {
    if let Err(err) = stream.set_nodelay(true) {
        debug!(%peer, "cannot disable Nagle's algorithm: {err}");
    }
    if let Err(err) = notice_a_gone_peer(&stream) {
        debug!(%peer, "cannot set the socket's keepalive: {err}");
    }
    #[cfg(target_os = "linux")]
    if let Err(err) = hold_back_unsent(&stream) {
        debug!(%peer, "cannot limit the socket's unsent bytes: {err}");
    }
    let ws = tokio::select! {
        () = cancel.cancelled() => return,
        ws = tokio::time::timeout(
            HANDSHAKE_TIMEOUT,
            open(&shared.tls, &shared.tunnel_gate, stream, peer, tunneled),
        ) => ws,
    };
    let (ws, device, pass) = match ws {
        Ok(Ok(ws)) => ws,
        Ok(Err(err)) => {
            info!(%peer, "handshake failed: {err:#}");
            return;
        }
        Err(_) => {
            info!(%peer, "handshake timed out");
            return;
        }
    };
    serve(ws, device, None, pass, peer, shared, cancel).await;
}

/// Serves a client that reached this host through its hub, on the relay stream `stream`
/// the hub at `peer` forwards: the client's own TLS, upgrade and hello run inside it, so it
/// is authenticated and served exactly as on a direct connection. `device` is the hub's
/// name for the client's device, and only counts it among those not signed in here yet.
pub(super) async fn run_relayed<B: Backend>(
    stream: Box<dyn tunnel::Io>,
    peer: SocketAddr,
    device: &DeviceId,
    shared: Arc<Shared<B>>,
    cancel: CancellationToken,
) {
    let Some(pass) = shared
        .tunnel_gate
        .enter(Some(&format!("hub device {device}")))
    else {
        warn!(%peer, %device, "refused a relayed connection: too many are not signed in yet");
        return;
    };
    let ws = tokio::select! {
        () = cancel.cancelled() => return,
        ws = tokio::time::timeout(
            HANDSHAKE_TIMEOUT,
            handshake(&shared.tls, Transport::Relay(stream)),
        ) => ws,
    };
    let (ws, device) = match ws {
        Ok(Ok(ws)) => ws,
        Ok(Err(err)) => return info!(%peer, "relayed handshake failed: {err:#}"),
        Err(_) => return info!(%peer, "relayed handshake timed out"),
    };
    serve(ws, device, None, Some(pass), peer, shared, cancel).await;
}

/// The handshakes of a connection: on a tunneled one, the upgrade from `cloudflared` first.
/// A tunneled connection also gets its pass through the tunnel's gate.
async fn open(
    tls: &Tls,
    gate: &Arc<tunnel::Gate>,
    stream: TcpStream,
    peer: SocketAddr,
    tunneled: bool,
) -> Result<(Ws, String, Option<tunnel::Pass>)> {
    let (transport, pass) = if tunneled {
        let (frames, pass, client) = tunnel::accept(stream, gate)
            .await
            .context("the tunnel's WebSocket upgrade")?;
        debug!(%peer, client = client.as_deref().unwrap_or("unknown"), "tunnel opened");
        (Transport::Tunnel(Box::new(frames)), Some(pass))
    } else {
        (Transport::Tcp(stream), None)
    };
    let (ws, device) = handshake(tls, transport).await?;
    Ok((ws, device, pass))
}

/// Has the kernel close `stream` once its peer is gone: one that stopped answering, with
/// the socket idle or with data on its way, without a word. The error then ends the
/// connection's reader or writer, and with it what the daemon holds and sends for the client.
pub(crate) fn notice_a_gone_peer(stream: &TcpStream) -> std::io::Result<()> {
    let socket = socket2::SockRef::from(stream);
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(KEEPALIVE_IDLE)
        .with_interval(KEEPALIVE_INTERVAL)
        .with_retries(KEEPALIVE_PROBES);
    socket.set_tcp_keepalive(&keepalive)?;
    // Where the kernel has no such limit, keepalive still finds the peer of an idle socket.
    #[cfg(target_os = "linux")]
    socket.set_tcp_user_timeout(Some(UNACKNOWLEDGED))?;
    Ok(())
}

/// Keeps what waits for a slow client in its [`Outbox`] rather than in the kernel: a socket
/// takes another message only once less than [`UNSENT`] bytes of it wait to go out. Else a
/// replay of megabytes fills the socket's buffer at once, and an answer the client waits on
/// to show anything, which the outbox puts first, waits behind all of it.
#[cfg(target_os = "linux")]
pub(crate) fn hold_back_unsent(stream: &TcpStream) -> std::io::Result<()> {
    socket2::SockRef::from(stream).set_tcp_notsent_lowat(UNSENT)
}

/// Unsent bytes a client's socket holds at most before it takes another message.
#[cfg(target_os = "linux")]
const UNSENT: u32 = 16 * 1024;

/// The client a connection serves, known once its hello is accepted.
struct Who {
    client: String,
    device_id: DeviceId,
}

/// Serves a connection whose handshakes are done; `first` is its first text frame when the
/// caller already read it, and `pass` counts a tunneled one until it authenticated. Logs why
/// the connection ended.
pub(super) async fn serve<B: Backend>(
    ws: Ws,
    device: String,
    first: Option<String>,
    pass: Option<tunnel::Pass>,
    peer: SocketAddr,
    shared: Arc<Shared<B>>,
    cancel: CancellationToken,
) {
    let started = Instant::now();
    let (sink, stream) = ws.split();
    let outbox = Arc::new(Outbox::default());
    // Set once both hellos said `deflate`: then the writer compresses, and the reader inflates.
    let deflate = Arc::new(AtomicBool::new(false));
    let writer = tokio::spawn(write(
        sink,
        Arc::clone(&outbox),
        Arc::clone(&shared.auth),
        Arc::clone(&deflate),
        cancel.clone(),
    ));
    let who = OnceLock::new();
    // `None` when the writer or a shutdown ended the connection under the reader.
    let ended = tokio::select! {
        () = cancel.cancelled() => None,
        result = read(
            stream,
            (first, pass),
            &shared,
            (&outbox, &deflate),
            (peer, &device),
            &cancel,
            &who,
        ) => {
            Some(result)
        }
    };
    // Nothing of the message was read, so there is no command to refuse: the client is told
    // why before the connection closes.
    if let Some(Err(err)) = &ended
        && matches!(
            err.downcast_ref::<tungstenite::Error>(),
            Some(tungstenite::Error::Capacity(_))
        )
    {
        warn!(%peer, "closing a connection for a message over the read limit: {err:#}");
        error(
            &outbox,
            ErrorCode::BadRequest,
            &format!("message too large: at most {MAX_MESSAGE_BYTES} bytes"),
        );
    }
    shared.fanout.disconnect(&outbox);
    shared.approvals.disconnect(&outbox);
    shared.terminals.disconnect(&outbox);
    // What the client had not been sent when its connection ended: a number that stays high
    // across a client's reconnects is a link too slow for what it is sent.
    let unsent = outbox.queued();
    outbox.finish();
    let wrote = writer.await.ok().flatten();
    let reason = match ended {
        Some(Ok(())) => "the client closed the connection".to_owned(),
        Some(Err(err)) => format!("{err:#}"),
        None => wrote.unwrap_or_else(|| "the daemon is shutting down".to_owned()),
    };
    let lasted = started.elapsed();
    // At info, with the connects: a client that keeps reconnecting shows here why.
    match who.get() {
        Some(Who { client, device_id }) => {
            info!(
                %peer,
                %client,
                %device_id,
                unsent,
                "client disconnected after {lasted:.1?}: {reason}"
            );
        }
        None => info!(%peer, "connection closed before its hello was accepted: {reason}"),
    }
}

/// The TLS and WebSocket handshakes; returns the fingerprint of the peer's device certificate.
pub(crate) async fn handshake(tls: &Tls, stream: Transport) -> Result<(Ws, String)> {
    let (tls, device) = secure(tls, stream).await?;
    let ws = tokio_tungstenite::accept_async_with_config(tls, Some(ws_config())).await?;
    Ok((ws, device))
}

/// The TLS handshake; returns the fingerprint of the peer's device certificate.
pub(crate) async fn secure(
    tls: &Tls,
    stream: Transport,
) -> Result<(tokio_rustls::server::TlsStream<Transport>, String)> {
    let tls = tls.acceptor().accept(stream).await?;
    // The verifier makes a client certificate mandatory; this only guards against a change there.
    let device = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|certs| certs.first())
        .map(|cert| fingerprint(cert))
        .context("the peer sent no device certificate")?;
    Ok((tls, device))
}

/// The WebSocket config of a connection to a daemon or a hub.
pub(crate) fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(READ_LIMIT))
        .max_frame_size(Some(READ_LIMIT))
}

/// Handles the messages of the client at `peer`, whose certificate is `device`'s, until it
/// closes the connection.
async fn read<B: Backend>(
    mut stream: SplitStream<Ws>,
    (first, pass): (Option<String>, Option<tunnel::Pass>),
    shared: &Arc<Shared<B>>,
    (outbox, deflate): (&Arc<Outbox>, &AtomicBool),
    (peer, device): (SocketAddr, &str),
    cancel: &CancellationToken,
    who: &OnceLock<Who>,
) -> Result<()> {
    let first = match first {
        Some(text) => Some(decode(&text)),
        None => tokio::time::timeout(HANDSHAKE_TIMEOUT, next(&mut stream, false))
            .await
            .map_err(|_| anyhow!("no hello within {HANDSHAKE_TIMEOUT:?}"))??,
    };
    let hello = match first {
        Some(Ok(ClientMessage::Hello(hello))) => hello,
        Some(Ok(_)) => return reject(outbox, "the first message must be a hello"),
        Some(Err(err)) => return reject(outbox, &err.message),
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
    let identity = if hello.sign_in && !shared.auth.is_paired(device) {
        sign_in(&mut stream, shared, outbox, device, &hello.client, cancel).await
    } else {
        shared
            .auth
            .authenticate(
                device,
                hello.pairing_code.as_deref(),
                &hello.client,
                auth::DeviceRole::Client,
                cancel,
            )
            .map_err(auth::SignInRefused::from)
    };
    let identity = match identity {
        Ok(identity) => identity,
        Err(auth::SignInRefused { error, refusal }) => {
            outbox.push(ServerMessage::Error { error, refusal });
            bail!("device {device} refused");
        }
    };
    // Authenticated: no longer one of the strangers the tunnel's gate counts.
    drop(pass);
    info!(
        %peer,
        client = %hello.client,
        user_id = %identity.user_id,
        device_id = %identity.device_id,
        resumes = hello.resume.len(),
        "client connected"
    );
    // Set before the hello is queued: the writer sends the hello itself as text either way.
    deflate.store(hello.deflate, Ordering::Relaxed);
    // Set once: this is the connection's only accepted hello.
    let _ = who.set(Who {
        client: hello.client.clone(),
        device_id: identity.device_id.clone(),
    });
    outbox.push(ServerMessage::Hello(ServerHello {
        protocol_version: PROTOCOL_VERSION,
        host_id: shared.host.id.clone(),
        host_name: shared.host.name.clone(),
        user_id: identity.user_id.clone(),
        device_id: identity.device_id.clone(),
        role: identity.role,
        person_id: identity.person_id.clone(),
        herder_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        addresses: shared.advertised().unwrap_or_default(),
        deflate: true,
    }));
    shared.fanout.connect(outbox, identity.role);
    let person = shared.auth.resolve(&identity.user_id);
    shared.approvals.connect(&person, outbox, hello.passkey);
    match shared.backend.sessions().await {
        Ok(sessions) => shared.fanout.initial_sessions(outbox, sessions),
        Err(err) => {
            warn!("cannot list sessions: {err:#}");
            error(outbox, ErrorCode::Internal, "cannot list sessions");
        }
    }
    shared
        .fanout
        .initial_accounts(outbox, shared.backend.accounts());
    shared.fanout.initial_providers(
        outbox,
        shared
            .providers
            .get()
            .map(|providers| providers.snapshot())
            .unwrap_or_default(),
    );
    shared.backend.refresh_usage();
    shared.backend.refresh_skills();
    shared
        .fanout
        .initial_terminals(outbox, shared.terminals.list());
    shared.fanout.initial_projects(outbox);
    // The client's messages wait here for their turn while the one before them is handled:
    // the socket is read, and so a ping answered, whatever the daemon is busy with. A client
    // takes a connection that does not answer its ping for dead and replaces it.
    let mut resume = hello
        .resume
        .iter()
        .cloned()
        .map(|cursor| Ok(ClientMessage::Subscribe(cursor)));
    // Replays run beside the messages, so a long one waiting on a slow client never holds up
    // the commands sent after it. The task ends with the connection: once its outbox closes.
    let (replays, queue) = mpsc::unbounded_channel();
    tokio::spawn(replay(Arc::clone(shared), Arc::clone(outbox), queue));
    let mut waiting = VecDeque::new();
    let mut handling = None;
    // How reading ended, once it did; what was read by then is still handled.
    let mut ended = None;
    loop {
        if handling.is_none() {
            match resume.next().or_else(|| waiting.pop_front()) {
                Some(message) => {
                    handling =
                        Some(handle(shared, outbox, &replays, &identity, &hello, message).boxed());
                }
                None => {
                    if let Some(ended) = ended.take() {
                        return ended;
                    }
                }
            }
        }
        let handled = async {
            match handling.as_mut() {
                Some(handling) => handling.await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            () = handled => handling = None,
            message = next(&mut stream, hello.deflate), if ended.is_none() && waiting.len() < WAITING => {
                match message {
                    Ok(Some(message)) => waiting.push_back(message),
                    Ok(None) => ended = Some(Ok(())),
                    Err(err) => ended = Some(Err(err)),
                }
            }
        }
    }
}

/// Handles one message of a client that said hello.
async fn handle<B: Backend>(
    shared: &Arc<Shared<B>>,
    outbox: &Arc<Outbox>,
    replays: &mpsc::UnboundedSender<Replay>,
    identity: &Identity,
    hello: &ClientHello,
    message: Result<ClientMessage, Undecodable>,
) {
    match message {
        Ok(ClientMessage::Hello(_)) => error(outbox, ErrorCode::BadRequest, "already said hello"),
        Ok(ClientMessage::SignIn(_)) => error(outbox, ErrorCode::BadRequest, "already signed in"),
        Ok(ClientMessage::Subscribe(cursor)) => {
            let _ = replays.send(Replay::Subscribe(cursor));
        }
        Ok(ClientMessage::Unsubscribe { session_id }) => {
            let _ = replays.send(Replay::Unsubscribe(session_id));
        }
        Ok(ClientMessage::Sync { token }) => {
            let _ = replays.send(Replay::Sync(token));
        }
        Ok(ClientMessage::Command(Command { id, body })) => {
            if let Err(error) = auth::authorize(identity.role, &body) {
                outbox.push(ServerMessage::CommandRejected {
                    command_id: id,
                    error,
                });
                return;
            }
            if let CommandBody::GetLatestEvents { session_id } = &body {
                // Ahead of the replays queued: it is what an app shows first.
                outbox.push_first(answered(id, latest_events(shared, session_id).await));
                return;
            }
            let waiting = match shared.approval(identity, &hello.client, hello.passkey, &body) {
                Ok(waiting) => waiting,
                Err(error) => {
                    outbox.push(answered(id, Err(error)));
                    return;
                }
            };
            let key = (identity.user_id.clone(), id.clone());
            let query = session::changes_nothing(&body);
            let apply = {
                let shared = Arc::clone(shared);
                let identity = identity.clone();
                let outbox = Arc::clone(outbox);
                let id = id.clone();
                let approved = waiting.is_some();
                async move { shared.apply(&identity, &outbox, &id, body, approved).await }
            };
            if let Some(waiting) = waiting {
                // The device waits for its person without holding up its other messages.
                let shared = Arc::clone(shared);
                let outbox = Arc::clone(outbox);
                tokio::spawn(async move {
                    let answer = match shared.approvals.wait(waiting).await {
                        Ok(()) => shared.commands.apply(key, apply).await,
                        Err(error) => Err(error),
                    };
                    outbox.push(answered(id, answer));
                });
                return;
            }
            // A query's answer is not kept: it may be large, and asking again is harmless.
            let answer = if query {
                apply.await
            } else {
                shared.commands.apply(key, apply).await
            };
            outbox.push(answered(id, answer));
        }
        Err(Undecodable {
            command_id,
            message,
            too_large,
        }) => {
            if too_large {
                warn!(
                    client = %hello.client,
                    device_id = %identity.device_id,
                    command_id = ?command_id,
                    "refused a client message: {message}"
                );
            }
            match command_id {
                // A command this build cannot read, such as one from a newer client, or
                // will not, is refused as that command, so the client does not wait for
                // an answer forever.
                Some(command_id) => outbox.push(ServerMessage::CommandRejected {
                    command_id,
                    error: ErrorInfo {
                        code: ErrorCode::BadRequest,
                        message,
                    },
                }),
                None => error(outbox, ErrorCode::BadRequest, &message),
            }
        }
    }
}

/// A frame that is not a client message: why, and the id of the command it carries when it
/// still reads as one.
struct Undecodable {
    command_id: Option<CommandId>,
    message: String,
    /// Whether it is a frame over [`MAX_MESSAGE_BYTES`], refused whatever it holds.
    too_large: bool,
}

impl Undecodable {
    fn new(message: String) -> Self {
        Self {
            command_id: None,
            message,
            too_large: false,
        }
    }
}

/// The next client message: `Some(Err)` describes a frame that is not one. With `deflate`,
/// the client may send a message compressed in a binary frame.
async fn next(
    stream: &mut SplitStream<Ws>,
    deflate: bool,
) -> Result<Option<Result<ClientMessage, Undecodable>>> {
    loop {
        let Some(frame) = stream.next().await else {
            return Ok(None);
        };
        let text = match frame.context("reading from the client")? {
            Message::Text(text) => text.as_str().to_owned(),
            Message::Binary(data) if deflate => {
                match herder_protocol::deflate::inflate(&data, READ_LIMIT) {
                    Ok(text) => text,
                    Err(err) => {
                        return Ok(Some(Err(Undecodable::new(format!(
                            "invalid compressed message: {err}"
                        )))));
                    }
                }
            }
            Message::Binary(_) => {
                return Ok(Some(Err(Undecodable::new(
                    "messages must be JSON text frames".to_owned(),
                ))));
            }
            // A client that gives its connection up says why, which the close log then shows.
            Message::Close(Some(frame)) if !frame.reason.is_empty() => {
                bail!(
                    "the client closed the connection: {:?}",
                    frame.reason.as_str()
                )
            }
            Message::Close(_) => return Ok(None),
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
        };
        if text.len() > MAX_MESSAGE_BYTES {
            return Ok(Some(Err(Undecodable {
                command_id: command_id(&text),
                message: format!(
                    "message too large: {} bytes, and one may have at most {MAX_MESSAGE_BYTES}",
                    text.len()
                ),
                too_large: true,
            })));
        }
        return Ok(Some(decode(&text)));
    }
}

fn decode(text: &str) -> Result<ClientMessage, Undecodable> {
    serde_json::from_str(text).map_err(|err| Undecodable {
        command_id: command_id(text),
        message: format!("invalid message: {err}"),
        too_large: false,
    })
}

/// The id of the command `text` is, read without its body.
fn command_id(text: &str) -> Option<CommandId> {
    #[derive(Deserialize)]
    struct Tagged {
        r#type: String,
        id: CommandId,
    }
    serde_json::from_str::<Tagged>(text)
        .ok()
        .filter(|tagged| tagged.r#type == "command")
        .map(|tagged| tagged.id)
}

/// A subscription change or sync, done in the order the client sent them.
enum Replay {
    Subscribe(Cursor),
    Unsubscribe(SessionId),
    /// Answered once every replay asked for before it is queued.
    Sync(String),
}

/// Does the connection's subscription changes and syncs, one after the other.
async fn replay<B: Backend>(
    shared: Arc<Shared<B>>,
    outbox: Arc<Outbox>,
    mut queue: mpsc::UnboundedReceiver<Replay>,
) {
    while let Some(next) = queue.recv().await {
        if outbox.state() != OutboxState::Open {
            return;
        }
        match next {
            Replay::Subscribe(cursor) => subscribe(&shared, &outbox, cursor).await,
            Replay::Unsubscribe(session_id) => shared.fanout.unsubscribe(&outbox, &session_id),
            Replay::Sync(token) => outbox.push(ServerMessage::Synced { token }),
        }
    }
}

/// Replays the session's journal after the cursor, then switches the subscription to live.
///
/// The fanout holds live events back from the moment of subscribing, so an event appended
/// during replay arrives either from the journal or from the fanout, and duplicates are dropped
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
    shared.fanout.subscribe(outbox, &session_id);
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
                shared.fanout.unsubscribe(outbox, &session_id);
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
    shared.fanout.go_live(outbox, &session_id, last);
}

/// The session's latest events, as `get_latest_events` answers: read from the journal like a
/// replay, so a hub answers with its copy.
async fn latest_events<B: Backend>(
    shared: &Shared<B>,
    session_id: &SessionId,
) -> Result<CommandResult, ErrorInfo> {
    let internal = |message: &str| ErrorInfo {
        code: ErrorCode::Internal,
        message: message.to_owned(),
    };
    let sessions = shared.backend.sessions().await.map_err(|err| {
        warn!("cannot list sessions: {err:#}");
        internal("cannot list sessions")
    })?;
    let Some(head) = sessions.iter().find(|s| s.session_id == *session_id) else {
        return Err(ErrorInfo {
            code: ErrorCode::NotFound,
            message: format!("session {session_id} does not exist"),
        });
    };
    let after = head.head_seq.saturating_sub(LATEST_EVENTS as Seq);
    let page = shared
        .backend
        .read_since(session_id, after, LATEST_EVENTS)
        .await
        .map_err(|err| {
            warn!(%session_id, "cannot read the journal: {err:#}");
            internal("cannot read the journal")
        })?;
    let mut events = Vec::new();
    let mut bytes = 0;
    for event in page.into_iter().rev() {
        // Bodies from a newer build cannot be sent.
        if matches!(event.body, EventBody::Unknown) {
            continue;
        }
        bytes += serde_json::to_vec(&event).map_or(0, |json| json.len());
        if bytes > LATEST_EVENTS_BYTES && !events.is_empty() {
            break;
        }
        events.push(event);
    }
    events.reverse();
    Ok(CommandResult::LatestEvents { events })
}

fn error(outbox: &Outbox, code: ErrorCode, message: &str) {
    outbox.push(ServerMessage::Error {
        error: ErrorInfo {
            code,
            message: message.to_owned(),
        },
        refusal: None,
    });
}

/// Signs in a device the daemon does not know as a person: sends the challenge, then follows
/// the device's `sign_in`, waiting for an app's approval for `start_login`.
async fn sign_in<B: Backend>(
    stream: &mut SplitStream<Ws>,
    shared: &Arc<Shared<B>>,
    outbox: &Arc<Outbox>,
    device: &str,
    client: &str,
    cancel: &CancellationToken,
) -> Result<Identity, auth::SignInRefused> {
    let (challenge, registration) = shared.auth.sign_in_challenge()?;
    outbox.push(ServerMessage::SignInChallenge {
        challenge,
        registration,
    });
    let refused = |message: &str| {
        auth::SignInRefused::from(ErrorInfo {
            code: ErrorCode::BadRequest,
            message: message.to_owned(),
        })
    };
    // A person gets the challenge's lifetime to answer it with their passkey.
    let answer = tokio::time::timeout(
        Duration::from_secs(PASSKEY_CHALLENGE_SECS),
        next(stream, false),
    )
    .await
    .map_err(|_| refused("no sign_in before the challenge expired"))?
    .map_err(|_| refused("the connection failed"))?;
    let method = match answer {
        Some(Ok(ClientMessage::SignIn(method))) => method,
        Some(Ok(_)) => return Err(refused("the answer to sign_in_challenge must be sign_in")),
        Some(Err(err)) => return Err(refused(&err.message)),
        None => return Err(refused("the connection closed")),
    };
    if method != SignIn::StartLogin {
        return shared.auth.sign_in(device, client, method, cancel);
    }
    let request = shared.auth.start_login(device, client)?;
    outbox.push(ServerMessage::LoginRequest {
        code: request.code,
        expires_at: request.expires_at,
    });
    let expired = || ErrorInfo {
        code: ErrorCode::Forbidden,
        message: "no app approved the login in time; run it again".to_owned(),
    };
    tokio::select! {
        approved = tokio::time::timeout(auth::PAIRING_TTL, request.approved) => match approved {
            Ok(Ok(())) => {}
            _ => return Err(expired().into()),
        },
        // The device says nothing while it waits; anything it sends ends the wait.
        _ = next(stream, false) => return Err(refused("the device stopped waiting for its login")),
    }
    Ok(shared
        .auth
        .authenticate(device, None, client, auth::DeviceRole::Client, cancel)?)
}

/// The answer to command `command_id`.
fn answered(command_id: CommandId, answer: Result<CommandResult, ErrorInfo>) -> ServerMessage {
    match answer {
        Ok(result) => ServerMessage::CommandAccepted { command_id, result },
        Err(error) => ServerMessage::CommandRejected { command_id, error },
    }
}

/// Answers a failed hello with an error; the connection then closes.
fn reject(outbox: &Outbox, message: &str) -> Result<()> {
    error(outbox, ErrorCode::BadRequest, message);
    bail!("hello refused: {message}")
}

/// Writes queued messages, attributed as `auth` resolves ids, until the outbox stops, the
/// socket fails, or `cancel`. Returns why it ended the connection, when it was not the outbox
/// finishing.
async fn write(
    mut sink: SplitSink<Ws, Message>,
    outbox: Arc<Outbox>,
    auth: Arc<Auth>,
    deflate: Arc<AtomicBool>,
    cancel: CancellationToken,
) -> Option<String> {
    let encode = |mut message: ServerMessage| {
        auth.attribute(&mut message);
        serde_json::to_string(&message)
            .inspect_err(|err| warn!("cannot encode a message for a client: {err}"))
            .ok()
    };
    let close = loop {
        while let Some(message) = outbox.pop() {
            // The hello goes by itself as text, so a client reads it as it reads any daemon's.
            let hello = matches!(message, ServerMessage::Hello(_));
            let Some(text) = encode(message) else {
                continue;
            };
            let frame = if hello || !deflate.load(Ordering::Relaxed) {
                Message::text(text)
            } else {
                let mut texts = vec![text];
                let mut bytes = texts[0].len();
                while bytes < BATCH_BYTES
                    && let Some(message) = outbox.pop()
                {
                    if let Some(text) = encode(message) {
                        bytes += text.len();
                        texts.push(text);
                    }
                }
                if texts.len() == 1 && bytes < DEFLATE_FROM {
                    Message::text(texts.swap_remove(0))
                } else {
                    // Off the runtime's threads: one large event takes a while to compress.
                    let deflated = tokio::task::spawn_blocking(move || {
                        herder_protocol::deflate::deflate(texts.iter().map(String::as_str))
                    });
                    match deflated.await {
                        Ok(frame) => Message::binary(frame),
                        Err(err) => {
                            warn!("cannot compress messages for a client: {err}");
                            continue;
                        }
                    }
                }
            };
            let sent = tokio::select! {
                () = cancel.cancelled() => None,
                sent = sink.send(frame) => Some(sent),
            };
            match sent {
                None => break,
                Some(Ok(())) => {}
                Some(Err(err)) => {
                    cancel.cancel();
                    return Some(format!("writing to the client: {err}"));
                }
            }
        }
        if cancel.is_cancelled() {
            break (
                CloseCode::Away,
                "herder daemon is shutting down",
                Some("the daemon is shutting down"),
            );
        }
        match outbox.state() {
            OutboxState::Open => {}
            OutboxState::Finished if outbox.is_empty() => break (CloseCode::Normal, "", None),
            OutboxState::Finished => continue,
            OutboxState::Overflowed => {
                break (
                    CloseCode::Again,
                    "too far behind; reconnect and resume from your cursor",
                    Some("the client fell too far behind what it was sent"),
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
    close.2.map(str::to_owned)
}
