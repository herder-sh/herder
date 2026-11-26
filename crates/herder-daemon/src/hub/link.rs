//! A host's link to its hub: the `[hub]` table it replicates to and fetches secrets from,
//! which an owner sets, switches and clears from a client while the daemon runs (`link_hub`,
//! `set_hub_link`, `unlink_hub`). A host links to one hub at most.
//!
//! Linking checks the hub first: the daemon connects with its device key and pairs with the
//! host-only code the client got from the hub (`pair_hub_host`), trying the hub's
//! addresses in order. Only once the hub said hello does it write the `[hub]` table, without
//! the spent code, into its config file, keeping the rest of the file as written, and connect:
//! to replicate with `backup` on, to fetch secrets with `secrets` on. Switching either rewrites
//! just those keys and reconnects; with both off the host keeps the link but does not connect.
//! Unlinking removes the table and disconnects; what the hub holds stays there. Unpairing the
//! host's device on the hub (`revoke_hub_host`) is the client's, as an owner of both.
//!
//! An owner may also have the daemon run a hub on its own machine (`install_hub`). The
//! daemon asks that hub, over its control socket, for a host code to link with, secrets on
//! and backup off, and then opens its window for a first owner, which the client claims.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use anyhow::Result;
use herder_protocol::{CommandBody, CommandResult, ErrorCode, ErrorInfo, LinkedHub};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::install::LocalHub;
use super::{HostSecrets, Replicator};
use crate::auth::control::Request;
use crate::config::{self, HubConfig};
use crate::session::SessionManager;
use crate::session::fork::Forks;
use crate::ws::{Host, Relayed};

/// What a host replicates with.
pub struct Setup {
    /// The daemon's config file, which keeps the `[hub]` table.
    pub config_file: PathBuf,
    /// This host.
    pub host: Host,
    /// Whose journals are replicated.
    pub sessions: SessionManager,
    /// Notified when the journal grows; see [`super::WakeOnEvent`].
    pub changed: Arc<Notify>,
    /// The data dir, which keeps the host's device key.
    pub data_dir: PathBuf,
    /// Stops replicating, with the daemon.
    pub shutdown: CancellationToken,
    /// What the host's sessions get of the hub's secrets.
    pub secrets: Arc<HostSecrets>,
    /// The hub this machine runs or may run next to the daemon; `None` where the daemon
    /// installs no hub.
    pub local: Option<LocalHub>,
}

/// The hub this host replicates to, if any, and the replicator streaming there.
pub struct Link {
    setup: Setup,
    /// Held through a link or an unlink, so they apply one at a time.
    changing: tokio::sync::Mutex<()>,
    state: Mutex<State>,
    /// Serves the clients the hub relays to this host; see [`Link::serve_relayed`].
    relayed: Arc<OnceLock<Relayed>>,
}

#[derive(Default)]
struct State {
    hub: Option<HubConfig>,
    /// Stops the running replicator.
    replicating: Option<CancellationToken>,
}

impl Link {
    /// A link to `hub`, the config's `[hub]` table, replicating there at once; none
    /// without one.
    pub fn start(setup: Setup, hub: Option<HubConfig>) -> Result<Self> {
        let link = Self {
            setup,
            changing: tokio::sync::Mutex::new(()),
            state: Mutex::new(State::default()),
            relayed: Arc::default(),
        };
        link.setup.sessions.fork_from(Forks {
            host: link.setup.host.id.clone(),
        })?;
        if let Some(hub) = hub {
            link.replicate(hub)?;
        }
        Ok(link)
    }

    /// Has `relayed` serve the clients the hub relays to this host; once.
    pub fn serve_relayed(&self, relayed: Relayed) -> Result<()> {
        self.relayed
            .set(relayed)
            .map_err(|_| anyhow::anyhow!("relayed clients are served already"))
    }

    /// The hub this host replicates to now.
    pub fn hub(&self) -> Option<HubConfig> {
        self.lock().hub.clone()
    }

    /// Applies `get_hub_link`, `link_hub`, `set_hub_link` or `unlink_hub`.
    pub async fn command(&self, command: CommandBody) -> Result<CommandResult, ErrorInfo> {
        match command {
            CommandBody::GetHubLink => Ok(CommandResult::HubLink {
                is_hub: false,
                hub: self.hub().map(|hub| LinkedHub {
                    address: hub.address,
                    fingerprint: hub.fingerprint,
                    backup: hub.backup,
                    secrets: hub.secrets,
                    // Its key is a secret: of no use to a host that fetches none.
                    cliproxyapi: hub
                        .secrets
                        .then(|| self.setup.secrets.cliproxyapi())
                        .flatten(),
                }),
                volume: None,
                local_hub: self
                    .setup
                    .local
                    .as_ref()
                    .is_some_and(LocalHub::is_installed),
            }),
            CommandBody::LinkHub {
                addresses,
                fingerprint,
                pairing_code,
                backup,
                secrets,
            } => {
                let link = self.link(addresses, &fingerprint, pairing_code, backup, secrets);
                link.await
            }
            CommandBody::SetHubLink { backup, secrets } => self.switch(backup, secrets).await,
            CommandBody::UnlinkHub => self.unlink().await,
            _ => Err(error(
                ErrorCode::BadRequest,
                "not a hub link command".to_owned(),
            )),
        }
    }

    /// Applies `install_hub`: installs a hub on this machine, links to it, and answers with
    /// the window in which the caller claims it as its first owner.
    pub async fn install_hub(&self) -> Result<CommandResult, ErrorInfo> {
        let _changing = self.changing.lock().await;
        self.unlinked()?;
        let Some(local) = self.setup.local.clone() else {
            return Err(error(
                ErrorCode::Unsupported,
                "this daemon does not run a hub on its machine".to_owned(),
            ));
        };
        if local.is_installed() {
            return Err(error(
                ErrorCode::Conflict,
                "a hub is installed on this machine already; link to it with a code from \
                 `herder pair --host` there"
                    .to_owned(),
            ));
        }
        let installing = local.clone();
        blocking(move || installing.install(&mut |step| info!("installing the hub: {step}")))
            .await?;
        let host = Request::PairHost {
            host: self.setup.host.name.clone(),
        };
        let hub = local.clone();
        let host = blocking(move || hub.pair(&host)).await?;
        // The hub's own addresses, not loopback: the host keeps the first that answers, and
        // devices are told where it is from there.
        self.link_unlocked(host.addresses, &host.fingerprint, host.code, false, true)
            .await?;
        let own = blocking(move || local.own()).await?;
        info!(hub = %own.fingerprint, "installed the hub on this machine, waiting for its owner");
        Ok(CommandResult::OwnershipClaim {
            code: own.code,
            fingerprint: own.fingerprint,
            addresses: own.addresses,
            expires_at: own.expires_at,
        })
    }

    /// Refuses a new link while this host is linked to a hub.
    fn unlinked(&self) -> Result<(), ErrorInfo> {
        match self.hub() {
            Some(hub) => Err(error(
                ErrorCode::Conflict,
                format!(
                    "this host is linked to the hub at {} already; unlink it first",
                    hub.address
                ),
            )),
            None => Ok(()),
        }
    }

    async fn link(
        &self,
        addresses: Vec<String>,
        fingerprint: &str,
        pairing_code: String,
        backup: bool,
        secrets: bool,
    ) -> Result<CommandResult, ErrorInfo> {
        let _changing = self.changing.lock().await;
        self.unlinked()?;
        self.link_unlocked(addresses, fingerprint, pairing_code, backup, secrets)
            .await
    }

    /// Links to the hub; the caller holds `changing` and checked this host is unlinked.
    async fn link_unlocked(
        &self,
        addresses: Vec<String>,
        fingerprint: &str,
        pairing_code: String,
        backup: bool,
        secrets: bool,
    ) -> Result<CommandResult, ErrorInfo> {
        let fingerprint: String = fingerprint
            .chars()
            .filter(|c| !c.is_whitespace() && *c != ':')
            .collect::<String>()
            .to_ascii_lowercase();
        if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(error(
                ErrorCode::BadRequest,
                "the hub's fingerprint is 64 hex digits".to_owned(),
            ));
        }
        if addresses.is_empty() {
            return Err(error(
                ErrorCode::BadRequest,
                "name at least one address of the hub".to_owned(),
            ));
        }
        let mut failures = Vec::new();
        let mut linked = None;
        for address in addresses {
            let hub = HubConfig::new(address, fingerprint.clone(), Some(pairing_code.clone()));
            match self
                .replicator(hub.clone())
                .map_err(internal)?
                .check()
                .await
            {
                Ok(()) => {
                    linked = Some(hub);
                    break;
                }
                Err(err) => failures.push(format!("{}: {err:#}", hub.address)),
            }
        }
        let Some(hub) = linked else {
            return Err(error(
                ErrorCode::Conflict,
                format!("the hub did not take this host: {}", failures.join("; ")),
            ));
        };
        // Paired now: the spent code is not kept.
        let hub = HubConfig {
            pairing_code: None,
            backup,
            secrets,
            ..hub
        };
        self.save(Some(hub.clone())).await?;
        self.replicate(hub.clone()).map_err(internal)?;
        info!(hub = %hub.address, backup, secrets, "linked to the hub");
        Ok(CommandResult::Applied)
    }

    /// Turns replicating and fetching secrets on or off, keeping the link.
    async fn switch(&self, backup: bool, secrets: bool) -> Result<CommandResult, ErrorInfo> {
        let _changing = self.changing.lock().await;
        let Some(hub) = self.hub() else {
            return Err(not_linked());
        };
        if (hub.backup, hub.secrets) == (backup, secrets) {
            return Ok(CommandResult::Applied);
        }
        let file = self.setup.config_file.clone();
        tokio::task::spawn_blocking(move || config::set_hub_switches(&file, backup, secrets))
            .await
            .map_err(|err| error(ErrorCode::Internal, format!("{err}")))?
            .map_err(internal)?;
        let hub = HubConfig {
            backup,
            secrets,
            ..hub
        };
        self.replicate(hub.clone()).map_err(internal)?;
        info!(hub = %hub.address, backup, secrets, "the hub link changed");
        Ok(CommandResult::Applied)
    }

    async fn unlink(&self) -> Result<CommandResult, ErrorInfo> {
        let _changing = self.changing.lock().await;
        let Some(hub) = self.hub() else {
            return Err(not_linked());
        };
        self.save(None).await?;
        let mut state = self.lock();
        state.hub = None;
        if let Some(replicating) = state.replicating.take() {
            replicating.cancel();
        }
        self.setup.secrets.set_linked(false);
        info!(hub = %hub.address, "unlinked from the hub");
        Ok(CommandResult::Applied)
    }

    /// Writes `hub` as the config file's `[hub]` table, or removes it.
    async fn save(&self, hub: Option<HubConfig>) -> Result<(), ErrorInfo> {
        let file = self.setup.config_file.clone();
        tokio::task::spawn_blocking(move || config::set_hub(&file, hub.as_ref()))
            .await
            .map_err(|err| error(ErrorCode::Internal, format!("{err}")))?
            .map_err(internal)
    }

    /// Connects to `hub` to do what its switches say, in place of any replicator running;
    /// with both off, it does not connect.
    fn replicate(&self, hub: HubConfig) -> Result<()> {
        let stop = if hub.backup || hub.secrets {
            let replicator = self.replicator(hub.clone())?;
            let stop = self.setup.shutdown.child_token();
            tokio::spawn(replicator.run(stop.clone()));
            Some(stop)
        } else {
            None
        };
        self.setup.secrets.set_linked(hub.secrets);
        let mut state = self.lock();
        state.hub = Some(hub);
        if let Some(previous) = std::mem::replace(&mut state.replicating, stop) {
            previous.cancel();
        }
        Ok(())
    }

    fn replicator(&self, hub: HubConfig) -> Result<Replicator> {
        Ok(Replicator {
            hub,
            device: Replicator::device_key(&self.setup.data_dir)?,
            host: self.setup.host.clone(),
            sessions: self.setup.sessions.clone(),
            changed: Arc::clone(&self.setup.changed),
            secrets: Arc::clone(&self.setup.secrets),
            relayed: Arc::clone(&self.relayed),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // Every update is a field write or two, so a poisoned state is consistent.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn error(code: ErrorCode, message: String) -> ErrorInfo {
    ErrorInfo { code, message }
}

fn not_linked() -> ErrorInfo {
    error(
        ErrorCode::NotFound,
        "this host is linked to no hub".to_owned(),
    )
}

/// Runs `call`, which waits on files, services or the hub's socket, on the blocking pool.
async fn blocking<T: Send + 'static>(
    call: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T, ErrorInfo> {
    tokio::task::spawn_blocking(call)
        .await
        .map_err(|err| error(ErrorCode::Internal, format!("{err}")))?
        .map_err(internal)
}

fn internal(err: anyhow::Error) -> ErrorInfo {
    warn!("the hub link failed: {err:#}");
    error(ErrorCode::Internal, format!("{err:#}"))
}
