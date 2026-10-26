//! `update_daemon`: an owner updates the daemon's herder from a client, as `herder update` does
//! on the machine.
//!
//! The update holds new turns ([`Admission::hold`]), waits for the running ones to end when
//! asked to, downloads the latest release ([`release`]) and puts it in place of the binary.
//! Each stage reaches clients as `daemon_update` through the [`Hub`]. Then the daemon stops
//! with [`crate::Exit::Updated`], and the herder binary restarts it onto the new binary, and
//! an installed vault next to it.

pub mod release;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use herder_protocol::{CommandResult, ErrorCode, ErrorInfo, UpdateStage, UpdateWhen};
use semver::Version;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::hub::Hub;
use crate::resources::Admission;

/// Set once the binary was updated; [`crate::run`] reports it when it returns.
static UPDATED: AtomicBool = AtomicBool::new(false);

/// How long the daemon waits after `restarting`, so it reaches clients first.
const RESTART_DELAY: Duration = Duration::from_millis(250);

/// Whether the binary was updated, so the daemon restarts onto it.
pub(crate) fn updated() -> bool {
    UPDATED.load(Ordering::SeqCst)
}

/// What `update_daemon` replaces, and where it downloads from. The herder binary sets it when
/// it runs the daemon as the service herder installed, which restarts onto a new binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfUpdate {
    /// The herder binary.
    pub exe: PathBuf,
    /// Where releases are downloaded from ([`release::download_base`]).
    pub base: String,
}

/// Runs owners' updates, one at a time.
pub struct Updater {
    update: Option<SelfUpdate>,
    /// What admits turns on a host; a vault runs none.
    admission: Option<Arc<Admission>>,
    /// Stops the daemon, for the restart.
    shutdown: CancellationToken,
    /// The update underway, woken to stop waiting for turns.
    underway: Mutex<Option<Arc<Notify>>>,
}

impl Updater {
    /// Updates `update`'s binary, holding the turns `admission` admits; the restart cancels
    /// `shutdown`. Without `update`, every update is refused.
    pub fn new(
        update: Option<SelfUpdate>,
        admission: Option<Arc<Admission>>,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            update,
            admission,
            shutdown,
            underway: Mutex::new(None),
        }
    }

    /// Answers `update_daemon`: starts the update, telling `hub` how it goes, and answers at
    /// once, or refuses it.
    pub fn start(
        self: &Arc<Self>,
        when: UpdateWhen,
        hub: &Arc<Hub>,
    ) -> Result<CommandResult, ErrorInfo> {
        let Some(update) = self.update.clone() else {
            return Err(error(
                ErrorCode::Unsupported,
                "this daemon does not run as the service herder installed, so it cannot restart \
                 onto a new herder; run herder update on its machine",
            ));
        };
        release::check_writable(&update.exe).map_err(|err| {
            error(
                ErrorCode::Unsupported,
                format!("cannot replace the herder binary: {err:#}"),
            )
        })?;
        let mut underway = self.underway.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(now) = &*underway {
            if when == UpdateWhen::Now {
                now.notify_one();
                return Ok(CommandResult::Applied);
            }
            return Err(error(ErrorCode::Conflict, "an update is underway already"));
        }
        let now = Arc::new(Notify::new());
        *underway = Some(Arc::clone(&now));
        drop(underway);
        info!(?when, "an owner asked the daemon to update");
        let updater = Arc::clone(self);
        let hub = Arc::clone(hub);
        tokio::spawn(async move {
            let stage = updater.run(update, when, &now, &hub).await;
            let restarting = matches!(stage, UpdateStage::Restarting { .. });
            match &stage {
                UpdateStage::Failed { reason } => warn!("the update failed: {reason}"),
                stage => info!(?stage, "the update ended"),
            }
            hub.daemon_update(stage);
            if restarting {
                UPDATED.store(true, Ordering::SeqCst);
                tokio::time::sleep(RESTART_DELAY).await;
                updater.shutdown.cancel();
            } else {
                *updater
                    .underway
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = None;
            }
        });
        Ok(CommandResult::Applied)
    }

    /// Runs the update up to its last stage: `restarting` once the new binary is in place.
    /// New turns wait until the daemon restarts, or start again when it does not.
    async fn run(
        &self,
        update: SelfUpdate,
        when: UpdateWhen,
        now: &Notify,
        hub: &Arc<Hub>,
    ) -> UpdateStage {
        let hold = self.admission.as_ref().map(|admission| admission.hold());
        if let (UpdateWhen::Idle, Some(hold)) = (when, &hold) {
            let waiting = hold.idle(|running_turns| {
                hub.daemon_update(UpdateStage::WaitingForIdle { running_turns });
            });
            tokio::select! {
                () = waiting => {}
                () = now.notified() => info!("an owner asked to update now"),
            }
        }
        let hub = Arc::clone(hub);
        let installed = tokio::task::spawn_blocking(move || install(&update, &hub)).await;
        let stage = match installed {
            Ok(Ok(Some((from, to)))) => UpdateStage::Restarting {
                from: from.to_string(),
                to: to.to_string(),
            },
            Ok(Ok(None)) => UpdateStage::UpToDate {
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
            Ok(Err(err)) => UpdateStage::Failed {
                reason: format!("{err:#}"),
            },
            Err(err) => UpdateStage::Failed {
                reason: err.to_string(),
            },
        };
        if matches!(stage, UpdateStage::Restarting { .. }) {
            // Held until the daemon stops, so no turn starts that the restart would end.
            std::mem::forget(hold);
        }
        stage
    }
}

/// Puts the latest release in place of `update`'s binary, telling `hub` it downloads; returns
/// the versions it updated from and to, or `None` when the latest runs already.
fn install(update: &SelfUpdate, hub: &Hub) -> anyhow::Result<Option<(Version, Version)>> {
    let current = release::current()?;
    let target = release::this_target()?;
    let latest = release::latest(&update.base)?;
    if latest <= current {
        return Ok(None);
    }
    hub.daemon_update(UpdateStage::Downloading {
        from: current.to_string(),
        to: latest.to_string(),
    });
    let url = release::asset_url(&update.base, &latest, target);
    let binary = release::download(&url, &latest, target)?;
    release::replace_exe(&update.exe, &binary)?;
    info!(
        "updated {} from {current} to {latest}",
        update.exe.display()
    );
    Ok(Some((current, latest)))
}

fn error(code: ErrorCode, message: impl Into<String>) -> ErrorInfo {
    ErrorInfo {
        code,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests;
