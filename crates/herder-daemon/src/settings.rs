//! The daemon's settings as owners read and change them from a client: every daemon-wide key
//! of its config file ([`crate::config::set_settings`]).
//!
//! A change is written to the file at once. The turn limit applies at once too; everything
//! else applies when the daemon next starts, which `restart_daemon` does without anyone at
//! the machine. The answer says whether the file holds settings not in effect yet, whoever
//! wrote them.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use herder_protocol::{CommandBody, CommandResult, DaemonSettings, ErrorCode, ErrorInfo};
use nix::ifaddrs::getifaddrs;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::config::{self, Config, Mode};
use crate::resources::{self, Admission};

/// Set once an owner asked the daemon to restart; [`crate::run`] reports it when it returns.
static RESTART: AtomicBool = AtomicBool::new(false);

/// How long a restart waits after answering, so the answer reaches the client first.
const RESTART_DELAY: Duration = Duration::from_millis(250);

/// Whether an owner asked the daemon to restart.
pub(crate) fn restart_requested() -> bool {
    RESTART.load(Ordering::SeqCst)
}

/// The daemon's settings: its config file, and what of it is in effect.
pub struct Settings {
    path: PathBuf,
    data_dir: PathBuf,
    is_vault: bool,
    /// The settings in effect: the file's as the daemon started, with the turn limit as it
    /// was changed since. Locked across a change, so changes apply one at a time.
    running: Mutex<DaemonSettings>,
    /// What admits turns on a host, which a new turn limit applies to.
    admission: Option<Arc<Admission>>,
    /// Stops the daemon, for a restart.
    shutdown: CancellationToken,
}

impl Settings {
    /// The settings of the daemon `config` started, with turns admitted by `admission` on a
    /// host; a restart cancels `shutdown`.
    pub fn new(
        config: &Config,
        admission: Option<Arc<Admission>>,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            path: config.path.clone(),
            data_dir: config.data_dir.clone(),
            is_vault: config.mode == Mode::Vault,
            running: Mutex::new(config.settings()),
            admission,
            shutdown,
        }
    }

    /// Answers `get_settings`, `set_settings`, `set_resource_limits` and `restart_daemon`.
    pub async fn command(&self, command: CommandBody) -> Result<CommandResult, ErrorInfo> {
        match command {
            CommandBody::GetSettings => {
                let running = self.running.lock().await;
                let file = self.load().await?;
                Ok(self.answer(file.settings(), &running))
            }
            CommandBody::SetSettings { settings } => self.set(*settings).await,
            CommandBody::SetResourceLimits { max_turns } => {
                let limit = herder_protocol::MAX_TURNS_LIMIT;
                if !(1..=limit).contains(&max_turns) {
                    return Err(error(
                        ErrorCode::BadRequest,
                        format!("turns at once must be 1 to {limit}"),
                    ));
                }
                if self.admission.is_none() {
                    return Err(error(
                        ErrorCode::Unsupported,
                        "this daemon admits turns without a limit",
                    ));
                }
                let mut settings = self.load().await?.settings();
                settings.resources.max_turns = Some(max_turns);
                self.set(settings).await?;
                Ok(CommandResult::Applied)
            }
            CommandBody::RestartDaemon => {
                info!("an owner asked the daemon to restart");
                RESTART.store(true, Ordering::SeqCst);
                let shutdown = self.shutdown.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(RESTART_DELAY).await;
                    shutdown.cancel();
                });
                Ok(CommandResult::Applied)
            }
            _ => Err(error(
                ErrorCode::Internal,
                "the settings do not handle this command",
            )),
        }
    }

    /// Writes `new` to the config file and applies what applies at once.
    async fn set(&self, new: DaemonSettings) -> Result<CommandResult, ErrorInfo> {
        let mut running = self.running.lock().await;
        let file = self.load().await?;
        let old = file.settings();
        // Checked only when it changes: the address in effect may be down for a while.
        if new.listen != old.listen {
            for address in &new.listen {
                let listen: SocketAddr = address.parse().map_err(|_| {
                    error(
                        ErrorCode::BadRequest,
                        format!("listen: {address:?} is not an ip:port address"),
                    )
                })?;
                if !is_local(listen.ip()) {
                    return Err(error(
                        ErrorCode::BadRequest,
                        format!("listen: {} is not an address of this machine", listen.ip()),
                    ));
                }
            }
        }
        let backup = |settings: &DaemonSettings| {
            (settings.backup.attachments, settings.backup.attachments_cap)
        };
        if file.vault.is_none() && !self.is_vault && backup(&new) != backup(&old) {
            return Err(error(
                ErrorCode::BadRequest,
                "images are backed up only while the daemon is linked to a vault",
            ));
        }
        let path = self.path.clone();
        let config = tokio::task::spawn_blocking(move || config::set_settings(&path, &old, &new))
            .await
            .map_err(|err| error(ErrorCode::Internal, format!("{err}")))?
            .map_err(|err| error(ErrorCode::BadRequest, format!("{err:#}")))?;
        let settings = config.settings();
        if let Some(admission) = &self.admission
            && settings.resources.max_turns != running.resources.max_turns
        {
            let max_turns = config.resources.budget(resources::cores()).max_turns;
            admission.set_max_turns(max_turns);
            running.resources.max_turns = settings.resources.max_turns;
            info!(max_turns, "the turn limit changed");
        }
        info!("an owner changed the daemon's settings");
        Ok(self.answer(settings, &running))
    }

    /// The config file as the daemon would load it now.
    async fn load(&self) -> Result<Config, ErrorInfo> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || Config::load_file(&path))
            .await
            .map_err(|err| error(ErrorCode::Internal, format!("{err}")))?
            .map_err(|err| error(ErrorCode::Internal, format!("{err:#}")))
    }

    fn answer(&self, settings: DaemonSettings, running: &DaemonSettings) -> CommandResult {
        CommandResult::Settings {
            restart_required: settings != *running,
            settings: Box::new(settings),
            data_dir: self.data_dir.display().to_string(),
            is_vault: self.is_vault,
        }
    }
}

/// Whether the daemon can listen on `ip`: every address, loopback, or one of this machine's.
fn is_local(ip: IpAddr) -> bool {
    if ip.is_unspecified() || ip.is_loopback() {
        return true;
    }
    getifaddrs().into_iter().flatten().any(|interface| {
        interface.address.is_some_and(|address| {
            address.as_sockaddr_in().map(|v4| IpAddr::V4(v4.ip())) == Some(ip)
                || address.as_sockaddr_in6().map(|v6| IpAddr::V6(v6.ip())) == Some(ip)
        })
    })
}

fn error(code: ErrorCode, message: impl Into<String>) -> ErrorInfo {
    ErrorInfo {
        code,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn other_settings_change_while_the_address_in_effect_is_down() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("daemon.toml");
        // TEST-NET-3: as a Tailscale address is while Tailscale is down.
        std::fs::write(&path, "listen = \"203.0.113.9:7447\"\n").unwrap();
        let config = Config::load_file(&path).unwrap();
        let settings = Settings::new(&config, None, CancellationToken::new());
        let mut new = config.settings();
        new.failover.pin = true;
        let set = CommandBody::SetSettings {
            settings: Box::new(new),
        };
        settings.command(set).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "listen = \"203.0.113.9:7447\"\n\n[failover]\npin = true\n"
        );
    }
}
