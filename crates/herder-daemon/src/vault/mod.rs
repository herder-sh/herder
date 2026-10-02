//! The vault: the same daemon in `mode = "vault"`, keeping durable copies of every host's
//! session journals, and the host side that streams them there.
//!
//! The vault runs no sessions. It listens for hosts on the daemon's port with the daemon's TLS
//! identity, and pairs them as devices with `herder pair`, as clients pair. A device that has
//! paired replicates as the host its first hello names, and only as that host. Every host's
//! journals and fleet index go into one database, `<data_dir>/db/vault.db` ([`VaultStore`]).
//!
//! A host whose config has a `[vault]` table runs a [`Replicator`], which streams every
//! session's journal there under [`herder_protocol::replication`], resuming from the cursors
//! in the vault's hello.

mod conn;
mod replicator;
mod store;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

pub use replicator::{Replicator, WakeOnEvent};
pub use store::{Outcome, VaultStore};

use crate::auth::{self, Auth};
use crate::ws::Tls;
use crate::{Config, DataDir};

/// Build name and version, sent in hellos.
pub(crate) const BUILD: &str = concat!("herder/", env!("CARGO_PKG_VERSION"));

/// The vault's server for hosts.
pub struct Server {
    shared: Arc<Shared>,
}

struct Shared {
    tls: Tls,
    auth: Arc<Auth>,
    store: Arc<Mutex<VaultStore>>,
}

impl Server {
    /// A server keeping what hosts send in `store`; `auth` decides which devices may connect.
    pub fn new(tls: Tls, auth: Arc<Auth>, store: VaultStore) -> Self {
        Self {
            shared: Arc::new(Shared {
                tls,
                auth,
                store: Arc::new(Mutex::new(store)),
            }),
        }
    }

    /// The store, for reading what hosts replicated.
    pub fn store(&self) -> Arc<Mutex<VaultStore>> {
        Arc::clone(&self.shared.store)
    }

    /// Accepts hosts on `listener` until `shutdown`, which also closes every connection.
    pub async fn run(self, listener: TcpListener, shutdown: CancellationToken) {
        loop {
            let accepted = tokio::select! {
                () = shutdown.cancelled() => break,
                accepted = listener.accept() => accepted,
            };
            match accepted {
                Ok((stream, peer)) => {
                    debug!(%peer, "host connection accepted");
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
    }
}

/// Runs the vault on `config`'s data dir and port until `shutdown`.
pub async fn serve(config: &Config, shutdown: CancellationToken) -> Result<()> {
    let data_dir = DataDir::open(&config.data_dir)?;
    let tls = Tls::load_or_create(&data_dir.root().join("tls"), &crate::host_name())?;
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
    Server::new(tls, auth, store).run(listener, shutdown).await;
    info!("herder vault stopped");
    Ok(())
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
