//! `herder service`: run the daemon as a service that starts with the machine: a systemd user
//! service on Linux, a launchd LaunchAgent on macOS. `herder vault install` runs a vault the
//! same way, as a service of its own.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
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

/// A service herder installs: the daemon, or a vault next to it.
pub struct Service {
    /// The launchd label.
    pub label: &'static str,
    /// The systemd unit.
    pub unit: &'static str,
    /// What it is called in messages.
    pub name: &'static str,
    /// What it runs, as its unit describes it.
    pub description: &'static str,
    /// The command that writes its service file, named in the file.
    pub installer: &'static str,
    /// The arguments it runs the herder binary with.
    pub args: Vec<String>,
    /// A private file of `NAME=value` lines put in its environment, for variables no other
    /// process should see.
    pub env_file: Option<PathBuf>,
    /// Its log file under `~/Library/Logs/herder` on macOS.
    pub log: &'static str,
}

impl Service {
    /// The herder daemon.
    pub fn daemon() -> Self {
        Self {
            label: launchd::LABEL,
            unit: systemd::UNIT,
            name: "herder service",
            description: "herder daemon",
            installer: "herder service install",
            args: vec!["daemon".to_owned()],
            env_file: None,
            log: "daemon.log",
        }
    }
}

/// Installs and starts `service` for this herder binary.
pub fn install(service: &Service) -> Result<()> {
    if MACOS {
        launchd::install(service)
    } else {
        systemd::install(service)
    }
}

/// Stops and removes `service`.
pub fn uninstall(service: &Service) -> Result<()> {
    if MACOS {
        launchd::uninstall(service)
    } else {
        systemd::uninstall(service)
    }
}

/// Where `service` logs, to point at when it does not come up.
pub fn log_hint(service: &Service) -> Result<String> {
    if MACOS {
        Ok(launchd::log_path(&launchd::home()?, service)
            .display()
            .to_string())
    } else {
        Ok(format!("journalctl --user -u {}", service.unit))
    }
}

/// The `NAME=value` lines of an environment file, as systemd's `EnvironmentFile=` reads them;
/// blank lines and `#` comments are skipped.
fn read_env_file(path: &Path) -> Result<Vec<(String, String)>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut vars = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            bail!("{} has a line without `=`", path.display());
        };
        vars.push((name.trim().to_owned(), value.trim().to_owned()));
    }
    Ok(vars)
}

pub fn run(action: Action) -> Result<ExitCode> {
    match action {
        Action::Install => install(&Service::daemon())?,
        Action::Uninstall => uninstall(&Service::daemon())?,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_file_lines_are_name_value_pairs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.env");
        std::fs::write(&path, "# key\n\nHERDER_VAULT_SECRETS_KEY=YWJj+/==\n").unwrap();
        assert_eq!(
            read_env_file(&path).unwrap(),
            [("HERDER_VAULT_SECRETS_KEY".to_owned(), "YWJj+/==".to_owned())]
        );
        std::fs::write(&path, "oops\n").unwrap();
        assert!(read_env_file(&path).is_err());
    }
}
