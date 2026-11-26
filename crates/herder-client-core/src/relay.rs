//! Reaching a machine through a vault this device is signed in to, when it cannot dial the
//! machine itself: away from the machine's network, with no VPN and no public address of the
//! machine's own.
//!
//! The client opens a connection to the vault as it always does, pinned to the vault's
//! certificate and with this device's key for the vault, and asks for a stream to the machine
//! ([`herder_protocol::relay`]). The vault forwards the stream's bytes to the machine, which
//! keeps a connection to the vault. Inside the stream the client then runs its ordinary
//! connection to the machine: TLS pinning the machine's certificate and presenting this
//! device's key for it, the WebSocket and the hello. The vault relays only ciphertext, and
//! the machine knows the device as on a direct connection.

use std::sync::{Arc, Mutex, PoisonError, Weak};

use anyhow::{Context as _, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use herder_protocol::{HostId, RELAY_VERSION, RelayAnswer, RelayHello};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::tungstenite::Message;

use crate::address;
use crate::auth::{DeviceKey, client_config};
use crate::supervisor::{self, Supervisor, Watched};
use crate::tunnel::{self, Frames};

/// Where a machine's `vault://` address leads: the machine it is of, and the client's
/// machines, the vault among them.
#[derive(Clone, Default)]
pub(crate) struct Via {
    /// The machine to reach; `None` for one that is being signed in to, which no vault
    /// relays to yet.
    machine: Option<HostId>,
    machines: Weak<Mutex<Vec<Arc<Supervisor>>>>,
}

/// A relay stream through a vault: the frames of a connection to it.
pub(crate) type Stream = Frames<TlsStream<Watched>>;

impl Via {
    /// The routes of `machine`, one of `machines`.
    pub(crate) fn new(machine: HostId, machines: Weak<Mutex<Vec<Arc<Supervisor>>>>) -> Self {
        Self {
            machine: Some(machine),
            machines,
        }
    }

    /// A stream to the machine through `vault`, one of the client's machines: the vault is
    /// reached at its own addresses, raced as any machine's are.
    pub(crate) async fn stream(&self, vault: &HostId) -> anyhow::Result<Stream> {
        let host_id = self
            .machine
            .clone()
            .context("a machine is first signed in to at an address of its own")?;
        let saved = self
            .machines
            .upgrade()
            .and_then(|machines| {
                // Every update leaves the list whole, so a poisoned list is consistent.
                let machines = machines.lock().unwrap_or_else(PoisonError::into_inner);
                machines
                    .iter()
                    .find(|machine| machine.saved.host_id == *vault)
                    .cloned()
            })
            .map(|vault| vault.saved())
            .with_context(|| format!("this device is not signed in to the vault {vault}"))?;
        let addresses: Vec<String> = saved
            .addresses
            .into_iter()
            .filter(|address| address::vault(address).is_none())
            .collect();
        let device = DeviceKey::from_pem(&saved.device_key)?;
        let connector = TlsConnector::from(Arc::new(client_config(&saved.fingerprint, &device)?));
        let reaching = |address: String| {
            let connector = connector.clone();
            // Each frame goes out as written: the TLS inside decides what to batch.
            async move { supervisor::direct(&address, &connector, Some(tunnel::config())).await }
        };
        let (mut ws, _) = supervisor::race(&addresses, reaching)
            .await
            .map_err(|error| anyhow!("the vault {}: {error}", saved.name))?;
        let connect = RelayHello::Connect {
            relay_version: RELAY_VERSION,
            host_id,
        };
        ws.send(Message::text(serde_json::to_string(&connect)?))
            .await
            .context("asking the vault")?;
        loop {
            let text = match ws.next().await {
                Some(Ok(Message::Text(text))) => text,
                Some(Ok(Message::Close(_))) | None => bail!("the vault closed the connection"),
                Some(Ok(_)) => continue,
                Some(Err(err)) => return Err(err).context("the vault's answer"),
            };
            return match serde_json::from_str(&text).context("the vault's answer")? {
                RelayAnswer::Ready => Ok(Frames::new(ws)),
                RelayAnswer::Refused { error } => {
                    Err(anyhow!("the vault {}: {}", saved.name, error.message))
                }
                RelayAnswer::Unknown => Err(anyhow!("the vault's answer is newer than this app")),
            };
        }
    }
}
