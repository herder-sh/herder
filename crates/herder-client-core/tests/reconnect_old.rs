//! The client against a real daemon, with the fake adapter, over TLS on localhost: pairing, a
//! turn, a daemon killed and restarted mid-turn, a terminal across a cut connection, an
//! account login, an AWS sign-in, resource figures, the sync barrier, renaming and forgetting a machine, a
//! reconnect through a chosen address, the address that answered last, the app lifecycle: suspension, wake probes and the offline cache,
//! a connection gone dead, a slow upload, a new device asked to sign in as a person, and what
//! keeps a client from reconnecting in storms: one hello however many addresses race,
//! connections that die young made less and less often, a slow link kept through a long
//! replay, the daemon's log saying why a client let a connection go, and a machine reached only
//! through its tunnel.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use herder_adapters::fake::FakeAdapter;
use herder_client_core::{
    Client, ConnectionState, Error, FleetLink, FleetPerson, LoginUri, Machine, NewAccount,
    SessionSubscription, SessionUpdate, TerminalEvent, TerminalStream,
};
use herder_daemon::auth::Auth;
use herder_daemon::auth::authenticator::{self, Authenticator};
use herder_daemon::aws::Aws;
use herder_daemon::login::{LoginProgram, LoginStatus, Logins};
use herder_daemon::session::{AccountConfig, Accounts, Adapters, SessionManager, Setup};
use herder_daemon::settings::Settings;
use herder_daemon::terminal::Terminals;
use herder_daemon::worktree::Worktrees;
use herder_daemon::ws::{Host, Server, Tls};
use herder_daemon::{Fanout, session};
use herder_protocol::{
    AccountId, AwsDeviceCode, AwsProfile, Bytes, CommandBody, CommandResult, Constraint, Container,
    ContainerState, ErrorCode, Event, EventBody, FailoverSettings, HostId, HostResources, Image,
    ItemBody, MAX_IMAGE_BYTES, NewPerson, PasskeyAssertion, PasskeyChallenge, PermissionMode,
    PersonPasskey, Provider, Role, SessionId, SessionStatus, SessionUsage, SignIn, SignInRefusal,
    TerminalPurpose, TurnId,
};
use herder_store::Store;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
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
    /// Where it takes tunneled clients, as `cloudflared` delivers them.
    tunnel: SocketAddr,
    fingerprint: String,
    auth: Arc<Auth>,
    fanout: Arc<Fanout>,
}

impl Daemon {
    /// Starts a daemon on `dir`, listening on `port` (0 for any), whose sessions run `script`;
    /// `turns` numbers turn ids across restarts, as the scripts expect.
    async fn start(dir: &Path, port: u16, script: &str, turns: Arc<AtomicU64>) -> Self {
        Self::start_as("host-1", dir, port, script, turns).await
    }

    /// [`Daemon::start`] for the host `host_id`.
    async fn start_as(
        host_id: &str,
        dir: &Path,
        port: u16,
        script: &str,
        turns: Arc<AtomicU64>,
    ) -> Self {
        let host_id = HostId::new(host_id);
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
            let fanout = Arc::new(Fanout::default());
            fanout.set_failover(failover());
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
                    fallback: false,
                    endpoint: None,
                },
            );
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
            let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
            let addr = listener.local_addr().unwrap();
            let tunnel = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let tunnel_addr = tunnel.local_addr().unwrap();
            let fingerprint = tls.fingerprint().to_owned();
            let host = Host {
                name: format!("test-{host_id}"),
                id: host_id,
            };
            let terminals = Terminals::new(Arc::clone(&fanout), PathBuf::from("/bin/sh"));
            // The login is fake; the account it adds is a real provider's, as the config holds,
            // and the one it logs in again the fake one.
            let logins = Logins::new(
                HashMap::from([
                    (Provider::Codex, fake_login()),
                    (fake_provider(), fake_login()),
                ]),
                dir.join("daemon.toml"),
                sessions.clone(),
            );
            let server = Server::new(
                tls,
                Arc::clone(&auth),
                Arc::clone(&fanout),
                sessions,
                terminals,
                logins,
                host,
            );
            // The AWS CLI and its config, both absent until a test writes them.
            let aws = dir.join("aws");
            let env = move |key: &str| match key {
                "PATH" => Some(aws.join("bin").into_os_string()),
                "AWS_CONFIG_FILE" => Some(aws.join("config").into_os_string()),
                _ => None,
            };
            server.manage_aws(Aws::from_env(env)).unwrap();
            let config = herder_daemon::Config::load_file(&dir.join("daemon.toml")).unwrap();
            let settings = Settings::new(&config, None, shutdown.clone());
            server.manage_settings(Arc::new(settings)).unwrap();
            started
                .send((addr, tunnel_addr, fingerprint, auth, fanout))
                .ok()
                .unwrap();
            server
                .run_tunneled(vec![listener], Some(tunnel), shutdown)
                .await;
        });
        let (addr, tunnel, fingerprint, auth, fanout) = ready.await.unwrap();
        Self {
            runtime: Some(runtime),
            addr,
            tunnel,
            fingerprint,
            auth,
            fanout,
        }
    }

    /// Signs `client` in as the person `name` by `addr`, joining as a new person the first time.
    async fn sign_in(
        &self,
        client: &Client,
        name: &str,
        addr: SocketAddr,
    ) -> Result<Machine, String> {
        let hosts = vec![addr.to_string()];
        authenticator::sign_in(&self.auth, client, hosts, &self.fingerprint, name)
            .await
            .map_err(|err| err.to_string())
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

/// `len` characters of text that compression barely shrinks: the daemon compresses what it
/// sends, and a test that needs a slow transfer must not have it undone.
fn noise_text(len: usize) -> String {
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 .";
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            char::from(LETTERS[usize::from(state.to_le_bytes()[0]) % LETTERS.len()])
        })
        .collect()
}

fn connected(state: &ConnectionState) -> bool {
    *state == ConnectionState::Connected
}

fn turn_ended(turn: &str) -> impl Fn(&View) -> bool {
    let turn = TurnId::new(turn);
    move |view| {
        view.has(|body| match body {
            EventBody::TurnCompleted { turn_id, .. } | EventBody::TurnFailed { turn_id, .. } => {
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
    let machine = daemon.sign_in(&client, "alice", daemon.addr).await.unwrap();
    let host = machine.host_id;
    assert_eq!(host, HostId::new("host-1"));
    assert_eq!(machine.name, "test-host-1");
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
                failover_pin: None,
                chat: false,
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
        files: Vec::new(),
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
                        retry_at: None,
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
async fn a_machine_reached_only_through_its_tunnel_streams_its_sessions() {
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
    let tunnel = format!("ws://{}", daemon.tunnel);
    let client = Client::open(config.display().to_string(), "herder-test/0".into()).unwrap();

    // The daemon's certificate is pinned through the tunnel: another one is refused.
    let wrong = "00".repeat(32);
    let refused =
        authenticator::sign_in(&daemon.auth, &client, vec![tunnel.clone()], &wrong, "alice")
            .await
            .expect_err("a tunnel to an unpinned certificate signed in");
    assert!(
        refused.to_string().contains(&daemon.fingerprint),
        "{refused}"
    );

    let machine = authenticator::sign_in(
        &daemon.auth,
        &client,
        vec![tunnel.clone()],
        &daemon.fingerprint,
        "alice",
    )
    .await
    .unwrap();
    assert_eq!(machine.addresses, std::slice::from_ref(&tunnel));
    let host = machine.host_id;
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
                failover_pin: None,
                chat: false,
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
    tokio::time::timeout(TIMEOUT, client.synced(host.clone()))
        .await
        .unwrap()
        .unwrap();
    let machine = client.machines().remove(0);
    assert_eq!(machine.connection, ConnectionState::Connected);
    let update = sub.next().await.unwrap();
    assert!(matches!(
        update.events.first().map(|event| &event.body),
        Some(EventBody::SessionCreated { .. })
    ));
}

/// The owner sets the machine's public address once, as a port forward on the router; another
/// device, signed in at home, picks it up from its next hello and connects through it once home
/// is out of reach, with nothing typed. Removed on the daemon, it leaves the device too.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_public_address_set_on_the_daemon_reaches_every_device() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    let owner = Client::open(
        tmp.path().join("owner").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    let host = daemon
        .sign_in(&owner, "alice", daemon.addr)
        .await
        .unwrap()
        .host_id;
    // The device's way in at home, and the router's port forward.
    let home = Relay::start(daemon.addr).await;
    let forward = Relay::start(daemon.addr).await;
    // 127.0.0.1 as a public address would be written: one not read as this device itself.
    let public = format!("127.1:{}", forward.addr.port());
    let phone = Client::open(
        tmp.path().join("phone").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    let at_home = home.addr.to_string();
    daemon.sign_in(&phone, "bob", home.addr).await.unwrap();
    phone.synced(host.clone()).await.unwrap();
    assert_eq!(
        phone.machines()[0].addresses,
        std::slice::from_ref(&at_home)
    );

    let set_public = |addresses: Vec<String>| {
        let (owner, host) = (&owner, host.clone());
        async move {
            let CommandResult::Settings { mut settings, .. } = owner
                .send(host.clone(), CommandBody::GetSettings)
                .await
                .unwrap()
            else {
                panic!("expected the settings");
            };
            settings.public_addresses = addresses;
            owner
                .send(host, CommandBody::SetSettings { settings })
                .await
                .unwrap();
        }
    };
    set_public(vec![public.clone()]).await;
    assert_eq!(phone.reconnect(host.clone()).await, Ok(at_home.clone()));
    wait_machines(&phone, |m| {
        m[0].addresses == [at_home.clone(), public.clone()]
    })
    .await;

    // Away from home: the home route takes connections and never answers.
    home.swallow(true);
    home.cut();
    wait_machines(&phone, |m| {
        m[0].connection == ConnectionState::Connected && m[0].address.as_ref() == Some(&public)
    })
    .await;
    phone.synced(host.clone()).await.unwrap();

    // Home again; the owner removes the port forward, and the device drops it.
    home.swallow(false);
    assert_eq!(phone.reconnect(host.clone()).await, Ok(at_home.clone()));
    set_public(Vec::new()).await;
    assert_eq!(phone.reconnect(host.clone()).await, Ok(at_home.clone()));
    wait_machines(&phone, |m| m[0].addresses == [at_home.clone()]).await;
}

/// A proxy in front of the daemon's tunnel listener that lets an upgrade through only with
/// `headers`, as Cloudflare Access does with a service token, and passes on the request without
/// them; anything else gets 403.
struct HeaderGate {
    addr: SocketAddr,
    /// Whether a request it passed on still carried one of the headers.
    leaked: Arc<AtomicBool>,
}

impl HeaderGate {
    async fn start(tunnel: SocketAddr, headers: &[(&str, &str)]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let leaked = Arc::new(AtomicBool::new(false));
        let wanted: Vec<String> = headers
            .iter()
            .map(|(name, value)| format!("{}: {value}", name.to_ascii_lowercase()))
            .collect();
        let leaks = Arc::clone(&leaked);
        tokio::spawn(async move {
            while let Ok((mut inbound, _)) = listener.accept().await {
                let (wanted, leaks) = (wanted.clone(), Arc::clone(&leaks));
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    while !head.ends_with(b"\r\n\r\n") {
                        let mut byte = [0];
                        if inbound.read_exact(&mut byte).await.is_err() {
                            return;
                        }
                        head.push(byte[0]);
                    }
                    let head = String::from_utf8_lossy(&head).into_owned();
                    let lines: Vec<String> = head
                        .split("\r\n")
                        .map(|line| match line.split_once(':') {
                            Some((name, value)) => {
                                format!("{}: {}", name.to_ascii_lowercase(), value.trim())
                            }
                            None => line.to_owned(),
                        })
                        .collect();
                    if !wanted.iter().all(|header| lines.contains(header)) {
                        let refusal = "HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n";
                        let _ = inbound.write_all(refusal.as_bytes()).await;
                        return;
                    }
                    let passed: Vec<&String> =
                        lines.iter().filter(|line| !wanted.contains(line)).collect();
                    let names: Vec<&str> =
                        wanted.iter().filter_map(|h| h.split(':').next()).collect();
                    if passed
                        .iter()
                        .any(|line| names.iter().any(|name| line.starts_with(name)))
                    {
                        leaks.store(true, Ordering::SeqCst);
                    }
                    let passed = passed.iter().map(|line| line.as_str()).collect::<Vec<_>>();
                    let mut outbound = TcpStream::connect(tunnel).await.unwrap();
                    outbound
                        .write_all(passed.join("\r\n").as_bytes())
                        .await
                        .unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                });
            }
        });
        Self { addr, leaked }
    }
}

/// A machine whose tunnel sits behind a proxy that wants headers: without them, or with
/// wrong ones, the machine says the proxy refused and what to check; with them it connects
/// and streams, and they go to the proxy only.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tunnel_behind_a_proxy_that_wants_headers_gets_them_from_this_device_only() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    let host = daemon
        .sign_in(&client, "alice", daemon.addr)
        .await
        .unwrap()
        .host_id;
    let token = [
        ("CF-Access-Client-Id", "abc.access"),
        ("CF-Access-Client-Secret", "s3cret"),
    ];
    let gate = HeaderGate::start(daemon.tunnel, &token).await;
    let url = format!("ws://{}", gate.addr);
    client
        .set_addresses(host.clone(), vec![url.clone()])
        .unwrap();
    let unreachable = |result: Result<String, Error>| match result {
        Err(Error::Unreachable { message }) => message,
        other => panic!("{other:?}"),
    };

    let message = unreachable(client.reconnect(host.clone()).await);
    assert!(message.contains("403 Forbidden"), "{message}");
    assert!(message.contains("set them for this address"), "{message}");

    let headers = |secret: &str| {
        HashMap::from([
            (token[0].0.to_owned(), token[0].1.to_owned()),
            (token[1].0.to_owned(), secret.to_owned()),
        ])
    };
    client
        .set_address_headers(host.clone(), url.clone(), headers("wrong"))
        .unwrap();
    let message = unreachable(client.reconnect(host.clone()).await);
    assert!(
        message.contains(
            "refused the headers set for this address (cf-access-client-id, \
             cf-access-client-secret)"
        ),
        "{message}"
    );
    assert!(
        !message.contains("wrong"),
        "a header's value shows: {message}"
    );

    client
        .set_address_headers(host.clone(), url.clone(), headers("s3cret"))
        .unwrap();
    assert_eq!(client.reconnect(host.clone()).await, Ok(url.clone()));
    client.synced(host.clone()).await.unwrap();
    let machine = client.machines().remove(0);
    assert_eq!(
        machine.address_headers,
        HashMap::from([(
            url.clone(),
            vec![
                "cf-access-client-id".to_owned(),
                "cf-access-client-secret".to_owned()
            ]
        )])
    );
    assert!(!gate.leaked.load(Ordering::SeqCst));

    // Not shared: a link to the machine names its addresses only.
    let shared = std::fs::read_to_string(tmp.path().join("client/machines.json")).unwrap();
    assert!(shared.contains("s3cret"), "the profile keeps them");
    let link = FleetLink {
        person: FleetPerson {
            name: "alice".into(),
            passkeys: Vec::new(),
        },
        machines: vec![herder_client_core::FleetMachine {
            hosts: machine.addresses.clone(),
            fingerprint: machine.fingerprint.clone(),
            name: None,
        }],
    };
    assert!(!link.to_string().contains("s3cret"));

    // Refused: what is not a header, one the upgrade sets itself, and an address that is
    // not one of its tunnels.
    for (address, name) in [
        (url.as_str(), "Sec-WebSocket-Key"),
        (url.as_str(), "not a name"),
        ("ws://elsewhere.example.com", "X-Token"),
    ] {
        let one = HashMap::from([(name.to_owned(), "x".to_owned())]);
        let refused = client.set_address_headers(host.clone(), address.to_owned(), one);
        assert!(
            matches!(refused, Err(Error::Local { .. })),
            "{name}: {refused:?}"
        );
    }
    // Removing the address drops its headers.
    client
        .set_addresses(host.clone(), vec![daemon.addr.to_string()])
        .unwrap();
    assert!(client.machines()[0].address_headers.is_empty());
    let saved = std::fs::read_to_string(tmp.path().join("client/machines.json")).unwrap();
    assert!(!saved.contains("s3cret"), "{saved}");
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
    let host = daemon
        .sign_in(&client, "alice", daemon.addr)
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
                failover_pin: None,
                chat: false,
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
    let host = daemon
        .sign_in(&client, "alice", daemon.addr)
        .await
        .unwrap()
        .host_id;
    client.rename(host.clone(), "build box".into()).unwrap();
    assert_eq!(client.machines()[0].name, "build box");
    // Pairing again keeps the name.
    daemon.sign_in(&client, "alice", daemon.addr).await.unwrap();
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

/// Waits until `client` holds machines that satisfy `wanted`.
async fn wait_machines(client: &Client, wanted: impl Fn(&[herder_client_core::Machine]) -> bool) {
    let changes = client.changes();
    tokio::time::timeout(TIMEOUT, async {
        while !wanted(&client.machines()) {
            assert!(changes.next().await);
        }
    })
    .await
    .expect("the machines never got there");
}

/// Two clients on one config dir, as the Mac app and the TUI on one Mac: what either changes
/// shows up in the other without a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clients_sharing_a_profile_see_each_others_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("client").display().to_string();
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    let app = Client::open(config.clone(), "herder-app/0".into()).unwrap();
    let tui = Client::open(config.clone(), "herder-tui/0".into()).unwrap();

    let host = daemon
        .sign_in(&app, "alice", daemon.addr)
        .await
        .unwrap()
        .host_id;
    wait_machines(&tui, |m| m.len() == 1 && m[0].host_id == host).await;
    wait_connection(&tui, connected).await;

    tui.rename(host.clone(), "build box".into()).unwrap();
    wait_machines(&app, |m| m[0].name == "build box").await;

    let addresses = vec![daemon.addr.to_string(), "127.0.0.1:1".to_owned()];
    app.set_addresses(host.clone(), addresses.clone()).unwrap();
    wait_machines(&tui, |m| m[0].addresses == addresses).await;
    assert_eq!(tui.machines()[0].name, "build box");

    // Pairing again with a new key replaces the other client's connection too.
    daemon.sign_in(&tui, "alice", daemon.addr).await.unwrap();
    wait_connection(&app, connected).await;

    app.forget(host.clone()).unwrap();
    wait_machines(&tui, <[_]>::is_empty).await;
    assert_eq!(
        tui.rename(host.clone(), "x".into()),
        Err(Error::UnknownMachine { host_id: host })
    );
}

/// Two clients pairing at once on one config dir: neither overwrites the other's machine.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clients_sharing_a_profile_lose_no_pairing() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("client").display().to_string();
    let one = Daemon::start_as(
        "host-1",
        &tmp.path().join("one"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    let two = Daemon::start_as(
        "host-2",
        &tmp.path().join("two"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    let app = Client::open(config.clone(), "herder-app/0".into()).unwrap();
    let tui = Client::open(config.clone(), "herder-tui/0".into()).unwrap();

    let (paired_one, paired_two) = tokio::join!(
        one.sign_in(&app, "alice", one.addr),
        two.sign_in(&tui, "alice", two.addr)
    );
    paired_one.unwrap();
    paired_two.unwrap();
    let both = |m: &[herder_client_core::Machine]| {
        let mut ids: Vec<&str> = m.iter().map(|m| m.host_id.as_str()).collect();
        ids.sort_unstable();
        ids == ["host-1", "host-2"]
    };
    wait_machines(&app, both).await;
    wait_machines(&tui, both).await;
    drop((app, tui));

    let reopened = Client::open(config, "herder-tui/0".into()).unwrap();
    assert!(both(&reopened.machines()));
}

/// A dead address before a live one costs a head start, not a connect timeout; the order the
/// user sets holds, persists, and moves a connection that is up to an address that comes first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn addresses_race_in_order_and_the_first_that_answers_wins() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("client");
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    // Accepts TCP connections in its backlog and never answers them, as a route that is down
    // behind a VPN often does.
    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_addr = dead.local_addr().unwrap().to_string();
    let live = daemon.addr.to_string();
    let client = Client::open(config.display().to_string(), "herder-test/0".into()).unwrap();
    let started = std::time::Instant::now();
    let hosts = vec![dead_addr.clone(), live.clone()];
    let host = authenticator::sign_in(&daemon.auth, &client, hosts, &daemon.fingerprint, "alice")
        .await
        .unwrap()
        .host_id;
    client.synced(host.clone()).await.unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    let machine = &client.machines()[0];
    assert_eq!(machine.addresses, [dead_addr.clone(), live.clone()]);
    assert_eq!(machine.address.as_deref(), Some(live.as_str()));

    // A new first address that answers takes over the connection.
    let relay = Relay::start(daemon.addr).await;
    let via_relay = relay.addr.to_string();
    client
        .set_addresses(host.clone(), vec![via_relay.clone(), live.clone()])
        .unwrap();
    wait_machine(&client, |machine| {
        machine.address.as_deref() == Some(via_relay.as_str())
    })
    .await;
    client.synced(host.clone()).await.unwrap();

    // Dropping the address in use moves the connection to one still listed.
    client
        .set_addresses(host.clone(), vec![live.clone()])
        .unwrap();
    wait_machine(&client, |machine| {
        machine.address.as_deref() == Some(live.as_str())
    })
    .await;
    assert_eq!(
        client.set_addresses(host.clone(), Vec::new()),
        Err(Error::Local {
            message: "a machine needs at least one address".into()
        })
    );
    assert!(
        client
            .set_addresses(host.clone(), vec!["box:x".into()])
            .is_err()
    );
    // A typed address gets the default port.
    client
        .set_addresses(host.clone(), vec![live.clone(), "box.local".into()])
        .unwrap();
    drop(client);

    let client = Client::open(config.display().to_string(), "herder-test/0".into()).unwrap();
    assert_eq!(
        client.machines()[0].addresses,
        [live, "box.local:7447".to_owned()]
    );
    drop(dead);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconnect_dials_the_first_address_and_falls_back_when_it_is_dead() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    let host = daemon
        .sign_in(&client, "alice", daemon.addr)
        .await
        .unwrap()
        .host_id;
    client.synced(host.clone()).await.unwrap();
    let live = daemon.addr.to_string();

    // The connection in use is replaced, even on the same address.
    assert_eq!(client.reconnect(host.clone()).await, Ok(live.clone()));
    assert_eq!(client.machines()[0].quality.reconnects, 1);

    // An address put first is dialled first and takes over the connection.
    let relay = Relay::start(daemon.addr).await;
    let via_relay = relay.addr.to_string();
    client
        .set_addresses(host.clone(), vec![via_relay.clone(), live.clone()])
        .unwrap();
    assert_eq!(client.reconnect(host.clone()).await, Ok(via_relay.clone()));
    assert_eq!(
        client.machines()[0].address.as_deref(),
        Some(via_relay.as_str())
    );
    client.synced(host.clone()).await.unwrap();

    // A dead first address falls back to the next.
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = closed.local_addr().unwrap().to_string();
    drop(closed);
    client
        .set_addresses(host.clone(), vec![dead.clone(), live.clone()])
        .unwrap();
    assert_eq!(client.reconnect(host.clone()).await, Ok(live.clone()));
    client.synced(host.clone()).await.unwrap();

    // With no address answering, it says why, and the machine is disconnected.
    client
        .set_addresses(host.clone(), vec![dead.clone()])
        .unwrap();
    match client.reconnect(host.clone()).await {
        Err(Error::Unreachable { message }) => assert!(message.contains(&dead), "{message}"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        client.machines()[0].connection,
        ConnectionState::Disconnected { .. }
    ));
    assert_eq!(
        client.reconnect(HostId::new("nope")).await,
        Err(Error::UnknownMachine {
            host_id: HostId::new("nope")
        })
    );
}

/// A connection that works is kept: a wake, as every network change sends one, does not move
/// it to an address listed before its own, though that one answers again. Lost, it comes back
/// through the address that answered last, and a reconnect the user asks for starts from the
/// order of the addresses again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_address_that_answered_last_is_kept_and_tried_first() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    // The first address answers nothing yet, as a home network's does from elsewhere.
    let away = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let first = away.local_addr().unwrap().to_string();
    let second = Relay::start(daemon.addr).await;
    let via_second = second.addr.to_string();
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    let hosts = vec![first.clone(), via_second.clone()];
    let host = authenticator::sign_in(&daemon.auth, &client, hosts, &daemon.fingerprint, "alice")
        .await
        .unwrap()
        .host_id;
    client.synced(host.clone()).await.unwrap();
    let address = || client.machines()[0].address.clone();
    assert_eq!(address(), Some(via_second.clone()));

    // The first address answers from now on.
    let _home = Relay::relaying(away, daemon.addr, None);
    client.wake();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(address(), Some(via_second.clone()));
    assert_eq!(client.machines()[0].quality.reconnects, 0);

    second.cut();
    wait_machine(&client, |m| {
        m.quality.reconnects == 1 && connected(&m.connection)
    })
    .await;
    assert_eq!(address(), Some(via_second.clone()));

    assert_eq!(client.reconnect(host.clone()).await, Ok(first));
    client.synced(host).await.unwrap();
}

/// A network change while the machine is connecting: the attempt under way is stuck on the
/// network there was before, and the wake the change sends races the addresses again at
/// once instead of after that attempt's timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wake_while_connecting_does_not_wait_for_a_stuck_attempt() {
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
    let host = daemon
        .sign_in(&client, "alice", relay.addr)
        .await
        .unwrap()
        .host_id;
    client.synced(host.clone()).await.unwrap();

    // The network goes: the connection drops, and the next attempt is never answered.
    let before = relay.accepted();
    relay.swallow(true);
    relay.cut();
    tokio::time::timeout(TIMEOUT, async {
        while relay.accepted() == before {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the client did not try to reconnect");
    // A drop this short does not show.
    assert_eq!(client.machines()[0].connection, ConnectionState::Connected);
    assert_eq!(client.machines()[0].quality.reconnects, 0);

    // Another network is there, and the app wakes the client as it sees the change.
    relay.swallow(false);
    let woken = std::time::Instant::now();
    client.wake();
    wait_machine(&client, |m| {
        connected(&m.connection) && m.quality.reconnects == 1
    })
    .await;
    // Far sooner than the 10 s the stuck attempt has before it gives up.
    assert!(
        woken.elapsed() < Duration::from_secs(5),
        "{:?}",
        woken.elapsed()
    );
    client.synced(host).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signing_in_fails_on_a_wrong_fingerprint_and_saves_nothing() {
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

    let hosts = vec![daemon.addr.to_string()];
    let err = client
        .sign_in(hosts.clone(), "00".repeat(32), Vec::new())
        .await
        .err()
        .unwrap();
    assert!(err.to_string().contains("fingerprint"), "{err}");
    assert!(client.machines().is_empty());
    assert!(!config.join("machines.json").exists());

    // The real fingerprint works, and a command the daemon refuses comes back as rejected.
    let host = daemon
        .sign_in(&client, "alice", daemon.addr)
        .await
        .unwrap()
        .host_id;
    let refused = client
        .send(
            host.clone(),
            CommandBody::SendPrompt {
                session_id: SessionId::new("nope"),
                text: "Hi.".into(),
                images: Vec::new(),
                files: Vec::new(),
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
    swallowing: Arc<AtomicBool>,
    accepted: Arc<AtomicU64>,
}

impl Relay {
    async fn start(target: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        Self::relaying(listener, target, None)
    }

    /// A relay that passes on what a client sends at no more than `rate` bytes a second, as
    /// a slow uplink would.
    fn slow(target: SocketAddr, rate: u64) -> Self {
        let socket = TcpSocket::new_v4().unwrap();
        // What is not passed on yet waits in the client's socket, as on a slow uplink, not
        // in the relay's.
        socket.set_recv_buffer_size(64 * 1024).unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        Self::relaying(socket.listen(64).unwrap(), target, Some(rate))
    }

    fn relaying(listener: TcpListener, target: SocketAddr, rate: Option<u64>) -> Self {
        let addr = listener.local_addr().unwrap();
        let cut = tokio::sync::watch::Sender::new(0);
        let stall = tokio::sync::watch::Sender::new(0);
        let swallowing = Arc::new(AtomicBool::new(false));
        let accepted = Arc::new(AtomicU64::new(0));
        let (cuts, stalls, count) = (cut.clone(), stall.clone(), Arc::clone(&accepted));
        let swallows = Arc::clone(&swallowing);
        tokio::spawn(async move {
            while let Ok((mut inbound, _)) = listener.accept().await {
                count.fetch_add(1, Ordering::SeqCst);
                let mut cut = cuts.subscribe();
                let mut stall = stalls.subscribe();
                let swallowed = swallows.load(Ordering::SeqCst);
                tokio::spawn(async move {
                    let relayed = async {
                        if swallowed {
                            std::future::pending::<()>().await;
                        }
                        let mut outbound = TcpStream::connect(target).await.unwrap();
                        let Some(rate) = rate else {
                            let _ =
                                tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                            return;
                        };
                        let (mut from_client, mut to_client) = inbound.split();
                        let (mut from_daemon, mut to_daemon) = outbound.split();
                        tokio::select! {
                            () = trickle(&mut from_client, &mut to_daemon, rate) => {}
                            _ = tokio::io::copy(&mut from_daemon, &mut to_client) => {}
                        }
                    };
                    tokio::select! {
                        () = relayed => {}
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
            swallowing,
            accepted,
        }
    }

    /// Whether connections made from now on are taken and never answered, as on a network
    /// that swallows what is sent.
    fn swallow(&self, swallowing: bool) {
        self.swallowing.store(swallowing, Ordering::SeqCst);
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

/// Copies `from` to `to` at no more than `rate` bytes a second.
async fn trickle(
    from: &mut (impl AsyncRead + Unpin),
    to: &mut (impl AsyncWrite + Unpin),
    rate: u64,
) {
    let mut chunk = vec![0; 64 * 1024];
    loop {
        let read = match from.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        if to.write_all(&chunk[..read]).await.is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_micros(read as u64 * 1_000_000 / rate)).await;
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
    let face_id = Arc::new(FaceId::default());
    let client = Client::open_with_passkey(
        tmp.path().join("client").display().to_string(),
        "herder-test/0".into(),
        face_id.clone(),
    )
    .unwrap();
    let host = daemon
        .sign_in(&client, "alice", relay.addr)
        .await
        .unwrap()
        .host_id;
    face_id.hold(&daemon.auth, "alice");
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
                failover_pin: None,
                chat: false,
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
    let other = Client::open_with_passkey(
        tmp.path().join("other").display().to_string(),
        "herder-test/0".into(),
        face_id.clone(),
    )
    .unwrap();
    daemon.sign_in(&other, "alice", daemon.addr).await.unwrap();
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
    daemon.sign_in(&member, "bob", daemon.addr).await.unwrap();
    wait_connection(&member, connected).await;
    assert_eq!(member.machines()[0].role, Some(Role::Member));
    let bob = member.machines()[0].person_id.clone();
    assert!(bob.is_some(), "the machine names the person the member is");
    assert_ne!(bob, other.machines()[0].person_id, "a person of their own");
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
    let face_id = Arc::new(FaceId::default());
    let client = Client::open_with_passkey(
        tmp.path().join("client").display().to_string(),
        "herder-test/0".into(),
        face_id.clone(),
    )
    .unwrap();
    let host = daemon
        .sign_in(&client, "alice", daemon.addr)
        .await
        .unwrap()
        .host_id;
    face_id.hold(&daemon.auth, "alice");
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
    daemon.sign_in(&member, "bob", daemon.addr).await.unwrap();
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

    // A cancelled login adds nothing, and only an owner may cancel it.
    let stream = client
        .add_account(host.clone(), new_account("codex-2"), 80, 24)
        .await
        .unwrap();
    let mut screen = Screen::default();
    screen.read_until(&stream, "Code: ").await;
    assert!(config_dir.exists());
    let refused = member
        .close_terminal(host.clone(), stream.terminal_id())
        .await
        .err();
    assert!(
        matches!(&refused, Some(Error::Rejected { info }) if info.code == ErrorCode::Forbidden),
        "{refused:?}"
    );
    client
        .close_terminal(host.clone(), stream.terminal_id())
        .await
        .unwrap();
    assert_eq!(login_exit(&stream, &mut screen).await, None);
    assert!(
        screen.text.contains("account codex-2 was not added"),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn logging_an_account_in_again_relays_its_login_in_its_own_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon_dir = tmp.path().join("daemon");
    let daemon = Daemon::start(&daemon_dir, 0, "mid_turn.jsonl", Arc::default()).await;
    // The account's config dir, holding a login that expired.
    let account_dir = daemon_dir.join("account");
    std::fs::create_dir_all(&account_dir).unwrap();
    let face_id = Arc::new(FaceId::default());
    let client = Client::open_with_passkey(
        tmp.path().join("client").display().to_string(),
        "herder-test/0".into(),
        face_id.clone(),
    )
    .unwrap();
    let host = daemon
        .sign_in(&client, "alice", daemon.addr)
        .await
        .unwrap()
        .host_id;
    face_id.hold(&daemon.auth, "alice");

    // Members never see a login.
    let member = Client::open(
        tmp.path().join("member").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    daemon.sign_in(&member, "bob", daemon.addr).await.unwrap();
    let refused = member
        .log_in_account(host.clone(), account(), 80, 24)
        .await
        .err();
    assert!(
        matches!(&refused, Some(Error::Rejected { info }) if info.code == ErrorCode::Forbidden),
        "{refused:?}"
    );
    // Nor is an account that does not exist.
    let unknown = client
        .log_in_account(host.clone(), AccountId::new("nobody"), 80, 24)
        .await
        .err();
    assert!(
        matches!(&unknown, Some(Error::Rejected { info }) if info.code == ErrorCode::BadRequest),
        "{unknown:?}"
    );

    let stream = client
        .log_in_account(host.clone(), account(), 80, 24)
        .await
        .unwrap();
    let mut screen = Screen::default();
    screen.read_until(&stream, "Code: ").await;
    stream.input(b"ABCD\n".to_vec());
    assert_eq!(login_exit(&stream, &mut screen).await, Some(0));
    assert!(
        screen
            .text
            .contains("herder: logged account account-1 in again"),
        "{}",
        screen.text
    );
    // The login ran in the account's own dir, and the config is untouched.
    assert!(account_dir.join("logged-in").exists());
    assert!(!daemon_dir.join("daemon.toml").exists());
    assert_eq!(client.machines()[0].accounts.len(), 1);
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
    daemon.fanout.host_resources(host_resources(3));
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    daemon.sign_in(&client, "alice", daemon.addr).await.unwrap();
    wait_machine(&client, |m| m.resources == Some(host_resources(3))).await;

    // Later figures replace them.
    daemon.fanout.host_resources(host_resources(4));
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
    daemon.fanout.session_resources(&session, usage.clone());
    wait_machine(&client, |m| m.session_usage.get(&session) == Some(&usage)).await;
    // A session whose processes and containers are gone leaves the map.
    let idle = SessionUsage {
        cpu_percent: 0.0,
        memory_bytes: 0,
        processes: 0,
        containers: Vec::new(),
    };
    daemon.fanout.session_resources(&session, idle);
    wait_machine(&client, |m| m.session_usage.is_empty()).await;

    // Live figures go with the connection.
    daemon.fanout.session_resources(&session, usage);
    wait_machine(&client, |m| !m.session_usage.is_empty()).await;
    daemon.kill().await;
    wait_machine(&client, |m| {
        !connected(&m.connection) && m.resources.is_none() && m.session_usage.is_empty()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn skills_stay_current_while_connected() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    let status = |head: &str| herder_protocol::SkillsStatus {
        repo: Some("https://github.com/you/herder-skills".into()),
        head: Some(head.into()),
        last_pull: None,
        pull_error: None,
        skills: Vec::new(),
        reload: Vec::new(),
        accounts: Vec::new(),
    };
    // A status from before the client connects reaches it after hello.
    daemon.fanout.skills_status(status("a1"));
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    daemon.sign_in(&client, "alice", daemon.addr).await.unwrap();
    wait_machine(&client, |m| m.skills == Some(status("a1"))).await;
    daemon.fanout.skills_status(status("b2"));
    wait_machine(&client, |m| m.skills == Some(status("b2"))).await;

    let session = SessionId::new("s1");
    let skills = vec![herder_protocol::SessionSkill {
        name: "deploy".into(),
        description: "Deploys the app.".into(),
        source: herder_protocol::SkillSource::Library,
        path: None,
    }];
    daemon.fanout.session_skills(&session, skills.clone());
    wait_machine(&client, |m| m.session_skills.get(&session) == Some(&skills)).await;
    // A session with no skills leaves the map.
    daemon.fanout.session_skills(&session, Vec::new());
    wait_machine(&client, |m| m.session_skills.is_empty()).await;

    // Skills go with the connection.
    daemon.fanout.session_skills(&session, skills);
    wait_machine(&client, |m| !m.session_skills.is_empty()).await;
    daemon.kill().await;
    wait_machine(&client, |m| {
        !connected(&m.connection) && m.skills.is_none() && m.session_skills.is_empty()
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
    daemon.sign_in(&client, "alice", daemon.addr).await.unwrap();
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

/// A prompt whose message the daemon would not read fails at once on this device: it is not
/// sent, so it is not resent either, and the connection stays up for what comes next.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_prompt_too_large_to_send_fails_at_once_and_keeps_the_connection() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    daemon.sign_in(&client, "alice", daemon.addr).await.unwrap();
    wait_machine(&client, |m| m.quality.last_rtt_ms.is_some()).await;
    let host = client.machines()[0].host_id.clone();
    let prompt = |images: usize| CommandBody::SendPrompt {
        session_id: SessionId::new("nope"),
        text: "look".into(),
        images: (0..images)
            .map(|_| Image {
                media_type: "image/png".into(),
                data: Bytes(vec![0; MAX_IMAGE_BYTES]),
            })
            .collect(),
        files: Vec::new(),
    };

    // Five images, each as large as one may be: over a message's limit as base64.
    let sent = client.send(host.clone(), prompt(5)).await;
    let Err(Error::Local { message }) = sent else {
        panic!("expected the prompt refused on this device, got {sent:?}");
    };
    assert!(message.starts_with("too large to send: "), "{message}");

    // The daemon answers the next command, on the connection that was up all along.
    let sent = client.send(host, prompt(0)).await;
    assert!(
        matches!(&sent, Err(Error::Rejected { info }) if info.code == ErrorCode::NotFound),
        "{sent:?}"
    );
    let machine = &client.machines()[0];
    assert!(connected(&machine.connection));
    assert_eq!(machine.quality.reconnects, 0);
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
                failover_pin: None,
                chat: false,
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
    let host = daemon
        .sign_in(&phone, "alice", relay.addr)
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
        files: Vec::new(),
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
    daemon
        .sign_in(&desktop, "alice", daemon.addr)
        .await
        .unwrap();
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

/// In the background what the app asks for does not probe its connection, and one that dies
/// there is not replaced until a wake, however long the suspension: on a phone that would only
/// spend battery and data. The wake then replaces it at once.
#[tokio::test]
async fn a_suspended_client_replaces_no_connection_until_woken() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    let relay = Relay::start(daemon.addr).await;
    let phone = Client::open(
        tmp.path().join("phone").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    let host = daemon
        .sign_in(&phone, "alice", relay.addr)
        .await
        .unwrap()
        .host_id;
    phone.synced(host.clone()).await.unwrap();
    // One connection paired, the other is the supervisor's.
    assert_eq!(relay.accepted(), 2);

    phone.suspend();
    relay.stall();
    // In the foreground, this would probe the connection and replace it 5 s later.
    let sub = phone
        .subscribe_session(host.clone(), SessionId::new("nope"))
        .unwrap();

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(10 * 60)).await;
    // The connection's pings find it dead, and nothing replaces it.
    wait_connection(&phone, |state| {
        matches!(state, ConnectionState::Disconnected { .. })
    })
    .await;
    tokio::time::sleep(Duration::from_secs(5 * 60)).await;
    assert_eq!(relay.accepted(), 2);
    tokio::time::resume();

    phone.wake();
    wait_connection(&phone, |state| *state == ConnectionState::Connected).await;
    phone.synced(host).await.unwrap();
    assert_eq!(relay.accepted(), 3);
    drop(sub);
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
    let host = daemon
        .sign_in(&client, "alice", relay.addr)
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

/// A connection that goes silently dead while the app is in use is noticed by its pings, with
/// no wake to prompt a check, and replaced.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dead_connection_is_noticed_by_its_pings_and_replaced() {
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
    let host = daemon
        .sign_in(&client, "alice", relay.addr)
        .await
        .unwrap()
        .host_id;
    client.synced(host.clone()).await.unwrap();
    assert_eq!(relay.accepted(), 2);

    relay.stall();
    // The next ping goes out within 10 s, and gets 5 s.
    let changes = client.changes();
    tokio::time::timeout(Duration::from_secs(25), async {
        while relay.accepted() < 3 || client.machines()[0].connection != ConnectionState::Connected
        {
            assert!(changes.next().await);
        }
    })
    .await
    .expect("the dead connection was not replaced in time");
    client.synced(host).await.unwrap();
}

/// A prompt that a slow uplink takes seconds to carry holds up nothing else: what the daemon
/// sends meanwhile is read as it comes, and a wake's probe, whose pong waits behind the
/// prompt, does not make the connection look dead while the daemon keeps taking bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_upload_holds_up_neither_reading_nor_a_wake() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    // 1.5 MB a second: the prompt below takes over nine seconds.
    let relay = Relay::slow(daemon.addr, 1_500_000);
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    let host = daemon
        .sign_in(&client, "alice", relay.addr)
        .await
        .unwrap()
        .host_id;
    client.synced(host.clone()).await.unwrap();
    assert_eq!(relay.accepted(), 2);

    // Two images, each as large as one may be: close to 14 MB as base64.
    let prompt = CommandBody::SendPrompt {
        session_id: SessionId::new("nope"),
        text: "look".into(),
        images: (0..2)
            .map(|_| Image {
                media_type: "image/png".into(),
                data: Bytes(vec![0; MAX_IMAGE_BYTES]),
            })
            .collect(),
        files: Vec::new(),
    };
    let started = std::time::Instant::now();
    let sending = client.send(host.clone(), prompt);
    tokio::pin!(sending);
    tokio::select! {
        sent = &mut sending => panic!("the prompt was through at once: {sent:?}"),
        () = tokio::time::sleep(Duration::from_millis(500)) => {}
    }

    // What the daemon sends now arrives while the prompt is still on its way.
    daemon.fanout.host_resources(host_resources(4));
    tokio::select! {
        sent = &mut sending => panic!("the prompt was through too soon to tell: {sent:?}"),
        () = wait_machine(&client, |m| m.resources == Some(host_resources(4))) => {}
    }
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );

    // The probe gets five seconds, and its pong comes only once the prompt is through.
    client.wake();
    let sent = tokio::time::timeout(Duration::from_secs(60), &mut sending)
        .await
        .expect("the prompt got no answer");
    assert!(matches!(sent, Err(Error::Rejected { .. })), "{sent:?}");
    assert!(
        started.elapsed() > Duration::from_secs(8),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(relay.accepted(), 2);
    let machine = &client.machines()[0];
    assert!(connected(&machine.connection));
    assert_eq!(machine.quality.reconnects, 0);
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
    let host = daemon
        .sign_in(&client, "alice", daemon.addr)
        .await
        .unwrap()
        .host_id;
    let session_id = create_session(&client, &host, repo).await;
    let sub = client
        .subscribe_session(host.clone(), session_id.clone())
        .unwrap();
    let prompt = |text: &str| CommandBody::SendPrompt {
        session_id: session_id.clone(),
        text: text.into(),
        images: Vec::new(),
        files: Vec::new(),
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
                        retry_at: None,
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
    // The first update waits for the cache and carries all of it.
    let sub = client.subscribe_session(host, session_id).unwrap();
    let reopened = sub.next().await.unwrap();
    assert_eq!(reopened.events, view.events);
    wait_machine(&client, |machine| machine.sessions == live).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shared_link_names_every_machine_for_another_device_to_sign_in_to() {
    let tmp = tempfile::tempdir().unwrap();
    let start = |host: &'static str| {
        let dir = tmp.path().join(host);
        async move { Daemon::start_as(host, &dir, 0, "mid_turn.jsonl", Arc::default()).await }
    };
    let (first, second, gone) = (
        start("host-1").await,
        start("host-2").await,
        start("host-3").await,
    );
    // On the second machine the sharer is a member: carol claimed it first.
    let carol = Client::open(
        tmp.path().join("carol").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    second.sign_in(&carol, "carol", second.addr).await.unwrap();

    let mac = Client::open(
        tmp.path().join("mac").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    for daemon in [&first, &second, &gone] {
        daemon.sign_in(&mac, "alice", daemon.addr).await.unwrap();
    }
    let gone_addr = gone.addr.to_string();
    gone.kill().await;
    // Every machine here is on loopback, which no other device reaches, so each gets an
    // address another device would use, after its loopback one.
    let readdress = |host: &str, addresses: &[String]| {
        mac.set_addresses(HostId::new(host), addresses.to_vec())
            .unwrap();
    };
    readdress("host-1", &[first.addr.to_string(), "192.0.2.1:7447".into()]);
    readdress(
        "host-2",
        &[
            "localhost:7447".into(),
            second.addr.to_string(),
            "[2001:db8::2]:7447".into(),
        ],
    );
    readdress("host-3", &["192.0.2.3:7447".into(), gone_addr.clone()]);

    // The name this device gives a machine is the one it shares.
    mac.rename(HostId::new("host-2"), "alice's hub".into())
        .unwrap();
    wait_host(&mac, "host-1", true).await;
    wait_host(&mac, "host-2", true).await;

    // Every saved machine, connected or not, by its name, its addresses but loopback ones,
    // and its certificate, with no code; and alice, by her name and her passkey, as the
    // machines list her.
    let link = mac.share().await.unwrap();
    let alice = Authenticator::of(&first.auth, "alice").unwrap();
    assert_eq!(
        link.person,
        FleetPerson {
            name: "alice".into(),
            passkeys: vec![alice.key().credential_id],
        }
    );
    let hosts: Vec<_> = link.machines.iter().map(|m| m.hosts.clone()).collect();
    assert_eq!(
        hosts,
        [
            ["192.0.2.1:7447"],
            ["[2001:db8::2]:7447"],
            ["192.0.2.3:7447"]
        ]
    );
    let names: Vec<_> = link.machines.iter().map(|m| m.name.as_deref()).collect();
    assert_eq!(
        names,
        [
            Some("test-host-1"),
            Some("alice's hub"),
            Some("test-host-3")
        ]
    );
    assert_eq!(link.machines[1].fingerprint, second.fingerprint);
    let text = link.to_string();
    assert!(!text.contains("code"), "{text}");
    assert_eq!(text.parse::<FleetLink>().unwrap(), link);

    // A machine at loopback addresses only is left out.
    readdress("host-3", &[gone_addr, "[::1]:7447".into()]);
    let fingerprints: Vec<_> = mac
        .share()
        .await
        .unwrap()
        .machines
        .into_iter()
        .map(|m| m.fingerprint)
        .collect();
    assert_eq!(
        fingerprints,
        [first.fingerprint.clone(), second.fingerprint.clone()]
    );

    // Another device of alice's signs in to each that is up with her passkey, under a key of
    // its own, with her role on each. It is on this host, so it takes the loopback address.
    let phone = Client::open(
        tmp.path().join("phone").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    // Signing in from the link offers only her passkeys: not carol's, though carol is a person
    // of the second machine too.
    let carol = Authenticator::of(&second.auth, "carol").unwrap();
    for (daemon, machine) in [(&first, &link.machines[0]), (&second, &link.machines[1])] {
        let passkey = Authenticator::of(&daemon.auth, "alice").unwrap();
        let pending = phone
            .sign_in(
                vec![daemon.addr.to_string()],
                machine.fingerprint.clone(),
                link.person.passkeys.clone(),
            )
            .await
            .unwrap();
        let challenge = pending.challenge();
        assert_eq!(challenge.allow_credentials, link.person.passkeys);
        assert!(!carol.allowed(&challenge));
        let assertion = passkey.assert(&challenge);
        pending.finish(SignIn::Passkey { assertion }).await.unwrap();
    }
    wait_host(&phone, "host-1", true).await;
    wait_host(&phone, "host-2", true).await;
    let roles: Vec<_> = phone.machines().iter().map(|m| m.role).collect();
    assert_eq!(roles, [Some(Role::Owner), Some(Role::Member)]);
    for daemon in [&first, &second] {
        let alice = daemon
            .auth
            .people()
            .into_iter()
            .find(|person| person.name == "alice")
            .unwrap();
        assert_eq!(alice.devices.len(), 2, "{alice:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_with_no_machines_shares_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    assert!(matches!(client.share().await, Err(Error::Local { .. })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_shares_nothing_while_no_machine_says_who_it_is() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("host-1");
    let daemon = Daemon::start_as("host-1", &dir, 0, "mid_turn.jsonl", Arc::default()).await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    daemon.sign_in(&client, "alice", daemon.addr).await.unwrap();
    client
        .set_addresses(
            HostId::new("host-1"),
            vec![daemon.addr.to_string(), "192.0.2.1:7447".into()],
        )
        .unwrap();
    daemon.kill().await;
    // Down is enough: it keeps dialling the address that never answers, `CONNECT_TIMEOUT` a
    // round, and a wait for it to rest between rounds could miss the moment.
    wait_connection(&client, |state| !connected(state)).await;
    // The machine is saved, but only a connected one lists who this device's person is.
    let err = client.share().await.unwrap_err();
    assert!(matches!(err, Error::Local { .. }), "{err}");
    assert!(err.to_string().contains("person"), "{err}");
}

/// Waits until the machine `host` of `client` is connected, or not when `up` is false.
async fn wait_host(client: &Client, host: &str, up: bool) {
    let host = HostId::new(host);
    let changes = client.changes();
    tokio::time::timeout(TIMEOUT, async {
        while !client.machines().iter().any(|m| {
            m.host_id == host
                && (m.connection == ConnectionState::Connected) == up
                && m.connection != ConnectionState::Connecting
        }) {
            assert!(changes.next().await);
        }
    })
    .await
    .expect("the machine did not get there in time");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_aws_sign_in_relays_its_device_code_and_exit() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let daemon_dir = tmp.path().join("daemon");
    let daemon = Daemon::start(&daemon_dir, 0, "mid_turn.jsonl", Arc::default()).await;
    let bin = daemon_dir.join("aws/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(
        daemon_dir.join("aws/config"),
        "[default]\nregion = eu-west-1\n[profile prod]\nsso_session = corp\n",
    )
    .unwrap();
    // A fake `aws sso login`: the device-code prompt, then an exit once the code is "approved".
    std::fs::write(
        bin.join("aws"),
        "#!/bin/sh\n\
         printf 'Please visit the following URL:\\n\\nhttps://device.sso.eu-west-1.amazonaws.com/\\n'\n\
         printf '\\nThen enter the code:\\n\\nQRST-UVWX\\n'\n\
         read approved\n\
         exit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(bin.join("aws"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let face_id = Arc::new(FaceId::default());
    let client = Client::open_with_passkey(
        tmp.path().join("client").display().to_string(),
        "herder-test/0".into(),
        face_id.clone(),
    )
    .unwrap();
    let host = daemon
        .sign_in(&client, "alice", daemon.addr)
        .await
        .unwrap()
        .host_id;
    face_id.hold(&daemon.auth, "alice");
    assert_eq!(
        client.list_aws_profiles(host.clone()).await.unwrap(),
        [
            AwsProfile {
                name: "default".into(),
                sso: false,
            },
            AwsProfile {
                name: "prod".into(),
                sso: true,
            },
        ]
    );

    let stream = client
        .aws_sso_login(host.clone(), "prod".into(), 80, 24)
        .await
        .unwrap();
    let device = AwsDeviceCode {
        verification_url: "https://device.sso.eu-west-1.amazonaws.com/?user_code=QRST-UVWX".into(),
        user_code: "QRST-UVWX".into(),
    };
    let listed = |client: &Client| {
        client.machines()[0].terminals.iter().any(|terminal| {
            terminal.terminal_id == stream.terminal_id()
                && terminal.purpose
                    == TerminalPurpose::AwsLogin {
                        profile: "prod".into(),
                        device: Some(device.clone()),
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
    .expect("the device code was never listed");
    stream.input(b"\n".to_vec());
    let mut screen = Screen::default();
    assert_eq!(login_exit(&stream, &mut screen).await, Some(0));
    daemon.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_device_is_asked_to_sign_in_and_a_terminal_gets_a_code_to_show() {
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
    let hosts = vec![daemon.addr.to_string()];

    let pending = tokio::time::timeout(
        TIMEOUT,
        client.sign_in(hosts.clone(), daemon.fingerprint.clone(), Vec::new()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(pending.challenge().rp_id, herder_protocol::PASSKEY_RP_ID);
    let wrong = pending
        .finish(herder_protocol::SignIn::StartLogin)
        .await
        .unwrap_err();
    assert!(matches!(wrong, Error::Local { .. }), "{wrong:?}");

    let (shown, code) = tokio::sync::oneshot::channel();
    let fingerprint = daemon.fingerprint.clone();
    let login = {
        let (client, hosts) = (client.clone(), hosts.clone());
        tokio::spawn(async move {
            client
                .log_in(hosts, fingerprint, move |code, _| {
                    let _ = shown.send(code.to_owned());
                })
                .await
        })
    };
    let code = tokio::time::timeout(TIMEOUT, code).await.unwrap().unwrap();
    assert_eq!(code.len(), 7, "{code}");
    assert_eq!(&code[3..4], "-", "{code}");
    let link = LoginUri {
        hosts,
        fingerprint: daemon.fingerprint.clone(),
        code: code.clone(),
    }
    .to_string();
    assert_eq!(link.parse::<LoginUri>().unwrap().code, code);
    assert!(!login.is_finished(), "a login waits for an app's approval");
    login.abort();
    assert!(client.machines().is_empty(), "nothing is saved before it");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_app_claims_a_machine_and_approves_a_terminal_that_logs_in() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    let hosts = vec![daemon.addr.to_string()];

    // The app claims the fresh machine with a new passkey, with the code its terminal shows.
    let app = Client::open(
        tmp.path().join("app").display().to_string(),
        "herder-app/0".into(),
    )
    .unwrap();
    let (claim, _) = daemon.auth.open_claim().unwrap();
    let pending = tokio::time::timeout(
        TIMEOUT,
        app.sign_in(hosts.clone(), daemon.fingerprint.clone(), Vec::new()),
    )
    .await
    .unwrap()
    .unwrap();
    let (passkey, registration) = Authenticator::register(
        "tomas",
        &pending.registration().expect("a fresh machine offers one"),
    );
    let claimed = pending
        .finish(SignIn::ClaimOwnership {
            code: claim,
            person: NewPerson {
                name: "Tomas".into(),
                passkey: PersonPasskey::Register { registration },
            },
        })
        .await
        .unwrap();
    assert_eq!(claimed.host_id, HostId::new("host-1"));
    wait_connection(&app, connected).await;
    assert_eq!(app.machines()[0].role, Some(Role::Owner));
    let person = app.machines()[0].person_id.clone();
    assert!(
        person.is_some(),
        "the machine names the person the app acts for"
    );
    assert_eq!(
        app.machines()[0].user_id.as_ref().map(|user| user.as_str()),
        person.as_ref().map(|person| person.as_str()),
        "a person's user id is theirs"
    );

    // A terminal logs in; the app approves the code it shows with the passkey.
    let terminal = Client::open(
        tmp.path().join("terminal").display().to_string(),
        "herder-cli/0".into(),
    )
    .unwrap();
    let (shown, code) = tokio::sync::oneshot::channel();
    let login = {
        let (terminal, hosts) = (terminal.clone(), hosts.clone());
        let fingerprint = daemon.fingerprint.clone();
        tokio::spawn(async move {
            terminal
                .log_in(hosts, fingerprint, move |code, _| {
                    let _ = shown.send(code.to_owned());
                })
                .await
        })
    };
    let code = tokio::time::timeout(TIMEOUT, code).await.unwrap().unwrap();
    let CommandResult::PasskeyChallenge { challenge, .. } = app
        .send(claimed.host_id.clone(), CommandBody::PasskeyChallenge)
        .await
        .unwrap()
    else {
        panic!("expected a passkey challenge");
    };
    let approve = CommandBody::ApproveLogin {
        code,
        assertion: Box::new(passkey.assert(&challenge)),
    };
    let CommandResult::LoginApproved { client, .. } =
        app.send(claimed.host_id.clone(), approve).await.unwrap()
    else {
        panic!("expected the login approved");
    };
    assert_eq!(client, "herder-cli/0");

    let machine = tokio::time::timeout(TIMEOUT, login)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(machine.host_id, claimed.host_id);
    wait_connection(&terminal, connected).await;
    assert_eq!(terminal.machines()[0].role, Some(Role::Owner));
    assert_eq!(
        terminal.machines()[0].person_id,
        person,
        "the same person as the app"
    );
}

/// The test passkey as an app's Face ID, counting its prompts; it holds the passkey once the
/// app claims the machine with it.
#[derive(Default)]
struct FaceId {
    passkey: std::sync::Mutex<Option<Arc<Authenticator>>>,
    prompts: AtomicU64,
}

impl FaceId {
    /// Holds the passkey of `auth`'s person `name`, once they are there.
    fn hold(&self, auth: &Auth, name: &str) {
        *self.passkey.lock().unwrap() = Authenticator::of(auth, name).map(Arc::new);
    }
}

impl herder_client_core::Authenticator for FaceId {
    fn assert(&self, _: HostId, challenge: PasskeyChallenge) -> herder_client_core::Assertion {
        self.prompts.fetch_add(1, Ordering::SeqCst);
        let passkey = self.passkey.lock().unwrap().clone();
        Box::pin(async move {
            Ok(passkey
                .expect("the app claimed the machine")
                .assert(&challenge))
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_app_signs_with_face_id_and_approves_what_a_terminal_asks() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    let hosts = vec![daemon.addr.to_string()];
    let face_id = Arc::new(FaceId::default());
    let app = Client::open_with_passkey(
        tmp.path().join("app").display().to_string(),
        "herder-app/0".into(),
        face_id.clone(),
    )
    .unwrap();
    let (claim, _) = daemon.auth.open_claim().unwrap();
    let pending = app
        .sign_in(hosts.clone(), daemon.fingerprint.clone(), Vec::new())
        .await
        .unwrap();
    let (passkey, registration) = Authenticator::register(
        "tomas",
        &pending.registration().expect("a fresh machine offers one"),
    );
    let passkey = Arc::new(passkey);
    *face_id.passkey.lock().unwrap() = Some(Arc::clone(&passkey));
    let host = pending
        .finish(SignIn::ClaimOwnership {
            code: claim,
            person: NewPerson {
                name: "Tomas".into(),
                passkey: PersonPasskey::Register { registration },
            },
        })
        .await
        .unwrap()
        .host_id;
    wait_connection(&app, connected).await;

    // Opening a terminal asks Face ID once; attaching the terminal it opened asks nothing.
    let session = create_session(&app, &host, repo(&tmp.path().join("repo"))).await;
    let stream = tokio::time::timeout(
        TIMEOUT,
        app.open_terminal(host.clone(), session.clone(), 80, 24),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(face_id.prompts.load(Ordering::SeqCst), 1);
    drop(stream);

    // A terminal logs in, approved by the app.
    let terminal = Client::open(
        tmp.path().join("terminal").display().to_string(),
        "herder-tui/0".into(),
    )
    .unwrap();
    let (shown, code) = tokio::sync::oneshot::channel();
    let login = {
        let (terminal, hosts) = (terminal.clone(), hosts.clone());
        let fingerprint = daemon.fingerprint.clone();
        tokio::spawn(async move {
            terminal
                .log_in(hosts, fingerprint, move |code, _| {
                    let _ = shown.send(code.to_owned());
                })
                .await
        })
    };
    let code = tokio::time::timeout(TIMEOUT, code).await.unwrap().unwrap();
    let CommandResult::PasskeyChallenge { challenge, .. } = app
        .send(host.clone(), CommandBody::PasskeyChallenge)
        .await
        .unwrap()
    else {
        panic!("expected a passkey challenge");
    };
    let approve = CommandBody::ApproveLogin {
        code,
        assertion: Box::new(passkey.assert(&challenge)),
    };
    app.send(host.clone(), approve).await.unwrap();
    tokio::time::timeout(TIMEOUT, login)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    wait_connection(&terminal, connected).await;

    // It has no passkey: what needs one waits for the app, which approves it with Face ID.
    let asked = {
        let (terminal, host, session) = (terminal.clone(), host.clone(), session.clone());
        tokio::spawn(async move { terminal.open_terminal(host, session, 80, 24).await })
    };
    wait_machine(&app, |machine| !machine.approvals.is_empty()).await;
    let request = app.machines()[0].approvals[0].clone();
    assert_eq!(
        (request.action.as_str(), request.client.as_str()),
        ("Open a terminal", "herder-tui/0")
    );
    app.approve_action(host.clone(), request.request_id)
        .await
        .unwrap();
    assert_eq!(face_id.prompts.load(Ordering::SeqCst), 2);
    tokio::time::timeout(TIMEOUT, asked)
        .await
        .unwrap()
        .unwrap()
        .expect("the approved terminal opens");
    wait_machine(&app, |machine| machine.approvals.is_empty()).await;

    // Refused on the app, the terminal is told so.
    let asked = {
        let (terminal, host) = (terminal.clone(), host.clone());
        let change = CommandBody::CreateSecretSet {
            set: "prod".into(),
            assertion: None,
        };
        tokio::spawn(async move { terminal.send(host, change).await })
    };
    wait_machine(&app, |machine| !machine.approvals.is_empty()).await;
    let request = app.machines()[0].approvals[0].clone();
    app.refuse_action(host.clone(), request.request_id)
        .await
        .unwrap();
    let refused = tokio::time::timeout(TIMEOUT, asked).await.unwrap().unwrap();
    let Err(Error::Rejected { info }) = refused else {
        panic!("expected the refused change rejected, got {refused:?}");
    };
    assert_eq!(info.code, ErrorCode::Forbidden);
    assert_eq!(
        face_id.prompts.load(Ordering::SeqCst),
        2,
        "refusing needs no Face ID"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_passkey_sign_in_says_why() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    let owner = Client::open(
        tmp.path().join("owner").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    daemon.sign_in(&owner, "alice", daemon.addr).await.unwrap();
    let alice = Authenticator::of(&daemon.auth, "alice").unwrap();

    let phone = Client::open(
        tmp.path().join("phone").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    let hosts = vec![daemon.addr.to_string()];
    let refused = |sign: Box<dyn FnOnce(&PasskeyChallenge) -> PasskeyAssertion + Send>| {
        let (phone, hosts, fingerprint) =
            (phone.clone(), hosts.clone(), daemon.fingerprint.clone());
        async move {
            let pending = phone.sign_in(hosts, fingerprint, Vec::new()).await.unwrap();
            let assertion = sign(&pending.challenge());
            pending
                .finish(SignIn::Passkey { assertion })
                .await
                .unwrap_err()
        }
    };

    // Nobody on the machine has this passkey: ask its owner.
    let stranger = Authenticator::new("sam", &[7; 16]);
    let unknown = refused(Box::new(move |challenge| stranger.assert(challenge))).await;
    assert!(
        matches!(
            &unknown,
            Error::SignInRefused { refusal: SignInRefusal::UnknownPerson { owners }, .. }
                if owners == &["alice".to_owned()]
        ),
        "{unknown:?}"
    );

    // Alice's passkey, but Face ID was skipped.
    let skipped = refused(Box::new(move |challenge| {
        alice.assert_unverified(challenge)
    }))
    .await;
    assert!(
        matches!(
            &skipped,
            Error::SignInRefused {
                refusal: SignInRefusal::UserNotVerified,
                ..
            }
        ),
        "{skipped:?}"
    );
    assert!(phone.machines().is_empty());
}

fn skills_status(head: &str) -> herder_protocol::SkillsStatus {
    herder_protocol::SkillsStatus {
        repo: Some("https://github.com/you/herder-skills".into()),
        head: Some(head.into()),
        last_pull: None,
        pull_error: None,
        skills: Vec::new(),
        reload: Vec::new(),
        accounts: Vec::new(),
    }
}

fn usage(processes: u32) -> SessionUsage {
    SessionUsage {
        cpu_percent: 12.0,
        memory_bytes: 512 << 20,
        processes,
        containers: Vec::new(),
    }
}

/// A flaky network drops the connection three times in the middle of a turn. Each time the
/// client is back within moments, and nothing an app shows moves: the machine stays connected
/// with its lists, its address and its live values, the streaming text stays where it was, and
/// the session's events arrive once each. What the daemon dropped meanwhile is gone once the
/// new connection has sent its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn short_drops_mid_stream_are_never_seen() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("daemon");
    let repo = repo(&tmp.path().join("app"));
    let daemon = Daemon::start(&data, 0, "interrupted_turn.jsonl", Arc::default()).await;
    let relay = Relay::start(daemon.addr).await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    let host = daemon
        .sign_in(&client, "alice", relay.addr)
        .await
        .unwrap()
        .host_id;
    let (kept, gone) = (SessionId::new("kept"), SessionId::new("gone"));
    daemon.fanout.host_resources(host_resources(3));
    daemon.fanout.skills_status(skills_status("a1"));
    daemon.fanout.session_resources(&kept, usage(3));
    daemon.fanout.session_resources(&gone, usage(1));
    let session_id = create_session(&client, &host, repo).await;
    let sub = client
        .subscribe_session(host.clone(), session_id.clone())
        .unwrap();
    let prompt = CommandBody::SendPrompt {
        session_id: session_id.clone(),
        text: "First.".into(),
        images: Vec::new(),
        files: Vec::new(),
    };
    client.send(host.clone(), prompt).await.unwrap();
    let mut view = View::default();
    view.read_until(&sub, |view| view.streaming_text() == ["Half"])
        .await;
    wait_machine(&client, |m| {
        m.resources.is_some() && m.skills.is_some() && m.session_usage.len() == 2
    })
    .await;

    // What an app would have seen go wrong, looked for on every change.
    let wrong = Arc::new(std::sync::Mutex::new(Vec::new()));
    let watcher = tokio::spawn({
        let (client, wrong, kept) = (client.clone(), Arc::clone(&wrong), kept.clone());
        async move {
            let changes = client.changes();
            loop {
                let machine = client.machines().remove(0);
                let checks = [
                    (connected(&machine.connection), "the connection"),
                    (machine.address.is_some(), "the address"),
                    (!machine.sessions.is_empty(), "the sessions"),
                    (!machine.accounts.is_empty(), "the accounts"),
                    (machine.resources.is_some(), "the resources"),
                    (machine.skills.is_some(), "the skills"),
                    (machine.session_usage.contains_key(&kept), "the usage"),
                ];
                for (held, what) in checks {
                    if !held {
                        wrong.lock().unwrap().push(format!("{what}: {machine:?}"));
                    }
                }
                if !changes.next().await {
                    break;
                }
            }
        }
    });

    let idle = usage(0);
    for round in 1..=3 {
        relay.cut();
        if round == 1 {
            // Only the connection that is gone was told: the next one hears nothing of it.
            daemon.fanout.session_resources(&gone, idle.clone());
        }
        wait_machine(&client, |m| m.quality.reconnects == round).await;
        client.synced(host.clone()).await.unwrap();
        // Pairing took one connection, the supervisor's first another.
        assert_eq!(relay.accepted(), 2 + u64::from(round));
        let machine = client.machines().remove(0);
        assert!(!machine.session_usage.contains_key(&gone), "{machine:?}");
        assert_eq!(machine.resources, Some(host_resources(3)));
    }

    // The text that was streaming is there in every update until the turn's end takes it.
    let interrupt = CommandBody::Interrupt {
        session_id: session_id.clone(),
    };
    client.send(host.clone(), interrupt).await.unwrap();
    view.read_until(&sub, |view| {
        let ended = interrupted("turn-1")(view);
        assert!(
            ended || view.streaming_text() == ["Half"],
            "{:?}",
            view.latest
        );
        ended
    })
    .await;
    assert!(view.latest.streaming.is_empty(), "{:?}", view.latest);

    watcher.abort();
    let wrong = wrong.lock().unwrap().clone();
    assert!(wrong.is_empty(), "{wrong:#?}");
    daemon.kill().await;
    let journal = Store::open(data.join("herder.db"))
        .unwrap()
        .read_since(&session_id, 0, usize::MAX)
        .unwrap();
    assert_eq!(view.events[..], journal[..view.events.len()]);
}

/// The network is gone for longer than a drop that is ridden out. A prompt sent meanwhile
/// waits; the machine shows as connected at first and as lost after, with its lists but
/// without its live values. Once the network is back the prompt is applied, once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_prompt_sent_through_an_outage_is_applied_once() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("daemon");
    let repo = repo(&tmp.path().join("app"));
    let daemon = Daemon::start(&data, 0, "interrupted_turn.jsonl", Arc::default()).await;
    let relay = Relay::start(daemon.addr).await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    let host = daemon
        .sign_in(&client, "alice", relay.addr)
        .await
        .unwrap()
        .host_id;
    daemon.fanout.host_resources(host_resources(3));
    let session_id = create_session(&client, &host, repo).await;
    let sub = client
        .subscribe_session(host.clone(), session_id.clone())
        .unwrap();
    wait_machine(&client, |m| m.resources.is_some()).await;
    client.synced(host.clone()).await.unwrap();

    relay.swallow(true);
    relay.cut();
    let sending = tokio::spawn({
        let (client, host, session_id) = (client.clone(), host.clone(), session_id.clone());
        async move {
            let prompt = CommandBody::SendPrompt {
                session_id,
                text: "First.".into(),
                images: Vec::new(),
                files: Vec::new(),
            };
            client.send(host, prompt).await
        }
    });
    tokio::time::sleep(Duration::from_secs(2)).await;
    let machine = client.machines().remove(0);
    assert!(connected(&machine.connection), "{machine:?}");
    assert_eq!(machine.resources, Some(host_resources(3)));

    wait_machine(&client, |m| !connected(&m.connection)).await;
    let machine = client.machines().remove(0);
    assert_eq!(machine.resources, None);
    assert_eq!(machine.sessions.len(), 1);
    assert!(!sending.is_finished());

    relay.swallow(false);
    client.wake();
    assert_eq!(sending.await.unwrap().unwrap(), CommandResult::Applied);
    let mut view = View::default();
    view.read_until(&sub, |view| view.streaming_text() == ["Half"])
        .await;
    wait_machine(&client, |m| {
        connected(&m.connection) && m.resources == Some(host_resources(3))
    })
    .await;
    let interrupt = CommandBody::Interrupt {
        session_id: session_id.clone(),
    };
    client.send(host.clone(), interrupt).await.unwrap();
    view.read_until(&sub, interrupted("turn-1")).await;

    daemon.kill().await;
    let journal = Store::open(data.join("herder.db"))
        .unwrap()
        .read_since(&session_id, 0, usize::MAX)
        .unwrap();
    assert_eq!(view.events[..], journal[..view.events.len()]);
    let turns = journal
        .iter()
        .filter(|event| matches!(event.body, EventBody::TurnStarted { .. }))
        .count();
    assert_eq!(turns, 1, "{journal:#?}");
}

/// A command sent into a connection that went silently dead is not left waiting for the next
/// ping: it probes the connection, which is replaced within the probe's five seconds, and is
/// answered on the new one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_command_finds_a_dead_connection_and_is_answered_on_the_next() {
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
    let host = daemon
        .sign_in(&client, "alice", relay.addr)
        .await
        .unwrap()
        .host_id;
    client.synced(host.clone()).await.unwrap();
    assert_eq!(relay.accepted(), 2);

    relay.stall();
    let prompt = CommandBody::SendPrompt {
        session_id: SessionId::new("nope"),
        text: "look".into(),
        images: Vec::new(),
        files: Vec::new(),
    };
    let sent = tokio::time::timeout(Duration::from_secs(9), client.send(host, prompt))
        .await
        .expect("the dead connection was not replaced in time");
    assert!(
        matches!(&sent, Err(Error::Rejected { info }) if info.code == ErrorCode::NotFound),
        "{sent:?}"
    );
    assert_eq!(relay.accepted(), 3);
    let machine = client.machines().remove(0);
    assert!(connected(&machine.connection));
    assert_eq!(machine.quality.reconnects, 1);
}

/// What the daemons of this test process log at info and above, as a daemon does by default.
#[derive(Clone, Default)]
struct Logged(Arc<Mutex<Vec<u8>>>);

/// The log of this test process: the subscriber is the process's, as whether anyone listens
/// to a log statement is settled once, on whichever thread reaches it first.
static LOGGED: LazyLock<Logged> = LazyLock::new(|| {
    let logged = Logged::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing_subscriber::filter::LevelFilter::INFO)
        .with_ansi(false)
        .with_writer({
            let logged = logged.clone();
            move || logged.clone()
        })
        .finish();
    // Nothing else in this test process sets one.
    let _ = tracing::subscriber::set_global_default(subscriber);
    logged
});

impl std::io::Write for Logged {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Logged {
    /// The lines logged so far that hold `text` about the client named `client`.
    fn lines(&self, text: &str, client: &str) -> Vec<String> {
        let client = format!("client={client} ");
        String::from_utf8_lossy(&self.0.lock().unwrap())
            .lines()
            .filter(|line| line.contains(text) && line.contains(&client))
            .map(str::to_owned)
            .collect()
    }

    /// [`Logged::lines`], once there are `count` of them.
    async fn wait_lines(&self, text: &str, client: &str, count: usize) -> Vec<String> {
        let logged = async {
            loop {
                let lines = self.lines(text, client);
                if lines.len() >= count {
                    return lines;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(TIMEOUT, logged)
            .await
            .unwrap_or_else(|_| panic!("fewer than {count} lines with {text:?} for {client}"))
    }
}

/// A relay to `target` on which everything arrives `lag` late, either way, as on a link with
/// that much latency.
async fn lagging(target: SocketAddr, lag: Duration) -> SocketAddr {
    /// Passes on what `from` sends, each read `lag` after it was made.
    async fn late(
        mut from: impl AsyncRead + Unpin,
        mut to: impl AsyncWrite + Unpin + Send + 'static,
        lag: Duration,
    ) {
        let (read, mut due) =
            tokio::sync::mpsc::unbounded_channel::<(tokio::time::Instant, Vec<u8>)>();
        tokio::spawn(async move {
            while let Some((at, bytes)) = due.recv().await {
                tokio::time::sleep_until(at).await;
                if to.write_all(&bytes).await.is_err() {
                    return;
                }
            }
        });
        let mut chunk = vec![0; 64 * 1024];
        loop {
            match from.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(count) => {
                    let at = tokio::time::Instant::now() + lag;
                    if read.send((at, chunk[..count].to_vec())).is_err() {
                        return;
                    }
                }
            }
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((inbound, _)) = listener.accept().await {
            tokio::spawn(async move {
                // The connect lags as what follows it does.
                tokio::time::sleep(lag).await;
                let Ok(outbound) = TcpStream::connect(target).await else {
                    return;
                };
                let (from_client, to_client) = inbound.into_split();
                let (from_daemon, to_daemon) = outbound.into_split();
                // Either end closing drops both.
                tokio::select! {
                    () = late(from_client, to_daemon, lag) => {}
                    () = late(from_daemon, to_client, lag) => {}
                }
            });
        }
    });
    addr
}

/// The daemon lists everything and replays every session asked for to each connection that
/// says hello. On a link slow enough that the first address is still at its hello when the
/// second has its head start behind it, both got that far, and the daemon did all of it
/// twice, for a connection the client dropped the moment the other answered: the log of a
/// phone on a bad network showed its connects in pairs, 30 to 300 ms apart. One connection
/// says hello, whichever addresses got how far; so does a connection that only finds out
/// whether an address that comes first answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn racing_addresses_say_one_hello() {
    let logged = &*LOGGED;
    let tmp = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    // A quarter of a second each way: an address says hello a second after it was dialled
    // and has its answer half a second later, 200 ms after the next one said its own.
    let lag = Duration::from_millis(250);
    let first = lagging(daemon.addr, lag).await.to_string();
    let second = lagging(daemon.addr, lag).await.to_string();
    let name = "herder-one-hello/0";
    let client =
        Client::open(tmp.path().join("client").display().to_string(), name.into()).unwrap();
    // Signing in says a hello of its own, on its own connection.
    let hosts = vec![first.clone(), second.clone()];
    let host = authenticator::sign_in(&daemon.auth, &client, hosts, &daemon.fingerprint, "alice")
        .await
        .unwrap()
        .host_id;
    client.synced(host.clone()).await.unwrap();
    let said = logged.lines("client connected", name).len();

    for reconnects in 1..=3 {
        assert_eq!(client.reconnect(host.clone()).await, Ok(first.clone()));
        client.synced(host.clone()).await.unwrap();
        // Past the time the address that lost would have needed for its hello.
        tokio::time::sleep(lag * 3).await;
        let hellos = logged.lines("client connected", name);
        assert_eq!(hellos.len(), said + reconnects, "{hellos:#?}");
    }
    let said = logged.lines("client connected", name).len();

    // An address put first that answers moves the connection there: finding out that it
    // answers says no hello, the connection that moves there says one.
    let direct = daemon.addr.to_string();
    client
        .set_addresses(host.clone(), vec![direct.clone(), first, second])
        .unwrap();
    wait_machine(&client, |machine| {
        machine.address.as_deref() == Some(direct.as_str()) && connected(&machine.connection)
    })
    .await;
    client.synced(host).await.unwrap();
    tokio::time::sleep(lag * 3).await;
    let hellos = logged.lines("client connected", name);
    assert_eq!(hellos.len(), said + 1, "{hellos:#?}");
}

/// A network that lets a connection be made and then drops it, or carries nothing on it,
/// had the client connect again at once, each time: the log of a phone on such a network
/// showed it every six seconds for minutes, each connection made at once after the one
/// before was found dead. The daemon lists everything and replays to every one of them,
/// which such a network carries no better. Connections that keep ending young are made less
/// and less often.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connections_that_keep_dying_young_are_made_less_and_less_often() {
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
    let host = daemon
        .sign_in(&client, "alice", relay.addr)
        .await
        .unwrap()
        .host_id;
    client.synced(host.clone()).await.unwrap();
    let before = relay.accepted();

    // Every connection is dropped a moment after it was made, for six seconds.
    let started = std::time::Instant::now();
    let mut cut = 0;
    while started.elapsed() < Duration::from_secs(6) {
        if relay.accepted() > cut {
            cut = relay.accepted();
            tokio::time::sleep(Duration::from_millis(100)).await;
            relay.cut();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // At once, then after a second or two, then after two to four; not every 350 ms.
    let made = relay.accepted() - before;
    assert!((2..=4).contains(&made), "{made} connections in six seconds");

    // The network is back: so is the connection, with nothing asked of the client.
    wait_machine(&client, |machine| {
        relay.accepted() > cut && connected(&machine.connection)
    })
    .await;
    client.synced(host).await.unwrap();
}

/// A relay to `target` that passes on what the daemon sends at no more than `rate` bytes a
/// second, as a slow downlink would; how many connections it accepted.
async fn slow_down(target: SocketAddr, rate: u64) -> (SocketAddr, Arc<AtomicU64>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = Arc::new(AtomicU64::new(0));
    let count = Arc::clone(&accepted);
    tokio::spawn(async move {
        while let Ok((mut inbound, _)) = listener.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut outbound = TcpStream::connect(target).await.unwrap();
                let (mut from_client, mut to_client) = inbound.split();
                let (mut from_daemon, mut to_daemon) = outbound.split();
                tokio::select! {
                    () = trickle(&mut from_daemon, &mut to_client, rate) => {}
                    _ = tokio::io::copy(&mut from_client, &mut to_daemon) => {}
                }
            });
        }
    });
    (addr, accepted)
}

/// A long replay on a slow link takes far longer than a dead connection is given to show a
/// sign of life, and the pongs of the pings sent meanwhile wait behind all of it. Taken for
/// dead, the connection would be replaced, the replay start over, and so on for as long as
/// the link is slow. A connection that carries anything is alive.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_link_carrying_a_long_replay_is_not_taken_for_dead() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(&tmp.path().join("app"));
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "interrupted_turn.jsonl",
        Arc::default(),
    )
    .await;
    let desktop = Client::open(
        tmp.path().join("desktop").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    let host = daemon
        .sign_in(&desktop, "alice", daemon.addr)
        .await
        .unwrap()
        .host_id;
    let session_id = create_session(&desktop, &host, repo).await;
    // Five megabytes of journal, in one event; some four compressed.
    let text = noise_text(5_000_000);
    let prompt = CommandBody::SendPrompt {
        session_id: session_id.clone(),
        text: text.clone(),
        images: Vec::new(),
        files: Vec::new(),
    };
    assert_eq!(
        desktop.send(host.clone(), prompt).await.unwrap(),
        CommandResult::Applied
    );

    // 250 kB a second: the replay takes some sixteen seconds.
    let (slow, accepted) = slow_down(daemon.addr, 250_000).await;
    let phone = Client::open(
        tmp.path().join("phone").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    daemon.sign_in(&phone, "alice", slow).await.unwrap();
    phone.synced(host.clone()).await.unwrap();
    assert_eq!(accepted.load(Ordering::SeqCst), 2);

    let started = std::time::Instant::now();
    let sub = phone.subscribe_session(host.clone(), session_id).unwrap();
    // The app comes to the front and the network changes while it replays.
    phone.wake();
    let replayed = async {
        loop {
            let update = sub.next().await.expect("the subscription ended");
            let prompted = |event: &Event| serde_json::to_string(event).unwrap().len() > text.len();
            if update.events.iter().any(prompted) {
                return;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(60), replayed)
        .await
        .expect("the replay did not arrive");
    assert!(
        started.elapsed() > Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(accepted.load(Ordering::SeqCst), 2);
    let machine = &phone.machines()[0];
    assert!(connected(&machine.connection));
    assert_eq!(machine.quality.reconnects, 0);
}

/// A client that keeps reconnecting is diagnosed from the daemon's log: a connection the
/// client gives up tells the daemon why as it closes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_connection_given_up_tells_the_daemon_why() {
    let logged = &*LOGGED;
    let tmp = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(
        &tmp.path().join("daemon"),
        0,
        "mid_turn.jsonl",
        Arc::default(),
    )
    .await;
    let name = "herder-says-why/0";
    let client =
        Client::open(tmp.path().join("client").display().to_string(), name.into()).unwrap();
    let host = daemon
        .sign_in(&client, "alice", daemon.addr)
        .await
        .unwrap()
        .host_id;
    client.synced(host.clone()).await.unwrap();

    client.reconnect(host.clone()).await.unwrap();
    // The connection that signed the device in was dropped without a word.
    let closed = "the client closed the connection: ";
    let line = logged.wait_lines(closed, name, 1).await.remove(0);
    assert!(line.contains("client disconnected"), "{line}");
    assert!(line.contains(r#""reconnecting, as asked""#), "{line}");

    // So does one whose address is no longer one of the machine's.
    let relay = Relay::start(daemon.addr).await;
    client
        .set_addresses(host.clone(), vec![relay.addr.to_string()])
        .unwrap();
    let line = logged.wait_lines(closed, name, 2).await.remove(1);
    let reason = format!(
        "{} is no longer one of the machine's addresses",
        daemon.addr
    );
    assert!(line.contains(&reason), "{line}");
    client.synced(host).await.unwrap();
}
