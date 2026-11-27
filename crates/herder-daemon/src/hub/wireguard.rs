//! herder's WireGuard on the hub (`docs/remote-access.md`): a userspace endpoint on one UDP
//! port, no TUN device and no privileges.
//!
//! Its peers are the WireGuard keys paired client devices enrolled (`enroll_wireguard`),
//! kept on each device's record ([`crate::auth::WireGuardPeer`]), so revoking a device drops
//! its key within [`PEERS_EVERY`] and cuts its connections. The endpoint answers no one
//! else. Inside the tunnel it takes TCP connections to the hub's herder port at
//! [`HUB_ADDRESS`] and nothing more, and serves each as a connection to that port: the
//! hub's TLS, the device's certificate, the hello, and relay streams to every machine
//! (`hub://`). It forwards no packet to any network.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{Context, Result};
use herder_protocol::{ErrorCode, ErrorInfo, WireGuardEnrollment, WireGuardSettings};
use herder_wireguard::{Endpoint, IpCidr, Key, Peer};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::Shared;
use super::conn::{self, Incoming};
use crate::auth::WIREGUARD_NETWORK;

/// The hub's own address inside the tunnel.
pub const HUB_ADDRESS: Ipv4Addr = Ipv4Addr::new(10, 247, 0, 1);

/// How often the endpoint takes the enrolled keys afresh.
const PEERS_EVERY: Duration = Duration::from_secs(1);

/// The hub's WireGuard: its key, and the endpoint while it is on.
pub struct HubWireGuard {
    key: Key,
    /// The hub's herder port, which the tunnel reaches.
    port: u16,
    shared: Weak<Shared>,
    shutdown: CancellationToken,
    running: tokio::sync::Mutex<Option<Running>>,
}

struct Running {
    settings: WireGuardSettings,
    stop: CancellationToken,
}

impl HubWireGuard {
    /// The hub's WireGuard, with its key in `<data_dir>/wireguard.key` (made the first time),
    /// reaching the hub's herder `port`; off until [`HubWireGuard::apply`].
    pub(super) fn open(
        data_dir: &Path,
        port: u16,
        shared: Weak<Shared>,
        shutdown: CancellationToken,
    ) -> Result<Self> {
        Ok(Self {
            key: key(&data_dir.join("wireguard.key"))?,
            port,
            shared,
            shutdown,
            running: tokio::sync::Mutex::new(None),
        })
    }

    /// The hub's herder port inside the tunnel.
    pub fn hub_address(&self) -> SocketAddr {
        SocketAddr::new(HUB_ADDRESS.into(), self.port)
    }

    /// Runs the endpoint as `settings` say: off with none, restarted when they changed. A
    /// port it cannot bind is refused, and the endpoint runs on as it did.
    pub async fn apply(&self, settings: Option<&WireGuardSettings>) -> Result<(), ErrorInfo> {
        let mut running = self.running.lock().await;
        if running.as_ref().map(|r| &r.settings) == settings {
            return Ok(());
        }
        let old = running.take();
        if let Some(old) = &old {
            old.stop.cancel();
        }
        let Some(settings) = settings else {
            info!("herder's WireGuard is off");
            return Ok(());
        };
        match self.start(settings).await {
            Ok(started) => {
                info!(
                    port = settings.port,
                    endpoint = settings.endpoint,
                    "herder's WireGuard is on"
                );
                *running = Some(started);
                Ok(())
            }
            Err(err) => {
                if let Some(old) = old {
                    match self.start(&old.settings).await {
                        Ok(again) => *running = Some(again),
                        Err(again) => warn!("herder's WireGuard is off: {}", again.message),
                    }
                }
                Err(err)
            }
        }
    }

    /// The endpoint, serving, on `settings`' port.
    async fn start(&self, settings: &WireGuardSettings) -> Result<Running, ErrorInfo> {
        let network = IpCidr::new(HUB_ADDRESS.into(), WIREGUARD_NETWORK.1);
        let any6 = SocketAddr::new(std::net::Ipv6Addr::UNSPECIFIED.into(), settings.port);
        let endpoint = match Endpoint::bind(any6, self.key, network, self.hub_address()).await {
            Ok(endpoint) => endpoint,
            // A host without IPv6 binds IPv4 alone.
            Err(_) => {
                let any4 = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), settings.port);
                Endpoint::bind(any4, self.key, network, self.hub_address())
                    .await
                    .map_err(|err| ErrorInfo {
                        code: ErrorCode::BadRequest,
                        message: format!("cannot bind UDP port {}: {err}", settings.port),
                    })?
            }
        };
        let stop = self.shutdown.child_token();
        tokio::spawn(serve(Arc::new(endpoint), self.shared.clone(), stop.clone()));
        Ok(Running {
            settings: settings.clone(),
            stop,
        })
    }

    /// What a device that enrolled at `address` needs, with its private key, to reach the hub,
    /// while the endpoint is on.
    pub async fn enrollment(&self, address: Ipv4Addr) -> Option<WireGuardEnrollment> {
        let running = self.running.lock().await;
        let settings = &running.as_ref()?.settings;
        Some(WireGuardEnrollment {
            endpoint: settings.endpoint.clone(),
            hub_public_key: self.key.public().to_base64(),
            address: address.to_string(),
            hub_address: self.hub_address().to_string(),
        })
    }

    /// The endpoint's settings while it is on.
    pub async fn settings(&self) -> Option<WireGuardSettings> {
        let running = self.running.lock().await;
        running.as_ref().map(|r| r.settings.clone())
    }
}

/// Serves the endpoint until `stop`: its peers kept to the enrolled keys, each connection it
/// takes served as one to the hub's herder port.
async fn serve(endpoint: Arc<Endpoint>, shared: Weak<Shared>, stop: CancellationToken) {
    let mut every = tokio::time::interval(PEERS_EVERY);
    let mut peers: Vec<Peer> = Vec::new();
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            _ = every.tick() => {
                let Some(shared) = shared.upgrade() else { return };
                let now: Vec<Peer> = shared
                    .auth
                    .wireguard_peers()
                    .into_iter()
                    .filter_map(|peer| {
                        let public_key = peer.public_key.parse().ok()?;
                        Some(Peer { public_key, address: peer.address.into() })
                    })
                    .collect();
                if now != peers {
                    endpoint.set_peers(now.clone());
                    peers = now;
                }
            }
            accepted = endpoint.accept() => {
                let Some(accepted) = accepted else { return };
                let Some(shared) = shared.upgrade() else { return };
                tokio::spawn(conn::run(
                    Incoming::WireGuard(accepted.stream),
                    accepted.from,
                    shared,
                    stop.child_token(),
                ));
            }
        }
    }
}

/// The hub's WireGuard key in `path`, made and kept there, private, the first time.
fn key(path: &PathBuf) -> Result<Key> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .parse()
            .map_err(|err: String| anyhow::anyhow!(err))
            .with_context(|| format!("reading {}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let key = Key::generate().map_err(|err| anyhow::anyhow!(err))?;
            let dir = path.parent().context("the key's directory")?;
            let file = path
                .file_name()
                .and_then(|name| name.to_str())
                .context("the key's file name")?;
            crate::data_dir::write_private(dir, file, key.to_base64().as_bytes())?;
            Ok(key)
        }
        Err(err) => {
            warn!("reading {}: {err}", path.display());
            Err(err).with_context(|| format!("reading {}", path.display()))
        }
    }
}
