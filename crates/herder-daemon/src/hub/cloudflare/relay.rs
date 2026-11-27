//! The hub's Durable Object relay in its owner's Cloudflare account
//! (`deploy/cloudflare-relay/`, `docs/remote-access.md`): deployed, kept current and removed
//! through Cloudflare's API as the remote access settings say (`CloudflareSettings::relay`).
//!
//! The hub generates the secret the relay checks tokens with, sets it as the script's
//! `RELAY_SECRET`, and keeps it with what it deployed in `<data_dir>/cloudflare/relay.json`,
//! readable by its user only. Each time the hub starts, and each time it is told to, it
//! checks the script with Cloudflare: one that is gone is deployed again, as is one older
//! than the source this hub bundles, so updating herder updates the relay.

use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result};
use herder_protocol::{RouteState, RouteStatus, SecretValue};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::api::Api;
use super::workers::{self, SCRIPT, WORKER};
use crate::hub::cloudflare_relay::{Deployed, Relay};

/// How long the hub waits before trying a failed set-up again.
const RETRY: Duration = Duration::from_secs(300);

/// The relay an owner asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WantedRelay {
    /// The Cloudflare account.
    pub account_id: String,
    /// The API token, from the hub secret the settings name.
    pub api_token: SecretValue,
}

/// What the hub was last told to make of the relay.
#[derive(Clone, Debug)]
struct Desired {
    /// The relay, none, or why the hub cannot tell (its API token is missing).
    wanted: Result<Option<WantedRelay>, String>,
    /// The API token to delete the relay with, when none is wanted.
    removal_token: Option<SecretValue>,
}

/// Where the hub is in making the relay match what it was told.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Phase {
    /// Done, or not told anything yet.
    #[default]
    Idle,
    SettingUp,
    Failed(String),
}

/// What the hub keeps of its relay, in `relay.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    account_id: String,
    /// `https://herder-relay.<subdomain>.workers.dev`.
    url: String,
    /// The relay's `RELAY_SECRET`.
    secret: SecretValue,
    /// SHA-256 of the source deployed, as hex.
    worker: String,
}

/// The hub's Durable Object relay, as deployed.
pub struct ManagedRelay {
    /// `<data_dir>/cloudflare`.
    dir: PathBuf,
    /// Cloudflare's API.
    api: String,
    /// What is deployed.
    state: Mutex<Option<State>>,
    /// Where the hub's connections find the relay, to hand out tokens and take streams.
    relay: Arc<Relay>,
    desired: watch::Sender<Option<Desired>>,
    phase: std::sync::Mutex<Phase>,
}

impl ManagedRelay {
    /// The relay of the hub with `data_dir`, deployed through the API at `api` ([`super::API`]
    /// but in tests), and used by the hub's connections through `relay`.
    pub(crate) fn open(data_dir: &Path, api: &str, relay: Arc<Relay>) -> Result<Arc<Self>> {
        let dir = data_dir.join("cloudflare");
        let state = load(&dir)?;
        // At once, on what was deployed, while the hub checks it with Cloudflare.
        relay.set(state.as_ref().map(deployed));
        Ok(Arc::new(Self {
            dir,
            api: api.to_owned(),
            state: Mutex::new(state),
            relay,
            desired: watch::Sender::new(None),
            phase: std::sync::Mutex::default(),
        }))
    }

    /// Makes the relay match what [`Self::want`] says from now on, until `shutdown`.
    pub fn start(self: &Arc<Self>, shutdown: CancellationToken) {
        tokio::spawn(Arc::clone(self).reconcile(self.desired.subscribe(), shutdown));
    }

    /// Deploys the relay as `wanted`, or deletes it with `removal_token` when none is; or,
    /// given why the hub cannot tell what is wanted, keeps what runs and says so.
    pub fn want(
        &self,
        wanted: Result<Option<WantedRelay>, String>,
        removal_token: Option<SecretValue>,
    ) {
        self.set_phase(Phase::SettingUp);
        self.desired.send_replace(Some(Desired {
            wanted,
            removal_token,
        }));
    }

    /// The relay as `RemoteAccess::relay` shows it: `None` when there is none, wanted or
    /// deployed; its URL empty until it is known.
    pub async fn status(&self) -> Option<RouteStatus> {
        let wanted = matches!(
            self.desired
                .borrow()
                .as_ref()
                .map(|desired| &desired.wanted),
            Some(Ok(Some(_)) | Err(_))
        );
        let state = self.state.lock().await.clone();
        if !wanted && state.is_none() {
            return None;
        }
        let url = state.map(|state| state.url).unwrap_or_default();
        let phase = self
            .phase
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let (state, error) = match phase {
            Phase::SettingUp => (RouteState::SettingUp, None),
            Phase::Failed(why) => (RouteState::Failed, Some(why)),
            Phase::Idle if url.is_empty() => (RouteState::SettingUp, None),
            Phase::Idle => (RouteState::Up, None),
        };
        Some(RouteStatus { url, state, error })
    }

    fn set_phase(&self, phase: Phase) {
        *self.phase.lock().unwrap_or_else(PoisonError::into_inner) = phase;
    }

    /// Makes the relay match each thing the hub is told, trying again every [`RETRY`] while
    /// that fails, until `shutdown`.
    async fn reconcile(
        self: Arc<Self>,
        mut desired: watch::Receiver<Option<Desired>>,
        shutdown: CancellationToken,
    ) {
        loop {
            let now = desired.borrow_and_update().clone();
            let retry = match now {
                None => None,
                Some(now) => {
                    self.set_phase(Phase::SettingUp);
                    match self.apply(now).await {
                        Ok(()) => {
                            self.set_phase(Phase::Idle);
                            None
                        }
                        Err(err) => {
                            warn!("the Cloudflare relay is not deployed: {err:#}");
                            self.set_phase(Phase::Failed(format!("{err:#}")));
                            Some(RETRY)
                        }
                    }
                }
            };
            tokio::select! {
                () = shutdown.cancelled() => return,
                changed = desired.changed() => if changed.is_err() { return },
                () = async {
                    match retry {
                        Some(retry) => tokio::time::sleep(retry).await,
                        None => std::future::pending().await,
                    }
                } => {}
            }
        }
    }

    /// Makes Cloudflare match `desired`.
    async fn apply(&self, desired: Desired) -> Result<()> {
        let mut state = self.state.lock().await;
        let previous = state.clone();
        let base = self.api.clone();
        let Some(wanted) = desired.wanted.map_err(anyhow::Error::msg)? else {
            let Some(old) = previous else {
                return Ok(());
            };
            let token = desired.removal_token.context(
                "the hub secret holding the Cloudflare API token is gone, so the relay cannot \
                 be deleted; put it back, or delete the herder-relay Worker in Cloudflare",
            )?;
            self.relay.set(None);
            tokio::task::spawn_blocking(move || {
                Api::new(&base, token).delete_relay(&old.account_id)
            })
            .await
            .context("the relay removal panicked")??;
            remove(&self.dir)?;
            *state = None;
            info!("the Cloudflare relay is deleted");
            return Ok(());
        };
        let new = tokio::task::spawn_blocking(move || {
            let api = Api::new(&base, wanted.api_token.clone());
            let mut previous = previous;
            if let Some(old) = previous.take_if(|old| old.account_id != wanted.account_id) {
                api.delete_relay(&old.account_id)?;
            }
            ensure(&api, &wanted.account_id, previous)
        })
        .await
        .context("the relay deployment panicked")??;
        save(&self.dir, &new)?;
        info!(url = new.url, "the Cloudflare relay is deployed");
        self.relay.set(Some(deployed(&new)));
        *state = Some(new);
        Ok(())
    }
}

/// Deploys the relay to `account`, unless `previous`, deployed there, is current and still
/// there: what is deployed after.
fn ensure(api: &Api, account: &str, previous: Option<State>) -> Result<State> {
    let subdomain = api.workers_subdomain(account)?;
    let url = format!("https://{SCRIPT}.{subdomain}.workers.dev");
    let script = api.relay_script(account)?;
    let migrated = script.as_deref().is_some_and(workers::migrated);
    let secret = match &previous {
        Some(previous) => previous.secret.clone(),
        None => new_secret()?,
    };
    let current = previous.as_ref().is_some_and(|previous| {
        previous.url == url && previous.worker == worker_hash() && migrated
    });
    if !current {
        api.upload_relay(account, secret.expose(), migrated)?;
    }
    Ok(State {
        account_id: account.to_owned(),
        url,
        secret,
        worker: worker_hash(),
    })
}

/// A new secret for the relay's tokens: 32 random bytes, as hex.
fn new_secret() -> Result<SecretValue> {
    let mut bytes = [0u8; 32];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
        .map_err(|_| anyhow::anyhow!("no randomness for the relay's secret"))?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    SecretValue::new(hex).context("an empty secret")
}

/// SHA-256 of the relay's source, as hex.
fn worker_hash() -> String {
    Sha256::digest(WORKER.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The relay `state` deploys, as the hub's connections use it.
fn deployed(state: &State) -> Deployed {
    Deployed {
        url: state.url.clone(),
        secret: state.secret.expose().to_owned(),
    }
}

fn state_path(dir: &Path) -> PathBuf {
    dir.join("relay.json")
}

fn load(dir: &Path) -> Result<Option<State>> {
    let path = state_path(dir);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .with_context(|| format!("reading {}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
    }
}

fn save(dir: &Path, state: &State) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    crate::data_dir::write_private(dir, "relay.json", &serde_json::to_vec_pretty(state)?)
}

fn remove(dir: &Path) -> Result<()> {
    let path = state_path(dir);
    match std::fs::remove_file(&path) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            Err(err).with_context(|| format!("removing {}", path.display()))
        }
        _ => Ok(()),
    }
}
