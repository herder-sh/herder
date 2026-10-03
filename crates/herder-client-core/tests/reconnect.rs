//! The client against a real daemon, with the fake adapter, over TLS on localhost: pairing, a
//! turn, a daemon killed and restarted mid-turn, a terminal across a cut connection, an
//! account login, resource figures, the sync barrier, renaming and forgetting a machine, and the
//! app lifecycle: suspension, wake probes and the offline cache.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use herder_adapters::fake::FakeAdapter;
use herder_client_core::PairingUri;
use herder_client_core::{
    Client, ConnectionState, Error, NewAccount, SessionSubscription, SessionUpdate, TerminalEvent,
    TerminalStream,
};
use herder_daemon::auth::{Auth, PAIRING_TTL};
use herder_daemon::login::{LoginProgram, LoginStatus, Logins};
use herder_daemon::session::{AccountConfig, Accounts, Adapters, SessionManager, Setup};
use herder_daemon::terminal::Terminals;
use herder_daemon::worktree::Worktrees;
use herder_daemon::ws::{Host, Server, Tls};
use herder_daemon::{Hub, session};
use herder_protocol::{
    AccountId, CommandBody, CommandResult, Constraint, Container, ContainerState, ErrorCode, Event,
    EventBody, FailoverSettings, HostId, HostResources, ItemBody, PermissionMode, Provider, Role,
    SessionId, SessionStatus, SessionUsage, TerminalPurpose, TurnId,
};
use herder_store::Store;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(20);

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
}

fn fake_provider() -> Provider {
    Provider::Other("fake".into())
}

/// A stand-in for a provider's device login: prints a URL, reads the code typed in the
/// terminal, and succeeds only for the right one.
fn fake_login() -> LoginProgram {
    let script = r#"echo "Open https://example.com/device to log in"
printf 'Code: '
read code
[ "$code" = "ABCD" ] || exit 1
touch "$FAKE_CONFIG_DIR/logged-in"
echo "Logged in"
"#;
    LoginProgram {
        program: PathBuf::from("/bin/sh"),
        args: vec!["-c".into(), script.into()],
        config_env: vec!["FAKE_CONFIG_DIR".into()],
        status: LoginStatus {
            args: vec!["-c".into(), r#"[ -e "$FAKE_CONFIG_DIR/logged-in" ]"#.into()],
            logged_in_field: None,
        },
    }
}

fn failover() -> FailoverSettings {
    FailoverSettings { pin: true }
}

fn account() -> AccountId {
    AccountId::new("account-1")
}

/// A daemon on its own runtime, so killing it drops every task at once, as a killed process
/// would: no close frames, no clean shutdown of its sessions.
struct Daemon {
    runtime: Option<tokio::runtime::Runtime>,
    addr: SocketAddr,
    fingerprint: String,
    auth: Arc<Auth>,
    hub: Arc<Hub>,
}

impl Daemon {
    /// Starts a daemon on `dir`, listening on `port` (0 for any), whose sessions run `script`;
    /// `turns` numbers turn ids across restarts, as the scripts expect.
    async fn start(dir: &Path, port: u16, script: &str, turns: Arc<AtomicU64>) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (dir, script) = (dir.to_owned(), fixture(script));
        let (started, ready) = tokio::sync::oneshot::channel();
        std::fs::create_dir_all(dir.join("tls")).unwrap();
        runtime.spawn(async move {
            let tls = Tls::load_or_create(&dir.join("tls"), "test-host").unwrap();
            let auth = Arc::new(Auth::open(&dir).unwrap());
            let hub = Arc::new(Hub::default());
            hub.set_failover(failover());
            let fake = fake_provider();
            let mut adapters = Adapters::new();
            adapters.register(fake.clone(), Arc::new(FakeAdapter::new(script)));
            let mut accounts = Accounts::new();
            accounts.insert(
                account(),
                AccountConfig {
                    provider: fake,
                    label: "Account 1".into(),
                    config_dir: Some(dir.join("account")),
                },
            );
            let setup = Setup {
                store: Store::open(dir.join("herder.db")).unwrap(),
                adapters,
                accounts,
                sink: Arc::clone(&hub) as Arc<dyn session::EventSink>,
                turn_ids: Box::new(move || {
                    TurnId::new(format!("turn-{}", turns.fetch_add(1, Ordering::SeqCst) + 1))
                }),
                worktrees: Worktrees::new(dir.join("worktrees")),
                attachments: dir.join("attachments"),
            };
            let shutdown = CancellationToken::new();
            let sessions = SessionManager::open(setup, shutdown.clone()).await.unwrap();
            let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
            let addr = listener.local_addr().unwrap();
            let fingerprint = tls.fingerprint().to_owned();
            let host = Host {
                id: HostId::new("host-1"),
                name: "test-host".into(),
            };
            let terminals = Terminals::new(Arc::clone(&hub), PathBuf::from("/bin/sh"));
            // The login is fake; the account it adds is a real provider's, as the config holds.
            let logins = Logins::new(
                HashMap::from([(Provider::Codex, fake_login())]),
                dir.join("daemon.toml"),
                sessions.clone(),
            );
            let server = Server::new(
                tls,
                Arc::clone(&auth),
                Arc::clone(&hub),
                sessions,
                terminals,
                logins,
                host,
            );
            started.send((addr, fingerprint, auth, hub)).ok().unwrap();
            server.run(listener, shutdown).await;
        });
        let (addr, fingerprint, auth, hub) = ready.await.unwrap();
        Self {
            runtime: Some(runtime),
            addr,
            fingerprint,
            auth,
            hub,
        }
    }

    /// A pairing link with a fresh code, as `herder pair` prints it.
    fn pairing_link(&self) -> String {
        self.pairing_link_via("alice", self.addr)
    }

    /// A pairing link for `user` that names `addr` as the daemon's address.
    fn pairing_link_via(&self, user: &str, addr: SocketAddr) -> String {
        let code = self.auth.mint(user, None, PAIRING_TTL).unwrap().code;
        PairingUri {
            hosts: vec![addr.to_string()],
            fingerprint: self.fingerprint.clone(),
            code,
        }
        .to_string()
    }

    /// Kills the daemon: drops every task where it stands and waits until they are gone.
    async fn kill(mut self) {
        let runtime = self.runtime.take().unwrap();
        tokio::task::spawn_blocking(move || runtime.shutdown_timeout(TIMEOUT))
            .await
            .unwrap();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// A git repository with one commit, for sessions to work on.
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

/// What a UI built from a subscription's updates: every event once, in order.
#[derive(Default)]
struct View {
    events: Vec<Event>,
    latest: SessionUpdate,
}

impl View {
    /// Reads updates until `done` holds, checking that events never repeat or skip a seq.
    async fn read_until(&mut self, sub: &SessionSubscription, done: impl Fn(&View) -> bool) {
        while !done(self) {
            let update = tokio::time::timeout(TIMEOUT, sub.next())
                .await
                .expect("no update in time")
                .expect("the subscription ended");
            for event in &update.events {
                let expected = self.events.last().map_or(1, |last| last.seq + 1);
                assert_eq!(event.seq, expected, "a gap or a duplicate: {event:?}");
                self.events.push(event.clone());
            }
            self.latest = update;
        }
    }

    fn has(&self, wanted: impl Fn(&EventBody) -> bool) -> bool {
        self.events.iter().any(|event| wanted(&event.body))
    }

    fn streaming_text(&self) -> Vec<String> {
        self.latest
            .streaming
            .iter()
            .filter_map(|item| match &item.body {
                ItemBody::AssistantMessage { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }
}

/// Waits until the machine's connection satisfies `wanted`.
async fn wait_connection(client: &Client, wanted: impl Fn(&ConnectionState) -> bool) {
    let changes = client.changes();
    tokio::time::timeout(TIMEOUT, async {
        while !client
            .machines()
            .iter()
            .any(|machine| wanted(&machine.connection))
        {
            assert!(changes.next().await);
        }
    })
    .await
    .expect("the connection did not get there in time");
}

fn connected(state: &ConnectionState) -> bool {
    *state == ConnectionState::Connected
}

fn turn_ended(turn: &str) -> impl Fn(&View) -> bool {
    let turn = TurnId::new(turn);
    move |view| {
        view.has(|body| match body {
            EventBody::TurnCompleted { turn_id } | EventBody::TurnFailed { turn_id, .. } => {
                *turn_id == turn
            }
            _ => false,
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_daemon_killed_mid_turn_leaves_no_gap_and_no_duplicate() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("daemon");
    let config = tmp.path().join("client");
    let repo = repo(&tmp.path().join("app"));
    let turns = Arc::new(AtomicU64::new(0));
    let daemon = Daemon::start(&data, 0, "mid_turn.jsonl", Arc::clone(&turns)).await;

    let client = Client::open(config.display().to_string(), "herder-test/0".into()).unwrap();
    let machine = client.pair(daemon.pairing_link()).await.unwrap();
    let host = machine.host_id;
    assert_eq!(host, HostId::new("host-1"));
    assert_eq!(machine.name, "test-host");
    wait_connection(&client, connected).await;
    assert_eq!(
        client.machines()[0].role,
        Some(herder_protocol::Role::Owner)
    );

    let created = client
        .send(
            host.clone(),
            CommandBody::CreateSession {
                repo: Some(repo),
                project_id: None,
                branch: None,
                account_id: Some(account()),
                provider: None,
                model: None,
                permission_mode: Some(PermissionMode::Ask),
                max_children: None,
                failover_pin: None,
            },
        )
        .await
        .unwrap();
    let CommandResult::SessionCreated { session_id } = created else {
        panic!("expected a session, got {created:?}");
    };
    let sub = client
        .subscribe_session(host.clone(), session_id.clone())
        .unwrap();
    let prompt = |text: &str| CommandBody::SendPrompt {
        session_id: session_id.clone(),
        text: text.into(),
        images: Vec::new(),
    };
    let sent = client.send(host.clone(), prompt("First.")).await.unwrap();
    assert_eq!(sent, CommandResult::Applied);

    // Mid-turn: one item done, another streaming.
    let mut view = View::default();
    view.read_until(&sub, |view| view.streaming_text() == ["Half"])
        .await;
    assert!(!turn_ended("turn-1")(&view));
    let port = daemon.addr.port();
    daemon.kill().await;
    wait_connection(&client, |state| {
        matches!(state, ConnectionState::Disconnected { .. })
    })
    .await;

    // The restarted daemon closes the abandoned turn; the client resumes from its cursor.
    let daemon = Daemon::start(&data, port, "after_restart.jsonl", turns).await;
    view.read_until(&sub, turn_ended("turn-1")).await;
    assert!(view.latest.streaming.is_empty(), "{:?}", view.latest);
    let sent = client.send(host.clone(), prompt("Second.")).await.unwrap();
    assert_eq!(sent, CommandResult::Applied);
    // The session goes idle last, once the turn has ended.
    view.read_until(&sub, |view| {
        turn_ended("turn-2")(view)
            && view.events.last().is_some_and(|event| {
                event.body
                    == EventBody::SessionStatusChanged {
                        status: SessionStatus::Idle,
                    }
            })
    })
    .await;

    // A new client on the same profile connects without pairing and replays the session.
    drop(sub);
    drop(client);
    let client = Client::open(config.display().to_string(), "herder-test/0".into()).unwrap();
    wait_connection(&client, connected).await;
    let sub = client
        .subscribe_session(host.clone(), session_id.clone())
        .unwrap();
    let mut replayed = View::default();
    let last = view.events.last().unwrap().seq;
    replayed
        .read_until(&sub, |replayed| {
            replayed
                .events
                .last()
                .is_some_and(|event| event.seq == last)
        })
        .await;
    assert_eq!(replayed.events, view.events);

    daemon.kill().await;
    let journal = Store::open(data.join("herder.db"))
        .unwrap()
        .read_since(&session_id, 0, usize::MAX)
        .unwrap();
    assert_eq!(view.events, journal);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn synced_waits_for_the_lists_and_the_replay() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("client");
    let repo = repo(&tmp.path().join("app"));
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    let client = Client::open(config.display().to_string(), "herder-test/0".into()).unwrap();
    let host = client.pair(daemon.pairing_link()).await.unwrap().host_id;
    let created = client
        .send(
            host.clone(),
            CommandBody::CreateSession {
                repo: Some(repo),
                project_id: None,
                branch: None,
                account_id: Some(account()),
                provider: None,
                model: None,
                permission_mode: Some(PermissionMode::Ask),
                max_children: None,
                failover_pin: None,
            },
        )
        .await
        .unwrap();
    let CommandResult::SessionCreated { session_id } = created else {
        panic!("expected a session, got {created:?}");
    };
    drop(client);

    // A fresh client has nothing cached; once synced, its lists and the replay are in.
    let client = Client::open(config.display().to_string(), "herder-test/0".into()).unwrap();
    let sub = client
        .subscribe_session(host.clone(), session_id.clone())
        .unwrap();
    tokio::time::timeout(TIMEOUT, client.synced(host.clone()))
        .await
        .unwrap()
        .unwrap();
    let machine = client.machines().remove(0);
    assert_eq!(machine.connection, ConnectionState::Connected);
    let head = &machine.sessions[0];
    assert_eq!(head.session_id, session_id);
    assert_eq!(
        (head.status, &head.account_id),
        (SessionStatus::Idle, &account())
    );
    assert_eq!(machine.failover, failover());
    assert_eq!(machine.accounts.len(), 1);
    let update = sub.next().await.unwrap();
    assert!(matches!(
        update.events.first().map(|event| &event.body),
        Some(EventBody::SessionCreated { .. })
    ));
    assert_eq!(
        update.events.last().map(|event| event.seq),
        Some(head.head_seq)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_renamed_machine_keeps_its_name_and_a_forgotten_one_is_gone() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("client");
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    let client = Client::open(config.display().to_string(), "herder-test/0".into()).unwrap();
    let host = client.pair(daemon.pairing_link()).await.unwrap().host_id;
    client.rename(host.clone(), "build box".into()).unwrap();
    assert_eq!(client.machines()[0].name, "build box");
    // Pairing again keeps the name.
    client.pair(daemon.pairing_link()).await.unwrap();
    assert_eq!(client.machines()[0].name, "build box");
    drop(client);

    let client = Client::open(config.display().to_string(), "herder-test/0".into()).unwrap();
    assert_eq!(client.machines()[0].name, "build box");
    let unknown = HostId::new("host-2");
    assert_eq!(
        client.rename(unknown.clone(), "x".into()),
        Err(Error::UnknownMachine {
            host_id: unknown.clone()
        })
    );
    assert_eq!(
        client.forget(unknown.clone()),
        Err(Error::UnknownMachine { host_id: unknown })
    );
    let sub = client
        .subscribe_session(host.clone(), SessionId::new("s"))
        .unwrap();
    client.forget(host.clone()).unwrap();
    assert!(client.machines().is_empty());
    assert!(sub.next().await.is_none());
    drop(client);

    let client = Client::open(config.display().to_string(), "herder-test/0".into()).unwrap();
    assert!(client.machines().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pairing_fails_on_a_wrong_code_or_fingerprint_and_saves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    let config = tmp.path().join("client");
    let client = Client::open(config.display().to_string(), "herder-test/0".into()).unwrap();

    let link: PairingUri = daemon.pairing_link().parse().unwrap();
    let wrong_code = PairingUri {
        code: "AAAAA-AAAAA".into(),
        ..link.clone()
    };
    let err = client.pair(wrong_code.to_string()).await.unwrap_err();
    assert!(
        matches!(&err, Error::Pairing { message } if message.contains("pairing code")),
        "{err}"
    );
    let wrong_fingerprint = PairingUri {
        fingerprint: "00".repeat(32),
        ..link.clone()
    };
    let err = client
        .pair(wrong_fingerprint.to_string())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Pairing { message } if message.contains("fingerprint")),
        "{err}"
    );
    assert!(matches!(
        client.pair("https://example.com".into()).await,
        Err(Error::InvalidLink { .. })
    ));
    assert!(client.machines().is_empty());
    assert!(!config.join("machines.json").exists());

    // The real code still works, and a command the daemon refuses comes back as rejected.
    let host = client.pair(link.to_string()).await.unwrap().host_id;
    let refused = client
        .send(
            host.clone(),
            CommandBody::SendPrompt {
                session_id: SessionId::new("nope"),
                text: "Hi.".into(),
                images: Vec::new(),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(refused, Error::Rejected { info } if info.code == ErrorCode::NotFound));
    let unknown = client
        .send(
            HostId::new("other"),
            CommandBody::Interrupt {
                session_id: SessionId::new("nope"),
            },
        )
        .await;
    assert!(matches!(unknown, Err(Error::UnknownMachine { .. })));
}

/// A TCP relay to the daemon whose connections can be cut, as a network drop would, or stalled,
/// as an OS that kills a suspended app's socket without telling it would, while the daemon and
/// its terminals keep running.
struct Relay {
    addr: SocketAddr,
    cut: tokio::sync::watch::Sender<u64>,
    stall: tokio::sync::watch::Sender<u64>,
    accepted: Arc<AtomicU64>,
}

impl Relay {
    async fn start(target: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cut = tokio::sync::watch::Sender::new(0);
        let stall = tokio::sync::watch::Sender::new(0);
        let accepted = Arc::new(AtomicU64::new(0));
        let (cuts, stalls, count) = (cut.clone(), stall.clone(), Arc::clone(&accepted));
        tokio::spawn(async move {
            while let Ok((mut inbound, _)) = listener.accept().await {
                count.fetch_add(1, Ordering::SeqCst);
                let mut cut = cuts.subscribe();
                let mut stall = stalls.subscribe();
                tokio::spawn(async move {
                    let mut outbound = TcpStream::connect(target).await.unwrap();
                    tokio::select! {
                        _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound) => {}
                        _ = cut.changed() => {}
                        // Both ends stay open, and nothing goes through any more.
                        _ = stall.changed() => std::future::pending::<()>().await,
                    }
                });
            }
        });
        Self {
            addr,
            cut,
            stall,
            accepted,
        }
    }

    /// Drops every connection through the relay now.
    fn cut(&self) {
        self.cut.send_modify(|version| *version += 1);
    }

    /// Silently stops every connection through the relay now; new ones work.
    fn stall(&self) {
        self.stall.send_modify(|version| *version += 1);
    }

    /// How many connections the relay accepted.
    fn accepted(&self) -> u64 {
        self.accepted.load(Ordering::SeqCst)
    }
}

/// What a terminal stream delivered: its events, and its output so far as text.
#[derive(Default)]
struct Screen {
    events: Vec<TerminalEvent>,
    text: String,
}

impl Screen {
    /// Reads events until the output holds `wanted`.
    async fn read_until(&mut self, stream: &TerminalStream, wanted: &str) {
        while !self.text.contains(wanted) {
            let event = tokio::time::timeout(TIMEOUT, stream.next())
                .await
                .unwrap_or_else(|_| panic!("no {wanted:?} in time; got {:?}", self.text))
                .expect("the stream ended");
            if let TerminalEvent::Output { data } = &event {
                self.text.push_str(&String::from_utf8_lossy(data));
            }
            self.events.push(event);
        }
    }
}

async fn next_event(stream: &TerminalStream) -> Option<TerminalEvent> {
    tokio::time::timeout(TIMEOUT, stream.next())
        .await
        .expect("no terminal event in time")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_terminal_streams_across_a_cut_connection_until_its_exit() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(&tmp.path().join("app"));
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    let relay = Relay::start(daemon.addr).await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    let host = client
        .pair(daemon.pairing_link_via("alice", relay.addr))
        .await
        .unwrap()
        .host_id;
    let created = client
        .send(
            host.clone(),
            CommandBody::CreateSession {
                repo: Some(repo),
                project_id: None,
                branch: None,
                account_id: Some(account()),
                provider: None,
                model: None,
                permission_mode: Some(PermissionMode::Ask),
                max_children: None,
                failover_pin: None,
            },
        )
        .await
        .unwrap();
    let CommandResult::SessionCreated { session_id } = created else {
        panic!("expected a session, got {created:?}");
    };

    let stream = client
        .open_terminal(host.clone(), session_id.clone(), 80, 24)
        .await
        .unwrap();
    let terminal_id = stream.terminal_id();
    let mut screen = Screen::default();
    stream.input(b"printf 'he%s\\n' llo\n".to_vec());
    screen.read_until(&stream, "hello").await;
    assert!(!screen.events.contains(&TerminalEvent::Reattached));

    // Another owner client attaches and gets the scrollback; a member gets nothing.
    let other = Client::open(
        tmp.path().join("other").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    other.pair(daemon.pairing_link()).await.unwrap();
    let watcher = other
        .attach_terminal(host.clone(), terminal_id.clone())
        .await
        .unwrap();
    Screen::default().read_until(&watcher, "hello").await;
    let err = other
        .attach_terminal(host.clone(), terminal_id.clone())
        .await
        .err();
    assert!(matches!(err, Some(Error::Local { .. })), "{err:?}");
    drop(watcher);
    let member = Client::open(
        tmp.path().join("member").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    member
        .pair(daemon.pairing_link_via("bob", daemon.addr))
        .await
        .unwrap();
    wait_connection(&member, connected).await;
    assert_eq!(member.machines()[0].role, Some(Role::Member));
    for refused in [
        member
            .attach_terminal(host.clone(), terminal_id.clone())
            .await
            .err(),
        member
            .open_terminal(host.clone(), session_id.clone(), 80, 24)
            .await
            .err(),
    ] {
        assert!(
            matches!(&refused, Some(Error::Rejected { info }) if info.code == ErrorCode::Forbidden),
            "{refused:?}"
        );
    }

    // The connection drops; a resize made meanwhile lands once the stream re-attaches.
    relay.cut();
    wait_connection(&client, |state| {
        matches!(state, ConnectionState::Disconnected { .. })
    })
    .await;
    stream.resize(100, 30);
    // Output sent before the cut may still be queued; the replayed scrollback follows the mark.
    while next_event(&stream).await != Some(TerminalEvent::Reattached) {}
    let mut after = Screen::default();
    after.read_until(&stream, "hello").await;
    stream.input(b"stty size\n".to_vec());
    after.read_until(&stream, "30 100").await;

    // Exiting the shell delivers its code and ends the stream.
    stream.input(b"exit 7\n".to_vec());
    loop {
        match next_event(&stream).await {
            Some(TerminalEvent::Output { .. }) => {}
            Some(event) => {
                assert_eq!(event, TerminalEvent::Closed { exit_code: Some(7) });
                break;
            }
            None => panic!("the stream ended without its exit"),
        }
    }
    assert_eq!(next_event(&stream).await, None);
    daemon.kill().await;
}

/// Reads a login terminal until it closes; returns its exit code.
async fn login_exit(stream: &TerminalStream, screen: &mut Screen) -> Option<i32> {
    loop {
        match next_event(stream).await {
            Some(TerminalEvent::Output { data }) => {
                screen.text.push_str(&String::from_utf8_lossy(&data))
            }
            Some(TerminalEvent::Closed { exit_code }) => return exit_code,
            Some(event) => panic!("unexpected {event:?}"),
            None => panic!("the stream ended without its exit"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adding_an_account_relays_its_login_and_saves_the_account() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon_dir = tmp.path().join("daemon");
    let daemon = Daemon::start(&daemon_dir, 0, "mid_turn.jsonl", Arc::default()).await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    let host = client.pair(daemon.pairing_link()).await.unwrap().host_id;
    let config_dir = tmp.path().join("codex-2");
    let new_account = |account_id: &str| NewAccount {
        account_id: AccountId::new(account_id),
        provider: Provider::Codex,
        label: Some("Second".into()),
        config_dir: Some(config_dir.to_str().unwrap().to_owned()),
    };

    // Members never see a login.
    let member = Client::open(
        tmp.path().join("member").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    member
        .pair(daemon.pairing_link_via("bob", daemon.addr))
        .await
        .unwrap();
    let refused = member
        .add_account(host.clone(), new_account("codex-2"), 80, 24)
        .await
        .err();
    assert!(
        matches!(&refused, Some(Error::Rejected { info }) if info.code == ErrorCode::Forbidden),
        "{refused:?}"
    );
    // Nor does an id already taken.
    let taken = client
        .add_account(host.clone(), new_account(account().as_str()), 80, 24)
        .await
        .err();
    assert!(
        matches!(&taken, Some(Error::Rejected { info }) if info.code == ErrorCode::Conflict),
        "{taken:?}"
    );

    // A failed login adds nothing.
    let stream = client
        .add_account(host.clone(), new_account("codex-2"), 80, 24)
        .await
        .unwrap();
    let mut screen = Screen::default();
    screen.read_until(&stream, "Code: ").await;
    stream.input(b"WRONG\n".to_vec());
    assert_eq!(login_exit(&stream, &mut screen).await, Some(1));
    assert!(
        screen.text.contains("herder: the login exited with 1"),
        "{}",
        screen.text
    );
    assert!(!config_dir.exists());
    drop(stream);

    let stream = client
        .add_account(host.clone(), new_account("codex-2"), 80, 24)
        .await
        .unwrap();
    let mut screen = Screen::default();
    screen.read_until(&stream, "Code: ").await;
    assert!(screen.text.contains("https://example.com/device"));
    let listed = |client: &Client| {
        client.machines()[0].terminals.iter().any(|terminal| {
            terminal.terminal_id == stream.terminal_id()
                && terminal.purpose
                    == TerminalPurpose::Login {
                        account_id: AccountId::new("codex-2"),
                    }
        })
    };
    let changes = client.changes();
    tokio::time::timeout(TIMEOUT, async {
        while !listed(&client) {
            changes.next().await;
        }
    })
    .await
    .expect("the login terminal was never listed");

    stream.input(b"ABCD\n".to_vec());
    assert_eq!(login_exit(&stream, &mut screen).await, Some(0));
    assert!(
        screen.text.contains("herder: added account codex-2"),
        "{}",
        screen.text
    );
    // The login ran in the config dir it was given.
    assert!(config_dir.join("logged-in").exists());
    // The account is listed to every client, and saved to the config.
    for client in [&client, &member] {
        let added = |client: &Client| {
            client.machines()[0].accounts.iter().any(|account| {
                account.account_id.as_str() == "codex-2" && account.label == "Second"
            })
        };
        let changes = client.changes();
        tokio::time::timeout(TIMEOUT, async {
            while !added(client) {
                changes.next().await;
            }
        })
        .await
        .expect("the account was never listed");
    }
    let saved = std::fs::read_to_string(daemon_dir.join("daemon.toml")).unwrap();
    assert!(saved.contains("id = \"codex-2\""), "{saved}");
    daemon.kill().await;
}

fn host_resources(running_turns: u32) -> HostResources {
    HostResources {
        cpu_cores: 8,
        cpu_percent: 42.5,
        load_1m: 3.2,
        memory_total_bytes: 16 << 30,
        memory_available_bytes: 6 << 30,
        pressure: None,
        running_turns,
        max_turns: 4,
        waiting_turns: 1,
        constraint: (running_turns == 4).then_some(Constraint::MaxTurns),
    }
}

/// Waits until the first machine's view satisfies `wanted`.
async fn wait_machine(client: &Client, wanted: impl Fn(&herder_client_core::Machine) -> bool) {
    let changes = client.changes();
    tokio::time::timeout(TIMEOUT, async {
        while !client.machines().first().is_some_and(&wanted) {
            assert!(changes.next().await);
        }
    })
    .await
    .expect("the machine never got there");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_and_session_resources_stay_current_while_connected() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    // Figures from before the client connects reach it after hello.
    daemon.hub.host_resources(host_resources(3));
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    client.pair(daemon.pairing_link()).await.unwrap();
    wait_machine(&client, |m| m.resources == Some(host_resources(3))).await;

    // Later figures replace them.
    daemon.hub.host_resources(host_resources(4));
    wait_machine(&client, |m| m.resources == Some(host_resources(4))).await;
    let session = SessionId::new("s1");
    let usage = SessionUsage {
        cpu_percent: 12.0,
        memory_bytes: 512 << 20,
        processes: 3,
        containers: vec![Container {
            id: "c1".into(),
            name: "app-db-1".into(),
            compose_project: Some("app".into()),
            image: "postgres:16".into(),
            state: ContainerState::Running,
        }],
    };
    daemon.hub.session_resources(&session, usage.clone());
    wait_machine(&client, |m| m.session_usage.get(&session) == Some(&usage)).await;
    // A session whose processes and containers are gone leaves the map.
    let idle = SessionUsage {
        cpu_percent: 0.0,
        memory_bytes: 0,
        processes: 0,
        containers: Vec::new(),
    };
    daemon.hub.session_resources(&session, idle);
    wait_machine(&client, |m| m.session_usage.is_empty()).await;

    // Live figures go with the connection.
    daemon.hub.session_resources(&session, usage);
    wait_machine(&client, |m| !m.session_usage.is_empty()).await;
    daemon.kill().await;
    wait_machine(&client, |m| {
        !connected(&m.connection) && m.resources.is_none() && m.session_usage.is_empty()
    })
    .await;
}

/// A connection is pinged as soon as it is up: its round trip, when it was established and how
/// often it was re-established are reported, and the round trips go with the connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connection_quality_reports_round_trips_and_reconnects() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("daemon");
    let daemon = Daemon::start(&data, 0, "mid_turn.jsonl", Arc::default()).await;
    let port = daemon.addr.port();
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    client.pair(daemon.pairing_link()).await.unwrap();
    wait_machine(&client, |m| m.quality.last_rtt_ms.is_some()).await;
    let quality = client.machines()[0].quality.clone();
    assert!(connected(&client.machines()[0].connection));
    assert!(quality.connected_since.is_some());
    assert_eq!(quality.reconnects, 0);
    assert_eq!(quality.missed_pongs, 0);
    let rtt = quality.last_rtt_ms.unwrap();
    assert_eq!(quality.average_rtt_ms, Some(rtt));
    assert_eq!(quality.min_rtt_ms, Some(rtt));
    assert_eq!(quality.max_rtt_ms, Some(rtt));

    daemon.kill().await;
    wait_machine(&client, |m| {
        !connected(&m.connection)
            && m.quality.connected_since.is_none()
            && m.quality.last_rtt_ms.is_none()
    })
    .await;
    let _daemon = Daemon::start(&data, port, "mid_turn.jsonl", Arc::default()).await;
    wait_machine(&client, |m| {
        m.quality.reconnects == 1
            && m.quality.connected_since.is_some()
            && m.quality.last_rtt_ms.is_some()
    })
    .await;
}

async fn create_session(client: &Client, host: &HostId, repo: String) -> SessionId {
    let created = client
        .send(
            host.clone(),
            CommandBody::CreateSession {
                repo: Some(repo),
                project_id: None,
                branch: None,
                account_id: Some(account()),
                provider: None,
                model: None,
                permission_mode: Some(PermissionMode::Ask),
                max_children: None,
                failover_pin: None,
            },
        )
        .await
        .unwrap();
    let CommandResult::SessionCreated { session_id } = created else {
        panic!("expected a session, got {created:?}");
    };
    session_id
}

fn interrupted(turn: &str) -> impl Fn(&View) -> bool {
    let turn = TurnId::new(turn);
    move |view| {
        view.has(|body| matches!(body, EventBody::TurnInterrupted { turn_id } if *turn_id == turn))
    }
}

fn status(machine: &herder_client_core::Machine, session_id: &SessionId) -> SessionStatus {
    machine
        .sessions
        .iter()
        .find(|head| head.session_id == *session_id)
        .expect("the session is listed")
        .status
}

/// The phone goes to the background mid-turn, the OS silently kills its socket, and another
/// device ends the turn. Ten minutes later, on a paused clock, the phone comes back: it did not
/// retry while suspended, and its wake resumes the session with no gap.
#[tokio::test]
async fn a_ten_minute_suspension_resumes_without_a_gap() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(&tmp.path().join("app"));
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "interrupted_turn.jsonl",
        Arc::default(),
    )
    .await;
    let relay = Relay::start(daemon.addr).await;
    let phone = Client::open(
        tmp.path().join("phone").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    let host = phone
        .pair(daemon.pairing_link_via("alice", relay.addr))
        .await
        .unwrap()
        .host_id;
    let session_id = create_session(&phone, &host, repo).await;
    let sub = phone
        .subscribe_session(host.clone(), session_id.clone())
        .unwrap();
    let prompt = CommandBody::SendPrompt {
        session_id: session_id.clone(),
        text: "First.".into(),
        images: Vec::new(),
    };
    assert_eq!(
        phone.send(host.clone(), prompt).await.unwrap(),
        CommandResult::Applied
    );
    let mut view = View::default();
    view.read_until(&sub, |view| view.streaming_text() == ["Half"])
        .await;

    phone.suspend();
    relay.stall();
    let desktop = Client::open(
        tmp.path().join("desktop").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    desktop.pair(daemon.pairing_link()).await.unwrap();
    let watched = desktop
        .subscribe_session(host.clone(), session_id.clone())
        .unwrap();
    let interrupt = CommandBody::Interrupt {
        session_id: session_id.clone(),
    };
    desktop.send(host.clone(), interrupt).await.unwrap();
    let mut full = View::default();
    full.read_until(&watched, interrupted("turn-1")).await;
    // Its timers would run on the paused clock too.
    drop(watched);
    drop(desktop);

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(10 * 60)).await;
    // On resume the silence is noticed, and nothing is retried while suspended.
    wait_connection(&phone, |state| {
        matches!(state, ConnectionState::Disconnected { .. })
    })
    .await;
    tokio::time::sleep(Duration::from_secs(5 * 60)).await;
    // One connection paired, the other was the supervisor's.
    assert_eq!(relay.accepted(), 2);
    assert!(matches!(
        phone.machines()[0].connection,
        ConnectionState::Disconnected { .. }
    ));
    tokio::time::resume();

    phone.wake();
    view.read_until(&sub, interrupted("turn-1")).await;
    view.read_until(&sub, |view| view.events.len() >= full.events.len())
        .await;
    assert_eq!(view.events[..full.events.len()], full.events);
    assert!(view.latest.streaming.is_empty(), "{:?}", view.latest);
    assert_eq!(relay.accepted(), 3);
}

/// Waking with a connection up checks it: a healthy one is kept, a silently dead one replaced.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wake_keeps_a_healthy_connection_and_replaces_a_dead_one() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    let relay = Relay::start(daemon.addr).await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    let host = client
        .pair(daemon.pairing_link_via("alice", relay.addr))
        .await
        .unwrap()
        .host_id;
    client.synced(host.clone()).await.unwrap();
    // The pairing itself went through the relay too.
    assert_eq!(relay.accepted(), 2);

    client.suspend();
    client.wake();
    client.synced(host.clone()).await.unwrap();
    // Past the probe's deadline, the connection is the same one.
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert_eq!(relay.accepted(), 2);
    assert_eq!(client.machines()[0].connection, ConnectionState::Connected);

    relay.stall();
    client.wake();
    let changes = client.changes();
    tokio::time::timeout(TIMEOUT, async {
        while relay.accepted() < 3 || client.machines()[0].connection != ConnectionState::Connected
        {
            assert!(changes.next().await);
        }
    })
    .await
    .expect("the dead connection was not replaced in time");
    client.synced(host).await.unwrap();
}

/// A client opens on its offline cache, with no daemon to talk to, then live data takes over
/// and is what the cache holds from then on.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_offline_cache_shows_the_last_state_and_live_data_wins() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("daemon");
    let config = tmp.path().join("client").display().to_string();
    let repo = repo(&tmp.path().join("app"));
    let turns = Arc::new(AtomicU64::new(0));
    let daemon = Daemon::start(&data, 0, "mid_turn.jsonl", Arc::clone(&turns)).await;
    let client = Client::open(config.clone(), "herder-test/0".into()).unwrap();
    let host = client.pair(daemon.pairing_link()).await.unwrap().host_id;
    let session_id = create_session(&client, &host, repo).await;
    let sub = client
        .subscribe_session(host.clone(), session_id.clone())
        .unwrap();
    let prompt = |text: &str| CommandBody::SendPrompt {
        session_id: session_id.clone(),
        text: text.into(),
        images: Vec::new(),
    };
    client.send(host.clone(), prompt("First.")).await.unwrap();
    let mut before = View::default();
    before
        .read_until(&sub, |view| view.streaming_text() == ["Half"])
        .await;
    // The daemon lists a session again when its status changes: it is running from here on.
    wait_machine(&client, |machine| {
        status(machine, &session_id) == SessionStatus::Running
    })
    .await;
    client.suspend();
    let cached = client.machines()[0].sessions.clone();
    drop(sub);
    drop(client);
    let port = daemon.addr.port();
    daemon.kill().await;

    // Offline, the reopened client shows what it had.
    let client = Client::open(config.clone(), "herder-test/0".into()).unwrap();
    wait_connection(&client, |state| {
        matches!(state, ConnectionState::Disconnected { .. })
    })
    .await;
    assert_eq!(client.machines()[0].role, Some(Role::Owner));
    assert_eq!(client.machines()[0].sessions, cached);
    let sub = client
        .subscribe_session(host.clone(), session_id.clone())
        .unwrap();
    let mut view = View::default();
    view.read_until(&sub, |view| !view.events.is_empty()).await;
    assert_eq!(view.events, before.events);
    // Only durable events are cached.
    assert!(view.latest.streaming.is_empty());

    // Back online, the session resumes after the cached events and the list is the daemon's.
    let daemon = Daemon::start(&data, port, "after_restart.jsonl", turns).await;
    client.wake();
    view.read_until(&sub, turn_ended("turn-1")).await;
    client.send(host.clone(), prompt("Second.")).await.unwrap();
    view.read_until(&sub, |view| {
        turn_ended("turn-2")(view)
            && view.events.last().is_some_and(|event| {
                event.body
                    == EventBody::SessionStatusChanged {
                        status: SessionStatus::Idle,
                    }
            })
    })
    .await;
    wait_machine(&client, |machine| {
        status(machine, &session_id) == SessionStatus::Idle
    })
    .await;
    let live = client.machines()[0].sessions.clone();
    assert!(live[0].head_seq > cached[0].head_seq, "{live:?}");

    // The cache now holds the live state, never the older one it started from.
    client.suspend();
    drop(sub);
    drop(client);
    daemon.kill().await;
    let client = Client::open(config, "herder-test/0".into()).unwrap();
    assert_eq!(client.machines()[0].sessions, live);
    let sub = client.subscribe_session(host, session_id).unwrap();
    let mut reopened = View::default();
    reopened
        .read_until(&sub, |reopened| !reopened.events.is_empty())
        .await;
    assert_eq!(reopened.events, view.events);
}
