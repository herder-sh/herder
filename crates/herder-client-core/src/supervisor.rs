//! One task per machine owns its connection: it connects, resumes every wanted session from the
//! cache's cursor, feeds what the daemon sends into the cache, dispatches commands, and on any
//! failure waits out a capped, jittered exponential backoff before trying again.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use herder_protocol::{
    ClientHello, ClientMessage, Command, CommandId, CommandResult, Cursor, ErrorInfo,
    PROTOCOL_VERSION, Role, ServerHello, ServerMessage, SessionId,
};
use ring::rand::{SecureRandom, SystemRandom};
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio::time::Instant;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::auth::{DeviceKey, client_config};
use crate::cache::SessionLog;
use crate::profile::SavedMachine;
use crate::{ConnectionState, Error, Machine, SessionUpdate};

/// Time one address gets for TCP, TLS, the WebSocket upgrade and the hellos.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// First retry delay; each failed attempt doubles it, up to [`BACKOFF_CAP`].
const BACKOFF_BASE: Duration = Duration::from_millis(250);

/// Longest wait between attempts.
const BACKOFF_CAP: Duration = Duration::from_secs(30);

/// How often an idle connection is pinged.
const PING_INTERVAL: Duration = Duration::from_secs(15);

/// Silence after which a connection is considered dead, though TCP has not noticed yet.
const SILENCE_LIMIT: Duration = Duration::from_secs(45);

type Ws = WebSocketStream<TlsStream<TcpStream>>;

/// Answer to a command, as the daemon gave it.
type Answer = Result<CommandResult, ErrorInfo>;

/// What the client asks of a machine's supervisor.
enum Op {
    /// A session gained its first subscriber.
    Subscribe(SessionId),
    /// A session lost its last subscriber.
    Unsubscribe(SessionId),
    /// Send a command and report the daemon's answer.
    Command(Command, oneshot::Sender<Answer>),
}

/// A machine's supervisor: the state its task maintains, and the handle the client drives it
/// through.
pub(crate) struct Supervisor {
    pub(crate) saved: SavedMachine,
    state: Mutex<State>,
    /// Bumped whenever [`Supervisor::view`] would change.
    changed: Arc<watch::Sender<u64>>,
    ops: mpsc::UnboundedSender<Op>,
    wake: Notify,
    stop: CancellationToken,
}

#[derive(Default)]
struct State {
    connection: Option<ConnectionState>,
    role: Option<Role>,
    sessions: Vec<herder_protocol::SessionHead>,
    accounts: Vec<herder_protocol::Account>,
    terminals: Vec<herder_protocol::Terminal>,
    logs: HashMap<SessionId, Log>,
    /// Subscribers per session; the daemon streams the sessions with at least one.
    wanted: HashMap<SessionId, usize>,
}

struct Log {
    log: SessionLog,
    /// Bumped on every change to `log`.
    changed: watch::Sender<u64>,
}

impl Default for Log {
    fn default() -> Self {
        Self {
            log: SessionLog::default(),
            changed: watch::Sender::new(0),
        }
    }
}

impl Supervisor {
    /// Starts the task that keeps `saved` connected until `stop`.
    pub(crate) fn start(
        saved: SavedMachine,
        client: String,
        changed: Arc<watch::Sender<u64>>,
        stop: CancellationToken,
    ) -> Result<Arc<Self>, Error> {
        let device = DeviceKey::from_pem(&saved.device_key)
            .map_err(|err| Error::Local(format!("the device key of {}: {err:#}", saved.name)))?;
        let (ops, queue) = mpsc::unbounded_channel();
        let supervisor = Arc::new(Self {
            saved,
            state: Mutex::default(),
            changed,
            ops,
            wake: Notify::new(),
            stop,
        });
        tokio::spawn(run(Arc::clone(&supervisor), device, client, queue));
        Ok(supervisor)
    }

    /// Stops the task; subscriptions end.
    pub(crate) fn stop(&self) {
        self.stop.cancel();
    }

    /// Skips the current backoff wait and reconnects now.
    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }

    pub(crate) fn view(&self) -> Machine {
        let state = self.lock();
        Machine {
            host_id: self.saved.host_id.clone(),
            name: self.saved.name.clone(),
            addresses: self.saved.addresses.clone(),
            fingerprint: self.saved.fingerprint.clone(),
            connection: state
                .connection
                .clone()
                .unwrap_or(ConnectionState::Connecting),
            role: state.role,
            sessions: state.sessions.clone(),
            accounts: state.accounts.clone(),
            terminals: state.terminals.clone(),
        }
    }

    /// Sends `command` once a connection is up, resending it with the same id after each
    /// reconnect until the daemon answers.
    pub(crate) async fn send(&self, command: Command) -> Result<Answer, Error> {
        let (reply, answer) = oneshot::channel();
        self.ops
            .send(Op::Command(command, reply))
            .map_err(|_| Error::Closed)?;
        tokio::select! {
            () = self.stop.cancelled() => Err(Error::Closed),
            answer = answer => answer.map_err(|_| Error::Closed),
        }
    }

    /// Adds a subscriber to `session_id`: the change counter to watch, and the stop token.
    fn add_subscriber(&self, session_id: &SessionId) -> watch::Receiver<u64> {
        let mut state = self.lock();
        let count = state.wanted.entry(session_id.clone()).or_default();
        *count += 1;
        if *count == 1 {
            // A stopped supervisor no longer reads ops; its subscriptions end on `stop`.
            let _ = self.ops.send(Op::Subscribe(session_id.clone()));
        }
        state
            .logs
            .entry(session_id.clone())
            .or_default()
            .changed
            .subscribe()
    }

    fn remove_subscriber(&self, session_id: &SessionId) {
        let mut state = self.lock();
        let Some(count) = state.wanted.get_mut(session_id) else {
            return;
        };
        *count -= 1;
        if *count == 0 {
            state.wanted.remove(session_id);
            let _ = self.ops.send(Op::Unsubscribe(session_id.clone()));
        }
    }

    /// What a subscriber that holds events up to `after` has not seen yet.
    fn update(&self, session_id: &SessionId, after: herder_protocol::Seq) -> SessionUpdate {
        let state = self.lock();
        let log = state.logs.get(session_id).map(|log| &log.log);
        SessionUpdate {
            events: log.map(|log| log.events_after(after)).unwrap_or_default(),
            streaming: log.map(SessionLog::streaming).unwrap_or_default(),
        }
    }

    fn set_connection(&self, connection: ConnectionState) {
        self.lock().connection = Some(connection);
        self.notify();
    }

    fn notify(&self) {
        self.changed.send_modify(|version| *version += 1);
    }

    /// Where each wanted session resumes.
    fn cursors(&self) -> Vec<Cursor> {
        let state = self.lock();
        state
            .wanted
            .keys()
            .map(|session_id| cursor(&state, session_id))
            .collect()
    }

    /// Applies one daemon message to the state.
    fn apply(&self, message: ServerMessage) {
        let mut state = self.lock();
        let log = match message {
            ServerMessage::Sessions { sessions } => {
                state.sessions = sessions;
                return self.notify_after(state);
            }
            ServerMessage::Accounts { accounts } => {
                state.accounts = accounts;
                return self.notify_after(state);
            }
            ServerMessage::Terminals { terminals } => {
                state.terminals = terminals;
                return self.notify_after(state);
            }
            ServerMessage::Event(event) => {
                let log = state.logs.entry(event.session_id.clone()).or_default();
                if !log.log.event(event) {
                    return;
                }
                log
            }
            ServerMessage::Snapshot { session_id, item } => {
                let log = state.logs.entry(session_id).or_default();
                log.log.snapshot(item);
                log
            }
            ServerMessage::Delta {
                session_id,
                item_id,
                text,
            } => {
                let Some(log) = state.logs.get_mut(&session_id) else {
                    return;
                };
                if !log.log.delta(&item_id, &text) {
                    return;
                }
                log
            }
            ServerMessage::Error { error } => {
                let machine = &self.saved.name;
                return warn!(%machine, "the daemon reported an error: {}", error.message);
            }
            // Terminal output and exits have no consumer yet; hellos and answers are handled
            // by the connection.
            ServerMessage::TerminalOutput { .. }
            | ServerMessage::TerminalClosed { .. }
            | ServerMessage::Hello(_)
            | ServerMessage::CommandAccepted { .. }
            | ServerMessage::CommandRejected { .. }
            | ServerMessage::Unknown => return,
        };
        log.changed.send_modify(|version| *version += 1);
    }

    fn notify_after(&self, state: MutexGuard<'_, State>) {
        drop(state);
        self.notify();
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // Every update is a single assignment or insert, so a poisoned state is consistent.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn cursor(state: &State, session_id: &SessionId) -> Cursor {
    Cursor {
        session_id: session_id.clone(),
        after_seq: state
            .logs
            .get(session_id)
            .map_or(0, |log| log.log.last_seq()),
    }
}

/// A stream of one session's updates; see [`crate::SessionSubscription`].
pub(crate) struct Subscription {
    supervisor: Arc<Supervisor>,
    session_id: SessionId,
    reader: tokio::sync::Mutex<Reader>,
}

struct Reader {
    changed: watch::Receiver<u64>,
    /// Seq of the latest event handed out.
    delivered: herder_protocol::Seq,
    first: bool,
}

impl Subscription {
    pub(crate) fn new(supervisor: Arc<Supervisor>, session_id: SessionId) -> Self {
        let changed = supervisor.add_subscriber(&session_id);
        Self {
            supervisor,
            session_id,
            reader: tokio::sync::Mutex::new(Reader {
                changed,
                delivered: 0,
                first: true,
            }),
        }
    }

    pub(crate) async fn next(&self) -> Option<SessionUpdate> {
        let mut reader = self.reader.lock().await;
        if !reader.first {
            tokio::select! {
                () = self.supervisor.stop.cancelled() => return None,
                changed = reader.changed.changed() => changed.ok()?,
            }
        }
        if self.supervisor.stop.is_cancelled() {
            return None;
        }
        reader.first = false;
        let update = self.supervisor.update(&self.session_id, reader.delivered);
        if let Some(last) = update.events.last() {
            reader.delivered = last.seq;
        }
        Some(update)
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.supervisor.remove_subscriber(&self.session_id);
    }
}

/// A command waiting for its answer, resent on every new connection until it gets one.
struct Pending {
    command: Command,
    reply: oneshot::Sender<Answer>,
}

/// Why a connection ended.
enum Ended {
    Stopped,
    Lost(String),
}

async fn run(
    supervisor: Arc<Supervisor>,
    device: DeviceKey,
    client: String,
    mut ops: mpsc::UnboundedReceiver<Op>,
) {
    let mut pending: Vec<Pending> = Vec::new();
    let mut attempt = 0;
    let saved = &supervisor.saved;
    loop {
        supervisor.set_connection(ConnectionState::Connecting);
        let hello = ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client: client.clone(),
            resume: supervisor.cursors(),
            pairing_code: None,
        };
        let connected = tokio::select! {
            () = supervisor.stop.cancelled() => return,
            connected = connect(saved, &device, hello) => connected,
        };
        let error = match connected {
            Ok((ws, hello)) => {
                attempt = 0;
                supervisor.lock().role = Some(hello.role);
                supervisor.set_connection(ConnectionState::Connected);
                match serve(&supervisor, ws, &mut ops, &mut pending).await {
                    Ended::Stopped => return,
                    Ended::Lost(error) => error,
                }
            }
            Err(error) => error,
        };
        debug!(machine = %saved.name, "disconnected: {error}");
        supervisor.set_connection(ConnectionState::Disconnected { error });
        let wait = tokio::time::sleep(backoff(attempt));
        attempt = attempt.saturating_add(1);
        tokio::pin!(wait);
        loop {
            tokio::select! {
                () = supervisor.stop.cancelled() => return,
                () = &mut wait => break,
                () = supervisor.wake.notified() => {
                    attempt = 0;
                    break;
                }
                op = ops.recv() => match op {
                    Some(Op::Command(command, reply)) => pending.push(Pending { command, reply }),
                    // The next hello resumes whatever is wanted by then.
                    Some(Op::Subscribe(_) | Op::Unsubscribe(_)) => {}
                    None => return,
                },
            }
        }
    }
}

/// Runs a connection until it fails or `stop`.
async fn serve(
    supervisor: &Supervisor,
    ws: Ws,
    ops: &mut mpsc::UnboundedReceiver<Op>,
    pending: &mut Vec<Pending>,
) -> Ended {
    let (mut sink, mut stream) = ws.split();
    pending.retain(|command| !command.reply.is_closed());
    for command in pending.iter() {
        let message = ClientMessage::Command(command.command.clone());
        if let Err(error) = write(&mut sink, encode(&message)).await {
            return Ended::Lost(error);
        }
    }
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.reset();
    let mut heard = Instant::now();
    loop {
        let message = tokio::select! {
            () = supervisor.stop.cancelled() => return Ended::Stopped,
            _ = ping.tick() => {
                if heard.elapsed() > SILENCE_LIMIT {
                    return Ended::Lost("the daemon stopped answering".to_owned());
                }
                if let Err(error) = write(&mut sink, Message::Ping(Default::default())).await {
                    return Ended::Lost(error);
                }
                continue;
            }
            op = ops.recv() => {
                let message = match op {
                    None => return Ended::Stopped,
                    Some(Op::Subscribe(session_id)) => {
                        let state = supervisor.lock();
                        if !state.wanted.contains_key(&session_id) {
                            continue;
                        }
                        ClientMessage::Subscribe(cursor(&state, &session_id))
                    }
                    Some(Op::Unsubscribe(session_id)) => {
                        if supervisor.lock().wanted.contains_key(&session_id) {
                            continue;
                        }
                        ClientMessage::Unsubscribe { session_id }
                    }
                    Some(Op::Command(command, reply)) => {
                        let message = ClientMessage::Command(command.clone());
                        pending.push(Pending { command, reply });
                        message
                    }
                };
                if let Err(error) = write(&mut sink, encode(&message)).await {
                    return Ended::Lost(error);
                }
                continue;
            }
            frame = stream.next() => frame,
        };
        heard = Instant::now();
        let text = match message {
            None => return Ended::Lost("the daemon closed the connection".to_owned()),
            Some(Err(err)) => return Ended::Lost(format!("reading from the daemon: {err}")),
            Some(Ok(Message::Close(frame))) => {
                let reason = frame
                    .map(|frame| frame.reason.to_string())
                    .unwrap_or_default();
                return Ended::Lost(format!("the daemon closed the connection: {reason}"));
            }
            Some(Ok(Message::Text(text))) => text,
            Some(Ok(_)) => continue,
        };
        let message: ServerMessage = match serde_json::from_str(&text) {
            Ok(message) => message,
            Err(err) => {
                warn!(machine = %supervisor.saved.name, "skipping an invalid message: {err}");
                continue;
            }
        };
        match message {
            ServerMessage::CommandAccepted { command_id, result } => {
                answer(pending, &command_id, Ok(result));
            }
            ServerMessage::CommandRejected { command_id, error } => {
                answer(pending, &command_id, Err(error));
            }
            message => supervisor.apply(message),
        }
    }
}

fn encode(message: &ClientMessage) -> Message {
    // Client messages are plain data with string keys: encoding cannot fail.
    Message::text(serde_json::to_string(message).unwrap_or_default())
}

async fn write(sink: &mut SplitSink<Ws, Message>, message: Message) -> Result<(), String> {
    sink.send(message)
        .await
        .map_err(|err| format!("writing to the daemon: {err}"))
}

fn answer(pending: &mut Vec<Pending>, command_id: &CommandId, result: Answer) {
    if let Some(index) = pending.iter().position(|p| p.command.id == *command_id) {
        let _ = pending.swap_remove(index).reply.send(result);
    }
}

/// Connects to the first of `saved`'s addresses that answers, as `device`, and exchanges hellos.
pub(crate) async fn connect(
    saved: &SavedMachine,
    device: &DeviceKey,
    hello: ClientHello,
) -> Result<(Ws, ServerHello), String> {
    let config = client_config(&saved.fingerprint, device).map_err(|err| format!("{err:#}"))?;
    let connector = TlsConnector::from(Arc::new(config));
    let mut errors = Vec::new();
    for address in &saved.addresses {
        let attempt = connect_to(address, &connector, &hello);
        match tokio::time::timeout(CONNECT_TIMEOUT, attempt).await {
            Ok(Ok(connected)) => return Ok(connected),
            Ok(Err(err)) => errors.push(format!("{address}: {err:#}")),
            Err(_) => errors.push(format!("{address}: no answer in {CONNECT_TIMEOUT:?}")),
        }
    }
    if errors.is_empty() {
        errors.push("the machine has no address".to_owned());
    }
    Err(errors.join("; "))
}

async fn connect_to(
    address: &str,
    connector: &TlsConnector,
    hello: &ClientHello,
) -> anyhow::Result<(Ws, ServerHello)> {
    let tcp = TcpStream::connect(address).await.context("connecting")?;
    tcp.set_nodelay(true)?;
    // The certificate is pinned by fingerprint, so the name is never checked.
    let name = ServerName::try_from("herder").context("the TLS server name")?;
    let tls = connector.connect(name, tcp).await.context("TLS")?;
    let request = format!("wss://{address}/").into_client_request()?;
    let (mut ws, _) = tokio_tungstenite::client_async(request, tls)
        .await
        .context("the WebSocket upgrade")?;
    let text = serde_json::to_string(&ClientMessage::Hello(hello.clone()))?;
    ws.send(Message::text(text)).await?;
    loop {
        let text = match ws.next().await {
            Some(Ok(Message::Text(text))) => text,
            Some(Ok(Message::Close(_))) | None => bail!("the daemon closed the connection"),
            Some(Ok(_)) => continue,
            Some(Err(err)) => return Err(err.into()),
        };
        return match serde_json::from_str(&text)? {
            ServerMessage::Hello(hello) if hello.protocol_version == PROTOCOL_VERSION => {
                Ok((ws, hello))
            }
            ServerMessage::Hello(hello) => Err(anyhow!(
                "the daemon speaks protocol {}, this client {PROTOCOL_VERSION}",
                hello.protocol_version
            )),
            ServerMessage::Error { error } => Err(anyhow!(error.message)),
            _ => Err(anyhow!("the daemon did not say hello")),
        };
    }
}

/// Wait before retry `attempt` (0 for the first): exponential from [`BACKOFF_BASE`], capped at
/// [`BACKOFF_CAP`], then a random cut of up to half, so clients do not retry in lockstep.
fn backoff(attempt: u32) -> Duration {
    let full = BACKOFF_BASE
        .saturating_mul(2u32.saturating_pow(attempt))
        .min(BACKOFF_CAP);
    let mut byte = [0u8; 1];
    // Without randomness the wait is just not jittered.
    let _ = SystemRandom::new().fill(&mut byte);
    full - full / 2 * u32::from(byte[0]) / 255
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_the_cap_with_jitter() {
        for attempt in 0..40 {
            let full = BACKOFF_BASE
                .saturating_mul(2u32.saturating_pow(attempt))
                .min(BACKOFF_CAP);
            let wait = backoff(attempt);
            assert!(wait <= full && wait >= full / 2, "{attempt}: {wait:?}");
        }
        assert!(backoff(0) <= BACKOFF_BASE);
        assert!(backoff(30) >= BACKOFF_CAP / 2);
    }
}
