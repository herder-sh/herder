//! `fake_daemon --vault`: a vault that keeps secrets, for the apps' secrets screen and its UI
//! test. Two hosts, `devbox` and `laptop`, have replicated to it and are offline; it holds no
//! sessions, sets or secrets until a client adds them.

use std::sync::Arc;

use anyhow::Result;
use herder_client_core::PairingUri;
use herder_daemon::auth::{Auth, PAIRING_TTL};
use herder_daemon::config::Retention;
use herder_daemon::vault::{LIVENESS_TIMEOUT, SecretsKey, Server, VaultStore};
use herder_daemon::ws::{Host, Tls};
use herder_protocol::{DeviceId, HostId};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// The hosts that replicated to the vault, as ids and names.
pub const HOSTS: [(&str, &str); 2] = [("devbox", "devbox"), ("laptop", "laptop")];

/// A running vault; [`Vault::stop`] stops it and removes everything it created.
pub struct Vault {
    /// A pairing link with a fresh code, for its owner.
    pub link: String,
    shutdown: CancellationToken,
    server: JoinHandle<()>,
    _tmp: TempDir,
}

impl Vault {
    /// Starts the vault on the current tokio runtime.
    pub async fn start() -> Result<Self> {
        let tmp = tempfile::tempdir()?;
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("tls"))?;
        let tls = Tls::load_or_create(&dir.join("tls"), "fake-vault")?;
        let auth = Arc::new(Auth::open(dir)?);
        let mut store = VaultStore::open(dir.join("vault.db"))?;
        for (id, name) in HOSTS {
            store.bind(&DeviceId::new(id), &HostId::new(id), name, None, &[])?;
        }
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let link = PairingUri {
            hosts: vec![listener.local_addr()?.to_string()],
            fingerprint: tls.fingerprint().to_owned(),
            code: auth.mint("sample", None, PAIRING_TTL)?.code,
        }
        .to_string();
        let host = Host {
            id: HostId::new("fake-vault"),
            name: "fake-vault".into(),
        };
        let server = Server::new(
            tls,
            auth,
            store,
            host,
            LIVENESS_TIMEOUT,
            Retention::default(),
            Some(SecretsKey::new(&[7; 32])),
        );
        let shutdown = CancellationToken::new();
        let server = tokio::spawn(server.run(vec![listener], shutdown.clone()));
        Ok(Self {
            link,
            shutdown,
            server,
            _tmp: tmp,
        })
    }

    /// Stops the vault and waits until it has.
    pub async fn stop(self) -> Result<()> {
        self.shutdown.cancel();
        Ok(self.server.await?)
    }
}
