//! `herder service`: run the daemon as a service that starts with the machine: a systemd user
//! service on Linux, a launchd LaunchAgent on macOS.

use std::process::ExitCode;

use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Subcommand;
use herder_daemon::auth::control::{self, Request, Response};
use herder_daemon::service::{self, Service};

/// How long `install` waits for the new service to answer on its control socket.
const STARTUP: Duration = Duration::from_secs(15);

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
        Action::Install => {
            service::install(&Service::daemon())?;
            offer_ownership()?;
        }
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

/// Once the service answers, shows how to become its owner if it has none yet; a daemon that
/// has an owner says nothing more.
fn offer_ownership() -> Result<()> {
    let config = herder_daemon::Config::load(None)?;
    let started = Instant::now();
    let response = loop {
        match control::request(&config.data_dir, &Request::Own) {
            Ok(response) => break response,
            Err(_) if started.elapsed() < STARTUP => std::thread::sleep(Duration::from_millis(250)),
            Err(err) => {
                eprintln!(
                    "the service did not answer yet ({err:#}); run `herder service install` again to claim it"
                );
                return Ok(());
            }
        }
    };
    match response {
        Response::Own(info) => crate::pair::print_own(&info),
        _ => Ok(()),
    }
}
