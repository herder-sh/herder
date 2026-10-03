//! A host's link to its vault: the `[vault]` table it replicates to, which an owner sets and
//! clears from a client while the daemon runs (`link_vault`, `unlink_vault`).
//!
//! Linking checks the vault first: the daemon connects with its device key and pairs with the
//! host-only code the client got from the vault (`pair_vault_host`), trying the vault's
//! addresses in order. Only once the vault said hello does it write the `[vault]` table, without
//! the spent code, into its config file, keeping the rest of the file as written, and start
//! replicating. Unlinking removes the table and stops replicating; what the vault holds stays
//! there. Unpairing the host's device on the vault (`revoke_vault_host`) is the client's, as an
//! owner of both.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::Result;
use herder_protocol::{CommandBody, CommandResult, ErrorCode, ErrorInfo, LinkedVault};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::Replicator;
use super::fork::FromVault;
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

    /// Applies `get_vault_link`, `link_vault` or `unlink_vault`.
    pub async fn command(&self, command: CommandBody) -> Result<CommandResult, ErrorInfo> {
        match command {
            CommandBody::GetVaultLink => Ok(CommandResult::VaultLink {
                is_vault: false,
                vault: self.vault().map(|vault| LinkedVault {
                    address: vault.address,
                    fingerprint: vault.fingerprint,
                }),
            }),
            CommandBody::LinkVault {
                addresses,
                fingerprint,
                pairing_code,
            } => self.link(addresses, &fingerprint, pairing_code).await,
            CommandBody::UnlinkVault => self.unlink().await,
            _ => Err(error(
                ErrorCode::BadRequest,
                "not a vault link command".to_owned(),
            )),
        }
    }

    async fn link(
        &self,
        addresses: Vec<String>,
        fingerprint: &str,
        pairing_code: String,
    ) -> Result<CommandResult, ErrorInfo> {
        let _changing = self.changing.lock().await;
        if let Some(vault) = self.vault() {
            return Err(error(
                ErrorCode::Conflict,
                format!(
                    "this host backs up to the vault at {} already; stop backing up first",
                    vault.address
                ),
            ));
        }
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
            let vault = VaultConfig {
                address,
                fingerprint: fingerprint.clone(),
                pairing_code: Some(pairing_code.clone()),
            };
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
            ..vault
        };
        self.save(Some(vault.clone())).await?;
        self.replicate(vault.clone()).map_err(internal)?;
        info!(vault = %vault.address, "backing up to the vault");
        Ok(CommandResult::Applied)
    }

    async fn unlink(&self) -> Result<CommandResult, ErrorInfo> {
        let _changing = self.changing.lock().await;
        let Some(vault) = self.vault() else {
            return Err(error(
                ErrorCode::NotFound,
                "this host backs up to no vault".to_owned(),
            ));
        };
        self.save(None).await?;
        self.configure_forks(None).map_err(internal)?;
        let mut state = self.lock();
        state.vault = None;
        if let Some(replicating) = state.replicating.take() {
            replicating.cancel();
        }
        info!(vault = %vault.address, "stopped backing up to the vault");
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

    /// Starts replicating to `vault`, in place of any replicator running.
    fn replicate(&self, vault: VaultConfig) -> Result<()> {
        let replicator = self.replicator(vault.clone())?;
        self.configure_forks(Some(vault.clone()))?;
        let stop = self.setup.shutdown.child_token();
        tokio::spawn(replicator.run(stop.clone()));
        let mut state = self.lock();
        state.vault = Some(vault);
        if let Some(previous) = state.replicating.replace(stop) {
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
            data_dir: self.setup.data_dir.clone(),
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

fn internal(err: anyhow::Error) -> ErrorInfo {
    warn!("the vault link failed: {err:#}");
    error(ErrorCode::Internal, format!("{err:#}"))
}
