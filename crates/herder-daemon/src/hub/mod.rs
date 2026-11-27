//! The hub: the same daemon in `mode = "hub"`, keeping durable copies of every host's
//! session journals, and the host side that streams them there.
//!
//! The hub runs no sessions. It listens for hosts and clients on the daemon's port with the
//! daemon's TLS identity, and pairs both as devices: hosts with `herder pair --host`, clients
//! with `herder pair`. The first message tells them apart: a host says a replication hello, a
//! client the client protocol's. A device that has paired replicates as the host its first
//! hello names, and only as that host. A host device reads nothing: it is refused as a client
//! ([`auth::DeviceRole`]), so all it sees is its own replication cursors. Every host's journals and fleet index go into one database, `<data_dir>/db/hub.db`
//! ([`HubStore`]). Clients get every replicated session, read-only ([`fleet`]).
//!
//! A host is online while its replication connection is open. The host pings an idle
//! connection, so one silent for [`LIVENESS_TIMEOUT`] is taken for a host that is gone and
//! closed.
//!
//! The hub also forwards: a client paired with it reaches a host that replicates to it
//! through a relay stream, whose bytes the hub copies and cannot read ([`relay`]). With
//! `tunnel` set it takes the connections `cloudflared` delivers, as a daemon does, so one
//! public hostname reaches the hub and, through it, every host. The hub can set that
//! hostname up in Cloudflare and run `cloudflared` for it itself ([`cloudflare`]).
//!
//! A host whose config has a `[hub]` table runs a [`Replicator`], which streams every
//! session's journal there under [`herder_protocol::replication`], resuming from the cursors
//! in the hub's hello. An owner links a host to a hub, or unlinks it, from a client while
//! the host runs ([`Link`]).
//!
//! A person can fork any session the hub holds onto another host, whether its host is up
//! or gone: their client reads it from the hub and relays it there
//! ([`crate::session::fork`]). A host never reads other hosts' sessions. A host that replicates a session id
//! another host replicated before takes it over: the hub then shows the session on that
//! host, and the old host makes its copy read-only.
//!
//! The hub keeps a host's images only up to the cap its hello gives, and drops archived
//! sessions after its retention period; a host and its sessions go only when an owner forgets
//! it ([`retention`]).
//!
//! The hub keeps secrets too, encrypted under its master key, in sets its owners grant to
//! hosts ([`SecretsKey`]). A host fetches their values over its replication connection, for its
//! sessions or for its own use, and keeps them in memory only ([`HostSecrets`]).

mod client;
pub mod cliproxyapi;
pub mod cloudflare;
mod conn;
mod fleet;
mod host_secrets;
pub mod install;
mod link;
pub mod pin;
mod relay;
pub mod remote_access;
mod replicator;
mod retention;
mod secrets;
mod store;
mod upgrade;

use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result};
use herder_protocol::HostId;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

pub use cliproxyapi::ClientKeys;
pub use host_secrets::{CACHE_TTL as SECRETS_CACHE_TTL, Held, HeldSecrets, HostSecrets};
pub use link::{Link, Setup as LinkSetup};
pub use replicator::{Replicator, WakeOnEvent};
pub use retention::{Admin, Forgot, PRUNE_EVERY};
pub use secrets::{KEY_ENV as SECRETS_KEY_ENV, SecretsKey};
pub use store::{HostRecord, HubStore, Kept, Outcome};

use crate::auth::{self, Auth};
use crate::config::Retention;
use crate::fanout::Fanout;
use crate::listen;
use crate::login::Logins;
use crate::terminal::{self, Terminals};
use crate::ws::{self, Host, Tls, tunnel};
use crate::{Config, DataDir};
use fleet::Fleet;

/// Build name and version, sent in hellos.
pub(crate) const BUILD: &str = concat!("herder/", env!("CARGO_PKG_VERSION"));

/// Silence after which a host's connection is closed and the host shown offline: three of
/// the host's keepalive pings.
pub const LIVENESS_TIMEOUT: Duration = Duration::from_secs(60);

/// The hub's server for hosts and clients.
pub struct Server {
    shared: Arc<Shared>,
}

struct Shared {
    tls: Tls,
    auth: Arc<Auth>,
    store: Arc<Mutex<HubStore>>,
    fanout: Arc<Fanout>,
    fleet: Fleet,
    clients: ws::Server<Fleet>,
    /// Silence after which a host is taken for gone; [`LIVENESS_TIMEOUT`] but in tests.
    liveness: Duration,
    /// How long archived sessions are kept.
    retention: Retention,
    /// Hosts whose copy of a session another host just took over; their connections drop.
    superseded: tokio::sync::broadcast::Sender<HostId>,
    /// Encrypts secrets' values; without it the hub keeps no secrets.
    secrets_key: Option<Arc<SecretsKey>>,
    /// The CLIProxyAPI hosts get client keys for, when the hub has one and keeps secrets.
    cliproxyapi: OnceLock<Arc<ClientKeys>>,
    /// The relay streams between clients and hosts.
    relays: Arc<relay::Relays>,
    /// Count the connections not authenticated yet.
    gates: tunnel::Gates,
}

impl Shared {
    /// Drops the connections of `hosts`, whose copies of a session another host took over,
    /// so they reconnect and stop the session.
    fn supersede(&self, hosts: Vec<HostId>) {
        for host in hosts {
            info!(host_id = %host, "another host took over a session of this host");
            // No receiver means the host is not connected.
            let _ = self.superseded.send(host);
        }
    }
}

impl Server {
    /// A server keeping what hosts send in `store` for as long as `retention` says, and
    /// showing it to clients as the hub on `host`; `auth` decides which devices may
    /// connect, and a host silent for `liveness` is disconnected. Secrets are kept under
    /// `secrets_key`, and refused without one.
    pub fn new(
        tls: Tls,
        auth: Arc<Auth>,
        store: HubStore,
        host: Host,
        liveness: Duration,
        retention: Retention,
        secrets_key: Option<SecretsKey>,
    ) -> Self {
        let store = Arc::new(Mutex::new(store));
        let fanout = Arc::new(Fanout::default());
        let secrets_key = secrets_key.map(Arc::new);
        let relays = Arc::new(relay::Relays::default());
        let fleet = Fleet::new(
            Arc::clone(&store),
            Arc::clone(&fanout),
            Arc::clone(&auth),
            secrets_key.clone(),
            Arc::clone(&relays),
        );
        // No session has a worktree here, so no terminal ever opens.
        let terminals = Terminals::new(Arc::clone(&fanout), terminal::login_shell());
        let clients = ws::Server::new(
            tls.clone(),
            Arc::clone(&auth),
            Arc::clone(&fanout),
            fleet.clone(),
            terminals,
            Logins::default(),
            host,
        );
        Self {
            shared: Arc::new(Shared {
                tls,
                auth,
                store,
                fanout,
                fleet,
                clients,
                liveness,
                retention,
                superseded: tokio::sync::broadcast::channel(16).0,
                secrets_key,
                cliproxyapi: OnceLock::new(),
                relays,
                gates: tunnel::Gates::new(),
            }),
        }
    }

    /// Answers clients' settings commands with `settings`; once per server.
    pub fn manage_settings(&self, settings: Arc<crate::settings::Settings>) -> Result<()> {
        self.shared.clients.manage_settings(settings)
    }

    /// Keeps a client key per host in the CLIProxyAPI `keys` manages, from [`Self::run`]
    /// on; once per server. Refused without a secrets key, as the keys are secrets.
    pub fn use_cliproxyapi(&self, keys: ClientKeys) -> Result<()> {
        anyhow::ensure!(
            self.shared.secrets_key.is_some(),
            "a hub with a CLIProxyAPI keeps its hosts' client keys as secrets: set \
             {SECRETS_KEY_ENV} (see deploy/README.md)"
        );
        self.shared.fleet.use_cliproxyapi(keys.management().clone());
        self.shared
            .cliproxyapi
            .set(Arc::new(keys))
            .map_err(|_| anyhow::anyhow!("the hub uses one CLIProxyAPI"))
    }

    /// Answers owners' remote access commands, running `tunnel`, with the settings kept in
    /// `data_dir`; once per server.
    pub fn use_remote_access(
        &self,
        data_dir: &std::path::Path,
        tunnel: Arc<cloudflare::ManagedTunnel>,
    ) -> Result<Arc<remote_access::RemoteAccessHub>> {
        let remote_access = remote_access::RemoteAccessHub::open(
            data_dir,
            Arc::clone(&self.shared.store),
            self.shared.secrets_key.clone(),
            tunnel,
        )?;
        self.shared
            .fleet
            .use_remote_access(Arc::clone(&remote_access));
        Ok(remote_access)
    }

    /// The store, for reading what hosts replicated.
    pub fn store(&self) -> Arc<Mutex<HubStore>> {
        Arc::clone(&self.shared.store)
    }

    /// What the control socket does on the hub.
    pub fn admin(&self) -> Admin {
        Admin {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Accepts hosts and clients on every one of `listeners`, and prunes archived sessions,
    /// until `shutdown`, which also closes every connection.
    pub async fn run(self, listeners: Vec<TcpListener>, shutdown: CancellationToken) {
        self.run_tunneled(listeners, None, shutdown).await;
    }

    /// [`Server::run`], also accepting the connections `cloudflared` delivers on `tunnel`.
    pub async fn run_tunneled(
        self,
        listeners: Vec<TcpListener>,
        tunnel: Option<TcpListener>,
        shutdown: CancellationToken,
    ) {
        match listen::local_addrs(&listeners) {
            Ok(listen) => self.shared.clients.listening_on(listen),
            Err(err) => warn!("cannot tell the addresses the hub listens on: {err}"),
        }
        // Every host is offline until it connects.
        self.shared.fleet.refresh_hosts().await;
        tokio::spawn(retention::run(
            Arc::clone(&self.shared),
            shutdown.child_token(),
        ));
        if let (Some(keys), Some(key)) = (self.shared.cliproxyapi.get(), &self.shared.secrets_key) {
            tokio::spawn(Arc::clone(keys).run(
                Arc::clone(&self.shared.store),
                Arc::clone(key),
                cliproxyapi::SYNC_EVERY,
                shutdown.child_token(),
            ));
        }
        tokio::spawn({
            let fleet = self.shared.fleet.clone();
            let shutdown = shutdown.clone();
            async move { fleet.publish_status(shutdown).await }
        });
        let flusher = tokio::spawn({
            let fanout = Arc::clone(&self.shared.fanout);
            let shutdown = shutdown.clone();
            async move { fanout.run_flusher(shutdown).await }
        });
        let mut acceptor = listen::Acceptor::default();
        loop {
            let (accepted, tunneled) = tokio::select! {
                () = shutdown.cancelled() => break,
                accepted = acceptor.accept(&listeners) => (accepted, false),
                accepted = listen::accept_tunneled(tunnel.as_ref()) => (accepted, true),
            };
            match accepted {
                Ok((stream, peer)) => {
                    debug!(%peer, tunneled, "connection accepted");
                    let shared = Arc::clone(&self.shared);
                    let cancel = shutdown.child_token();
                    tokio::spawn(conn::run(stream, peer, tunneled, shared, cancel));
                }
                Err(err) => {
                    // Usually out of file descriptors; back off instead of spinning.
                    warn!("cannot accept a connection: {err}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
        let _ = flusher.await;
    }
}

/// Runs the hub on `config`'s data dir and port until `shutdown`.
pub async fn serve(config: &Config, shutdown: CancellationToken) -> Result<()> {
    let secrets_key = SecretsKey::from_env()?;
    let cloudflare = Cloudflare {
        api: cloudflare::API,
        cloudflared: cloudflare::cloudflared::download,
    };
    serve_with(config, secrets_key, cloudflare, shutdown).await
}

/// Where the hub reaches Cloudflare: its API, and the `cloudflared` it runs.
struct Cloudflare<'a> {
    api: &'a str,
    cloudflared: cloudflare::cloudflared::Download,
}

/// [`serve`], keeping secrets under `secrets_key` and reaching `cloudflare`.
async fn serve_with(
    config: &Config,
    secrets_key: Option<SecretsKey>,
    cloudflare: Cloudflare<'_>,
    shutdown: CancellationToken,
) -> Result<()> {
    let data_dir = DataDir::open(&config.data_dir)?;
    let host = Host {
        id: HostId::new(data_dir.host_id().to_string()),
        name: crate::host_name(),
    };
    let tls = Tls::load_or_create(&data_dir.root().join("tls"), &host.name)?;
    let auth = Arc::new(Auth::open(data_dir.root())?);
    let store_path = data_dir.root().join("db/hub.db");
    let store = HubStore::open(&store_path)
        .with_context(|| format!("opening the hub database {}", store_path.display()))?;
    if secrets_key.is_none() {
        info!("no secrets key in {SECRETS_KEY_ENV}: this hub keeps no secrets");
    }
    let demoted = auth.migrate_device_roles(&store.host_devices()?)?;
    if !demoted.is_empty() {
        info!(
            devices = demoted.len(),
            "devices that replicated as hosts made host-only"
        );
    }
    let listeners = listen::bind(&config.listen).await?;
    let listen = listen::local_addrs(&listeners)?;
    let tunnel = listen::bind_tunnel(config.tunnel).await?;
    let fingerprint = tls.fingerprint().to_owned();
    let server = Server::new(
        tls,
        Arc::clone(&auth),
        store,
        host,
        LIVENESS_TIMEOUT,
        config.retention,
        secrets_key,
    );
    if let Some(url) = &config.cliproxyapi {
        let management = cliproxyapi::Management::from_env(url)?;
        let endpoint = config.cliproxyapi_endpoint.clone();
        server.use_cliproxyapi(ClientKeys::new(management, endpoint)?)?;
        info!(
            cliproxyapi = url,
            endpoint = config.cliproxyapi_endpoint.as_deref().unwrap_or("not set"),
            "hosts get client keys for this CLIProxyAPI"
        );
    }
    let settings = Arc::new(crate::settings::Settings::new(
        config,
        None,
        shutdown.clone(),
    ));
    let public = settings.public_addresses();
    server.manage_settings(settings)?;
    let managed = cloudflare::ManagedTunnel::open(data_dir.root(), cloudflare.api, config.tunnel)?;
    managed
        .start(
            Arc::clone(&public),
            cloudflare.cloudflared,
            shutdown.clone(),
        )
        .await;
    let remote_access = server.use_remote_access(data_dir.root(), managed)?;
    remote_access.start().await;
    let control = auth::control::bind(data_dir.root())?;
    tokio::spawn(auth::control::serve(
        control,
        Arc::clone(&auth),
        auth::control::Daemon {
            fingerprint: fingerprint.clone(),
            listen: listen.clone(),
            public,
            hub: Some(server.admin()),
            remote_access: Some(remote_access),
        },
        shutdown.clone(),
    ));
    info!(
        data_dir = %data_dir.root().display(),
        listen = ?listen,
        tunnel = ?config.tunnel,
        tls_fingerprint = fingerprint,
        archive_retention_days = config.retention.archive_days,
        "herder hub started"
    );
    server.run_tunneled(listeners, tunnel, shutdown).await;
    info!("herder hub stopped");
    Ok(())
}

/// Runs `call` on the store on the blocking pool; a malformed batch comes back as
/// [`store::BadBatch`].
async fn blocking<T: Send + 'static>(
    store: &Arc<Mutex<HubStore>>,
    call: impl FnOnce(&mut HubStore) -> store::Result<T> + Send + 'static,
) -> Result<T> {
    let store = Arc::clone(store);
    tokio::task::spawn_blocking(move || {
        // Every write is one transaction, so a poisoned store is consistent.
        let mut store = store.lock().unwrap_or_else(PoisonError::into_inner);
        call(&mut store).map_err(|err| match err {
            store::Error::BadBatch(bad) => anyhow::Error::new(bad),
            err => anyhow::Error::new(err).context("the hub database failed"),
        })
    })
    .await
    .context("the hub store task panicked")?
}

#[cfg(test)]
mod tests {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    use super::*;

    /// A hub's config in `tmp`, taking tunneled connections at `tunnel`.
    fn config(tmp: &std::path::Path, tunnel: Option<std::net::SocketAddr>) -> Config {
        Config {
            path: tmp.join("daemon.toml"),
            listen: vec!["127.0.0.1:0".parse().unwrap()],
            tunnel,
            public_addresses: Vec::new(),
            data_dir: tmp.join("data"),
            log: Default::default(),
            accounts: Default::default(),
            binaries: Default::default(),
            clis: Default::default(),
            failover: Default::default(),
            titles: Default::default(),
            follow_ups: Default::default(),
            resources: Default::default(),
            projects: Default::default(),
            mode: crate::config::Mode::Hub,
            hub: None,
            retention: Default::default(),
            cliproxyapi: None,
            cliproxyapi_endpoint: None,
            mcp_servers: Vec::new(),
            bots: Vec::new(),
            self_update: None,
        }
    }

    #[tokio::test]
    async fn serve_opens_the_hub_and_returns_once_shutdown_is_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path(), None);
        let shutdown = CancellationToken::new();
        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            async move { serve(&config, shutdown).await }
        });
        shutdown.cancel();
        task.await.unwrap().unwrap();
        assert!(tmp.path().join("data/db/hub.db").is_file());
        assert!(tmp.path().join("data/tls/cert.pem").is_file());
        assert!(tmp.path().join("data/control.sock").exists());
        // A hub runs no sessions, so it has no journal.
        assert!(!tmp.path().join("data/db/herder.db").exists());
    }

    /// Sends `request` to the hub on `data_dir`'s control socket, once it is up.
    async fn control(
        data_dir: std::path::PathBuf,
        request: auth::control::Request,
    ) -> auth::control::Response {
        tokio::task::spawn_blocking(move || {
            for _ in 0..200 {
                if let Ok(response) = auth::control::request(&data_dir, &request) {
                    return response;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            panic!("the hub's control socket never answered");
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn the_hub_sets_up_its_tunnel_runs_cloudflared_and_is_reached_through_the_route() {
        use auth::control::{Request, Response};
        use cloudflare::fake::FakeCloudflare;
        use herder_protocol::RouteState;

        let fake = FakeCloudflare::start();
        let tmp = tempfile::tempdir().unwrap();
        let tunnel = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let config = config(tmp.path(), Some(tunnel));
        let data_dir = config.data_dir.clone();
        let shutdown = CancellationToken::new();
        let task = tokio::spawn({
            let (shutdown, api) = (shutdown.clone(), fake.url.clone());
            async move {
                let cloudflare = Cloudflare {
                    api: &api,
                    cloudflared: cloudflare::tunnel::tests::fake_cloudflared,
                };
                let key = SecretsKey::new(&[7; 32]);
                serve_with(&config, Some(key), cloudflare, shutdown).await
            }
        });

        // As `herder hub tunnel set-up` asks, the token put as a hub secret.
        let set_up = Request::TunnelSetUp {
            account_id: Some(cloudflare::fake::ACCOUNT.into()),
            hostname: "https://hub.example.com".into(),
            api_token: herder_protocol::SecretValue::new(cloudflare::fake::TOKEN),
        };
        for _ in 0..200 {
            if data_dir.join("control.sock").exists() || task.is_finished() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(!task.is_finished(), "{:?}", task.await);
        let Response::Tunnel {
            tunnel: Some(mut status),
        } = control(data_dir.clone(), set_up).await
        else {
            panic!("the tunnel was not set up");
        };
        assert_eq!(status.url, "wss://hub.example.com");
        for _ in 0..400 {
            if status.state == RouteState::Up {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
            let Response::Tunnel { tunnel: Some(now) } =
                control(data_dir.clone(), Request::TunnelStatus).await
            else {
                panic!("no tunnel");
            };
            status = now;
        }
        assert_eq!(status.state, RouteState::Up, "{status:?}");

        // The fake edge: what Cloudflare routes hub.example.com to, as an edge and
        // cloudflared would deliver a client's WebSocket there.
        let route = fake.held().route("hub.example.com").unwrap();
        let address = route.strip_prefix("http://").unwrap().to_owned();
        let stream = tokio::net::TcpStream::connect(&address).await.unwrap();
        let request = format!("ws://{address}/").into_client_request().unwrap();
        let (_ws, response) = tokio_tungstenite::client_async(request, stream)
            .await
            .unwrap();
        assert_eq!(response.status(), 101);

        // The hub runs cloudflared with the tunnel's token, and names the hostname to devices.
        let token = data_dir.join("cloudflare/cloudflared.token");
        for _ in 0..400 {
            if std::fs::read_to_string(&token).is_ok_and(|text| !text.is_empty()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(
            std::fs::read_to_string(&token).unwrap().trim(),
            "token-of-tunnel1"
        );
        let Response::Address { addresses, .. } = control(data_dir.clone(), Request::Address).await
        else {
            panic!("no address");
        };
        assert!(
            addresses.contains(&"wss://hub.example.com".to_owned()),
            "{addresses:?}"
        );

        // Removed, and gone in Cloudflare.
        let Response::Tunnel { .. } = control(data_dir.clone(), Request::TunnelRemove).await else {
            panic!("the tunnel was not removed");
        };
        for _ in 0..400 {
            if fake.held().route("hub.example.com").is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(fake.held().route("hub.example.com"), None);

        shutdown.cancel();
        task.await.unwrap().unwrap();
    }
}
