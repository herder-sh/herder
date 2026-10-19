//! `herder service`: run the daemon as a service that starts with the machine: a systemd user
//! service on Linux, a launchd LaunchAgent on macOS.

use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;

mod launchd;
mod systemd;

const MACOS: bool = cfg!(target_os = "macos");

#[derive(Subcommand)]
pub enum Action {
    /// Install and start the service for this herder binary, so it starts at boot (Linux) or
    /// login (macOS).
    Install,
    /// Stop and remove the service.
    Uninstall,
    /// Show the service status.
    Status,
    /// Restart the service.
    Restart,
}

pub fn run(action: Action) -> Result<ExitCode> {
    match action {
        Action::Install if MACOS => launchd::install()?,
        Action::Install => systemd::install()?,
        Action::Uninstall if MACOS => launchd::uninstall()?,
        Action::Uninstall => systemd::uninstall()?,
        Action::Status => {
            // The service manager's exit code is the answer, so pass it on.
            let status = if MACOS {
                launchd::status()?
            } else {
                systemd::status()?
            };
            return Ok(ExitCode::from(
                u8::try_from(status.code().unwrap_or(1)).unwrap_or(1),
            ));
        }
        Action::Restart => restart()?,
    }
    Ok(ExitCode::SUCCESS)
}

pub fn restart() -> Result<()> {
    if MACOS {
        launchd::restart()
    } else {
        systemd::restart()
    }
}

/// Whether the herder service is running right now.
pub fn is_active() -> bool {
    if MACOS {
        launchd::is_active()
    } else {
        systemd::is_active()
    }
}

/// What the service manager says of the herder service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    /// The unit file or plist `herder service install` writes exists.
    pub installed: bool,
    /// The service starts at boot (Linux) or login (macOS).
    pub enabled: bool,
    /// The service is running right now.
    pub active: bool,
}

/// The herder service's state; `None` on a machine without a service manager to ask.
pub fn state() -> Option<State> {
    if MACOS {
        Some(launchd::state())
    } else {
        systemd::state()
    }
}
