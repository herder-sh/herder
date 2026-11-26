//! One task per machine owns its connection: it connects, resumes every wanted session from the
//! cache's cursor, feeds what the daemon sends into the cache, dispatches commands, and on any
//! failure waits out a capped, jittered exponential backoff before trying again.
//!
//! While the app is in the background ([`Supervisor::suspend`]) the task keeps a connection
//! that is up but does not retry a lost one. Back in the foreground ([`Supervisor::wake`]) it
//! reconnects at once, and checks a connection that is up before trusting it: one silent for
//! longer than a running task would have let it be (a suspension the OS may have killed it
//! in, unnoticed) is replaced right away, any other is pinged and replaced if the daemon
//! shows no sign of life within [`PROBE_TIMEOUT`].
//!
//! An app wakes the task on every network change too. A wake that finds it connecting races
//! the addresses again beside the race under way, which may be stuck on the network there
//! was before, or about to succeed: neither waits for the other.
//!
//! However many addresses and races there are, one connection says hello: the daemon lists
//! everything and replays every session wanted for each hello it takes, so an address that
//! lost the race is dropped before it said one.
//!
//! A connection that ends sooner than [`STEADY`] after it was made did not work. The first
//! such is replaced as any other; from the second in a row on, the task waits longer before
//! each next one, as it does between failed attempts: on a network that lets a connection be
//! made and then carries nothing, connecting over and over only adds to what it has to carry.
//! A wake still connects at once, and going to the background starts the count afresh.
//!
//! A connection the task gives up tells the daemon why as it closes, so the daemon's log
//! shows what a client that keeps reconnecting saw.
//!
//! A connection is pinged as soon as it is up and every [`PING_INTERVAL`] after; each ping
//! carries a fresh payload, so its pong gives the round trip, and a ping still unanswered when
//! the next one is due counts as a missed pong. A connection whose daemon shows no sign of
//! life within [`SILENCE_LIMIT`] of a ping is lost. What the user asks of a connection, a
//! command or a session, probes it as a wake does: a dead one is found [`PROBE_TIMEOUT`]
//! after it is asked, not only at its next ping.
//!
//! A lost connection is ridden out: for [`QUIET`] the machine still shows as connected, with
//! what its connection brought, while the task reconnects. A connection made within that
//! time takes over unseen, and what the one before brought of the live values (resources,
//! skills, a vault's status, approvals) is held until the new one has sent its own. Only a
//! connection still lost after [`QUIET`] shows as lost.
//!
//! A sign of life is anything that only a daemon that is there brings about ([`Watched`]),
//! not just the pong: a connection busy with a long replay or a large upload, whose pong
//! waits behind megabytes, is as alive as an idle one. What the connection sends is written
//! beside its reading, so a write the network is slow to take, or never takes, holds up
//! neither what the daemon sends nor these checks.
//!
//! Terminal attachments belong to a connection on the daemon, so the task re-attaches every
//! terminal this client holds a stream for on each new connection, with fresh command ids (a
//! resent id would be answered from the daemon's memory without attaching anything).

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::task::{self, Poll};
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use futures_util::future::BoxFuture;
use futures_util::stream::{FuturesUnordered, SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use herder_protocol::{
    ApprovalRequest, ClientHello, ClientMessage, Command, CommandBody, CommandId, CommandResult,
    Cursor, ErrorInfo, FailoverSettings, MAX_MESSAGE_BYTES, PROTOCOL_VERSION, PasskeyAssertion,
    PasskeyChallenge, PasskeyCreation, PersonId, Project, Role, ServerHello, ServerMessage,
    SessionId, TerminalId, Timestamp, UserId,
};
use ring::rand::{SecureRandom, SystemRandom};
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::address;
use crate::auth::{DeviceKey, client_config};
use crate::cache::SessionLog;
use crate::demo::{self, DemoAnswer, DemoMachine};
use crate::offline::{self, Cached};
use crate::profile::SavedMachine;
use crate::relay::Via;
use crate::terminal::{TerminalEvent, TerminalStream};
use crate::tunnel::{self, Io};
use crate::{ConnectionQuality, ConnectionState, Error, Machine, SessionUpdate, new_command_id};

/// Time one address gets for TCP, TLS and the WebSocket upgrade.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Time a daemon that was reached gets to answer a hello, which it does at once. The other
/// addresses that were reached wait for it, so it is short.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// How often a connection that runs through a vault checks whether an address that comes
/// before the vault's answers again, to move there: a wake checks at once.
const ROUTE_CHECK: Duration = Duration::from_secs(60);

/// Head start each address gets over the next one when connecting; one that fails sooner
/// starts the next at once.
const HEAD_START: Duration = Duration::from_millis(300);

/// How soon after one race of the addresses a wake may start another beside it: a burst of
/// wakes, as one network change sends, starts few.
const REDIAL_SPACING: Duration = Duration::from_secs(1);

/// First retry delay; each failed attempt doubles it, up to [`BACKOFF_CAP`].
const BACKOFF_BASE: Duration = Duration::from_millis(250);

/// Longest wait between attempts.
const BACKOFF_CAP: Duration = Duration::from_secs(30);

/// How many of the latest round trips [`ConnectionQuality`] sums up.
const RECENT_PONGS: usize = 20;

/// How often a connection is pinged.
const PING_INTERVAL: Duration = Duration::from_secs(10);

/// Silence after a ping after which a connection is considered dead, though TCP has not
/// noticed yet. Any byte the daemon sends or takes ends the silence, so a slow link that
/// carries something is kept; one that carries nothing for this long is quicker replaced
/// than waited for, and replacing it shows nowhere ([`QUIET`]).
const SILENCE_LIMIT: Duration = Duration::from_secs(5);

/// Silence after a probe, which a wake and what the user asks for send, after which a
/// connection is replaced.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a lost connection still shows as connected while it is being replaced: a drop
/// shorter than this is never seen.
const QUIET: Duration = Duration::from_secs(5);

/// How often a connection saves what changed to the offline cache.
const SAVE_INTERVAL: Duration = Duration::from_secs(30);

/// How long a connection has to last to count as one that worked.
const STEADY: Duration = Duration::from_secs(30);

/// Time a connection given up gets to tell the daemon why before its socket is dropped.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

/// Most bytes of the reason a close frame carries: a control frame holds 125, two of them
/// the close code.
const CLOSE_REASON_BYTES: usize = 123;

/// Whose turn it is to say hello, among every attempt under way to connect to one machine.
type Turn = Arc<tokio::sync::Mutex<()>>;

pub(crate) type Ws = WebSocketStream<TlsStream<Watched>>;

/// When a daemon last showed that it is there, as its stream saw it.
#[derive(Clone)]
struct Alive(Arc<Mutex<Instant>>);

impl Alive {
    fn mark(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Instant::now();
    }

    fn at(&self) -> Instant {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A daemon's TCP stream that notes each sign that the daemon is there: bytes read from it,
/// and a write taken after one the socket refused, since a full socket takes more only once
/// the daemon acknowledged what it was sent. A write into a socket with room shows nothing:
/// the kernel takes it for a daemon that is long gone too.
pub(crate) struct Watched {
    tcp: Box<dyn Io>,
    alive: Alive,
    /// Whether the socket refused the last write.
    blocked: bool,
}

impl Watched {
    fn new(tcp: Box<dyn Io>) -> Self {
        Self {
            tcp,
            alive: Alive(Arc::new(Mutex::new(Instant::now()))),
            blocked: false,
        }
    }
}

impl AsyncRead for Watched {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let poll = Pin::new(&mut self.tcp).poll_read(cx, buf);
        if buf.filled().len() > before {
            self.alive.mark();
        }
        poll
    }
}

impl AsyncWrite for Watched {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let poll = Pin::new(&mut self.tcp).poll_write(cx, buf);
        match &poll {
            Poll::Pending => self.blocked = true,
            Poll::Ready(Ok(_)) if self.blocked => {
                self.blocked = false;
                self.alive.mark();
            }
            Poll::Ready(_) => {}
        }
        poll
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.tcp).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.tcp).poll_shutdown(cx)
    }
}

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
    /// Drop the connection and race the addresses again now, in their order; report the
    /// address that answered, or why none did.
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
    /// Whether the client signs passkey assertions, as its hello says.
    passkey: bool,
    /// Held across each offline cache save, from taking the state to writing it.
    saving: Mutex<()>,
    /// Whether the offline cache was read into the state; it is, before the first connect.
    loaded: watch::Sender<bool>,
    /// For a machine of a demo, what the daemon answered to the commands recorded; such a
    /// supervisor has no task and sends nothing.
    demo: Option<Vec<DemoAnswer>>,
    /// The client's machines, among them the vault a `vault://` address goes through.
    machines: Weak<Mutex<Vec<Arc<Supervisor>>>>,
}

#[derive(Default)]
struct State {
    /// The machine's display name, once renamed.
    name: Option<String>,
    /// Addresses in order of preference.
    addresses: Vec<String>,
    /// The address the current connection uses.
    address: Option<String>,
    /// The address that answered last, which the next connection tries first: it most likely
    /// answers again, whatever became of the ones before it. Forgotten when the user orders
    /// the addresses or asks to reconnect, which both start from their order.
    answered: Option<String>,
    /// The connection as the machine shows it.
    connection: Option<ConnectionState>,
    /// Where the connection stands while the machine still shows the one just lost
    /// ([`QUIET`]); `None` when it shows the connection as it is.
    hidden: Option<ConnectionState>,
    quality: Quality,
    role: Option<Role>,
    user_id: Option<UserId>,
    person_id: Option<PersonId>,
    herder_version: Option<String>,
    sessions: Vec<herder_protocol::SessionHead>,
    hosts: Vec<herder_protocol::FleetHost>,
    projects: Vec<Project>,
    accounts: Vec<herder_protocol::Account>,
    failover: FailoverSettings,
    providers: Vec<herder_protocol::ProviderStatus>,
    terminals: Vec<herder_protocol::Terminal>,
    /// The live values as the machine shows them.
    live: Live,
    /// On a connection that took over from a lost one unseen, the live values it sent so
    /// far: `live` still holds the ones before too, until the daemon has sent all of its own
    /// and these replace them.
    fresh: Option<Live>,
    logs: HashMap<SessionId, Log>,
    /// Subscribers per session; the daemon streams the sessions with at least one.
    wanted: HashMap<SessionId, usize>,
    /// Terminals this client holds a stream for; each new connection re-attaches them.
    streams: HashMap<TerminalId, Attached>,
    /// Whether something the offline cache holds changed since the last save.
    dirty: bool,
}

/// What a daemon sends of its state now, which only a connection keeps current: each
/// connection brings its own, and none is cached.
#[derive(Clone, Default)]
struct Live {
    resources: Option<herder_protocol::HostResources>,
    session_usage: HashMap<SessionId, herder_protocol::SessionUsage>,
    vault: Option<herder_protocol::VaultStatus>,
    skills: Option<herder_protocol::SkillsStatus>,
    session_skills: HashMap<SessionId, Vec<herder_protocol::SessionSkill>>,
    update: Option<herder_protocol::UpdateStage>,
    /// Requests of this person's devices without a passkey, open on the machine.
    approvals: Vec<ApprovalRequest>,
}

impl Live {
    /// Takes in `message` if it carries a live value; `false` for any other.
    fn apply(&mut self, message: &ServerMessage) -> bool {
        match message {
            ServerMessage::HostResources(resources) => self.resources = Some(resources.clone()),
            ServerMessage::DaemonUpdate { stage } => self.update = Some(stage.clone()),
            ServerMessage::VaultStatus(status) => self.vault = Some(status.clone()),
            ServerMessage::ApprovalRequest(request) => {
                self.approvals
                    .retain(|open| open.request_id != request.request_id);
                self.approvals.push(request.clone());
            }
            ServerMessage::ApprovalClosed { request_id } => {
                self.approvals.retain(|open| open.request_id != *request_id);
            }
            ServerMessage::SkillsStatus(status) => self.skills = Some(status.clone()),
            ServerMessage::SessionSkills { session_id, skills } => {
                // The daemon sends a session's empty list once, then nothing until it changes.
                if skills.is_empty() {
                    self.session_skills.remove(session_id);
                } else {
                    self.session_skills
                        .insert(session_id.clone(), skills.clone());
                }
            }
            ServerMessage::SessionResources { session_id, usage } => {
                // The daemon sends a session's idle usage once, then nothing until it changes.
                if usage.processes == 0 && usage.containers.is_empty() {
                    self.session_usage.remove(session_id);
                } else {
                    self.session_usage.insert(session_id.clone(), usage.clone());
                }
            }
            _ => return false,
        }
        true
    }
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
            providers: cached.providers,
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
            providers: self.providers.clone(),
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
    /// Goes with the first attach, which the person this client acts as may need for it;
    /// later re-attaches need none.
    assertion: Option<Box<PasskeyAssertion>>,
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
    /// Starts the task that keeps `saved` connected until `stop`. The task first reads what
    /// the offline cache in `config_dir` has for it, off the async runtime, and holds that
    /// until the daemon says otherwise.
    pub(crate) fn start(
        saved: SavedMachine,
        config_dir: PathBuf,
        client: String,
        passkey: bool,
        changed: Arc<watch::Sender<u64>>,
        stop: CancellationToken,
        machines: Weak<Mutex<Vec<Arc<Supervisor>>>>,
    ) -> Result<Arc<Self>, Error> {
        let device = DeviceKey::from_pem(&saved.device_key).map_err(|err| Error::Local {
            message: format!("the device key of {}: {err:#}", saved.name),
        })?;
        let (ops, queue) = mpsc::unbounded_channel();
        let state = State {
            addresses: saved.addresses.clone(),
            ..State::default()
        };
        let supervisor = Arc::new(Self {
            saved,
            state: Mutex::new(state),
            changed,
            ops,
            stop,
            config_dir,
            client,
            passkey,
            saving: Mutex::new(()),
            loaded: watch::Sender::new(false),
            demo: None,
            machines,
        });
        tokio::spawn(run(Arc::clone(&supervisor), device, queue));
        Ok(supervisor)
    }

    /// A supervisor for `recorded`, a machine of a demo: it holds what was recorded, shows as
    /// connected, and never connects.
    pub(crate) fn demo(
        recorded: DemoMachine,
        changed: Arc<watch::Sender<u64>>,
        stop: CancellationToken,
    ) -> Arc<Self> {
        let DemoMachine {
            machine,
            logs,
            answers,
        } = recorded;
        let logs = logs
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
        let state = State {
            addresses: machine.addresses.clone(),
            address: machine.address,
            connection: Some(ConnectionState::Connected),
            role: machine.role,
            person_id: machine.person_id,
            herder_version: machine.herder_version,
            sessions: machine.sessions,
            hosts: machine.hosts,
            projects: machine.projects,
            accounts: machine.accounts,
            failover: machine.failover,
            providers: machine.providers,
            terminals: machine.terminals,
            live: Live {
                resources: machine.resources,
                session_usage: machine.session_usage,
                vault: machine.vault,
                skills: machine.skills,
                session_skills: machine.session_skills,
                update: machine.update,
                approvals: machine.approvals,
            },
            logs,
            ..State::default()
        };
        // Nothing reads the ops of a demo machine.
        let (ops, _) = mpsc::unbounded_channel();
        Arc::new(Self {
            saved: SavedMachine {
                host_id: machine.host_id,
                name: machine.name,
                addresses: machine.addresses,
                fingerprint: machine.fingerprint,
                device_key: String::new(),
            },
            state: Mutex::new(state),
            changed,
            ops,
            stop,
            config_dir: PathBuf::new(),
            client: String::new(),
            passkey: false,
            saving: Mutex::new(()),
            loaded: watch::Sender::new(true),
            demo: Some(answers),
            machines: Weak::new(),
        })
    }

    /// What a demo machine answers to `command`: what was recorded for it, else
    /// [`Error::Demo`]; a machine that is no demo's sends nothing either.
    pub(crate) fn demo_answer(&self, command: &CommandBody) -> Result<CommandResult, Error> {
        demo::answer(self.demo.as_deref().unwrap_or_default(), command)
    }

    /// Reads the offline cache on the blocking pool: it can hold many complete transcripts.
    async fn load(&self) -> State {
        let config_dir = self.config_dir.clone();
        let host_id = self.saved.host_id.clone();
        tokio::task::spawn_blocking(move || State::cached(offline::load(&config_dir, &host_id)))
            .await
            .unwrap_or_else(|err| {
                warn!(machine = %self.saved.name, "reading the offline cache: {err}");
                State::default()
            })
    }

    /// Takes in what the offline cache had. Nothing was connected yet, so the lists are the
    /// cache's; a session subscribed to meanwhile has an empty log, which the cached one
    /// replaces, waking its subscribers.
    fn restore(&self, cached: State) {
        let mut state = self.lock();
        state.role = cached.role;
        state.sessions = cached.sessions;
        state.hosts = cached.hosts;
        state.projects = cached.projects;
        state.accounts = cached.accounts;
        state.failover = cached.failover;
        state.providers = cached.providers;
        for (session_id, log) in cached.logs {
            match state.logs.entry(session_id) {
                Entry::Occupied(mut entry) => {
                    let held = entry.get_mut();
                    held.log = log.log;
                    held.changed.send_modify(|version| *version += 1);
                }
                Entry::Vacant(entry) => {
                    entry.insert(log);
                }
            }
        }
        drop(state);
        self.loaded.send_replace(true);
        self.notify();
    }

    /// Stops the task; subscriptions end.
    pub(crate) fn stop(&self) {
        self.stop.cancel();
    }

    /// The app went to the background: a lost connection is not retried until [`Self::wake`].
    pub(crate) fn suspend(&self) {
        let _ = self.ops.send(Op::Suspend);
    }

    /// The app is in the foreground, or its network changed: reconnects now if disconnected,
    /// races the addresses again if connecting, else probes the connection and replaces it
    /// if it is dead.
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
        let mut state = self.lock();
        state.addresses = addresses;
        state.answered = None;
        drop(state);
        self.notify();
        let _ = self.ops.send(Op::Readdress);
    }

    /// Drops the connection, if up, and races the addresses again at once, in their order:
    /// the address the new connection uses, or [`Error::Unreachable`] when none answered.
    pub(crate) async fn reconnect(&self) -> Result<String, Error> {
        if self.demo.is_some() {
            return Err(Error::Demo);
        }
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
            user_id: state.user_id.clone(),
            person_id: state.person_id.clone(),
            herder_version: state.herder_version.clone(),
            sessions: state.sessions.clone(),
            hosts: state.hosts.clone(),
            projects: state.projects.clone(),
            accounts: state.accounts.clone(),
            failover: state.failover.clone(),
            providers: state.providers.clone(),
            terminals: state.terminals.clone(),
            resources: state.live.resources.clone(),
            session_usage: state.live.session_usage.clone(),
            vault: state.live.vault.clone(),
            skills: state.live.skills.clone(),
            session_skills: state.live.session_skills.clone(),
            update: state.live.update.clone(),
            approvals: state.live.approvals.clone(),
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
        // On a connection that took over unseen, only what it sent itself: the status of
        // the connection before is no report of this one.
        let live = state.fresh.as_ref().unwrap_or(&state.live);
        let repo = live.skills.as_ref()?.repo.clone();
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
        if self.demo.is_some() {
            return Ok(());
        }
        let (reply, done) = oneshot::channel();
        self.ops.send(Op::Sync(reply)).map_err(|_| Error::Closed)?;
        tokio::select! {
            () = self.stop.cancelled() => Err(Error::Closed),
            done = done => done.map_err(|_| Error::Closed),
        }
    }

    /// Sends `command` once a connection is up, resending it with the same id after each
    /// reconnect until the daemon answers. Fails at once with `Local` for a command too large
    /// to send.
    pub(crate) async fn send(&self, command: Command) -> Result<Answer, Error> {
        if self.demo.is_some() {
            return self.demo_answer(&command.body).map(Ok);
        }
        if let Some(error) = too_large(&command) {
            return Err(error);
        }
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
        if self.demo.is_some() {
            return Err(Error::Demo);
        }
        let (events, receiver) = mpsc::unbounded_channel();
        let (reply, answer) = oneshot::channel();
        let command = Command {
            id: new_command_id(),
            body,
        };
        if let Some(error) = too_large(&command) {
            return Err(error);
        }
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
    /// terminal per client. `assertion` goes with the first attach.
    pub(crate) async fn attach_terminal(
        self: &Arc<Self>,
        terminal_id: TerminalId,
        assertion: Option<Box<PasskeyAssertion>>,
    ) -> Result<TerminalStream, Error> {
        if self.demo.is_some() {
            return Err(Error::Demo);
        }
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
                assertion,
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
            messages.push(attach(terminal_id, attached.assertion.take(), attaching));
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
        Some(attach(terminal_id, attached.assertion.take(), attaching))
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
                assertion: None,
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

    /// Where the connection stands now. While the machine still shows a connection just
    /// lost ([`Self::lost`]), only a new connection shows: it takes over unseen.
    fn set_connection(&self, connection: ConnectionState) {
        let mut state = self.lock();
        if connection == ConnectionState::Connected {
            // The live values of the connection before are shown until this one sent its own.
            if state.hidden.take().is_some() {
                state.fresh = Some(Live::default());
            }
            state.quality.connected();
        } else if state.hidden.is_some() {
            state.hidden = Some(connection);
            return;
        } else {
            // The live values go with the connection; a new one sends them afresh.
            state.live = Live::default();
            state.fresh = None;
            state.address = None;
            state.quality.disconnected();
        }
        state.connection = Some(connection);
        drop(state);
        self.notify();
    }

    /// The connection ended with `error`. The machine goes on showing it, and what it
    /// brought, until [`Self::reveal`] or a new connection.
    fn lost(&self, error: String) {
        self.lock().hidden = Some(ConnectionState::Disconnected { error });
    }

    /// Shows the connection as it is, if the machine still showed one that was lost.
    fn reveal(&self) {
        let hidden = self.lock().hidden.take();
        if let Some(connection) = hidden {
            self.set_connection(connection);
        }
    }

    /// The daemon sent every live value it has on this connection: what the machine still
    /// shows of the connection before goes.
    fn settled(&self) {
        let mut state = self.lock();
        if let Some(fresh) = state.fresh.take() {
            state.live = fresh;
            self.notify_after(state);
        }
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

    /// The machine's addresses in the order to dial them: the one that answered last, then
    /// the rest in their order.
    fn dial_order(&self) -> Vec<String> {
        let state = self.lock();
        let mut addresses = state.addresses.clone();
        let answered = state.answered.as_ref();
        if let Some(index) = answered.and_then(|a| addresses.iter().position(|b| a == b)) {
            addresses[..=index].rotate_right(1);
        }
        addresses
    }

    /// Where this machine's `vault://` addresses lead.
    fn via(&self) -> Via {
        Via::new(self.saved.host_id.clone(), self.machines.clone())
    }

    /// Whether the current connection runs through a vault.
    fn through_vault(&self) -> bool {
        let state = self.lock();
        state
            .address
            .as_deref()
            .is_some_and(|address| address::vault(address).is_some())
    }

    /// The hosts this machine, a vault, says it relays clients to now.
    pub(crate) fn relays_to(&self) -> Vec<herder_protocol::HostId> {
        let state = self.lock();
        let relayed = state.hosts.iter().filter(|host| host.relay);
        relayed.map(|host| host.host_id.clone()).collect()
    }

    /// A connection to the first of the addresses before the current connection's that
    /// answers, which proves it reachable and is then dropped, before it said hello; `None`
    /// when the current connection uses the first address.
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
        let via = self.via();
        Some(Box::pin(async move {
            let config = client_config(&fingerprint, &device).map_err(|err| format!("{err:#}"))?;
            let connector = TlsConnector::from(Arc::new(config));
            let reaching = |address: String| {
                let (connector, via) = (connector.clone(), via.clone());
                async move { reach(&address, &connector, &via).await.map(drop) }
            };
            race(&better, reaching).await.map(|((), address)| address)
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
        if let Some(fresh) = &mut state.fresh {
            fresh.apply(&message);
        }
        if state.live.apply(&message) {
            return self.notify_after(state);
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
            ServerMessage::Providers { providers } => {
                state.providers = providers;
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
            ServerMessage::Error { error, .. } => {
                let machine = &self.saved.name;
                return warn!(%machine, "the daemon reported an error: {}", error.message);
            }
            // Hellos, answers, syncs and terminal messages are handled by the connection, and
            // the live values were taken in above.
            ServerMessage::HostResources(_)
            | ServerMessage::DaemonUpdate { .. }
            | ServerMessage::VaultStatus(_)
            | ServerMessage::ApprovalRequest(_)
            | ServerMessage::ApprovalClosed { .. }
            | ServerMessage::SkillsStatus(_)
            | ServerMessage::SessionSkills { .. }
            | ServerMessage::SessionResources { .. }
            | ServerMessage::Synced { .. }
            | ServerMessage::TerminalOutput { .. }
            | ServerMessage::TerminalClosed { .. }
            | ServerMessage::Hello(_)
            | ServerMessage::SignInChallenge { .. }
            | ServerMessage::LoginRequest { .. }
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
        if reader.first {
            // The first update carries every cached event, so it waits for the cache.
            let mut loaded = self.supervisor.loaded.subscribe();
            tokio::select! {
                () = self.supervisor.stop.cancelled() => return None,
                loaded = loaded.wait_for(|loaded| *loaded) => {
                    loaded.ok()?;
                }
            }
            // Loading it woke the log's watchers; this update covers that.
            reader.changed.borrow_and_update();
        } else {
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

/// What the machine's task is asked, in the order asked.
struct Ops {
    asked: mpsc::UnboundedReceiver<Op>,
    /// What a connection attempt read and left for the connection, or for the wait for the
    /// next attempt.
    kept: VecDeque<Op>,
}

impl Ops {
    /// The next thing asked, or `None` once nobody can ask any more.
    async fn recv(&mut self) -> Option<Op> {
        match self.kept.pop_front() {
            Some(op) => Some(op),
            None => self.asked.recv().await,
        }
    }
}

/// Why a connection ended.
enum Ended {
    Stopped,
    Lost(String),
    /// A probe found it dead; reconnect without a backoff, unless connections keep ending
    /// young.
    Replaced(String),
    /// Given up for a reason that says nothing of how the connection worked, as for an
    /// address that comes first or a suspension it was silent through; reconnect without a
    /// backoff.
    Dropped(String),
    /// The client asked for a new connection; reconnect without a backoff and report how
    /// that went.
    Reconnect(oneshot::Sender<Result<String, String>>),
}

impl Ended {
    /// What the daemon is told as the connection closes.
    fn said(&self) -> &str {
        match self {
            Self::Stopped => "the client stopped",
            Self::Lost(error) | Self::Replaced(error) | Self::Dropped(error) => error,
            Self::Reconnect(_) => "reconnecting, as asked",
        }
    }
}

/// How long to wait before the next connection, after one that was lost (`replaced`: found
/// dead by a probe) as the latest of `flaps` in a row that ended young; zero for none.
///
/// A connection that worked, and the first that did not, are replaced as fast as ever: at
/// once after a probe, else after the shortest backoff. From the second in a row on, the
/// wait doubles from a second or two up to [`BACKOFF_CAP`].
fn flap_wait(flaps: u32, replaced: bool) -> Duration {
    match flaps {
        0 | 1 if replaced => Duration::ZERO,
        0 | 1 => backoff(0),
        flaps => backoff(flaps.saturating_add(1)),
    }
}

async fn run(supervisor: Arc<Supervisor>, device: DeviceKey, ops: mpsc::UnboundedReceiver<Op>) {
    let mut ops = Ops {
        asked: ops,
        kept: VecDeque::new(),
    };
    let mut pending: Vec<Pending> = Vec::new();
    // Syncs waiting for their answer, by token, sent again on each new connection.
    let mut syncs: HashMap<String, oneshot::Sender<()>> = HashMap::new();
    // Reconnects asked for, answered once the next attempt connects or fails.
    let mut reconnects: Vec<oneshot::Sender<Result<String, String>>> = Vec::new();
    let mut attempt = 0;
    // Connections in a row that ended sooner than `STEADY` after they were made.
    let mut flaps: u32 = 0;
    let mut suspended = false;
    // Until when the machine shows a connection that was lost, once one was.
    let mut quiet: Option<Instant> = None;
    let saved = &supervisor.saved;
    let cached = tokio::select! {
        () = supervisor.stop.cancelled() => return,
        cached = supervisor.load() => cached,
    };
    supervisor.restore(cached);
    let device = &device;
    // One race of the machine's addresses, as they and the sessions wanted are now; of the
    // races that share `turn`, one connection says hello.
    let dial = |turn: &Turn| {
        let turn = Arc::clone(turn);
        let addresses = supervisor.dial_order();
        let hello = ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client: supervisor.client.clone(),
            resume: supervisor.cursors(),
            pairing_code: None,
            sign_in: false,
            passkey: supervisor.passkey,
        };
        let via = supervisor.via();
        async move { connect(&addresses, &saved.fingerprint, device, hello, &turn, &via).await }
    };
    'connection: loop {
        supervisor.set_connection(ConnectionState::Connecting);
        let connected = {
            // The races under way; the first connection any of them makes is taken.
            let mut races = FuturesUnordered::new();
            let turn = Turn::default();
            races.push(dial(&turn));
            let mut dialled = Instant::now();
            // When the next race starts, once one is asked for.
            let mut redial: Option<Instant> = None;
            loop {
                tokio::select! {
                    () = supervisor.stop.cancelled() => return,
                    () = until(quiet) => {
                        quiet = None;
                        supervisor.reveal();
                    }
                    Some(connected) = races.next() => match connected {
                        Ok(connected) => break Ok(connected),
                        Err(error) if races.is_empty() && redial.is_none() => break Err(error),
                        // Another race is under way, or about to start.
                        Err(_) => {}
                    },
                    () = async {
                        match redial {
                            Some(at) => tokio::time::sleep_until(at).await,
                            None => std::future::pending().await,
                        }
                    } => {
                        races.push(dial(&turn));
                        dialled = Instant::now();
                        redial = None;
                    }
                    op = ops.asked.recv() => match op {
                        // The network may have changed under the races so far: they are
                        // left to end as they will, and the addresses raced again.
                        Some(Op::Wake) => {
                            suspended = false;
                            attempt = 0;
                            redial = Some(dialled + REDIAL_SPACING);
                        }
                        Some(Op::Suspend) => {
                            suspended = true;
                            flaps = 0;
                        }
                        // What is raced changed: the races so far are dropped.
                        Some(Op::Readdress) => {
                            attempt = 0;
                            continue 'connection;
                        }
                        Some(Op::Reconnect(reply)) => {
                            reconnects.push(reply);
                            supervisor.lock().answered = None;
                            attempt = 0;
                            continue 'connection;
                        }
                        Some(op) => ops.kept.push_back(op),
                        None => return,
                    },
                }
            }
        };
        let failed = connected.is_err();
        let (error, wait) = match connected {
            Ok((ws, hello, address)) => {
                attempt = 0;
                let made = Instant::now();
                {
                    let mut state = supervisor.lock();
                    state.role = Some(hello.role);
                    state.user_id = Some(hello.user_id);
                    state.person_id = hello.person_id;
                    state.herder_version = hello.herder_version;
                    state.address = Some(address.clone());
                    state.answered = Some(address.clone());
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
                let error = ended.said().to_owned();
                let wait = match ended {
                    Ended::Stopped => return,
                    Ended::Lost(_) | Ended::Replaced(_) => {
                        // What ends in the background says little of the network the app
                        // comes back to.
                        flaps = if !suspended && made.elapsed() < STEADY {
                            flaps.saturating_add(1)
                        } else {
                            0
                        };
                        let wait = flap_wait(flaps, matches!(ended, Ended::Replaced(_)));
                        if !wait.is_zero() {
                            attempt = 1;
                        }
                        wait
                    }
                    Ended::Dropped(_) => Duration::ZERO,
                    Ended::Reconnect(reply) => {
                        reconnects.push(reply);
                        supervisor.lock().answered = None;
                        Duration::ZERO
                    }
                };
                quiet = Some(Instant::now() + QUIET);
                supervisor.lost(error.clone());
                (error, wait)
            }
            Err(error) => {
                supervisor.set_connection(ConnectionState::Disconnected {
                    error: error.clone(),
                });
                let wait = backoff(attempt);
                attempt = attempt.saturating_add(1);
                (error, wait)
            }
        };
        debug!(machine = %saved.name, flaps, "disconnected: {error}; next attempt in {wait:?}");
        if failed && !reconnects.is_empty() {
            // Whoever asked to reconnect is told it failed, so the machine shows it too.
            quiet = None;
            supervisor.reveal();
            for reply in reconnects.drain(..) {
                let _ = reply.send(Err(error.clone()));
            }
        }
        if wait.is_zero() {
            continue;
        }
        let wait = tokio::time::sleep(wait);
        tokio::pin!(wait);
        loop {
            tokio::select! {
                () = supervisor.stop.cancelled() => return,
                () = until(quiet) => {
                    quiet = None;
                    supervisor.reveal();
                }
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
                        supervisor.lock().answered = None;
                        attempt = 0;
                        break;
                    }
                    Some(Op::Suspend) => {
                        suspended = true;
                        flaps = 0;
                    }
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
                        syncs.insert(ulid::Ulid::generate().to_string(), reply);
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

/// Waits until `at`; forever for `None`.
async fn until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Saves to the offline cache off the async runtime.
fn save_in_background(supervisor: &Arc<Supervisor>) {
    let supervisor = Arc::clone(supervisor);
    tokio::task::spawn_blocking(move || supervisor.save());
}

/// Runs a connection until it fails or `stop`, then tells the daemon why it ended.
async fn serve(
    supervisor: &Arc<Supervisor>,
    ws: Ws,
    ops: &mut Ops,
    pending: &mut Vec<Pending>,
    syncs: &mut HashMap<String, oneshot::Sender<()>>,
    suspended: &mut bool,
) -> Ended {
    let alive = ws.get_ref().get_ref().0.alive.clone();
    let (sink, stream) = ws.split();
    // What the connection sends is queued here and written beside its reading.
    let (outgoing, queued) = mpsc::unbounded_channel();
    let mut writing = Box::pin(write(sink, queued));
    let mut link = Link {
        alive,
        stream,
        outgoing,
    };
    let ended = converse(
        supervisor,
        &mut link,
        writing.as_mut(),
        ops,
        pending,
        syncs,
        suspended,
    )
    .await;
    // Said beside whatever comes next: a connection that is dead takes no close frame, and
    // its replacement does not wait for it.
    let mut reason = ended.said();
    if let Some((cut, _)) = reason
        .char_indices()
        .find(|(at, c)| at + c.len_utf8() > CLOSE_REASON_BYTES)
    {
        reason = &reason[..cut];
    }
    let frame = CloseFrame {
        code: CloseCode::Normal,
        reason: reason.into(),
    };
    if link.outgoing.send(Message::Close(Some(frame))).is_ok() {
        // The queue closes behind the frame, which ends the writing once it is written.
        drop(link);
        tokio::spawn(async move {
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, writing).await;
        });
    }
    ended
}

/// A connection's two ways: what the daemon sends, and the queue of what it is sent.
struct Link {
    alive: Alive,
    stream: SplitStream<Ws>,
    outgoing: mpsc::UnboundedSender<Message>,
}

/// Runs a connection until it fails or `stop`.
async fn converse(
    supervisor: &Arc<Supervisor>,
    link: &mut Link,
    mut writing: Pin<&mut impl Future<Output = String>>,
    ops: &mut Ops,
    pending: &mut Vec<Pending>,
    syncs: &mut HashMap<String, oneshot::Sender<()>>,
    suspended: &mut bool,
) -> Ended {
    let Link {
        alive,
        stream,
        outgoing,
    } = link;
    let outgoing = &*outgoing;
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
    // Asked first, ahead of commands the daemon may take long over: its answer follows the
    // live values the daemon sends a new connection, and the replays the hello asked for.
    let settle = ulid::Ulid::generate().to_string();
    let settling = ClientMessage::Sync {
        token: settle.clone(),
    };
    let messages: Vec<_> = std::iter::once(settling)
        .chain(supervisor.reattach(&mut attaching))
        .chain(resent)
        .chain(synced)
        .collect();
    for message in messages {
        send(outgoing, &message);
    }
    // Its first tick is now: the first round trip is known as soon as the connection is up.
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // The payload of the latest steady ping and when it went out, until its pong comes back.
    let mut pinged: Option<(String, Instant)> = None;
    let mut save = tokio::time::interval(SAVE_INTERVAL);
    save.reset();
    let mut route_check = tokio::time::interval(ROUTE_CHECK);
    route_check.set_missed_tick_behavior(MissedTickBehavior::Delay);
    route_check.reset();
    // The payload of the ping a probe sent, and when it went out, until its pong comes back.
    let mut probe: Option<(String, Instant)> = None;
    // A connection to an address before this one's, which new addresses start; once one
    // answers, this connection is replaced to move there.
    let mut better: Option<BoxFuture<'static, Result<String, String>>> = None;
    loop {
        // When the connection counts as dead: each ping gives the daemon its time to show it
        // is there, counted from the ping or from its last sign of life, whichever is later.
        let heard = alive.at();
        let due = |ping: &Option<(String, Instant)>, limit: Duration| {
            ping.as_ref().map(|(_, sent)| heard.max(*sent) + limit)
        };
        let probe_due = due(&probe, PROBE_TIMEOUT);
        let dead = [probe_due, due(&pinged, SILENCE_LIMIT)]
            .into_iter()
            .flatten()
            .min();
        // In this order, so that a connection is found dead only with nothing left to read:
        // after a suspension, what the daemon sent meanwhile is read before the deadlines
        // that passed meanwhile are looked at.
        let message = tokio::select! {
            biased;
            () = supervisor.stop.cancelled() => return Ended::Stopped,
            error = &mut writing => return Ended::Lost(error),
            op = ops.recv() => {
                // What the user asks for and waits to see probes the connection.
                if matches!(op, Some(Op::Subscribe(_) | Op::Command(..) | Op::Open(..)))
                    && probe.is_none()
                {
                    probe = Some(send_ping(outgoing));
                }
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
                        let token = ulid::Ulid::generate().to_string();
                        syncs.insert(token.clone(), reply);
                        ClientMessage::Sync { token }
                    }
                    Some(Op::Suspend) => {
                        *suspended = true;
                        continue;
                    }
                    Some(Op::Wake) => {
                        *suspended = false;
                        // A running task would have found it dead by now.
                        if alive.at().elapsed() > PING_INTERVAL + SILENCE_LIMIT {
                            return Ended::Dropped(
                                "the connection was silent through a suspension".to_owned(),
                            );
                        }
                        if probe.is_none() {
                            probe = Some(send_ping(outgoing));
                        }
                        // The network may have changed: the machine may answer itself again.
                        if better.is_none() && supervisor.through_vault() {
                            better = supervisor.better();
                        }
                        continue;
                    }
                    Some(Op::Readdress) => {
                        let state = supervisor.lock();
                        if let Some(address) = &state.address
                            && !state.addresses.contains(address)
                        {
                            return Ended::Dropped(format!(
                                "{address} is no longer one of the machine's addresses"
                            ));
                        }
                        drop(state);
                        better = supervisor.better();
                        continue;
                    }
                    Some(Op::Reconnect(reply)) => return Ended::Reconnect(reply),
                };
                send(outgoing, &message);
                continue;
            }
            reached = async {
                match better.as_mut() {
                    Some(reaching) => reaching.await,
                    None => std::future::pending().await,
                }
            } => {
                match reached {
                    Ok(address) => {
                        // Dialled first, ahead of the address this connection answered at.
                        supervisor.lock().answered = Some(address.clone());
                        return Ended::Dropped(format!("moving to {address}, which comes first"));
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
            _ = route_check.tick() => {
                if better.is_none() && supervisor.through_vault() {
                    better = supervisor.better();
                }
                continue;
            }
            _ = ping.tick() => {
                if pinged.is_some() {
                    supervisor.missed_pong();
                }
                pinged = Some(send_ping(outgoing));
                continue;
            }
            frame = stream.next() => frame,
            () = async {
                match dead {
                    Some(dead) => tokio::time::sleep_until(dead).await,
                    None => std::future::pending().await,
                }
            } => {
                // Bytes that came or went while this waited show the daemon is there.
                if alive.at() != heard {
                    continue;
                }
                return if probe_due == dead {
                    Ended::Replaced("the daemon did not answer a probe".to_owned())
                } else {
                    Ended::Lost("the daemon stopped answering".to_owned())
                };
            }
        };
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
                let answered = [&pinged, &probe]
                    .into_iter()
                    .flatten()
                    .find(|(sent, _)| *payload == *sent.as_bytes())
                    .map(|(_, at)| *at);
                if let Some(at) = answered {
                    supervisor.pong(at.elapsed());
                    // A daemon with two pings to answer may answer only the later one: its
                    // pong answers the one sent before it too, which would else wait for a
                    // pong that never comes and take a healthy, idle connection for dead.
                    for ping in [&mut pinged, &mut probe] {
                        ping.take_if(|(_, sent)| *sent <= at);
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
                if token == settle {
                    supervisor.settled();
                }
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
        if let Some(message) = reply {
            send(outgoing, &message);
        }
    }
}

/// Queues `message` for the daemon, unless it is over [`MAX_MESSAGE_BYTES`]. A command that
/// waits for its answer never is (see [`too_large`]); that leaves what is sent unanswered,
/// such as a terminal's input, which is dropped.
fn send(outgoing: &mpsc::UnboundedSender<Message>, message: &ClientMessage) {
    // Client messages are plain data with string keys: encoding cannot fail.
    let text = serde_json::to_string(message).unwrap_or_default();
    if text.len() > MAX_MESSAGE_BYTES {
        warn!(bytes = text.len(), "dropped a message too large to send");
        return;
    }
    // The queue closes only once writing failed, which ends the connection.
    let _ = outgoing.send(Message::text(text));
}

/// Queues a ping with a fresh payload: the payload, and when it went out.
fn send_ping(outgoing: &mpsc::UnboundedSender<Message>) -> (String, Instant) {
    let payload = ulid::Ulid::generate().to_string();
    // As in `send`.
    let _ = outgoing.send(Message::Ping(payload.clone().into_bytes().into()));
    (payload, Instant::now())
}

/// Why `command` cannot be sent, if its message would be over [`MAX_MESSAGE_BYTES`]. The
/// daemon reads no larger one, and a command is resent on every new connection until it is
/// answered: one never answered would take down each connection in turn.
fn too_large(command: &Command) -> Option<Error> {
    const MB: usize = 1024 * 1024;
    let bytes = message_bytes(command);
    (bytes > MAX_MESSAGE_BYTES).then(|| Error::Local {
        message: format!(
            "too large to send: {} MB, and one message may have at most {} MB; \
             attach fewer or smaller images and files",
            bytes.div_ceil(MB),
            MAX_MESSAGE_BYTES / MB
        ),
    })
}

/// How many bytes the message `command` is sent as has.
fn message_bytes(command: &Command) -> usize {
    /// Counts the bytes written to it.
    struct Count(usize);

    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    // A command's message is the command with its tag as a `ClientMessage` ahead of its fields.
    let mut count = Count(r#""type":"command","#.len());
    // Neither can fail: a command is plain data with string keys, and counting always works.
    let _ = serde_json::to_writer(&mut count, command);
    count.0
}

/// Writes what is `queued` to the daemon, for as long as it is polled; ends with why a write
/// failed, or once the queue closed and all of it is written.
async fn write(
    mut sink: SplitSink<Ws, Message>,
    mut queued: mpsc::UnboundedReceiver<Message>,
) -> String {
    while let Some(message) = queued.recv().await {
        if let Err(err) = sink.send(message).await {
            return format!("writing to the daemon: {err}");
        }
    }
    "the connection was given up".to_owned()
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
            // This device opened it, so the daemon needs no assertion to attach it.
            message = Some(attach(terminal_id, None, attaching));
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
    assertion: Option<Box<PasskeyAssertion>>,
    attaching: &mut HashMap<CommandId, TerminalId>,
) -> ClientMessage {
    let id = new_command_id();
    attaching.insert(id.clone(), terminal_id.clone());
    ClientMessage::Command(Command {
        id,
        body: CommandBody::AttachTerminal {
            terminal_id: terminal_id.clone(),
            assertion,
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
/// and exchanges hellos; returns the address it connected to. Of the calls that share `turn`,
/// one connection says hello at a time, and none after one was answered.
///
/// The addresses race in the order given: each starts [`HEAD_START`] after the one before,
/// or as soon as every attempt so far failed, and the first to finish its hello wins. An
/// address that comes first wins whenever it answers about as fast as the rest, and a dead
/// one delays the next by [`HEAD_START`], not [`CONNECT_TIMEOUT`].
pub(crate) async fn connect(
    addresses: &[String],
    fingerprint: &str,
    device: &DeviceKey,
    hello: ClientHello,
    turn: &Turn,
    via: &Via,
) -> Result<(Ws, ServerHello, String), String> {
    match open_in_turn(addresses, fingerprint, device, hello, turn, via).await? {
        (ws, Opened::Hello(hello), address) => Ok((ws, *hello, address)),
        (_, Opened::SignIn { .. }, address) => Err(format!(
            "{address}: the daemon asked this device to sign in"
        )),
    }
}

/// What a daemon first answers a hello with.
pub(crate) enum Opened {
    /// It knows the device.
    Hello(Box<ServerHello>),
    /// It asks the device, which set `sign_in`, to sign in.
    SignIn {
        /// What an existing passkey signs.
        challenge: PasskeyChallenge,
        /// What a new passkey is created from, when the daemon takes a claim or an invite.
        registration: Option<PasskeyCreation>,
    },
}

/// [`connect`] by itself, also taking a daemon's request to sign in as an answer.
pub(crate) async fn open(
    addresses: &[String],
    fingerprint: &str,
    device: &DeviceKey,
    hello: ClientHello,
) -> Result<(Ws, Opened, String), String> {
    let (turn, via) = (Turn::default(), Via::default());
    open_in_turn(addresses, fingerprint, device, hello, &turn, &via).await
}

/// [`open`] among the calls that share `turn`.
///
/// An address races up to its WebSocket upgrade, then waits for its turn to say hello: the
/// one that got there first says it, and the rest only if that hello fails. A daemon does a
/// connection's work, the lists and every replay asked for, for each hello it takes, so one
/// that lost the race must not have said one.
async fn open_in_turn(
    addresses: &[String],
    fingerprint: &str,
    device: &DeviceKey,
    hello: ClientHello,
    turn: &Turn,
    via: &Via,
) -> Result<(Ws, Opened, String), String> {
    let config = client_config(fingerprint, device).map_err(|err| format!("{err:#}"))?;
    let connector = TlsConnector::from(Arc::new(config));
    let attempt = |address: String| {
        let (connector, hello, turn) = (connector.clone(), hello.clone(), Arc::clone(turn));
        async move {
            let mut ws = reach(&address, &connector, via).await?;
            // Held until this attempt is dropped: with its hello answered, that is once
            // every other attempt is gone.
            let _turn = turn.lock().await;
            let opened = tokio::time::timeout(HELLO_TIMEOUT, greet(&mut ws, &hello))
                .await
                .unwrap_or_else(|_| Err(anyhow!("no hello in {HELLO_TIMEOUT:?}")))?;
            Ok((ws, opened))
        }
    };
    race(addresses, attempt)
        .await
        .map(|((ws, opened), address)| (ws, opened, address))
}

/// What the first `attempt` on one of `addresses` to succeed made, and its address; else why
/// each failed. Each starts [`HEAD_START`] after the one before, or as soon as every attempt
/// so far failed. While an address has not answered yet, one that failed is tried again.
pub(crate) async fn race<T, A>(
    addresses: &[String],
    attempt: impl Fn(String) -> A,
) -> Result<(T, String), String>
where
    A: Future<Output = anyhow::Result<T>>,
{
    let attempt = |address: &String, after: Duration| {
        let (address, attempting) = (address.clone(), attempt(address.clone()));
        async move {
            tokio::time::sleep(after).await;
            (address, attempting.await)
        }
    };
    let mut queue = addresses.iter();
    let distinct = addresses.iter().collect::<HashSet<_>>().len();
    let mut running = FuturesUnordered::new();
    // Each address that failed, in the order they first did: its latest error, and how many
    // times it failed.
    let mut errors: Vec<(String, String, u32)> = Vec::new();
    let next_start = tokio::time::sleep(HEAD_START);
    tokio::pin!(next_start);
    loop {
        if running.is_empty() {
            let Some(address) = queue.next() else { break };
            running.push(attempt(address, Duration::ZERO));
            next_start.as_mut().reset(Instant::now() + HEAD_START);
        }
        let waiting = !queue.as_slice().is_empty();
        tokio::select! {
            Some((address, result)) = running.next() => match result {
                Ok(made) => return Ok((made, address)),
                Err(err) => {
                    let error = format!("{address}: {err:#}");
                    let failed = match errors.iter_mut().find(|(failed, ..)| *failed == address) {
                        Some(known) => {
                            known.1 = error;
                            known.2 += 1;
                            known.2
                        }
                        None => {
                            errors.push((address.clone(), error, 1));
                            1
                        }
                    };
                    // Every address failed: the race is over, whatever is tried again.
                    if errors.len() == distinct {
                        break;
                    }
                    // An address that has not answered yet, as one whose packets are lost
                    // does not for the whole connect timeout, keeps the race open. One that
                    // failed meanwhile is tried again after the backoff's waits, not only
                    // once that timeout is over: a daemon that restarts refuses for a
                    // moment, and a connection cut while it is made fails once.
                    let wait = BACKOFF_BASE.saturating_mul(2u32.saturating_pow(failed - 1));
                    running.push(attempt(&address, wait));
                }
            },
            () = &mut next_start, if waiting => {
                if let Some(address) = queue.next() {
                    running.push(attempt(address, Duration::ZERO));
                    next_start.as_mut().reset(Instant::now() + HEAD_START);
                }
            }
        }
    }
    if errors.is_empty() {
        return Err("the machine has no address".to_owned());
    }
    let errors: Vec<String> = errors.into_iter().map(|(_, error, _)| error).collect();
    Err(errors.join("; "))
}

/// A connection to the daemon at `address`, up to its WebSocket upgrade: the daemon is
/// there, and was told nothing yet. An address through a vault leads where `via` says.
async fn reach(address: &str, connector: &TlsConnector, via: &Via) -> anyhow::Result<Ws> {
    let reaching = async {
        match address::vault(address) {
            Some(vault) => {
                let stream = via.stream(&vault).await?;
                secure(Box::new(stream), connector, None).await
            }
            None => direct(address, connector, None).await,
        }
    };
    tokio::time::timeout(CONNECT_TIMEOUT, reaching)
        .await
        .unwrap_or_else(|_| Err(anyhow!("no answer in {CONNECT_TIMEOUT:?}")))
}

/// A connection to the daemon at `address`, its own or a tunnel's, up to its WebSocket
/// upgrade, with the WebSocket configured as `config` says.
pub(crate) async fn direct(
    address: &str,
    connector: &TlsConnector,
    config: Option<WebSocketConfig>,
) -> anyhow::Result<Ws> {
    let stream: Box<dyn Io> = if address::is_tunnel(address) {
        Box::new(tunnel::open(address).await?)
    } else {
        let tcp = TcpStream::connect(address).await.context("connecting")?;
        tcp.set_nodelay(true)?;
        Box::new(tcp)
    };
    secure(stream, connector, config).await
}

/// The TLS, pinned through `connector`, and the WebSocket upgrade of a connection to a
/// daemon on `stream`.
async fn secure(
    stream: Box<dyn Io>,
    connector: &TlsConnector,
    config: Option<WebSocketConfig>,
) -> anyhow::Result<Ws> {
    // The certificate is pinned by fingerprint, so the name is never checked.
    let name = ServerName::try_from("herder").context("the TLS server name")?;
    let tls = connector
        .connect(name, Watched::new(stream))
        .await
        .context("TLS")?;
    // The daemon reads nothing of the URL: inside a tunnel, its own address is as good.
    let request = "wss://herder/".into_client_request()?;
    let (ws, _) = tokio_tungstenite::client_async_with_config(request, tls, config)
        .await
        .context("the WebSocket upgrade")?;
    Ok(ws)
}

/// Says `hello` on `ws`, and takes what the daemon answers.
async fn greet(ws: &mut Ws, hello: &ClientHello) -> anyhow::Result<Opened> {
    let text = serde_json::to_string(&ClientMessage::Hello(hello.clone()))?;
    ws.send(Message::text(text)).await?;
    match receive(ws).await? {
        ServerMessage::Hello(hello) if hello.protocol_version == PROTOCOL_VERSION => {
            Ok(Opened::Hello(Box::new(hello)))
        }
        ServerMessage::Hello(hello) => Err(anyhow!(
            "the daemon speaks protocol {}, this client {PROTOCOL_VERSION}",
            hello.protocol_version
        )),
        ServerMessage::SignInChallenge {
            challenge,
            registration,
        } if hello.sign_in => Ok(Opened::SignIn {
            challenge,
            registration,
        }),
        ServerMessage::Error { error, .. } => Err(anyhow!(error.message)),
        _ => Err(anyhow!("the daemon did not say hello")),
    }
}

/// The next message the daemon sends on `ws`.
pub(crate) async fn receive(ws: &mut Ws) -> anyhow::Result<ServerMessage> {
    loop {
        let text = match ws.next().await {
            Some(Ok(Message::Text(text))) => text,
            Some(Ok(Message::Close(_))) | None => bail!("the daemon closed the connection"),
            Some(Ok(_)) => continue,
            Some(Err(err)) => return Err(err.into()),
        };
        return Ok(serde_json::from_str(&text)?);
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
    use herder_protocol::{Bytes, Image};

    use super::*;

    /// A prompt whose message has exactly `bytes` bytes.
    fn prompt(bytes: usize) -> Command {
        let command = |text: String| Command {
            id: CommandId::new("c1"),
            body: CommandBody::SendPrompt {
                session_id: SessionId::new("s1"),
                text,
                images: Vec::new(),
                files: Vec::new(),
            },
        };
        let empty = message_bytes(&command(String::new()));
        command("x".repeat(bytes - empty))
    }

    #[test]
    fn a_command_is_measured_as_the_message_it_is_sent_as() {
        let command = Command {
            id: CommandId::new("c1"),
            body: CommandBody::SendPrompt {
                session_id: SessionId::new("s1"),
                text: "see \"this\"\n".to_owned(),
                images: vec![Image {
                    media_type: "image/png".to_owned(),
                    data: Bytes(vec![7; 1000]),
                }],
                files: Vec::new(),
            },
        };
        let sent = serde_json::to_string(&ClientMessage::Command(command.clone())).unwrap();
        assert_eq!(message_bytes(&command), sent.len());
    }

    #[test]
    fn only_a_command_over_the_message_limit_is_too_large_to_send() {
        assert_eq!(too_large(&prompt(MAX_MESSAGE_BYTES)), None);
        let Some(Error::Local { message }) = too_large(&prompt(MAX_MESSAGE_BYTES + 1)) else {
            panic!("expected the command refused");
        };
        assert_eq!(
            message,
            "too large to send: 17 MB, and one message may have at most 16 MB; \
             attach fewer or smaller images and files"
        );
    }

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
    fn connections_that_keep_ending_young_wait_longer_each_time() {
        // One that worked, and the first that did not, are replaced as fast as ever.
        for flaps in [0, 1] {
            assert_eq!(flap_wait(flaps, true), Duration::ZERO);
            assert!(flap_wait(flaps, false) <= BACKOFF_BASE);
        }
        // From the second in a row on, however it was found dead.
        for replaced in [true, false] {
            let mut least = Duration::from_secs(1);
            for flaps in 2..7 {
                let wait = flap_wait(flaps, replaced);
                assert!(wait >= least.min(BACKOFF_CAP / 2), "{flaps}: {wait:?}");
                assert!(wait <= (least * 2).min(BACKOFF_CAP), "{flaps}: {wait:?}");
                least *= 2;
            }
            assert!(flap_wait(u32::MAX, replaced) >= BACKOFF_CAP / 2);
        }
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
