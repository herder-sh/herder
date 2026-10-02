//! One task per machine owns its connection: it connects, resumes every wanted session from the
//! cache's cursor, feeds what the daemon sends into the cache, dispatches commands, and on any
//! failure waits out a capped, jittered exponential backoff before trying again.
//!
//! Terminal attachments belong to a connection on the daemon, so the task re-attaches every
//! terminal this client holds a stream for on each new connection, with fresh command ids (a
//! resent id would be answered from the daemon's memory without attaching anything).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use herder_protocol::{
    ClientHello, ClientMessage, Command, CommandBody, CommandId, CommandResult, Cursor, ErrorInfo,
    PROTOCOL_VERSION, Role, ServerHello, ServerMessage, SessionId, TerminalId,
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
use crate::terminal::{TerminalEvent, TerminalStream};
use crate::{ConnectionState, Error, Machine, SessionUpdate, new_command_id};

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
    /// Open a terminal: a command whose answer registers a stream feeding `events`.
    Open(
        Command,
        oneshot::Sender<Answer>,
        mpsc::UnboundedSender<TerminalEvent>,
    ),
    /// A stream was added for this terminal; attach it, unless a new connection already did.
    Attach(TerminalId),
    /// Send a terminal command once if connected, ignoring its answer: input, resize, detach.
    Once(CommandBody),
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
    pub(crate) stop: CancellationToken,
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
    /// Terminals this client holds a stream for; each new connection re-attaches them.
    streams: HashMap<TerminalId, Attached>,
}

/// A terminal with a [`TerminalStream`].
struct Attached {
    events: mpsc::UnboundedSender<TerminalEvent>,
    /// The latest size asked for, sent again after every re-attach.
    size: Option<(u16, u16)>,
    /// Whether an attach went out on some connection, so the next one is a re-attach.
    sent: bool,
    /// Answers the first attach, for [`Supervisor::attach_terminal`]; `None` once answered,
    /// and for a terminal this client opened.
    ready: Option<oneshot::Sender<Result<(), ErrorInfo>>>,
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

    /// Sends `body`, an `open_terminal` or `add_account`, and streams the terminal it opens,
    /// once a connection is up. An open lost to a dropped connection is resent with the same
    /// id, so it opens one terminal.
    pub(crate) async fn open_terminal(
        self: &Arc<Self>,
        body: CommandBody,
    ) -> Result<TerminalStream, Error> {
        let (events, receiver) = mpsc::unbounded_channel();
        let (reply, answer) = oneshot::channel();
        let command = Command {
            id: new_command_id(),
            body,
        };
        self.ops
            .send(Op::Open(command, reply, events))
            .map_err(|_| Error::Closed)?;
        let answer = tokio::select! {
            () = self.stop.cancelled() => return Err(Error::Closed),
            answer = answer => answer.map_err(|_| Error::Closed)?,
        };
        match answer.map_err(Error::Rejected)? {
            CommandResult::TerminalOpened { terminal_id } => Ok(TerminalStream {
                supervisor: Arc::clone(self),
                terminal_id,
                events: tokio::sync::Mutex::new(receiver),
            }),
            other => Err(Error::Local(format!(
                "the daemon answered a terminal open with {other:?}"
            ))),
        }
    }

    /// Attaches to `terminal_id` and streams it, once a connection is up; one stream per
    /// terminal per client.
    pub(crate) async fn attach_terminal(
        self: &Arc<Self>,
        terminal_id: TerminalId,
    ) -> Result<TerminalStream, Error> {
        let (events, receiver) = mpsc::unbounded_channel();
        let (ready, answer) = oneshot::channel();
        {
            let mut state = self.lock();
            if state.streams.contains_key(&terminal_id) {
                return Err(Error::Local(format!(
                    "terminal {terminal_id} is already attached on this client"
                )));
            }
            let attached = Attached {
                events,
                size: None,
                sent: false,
                ready: Some(ready),
            };
            state.streams.insert(terminal_id.clone(), attached);
        }
        // Dropped on failure, which forgets the stream and detaches.
        let stream = TerminalStream {
            supervisor: Arc::clone(self),
            terminal_id: terminal_id.clone(),
            events: tokio::sync::Mutex::new(receiver),
        };
        self.ops
            .send(Op::Attach(terminal_id))
            .map_err(|_| Error::Closed)?;
        tokio::select! {
            () = self.stop.cancelled() => Err(Error::Closed),
            answer = answer => answer.map_err(|_| Error::Closed)?.map_err(Error::Rejected),
        }?;
        Ok(stream)
    }

    /// Sends `body` for a streamed terminal if connected; see [`Op::Once`].
    pub(crate) fn terminal_once(&self, terminal_id: &TerminalId, body: CommandBody) {
        if self.lock().streams.contains_key(terminal_id) {
            let _ = self.ops.send(Op::Once(body));
        }
    }

    /// Remembers a streamed terminal's size and sends it if connected.
    pub(crate) fn terminal_resize(&self, terminal_id: &TerminalId, cols: u16, rows: u16) {
        let mut state = self.lock();
        let Some(attached) = state.streams.get_mut(terminal_id) else {
            return;
        };
        attached.size = Some((cols, rows));
        let _ = self.ops.send(Op::Once(CommandBody::ResizeTerminal {
            terminal_id: terminal_id.clone(),
            cols,
            rows,
        }));
    }

    /// Forgets a terminal's stream and detaches from it, if it is still open.
    pub(crate) fn terminal_dropped(&self, terminal_id: &TerminalId) {
        if self.lock().streams.remove(terminal_id).is_some() {
            let _ = self.ops.send(Op::Once(CommandBody::DetachTerminal {
                terminal_id: terminal_id.clone(),
            }));
        }
    }

    /// The attach for each streamed terminal, and its latest size, on a new connection;
    /// streams attached on an earlier one are told their scrollback is coming again.
    fn reattach(&self, attaching: &mut HashMap<CommandId, TerminalId>) -> Vec<ClientMessage> {
        let mut state = self.lock();
        let mut messages = Vec::new();
        for (terminal_id, attached) in &mut state.streams {
            if attached.sent {
                let _ = attached.events.send(TerminalEvent::Reattached);
            }
            attached.sent = true;
            messages.push(attach(terminal_id, attaching));
            if let Some((cols, rows)) = attached.size {
                messages.push(once(CommandBody::ResizeTerminal {
                    terminal_id: terminal_id.clone(),
                    cols,
                    rows,
                }));
            }
        }
        messages
    }

    /// The attach for a stream just added, unless a new connection already sent one.
    fn first_attach(
        &self,
        terminal_id: &TerminalId,
        attaching: &mut HashMap<CommandId, TerminalId>,
    ) -> Option<ClientMessage> {
        let mut state = self.lock();
        let attached = state.streams.get_mut(terminal_id)?;
        if attached.sent {
            return None;
        }
        attached.sent = true;
        Some(attach(terminal_id, attaching))
    }

    /// Settles the daemon's answer to an attach: a refused one ends the stream.
    fn attach_answered(&self, terminal_id: &TerminalId, result: Result<(), ErrorInfo>) {
        let mut state = self.lock();
        match result {
            Ok(()) => {
                if let Some(ready) = state
                    .streams
                    .get_mut(terminal_id)
                    .and_then(|attached| attached.ready.take())
                {
                    let _ = ready.send(Ok(()));
                }
            }
            Err(error) => {
                let Some(mut attached) = state.streams.remove(terminal_id) else {
                    return;
                };
                match attached.ready.take() {
                    Some(ready) => {
                        let _ = ready.send(Err(error));
                    }
                    // The shell exited while this client was away; its exit code went with
                    // the old connection.
                    None => {
                        let _ = attached
                            .events
                            .send(TerminalEvent::Closed { exit_code: None });
                    }
                }
            }
        }
    }

    /// Registers the stream of a terminal this client opened, handing it what the terminal
    /// sent before the open was answered; `false` if the shell already exited.
    fn opened(
        &self,
        terminal_id: TerminalId,
        events: mpsc::UnboundedSender<TerminalEvent>,
        size: Option<(u16, u16)>,
        early: Vec<TerminalEvent>,
    ) -> bool {
        let mut closed = false;
        for event in early {
            closed |= matches!(event, TerminalEvent::Closed { .. });
            let _ = events.send(event);
        }
        if !closed {
            let attached = Attached {
                events,
                size,
                sent: true,
                ready: None,
            };
            self.lock().streams.insert(terminal_id, attached);
        }
        !closed
    }

    /// Routes a terminal's output or exit to its stream. Without one, it is kept in `early`
    /// while an open is in flight, as it may be the new terminal's.
    fn terminal_event(
        &self,
        terminal_id: TerminalId,
        event: TerminalEvent,
        early: &mut HashMap<TerminalId, Vec<TerminalEvent>>,
        opening: bool,
    ) {
        let mut state = self.lock();
        if let TerminalEvent::Closed { .. } = event {
            if let Some(mut attached) = state.streams.remove(&terminal_id) {
                // An attach still waiting for its answer hands out the stream, ended.
                if let Some(ready) = attached.ready.take() {
                    let _ = ready.send(Ok(()));
                }
                let _ = attached.events.send(event);
                return;
            }
        } else if let Some(attached) = state.streams.get(&terminal_id) {
            let _ = attached.events.send(event);
            return;
        }
        if opening {
            early.entry(terminal_id).or_default().push(event);
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
            // Hellos, answers and terminal messages are handled by the connection; resource
            // usage and projects are not exposed through the client core yet.
            ServerMessage::Projects { .. }
            | ServerMessage::HostResources(_)
            | ServerMessage::SessionResources { .. }
            | ServerMessage::TerminalOutput { .. }
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
    /// For a terminal open: where the new terminal's events go.
    open: Option<mpsc::UnboundedSender<TerminalEvent>>,
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
                    Some(Op::Command(command, reply)) => pending.push(Pending {
                        command,
                        reply,
                        open: None,
                    }),
                    Some(Op::Open(command, reply, events)) => pending.push(Pending {
                        command,
                        reply,
                        open: Some(events),
                    }),
                    // The next hello resumes whatever is wanted by then, and the next
                    // connection attaches every stream; terminal commands are best effort.
                    Some(Op::Subscribe(_) | Op::Unsubscribe(_) | Op::Attach(_) | Op::Once(_)) => {}
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
    // Attaches in flight on this connection, by command id.
    let mut attaching = HashMap::new();
    // Terminal events with no stream yet, kept while an open is in flight.
    let mut early = HashMap::new();
    pending.retain(|command| !command.reply.is_closed());
    let resent = pending
        .iter()
        .map(|command| ClientMessage::Command(command.command.clone()));
    let messages: Vec<_> = supervisor
        .reattach(&mut attaching)
        .into_iter()
        .chain(resent)
        .collect();
    for message in messages {
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
                        pending.push(Pending { command, reply, open: None });
                        message
                    }
                    Some(Op::Open(command, reply, events)) => {
                        let message = ClientMessage::Command(command.clone());
                        pending.push(Pending { command, reply, open: Some(events) });
                        message
                    }
                    Some(Op::Attach(terminal_id)) => {
                        match supervisor.first_attach(&terminal_id, &mut attaching) {
                            Some(message) => message,
                            None => continue,
                        }
                    }
                    Some(Op::Once(body)) => once(body),
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
        let reply = match message {
            ServerMessage::CommandAccepted { command_id, result } => {
                if let Some(terminal_id) = attaching.remove(&command_id) {
                    supervisor.attach_answered(&terminal_id, Ok(()));
                    continue;
                }
                answered(
                    supervisor,
                    pending,
                    &command_id,
                    Ok(result),
                    &mut early,
                    &mut attaching,
                )
            }
            ServerMessage::CommandRejected { command_id, error } => {
                if let Some(terminal_id) = attaching.remove(&command_id) {
                    supervisor.attach_answered(&terminal_id, Err(error));
                    continue;
                }
                answered(
                    supervisor,
                    pending,
                    &command_id,
                    Err(error),
                    &mut early,
                    &mut attaching,
                )
            }
            ServerMessage::TerminalOutput { terminal_id, data } => {
                let opening = pending.iter().any(|command| command.open.is_some());
                let event = TerminalEvent::Output(data.0);
                supervisor.terminal_event(terminal_id, event, &mut early, opening);
                None
            }
            ServerMessage::TerminalClosed {
                terminal_id,
                exit_code,
            } => {
                let opening = pending.iter().any(|command| command.open.is_some());
                let event = TerminalEvent::Closed { exit_code };
                supervisor.terminal_event(terminal_id, event, &mut early, opening);
                None
            }
            message => {
                supervisor.apply(message);
                None
            }
        };
        if !pending.iter().any(|command| command.open.is_some()) {
            early.clear();
        }
        if let Some(message) = reply
            && let Err(error) = write(&mut sink, encode(&message)).await
        {
            return Ended::Lost(error);
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

/// Hands the daemon's answer to the command waiting for it. An answered terminal open gets its
/// stream registered and returns the attach to send: on the connection that sent the open the
/// daemon attached it already and ignores a second attach, and after a resend it did not.
fn answered(
    supervisor: &Supervisor,
    pending: &mut Vec<Pending>,
    command_id: &CommandId,
    result: Answer,
    early: &mut HashMap<TerminalId, Vec<TerminalEvent>>,
    attaching: &mut HashMap<CommandId, TerminalId>,
) -> Option<ClientMessage> {
    let index = pending.iter().position(|p| p.command.id == *command_id)?;
    let Pending {
        command,
        reply,
        open,
    } = pending.swap_remove(index);
    let mut message = None;
    if let (Some(events), Ok(CommandResult::TerminalOpened { terminal_id })) = (open, &result) {
        let size = match command.body {
            CommandBody::OpenTerminal { cols, rows, .. }
            | CommandBody::AddAccount { cols, rows, .. } => Some((cols, rows)),
            _ => None,
        };
        let early = early.remove(terminal_id).unwrap_or_default();
        if supervisor.opened(terminal_id.clone(), events, size, early) {
            message = Some(attach(terminal_id, attaching));
        }
    }
    if let Err(Ok(CommandResult::TerminalOpened { terminal_id })) = reply.send(result) {
        // Nobody waits for the stream any more.
        supervisor.terminal_dropped(&terminal_id);
    }
    message
}

/// An attach of `terminal_id` with a fresh id, recorded in `attaching`.
fn attach(
    terminal_id: &TerminalId,
    attaching: &mut HashMap<CommandId, TerminalId>,
) -> ClientMessage {
    let id = new_command_id();
    attaching.insert(id.clone(), terminal_id.clone());
    ClientMessage::Command(Command {
        id,
        body: CommandBody::AttachTerminal {
            terminal_id: terminal_id.clone(),
        },
    })
}

/// `body` as a command with a fresh id, whose answer nobody waits for.
fn once(body: CommandBody) -> ClientMessage {
    ClientMessage::Command(Command {
        id: new_command_id(),
        body,
    })
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
