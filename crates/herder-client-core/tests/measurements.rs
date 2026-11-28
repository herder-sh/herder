//! What the client core measures for PostHog, as a fake PostHog on localhost gets it: a real
//! daemon with the fake adapter, over TLS on localhost, in this process, its own, since
//! measuring is on or off for the whole process. Nothing here reaches the real PostHog.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use herder_adapters::fake::FakeAdapter;
use herder_client_core::{Client, ConnectionState};
use herder_daemon::Fanout;
use herder_daemon::auth::{Auth, authenticator};
use herder_daemon::login::Logins;
use herder_daemon::session::{self, AccountConfig, Accounts, Adapters, SessionManager, Setup};
use herder_daemon::terminal::Terminals;
use herder_daemon::worktree::Worktrees;
use herder_daemon::ws::{Host, Server, Tls};
use herder_protocol::{AccountId, HostId, Provider, TurnId};
use herder_store::Store;
use herder_telemetry::{Component, Endpoints, Telemetry};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(20);

/// Starts a daemon on `dir`; answers its address, its fingerprint and its auth.
async fn daemon(dir: &Path) -> (SocketAddr, String, Arc<Auth>, CancellationToken) {
    std::fs::create_dir_all(dir.join("tls")).unwrap();
    let tls = Tls::load_or_create(&dir.join("tls"), "test-host").unwrap();
    let auth = Arc::new(Auth::open(dir).unwrap());
    let fanout = Arc::new(Fanout::default());
    let fake = Provider::Other("fake".into());
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/traced_turn.jsonl");
    let mut adapters = Adapters::new();
    adapters.register(fake.clone(), Arc::new(FakeAdapter::new(script)));
    let mut accounts = Accounts::new();
    accounts.insert(
        AccountId::new("account-1"),
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
        turn_ids: Box::new(|| TurnId::new("turn-1")),
        worktrees: Worktrees::new(dir.join("worktrees")),
        attachments: dir.join("attachments"),
        chats: dir.join("chats"),
    };
    let shutdown = CancellationToken::new();
    let sessions = SessionManager::open(setup, shutdown.clone()).await.unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let fingerprint = tls.fingerprint().to_owned();
    let host = Host {
        id: HostId::new("host-1"),
        name: "test-host-1".into(),
    };
    let terminals = Terminals::new(Arc::clone(&fanout), PathBuf::from("/bin/sh"));
    let logins = Logins::new(HashMap::new(), dir.join("daemon.toml"), sessions.clone());
    let server = Server::new(
        tls,
        Arc::clone(&auth),
        fanout,
        sessions,
        terminals,
        logins,
        host,
    );
    tokio::spawn(server.run(vec![listener], shutdown.clone()));
    (addr, fingerprint, auth, shutdown)
}

/// A relay to a daemon that, while it swallows, takes new connections and never answers
/// them, as a network that loses what is sent.
struct Relay {
    addr: SocketAddr,
    swallowing: Arc<AtomicBool>,
}

impl Relay {
    async fn start(target: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let swallowing = Arc::new(AtomicBool::new(false));
        let swallows = Arc::clone(&swallowing);
        tokio::spawn(async move {
            while let Ok((mut inbound, _)) = listener.accept().await {
                let swallowed = swallows.load(Ordering::SeqCst);
                tokio::spawn(async move {
                    if swallowed {
                        std::future::pending::<()>().await;
                    }
                    let mut outbound = TcpStream::connect(target).await.unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                });
            }
        });
        Self { addr, swallowing }
    }

    fn swallow(&self, swallowing: bool) {
        self.swallowing.store(swallowing, Ordering::SeqCst);
    }
}

/// A PostHog that keeps every request body it is sent, on localhost.
struct FakePostHog {
    bodies: Arc<Mutex<Vec<String>>>,
}

impl FakePostHog {
    fn start() -> Self {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let host = format!("http://{}", listener.local_addr().unwrap());
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&bodies);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let kept = Arc::clone(&kept);
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut writer = stream;
                    loop {
                        let mut length = 0;
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                                return;
                            }
                            let line = line.trim_end();
                            if line.is_empty() {
                                break;
                            }
                            if let Some((name, value)) = line.split_once(':')
                                && name.eq_ignore_ascii_case("content-length")
                            {
                                length = value.trim().parse().unwrap();
                            }
                        }
                        let mut body = vec![0; length];
                        reader.read_exact(&mut body).unwrap();
                        kept.lock()
                            .unwrap()
                            .push(String::from_utf8_lossy(&body).into_owned());
                        let answer = "{\"status\":\"Ok\"}";
                        let _ = write!(
                            writer,
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{answer}",
                            answer.len()
                        );
                    }
                });
            }
        });
        // On, sending here: this process's measurements all come to this PostHog. PostHog's
        // client blocks, so it is made and flushed off the async runtime.
        tokio::task::block_in_place(|| {
            herder_telemetry::set_with(
                Some(Telemetry {
                    install_id: "0b7f1c2e-4d4b-4e0e-9a51-3f6f1b2a9c10".into(),
                    sentry_dsn: None,
                    posthog_token: Some("phc_test".into()),
                    component: Component::App,
                }),
                Endpoints {
                    posthog_host: host.clone(),
                    ..Endpoints::default()
                },
            )
        });
        Self { bodies }
    }

    /// The properties of every `event` it got so far, oldest first.
    fn events(&self, event: &str) -> Vec<serde_json::Value> {
        tokio::task::block_in_place(herder_telemetry::flush_measurements);
        self.bodies
            .lock()
            .unwrap()
            .iter()
            .filter_map(|body| serde_json::from_str::<serde_json::Value>(body).ok())
            .flat_map(|body| match body.get("batch") {
                Some(serde_json::Value::Array(batch)) => batch.clone(),
                _ => vec![body],
            })
            .filter(|sent| sent["event"] == event)
            .map(|sent| sent["properties"].clone())
            .collect()
    }
}

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

fn number(properties: &serde_json::Value, name: &str) -> u64 {
    properties[name]
        .as_u64()
        .unwrap_or_else(|| panic!("no {name} in {properties}"))
}

/// The OS may freeze an app in the background while it connects: a connect counts from the
/// wake that brings it back, not from before, and says where its time went.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_connect_counts_from_the_wake_out_of_the_background() {
    let posthog = FakePostHog::start();
    let tmp = tempfile::tempdir().unwrap();
    let (addr, fingerprint, auth, shutdown) = daemon(&tmp.path().join("daemon")).await;
    let relay = Relay::start(addr).await;
    let phone = Client::open(
        tmp.path().join("phone").to_str().unwrap().to_owned(),
        "herder-test/0".into(),
    )
    .unwrap();
    let host = authenticator::sign_in(
        &auth,
        &phone,
        vec![relay.addr.to_string()],
        &fingerprint,
        "alice",
    )
    .await
    .unwrap()
    .host_id;
    phone.synced(host.clone()).await.unwrap();

    // A new connection that goes unanswered, and the app goes to the background meanwhile.
    relay.swallow(true);
    let reconnecting = tokio::spawn({
        let (phone, host) = (phone.clone(), host.clone());
        async move { phone.reconnect(host).await }
    });
    wait_connection(&phone, |state| *state != ConnectionState::Connected).await;
    phone.suspend();
    tokio::time::sleep(Duration::from_secs(4)).await;
    relay.swallow(false);
    phone.wake();
    reconnecting.await.unwrap().unwrap();

    let connected = posthog.events("connected");
    let last = connected.last().expect("no connected event");
    // A second for the wake's race to start beside the one under way, then a connection on
    // localhost: not the four seconds in the background.
    assert!(number(last, "connect_ms") < 2500, "{last}");
    assert!(
        number(last, "waited_ms") <= number(last, "connect_ms"),
        "{last}"
    );
    assert!(number(last, "reach_ms") < 1000, "{last}");
    assert!(number(last, "hello_ms") < 1000, "{last}");
    shutdown.cancel();
}
