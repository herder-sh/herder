//! A host's link to its vault: the `[vault]` table it replicates to and fetches secrets from,
//! which an owner sets, switches and clears from a client while the daemon runs (`link_vault`,
//! `set_vault_link`, `unlink_vault`). A host links to one vault at most.
//!
//! Linking checks the vault first: the daemon connects with its device key and pairs with the
//! host-only code the client got from the vault (`pair_vault_host`), trying the vault's
//! addresses in order. Only once the vault said hello does it write the `[vault]` table, without
//! the spent code, into its config file, keeping the rest of the file as written, and connect:
//! to replicate with `backup` on, to fetch secrets with `secrets` on. Switching either rewrites
//! just those keys and reconnects; with both off the host keeps the link but does not connect.
//! Unlinking removes the table and disconnects; what the vault holds stays there. Unpairing the
//! host's device on the vault (`revoke_vault_host`) is the client's, as an owner of both.
//!
//! An owner may also have the daemon run a vault on its own machine (`install_vault`), or pair
//! with one installed there by hand (`link_local_vault`). The daemon asks that vault, over its
//! control socket, for a host code to link with, secrets on and backup off, and then for a code
//! that pairs the owner's device with it as an owner, which the client redeems.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::Result;
use herder_protocol::{CommandBody, CommandResult, ErrorCode, ErrorInfo, LinkedVault, Role};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::fork::FromVault;
use super::install::LocalVault;
use super::{HostSecrets, Replicator};
use crate::auth::control::Request;
use crate::config::{self, VaultConfig};
use crate::session::SessionManager;
use crate::session::fork::Forks;
use crate::ws::Host;

/// What a host replicates with.
pub struct Setup {
    /// The daemon's config file, which keeps the `[vault]` table.
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
    /// What the host's sessions get of the vault's secrets.
    pub secrets: Arc<HostSecrets>,
    /// The vault this machine runs or may run next to the daemon; `None` where the daemon
    /// installs no vault.
    pub local: Option<LocalVault>,
}

/// The vault this host replicates to, if any, and the replicator streaming there.
pub struct Link {
    setup: Setup,
    /// Held through a link or an unlink, so they apply one at a time.
    changing: tokio::sync::Mutex<()>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    vault: Option<VaultConfig>,
    /// Stops the running replicator.
    replicating: Option<CancellationToken>,
}

impl Link {
    /// A link to `vault`, the config's `[vault]` table, replicating there at once; none
    /// without one.
    pub fn start(setup: Setup, vault: Option<VaultConfig>) -> Result<Self> {
        let link = Self {
            setup,
            changing: tokio::sync::Mutex::new(()),
            state: Mutex::new(State::default()),
        };
        link.configure_forks(None)?;
        if let Some(vault) = vault {
            link.replicate(vault)?;
        }
        Ok(link)
    }

    /// The vault this host replicates to now.
    pub fn vault(&self) -> Option<VaultConfig> {
        self.lock().vault.clone()
    }

    /// Keeps fork lookup aligned with the live vault link, including no vault.
    fn configure_forks(&self, vault: Option<VaultConfig>) -> Result<()> {
        self.setup.sessions.fork_from(Forks {
            host: self.setup.host.id.clone(),
            vault: vault
                .map(|vault| {
                    Ok::<_, anyhow::Error>(FromVault {
                        vault,
                        device: Replicator::device_key(&self.setup.data_dir)?,
                    })
                })
                .transpose()?,
        })
    }

    /// Applies `get_vault_link`, `link_vault`, `set_vault_link` or `unlink_vault`.
    pub async fn command(&self, command: CommandBody) -> Result<CommandResult, ErrorInfo> {
        match command {
            CommandBody::GetVaultLink => Ok(CommandResult::VaultLink {
                is_vault: false,
                vault: self.vault().map(|vault| LinkedVault {
                    address: vault.address,
                    fingerprint: vault.fingerprint,
                    backup: vault.backup,
                    secrets: vault.secrets,
                }),
                volume: None,
                local_vault: self
                    .setup
                    .local
                    .as_ref()
                    .is_some_and(LocalVault::is_installed),
            }),
            CommandBody::LinkVault {
                addresses,
                fingerprint,
                pairing_code,
                backup,
                secrets,
            } => {
                let link = self.link(addresses, &fingerprint, pairing_code, backup, secrets);
                link.await
            }
            CommandBody::SetVaultLink { backup, secrets } => self.switch(backup, secrets).await,
            CommandBody::UnlinkVault => self.unlink().await,
            _ => Err(error(
                ErrorCode::BadRequest,
                "not a vault link command".to_owned(),
            )),
        }
    }

    /// Applies `install_vault` or `link_local_vault` for the owner named `user`, answering
    /// with a code that pairs their device with the vault.
    pub async fn local_vault(
        &self,
        command: CommandBody,
        user: String,
    ) -> Result<CommandResult, ErrorInfo> {
        let _changing = self.changing.lock().await;
        self.unlinked()?;
        let Some(local) = self.setup.local.clone() else {
            return Err(error(
                ErrorCode::Unsupported,
                "this daemon does not run a vault on its machine".to_owned(),
            ));
        };
        let installed = local.is_installed();
        match command {
            CommandBody::InstallVault if installed => {
                return Err(error(
                    ErrorCode::Conflict,
                    "a vault is installed on this machine already; pair and link it instead"
                        .to_owned(),
                ));
            }
            CommandBody::InstallVault => {
                let local = local.clone();
                blocking(move || local.install(&mut |step| info!("installing the vault: {step}")))
                    .await?;
            }
            CommandBody::LinkLocalVault if !installed => {
                return Err(error(
                    ErrorCode::NotFound,
                    "no vault is installed on this machine".to_owned(),
                ));
            }
            CommandBody::LinkLocalVault => {}
            _ => {
                return Err(error(
                    ErrorCode::BadRequest,
                    "not a local vault command".to_owned(),
                ));
            }
        }
        let host = Request::PairHost {
            host: self.setup.host.name.clone(),
        };
        let vault = local.clone();
        let host = blocking(move || vault.pair(&host)).await?;
        // The vault's own addresses, not loopback: the host keeps the first that answers, and
        // devices are told where it is from there.
        self.link_unlocked(host.addresses, &host.fingerprint, host.code, false, true)
            .await?;
        let owner = Request::Pair {
            user,
            role: Some(Role::Owner),
        };
        let device = blocking(move || local.pair(&owner)).await?;
        info!(vault = %device.fingerprint, "paired a device with the vault on this machine");
        Ok(CommandResult::DevicePairing {
            code: device.code,
            fingerprint: device.fingerprint,
            addresses: device.addresses,
            expires_at: device.expires_at,
        })
    }

    /// Refuses a new link while this host is linked to a vault.
    fn unlinked(&self) -> Result<(), ErrorInfo> {
        match self.vault() {
            Some(vault) => Err(error(
                ErrorCode::Conflict,
                format!(
                    "this host is linked to the vault at {} already; unlink it first",
                    vault.address
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

    /// Links to the vault; the caller holds `changing` and checked this host is unlinked.
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
                "the vault's fingerprint is 64 hex digits".to_owned(),
            ));
        }
        if addresses.is_empty() {
            return Err(error(
                ErrorCode::BadRequest,
                "name at least one address of the vault".to_owned(),
            ));
        }
        let mut failures = Vec::new();
        let mut linked = None;
        for address in addresses {
            let vault = VaultConfig::new(address, fingerprint.clone(), Some(pairing_code.clone()));
            match self
                .replicator(vault.clone())
                .map_err(internal)?
                .check()
                .await
            {
                Ok(()) => {
                    linked = Some(vault);
                    break;
                }
                Err(err) => failures.push(format!("{}: {err:#}", vault.address)),
            }
        }
        let Some(vault) = linked else {
            return Err(error(
                ErrorCode::Conflict,
                format!("the vault did not take this host: {}", failures.join("; ")),
            ));
        };
        // Paired now: the spent code is not kept.
        let vault = VaultConfig {
            pairing_code: None,
            backup,
            secrets,
            ..vault
        };
        self.save(Some(vault.clone())).await?;
        self.replicate(vault.clone()).map_err(internal)?;
        info!(vault = %vault.address, backup, secrets, "linked to the vault");
        Ok(CommandResult::Applied)
    }

    /// Turns replicating and fetching secrets on or off, keeping the link.
    async fn switch(&self, backup: bool, secrets: bool) -> Result<CommandResult, ErrorInfo> {
        let _changing = self.changing.lock().await;
        let Some(vault) = self.vault() else {
            return Err(not_linked());
        };
        if (vault.backup, vault.secrets) == (backup, secrets) {
            return Ok(CommandResult::Applied);
        }
        let file = self.setup.config_file.clone();
        tokio::task::spawn_blocking(move || config::set_vault_switches(&file, backup, secrets))
            .await
            .map_err(|err| error(ErrorCode::Internal, format!("{err}")))?
            .map_err(internal)?;
        let vault = VaultConfig {
            backup,
            secrets,
            ..vault
        };
        self.replicate(vault.clone()).map_err(internal)?;
        info!(vault = %vault.address, backup, secrets, "the vault link changed");
        Ok(CommandResult::Applied)
    }

    async fn unlink(&self) -> Result<CommandResult, ErrorInfo> {
        let _changing = self.changing.lock().await;
        let Some(vault) = self.vault() else {
            return Err(not_linked());
        };
        self.save(None).await?;
        self.configure_forks(None).map_err(internal)?;
        let mut state = self.lock();
        state.vault = None;
        if let Some(replicating) = state.replicating.take() {
            replicating.cancel();
        }
        self.setup.secrets.set_linked(false);
        info!(vault = %vault.address, "unlinked from the vault");
        Ok(CommandResult::Applied)
    }

    /// Writes `vault` as the config file's `[vault]` table, or removes it.
    async fn save(&self, vault: Option<VaultConfig>) -> Result<(), ErrorInfo> {
        let file = self.setup.config_file.clone();
        tokio::task::spawn_blocking(move || config::set_vault(&file, vault.as_ref()))
            .await
            .map_err(|err| error(ErrorCode::Internal, format!("{err}")))?
            .map_err(internal)
    }

    /// Connects to `vault` to do what its switches say, in place of any replicator running;
    /// with both off, it does not connect.
    fn replicate(&self, vault: VaultConfig) -> Result<()> {
        self.configure_forks(Some(vault.clone()))?;
        let stop = if vault.backup || vault.secrets {
            let replicator = self.replicator(vault.clone())?;
            let stop = self.setup.shutdown.child_token();
            tokio::spawn(replicator.run(stop.clone()));
            Some(stop)
        } else {
            None
        };
        self.setup.secrets.set_linked(vault.secrets);
        let mut state = self.lock();
        state.vault = Some(vault);
        if let Some(previous) = std::mem::replace(&mut state.replicating, stop) {
            previous.cancel();
        }
        Ok(())
    }

    fn replicator(&self, vault: VaultConfig) -> Result<Replicator> {
        Ok(Replicator {
            vault,
            device: Replicator::device_key(&self.setup.data_dir)?,
            host: self.setup.host.clone(),
            sessions: self.setup.sessions.clone(),
            changed: Arc::clone(&self.setup.changed),
            secrets: Arc::clone(&self.setup.secrets),
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
        "this host is linked to no vault".to_owned(),
    )
}

/// Runs `call`, which waits on files, services or the vault's socket, on the blocking pool.
async fn blocking<T: Send + 'static>(
    call: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T, ErrorInfo> {
    tokio::task::spawn_blocking(call)
        .await
        .map_err(|err| error(ErrorCode::Internal, format!("{err}")))?
        .map_err(internal)
}

fn internal(err: anyhow::Error) -> ErrorInfo {
    warn!("the vault link failed: {err:#}");
    error(ErrorCode::Internal, format!("{err:#}"))
}
