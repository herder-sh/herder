//! The vault: the same daemon in `mode = "vault"`, keeping durable copies of every host's
//! session journals, and the host side that streams them there.
//!
//! The vault runs no sessions. It listens for hosts and clients on the daemon's port with the
//! daemon's TLS identity, and pairs both as devices with `herder pair`. The first message
//! tells them apart: a host says a replication hello, a client the client protocol's. A
//! device that has paired replicates as the host its first hello names, and only as that
//! host. Every host's journals and fleet index go into one database, `<data_dir>/db/vault.db`
//! ([`VaultStore`]). Clients get every replicated session, read-only ([`fleet`]).
//!
//! A host is online while its replication connection is open. The host pings an idle
//! connection, so one silent for [`LIVENESS_TIMEOUT`] is taken for a host that is gone and
//! closed.
//!
//! A host whose config has a `[vault]` table runs a [`Replicator`], which streams every
//! session's journal there under [`herder_protocol::replication`], resuming from the cursors
//! in the vault's hello.

mod conn;
mod fleet;
mod replicator;
mod store;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result};
use herder_protocol::HostId;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

pub use replicator::{Replicator, WakeOnEvent};
pub use store::{HostRecord, Outcome, VaultStore};

use crate::auth::{self, Auth};
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
}

impl Server {
    /// A server keeping what hosts send in `store` and showing it to clients as the vault on
    /// `host`; `auth` decides which devices may connect, and a host silent for `liveness` is
    /// disconnected.
    pub fn new(
        tls: Tls,
        auth: Arc<Auth>,
        store: VaultStore,
        host: Host,
        liveness: Duration,
    ) -> Self {
        let store = Arc::new(Mutex::new(store));
        let hub = Arc::new(Hub::default());
        let fleet = Fleet::new(Arc::clone(&store), Arc::clone(&hub));
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
            }),
        }
    }

    /// The store, for reading what hosts replicated.
    pub fn store(&self) -> Arc<Mutex<VaultStore>> {
        Arc::clone(&self.shared.store)
    }

    /// Accepts hosts and clients on `listener` until `shutdown`, which also closes every
    /// connection.
    pub async fn run(self, listener: TcpListener, shutdown: CancellationToken) {
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
    let listener = TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("listening on {}", config.listen))?;
    let control = auth::control::bind(data_dir.root())?;
    tokio::spawn(auth::control::serve(
        control,
        Arc::clone(&auth),
        auth::control::Daemon {
            fingerprint: tls.fingerprint().to_owned(),
            listen: listener.local_addr()?,
        },
        shutdown.clone(),
    ));
    info!(
        data_dir = %data_dir.root().display(),
        listen = %listener.local_addr()?,
        tls_fingerprint = tls.fingerprint(),
        "herder vault started"
    );
    Server::new(tls, auth, store, host, LIVENESS_TIMEOUT)
        .run(listener, shutdown)
        .await;
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
            resources: Default::default(),
            projects: Default::default(),
            mode: crate::config::Mode::Vault,
            vault: None,
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
