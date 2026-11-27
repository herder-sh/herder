//! The client on a hostile network, checked against what an app would show.
//!
//! A real daemon with the fake adapter, the client under test and a second client that reaches
//! the daemon directly all run on one thread, on tokio's paused clock: hundreds of seconds of
//! network trouble take a few real ones. Between the client under test and the daemon sits
//! [`Net`], a relay over real sockets with latency, jitter, a bandwidth limit, lost packets,
//! a radio that idles, freezes, cuts in the middle of a frame, resets, connections gone silent
//! with no FIN, routes that refuse or lose new connections, and two addresses that go in and
//! out of reach.
//!
//! [`slow_network`] runs the app on real slow networks: EDGE, 3G, an 800 ms round trip, 2%
//! loss and bufferbloat. A connection that carries everything late must never be taken for
//! dead there; a client that waited a fixed second or two for a pong fails them.
//!
//! While a session streams turns and a user sends prompts and commands, [`watch_machine`] and
//! [`watch_session`] read what the public `Client` API shows, as an app does, and note
//! whatever an app must never show:
//!
//! - a list (sessions, hosts, projects, accounts, providers) that shrank;
//! - a session event out of order, twice or missing, or streaming text gone before the event
//!   that makes it durable;
//! - a machine that reads other than `Connected` outside a long outage or a suspension, or
//!   for longer after one than the client is given to come back;
//! - a live value blanked while the machine reads `Connected`.
//!
//! Each test also checks that every command was applied exactly once, that the client made no
//! more connections than its troubles account for, and that once the network heals the app
//! shows what the direct client shows, within a bound.
//!
//! A drop counts as short, and must never show, when the client can mend it within the five
//! seconds it rides a drop out for: [`World::trouble`] says for each trouble when it may
//! show, and why.
//!
//! The same holds on the route through a hub ([`World::relayed`]): the app reaches the
//! hub over one such network and the daemon reaches the hub over another, so a drop can
//! hit the app's leg, the host's leg, or both at once, as a hub that restarts does.
//!
//! The soak is driven by a seed, printed first with every trouble and every connection state
//! the machine showed: `HERDER_SOAK_SEEDS=7,8 HERDER_SOAK_SECONDS=3600 cargo test -p
//! herder-client-core --test bad_network soak_from_env -- --nocapture` runs seeds 7 and 8 for
//! an hour of network time each. A seed's troubles are always the same; the backoff's jitter
//! and the order ready tasks run in are not, so a failure may take a few runs to see again.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use herder_adapters::fake::FakeAdapter;
use herder_client_core::{Client, ConnectionState, Machine, SessionSubscription};
use herder_daemon::auth::{Auth, PAIRING_TTL, authenticator};
use herder_daemon::aws::Aws;
use herder_daemon::config::{HubConfig, Retention};
use herder_daemon::hub::{self, HostSecrets, HubStore, LIVENESS_TIMEOUT, Replicator};
use herder_daemon::login::Logins;
use herder_daemon::session::{AccountConfig, Accounts, Adapters, SessionManager, Setup};
use herder_daemon::terminal::Terminals;
use herder_daemon::worktree::Worktrees;
use herder_daemon::ws::{Host, Server, Tls};
use herder_daemon::{Fanout, session};
use herder_protocol::{
    AccountId, CommandBody, CommandResult, Event, EventBody, FailoverSettings, FleetHost, HostId,
    HostResources, ItemBody, ItemId, PermissionMode, Project, ProjectId, Provider, SessionId,
    TurnId,
};
use herder_store::Store;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, mpsc, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const fn secs(secs: u64) -> Duration {
    Duration::from_secs(secs)
}

const fn ms(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

/// How long the client shows a lost connection as connected (`QUIET` in the supervisor).
const QUIET: Duration = secs(5);

/// How long a connection must last for the client to replace it at once when it ends
/// (`STEADY` in the supervisor): from the second in a row that ended sooner, it waits first.
const STEADY: Duration = secs(30);

/// How soon after a wake a client that showed no connection shows one again.
const WAKE_BOUND: Duration = secs(6);

/// How soon after an outage a client that was not woken shows a connection again: the longest
/// wait between attempts, and a connection made over a bad link.
const RETRY_BOUND: Duration = secs(36);

/// How soon after the network healed the client shows what the daemon has: a connection that
/// died silently is found by its pings within 15 s, and replaced.
const CONVERGE_BOUND: Duration = secs(25);

/// A small seeded generator (xorshift64*), so a run is its seed.
#[derive(Clone)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number from `low` to `high`, both included.
    fn range(&mut self, low: u64, high: u64) -> u64 {
        low + self.next() % (high - low + 1)
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.range(1, 100) <= percent
    }

    fn pick<T: Copy>(&mut self, of: &[T]) -> T {
        of[self.range(0, of.len() as u64 - 1) as usize]
    }
}

/// What the link does to the bytes it carries, each way.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Link {
    latency_ms: u64,
    jitter_ms: u64,
    /// Bytes a second; `None` for no limit.
    rate: Option<u64>,
    /// Percent of the pieces lost on the way. TCP sends a lost piece again once its
    /// retransmission timer fires, a round trip and 200 ms, twice as long each time it is lost
    /// again; whatever was sent after it waits for it.
    loss: u64,
    /// How long a phone's radio that went idle ([`RADIO_IDLE`]) takes to come up again for the
    /// next piece either way, as a 3G or EDGE radio's does.
    wake_ms: u64,
}

/// How long a radio carries nothing before it goes idle, and has to come up for the next piece.
const RADIO_IDLE: Duration = secs(5);

impl Link {
    const CLEAN: Self = Self {
        latency_ms: 2,
        jitter_ms: 0,
        rate: None,
        loss: 0,
        wake_ms: 0,
    };

    /// Whether making a connection over it takes most of the five seconds a drop is ridden
    /// out for, so that a drop may show however soon the network is back: three round trips
    /// and some 5 kB take up to 1.6 s on a link that is not slow, and over 4 s on the slowest.
    fn slow(self) -> bool {
        self.latency_ms + self.jitter_ms > 180 || self.rate.is_some_and(|rate| rate < 20_000)
    }
}

/// What an address does with a new connection.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Route {
    /// Carries it.
    Up,
    /// Resets it at once, as a host with nothing listening does.
    Refused,
    /// Leaves it unanswered until the route is up again, as a network that drops packets does
    /// to a SYN that TCP sends again.
    Lost,
}

/// How a connection dies.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kill {
    /// Closed, with a FIN.
    Fin,
    /// Closed halfway through what the daemon sends next: half a frame arrives.
    Torn,
    /// Reset, with an RST.
    Reset,
    /// Half-open: both ends stay open and nothing goes through any more, with no FIN, as
    /// after a NAT forgot the connection or an OS killed a suspended app's socket.
    Silent,
}

/// The network between the client and the daemon: a relay on two addresses.
struct Net {
    target: SocketAddr,
    addrs: [SocketAddr; 2],
    link: watch::Sender<Link>,
    /// While set nothing is delivered; what was sent arrives once it is cleared, as after
    /// packets lost and sent again.
    frozen: watch::Sender<bool>,
    routes: [watch::Sender<Route>; 2],
    /// The latest kill: its number, the address whose connections die (`None` for both), how.
    kills: watch::Sender<(u64, Option<usize>, Kill)>,
    accepted: AtomicU64,
    /// Bytes delivered, both ways.
    carried: AtomicU64,
}

impl Net {
    async fn start(target: SocketAddr, seed: u64) -> Arc<Self> {
        let mut listeners = Vec::new();
        for _ in 0..2 {
            listeners.push(TcpListener::bind("127.0.0.1:0").await.unwrap());
        }
        let addr = |index: usize| listeners[index].local_addr().unwrap();
        let net = Arc::new(Self {
            target,
            addrs: [addr(0), addr(1)],
            link: watch::Sender::new(Link::CLEAN),
            frozen: watch::Sender::new(false),
            routes: [watch::Sender::new(Route::Up), watch::Sender::new(Route::Up)],
            kills: watch::Sender::new((0, None, Kill::Fin)),
            accepted: AtomicU64::new(0),
            carried: AtomicU64::new(0),
        });
        for (path, listener) in listeners.into_iter().enumerate() {
            let net = Arc::clone(&net);
            tokio::spawn(async move {
                while let Ok((inbound, _)) = listener.accept().await {
                    let net = Arc::clone(&net);
                    tokio::spawn(async move {
                        // The client's connections start with a TLS handshake record. Whatever
                        // else finds the port, as tools that probe a machine's listening ports
                        // do, is not the client's doing and is dropped.
                        let mut first = [0; 1];
                        if !matches!(inbound.peek(&mut first).await, Ok(1)) || first[0] != 0x16 {
                            eprintln!("dropped a connection that is not the client's");
                            return;
                        }
                        let number = net.accepted.fetch_add(1, Ordering::SeqCst);
                        let rng = Rng::new(seed ^ (number + 1).wrapping_mul(0xA24B_AED4_963E_E407));
                        carry(net, path, inbound, rng).await;
                    });
                }
            });
        }
        net
    }

    fn kill(&self, path: Option<usize>, kill: Kill) {
        self.kills
            .send_modify(|latest| *latest = (latest.0 + 1, path, kill));
    }

    fn accepted(&self) -> u64 {
        self.accepted.load(Ordering::SeqCst)
    }
}

/// Carries one connection of the client to the daemon until it ends or is killed.
async fn carry(net: Arc<Net>, path: usize, mut inbound: TcpStream, mut rng: Rng) {
    let mut kills = net.kills.subscribe();
    // When a piece last went either way: the phone's radio.
    let radio = Mutex::new(Instant::now());
    let torn = AtomicBool::new(false);
    let mut outbound = None;
    let kill = {
        let carried = async {
            let mut route = net.routes[path].subscribe();
            loop {
                let now = *route.borrow_and_update();
                match now {
                    Route::Up => break,
                    Route::Refused => return Kill::Reset,
                    Route::Lost => {}
                }
                if route.changed().await.is_err() {
                    return Kill::Fin;
                }
            }
            let Ok(daemon) = TcpStream::connect(net.target).await else {
                return Kill::Reset;
            };
            // The relay passes on small pieces in quick succession. Nagle's algorithm would
            // hold each back until the one before is acknowledged, which the other end may
            // put off for 40 ms of real time: hours, on this clock.
            let _ = (inbound.set_nodelay(true), daemon.set_nodelay(true));
            let daemon = outbound.insert(daemon);
            let (from_client, to_client) = inbound.split();
            let (from_daemon, to_daemon) = daemon.split();
            let (up, down) = (Rng::new(rng.next()), Rng::new(rng.next()));
            tokio::select! {
                () = pipe(&net, &radio, from_client, to_daemon, up, None) => {}
                () = pipe(&net, &radio, from_daemon, to_client, down, Some(&torn)) => {}
            }
            Kill::Fin
        };
        tokio::pin!(carried);
        // A connection to tear is closed anyway when the daemon sends nothing to tear.
        let mut tear_by: Option<Instant> = None;
        loop {
            tokio::select! {
                kill = &mut carried => break kill,
                () = async {
                    match tear_by {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => break Kill::Fin,
                changed = kills.changed() => {
                    if changed.is_err() {
                        break Kill::Fin;
                    }
                    let (_, target, kill) = *kills.borrow_and_update();
                    if target.is_some_and(|target| target != path) {
                        continue;
                    }
                    if kill != Kill::Torn {
                        break kill;
                    }
                    torn.store(true, Ordering::SeqCst);
                    tear_by = Some(Instant::now() + secs(2));
                }
            }
        }
    };
    match kill {
        // Dropping the sockets closes them.
        Kill::Fin | Kill::Torn => {}
        Kill::Reset => {
            let _ = inbound.set_zero_linger();
            if let Some(daemon) = &outbound {
                let _ = daemon.set_zero_linger();
            }
        }
        Kill::Silent => std::future::pending().await,
    }
}

/// Carries bytes one way as the link does: read at its rate, delivered after its latency and
/// jitter in the order they were sent, late when lost or the radio idled, and held while the
/// network is frozen. A limited link
/// passes them on in pieces a twentieth of a second long, so bytes keep coming, and what it
/// has not read yet waits in the sender's socket. With `torn` set, the next piece is cut in
/// half and the connection ends there.
async fn pipe(
    net: &Net,
    radio: &Mutex<Instant>,
    mut from: ReadHalf<'_>,
    mut to: WriteHalf<'_>,
    mut rng: Rng,
    torn: Option<&AtomicBool>,
) {
    let (queue, mut queued) = mpsc::unbounded_channel::<(Instant, Vec<u8>)>();
    let reading = async move {
        let mut last = Instant::now();
        loop {
            let link = *net.link.borrow();
            let piece = link
                .rate
                .map_or(16 * 1024, |rate| (rate / 20).clamp(64, 4096));
            let mut bytes = vec![0; piece as usize];
            let read = match from.read(&mut bytes).await {
                Ok(0) | Err(_) => return,
                Ok(read) => read,
            };
            bytes.truncate(read);
            if let Some(rate) = link.rate {
                tokio::time::sleep(Duration::from_micros(read as u64 * 1_000_000 / rate)).await;
            }
            let link = *net.link.borrow();
            let mut delay = link.latency_ms + rng.range(0, link.jitter_ms);
            let now = Instant::now();
            let idled = now - std::mem::replace(&mut *radio.lock().unwrap(), now) > RADIO_IDLE;
            if idled {
                delay += link.wake_ms;
            }
            // Checked only on a lossy link, so that the jitter of any other stays its seed's.
            let mut retransmit = 2 * link.latency_ms + link.jitter_ms + 200;
            while link.loss > 0 && rng.chance(link.loss) {
                delay += retransmit;
                retransmit *= 2;
            }
            last = last.max(Instant::now() + ms(delay));
            if queue.send((last, bytes)).is_err() {
                return;
            }
        }
    };
    let writing = async {
        let mut frozen = net.frozen.subscribe();
        while let Some((at, bytes)) = queued.recv().await {
            tokio::time::sleep_until(at).await;
            if frozen.wait_for(|frozen| !frozen).await.is_err() {
                return;
            }
            if torn.is_some_and(|torn| torn.load(Ordering::SeqCst)) && bytes.len() > 1 {
                let _ = to.write_all(&bytes[..bytes.len() / 2]).await;
                return;
            }
            if to.write_all(&bytes).await.is_err() {
                return;
            }
            net.carried.fetch_add(bytes.len() as u64, Ordering::SeqCst);
        }
    };
    tokio::pin!(writing);
    tokio::select! {
        () = &mut writing => return,
        () = reading => {}
    }
    // The sender closed: what it sent before still arrives.
    writing.await;
}

fn account() -> AccountId {
    AccountId::new("account-1")
}

/// A daemon on the test's own runtime, so that it runs on the same paused clock.
struct Daemon {
    addr: SocketAddr,
    fingerprint: String,
    auth: Arc<Auth>,
    fanout: Arc<Fanout>,
    shutdown: CancellationToken,
    serving: tokio::task::JoinHandle<()>,
}

impl Daemon {
    /// Starts a daemon on `dir` whose sessions run `script`; its turns are `turn-1`, `turn-2`
    /// and so on. Linked to `hub`, it takes the connections that hub relays; it backs
    /// nothing up there.
    async fn start(dir: &Path, script: PathBuf, hub: Option<HubConfig>) -> Self {
        std::fs::create_dir_all(dir.join("tls")).unwrap();
        let tls = Tls::load_or_create(&dir.join("tls"), "test-host").unwrap();
        let auth = Arc::new(Auth::open(dir).unwrap());
        let fanout = Arc::new(Fanout::default());
        fanout.set_failover(FailoverSettings { pin: true });
        let fake = Provider::Other("fake".into());
        let mut adapters = Adapters::new();
        adapters.register(fake.clone(), Arc::new(FakeAdapter::new(script)));
        let mut accounts = Accounts::new();
        accounts.insert(
            account(),
            AccountConfig {
                provider: fake,
                label: "Account 1".into(),
                config_dir: Some(dir.join("account")),
                fallback: false,
                endpoint: None,
            },
        );
        let turns = AtomicU64::new(0);
        let setup = Setup {
            store: Store::open(dir.join("herder.db")).unwrap(),
            adapters,
            accounts,
            sink: Arc::clone(&fanout) as Arc<dyn session::EventSink>,
            turn_ids: Box::new(move || {
                TurnId::new(format!("turn-{}", turns.fetch_add(1, Ordering::SeqCst) + 1))
            }),
            worktrees: Worktrees::new(dir.join("worktrees")),
            attachments: dir.join("attachments"),
            chats: dir.join("chats"),
        };
        let shutdown = CancellationToken::new();
        let sessions = SessionManager::open(setup, shutdown.clone()).await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let fingerprint = tls.fingerprint().to_owned();
        let host = Host {
            name: "test-host-1".into(),
            id: HostId::new("host-1"),
        };
        let terminals = Terminals::new(Arc::clone(&fanout), PathBuf::from("/bin/sh"));
        let logins = Logins::new(HashMap::new(), dir.join("daemon.toml"), sessions.clone());
        let server = Server::new(
            tls,
            Arc::clone(&auth),
            Arc::clone(&fanout),
            sessions.clone(),
            terminals,
            logins,
            host.clone(),
        );
        server.manage_aws(Aws::from_env(|_| None)).unwrap();
        if let Some(hub) = hub {
            let relayed = Arc::new(OnceLock::new());
            assert!(relayed.set(server.relayed(shutdown.clone())).is_ok());
            let replicator = Replicator {
                hub,
                device: Replicator::device_key(dir).unwrap(),
                host,
                sessions,
                changed: Arc::new(Notify::new()),
                secrets: Arc::new(HostSecrets::default()),
                relayed,
            };
            tokio::spawn(replicator.run(shutdown.clone()));
        }
        let stop = shutdown.clone();
        let serving = tokio::spawn(async move { server.run(vec![listener], stop).await });
        Self {
            addr,
            fingerprint,
            auth,
            fanout,
            shutdown,
            serving,
        }
    }
}

/// A hub on the test's own runtime, which the daemon links to and the app reaches the
/// daemon through.
struct Hub {
    addr: SocketAddr,
    fingerprint: String,
    auth: Arc<Auth>,
}

impl Hub {
    async fn start(dir: &Path) -> Self {
        std::fs::create_dir_all(dir.join("tls")).unwrap();
        let tls = Tls::load_or_create(&dir.join("tls"), "test-hub").unwrap();
        let auth = Arc::new(Auth::open(dir).unwrap());
        let store = HubStore::open(dir.join("hub.db")).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let fingerprint = tls.fingerprint().to_owned();
        let host = Host {
            name: "test-hub".into(),
            id: HostId::new("hub-1"),
        };
        let server = hub::Server::new(
            tls,
            Arc::clone(&auth),
            store,
            host,
            LIVENESS_TIMEOUT,
            Retention::default(),
            None,
        );
        tokio::spawn(server.run(vec![listener], CancellationToken::new()));
        Self {
            addr,
            fingerprint,
            auth,
        }
    }
}

fn host_resources() -> HostResources {
    HostResources {
        cpu_cores: 8,
        cpu_percent: 42.5,
        load_1m: 3.2,
        memory_total_bytes: 16 << 30,
        memory_available_bytes: 6 << 30,
        pressure: None,
        running_turns: 1,
        max_turns: 4,
        waiting_turns: 0,
        constraint: None,
    }
}

/// A git repository with one commit, for a session to work on.
fn repo(dir: &Path) -> String {
    std::fs::create_dir(dir).unwrap();
    for args in [
        &["init", "--quiet", "--initial-branch=main"][..],
        &["commit", "--quiet", "--allow-empty", "-m", "init"],
    ] {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .status()
            .unwrap();
        assert!(status.success());
    }
    dir.to_str().unwrap().to_owned()
}

/// Writes a fake-adapter script of `turns` turns, each answering `Prompt <n>.` with a short
/// message and one that streams for a few seconds. With `big`, the first turn instead adds
/// that many bytes of messages at once, for a replay that takes a slow link long.
fn script(dir: &Path, rng: &mut Rng, turns: u64, big: Option<usize>) -> PathBuf {
    let mut steps = Vec::new();
    let message = |id: &str, turn: &str, text: &str| json!({"id": id, "turn_id": turn, "body": {"type": "assistant_message", "text": text}});
    for number in 1..=turns {
        let turn = format!("turn-{number}");
        let prompt = format!("Prompt {number}.");
        steps.push(json!({"expect": {"type": "send_prompt", "turn_id": turn, "text": prompt}}));
        steps.push(json!({"emit": {"type": "turn_started", "turn_id": turn}}));
        if let Some(bytes) = big.filter(|_| number == 1) {
            for part in 0..40 {
                let item = message(&format!("big-{part}"), &turn, &"x".repeat(bytes / 40));
                steps.push(json!({"emit": {"type": "item_completed", "item": item}}));
                steps.push(json!({"sleep_ms": 20}));
            }
            steps.push(json!({"emit": {"type": "turn_completed", "turn_id": turn}}));
            continue;
        }
        let first = message(&format!("item-{number}-1"), &turn, "On it.");
        steps.push(json!({"emit": {"type": "item_completed", "item": first}}));
        let id = format!("item-{number}-2");
        steps.push(json!({"emit": {"type": "item_started", "item": message(&id, &turn, "")}}));
        let mut text = String::new();
        for delta in 0..rng.range(4, 9) {
            steps.push(json!({"sleep_ms": rng.range(150, 900)}));
            let mut piece = format!("{number}.{delta} ");
            if rng.chance(25) {
                piece.push_str(&"y".repeat(rng.range(200, 3000) as usize));
            }
            text.push_str(&piece);
            steps.push(json!({"emit": {"type": "item_delta", "item_id": id, "text": piece}}));
        }
        steps.push(json!({"sleep_ms": rng.range(50, 400)}));
        let done = message(&id, &turn, &text);
        steps.push(json!({"emit": {"type": "item_completed", "item": done}}));
        steps.push(json!({"emit": {"type": "turn_completed", "turn_id": turn}}));
    }
    let path = dir.join("script.jsonl");
    let lines: Vec<String> = steps.iter().map(ToString::to_string).collect();
    std::fs::write(&path, lines.join("\n")).unwrap();
    path
}

/// What an app showed: what went wrong in it, and the session's events as they arrived.
struct Seen {
    start: Instant,
    wrong: Mutex<Vec<String>>,
    /// When the machine may read other than `Connected`: long outages and suspensions, each
    /// until the client is bound to be back.
    excused: Mutex<Vec<(Instant, Instant)>>,
    /// How many times the machine went from `Connected` to anything else.
    shows: AtomicU64,
    /// When the machine last showed a new connection.
    made: Mutex<Instant>,
    events: Mutex<Vec<Event>>,
    /// How many turns have ended in `events`.
    ended: watch::Sender<u64>,
}

impl Seen {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            start: Instant::now(),
            wrong: Mutex::default(),
            excused: Mutex::default(),
            shows: AtomicU64::new(0),
            made: Mutex::new(Instant::now()),
            events: Mutex::default(),
            ended: watch::Sender::new(0),
        })
    }

    fn wrong(&self, what: String) {
        let at = self.start.elapsed().as_secs_f32();
        self.wrong
            .lock()
            .unwrap()
            .push(format!("{at:.3} s: {what}"));
    }

    fn excused(&self, at: Instant) -> bool {
        let excused = self.excused.lock().unwrap();
        excused
            .iter()
            .any(|(from, until)| *from <= at && at < *until)
    }

    fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }

    fn shows(&self) -> u64 {
        self.shows.load(Ordering::SeqCst)
    }
}

fn connected(machine: &Machine) -> bool {
    machine.connection == ConnectionState::Connected
}

/// Reads the machine on every change, as an app's lists and status bar do, and notes what
/// they must never show.
async fn watch_machine(app: Client, seen: Arc<Seen>) {
    let changes = app.changes();
    let mut before: Option<Machine> = None;
    let mut noted: Option<ConnectionState> = None;
    loop {
        let machine = app.machines().remove(0);
        let excused = seen.excused(Instant::now());
        let up = connected(&machine);
        if before
            .as_ref()
            .is_none_or(|before| before.connection != machine.connection)
        {
            let at = seen.start.elapsed().as_secs_f32();
            eprintln!("{at:9.3} s:     the machine shows {:?}", machine.connection);
        }
        // Noted once for as long as it shows.
        if up || excused {
            noted = None;
        } else if noted.as_ref() != Some(&machine.connection) {
            seen.wrong(format!(
                "the machine shows {:?} with no long outage",
                machine.connection
            ));
            noted = Some(machine.connection.clone());
        }
        if up && machine.address.is_none() {
            seen.wrong("a connected machine has no address".into());
        }
        if let Some(before) = &before {
            if connected(before) && !up {
                seen.shows.fetch_add(1, Ordering::SeqCst);
            }
            if up && (!connected(before) || before.quality.reconnects != machine.quality.reconnects)
            {
                *seen.made.lock().unwrap() = Instant::now();
            }
            let lists = [
                ("sessions", before.sessions.len(), machine.sessions.len()),
                ("hosts", before.hosts.len(), machine.hosts.len()),
                ("projects", before.projects.len(), machine.projects.len()),
                ("accounts", before.accounts.len(), machine.accounts.len()),
                ("providers", before.providers.len(), machine.providers.len()),
            ];
            for (list, had, has) in lists {
                if has < had {
                    seen.wrong(format!("the {list} went from {had} to {has}"));
                }
            }
            for head in &before.sessions {
                let id = &head.session_id;
                if !machine.sessions.iter().any(|now| now.session_id == *id) {
                    seen.wrong(format!("session {id} left the list"));
                }
            }
            let blanked = connected(before) && before.resources.is_some();
            if blanked && up && machine.resources.is_none() && !excused {
                seen.wrong("the resources went blank on a connected machine".into());
            }
        }
        before = Some(machine);
        // On every change, and often enough to see what is still shown once an excuse is over.
        tokio::select! {
            more = changes.next() => if !more {
                return;
            },
            () = tokio::time::sleep(ms(100)) => {}
        }
    }
}

fn message_text(body: &ItemBody) -> &str {
    match body {
        ItemBody::AssistantMessage { text } => text,
        _ => "",
    }
}

/// Reads a subscription's updates, as an open transcript does, and notes what it must never
/// show: an event out of order, and streaming text that shrank or went before the event that
/// makes it durable.
async fn watch_session(sub: SessionSubscription, seen: Arc<Seen>) {
    let mut streaming: HashMap<ItemId, (TurnId, String)> = HashMap::new();
    let mut added: HashSet<ItemId> = HashSet::new();
    let mut ended: HashSet<TurnId> = HashSet::new();
    let mut next_seq = 1;
    while let Some(update) = sub.next().await {
        for event in &update.events {
            if event.seq != next_seq {
                seen.wrong(format!(
                    "event {} arrived where {next_seq} was due",
                    event.seq
                ));
            }
            next_seq = event.seq + 1;
            match &event.body {
                EventBody::ItemAdded { item } => {
                    added.insert(item.id.clone());
                }
                EventBody::TurnCompleted { turn_id, .. }
                | EventBody::TurnInterrupted { turn_id }
                | EventBody::TurnFailed { turn_id, .. } => {
                    ended.insert(turn_id.clone());
                }
                _ => {}
            }
        }
        seen.events.lock().unwrap().extend(update.events);
        let now: HashMap<ItemId, (TurnId, String)> = update
            .streaming
            .iter()
            .map(|item| {
                let text = message_text(&item.body).to_owned();
                (item.id.clone(), (item.turn_id.clone(), text))
            })
            .collect();
        for (id, (turn, text)) in &streaming {
            match now.get(id) {
                Some((_, longer)) if longer.starts_with(text.as_str()) => {}
                Some((_, other)) => seen.wrong(format!(
                    "the streaming text of {id} went from {} to {} bytes",
                    text.len(),
                    other.len()
                )),
                None if added.contains(id) || ended.contains(turn) => {}
                None => seen.wrong(format!("the streaming text of {id} went before its event")),
            }
        }
        streaming = now;
        seen.ended.send_replace(ended.len() as u64);
    }
}

/// What the user did, for the daemon's journal to be checked against.
#[derive(Debug, Default)]
struct Did {
    prompts: u64,
    created: u64,
    renamed: Vec<(SessionId, String)>,
}

fn prompt(session_id: &SessionId, number: u64) -> CommandBody {
    CommandBody::SendPrompt {
        session_id: session_id.clone(),
        text: format!("Prompt {number}."),
        images: Vec::new(),
        files: Vec::new(),
    }
}

fn create_chat() -> CommandBody {
    CommandBody::CreateSession {
        repo: None,
        project_id: None,
        branch: None,
        account_id: Some(account()),
        provider: None,
        model: None,
        permission_mode: Some(PermissionMode::Ask),
        failover_pin: None,
        chat: true,
    }
}

async fn created(client: &Client, host: &HostId, command: CommandBody) -> SessionId {
    match client.send(host.clone(), command).await.unwrap() {
        CommandResult::SessionCreated { session_id } => session_id,
        other => panic!("expected a session, got {other:?}"),
    }
}

/// One kind of network trouble.
#[derive(Clone, Copy, Debug)]
enum Trouble {
    /// The link changes.
    Link(Link),
    /// Nothing gets through for this many milliseconds, then all of it does.
    Freeze(u64),
    /// Every connection dies; a new one can be made at once.
    Kill(Kill),
    /// This many cuts, 300 ms apart.
    Flap(u64),
    /// Every connection dies and new ones are refused for this many milliseconds.
    Refuse(u64, Kill),
    /// Every connection dies and new ones go unanswered for this many milliseconds.
    Lose(u64, Kill),
    /// A long outage: every connection dies and no new one can be made for `secs`; the app
    /// may hear that the network is back (`wake`).
    Outage {
        secs: u64,
        refuse: bool,
        kill: Kill,
        wake: bool,
    },
    /// Which of the machine's two addresses are in reach changes to `reach`, as for a phone
    /// leaving or joining the network the machine is on; the app may hear of the change.
    Move { reach: [bool; 2], wake: bool },
    /// The app is suspended for `secs`, its socket silently killed meanwhile or not, and
    /// woken.
    Suspend { secs: u64, killed: bool },
}

/// A daemon, the network, the app's client on it and a client that reaches the daemon
/// directly, with one session streaming on both.
struct World {
    tmp: tempfile::TempDir,
    daemon: Daemon,
    /// The network the app reaches the daemon over; in a relayed world, the hub.
    net: Arc<Net>,
    /// In a relayed world, the network the daemon reaches the hub over.
    host_net: Option<Arc<Net>>,
    /// The connections made while the world was set up.
    setup: u64,
    app: Client,
    truth: Client,
    host: HostId,
    session: SessionId,
    /// Sessions that only sit in the list.
    others: Vec<SessionId>,
    /// What the app showed, and what the direct client did.
    seen: Arc<Seen>,
    real: Arc<Seen>,
    /// Which of the machine's addresses are in reach.
    reach: [bool; 2],
    /// How many connections the client may have made by now.
    allowed: u64,
    /// How many troubles may each have shown the machine without a connection once.
    may_show: u64,
    /// Until when a connection may be dead without the client knowing.
    unnoticed: Instant,
    /// How many connections in a row ended sooner than [`STEADY`] after they were made, at
    /// most: what the client counts, as far as the network can tell.
    flaps: u32,
    suspended: Arc<AtomicBool>,
    stop: CancellationToken,
    user: Option<tokio::task::JoinHandle<Did>>,
    /// What a test without a user did itself.
    did: Did,
}

impl World {
    /// Everything set up on a clean network and the clock paused from then on.
    async fn start(seed: u64, turns: u64, big: Option<usize>) -> Self {
        Self::start_on(seed, turns, big, false).await
    }

    /// [`World::start`], with the app reaching the daemon through a hub only: the app's
    /// network leads to the hub, and the daemon reaches the hub over a network of its
    /// own, [`World::host_net`].
    async fn relayed(seed: u64, turns: u64) -> Self {
        Self::start_on(seed, turns, None, true).await
    }

    async fn start_on(seed: u64, turns: u64, big: Option<usize>, relayed: bool) -> Self {
        eprintln!("bad network: seed {seed}");
        let tmp = tempfile::tempdir().unwrap();
        let script = script(tmp.path(), &mut Rng::new(seed), turns, big);
        let open = |name: &str| {
            let dir = tmp.path().join(name).display().to_string();
            Client::open(dir, "herder-test/0".into()).unwrap()
        };
        let (app, truth) = (open("app"), open("truth"));
        let (daemon, net, host_net, host) = if relayed {
            let hub = Hub::start(&tmp.path().join("hub")).await;
            let host_net = Net::start(hub.addr, seed ^ 0x4057).await;
            let code = hub.auth.mint_host("test-host-1", PAIRING_TTL).unwrap().code;
            let link = HubConfig {
                backup: false,
                secrets: false,
                ..HubConfig::new(
                    host_net.addrs[0].to_string(),
                    hub.fingerprint.clone(),
                    Some(code),
                )
            };
            let daemon = Daemon::start(&tmp.path().join("daemon"), script, Some(link)).await;
            let net = Net::start(hub.addr, seed).await;
            // The app signs in to the daemon where it is, then to the hub over its
            // network, and from then on reaches the daemon through the hub only.
            let direct = vec![daemon.addr.to_string()];
            let host =
                authenticator::sign_in(&daemon.auth, &app, direct, &daemon.fingerprint, "alice")
                    .await
                    .unwrap()
                    .host_id;
            let through = net.addrs.iter().map(ToString::to_string).collect();
            let hub_id =
                authenticator::sign_in(&hub.auth, &app, through, &hub.fingerprint, "alice")
                    .await
                    .unwrap()
                    .host_id;
            let route = format!("hub://{hub_id}");
            let routed = |m: &Machine| m.host_id == host && m.addresses.contains(&route);
            while !app.machines().iter().any(routed) {
                tokio::time::sleep(ms(20)).await;
            }
            app.set_addresses(host.clone(), vec![route.clone()])
                .unwrap();
            let relayed = |m: &Machine| {
                m.host_id == host && connected(m) && m.address.as_ref() == Some(&route)
            };
            while !app.machines().iter().any(relayed) {
                tokio::time::sleep(ms(20)).await;
            }
            (daemon, net, Some(host_net), host)
        } else {
            let daemon = Daemon::start(&tmp.path().join("daemon"), script, None).await;
            let net = Net::start(daemon.addr, seed).await;
            let through = net.addrs.iter().map(ToString::to_string).collect();
            let host =
                authenticator::sign_in(&daemon.auth, &app, through, &daemon.fingerprint, "alice")
                    .await
                    .unwrap()
                    .host_id;
            (daemon, net, None, host)
        };
        let direct = vec![daemon.addr.to_string()];
        authenticator::sign_in(&daemon.auth, &truth, direct, &daemon.fingerprint, "alice")
            .await
            .unwrap();
        daemon.fanout.host_resources(host_resources());
        // What a hub and discovery would list. The providers stay empty: a daemon lists
        // those it probed, and this one probes none.
        daemon.fanout.hosts_changed(vec![FleetHost {
            host_id: host.clone(),
            host_name: "test-host-1".into(),
            online: true,
            last_seen: "2026-01-01T00:00:00Z".parse().unwrap(),
            usage: None,
            relay: false,
        }]);
        daemon.fanout.projects_changed(vec![Project {
            project_id: ProjectId::new("github.com/org/repo"),
            name: "repo".into(),
            paths: vec!["/src/repo".into()],
            remote: None,
            default_permission_mode: None,
            default_account: None,
            setup_command: None,
            icon: None,
            icon_uploaded: false,
            icon_background: None,
            private_artifacts: false,
        }]);

        // Git runs here, before the clock is paused: a process takes real time.
        let on_repo = CommandBody::CreateSession {
            repo: Some(repo(&tmp.path().join("repo"))),
            project_id: None,
            branch: None,
            account_id: Some(account()),
            provider: None,
            model: None,
            permission_mode: Some(PermissionMode::Ask),
            failover_pin: None,
            chat: false,
        };
        let mut others = vec![created(&app, &host, on_repo).await];
        others.push(created(&app, &host, create_chat()).await);
        let session = created(&app, &host, create_chat()).await;
        app.synced(host.clone()).await.unwrap();
        truth.synced(host.clone()).await.unwrap();
        let machine = app.machines().remove(0);
        assert!(
            connected(&machine) && machine.resources.is_some(),
            "{machine:?}"
        );
        assert_eq!(machine.sessions.len(), 3);
        let lists = [
            machine.hosts.len(),
            machine.projects.len(),
            machine.accounts.len(),
        ];
        assert_eq!(lists, [1, 1, 1], "{machine:?}");

        tokio::time::pause();
        // The clock moves on to the next timer whenever no task has work, bytes on their way
        // through a socket or not: with a timer every millisecond, a hop costs at most that.
        // Loopback delivers within the write, unless the kernel puts it off on a busy
        // machine: giving up the CPU at each tick lets it catch up before time moves on.
        tokio::spawn(async {
            loop {
                tokio::time::sleep(ms(1)).await;
                std::thread::yield_now();
            }
        });
        let (seen, real) = (Seen::new(), Seen::new());
        tokio::spawn(watch_machine(app.clone(), Arc::clone(&seen)));
        for (client, seen) in [(&app, &seen), (&truth, &real)] {
            let sub = client
                .subscribe_session(host.clone(), session.clone())
                .unwrap();
            tokio::spawn(watch_session(sub, Arc::clone(seen)));
        }
        let allowed = net.accepted();
        Self {
            tmp,
            daemon,
            net,
            host_net,
            setup: allowed,
            app,
            truth,
            host,
            session,
            others,
            seen,
            real,
            reach: [true, true],
            allowed,
            may_show: 0,
            unnoticed: Instant::now(),
            flaps: 0,
            suspended: Arc::default(),
            stop: CancellationToken::new(),
            user: None,
            did: Did::default(),
        }
    }

    fn at(&self) -> f32 {
        self.seen.start.elapsed().as_secs_f32()
    }

    fn machine(&self) -> Machine {
        self.app.machines().remove(0)
    }

    /// Starts the user: one who sends `turns` prompts, mostly once the turn before ended and
    /// sometimes while it runs, and now and then renames a session or starts a chat, waiting
    /// while the app is suspended.
    fn spawn_user(&mut self, seed: u64, turns: u64) {
        let (app, host, session) = (self.app.clone(), self.host.clone(), self.session.clone());
        let (seen, others) = (Arc::clone(&self.seen), self.others.clone());
        let (suspended, stop) = (Arc::clone(&self.suspended), self.stop.clone());
        let mut rng = Rng::new(seed ^ 0x5EED);
        self.user = Some(tokio::spawn(async move {
            let mut did = Did::default();
            let mut ended = seen.ended.subscribe();
            for number in 1..=turns {
                tokio::time::sleep(ms(rng.range(300, 4000))).await;
                while suspended.load(Ordering::SeqCst) && !stop.is_cancelled() {
                    tokio::time::sleep(ms(200)).await;
                }
                if stop.is_cancelled() {
                    break;
                }
                if rng.chance(15) {
                    let renamed = rng.pick(&[0, 1]);
                    let title = format!("Title {number}");
                    let rename = CommandBody::RenameSession {
                        session_id: others[renamed].clone(),
                        title: title.clone(),
                    };
                    let answer = app.send(host.clone(), rename).await;
                    assert_eq!(answer.unwrap(), CommandResult::Applied);
                    did.renamed.push((others[renamed].clone(), title));
                }
                if rng.chance(8) && did.created < 4 {
                    created(&app, &host, create_chat()).await;
                    did.created += 1;
                }
                let answer = app.send(host.clone(), prompt(&session, number)).await;
                assert_eq!(answer.unwrap(), CommandResult::Applied);
                did.prompts = number;
                if rng.chance(80) {
                    tokio::select! {
                        _ = ended.wait_for(|ended| *ended >= number) => {}
                        () = stop.cancelled() => {}
                    }
                }
            }
            did
        }));
    }

    fn excuse(&mut self, from: Instant, until: Instant) {
        self.seen.excused.lock().unwrap().push((from, until));
        self.may_show += 1;
    }

    /// Notes that the client loses its connection now, if it has one. From the third in a row
    /// that ended young, the client waits 2 s and more before the next, twice as long each
    /// time: a network that drops every connection it lets through is asked for fewer, and
    /// the drop may show.
    fn lost(&mut self) {
        let now = Instant::now();
        let made = *self.seen.made.lock().unwrap();
        self.flaps = if now - made < STEADY + ms(300) {
            self.flaps + 1
        } else {
            0
        };
        if self.flaps >= 3 {
            let wait = secs(1 << (self.flaps - 1).min(5)).min(secs(30));
            self.excuse(now, now + wait + secs(12));
        }
    }

    /// Kills every connection, each of which the client may replace with a race of its two
    /// addresses.
    fn kill(&mut self, kill: Kill) {
        self.lost();
        self.net.kill(None, kill);
        self.allowed += if self.reach == [true, true] { 2 } else { 6 };
        if kill == Kill::Silent {
            // Its pings find it within 15 s.
            self.unnoticed = Instant::now() + secs(16);
        }
    }

    /// Kills every connection of the daemon to the hub, in a relayed world: the one it
    /// replicates over and the streams it carries for the app, which the app replaces with a
    /// race of the hub's two addresses once the daemon is back at the hub.
    fn kill_host_leg(&mut self, kill: Kill) {
        eprintln!("{:9.3} s: on the daemon's leg, Kill({kill:?})", self.at());
        self.lost();
        let host_net = self.host_net.as_ref().expect("a relayed world");
        host_net.kill(None, kill);
        self.allowed += 2;
    }

    /// What every address in reach does with new connections from now on.
    fn route(&self, route: Route) {
        for (path, reached) in self.reach.into_iter().enumerate() {
            if reached {
                self.net.routes[path].send_replace(route);
            }
        }
    }

    /// Makes `trouble` and waits it out. Notes how many connections the client may make over
    /// it, and when the machine may show that it has none.
    async fn trouble(&mut self, trouble: Trouble) {
        eprintln!("{:9.3} s: {trouble:?}", self.at());
        let now = Instant::now();
        // A drop that the client cannot mend within its five seconds may show: one on a link
        // too slow to connect over in time, which a silent one is found on up to 16 s later,
        // or one found late, in the middle of another.
        let slow = self.net.link.borrow().slow();
        let late = now < self.unnoticed;
        let drops = !matches!(
            trouble,
            Trouble::Link(_)
                | Trouble::Freeze(_)
                | Trouble::Outage { .. }
                | Trouble::Suspend { .. }
        );
        if slow && drops {
            self.excuse(now, now + secs(40));
        }
        match trouble {
            Trouble::Link(link) => {
                if link.slow() && now < self.unnoticed {
                    self.excuse(now, self.unnoticed + secs(10));
                }
                self.net.link.send_replace(link);
            }
            Trouble::Freeze(millis) => {
                if late {
                    self.excuse(now, now + ms(millis) + secs(20));
                }
                self.net.frozen.send_replace(true);
                tokio::time::sleep(ms(millis)).await;
                self.net.frozen.send_replace(false);
            }
            Trouble::Kill(kill) => {
                // It may hit the connection being made in place of one that died unnoticed,
                // which then goes unanswered for the ten seconds an attempt is given.
                if kill == Kill::Silent && late && !slow {
                    self.excuse(now, now + secs(16));
                }
                self.kill(kill);
            }
            Trouble::Flap(times) => {
                for _ in 0..times {
                    self.kill(Kill::Fin);
                    tokio::time::sleep(ms(300)).await;
                }
            }
            Trouble::Refuse(millis, kill) | Trouble::Lose(millis, kill) => {
                if late && !slow {
                    self.excuse(now, now + ms(millis) + secs(20));
                }
                let refuse = matches!(trouble, Trouble::Refuse(..));
                self.route(if refuse { Route::Refused } else { Route::Lost });
                self.kill(kill);
                // Each attempt of the backoff in that time dials both addresses.
                self.allowed += 12;
                tokio::time::sleep(ms(millis)).await;
                self.route(Route::Up);
            }
            Trouble::Outage {
                secs: long,
                refuse,
                kill,
                wake,
            } => {
                let back = if wake { WAKE_BOUND } else { RETRY_BOUND };
                self.excuse(now, now + secs(long) + back);
                self.route(if refuse { Route::Refused } else { Route::Lost });
                self.kill(kill);
                // The backoff's attempts: seven in the first 16 s, then one every 15 s or
                // more; and what a wake starts.
                self.allowed += 2 * (8 + long / 15) + 4;
                tokio::time::sleep(secs(long)).await;
                self.route(Route::Up);
                if wake {
                    self.app.wake();
                }
            }
            Trouble::Move { reach, wake } => {
                for (path, (now_in, was_in)) in reach.into_iter().zip(self.reach).enumerate() {
                    if now_in && !was_in {
                        self.net.routes[path].send_replace(Route::Up);
                    } else if !now_in && was_in {
                        self.net.routes[path].send_replace(Route::Lost);
                        self.lost();
                        self.net.kill(Some(path), Kill::Silent);
                        self.unnoticed = now + secs(16);
                    }
                }
                self.reach = reach;
                // A race to replace a dead connection, in which an address that failed is
                // tried again while the other goes unanswered.
                self.allowed += 6;
                if wake {
                    self.app.wake();
                    self.allowed += 2;
                }
            }
            Trouble::Suspend { secs: long, killed } => {
                self.excuse(now, now + secs(long) + WAKE_BOUND + QUIET);
                self.suspended.store(true, Ordering::SeqCst);
                self.app.suspend();
                if killed {
                    self.kill(Kill::Silent);
                }
                tokio::time::sleep(secs(long)).await;
                self.suspended.store(false, Ordering::SeqCst);
                self.app.wake();
                self.allowed += 4;
            }
        }
    }

    /// Whether the app shows what the direct client shows.
    fn converged(&self) -> bool {
        let (app, truth) = (self.machine(), self.truth.machines().remove(0));
        connected(&app)
            && app.sessions == truth.sessions
            && app.hosts == truth.hosts
            && app.projects == truth.projects
            && app.accounts == truth.accounts
            && app.providers == truth.providers
            && app.resources == truth.resources
            && *self.seen.events.lock().unwrap() == *self.real.events.lock().unwrap()
    }

    /// Waits until every excuse is over, then for the app to show what the daemon has, which
    /// it must within [`CONVERGE_BOUND`].
    async fn settle(&self) {
        let over = self.seen.excused.lock().unwrap().iter().map(|e| e.1).max();
        if let Some(over) = over {
            tokio::time::sleep_until(over).await;
        }
        let converging = async {
            self.app.synced(self.host.clone()).await.unwrap();
            while !self.converged() {
                tokio::time::sleep(ms(50)).await;
            }
        };
        if tokio::time::timeout(CONVERGE_BOUND, converging)
            .await
            .is_err()
        {
            let (app, truth) = (self.machine(), self.truth.machines().remove(0));
            panic!(
                "at {:.3} s the app did not show the daemon's state within {CONVERGE_BOUND:?}:\n\
                 app {app:#?}\ndaemon {truth:#?}\n{} events against {}\nwrong: {:#?}",
                self.at(),
                self.seen.events().len(),
                self.real.events().len(),
                self.seen.wrong.lock().unwrap(),
            );
        }
    }

    /// Heals the network, lets the user and the turns finish, and checks the whole run:
    /// nothing wrong was shown, the app shows the daemon's state, every command was applied
    /// once, and the client made no more connections than its troubles account for.
    async fn finish(mut self) {
        eprintln!("{:9.3} s: the network heals", self.at());
        self.net.frozen.send_replace(false);
        self.net.link.send_replace(Link::CLEAN);
        self.reach = [true, true];
        self.route(Route::Up);
        self.app.wake();
        self.stop.cancel();
        let did = match self.user.take() {
            Some(user) => tokio::time::timeout(secs(60), user)
                .await
                .expect("the user's last command was not answered once the network healed")
                .unwrap(),
            None => std::mem::take(&mut self.did),
        };
        let mut ended = self.real.ended.subscribe();
        tokio::time::timeout(secs(120), ended.wait_for(|ended| *ended >= did.prompts))
            .await
            .expect("the turns did not end")
            .unwrap();
        self.settle().await;
        tokio::time::sleep(secs(2)).await;

        let wrong = self.seen.wrong.lock().unwrap().clone();
        assert!(wrong.is_empty(), "the app showed: {wrong:#?}");
        let direct = self.real.wrong.lock().unwrap().clone();
        assert!(direct.is_empty(), "the direct client showed: {direct:#?}");
        assert!(
            self.seen.shows() <= self.may_show,
            "the machine lost its connection {} times over {} long troubles",
            self.seen.shows(),
            self.may_show
        );
        // Through a hub the app keeps two connections, to the hub and through it to the
        // daemon, and each is replaced with a race of the hub's two addresses.
        let allowed = match self.host_net {
            Some(_) => self.setup + (self.allowed - self.setup) * 2,
            None => self.allowed,
        };
        assert!(
            self.net.accepted() <= allowed,
            "a reconnect storm: {} connections where the troubles account for {allowed}",
            self.net.accepted(),
        );

        // The journal is what happened: the app shows all of it, and each command once.
        self.daemon.shutdown.cancel();
        tokio::time::timeout(secs(30), &mut self.daemon.serving)
            .await
            .expect("the daemon did not stop")
            .unwrap();
        let store = Store::open(self.tmp.path().join("daemon/herder.db")).unwrap();
        let journal = store.read_since(&self.session, 0, usize::MAX).unwrap();
        let events = self.seen.events();
        assert_eq!(events[..], journal[..events.len()]);
        let count = |wanted: &dyn Fn(&EventBody) -> bool| {
            journal.iter().filter(|event| wanted(&event.body)).count() as u64
        };
        let started = count(&|body| matches!(body, EventBody::TurnStarted { .. }));
        let completed = count(&|body| matches!(body, EventBody::TurnCompleted { .. }));
        assert_eq!((started, completed), (did.prompts, did.prompts), "{did:?}");
        for number in 1..=did.prompts {
            let text = format!("Prompt {number}.");
            let sent = count(&|body| {
                matches!(body, EventBody::ItemAdded { item }
                    if item.body == ItemBody::UserMessage { text: text.clone(), attachments: Vec::new() })
            });
            assert_eq!(sent, 1, "{text} is in the journal {sent} times");
        }
        let machine = self.machine();
        assert_eq!(machine.sessions.len() as u64, 3 + did.created, "{did:?}");
        for other in &self.others {
            let titles: Vec<&String> = did
                .renamed
                .iter()
                .filter(|(session, _)| session == other)
                .map(|(_, title)| title)
                .collect();
            let renames = store
                .read_since(other, 0, usize::MAX)
                .unwrap()
                .into_iter()
                .filter_map(|event| match event.body {
                    EventBody::TitleChanged { title, .. } => Some(title),
                    _ => None,
                })
                .filter(|title| title.starts_with("Title "))
                .collect::<Vec<_>>();
            assert_eq!(renames.iter().collect::<Vec<_>>(), titles);
        }
        eprintln!(
            "{:9.3} s: done; {} prompts, {} connections of {allowed} allowed, shown without a connection {} times",
            self.at(),
            did.prompts,
            self.net.accepted(),
            self.seen.shows()
        );
    }

    /// Waits for the machine to show what `wanted` asks, which it must `within`.
    async fn shows(&self, within: Duration, what: &str, wanted: impl Fn(&Machine) -> bool) {
        let waiting = async {
            while !wanted(&self.machine()) {
                tokio::time::sleep(ms(20)).await;
            }
        };
        if tokio::time::timeout(within, waiting).await.is_err() {
            panic!(
                "at {:.3} s the machine did not show {what} within {within:?}: {:#?}",
                self.at(),
                self.machine()
            );
        }
    }

    /// Panics if the app showed anything it must not, so far.
    fn nothing_wrong(&self) {
        let wrong = self.seen.wrong.lock().unwrap().clone();
        assert!(
            wrong.is_empty(),
            "at {:.3} s the app showed: {wrong:#?}",
            self.at()
        );
    }
}

/// Runs `test` on a runtime of its own, whose clock [`World::start`] pauses.
fn run(test: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(test);
}

/// A link a phone has on a good day: what a drop is mended over within the client's five
/// seconds.
const MOBILE: Link = Link {
    latency_ms: 60,
    jitter_ms: 20,
    rate: Some(200_000),
    ..Link::CLEAN
};

/// Every kind of short drop, one after another in the middle of streaming turns: the app
/// shows none of them, and each costs one new connection.
#[test]
fn short_drops_of_every_kind_are_never_seen() {
    run(async {
        let mut world = World::start(11, 120, None).await;
        world.spawn_user(11, 120);
        world.trouble(Trouble::Link(MOBILE)).await;
        let drops = [
            Trouble::Kill(Kill::Fin),
            Trouble::Kill(Kill::Torn),
            Trouble::Kill(Kill::Reset),
            Trouble::Refuse(1500, Kill::Fin),
            Trouble::Lose(2500, Kill::Reset),
            Trouble::Refuse(1000, Kill::Torn),
            // Found by a ping within 15 s, or sooner by what the user sends.
            Trouble::Kill(Kill::Silent),
            Trouble::Lose(2000, Kill::Fin),
        ];
        for (round, drop) in drops.into_iter().enumerate() {
            // Every other drop hits a connection that is seconds old: the second in a row
            // is still mended in time.
            tokio::time::sleep(secs(if round % 2 == 0 { 32 } else { 4 })).await;
            world.trouble(drop).await;
            let round = round as u32 + 1;
            world
                .shows(secs(20), "a new connection", |machine| {
                    machine.quality.reconnects == round
                })
                .await;
            // The silent death was found.
            world.unnoticed = Instant::now();
            world.nothing_wrong();
        }
        let before = world.net.accepted();
        // Nothing arrives for a while and then all of it does: the connection is kept.
        for freeze in [500, 2500, 1500] {
            tokio::time::sleep(secs(4)).await;
            world.trouble(Trouble::Freeze(freeze)).await;
        }
        tokio::time::sleep(secs(8)).await;
        assert_eq!(world.net.accepted(), before);
        // A burst of cuts, some into connections still being made.
        tokio::time::sleep(secs(32)).await;
        world.trouble(Trouble::Flap(4)).await;
        tokio::time::sleep(secs(8)).await;
        assert!(connected(&world.machine()));
        assert_eq!((world.seen.shows(), world.may_show), (0, 0));
        world.finish().await;
    });
}

/// The network is gone for longer than a drop that is ridden out: the machine shows it once,
/// not before its five seconds are over, keeps its lists, and is back within a bound, sooner
/// when the app hears that the network is. What the user sent meanwhile is applied once.
#[test]
fn a_long_outage_shows_once_and_recovers() {
    run(async {
        let mut world = World::start(12, 60, None).await;
        world.spawn_user(12, 60);
        tokio::time::sleep(secs(6)).await;

        let app = world.app.clone();
        let during = async move {
            tokio::time::sleep(secs(3)).await;
            assert!(connected(&app.machines()[0]), "shown too soon");
            tokio::time::sleep(secs(4)).await;
            let machine = app.machines().remove(0);
            assert!(!connected(&machine), "{machine:?}");
            assert_eq!(machine.resources, None);
            assert_eq!(machine.sessions.len(), 3);
        };
        let outage = Trouble::Outage {
            secs: 40,
            refuse: true,
            kill: Kill::Fin,
            wake: false,
        };
        tokio::join!(world.trouble(outage), during);
        world.shows(RETRY_BOUND, "a connection", connected).await;
        assert_eq!(world.seen.shows(), 1);
        world.settle().await;

        // New connections go unanswered this time, and the app is told when it is over.
        let outage = Trouble::Outage {
            secs: 25,
            refuse: false,
            kill: Kill::Reset,
            wake: true,
        };
        world.trouble(outage).await;
        world.shows(WAKE_BOUND, "a connection", connected).await;
        assert_eq!(world.seen.shows(), 2);
        world.settle().await;
        world.finish().await;
    });
}

/// The app sleeps through a turn that adds two megabytes to the session and wakes on a link
/// of 25 kB a second: the replay takes over a minute. The one connection that carries it is
/// kept all the way, through the pings it delays, a command and a wake's probe; it is never
/// taken for dead and replaced.
#[test]
fn a_slow_link_carrying_a_big_replay_is_kept() {
    run(async {
        let mut world = World::start(13, 4, Some(2_000_000)).await;
        // Until the probe that subscribing sent is answered: a connection that fails a probe
        // is replaced at once, suspended or not.
        tokio::time::sleep(secs(2)).await;
        // Suspended for some 31 s, and woken on a slow link.
        world.excuse(Instant::now(), Instant::now() + secs(35) + WAKE_BOUND);
        world.app.suspend();
        world.net.kill(None, Kill::Silent);
        let sent = world
            .truth
            .send(world.host.clone(), prompt(&world.session, 1))
            .await;
        assert_eq!(sent.unwrap(), CommandResult::Applied);
        world.did.prompts = 1;
        let mut ended = world.real.ended.subscribe();
        tokio::time::timeout(secs(30), ended.wait_for(|ended| *ended == 1))
            .await
            .expect("the turn did not end")
            .unwrap();
        tokio::time::sleep(secs(30)).await;

        let slow = Link {
            latency_ms: 150,
            jitter_ms: 40,
            rate: Some(25_000),
            ..Link::CLEAN
        };
        world.trouble(Trouble::Link(slow)).await;
        let woken = Instant::now();
        let before = world.net.carried.load(Ordering::SeqCst);
        world.app.wake();
        world.shows(WAKE_BOUND, "a connection", connected).await;
        // One race of the two addresses, and nothing after it.
        let raced = world.net.accepted();
        world.allowed += 2;
        assert!(raced <= world.allowed, "{raced}");

        tokio::time::sleep(secs(10)).await;
        let renamed = (world.others[1].clone(), "Title on a slow link".to_owned());
        let rename = CommandBody::RenameSession {
            session_id: renamed.0.clone(),
            title: renamed.1.clone(),
        };
        let answer = world.app.send(world.host.clone(), rename).await;
        assert_eq!(answer.unwrap(), CommandResult::Applied);
        world.did.renamed.push(renamed);
        tokio::time::sleep(secs(10)).await;
        world.app.wake();

        let mut ended = world.seen.ended.subscribe();
        tokio::time::timeout(secs(300), ended.wait_for(|ended| *ended == 1))
            .await
            .expect("the replay did not arrive")
            .unwrap();
        let took = woken.elapsed();
        let carried = world.net.carried.load(Ordering::SeqCst) - before;
        assert!(
            took > secs(60) && carried > 2_000_000,
            "the replay was no load: {carried} bytes in {took:?}"
        );
        assert_eq!(world.net.accepted(), raced, "the connection was replaced");
        assert_eq!(world.seen.shows(), 1);
        world.settle().await;
        world.finish().await;
    });
}

/// A phone leaves the network its machine is on and comes back, twice: the connection moves
/// to the address in reach, told of the change or not, and the app shows none of it.
#[test]
fn moving_between_the_machines_addresses_is_never_seen() {
    run(async {
        let mut world = World::start(14, 80, None).await;
        world.spawn_user(14, 80);
        world.trouble(Trouble::Link(MOBILE)).await;
        let [first, second] = world.net.addrs.map(|addr| addr.to_string());
        let on = |address: &str| {
            let address = address.to_owned();
            move |machine: &Machine| machine.address.as_deref() == Some(address.as_str())
        };
        // Each move below finds a connection that has lasted: one that ends young is
        // replaced later and later (see `World::lost`).
        let lasting = secs(32);
        tokio::time::sleep(lasting).await;
        assert!(on(&first)(&world.machine()));

        // Away, and told: the dead connection is found by the wake's probe.
        let (home, away, vpn_down) = ([true, true], [false, true], [true, false]);
        let moved = |reach, wake| Trouble::Move { reach, wake };
        world.trouble(moved(away, true)).await;
        // Both attempts of the race that follows are reset, the unanswered one too: the
        // race ends there, and the next is not held up by the first address either.
        tokio::time::sleep(ms(5400)).await;
        world.trouble(Trouble::Kill(Kill::Reset)).await;
        world
            .shows(secs(8), "the second address", on(&second))
            .await;
        // Short drops there, with the first address unanswered: a connection cut while it
        // is made and an address that refuses for a moment are tried again at once, not
        // after the ten seconds the first address is given.
        tokio::time::sleep(lasting).await;
        world.trouble(Trouble::Flap(2)).await;
        tokio::time::sleep(secs(5)).await;
        world.trouble(Trouble::Refuse(1500, Kill::Reset)).await;
        tokio::time::sleep(lasting).await;
        // Home, and told: the connection that works is kept.
        world.trouble(moved(home, true)).await;
        tokio::time::sleep(lasting).await;
        assert!(on(&second)(&world.machine()));
        // The second address goes, and the app is not told: its pings find the dead
        // connection.
        world.trouble(moved(vpn_down, false)).await;
        world.shows(secs(22), "the first address", on(&first)).await;
        tokio::time::sleep(secs(5)).await;
        world.trouble(moved(home, false)).await;
        tokio::time::sleep(lasting).await;
        assert!(on(&first)(&world.machine()));
        world.trouble(moved(away, false)).await;
        world
            .shows(secs(22), "the second address", on(&second))
            .await;

        assert_eq!((world.seen.shows(), world.may_show), (0, 0));
        world.settle().await;
        world.finish().await;
    });
}

/// How many round trips of the app's link a connect costs, directly or through a hub whose
/// host is on a clean link of its own: the time `reconnect` takes on the paused clock over a
/// link with no jitter, in units of its round trip. The relay listens on loopback, so TCP's
/// own handshake costs nothing here; on a real network it adds one to both.
fn connect_round_trips(relayed: bool) -> f64 {
    let link = Link {
        latency_ms: 100,
        ..Link::CLEAN
    };
    let mut round_trips = 0.0;
    run(async {
        let mut world = World::start_on(16, 4, None, relayed).await;
        world.trouble(Trouble::Link(link)).await;
        tokio::time::sleep(secs(2)).await;
        let mut measured = Vec::new();
        for _ in 0..3 {
            let started = Instant::now();
            world.app.reconnect(world.host.clone()).await.unwrap();
            measured
                .push(started.elapsed().as_secs_f64() / (2.0 * ms(link.latency_ms).as_secs_f64()));
            tokio::time::sleep(secs(2)).await;
        }
        measured.sort_by(f64::total_cmp);
        round_trips = measured[1];
        world.trouble(Trouble::Link(Link::CLEAN)).await;
    });
    round_trips
}

/// A connect through the hub costs one round trip of the app's link more than a direct one:
/// the upgrade to the hub and the TLS to the machine share one.
#[test]
fn a_relayed_connect_costs_one_round_trip_more_than_a_direct_one() {
    let direct = connect_round_trips(false);
    let relayed = connect_round_trips(true);
    eprintln!("connect round trips: direct {direct:.2}, through the hub {relayed:.2}");
    assert!(
        relayed - direct < 1.4,
        "a relayed connect costs {relayed:.2} round trips, a direct one {direct:.2}"
    );
}

/// The app is suspended and woken over and over, for seconds and for minutes, its socket
/// silently killed meanwhile or not, while turns go on: each time it is back within a bound
/// and shows what the daemon has, with no event twice or missing.
#[test]
fn suspend_and_wake_cycles_resume_where_the_daemon_is() {
    run(async {
        let mut world = World::start(15, 60, None).await;
        world.spawn_user(15, 60);
        for (long, killed) in [(3, false), (30, true), (600, true), (90, false), (20, true)] {
            tokio::time::sleep(secs(6)).await;
            world.trouble(Trouble::Suspend { secs: long, killed }).await;
            world.shows(WAKE_BOUND, "a connection", connected).await;
            world.settle().await;
            world.nothing_wrong();
        }
        world.finish().await;
    });
}

/// EDGE: a 120 kbit/s link with a round trip of 600 to 900 ms, whose radio takes a second to
/// come up after it idled.
const EDGE: Link = Link {
    latency_ms: 300,
    jitter_ms: 150,
    rate: Some(15_000),
    loss: 0,
    wake_ms: 1000,
};

/// 3G: 500 kbit/s with a round trip of 250 to 400 ms, whose radio takes two seconds to come
/// up after it idled, as one that dropped to a low-power state does.
const THREE_G: Link = Link {
    latency_ms: 125,
    jitter_ms: 75,
    rate: Some(60_000),
    loss: 0,
    wake_ms: 2000,
};

/// A link with a round trip of 800 ms and up to 400 ms more, as across the world or over a
/// satellite.
const FAR: Link = Link {
    latency_ms: 400,
    jitter_ms: 200,
    rate: None,
    loss: 0,
    wake_ms: 0,
};

/// A 3G link losing 2% of what it carries, each loss held up for a retransmission.
const LOSSY: Link = Link { loss: 2, ..THREE_G };

/// A phone's link while another app fills the buffers on the way: what herder sends waits
/// behind seconds of someone else's bytes, a round trip of 2.4 to 3.6 s.
const BLOATED: Link = Link {
    latency_ms: 1200,
    jitter_ms: 600,
    rate: Some(50_000),
    loss: 0,
    wake_ms: 0,
};

/// A user works over a slow network for a few turns, then leaves the app open on it with
/// nothing going on but its pings; the network changes to the next of `links` every 15 s. No
/// connection is taken for dead while it carries what it is sent, however late, and the app
/// never shows the machine without one. A drop on it is then mended, a connection made over
/// it too.
async fn slow_network(seed: u64, links: &[Link]) {
    let mut world = World::start(seed, 6, None).await;
    world.spawn_user(seed, 6);
    world.trouble(Trouble::Link(links[0])).await;
    let kept = world.net.accepted();
    for step in 1..=10 {
        tokio::time::sleep(secs(15)).await;
        let link = links[step % links.len()];
        if link != *world.net.link.borrow() {
            world.trouble(Trouble::Link(link)).await;
        }
    }
    assert_eq!(
        world.net.accepted(),
        kept,
        "at {:.3} s a connection that carried what it was sent was replaced",
        world.at()
    );
    world.nothing_wrong();
    assert_eq!(world.seen.shows(), 0);

    world.trouble(Trouble::Kill(Kill::Fin)).await;
    world
        .shows(secs(20), "a new connection", |machine| {
            machine.quality.reconnects == 1
        })
        .await;
    tokio::time::sleep(secs(30)).await;
    world.finish().await;
}

#[test]
fn an_edge_link_is_kept() {
    run(slow_network(31, &[EDGE]));
}

#[test]
fn a_3g_link_is_kept() {
    run(slow_network(32, &[THREE_G]));
}

#[test]
fn a_link_with_an_800_ms_round_trip_is_kept() {
    run(slow_network(33, &[FAR]));
}

#[test]
fn a_link_that_loses_2_percent_is_kept() {
    run(slow_network(34, &[LOSSY]));
}

#[test]
fn a_bloated_link_is_kept() {
    run(slow_network(35, &[MOBILE, BLOATED]));
}

/// The phone's buffers fill with another app's uploads until a round trip takes 6 s, longer
/// than a connection on a good link is given to answer: the connection, whose pongs measured
/// the round trips growing, is kept. Then it dies there with no FIN, and is still found dead
/// within a ping's interval and the most silence any connection is given.
#[test]
fn a_link_bloated_until_a_round_trip_takes_6_s_is_kept_and_found_dead_in_time() {
    run(async {
        let mut world = World::start(36, 4, None).await;
        world.spawn_user(36, 4);
        world.trouble(Trouble::Link(MOBILE)).await;
        tokio::time::sleep(secs(30)).await;
        let kept = world.net.accepted();
        for latency_ms in [600, 1200, 1800, 2400, 3000] {
            let link = Link {
                latency_ms,
                jitter_ms: 200,
                ..BLOATED
            };
            world.trouble(Trouble::Link(link)).await;
            tokio::time::sleep(secs(30)).await;
        }
        assert_eq!(
            world.net.accepted(),
            kept,
            "at {:.3} s a connection that carried what it was sent was replaced",
            world.at()
        );
        world.nothing_wrong();
        let slowest = world.machine().quality.max_rtt_ms;
        assert!(slowest > Some(5000), "{slowest:?}");

        world.trouble(Trouble::Kill(Kill::Silent)).await;
        let died = Instant::now();
        let found = async {
            while world.net.accepted() == kept {
                tokio::time::sleep(ms(100)).await;
            }
        };
        // The ping interval, 10 s, and the most silence a connection is given, 20 s.
        tokio::time::timeout(secs(31), found)
            .await
            .expect("the dead connection was not found in time");
        eprintln!(
            "{:9.3} s: found dead after {:?}",
            world.at(),
            died.elapsed()
        );
        // The other app's uploads are done; a new connection is made at once.
        world.trouble(Trouble::Link(MOBILE)).await;
        world.allowed += 4;
        world.shows(secs(10), "a connection", connected).await;
        world.finish().await;
    });
}

fn random_link(rng: &mut Rng) -> Link {
    match rng.range(1, 10) {
        1..=4 => Link {
            latency_ms: rng.range(2, 40),
            jitter_ms: rng.range(0, 10),
            rate: None,
            ..Link::CLEAN
        },
        5..=7 => Link {
            latency_ms: rng.range(40, 150),
            jitter_ms: rng.range(0, 30),
            rate: Some(rng.range(20_000, 300_000)),
            ..Link::CLEAN
        },
        _ => Link {
            latency_ms: rng.range(180, 350),
            jitter_ms: rng.range(0, 120),
            rate: Some(rng.range(5_000, 20_000)),
            ..Link::CLEAN
        },
    }
}

fn random_trouble(rng: &mut Rng, reach: [bool; 2]) -> Trouble {
    let kill = rng.pick(&[Kill::Fin, Kill::Torn, Kill::Reset, Kill::Silent]);
    match rng.range(1, 100) {
        1..=15 => Trouble::Link(random_link(rng)),
        16..=27 => Trouble::Freeze(rng.range(200, 2500)),
        28..=47 => Trouble::Kill(kill),
        48..=53 => Trouble::Flap(rng.range(2, 4)),
        54..=61 => Trouble::Refuse(rng.range(100, 1500), kill),
        62..=69 => Trouble::Lose(rng.range(200, 2000), kill),
        70..=77 => Trouble::Outage {
            secs: rng.range(8, 45),
            refuse: rng.chance(50),
            kill,
            wake: rng.chance(50),
        },
        78..=89 => {
            let places = [[true, true], [false, true], [true, false]];
            let elsewhere: Vec<[bool; 2]> =
                places.into_iter().filter(|place| *place != reach).collect();
            Trouble::Move {
                reach: rng.pick(&elsewhere),
                wake: rng.chance(60),
            }
        }
        _ => Trouble::Suspend {
            secs: rng.pick(&[3, 20, 90, 400]),
            killed: rng.chance(60),
        },
    }
}

/// `seconds` of network time in which trouble after trouble, drawn from `seed`, hits a client
/// whose user keeps working. Every few troubles the network is left alone until the app must
/// show what the daemon has.
async fn soak(seed: u64, seconds: u64) {
    let turns = seconds / 3 + 10;
    let mut world = World::start(seed, turns, None).await;
    world.spawn_user(seed, turns);
    let mut rng = Rng::new(seed ^ 0xBAD);
    while world.seen.start.elapsed() < secs(seconds) {
        // Long enough for a drop to be mended before the next, which takes a slow link
        // longer: two in a row are an outage. Half the time long enough for the connection
        // to count as one that works, so that the next drop is one of a kind again.
        let calm = match (rng.chance(50), world.net.link.borrow().slow()) {
            (true, _) => 32_000,
            (false, true) => 8000,
            (false, false) => 3000,
        };
        tokio::time::sleep(ms(rng.range(calm, calm + 6000))).await;
        let trouble = random_trouble(&mut rng, world.reach);
        world.trouble(trouble).await;
        if rng.chance(30) {
            world.settle().await;
            world.nothing_wrong();
        }
    }
    world.finish().await;
}

/// How long each seed of the soak runs in CI, in seconds of network time.
const SOAK_SECONDS: u64 = 600;

macro_rules! soaks {
    ($($name:ident: $seed:expr,)*) => {
        $(
            #[test]
            fn $name() {
                run(soak($seed, SOAK_SECONDS));
            }
        )*
    };
}

soaks! {
    soak_seed_1: 1,
    soak_seed_2: 2,
    soak_seed_3: 3,
    soak_seed_4: 4,
    soak_seed_5: 5,
    soak_seed_6: 6,
}

/// The soak for the seeds in `HERDER_SOAK_SEEDS` (comma separated), each for
/// `HERDER_SOAK_SECONDS` of network time (an hour when unset); nothing when no seed is set.
#[test]
fn soak_from_env() {
    let Ok(seeds) = std::env::var("HERDER_SOAK_SEEDS") else {
        return;
    };
    let seconds = std::env::var("HERDER_SOAK_SECONDS")
        .map_or(3600, |seconds| seconds.parse().expect("seconds"));
    for seed in seeds.split(',') {
        run(soak(seed.trim().parse().expect("a seed"), seconds));
    }
}

/// Through a hub, short drops in the middle of streaming turns: on the app's leg to the
/// hub, on the daemon's leg to it, and on both at once, as a hub that restarts drops
/// them. The app shows none of them, and nothing is lost or seen twice.
#[test]
fn short_drops_on_either_leg_of_the_hub_route_are_never_seen() {
    run(async {
        let mut world = World::relayed(21, 100).await;
        world.spawn_user(21, 100);
        let mut round = 0;
        let mut mended = async |world: &mut World| {
            round += 1;
            world
                .shows(secs(20), "a new connection", |machine| {
                    machine.quality.reconnects == round
                })
                .await;
            world.unnoticed = Instant::now();
            world.nothing_wrong();
        };
        // The app's leg: the stream dies with its connection to the hub.
        for kill in [Kill::Fin, Kill::Torn, Kill::Reset, Kill::Silent] {
            tokio::time::sleep(secs(32)).await;
            world.trouble(Trouble::Kill(kill)).await;
            mended(&mut world).await;
        }
        // The daemon's leg: its replication connection dies with the stream, and the hub
        // gives the daemon a moment to come back before it turns the app away.
        for kill in [Kill::Fin, Kill::Reset, Kill::Torn] {
            tokio::time::sleep(secs(32)).await;
            world.kill_host_leg(kill);
            mended(&mut world).await;
        }
        // Both at once.
        tokio::time::sleep(secs(32)).await;
        world.kill_host_leg(Kill::Reset);
        world.net.kill(None, Kill::Reset);
        mended(&mut world).await;

        // Nothing arrives on the daemon's leg for a while and then all of it does: every
        // connection is kept.
        let host_net = Arc::clone(world.host_net.as_ref().unwrap());
        // Once the attempts that lost the last race, and the app's own connection to the
        // hub, are through.
        tokio::time::sleep(secs(8)).await;
        let before = (world.net.accepted(), host_net.accepted());
        for freeze in [500, 2500, 1500] {
            tokio::time::sleep(secs(4)).await;
            host_net.frozen.send_replace(true);
            tokio::time::sleep(ms(freeze)).await;
            host_net.frozen.send_replace(false);
        }
        tokio::time::sleep(secs(8)).await;
        assert_eq!((world.net.accepted(), host_net.accepted()), before);
        assert!(connected(&world.machine()));
        assert_eq!((world.seen.shows(), world.may_show), (0, 0));
        world.finish().await;
    });
}

/// Through a hub, the hub's network is gone for the app for longer than a drop that is
/// ridden out: the machine shows it once, keeps its lists, and is back within the bound a
/// direct connection is.
#[test]
fn a_long_outage_of_the_hub_route_shows_once_and_recovers() {
    run(async {
        let mut world = World::relayed(22, 40).await;
        world.spawn_user(22, 40);
        tokio::time::sleep(secs(6)).await;
        let outage = Trouble::Outage {
            secs: 40,
            refuse: true,
            kill: Kill::Fin,
            wake: false,
        };
        world.trouble(outage).await;
        world.shows(RETRY_BOUND, "a connection", connected).await;
        assert_eq!(world.seen.shows(), 1);
        assert_eq!(world.machine().sessions.len(), 3);
        world.settle().await;
        world.finish().await;
    });
}

/// Through a hub, the daemon's link to the hub dies without a word: no FIN, as when the
/// daemon's machine changes networks. The app finds its stream dead within 15 s, but the
/// hub can only reach the daemon again once the daemon has found its own connection dead,
/// which its keepalive does within 80 s. The machine shows that once, and is back.
#[test]
fn a_daemons_link_to_the_hub_that_dies_silently_shows_once_and_recovers() {
    run(async {
        let mut world = World::relayed(23, 4).await;
        tokio::time::sleep(secs(2)).await;
        let now = Instant::now();
        world.excuse(now, now + secs(140));
        world.kill_host_leg(Kill::Silent);
        // Every attempt meanwhile is a race of the hub's two addresses.
        world.allowed += 30;
        tokio::time::sleep(secs(25)).await;
        let machine = world.machine();
        assert!(!connected(&machine), "{machine:?}");
        assert_eq!(machine.sessions.len(), 3);
        world.shows(secs(110), "a connection", connected).await;
        assert_eq!(world.seen.shows(), 1);

        // And works as before.
        let sent = world
            .app
            .send(world.host.clone(), prompt(&world.session, 1))
            .await;
        assert_eq!(sent.unwrap(), CommandResult::Applied);
        world.did.prompts = 1;
        world.settle().await;
        world.finish().await;
    });
}
