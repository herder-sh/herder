//! `herder service`: run the daemon as a service that starts with the machine: a systemd user
//! service on Linux, a launchd LaunchAgent on macOS.

use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;
use herder_daemon::service::{self, Service};

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
        Action::Install => service::install(&Service::daemon())?,
        Action::Uninstall => service::uninstall(&Service::daemon())?,
        Action::Status => {
            // The service manager's exit code is the answer, so pass it on.
            let status = service::status()?;
            return Ok(ExitCode::from(
                u8::try_from(status.code().unwrap_or(1)).unwrap_or(1),
            ));
        }
        Action::Restart => service::restart(&Service::daemon())?,
    }
    Ok(ExitCode::SUCCESS)
}
