//! One task per machine owns its connection: it connects, resumes every wanted session from the
//! cache's cursor, feeds what the daemon sends into the cache, dispatches commands, and on any
//! failure waits out a capped, jittered exponential backoff before trying again.
//!
//! While the app is in the background ([`Supervisor::suspend`]) the task keeps a connection
//! that is up but does not retry a lost one. Back in the foreground ([`Supervisor::wake`]) it
//! reconnects at once, and checks a connection that is up before trusting it: one silent for
//! longer than [`SILENCE_LIMIT`] (a suspension the OS may have killed it in, unnoticed) is
//! replaced right away, any other is pinged and replaced if nothing comes back within
//! [`PROBE_TIMEOUT`].
//!
//! A connection is pinged as soon as it is up and every [`PING_INTERVAL`] after; each ping
//! carries a fresh payload, so its pong gives the round trip, and a ping still unanswered when
//! the next one is due counts as a missed pong.
//!
//! Terminal attachments belong to a connection on the daemon, so the task re-attaches every
//! terminal this client holds a stream for on each new connection, with fresh command ids (a
//! resent id would be answered from the daemon's memory without attaching anything).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use futures_util::future::BoxFuture;
use futures_util::stream::{FuturesUnordered, SplitSink};
use futures_util::{SinkExt, StreamExt};
use herder_protocol::{
    ClientHello, ClientMessage, Command, CommandBody, CommandId, CommandResult, Cursor, ErrorInfo,
    FailoverSettings, PROTOCOL_VERSION, Project, Role, ServerHello, ServerMessage, SessionId,
    TerminalId, Timestamp,
};
use ring::rand::{SecureRandom, SystemRandom};
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::auth::{DeviceKey, client_config};
use crate::cache::SessionLog;
use crate::offline::{self, Cached};
use crate::profile::SavedMachine;
use crate::terminal::{TerminalEvent, TerminalStream};
use crate::{ConnectionQuality, ConnectionState, Error, Machine, SessionUpdate, new_command_id};

/// Time one address gets for TCP, TLS, the WebSocket upgrade and the hellos.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Head start each address gets over the next one when connecting; one that fails sooner
/// starts the next at once.
const HEAD_START: Duration = Duration::from_millis(300);

/// First retry delay; each failed attempt doubles it, up to [`BACKOFF_CAP`].
const BACKOFF_BASE: Duration = Duration::from_millis(250);

/// Longest wait between attempts.
const BACKOFF_CAP: Duration = Duration::from_secs(30);

/// How many of the latest round trips [`ConnectionQuality`] sums up.
const RECENT_PONGS: usize = 20;

/// How often a connection is pinged.
const PING_INTERVAL: Duration = Duration::from_secs(15);

/// Silence after which a connection is considered dead, though TCP has not noticed yet.
const SILENCE_LIMIT: Duration = Duration::from_secs(45);

/// Time a connection gets to answer the probe a wake sends before it is replaced.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How often a connection saves what changed to the offline cache.
const SAVE_INTERVAL: Duration = Duration::from_secs(30);

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
    /// Report once the daemon sent everything it owes for what was sent before.
    Sync(oneshot::Sender<()>),
    /// The app went to the background: stop retrying until [`Op::Wake`].
    Suspend,
    /// The app is in the foreground: reconnect now, or probe the connection that is up.
    Wake,
    /// The machine's addresses changed: reconnect now, or move to one that comes first.
    Readdress,
    /// Drop the connection and race the addresses again now; report the address that
    /// answered, or why none did.
    Reconnect(oneshot::Sender<Result<String, String>>),
}

/// A machine's supervisor: the state its task maintains, and the handle the client drives it
/// through.
pub(crate) struct Supervisor {
    /// The machine as saved when the supervisor started; its name and addresses may have
    /// changed since ([`Supervisor::saved`]).
    pub(crate) saved: SavedMachine,
    state: Mutex<State>,
    /// Bumped whenever [`Supervisor::view`] would change.
    changed: Arc<watch::Sender<u64>>,
    ops: mpsc::UnboundedSender<Op>,
    pub(crate) stop: CancellationToken,
    /// The client's config dir, which holds the offline cache.
    config_dir: PathBuf,
    /// Names the client in daemon logs.
    client: String,
    /// Held across each offline cache save, from taking the state to writing it.
    saving: Mutex<()>,
}

#[derive(Default)]
struct State {
    /// The machine's display name, once renamed.
    name: Option<String>,
    /// Addresses in order of preference.
    addresses: Vec<String>,
    /// The address the current connection uses.
    address: Option<String>,
    connection: Option<ConnectionState>,
    quality: Quality,
    role: Option<Role>,
    sessions: Vec<herder_protocol::SessionHead>,
    hosts: Vec<herder_protocol::FleetHost>,
    projects: Vec<Project>,
    accounts: Vec<herder_protocol::Account>,
    failover: FailoverSettings,
    terminals: Vec<herder_protocol::Terminal>,
    resources: Option<herder_protocol::HostResources>,
    session_usage: HashMap<SessionId, herder_protocol::SessionUsage>,
    vault: Option<herder_protocol::VaultStatus>,
    skills: Option<herder_protocol::SkillsStatus>,
    session_skills: HashMap<SessionId, Vec<herder_protocol::SessionSkill>>,
    providers: Vec<herder_protocol::ProviderStatus>,
    logs: HashMap<SessionId, Log>,
    /// Subscribers per session; the daemon streams the sessions with at least one.
    wanted: HashMap<SessionId, usize>,
    /// Terminals this client holds a stream for; each new connection re-attaches them.
    streams: HashMap<TerminalId, Attached>,
    /// Whether something the offline cache holds changed since the last save.
    dirty: bool,
}

impl State {
    /// A state holding what the offline cache had.
    fn cached(cached: Cached) -> Self {
        let logs = cached
            .logs
            .into_iter()
            .filter_map(|events| {
                let session_id = events.first()?.session_id.clone();
                let log = Log {
                    log: SessionLog::from_events(events),
                    changed: watch::Sender::new(0),
                };
                Some((session_id, log))
            })
            .collect();
        Self {
            role: cached.role,
            sessions: cached.sessions,
            hosts: cached.hosts,
            projects: cached.projects,
            accounts: cached.accounts,
            failover: cached.failover,
            logs,
            ..Self::default()
        }
    }

    /// What the offline cache keeps of this state: the events of the [`offline::RECENT`]
    /// listed sessions with the newest latest event.
    fn to_cached(&self) -> Cached {
        let mut logs: Vec<&[herder_protocol::Event]> = self
            .sessions
            .iter()
            .filter_map(|head| self.logs.get(&head.session_id))
            .map(|log| log.log.events())
            .filter(|events| !events.is_empty())
            .collect();
        logs.sort_by_key(|events| std::cmp::Reverse(events.last().map(|event| event.at)));
        logs.truncate(offline::RECENT);
        Cached {
            role: self.role,
            sessions: self.sessions.clone(),
            hosts: self.hosts.clone(),
            projects: self.projects.clone(),
            accounts: self.accounts.clone(),
            failover: self.failover.clone(),
            logs: logs.into_iter().map(<[_]>::to_vec).collect(),
        }
    }
}

/// What [`ConnectionQuality`] is made of.
#[derive(Default)]
struct Quality {
    /// When the current connection was established.
    connected_since: Option<Timestamp>,
    /// Connections established so far.
    connections: u32,
    /// Round trips of the latest pongs on the current connection, oldest first.
    rtts: VecDeque<Duration>,
    missed_pongs: u32,
}

impl Quality {
    fn connected(&mut self) {
        self.connections = self.connections.saturating_add(1);
        self.connected_since = Some(Timestamp::now());
        self.rtts.clear();
    }

    fn disconnected(&mut self) {
        self.connected_since = None;
        self.rtts.clear();
    }

    fn pong(&mut self, rtt: Duration) {
        if self.rtts.len() == RECENT_PONGS {
            self.rtts.pop_front();
        }
        self.rtts.push_back(rtt);
    }

    fn view(&self) -> ConnectionQuality {
        let ms = |rtt: Duration| u32::try_from(rtt.as_millis()).unwrap_or(u32::MAX);
        let average = u32::try_from(self.rtts.len())
            .ok()
            .filter(|&count| count > 0)
            .map(|count| self.rtts.iter().sum::<Duration>() / count);
        ConnectionQuality {
            connected_since: self.connected_since,
            reconnects: self.connections.saturating_sub(1),
            last_rtt_ms: self.rtts.back().copied().map(ms),
            average_rtt_ms: average.map(ms),
            min_rtt_ms: self.rtts.iter().min().copied().map(ms),
            max_rtt_ms: self.rtts.iter().max().copied().map(ms),
            missed_pongs: self.missed_pongs,
        }
    }
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
    /// Starts the task that keeps `saved` connected until `stop`, holding what the offline
    /// cache in `config_dir` has for it until the daemon says otherwise.
    pub(crate) fn start(
        saved: SavedMachine,
        config_dir: PathBuf,
        client: String,
        changed: Arc<watch::Sender<u64>>,
        stop: CancellationToken,
    ) -> Result<Arc<Self>, Error> {
        let device = DeviceKey::from_pem(&saved.device_key).map_err(|err| Error::Local {
            message: format!("the device key of {}: {err:#}", saved.name),
        })?;
        let (ops, queue) = mpsc::unbounded_channel();
        let state = State {
            addresses: saved.addresses.clone(),
            ..State::cached(offline::load(&config_dir, &saved.host_id))
        };
        let supervisor = Arc::new(Self {
            saved,
            state: Mutex::new(state),
            changed,
            ops,
            stop,
            config_dir,
            client,
            saving: Mutex::new(()),
        });
        tokio::spawn(run(Arc::clone(&supervisor), device, queue));
        Ok(supervisor)
    }

    /// Stops the task; subscriptions end.
    pub(crate) fn stop(&self) {
        self.stop.cancel();
    }

    /// The app went to the background: a lost connection is not retried until [`Self::wake`].
    pub(crate) fn suspend(&self) {
        let _ = self.ops.send(Op::Suspend);
    }

    /// The app is in the foreground: reconnects now if disconnected, else probes the
    /// connection and replaces it if it is dead.
    pub(crate) fn wake(&self) {
        let _ = self.ops.send(Op::Wake);
    }

    /// Saves to the offline cache what changed since the last save. Blocks on the file system.
    pub(crate) fn save(&self) {
        let _saving = self.saving.lock().unwrap_or_else(PoisonError::into_inner);
        // A forgotten machine's cache is gone and stays gone.
        if self.stop.is_cancelled() {
            return;
        }
        let cached = {
            let mut state = self.lock();
            if !state.dirty {
                return;
            }
            state.dirty = false;
            state.to_cached()
        };
        if let Err(error) = offline::save(&self.config_dir, &self.saved.host_id, &cached) {
            warn!(machine = %self.saved.name, "saving the offline cache: {error}");
            self.lock().dirty = true;
        }
    }

    /// Stops the task and deletes the machine's offline cache.
    pub(crate) fn forget(&self) {
        self.stop();
        let _saving = self.saving.lock().unwrap_or_else(PoisonError::into_inner);
        offline::remove(&self.config_dir, &self.saved.host_id);
    }

    /// The machine as it should be saved now.
    pub(crate) fn saved(&self) -> SavedMachine {
        let mut saved = self.saved.clone();
        let state = self.lock();
        if let Some(name) = &state.name {
            saved.name.clone_from(name);
        }
        saved.addresses.clone_from(&state.addresses);
        saved
    }

    /// Connects to `addresses`, in this order of preference, from now on.
    pub(crate) fn set_addresses(&self, addresses: Vec<String>) {
        self.lock().addresses = addresses;
        self.notify();
        let _ = self.ops.send(Op::Readdress);
    }

    /// Drops the connection, if up, and races the addresses again at once: the address the
    /// new connection uses, or [`Error::Unreachable`] when none answered.
    pub(crate) async fn reconnect(&self) -> Result<String, Error> {
        let (reply, done) = oneshot::channel();
        self.ops
            .send(Op::Reconnect(reply))
            .map_err(|_| Error::Closed)?;
        tokio::select! {
            () = self.stop.cancelled() => Err(Error::Closed),
            done = done => done
                .map_err(|_| Error::Closed)?
                .map_err(|message| Error::Unreachable { message }),
        }
    }

    /// Shows the machine as `name` from now on.
    pub(crate) fn rename(&self, name: String) {
        self.lock().name = Some(name);
        self.notify();
    }

    pub(crate) fn view(&self) -> Machine {
        let state = self.lock();
        Machine {
            host_id: self.saved.host_id.clone(),
            name: state
                .name
                .clone()
                .unwrap_or_else(|| self.saved.name.clone()),
            addresses: state.addresses.clone(),
            address: state.address.clone(),
            fingerprint: self.saved.fingerprint.clone(),
            connection: state
                .connection
                .clone()
                .unwrap_or(ConnectionState::Connecting),
            quality: state.quality.view(),
            role: state.role,
            sessions: state.sessions.clone(),
            hosts: state.hosts.clone(),
            projects: state.projects.clone(),
            accounts: state.accounts.clone(),
            failover: state.failover.clone(),
            terminals: state.terminals.clone(),
            resources: state.resources.clone(),
            session_usage: state.session_usage.clone(),
            vault: state.vault.clone(),
            skills: state.skills.clone(),
            session_skills: state.session_skills.clone(),
            providers: state.providers.clone(),
        }
    }

    /// Which connection this is, counting from 1, and the skill library's repository as the
    /// daemon reported it on it; `None` unless connected as an owner to a daemon that sent its
    /// skills status.
    pub(crate) fn library(&self) -> Option<(u32, Option<String>)> {
        let state = self.lock();
        if state.connection != Some(ConnectionState::Connected) || state.role != Some(Role::Owner) {
            return None;
        }
        let repo = state.skills.as_ref()?.repo.clone();
        Some((state.quality.connections, repo))
    }

    /// Which connection this is, counting from 1.
    pub(crate) fn connections(&self) -> u32 {
        self.lock().quality.connections
    }

    /// Waits until the machine is connected and the daemon sent everything it owes for what
    /// this client sent before the call: the lists that follow its hello and the replay of
    /// every subscription made before.
    pub(crate) async fn synced(&self) -> Result<(), Error> {
        let (reply, done) = oneshot::channel();
        self.ops.send(Op::Sync(reply)).map_err(|_| Error::Closed)?;
        tokio::select! {
            () = self.stop.cancelled() => Err(Error::Closed),
            done = done => done.map_err(|_| Error::Closed),
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

    /// Sends `body`, an `open_terminal`, `add_account` or `log_in_account`, and streams the
    /// terminal it opens, once a connection is up. An open lost to a dropped connection is
    /// resent with the same id, so it opens one terminal.
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
        match answer.map_err(|info| Error::Rejected { info })? {
            CommandResult::TerminalOpened { terminal_id } => Ok(TerminalStream {
                supervisor: Arc::clone(self),
                terminal_id,
                events: tokio::sync::Mutex::new(receiver),
            }),
            other => Err(Error::Local {
                message: format!("the daemon answered a terminal open with {other:?}"),
            }),
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
                return Err(Error::Local {
                    message: format!("terminal {terminal_id} is already attached on this client"),
                });
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
            answer = answer => answer.map_err(|_| Error::Closed)?.map_err(|info| Error::Rejected { info }),
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
        let mut state = self.lock();
        // Resource figures, a vault's status and skills are live; a new connection sends them
        // afresh.
        if connection == ConnectionState::Connected {
            state.quality.connected();
        } else {
            state.resources = None;
            state.session_usage.clear();
            state.vault = None;
            state.skills = None;
            state.session_skills.clear();
            state.address = None;
            state.quality.disconnected();
        }
        state.connection = Some(connection);
        drop(state);
        self.notify();
    }

    /// Records a pong that came back `rtt` after its ping.
    fn pong(&self, rtt: Duration) {
        self.lock().quality.pong(rtt);
        self.notify();
    }

    /// Records a ping whose pong did not come back before the next ping was due.
    fn missed_pong(&self) {
        let mut state = self.lock();
        state.quality.missed_pongs = state.quality.missed_pongs.saturating_add(1);
        drop(state);
        self.notify();
    }

    fn notify(&self) {
        self.changed.send_modify(|version| *version += 1);
    }

    /// A connection to the first of the addresses before the current connection's that
    /// answers, which proves it reachable and is then dropped; `None` when the current
    /// connection uses the first address.
    fn better(&self) -> Option<BoxFuture<'static, Result<String, String>>> {
        let better = {
            let state = self.lock();
            let current = state.address.as_ref()?;
            let index = state.addresses.iter().position(|a| a == current)?;
            state.addresses[..index].to_vec()
        };
        if better.is_empty() {
            return None;
        }
        // It loaded when the supervisor started.
        let device = DeviceKey::from_pem(&self.saved.device_key).ok()?;
        let fingerprint = self.saved.fingerprint.clone();
        let hello = ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client: self.client.clone(),
            resume: Vec::new(),
            pairing_code: None,
        };
        Some(Box::pin(async move {
            connect(&better, &fingerprint, &device, hello)
                .await
                .map(|(_, _, address)| address)
        }))
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
        if matches!(
            message,
            ServerMessage::Sessions { .. }
                | ServerMessage::Hosts { .. }
                | ServerMessage::Projects { .. }
                | ServerMessage::Accounts { .. }
                | ServerMessage::Providers { .. }
                | ServerMessage::Event(_)
        ) {
            state.dirty = true;
        }
        let log = match message {
            ServerMessage::Sessions { sessions } => {
                state.sessions = sessions;
                return self.notify_after(state);
            }
            ServerMessage::Hosts { hosts } => {
                state.hosts = hosts;
                return self.notify_after(state);
            }
            ServerMessage::Projects { projects } => {
                state.projects = projects;
                return self.notify_after(state);
            }
            ServerMessage::Accounts { accounts, failover } => {
                state.accounts = accounts;
                state.failover = failover;
                return self.notify_after(state);
            }
            ServerMessage::Terminals { terminals } => {
                state.terminals = terminals;
                return self.notify_after(state);
            }
            ServerMessage::HostResources(resources) => {
                state.resources = Some(resources);
                return self.notify_after(state);
            }
            ServerMessage::VaultStatus(status) => {
                state.vault = Some(status);
                return self.notify_after(state);
            }
            ServerMessage::SkillsStatus(status) => {
                state.skills = Some(status);
                return self.notify_after(state);
            }
            ServerMessage::Providers { providers } => {
                state.providers = providers;
                return self.notify_after(state);
            }
            ServerMessage::SessionSkills { session_id, skills } => {
                // The daemon sends a session's empty list once, then nothing until it changes.
                if skills.is_empty() {
                    state.session_skills.remove(&session_id);
                } else {
                    state.session_skills.insert(session_id, skills);
                }
                return self.notify_after(state);
            }
            ServerMessage::SessionResources { session_id, usage } => {
                // The daemon sends a session's idle usage once, then nothing until it changes.
                if usage.processes == 0 && usage.containers.is_empty() {
                    state.session_usage.remove(&session_id);
                } else {
                    state.session_usage.insert(session_id, usage);
                }
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
            // Hellos, answers, syncs and terminal messages are handled by the connection.
            ServerMessage::Synced { .. }
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
    /// A wake found it dead; reconnect without a backoff.
    Replaced(String),
    /// The client asked for a new connection; reconnect without a backoff and report how
    /// that went.
    Reconnect(oneshot::Sender<Result<String, String>>),
}

async fn run(supervisor: Arc<Supervisor>, device: DeviceKey, mut ops: mpsc::UnboundedReceiver<Op>) {
    let mut pending: Vec<Pending> = Vec::new();
    // Syncs waiting for their answer, by token, sent again on each new connection.
    let mut syncs: HashMap<String, oneshot::Sender<()>> = HashMap::new();
    // Reconnects asked for, answered once the next attempt connects or fails.
    let mut reconnects: Vec<oneshot::Sender<Result<String, String>>> = Vec::new();
    let mut attempt = 0;
    let mut suspended = false;
    let saved = &supervisor.saved;
    loop {
        supervisor.set_connection(ConnectionState::Connecting);
        let addresses = supervisor.lock().addresses.clone();
        let hello = ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client: supervisor.client.clone(),
            resume: supervisor.cursors(),
            pairing_code: None,
        };
        let connected = tokio::select! {
            () = supervisor.stop.cancelled() => return,
            connected = connect(&addresses, &saved.fingerprint, &device, hello) => connected,
        };
        let failed = connected.is_err();
        let (error, now) = match connected {
            Ok((ws, hello, address)) => {
                attempt = 0;
                {
                    let mut state = supervisor.lock();
                    state.role = Some(hello.role);
                    state.address = Some(address.clone());
                    state.dirty = true;
                }
                supervisor.set_connection(ConnectionState::Connected);
                for reply in reconnects.drain(..) {
                    let _ = reply.send(Ok(address.clone()));
                }
                let ended = serve(
                    &supervisor,
                    ws,
                    &mut ops,
                    &mut pending,
                    &mut syncs,
                    &mut suspended,
                )
                .await;
                save_in_background(&supervisor);
                match ended {
                    Ended::Stopped => return,
                    Ended::Lost(error) => (error, false),
                    Ended::Replaced(error) => (error, true),
                    Ended::Reconnect(reply) => {
                        reconnects.push(reply);
                        ("reconnecting, as asked".to_owned(), true)
                    }
                }
            }
            Err(error) => (error, false),
        };
        debug!(machine = %saved.name, "disconnected: {error}");
        supervisor.set_connection(ConnectionState::Disconnected {
            error: error.clone(),
        });
        if failed {
            for reply in reconnects.drain(..) {
                let _ = reply.send(Err(error.clone()));
            }
        }
        if now {
            continue;
        }
        let wait = tokio::time::sleep(backoff(attempt));
        attempt = attempt.saturating_add(1);
        tokio::pin!(wait);
        loop {
            tokio::select! {
                () = supervisor.stop.cancelled() => return,
                () = &mut wait, if !suspended => break,
                op = ops.recv() => match op {
                    Some(Op::Wake) => {
                        suspended = false;
                        attempt = 0;
                        break;
                    }
                    Some(Op::Readdress) => {
                        attempt = 0;
                        break;
                    }
                    Some(Op::Reconnect(reply)) => {
                        reconnects.push(reply);
                        attempt = 0;
                        break;
                    }
                    Some(Op::Suspend) => suspended = true,
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
                    Some(Op::Sync(reply)) => {
                        syncs.insert(ulid::Ulid::new().to_string(), reply);
                    }
                    // The next hello resumes whatever is wanted by then, and the next
                    // connection attaches every stream; terminal commands are best effort.
                    Some(Op::Subscribe(_) | Op::Unsubscribe(_) | Op::Attach(_) | Op::Once(_)) => {}
                    None => return,
                },
            }
        }
    }
}

/// Saves to the offline cache off the async runtime.
fn save_in_background(supervisor: &Arc<Supervisor>) {
    let supervisor = Arc::clone(supervisor);
    tokio::task::spawn_blocking(move || supervisor.save());
}

/// Runs a connection until it fails or `stop`.
async fn serve(
    supervisor: &Arc<Supervisor>,
    ws: Ws,
    ops: &mut mpsc::UnboundedReceiver<Op>,
    pending: &mut Vec<Pending>,
    syncs: &mut HashMap<String, oneshot::Sender<()>>,
    suspended: &mut bool,
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
    syncs.retain(|_, reply| !reply.is_closed());
    let synced = syncs.keys().map(|token| ClientMessage::Sync {
        token: token.clone(),
    });
    let messages: Vec<_> = supervisor
        .reattach(&mut attaching)
        .into_iter()
        .chain(resent)
        .chain(synced)
        .collect();
    for message in messages {
        if let Err(error) = write(&mut sink, encode(&message)).await {
            return Ended::Lost(error);
        }
    }
    // Its first tick is now: the first round trip is known as soon as the connection is up.
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // The payload of the latest steady ping and when it went out, until its pong comes back.
    let mut pinged: Option<(String, Instant)> = None;
    let mut save = tokio::time::interval(SAVE_INTERVAL);
    save.reset();
    let mut heard = Instant::now();
    // The payload of the ping a wake sent, and when it went out; its pong must come back within
    // PROBE_TIMEOUT. Only that pong clears it: frames read before it may have been buffered
    // before a suspension.
    let mut probe: Option<(String, Instant)> = None;
    // A connection to an address before this one's, which a wake or new addresses start;
    // once one answers, this connection is replaced to move there.
    let mut better: Option<BoxFuture<'static, Result<String, String>>> = None;
    loop {
        let deadline = probe.as_ref().map(|(_, sent)| *sent + PROBE_TIMEOUT);
        let probed = async {
            match deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        let message = tokio::select! {
            () = supervisor.stop.cancelled() => return Ended::Stopped,
            () = probed => {
                return Ended::Replaced("the daemon did not answer the wake probe".to_owned());
            }
            reached = async {
                match better.as_mut() {
                    Some(reaching) => reaching.await,
                    None => std::future::pending().await,
                }
            } => {
                match reached {
                    Ok(address) => {
                        return Ended::Replaced(format!("moving to {address}, which comes first"));
                    }
                    Err(error) => {
                        debug!(machine = %supervisor.saved.name, "no better address: {error}");
                        better = None;
                        continue;
                    }
                }
            }
            _ = save.tick() => {
                save_in_background(supervisor);
                continue;
            }
            _ = ping.tick() => {
                if heard.elapsed() > SILENCE_LIMIT {
                    return Ended::Lost("the daemon stopped answering".to_owned());
                }
                if pinged.is_some() {
                    supervisor.missed_pong();
                }
                let payload = ulid::Ulid::new().to_string();
                let message = Message::Ping(payload.clone().into_bytes().into());
                pinged = Some((payload, Instant::now()));
                if let Err(error) = write(&mut sink, message).await {
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
                    Some(Op::Sync(reply)) => {
                        let token = ulid::Ulid::new().to_string();
                        syncs.insert(token.clone(), reply);
                        ClientMessage::Sync { token }
                    }
                    Some(Op::Suspend) => {
                        *suspended = true;
                        continue;
                    }
                    Some(Op::Wake) => {
                        *suspended = false;
                        if heard.elapsed() > SILENCE_LIMIT {
                            return Ended::Replaced(
                                "the connection was silent through a suspension".to_owned(),
                            );
                        }
                        if better.is_none() {
                            better = supervisor.better();
                        }
                        if probe.is_none() {
                            let payload = ulid::Ulid::new().to_string();
                            let ping = Message::Ping(payload.clone().into_bytes().into());
                            probe = Some((payload, Instant::now()));
                            if let Err(error) = write(&mut sink, ping).await {
                                return Ended::Lost(error);
                            }
                        }
                        continue;
                    }
                    Some(Op::Readdress) => {
                        let state = supervisor.lock();
                        if let Some(address) = &state.address
                            && !state.addresses.contains(address)
                        {
                            return Ended::Replaced(format!(
                                "{address} is no longer one of the machine's addresses"
                            ));
                        }
                        drop(state);
                        better = supervisor.better();
                        continue;
                    }
                    Some(Op::Reconnect(reply)) => return Ended::Reconnect(reply),
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
            Some(Ok(Message::Pong(payload))) => {
                for ping in [&mut pinged, &mut probe] {
                    if let Some((_, sent)) = ping.take_if(|(sent, _)| *payload == *sent.as_bytes())
                    {
                        supervisor.pong(sent.elapsed());
                    }
                }
                continue;
            }
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
            ServerMessage::Synced { token } => {
                if let Some(reply) = syncs.remove(&token) {
                    let _ = reply.send(());
                }
                None
            }
            ServerMessage::TerminalOutput { terminal_id, data } => {
                let opening = pending.iter().any(|command| command.open.is_some());
                let event = TerminalEvent::Output { data: data.0 };
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
            | CommandBody::AddAccount { cols, rows, .. }
            | CommandBody::InstallProvider { cols, rows, .. }
            | CommandBody::LogInAccount { cols, rows, .. } => Some((cols, rows)),
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

/// Connects to the first of `addresses` that answers, pinned to `fingerprint`, as `device`,
/// and exchanges hellos; returns the address it connected to.
///
/// The addresses race in order: each starts [`HEAD_START`] after the one before, or as soon as
/// every attempt so far failed, and the first to finish its hello wins. An address that comes
/// first wins whenever it answers about as fast as the rest, and a dead one delays the next by
/// [`HEAD_START`], not [`CONNECT_TIMEOUT`].
pub(crate) async fn connect(
    addresses: &[String],
    fingerprint: &str,
    device: &DeviceKey,
    hello: ClientHello,
) -> Result<(Ws, ServerHello, String), String> {
    let config = client_config(fingerprint, device).map_err(|err| format!("{err:#}"))?;
    let connector = TlsConnector::from(Arc::new(config));
    let attempt = |address: &String| {
        let (address, connector, hello) = (address.clone(), connector.clone(), hello.clone());
        async move {
            let result =
                tokio::time::timeout(CONNECT_TIMEOUT, connect_to(&address, &connector, &hello))
                    .await
                    .unwrap_or_else(|_| Err(anyhow!("no answer in {CONNECT_TIMEOUT:?}")));
            (address, result)
        }
    };
    let mut queue = addresses.iter();
    let mut running = FuturesUnordered::new();
    let mut errors = Vec::new();
    let next_start = tokio::time::sleep(HEAD_START);
    tokio::pin!(next_start);
    loop {
        if running.is_empty() {
            let Some(address) = queue.next() else { break };
            running.push(attempt(address));
            next_start.as_mut().reset(Instant::now() + HEAD_START);
        }
        let waiting = !queue.as_slice().is_empty();
        tokio::select! {
            Some((address, result)) = running.next() => match result {
                Ok((ws, hello)) => return Ok((ws, hello, address)),
                Err(err) => errors.push(format!("{address}: {err:#}")),
            },
            () = &mut next_start, if waiting => {
                if let Some(address) = queue.next() {
                    running.push(attempt(address));
                    next_start.as_mut().reset(Instant::now() + HEAD_START);
                }
            }
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

    #[test]
    fn quality_sums_up_the_latest_round_trips_of_the_current_connection() {
        let ms = Duration::from_millis;
        let mut quality = Quality::default();
        assert_eq!(quality.view(), ConnectionQuality::default());

        quality.connected();
        assert!(quality.view().connected_since.is_some());
        for rtt in [30, 10, 20] {
            quality.pong(ms(rtt));
        }
        quality.missed_pongs = 1;
        let view = quality.view();
        assert_eq!(view.reconnects, 0);
        assert_eq!(view.last_rtt_ms, Some(20));
        assert_eq!(view.average_rtt_ms, Some(20));
        assert_eq!(view.min_rtt_ms, Some(10));
        assert_eq!(view.max_rtt_ms, Some(30));

        // Only the latest RECENT_PONGS count.
        for _ in 0..RECENT_PONGS {
            quality.pong(ms(5));
        }
        let view = quality.view();
        assert_eq!((view.min_rtt_ms, view.max_rtt_ms), (Some(5), Some(5)));

        // Round trips belong to a connection; the counters to the client.
        quality.disconnected();
        let view = quality.view();
        assert_eq!(view.connected_since, None);
        assert_eq!(view.last_rtt_ms, None);
        quality.connected();
        let view = quality.view();
        assert_eq!(view.reconnects, 1);
        assert_eq!(view.missed_pongs, 1);
        assert_eq!(view.average_rtt_ms, None);
    }
}
