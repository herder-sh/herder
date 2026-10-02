//! The client against a real daemon, with the fake adapter, over TLS on localhost: pairing, a
//! turn, a daemon killed and restarted mid-turn, a terminal across a cut connection, and an
//! account login.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use herder_adapters::fake::FakeAdapter;
use herder_client_core::auth::PairingUri;
use herder_client_core::{
    Client, ConnectionState, Error, NewAccount, SessionSubscription, SessionUpdate, TerminalEvent,
    TerminalStream,
};
use herder_daemon::auth::{Auth, PAIRING_TTL};
use herder_daemon::login::{LoginProgram, Logins};
use herder_daemon::session::{AccountConfig, Accounts, Adapters, SessionManager, Setup};
use herder_daemon::terminal::Terminals;
use herder_daemon::worktree::Worktrees;
use herder_daemon::ws::{Host, Server, Tls};
use herder_daemon::{Hub, session};
use herder_protocol::{
    AccountId, CommandBody, CommandResult, ErrorCode, Event, EventBody, HostId, ItemBody,
    PermissionMode, Provider, Role, SessionId, SessionStatus, TerminalPurpose, TurnId,
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
        config_env: "FAKE_CONFIG_DIR".into(),
    }
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
                    failover: false,
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
            let logins = Logins::new(HashMap::from([(fake_provider(), fake_login())]));
            let server = Server::new(
                tls,
                Arc::clone(&auth),
                hub,
                sessions,
                terminals,
                logins,
                host,
            );
            started.send((addr, fingerprint, auth)).ok().unwrap();
            server.run(listener, shutdown).await;
        });
        let (addr, fingerprint, auth) = ready.await.unwrap();
        Self {
            runtime: Some(runtime),
            addr,
            fingerprint,
            auth,
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
            changes.next().await.unwrap();
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

    let client = Client::open(config.clone(), "herder-test/0".into()).unwrap();
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
            &host,
            CommandBody::CreateSession {
                repo,
                branch: None,
                account_id: account(),
                model: None,
                permission_mode: PermissionMode::Ask,
            },
        )
        .await
        .unwrap();
    let CommandResult::SessionCreated { session_id } = created else {
        panic!("expected a session, got {created:?}");
    };
    let sub = client.subscribe_session(&host, &session_id).unwrap();
    let prompt = |text: &str| CommandBody::SendPrompt {
        session_id: session_id.clone(),
        text: text.into(),
    };
    let sent = client.send(&host, prompt("First.")).await.unwrap();
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
    let sent = client.send(&host, prompt("Second.")).await.unwrap();
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
    let client = Client::open(config, "herder-test/0".into()).unwrap();
    wait_connection(&client, connected).await;
    let sub = client.subscribe_session(&host, &session_id).unwrap();
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
    let client = Client::open(config.clone(), "herder-test/0".into()).unwrap();

    let link: PairingUri = daemon.pairing_link().parse().unwrap();
    let wrong_code = PairingUri {
        code: "AAAAA-AAAAA".into(),
        ..link.clone()
    };
    let err = client.pair(wrong_code.to_string()).await.unwrap_err();
    assert!(
        matches!(&err, Error::Pairing(message) if message.contains("pairing code")),
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
        matches!(&err, Error::Pairing(message) if message.contains("fingerprint")),
        "{err}"
    );
    assert!(matches!(
        client.pair("https://example.com".into()).await,
        Err(Error::InvalidLink(_))
    ));
    assert!(client.machines().is_empty());
    assert!(!config.join("machines.json").exists());

    // The real code still works, and a command the daemon refuses comes back as rejected.
    let host = client.pair(link.to_string()).await.unwrap().host_id;
    let refused = client
        .send(
            &host,
            CommandBody::SendPrompt {
                session_id: SessionId::new("nope"),
                text: "Hi.".into(),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(refused, Error::Rejected(info) if info.code == ErrorCode::NotFound));
    let unknown = client
        .send(
            &HostId::new("other"),
            CommandBody::Interrupt {
                session_id: SessionId::new("nope"),
            },
        )
        .await;
    assert!(matches!(unknown, Err(Error::UnknownMachine(_))));
}

/// A TCP relay to the daemon whose connections can be cut, as a network drop would, while the
/// daemon and its terminals keep running.
struct Relay {
    addr: SocketAddr,
    cut: tokio::sync::watch::Sender<u64>,
}

impl Relay {
    async fn start(target: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cut = tokio::sync::watch::Sender::new(0);
        let cuts = cut.clone();
        tokio::spawn(async move {
            while let Ok((mut inbound, _)) = listener.accept().await {
                let mut cut = cuts.subscribe();
                tokio::spawn(async move {
                    let mut outbound = TcpStream::connect(target).await.unwrap();
                    tokio::select! {
                        _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound) => {}
                        _ = cut.changed() => {}
                    }
                });
            }
        });
        Self { addr, cut }
    }

    /// Drops every connection through the relay now.
    fn cut(&self) {
        self.cut.send_modify(|version| *version += 1);
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
            if let TerminalEvent::Output(data) = &event {
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
    let client = Client::open(tmp.path().join("client"), "herder-test/0".into()).unwrap();
    let host = client
        .pair(daemon.pairing_link_via("alice", relay.addr))
        .await
        .unwrap()
        .host_id;
    let created = client
        .send(
            &host,
            CommandBody::CreateSession {
                repo,
                branch: None,
                account_id: account(),
                model: None,
                permission_mode: PermissionMode::Ask,
            },
        )
        .await
        .unwrap();
    let CommandResult::SessionCreated { session_id } = created else {
        panic!("expected a session, got {created:?}");
    };

    let stream = client
        .open_terminal(&host, &session_id, 80, 24)
        .await
        .unwrap();
    let terminal_id = stream.terminal_id();
    let mut screen = Screen::default();
    stream.input(b"printf 'he%s\\n' llo\n".to_vec());
    screen.read_until(&stream, "hello").await;
    assert!(!screen.events.contains(&TerminalEvent::Reattached));

    // Another owner client attaches and gets the scrollback; a member gets nothing.
    let other = Client::open(tmp.path().join("other"), "herder-test/0".into()).unwrap();
    other.pair(daemon.pairing_link()).await.unwrap();
    let watcher = other.attach_terminal(&host, &terminal_id).await.unwrap();
    Screen::default().read_until(&watcher, "hello").await;
    let err = other.attach_terminal(&host, &terminal_id).await.err();
    assert!(matches!(err, Some(Error::Local(_))), "{err:?}");
    drop(watcher);
    let member = Client::open(tmp.path().join("member"), "herder-test/0".into()).unwrap();
    member
        .pair(daemon.pairing_link_via("bob", daemon.addr))
        .await
        .unwrap();
    wait_connection(&member, connected).await;
    assert_eq!(member.machines()[0].role, Some(Role::Member));
    for refused in [
        member.attach_terminal(&host, &terminal_id).await.err(),
        member.open_terminal(&host, &session_id, 80, 24).await.err(),
    ] {
        assert!(
            matches!(&refused, Some(Error::Rejected(info)) if info.code == ErrorCode::Forbidden),
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
            Some(TerminalEvent::Output(_)) => {}
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adding_an_account_relays_its_login_to_the_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    let client = Client::open(tmp.path().join("client"), "herder-test/0".into()).unwrap();
    let host = client.pair(daemon.pairing_link()).await.unwrap().host_id;
    let config_dir = tmp.path().join("fake-2");
    let new_account = |account_id: &str| NewAccount {
        account_id: AccountId::new(account_id),
        provider: fake_provider(),
        label: Some("Second".into()),
        config_dir: Some(config_dir.to_str().unwrap().to_owned()),
    };

    // Members never see a login.
    let member = Client::open(tmp.path().join("member"), "herder-test/0".into()).unwrap();
    member
        .pair(daemon.pairing_link_via("bob", daemon.addr))
        .await
        .unwrap();
    let refused = member
        .add_account(&host, new_account("fake-2"), 80, 24)
        .await
        .err();
    assert!(
        matches!(&refused, Some(Error::Rejected(info)) if info.code == ErrorCode::Forbidden),
        "{refused:?}"
    );
    // Nor does an id already taken.
    let taken = client
        .add_account(&host, new_account(account().as_str()), 80, 24)
        .await
        .err();
    assert!(
        matches!(&taken, Some(Error::Rejected(info)) if info.code == ErrorCode::Conflict),
        "{taken:?}"
    );

    let stream = client
        .add_account(&host, new_account("fake-2"), 80, 24)
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
                        account_id: AccountId::new("fake-2"),
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
    screen.read_until(&stream, "Logged in").await;
    loop {
        match next_event(&stream).await {
            Some(TerminalEvent::Output(_)) => {}
            Some(event) => {
                assert_eq!(event, TerminalEvent::Closed { exit_code: Some(0) });
                break;
            }
            None => panic!("the stream ended without its exit"),
        }
    }
    // The login ran in the config dir it was given.
    assert!(config_dir.join("logged-in").exists());
    daemon.kill().await;
}
