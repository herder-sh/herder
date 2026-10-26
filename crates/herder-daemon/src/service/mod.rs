//! Services herder runs as, starting with the machine: a systemd user service on Linux, a
//! launchd LaunchAgent on macOS. `herder service install` runs the daemon so; a vault, which
//! `herder vault install` or an owner's `install_vault` installs, runs as a service of its own.

use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use anyhow::{Context, Result, bail};

mod launchd;
mod systemd;

const MACOS: bool = cfg!(target_os = "macos");

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

/// Restarts `service`, starting it if it was stopped.
pub fn restart(service: &Service) -> Result<()> {
    if MACOS {
        launchd::restart(service)
    } else {
        systemd::restart(service)
    }
}

/// The herder service's status, as the service manager prints it.
pub fn status() -> Result<ExitStatus> {
    if MACOS {
        launchd::status()
    } else {
        systemd::status()
    }
}

/// Whether `service`'s service file is installed.
pub fn is_installed(service: &Service) -> bool {
    if MACOS {
        launchd::is_installed(service)
    } else {
        systemd::is_installed(service)
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

/// What installs services: this machine's service manager, or a fake in tests.
pub trait Services: Send + Sync {
    /// Installs and starts `service`.
    fn install(&self, service: &Service) -> Result<()>;
    /// Whether `service` is installed.
    fn is_installed(&self, service: &Service) -> bool;
    /// Where `service` logs, to point at when it does not come up.
    fn log_hint(&self, service: &Service) -> String;
}

/// This machine's service manager: launchd on macOS, systemd elsewhere.
pub struct System;

impl Services for System {
    fn install(&self, service: &Service) -> Result<()> {
        install(service)
    }

    fn is_installed(&self, service: &Service) -> bool {
        is_installed(service)
    }

    fn log_hint(&self, service: &Service) -> String {
        log_hint(service).unwrap_or_else(|_| "its log".to_owned())
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
