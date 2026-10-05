//! `herder service`: run the daemon as a systemd user service that starts at boot.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, Result, bail};
use clap::Subcommand;

const UNIT: &str = "herder.service";

#[derive(Subcommand)]
pub enum Action {
    /// Install, enable and start the systemd user service for this herder binary.
    Install,
    /// Stop, disable and remove the systemd user service.
    Uninstall,
    /// Show the service status.
    Status,
    /// Restart the service.
    Restart,
}

pub fn run(action: Action) -> Result<ExitCode> {
    match action {
        Action::Install => install()?,
        Action::Uninstall => uninstall()?,
        Action::Status => {
            // systemctl's exit code is the answer (0 = running, 3 = stopped), so pass it on.
            let status = Command::new("systemctl")
                .args(["--user", "status", UNIT])
                .status()
                .context("running systemctl")?;
            return Ok(ExitCode::from(
                u8::try_from(status.code().unwrap_or(1)).unwrap_or(1),
            ));
        }
        Action::Restart => restart()?,
    }
    Ok(ExitCode::SUCCESS)
}

/// The systemd user unit that runs `exe daemon`.
pub fn unit_file(exe: &Path) -> Result<String> {
    let exe = exe
        .to_str()
        .with_context(|| format!("{} is not valid UTF-8", exe.display()))?;
    if !exe.starts_with('/') {
        bail!("{exe} is not an absolute path");
    }
    Ok(format!(
        "\
# Written by `herder service install`; rerun it after moving the herder binary.
[Unit]
Description=herder daemon
Documentation=https://github.com/herder-sh/herder

[Service]
Type=exec
ExecStart={} daemon
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
",
        quote_exec_arg(exe)
    ))
}

/// Quotes one ExecStart argument so systemd reads it back verbatim: inside double quotes
/// `\` and `"` are escaped, `%` starts a specifier and `$` an environment variable.
fn quote_exec_arg(arg: &str) -> String {
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    for c in arg.chars() {
        match c {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '%' => quoted.push_str("%%"),
            '$' => quoted.push_str("$$"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

/// `$XDG_CONFIG_HOME/systemd/user/herder.service`, the directory systemd reads user units from.
fn unit_path(xdg_config_home: Option<&OsStr>, home: Option<&OsStr>) -> Result<PathBuf> {
    let config = match xdg_config_home.map(Path::new) {
        Some(dir) if dir.is_absolute() => dir.to_path_buf(),
        _ => match home.map(Path::new) {
            Some(home) if home.is_absolute() => home.join(".config"),
            _ => bail!("cannot find the config dir: HOME is not set to an absolute path"),
        },
    };
    Ok(config.join("systemd/user").join(UNIT))
}

fn install() -> Result<()> {
    let exe = std::env::current_exe().context("finding the herder binary")?;
    let path = unit_path(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )?;
    let dir = path.parent().context("unit path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::write(&path, unit_file(&exe)?)
        .with_context(|| format!("writing {}", path.display()))?;
    println!("wrote {}", path.display());

    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", UNIT])?;
    // restart rather than start, so a reinstall picks up a changed unit.
    systemctl(&["restart", UNIT])?;
    println!("herder service is running");
    enable_linger();
    Ok(())
}

fn uninstall() -> Result<()> {
    let path = unit_path(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )?;
    if !path.exists() {
        println!(
            "herder service is not installed ({} not found)",
            path.display()
        );
        return Ok(());
    }
    systemctl(&["disable", "--now", UNIT])?;
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    systemctl(&["daemon-reload"])?;
    println!("removed {}", path.display());
    Ok(())
}

pub fn restart() -> Result<()> {
    systemctl(&["restart", UNIT])?;
    println!("herder service restarted");
    Ok(())
}

/// Whether the herder service is running right now.
pub fn is_active() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", UNIT])
        .status()
        .is_ok_and(|status| status.success())
}

/// What systemd says of the herder service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    /// The unit file `herder service install` writes exists.
    pub installed: bool,
    /// The unit starts at boot.
    pub enabled: bool,
    /// The service is running right now.
    pub active: bool,
}

/// The herder service's state; `None` on a machine without systemd to ask.
pub fn state() -> Option<State> {
    Command::new("systemctl")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    let installed = unit_path(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
    .is_ok_and(|path| path.is_file());
    let enabled = Command::new("systemctl")
        .args(["--user", "is-enabled", "--quiet", UNIT])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    Some(State {
        installed,
        enabled,
        active: is_active(),
    })
}

fn systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .context("running systemctl; herder service needs systemd")?;
    if !status.success() {
        bail!("systemctl --user {} failed ({status})", args.join(" "));
    }
    Ok(())
}

/// Lingering starts the user's systemd instance at boot instead of at first login, which is
/// what makes the service start at boot. Enabling it may need privileges; then say how.
fn enable_linger() {
    let Some(user) = std::env::var("USER").ok().filter(|user| !user.is_empty()) else {
        println!("to start herder at boot, run: sudo loginctl enable-linger <your user name>");
        return;
    };
    let lingering = Command::new("loginctl")
        .args(["show-user", &user, "--property=Linger", "--value"])
        .output()
        .is_ok_and(|out| out.status.success() && out.stdout.trim_ascii() == b"yes");
    if lingering {
        return;
    }
    let enabled = Command::new("loginctl")
        .args(["enable-linger", &user])
        .status()
        .is_ok_and(|status| status.success());
    if enabled {
        println!("enabled lingering for {user}, so herder starts at boot");
    } else {
        println!("to start herder at boot, run: sudo loginctl enable-linger {user}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_runs_the_daemon_and_restarts_on_failure() {
        let unit = unit_file(Path::new("/home/ann/.local/bin/herder")).unwrap();
        assert_eq!(
            unit,
            "\
# Written by `herder service install`; rerun it after moving the herder binary.
[Unit]
Description=herder daemon
Documentation=https://github.com/herder-sh/herder

[Service]
Type=exec
ExecStart=\"/home/ann/.local/bin/herder\" daemon
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
"
        );
    }

    #[test]
    fn exec_path_is_quoted_for_systemd() {
        let unit = unit_file(Path::new("/opt/my apps/100%/$x/a\"b\\c")).unwrap();
        assert!(
            unit.contains(r#"ExecStart="/opt/my apps/100%%/$$x/a\"b\\c" daemon"#),
            "{unit}"
        );
    }

    #[test]
    fn relative_exe_is_rejected() {
        assert!(unit_file(Path::new("bin/herder")).is_err());
    }

    #[test]
    fn unit_path_follows_xdg_config_home() {
        let path = unit_path(Some(OsStr::new("/x/cfg")), Some(OsStr::new("/home/ann"))).unwrap();
        assert_eq!(path, Path::new("/x/cfg/systemd/user/herder.service"));
    }

    #[test]
    fn unit_path_falls_back_to_home_config() {
        let path = unit_path(Some(OsStr::new("rel")), Some(OsStr::new("/home/ann"))).unwrap();
        assert_eq!(
            path,
            Path::new("/home/ann/.config/systemd/user/herder.service")
        );
        assert!(unit_path(None, None).is_err());
    }
}
