//! The vault: the same daemon in `mode = "vault"`, keeping durable copies of every host's
//! session journals, and the host side that streams them there.
//!
//! The vault runs no sessions. It listens for hosts and clients on the daemon's port with the
//! daemon's TLS identity, and pairs both as devices: hosts with `herder pair --host`, clients
//! with `herder pair`. The first message tells them apart: a host says a replication hello, a
//! client the client protocol's. A device that has paired replicates as the host its first
//! hello names, and only as that host. A host device reads nothing: it is refused as a client
//! ([`auth::DeviceRole`]), so all it sees is its own replication cursors. Every host's journals and fleet index go into one database, `<data_dir>/db/vault.db`
//! ([`VaultStore`]). Clients get every replicated session, read-only ([`fleet`]).
//!
//! A host is online while its replication connection is open. The host pings an idle
//! connection, so one silent for [`LIVENESS_TIMEOUT`] is taken for a host that is gone and
//! closed.
//!
//! A host whose config has a `[vault]` table runs a [`Replicator`], which streams every
//! session's journal there under [`herder_protocol::replication`], resuming from the cursors
//! in the vault's hello. An owner links a host to a vault, or unlinks it, from a client while
//! the host runs ([`Link`]).
//!
//! Another host can fork any session the vault holds, whether its host is up or gone
//! ([`fork`]); the fork is a new session of that host. A host that replicates a session id
//! another host replicated before takes it over: the vault then shows the session on that
//! host, and the old host makes its copy read-only.
//!
//! The vault keeps a host's images only up to the cap its hello gives, and drops archived
//! sessions after its retention period; a host and its sessions go only when an owner forgets
//! it ([`retention`]).

mod client;
mod conn;
mod fleet;
pub mod fork;
mod link;
mod replicator;
mod retention;
mod store;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result};
use herder_protocol::HostId;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

pub use link::{Link, Setup as LinkSetup};
pub use replicator::{Replicator, WakeOnEvent};
pub use retention::{Admin, Forgot, PRUNE_EVERY};
pub use store::{HostRecord, Kept, Outcome, VaultStore};

use crate::auth::{self, Auth};
use crate::config::Retention;
use crate::hub::Hub;
use crate::login::Logins;
use crate::terminal::{self, Terminals};
use crate::ws::{self, Host, Tls};
use crate::{Config, DataDir};
use fleet::Fleet;

/// Build name and version, sent in hellos.
pub(crate) const BUILD: &str = concat!("herder/", env!("CARGO_PKG_VERSION"));

/// Silence after which a host's connection is closed and the host shown offline: three of
/// the host's keepalive pings.
pub const LIVENESS_TIMEOUT: Duration = Duration::from_secs(60);

/// The vault's server for hosts and clients.
pub struct Server {
    shared: Arc<Shared>,
}

struct Shared {
    tls: Tls,
    auth: Arc<Auth>,
    store: Arc<Mutex<VaultStore>>,
    hub: Arc<Hub>,
    fleet: Fleet,
    clients: ws::Server<Fleet>,
    /// Silence after which a host is taken for gone; [`LIVENESS_TIMEOUT`] but in tests.
    liveness: Duration,
    /// How long archived sessions are kept.
    retention: Retention,
    /// Hosts whose copy of a session another host just took over; their connections drop.
    superseded: tokio::sync::broadcast::Sender<HostId>,
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
    /// showing it to clients as the vault on `host`; `auth` decides which devices may
    /// connect, and a host silent for `liveness` is disconnected.
    pub fn new(
        tls: Tls,
        auth: Arc<Auth>,
        store: VaultStore,
        host: Host,
        liveness: Duration,
        retention: Retention,
    ) -> Self {
        let store = Arc::new(Mutex::new(store));
        let hub = Arc::new(Hub::default());
        let fleet = Fleet::new(Arc::clone(&store), Arc::clone(&hub), Arc::clone(&auth));
        // No session has a worktree here, so no terminal ever opens.
        let terminals = Terminals::new(Arc::clone(&hub), terminal::login_shell());
        let clients = ws::Server::new(
            tls.clone(),
            Arc::clone(&auth),
            Arc::clone(&hub),
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
                hub,
                fleet,
                clients,
                liveness,
                retention,
                superseded: tokio::sync::broadcast::channel(16).0,
            }),
        }
    }

    /// The store, for reading what hosts replicated.
    pub fn store(&self) -> Arc<Mutex<VaultStore>> {
        Arc::clone(&self.shared.store)
    }

    /// What the control socket does on the vault.
    pub fn admin(&self) -> Admin {
        Admin {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Accepts hosts and clients on `listener`, and prunes archived sessions, until
    /// `shutdown`, which also closes every connection.
    pub async fn run(self, listener: TcpListener, shutdown: CancellationToken) {
        // Every host is offline until it connects.
        self.shared.fleet.refresh_hosts().await;
        tokio::spawn(retention::run(
            Arc::clone(&self.shared),
            shutdown.child_token(),
        ));
        tokio::spawn({
            let fleet = self.shared.fleet.clone();
            let shutdown = shutdown.clone();
            async move { fleet.publish_status(shutdown).await }
        });
        let flusher = tokio::spawn({
            let hub = Arc::clone(&self.shared.hub);
            let shutdown = shutdown.clone();
            async move { hub.run_flusher(shutdown).await }
        });
        loop {
            let accepted = tokio::select! {
                () = shutdown.cancelled() => break,
                accepted = listener.accept() => accepted,
            };
            match accepted {
                Ok((stream, peer)) => {
                    debug!(%peer, "connection accepted");
                    let shared = Arc::clone(&self.shared);
                    tokio::spawn(conn::run(stream, peer, shared, shutdown.child_token()));
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

/// Runs the vault on `config`'s data dir and port until `shutdown`.
pub async fn serve(config: &Config, shutdown: CancellationToken) -> Result<()> {
    let data_dir = DataDir::open(&config.data_dir)?;
    let host = Host {
        id: HostId::new(data_dir.host_id().to_string()),
        name: crate::host_name(),
    };
    let tls = Tls::load_or_create(&data_dir.root().join("tls"), &host.name)?;
    let auth = Arc::new(Auth::open(data_dir.root())?);
    let store_path = data_dir.root().join("db/vault.db");
    let store = VaultStore::open(&store_path)
        .with_context(|| format!("opening the vault database {}", store_path.display()))?;
    let demoted = auth.migrate_device_roles(&store.host_devices()?)?;
    if !demoted.is_empty() {
        info!(
            devices = demoted.len(),
            "devices that replicated as hosts made host-only"
        );
    }
    let listener = TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("listening on {}", config.listen))?;
    let fingerprint = tls.fingerprint().to_owned();
    let server = Server::new(
        tls,
        Arc::clone(&auth),
        store,
        host,
        LIVENESS_TIMEOUT,
        config.retention,
    );
    let control = auth::control::bind(data_dir.root())?;
    tokio::spawn(auth::control::serve(
        control,
        Arc::clone(&auth),
        auth::control::Daemon {
            fingerprint: fingerprint.clone(),
            listen: listener.local_addr()?,
            sessions: None,
            vault: Some(server.admin()),
        },
        shutdown.clone(),
    ));
    info!(
        data_dir = %data_dir.root().display(),
        listen = %listener.local_addr()?,
        tls_fingerprint = fingerprint,
        archive_retention_days = config.retention.archive_days,
        "herder vault started"
    );
    server.run(listener, shutdown).await;
    info!("herder vault stopped");
    Ok(())
}

/// Runs `call` on the store on the blocking pool; a malformed batch comes back as
/// [`store::BadBatch`].
async fn blocking<T: Send + 'static>(
    store: &Arc<Mutex<VaultStore>>,
    call: impl FnOnce(&mut VaultStore) -> store::Result<T> + Send + 'static,
) -> Result<T> {
    let store = Arc::clone(store);
    tokio::task::spawn_blocking(move || {
        // Every write is one transaction, so a poisoned store is consistent.
        let mut store = store.lock().unwrap_or_else(PoisonError::into_inner);
        call(&mut store).map_err(|err| match err {
            store::Error::BadBatch(bad) => anyhow::Error::new(bad),
            err => anyhow::Error::new(err).context("the vault database failed"),
        })
    })
    .await
    .context("the vault store task panicked")?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn serve_opens_the_vault_and_returns_once_shutdown_is_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Config {
            path: tmp.path().join("daemon.toml"),
            listen: "127.0.0.1:0".parse().unwrap(),
            data_dir: tmp.path().join("data"),
            log: Default::default(),
            accounts: Default::default(),
            binaries: Default::default(),
            tasks: Default::default(),
            failover: Default::default(),
            titles: Default::default(),
            resources: Default::default(),
            projects: Default::default(),
            mode: crate::config::Mode::Vault,
            vault: None,
            retention: Default::default(),
        };
        let shutdown = CancellationToken::new();
        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            async move { serve(&config, shutdown).await }
        });
        shutdown.cancel();
        task.await.unwrap().unwrap();
        assert!(tmp.path().join("data/db/vault.db").is_file());
        assert!(tmp.path().join("data/tls/cert.pem").is_file());
        assert!(tmp.path().join("data/control.sock").exists());
        // A vault runs no sessions, so it has no journal.
        assert!(!tmp.path().join("data/db/herder.db").exists());
    }
}
