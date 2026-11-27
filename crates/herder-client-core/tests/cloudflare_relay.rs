//! Machines reached through the hub's Durable Object relay (`docs/remote-access.md`): the
//! relay Worker (`deploy/cloudflare-relay/`) runs in a local `workerd` (miniflare), the hub
//! signs tokens for it, and the hub and a host linked to it keep their connections to it. The
//! client gets a token from the hub, adds the `relay+ws://` route to both, last, and reaches
//! each through the relay alone, a session's journal included. A token half used is renewed.
//!
//! The Worker's tests need Node 22 and `npm ci` in `deploy/cloudflare-relay`; CI runs them,
//! and without them here the tests are skipped.

use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use herder_client_core::{Client, ConnectionState, Machine, RouteChoice};
use herder_daemon::auth::Auth;
use herder_daemon::auth::authenticator::Join;
use herder_daemon::auth::control::{self, Request, Response};
use herder_daemon::config::Retention;
use herder_daemon::hub::{HubStore, LIVENESS_TIMEOUT, Server};
use herder_daemon::ws::{Host, Tls};
use herder_protocol::{
    AccountId, CommandBody, CommandResult, EventBody, HostId, PermissionMode, Provider, Role,
    SessionId, Timestamp,
};
use herder_store::{NewEvent, Store};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(30);

/// The secret the hub signs tokens with, and the local relay checks them with.
const SECRET: &str = "relay-secret-for-tests";

/// The relay Worker in a local `workerd`; killed on drop.
struct LocalRelay {
    child: Child,
    /// Where it listens, `http://127.0.0.1:<port>`.
    url: String,
}

impl LocalRelay {
    /// Starts it, or `None` where `npm ci` did not run in `deploy/cloudflare-relay`; in CI
    /// that fails instead.
    fn start() -> Option<Self> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/cloudflare-relay");
        // workerd loads modules from below its working directory only.
        let dir = dir.canonicalize().unwrap();
        if !dir.join("node_modules/miniflare").exists() {
            assert!(
                std::env::var_os("CI").is_none(),
                "run `npm ci` in deploy/cloudflare-relay: CI tests the relay"
            );
            eprintln!("skipped: no miniflare; run `npm ci` in deploy/cloudflare-relay");
            return None;
        }
        let mut child = Command::new("node")
            .current_dir(&dir)
            .args(["serve.mjs", SECRET])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("node runs");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let url = line.trim().trim_end_matches('/').to_owned();
        assert!(url.starts_with("http://"), "miniflare said {line:?}");
        Some(Self { child, url })
    }

    /// The route through it, as the client saves it.
    fn route(&self) -> String {
        self.url.replacen("http://", "relay+ws://", 1)
    }
}

impl Drop for LocalRelay {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A hub in this process whose Cloudflare relay is the local one.
struct Hub {
    addr: SocketAddr,
    fingerprint: String,
    auth: Arc<Auth>,
    shutdown: CancellationToken,
}

impl Hub {
    async fn start(dir: &Path, relay: &LocalRelay) -> Self {
        std::fs::create_dir_all(dir.join("tls")).unwrap();
        let tls = Tls::load_or_create(&dir.join("tls"), "hub").unwrap();
        let fingerprint = tls.fingerprint().to_owned();
        let auth = Arc::new(Auth::open(dir).unwrap());
        let host = Host {
            id: HostId::new("hub"),
            name: "hub".into(),
        };
        let store = HubStore::open(dir.join("hub.db")).unwrap();
        let retention = Retention::default();
        let server = Server::new(
            tls,
            Arc::clone(&auth),
            store,
            host,
            LIVENESS_TIMEOUT,
            retention,
            None,
        );
        server.use_cloudflare_relay(&relay.url, SECRET);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let shutdown = CancellationToken::new();
        tokio::spawn(server.run(vec![listener], shutdown.clone()));
        Self {
            addr,
            fingerprint,
            auth,
            shutdown,
        }
    }
}

/// A host daemon, as `herder daemon` runs, serving its data dir.
struct Daemon {
    data_dir: PathBuf,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Daemon {
    async fn start(dir: &Path) -> Self {
        let data_dir = dir.join("data");
        let config = dir.join("daemon.toml");
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            &config,
            format!(
                "listen = \"127.0.0.1:0\"\ndata_dir = {:?}\n\n[projects]\ndir = {:?}\n",
                data_dir.to_str().unwrap(),
                dir.join("Projects").to_str().unwrap()
            ),
        )
        .unwrap();
        let loaded = herder_daemon::Config::load(Some(&config)).unwrap();
        let shutdown = CancellationToken::new();
        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                let (adapters, accounts) = (Default::default(), Default::default());
                herder_daemon::serve(
                    &loaded,
                    adapters,
                    Default::default(),
                    accounts,
                    None,
                    shutdown,
                )
                .await
            }
        });
        let socket = data_dir.join(control::SOCKET);
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        while !socket.exists() {
            assert!(tokio::time::Instant::now() < deadline, "no control socket");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Self {
            data_dir,
            shutdown,
            task,
        }
    }

    /// The code that claims the daemon, the addresses it listens on and its fingerprint.
    async fn claim_code(&self) -> (String, Vec<String>, String) {
        let data_dir = self.data_dir.clone();
        let own = tokio::task::spawn_blocking(move || control::request(&data_dir, &Request::Own));
        let Response::Own(info) = own.await.unwrap().unwrap() else {
            panic!("no claim code");
        };
        (info.code, info.addresses, info.fingerprint)
    }

    async fn stop(self) {
        self.shutdown.cancel();
        self.task.await.unwrap().unwrap();
    }
}

/// Puts one session in a host's journal before its daemon starts.
fn seed(data_dir: &Path, session: &str) {
    std::fs::create_dir_all(data_dir.join("db")).unwrap();
    let mut store = Store::open(data_dir.join("db/herder.db")).unwrap();
    store
        .append(NewEvent {
            session_id: SessionId::new(session),
            at: Timestamp::now(),
            by: None,
            body: EventBody::SessionCreated {
                repo: "/home/dev/app".into(),
                worktree: "/home/dev/worktrees/s1".into(),
                branch: Some("herder/s1".into()),
                provider: Provider::Claude,
                account_id: AccountId::new("main"),
                model: "m0".into(),
                permission_mode: PermissionMode::Ask,
                parent: None,
                parent_host: None,
                task: None,
                failover_pin: None,
                chat: false,
                origin: None,
            },
        })
        .unwrap();
}

async fn machine_when(client: &Client, ready: impl Fn(&Machine) -> bool) -> Machine {
    let changes = client.changes();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        if let Some(machine) = client.machines().into_iter().find(&ready) {
            return machine;
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        // The profile is followed on a timer, which changes nothing a client is told of.
        let _ = tokio::time::timeout(left.min(Duration::from_millis(100)), changes.next()).await;
        let shown = |m: Machine| (m.name, m.addresses, m.address, m.connection);
        assert!(
            tokio::time::Instant::now() < deadline,
            "the client never saw it: {:?}",
            client.machines().into_iter().map(shown).collect::<Vec<_>>()
        );
    }
}

async fn send(client: &Client, machine: &HostId, command: CommandBody) -> CommandResult {
    tokio::time::timeout(TIMEOUT, client.send(machine.clone(), command))
        .await
        .unwrap()
        .unwrap()
}

/// The token for the relay the client saved for the hub, as JSON.
fn saved_grant(config_dir: &Path, hub: &HostId) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(config_dir.join("machines.json")).ok()?;
    let saved: serde_json::Value = serde_json::from_str(&text).ok()?;
    let machines = saved.get("machines").unwrap_or(&saved).as_array()?.clone();
    machines
        .into_iter()
        .find(|m| m["host_id"] == hub.as_str())
        .and_then(|m| m.get("relay_grant").cloned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_hub_and_its_hosts_are_reached_through_the_cloudflare_relay_alone() {
    let Some(relay) = LocalRelay::start() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let hub = Hub::start(&tmp.path().join("hub"), &relay).await;
    let host_dir = tmp.path().join("host");
    seed(&host_dir.join("data"), "s1");
    let host = Daemon::start(&host_dir).await;
    let config_dir = tmp.path().join("client");
    let client = Client::open(
        config_dir.to_str().unwrap().to_owned(),
        "herder-test".into(),
    )
    .unwrap();

    let (_, on_hub) = Join::open(&hub.auth, Role::Owner)
        .join(
            &client,
            vec![hub.addr.to_string()],
            &hub.fingerprint,
            "alice",
        )
        .await;
    let (code, addresses, fingerprint) = host.claim_code().await;
    let (_, on_host) = Join::claim(code)
        .join(&client, addresses, &fingerprint, "alice")
        .await;
    let (hub_id, host_id) = (on_hub.host_id.clone(), on_host.host_id.clone());

    // Linked to the hub, the host keeps a connection to the relay with the token the hub
    // gave it.
    let pairing = CommandBody::PairHubHost {
        host_name: "devbox".into(),
    };
    let CommandResult::HostPairing { code, .. } = send(&client, &hub_id, pairing).await else {
        panic!("no host code");
    };
    let linking = CommandBody::LinkHub {
        addresses: on_hub.addresses.clone(),
        fingerprint: on_hub.fingerprint.clone(),
        pairing_code: code,
        backup: true,
        secrets: true,
    };
    assert_eq!(
        send(&client, &host_id, linking).await,
        CommandResult::Applied
    );

    // The client got a token from the hub, and both machines have the route through the
    // relay, last.
    let route = relay.route();
    let has_route = |m: &Machine| m.addresses.last() == Some(&route);
    let on_hub = machine_when(&client, |m| m.host_id == hub_id && has_route(m)).await;
    assert_eq!(on_hub.addresses, [hub.addr.to_string(), route.clone()]);
    let on_host = machine_when(&client, |m| m.host_id == host_id && has_route(m)).await;
    assert!(on_host.addresses.contains(&format!("hub://{hub_id}")));
    let grant = saved_grant(&config_dir, &hub_id).expect("the token is saved");
    assert_eq!(grant["url"], relay.url.as_str());

    // Pinned to the relay, the host is reached through it alone and works as before.
    client
        .set_route(host_id.clone(), route.clone(), RouteChoice::Pinned)
        .unwrap();
    client.reconnect(host_id.clone()).await.unwrap();
    let relayed = machine_when(&client, |m| {
        m.host_id == host_id
            && m.connection == ConnectionState::Connected
            && m.address.as_ref() == Some(&route)
    })
    .await;
    assert!(
        relayed
            .sessions
            .iter()
            .any(|s| s.session_id.as_str() == "s1")
    );
    let journal = client
        .subscribe_session(host_id.clone(), SessionId::new("s1"))
        .unwrap();
    let events = loop {
        let update = tokio::time::timeout(TIMEOUT, journal.next()).await.unwrap();
        let events = update.unwrap().events;
        if !events.is_empty() {
            break events;
        }
    };
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(matches!(events[0].body, EventBody::SessionCreated { .. }));

    // So is the hub itself.
    client
        .set_route(hub_id.clone(), route.clone(), RouteChoice::Pinned)
        .unwrap();
    client.reconnect(hub_id.clone()).await.unwrap();
    machine_when(&client, |m| {
        m.host_id == hub_id
            && m.connection == ConnectionState::Connected
            && m.address.as_ref() == Some(&route)
    })
    .await;

    drop(client);
    host.stop().await;
    hub.shutdown.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_token_half_used_is_renewed_once_connected_to_the_hub() {
    let Some(relay) = LocalRelay::start() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let hub = Hub::start(&tmp.path().join("hub"), &relay).await;
    let config_dir = tmp.path().join("client");
    let client = Client::open(
        config_dir.to_str().unwrap().to_owned(),
        "herder-test".into(),
    )
    .unwrap();
    let (_, on_hub) = Join::open(&hub.auth, Role::Owner)
        .join(
            &client,
            vec![hub.addr.to_string()],
            &hub.fingerprint,
            "alice",
        )
        .await;
    let hub_id = on_hub.host_id;
    let route = relay.route();
    machine_when(&client, |m| {
        m.host_id == hub_id && m.addresses.contains(&route)
    })
    .await;
    drop(client);

    // A token with an hour left, as after a day away.
    let path = config_dir.join("machines.json");
    let text = std::fs::read_to_string(&path).unwrap();
    let saved = saved_grant(&config_dir, &hub_id).unwrap();
    let old = saved["expires_at"].as_str().unwrap().to_owned();
    let soon = Timestamp::from_second(Timestamp::now().as_second() + 3600)
        .unwrap()
        .to_string();
    std::fs::write(&path, text.replace(&old, &soon)).unwrap();

    let client = Client::open(
        config_dir.to_str().unwrap().to_owned(),
        "herder-test".into(),
    )
    .unwrap();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let grant = saved_grant(&config_dir, &hub_id).unwrap();
        let expires: Timestamp = grant["expires_at"].as_str().unwrap().parse().unwrap();
        if expires.as_second() > Timestamp::now().as_second() + 20 * 3600 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the token was not renewed"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    drop(client);
    hub.shutdown.cancel();
}
