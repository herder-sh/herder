//! The herder service on Linux: a systemd user service that starts at boot.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use anyhow::{Context, Result, bail};

use super::{Service, State};

pub const UNIT: &str = "herder.service";

/// `systemctl --user status`, whose exit code is the answer (0 = running, 3 = stopped).
pub fn status() -> Result<ExitStatus> {
    Command::new("systemctl")
        .args(["--user", "status", UNIT])
        .status()
        .context("running systemctl")
}

/// The systemd user unit that runs `exe` with `service`'s arguments and `path` as its `PATH`.
///
/// A user service started at boot gets only systemd's bare default `PATH`, which leaves out
/// where agent CLIs are usually installed (`~/.local/bin`, version manager shims), so every
/// session would fail to start its CLI. The installing shell's `PATH` is the one the user's
/// CLIs are found on.
fn unit_file(service: &Service, exe: &Path, path: &str) -> Result<String> {
    let exe = absolute_utf8(exe)?;
    let mut exec_start = quote_exec_arg(exe);
    for arg in &service.args {
        exec_start.push(' ');
        exec_start.push_str(&exec_arg(arg));
    }
    let env_file = match &service.env_file {
        Some(file) => format!(
            "EnvironmentFile={}\n",
            absolute_utf8(file)?.replace('%', "%%")
        ),
        None => String::new(),
    };
    Ok(format!(
        "\
# Written by `{installer}`; rerun it after moving the herder binary or
# changing PATH.
[Unit]
Description={description}
Documentation=https://github.com/herder-sh/herder

[Service]
Type=exec
Environment={}
{env_file}ExecStart={exec_start}
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
",
        quote_environment(&format!("PATH={path}")),
        installer = service.installer,
        description = service.description,
    ))
}

fn absolute_utf8(path: &Path) -> Result<&str> {
    let text = path
        .to_str()
        .with_context(|| format!("{} is not valid UTF-8", path.display()))?;
    if !text.starts_with('/') {
        bail!("{text} is not an absolute path");
    }
    Ok(text)
}

/// One ExecStart argument: as it is when systemd reads it back verbatim, quoted otherwise.
fn exec_arg(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=".contains(c));
    if plain {
        arg.to_owned()
    } else {
        quote_exec_arg(arg)
    }
}

/// Quotes one `Environment=` assignment so systemd reads it back verbatim: inside double
/// quotes `\` and `"` are escaped and `%` starts a specifier; `$` is not expanded there.
fn quote_environment(assignment: &str) -> String {
    let mut quoted = String::with_capacity(assignment.len() + 2);
    quoted.push('"');
    for c in assignment.chars() {
        match c {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '%' => quoted.push_str("%%"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
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

/// `$XDG_CONFIG_HOME/systemd/user/<unit>`, in the directory systemd reads user units from.
fn unit_path(xdg_config_home: Option<&OsStr>, home: Option<&OsStr>, unit: &str) -> Result<PathBuf> {
    let config = match xdg_config_home.map(Path::new) {
        Some(dir) if dir.is_absolute() => dir.to_path_buf(),
        _ => match home.map(Path::new) {
            Some(home) if home.is_absolute() => home.join(".config"),
            _ => bail!("cannot find the config dir: HOME is not set to an absolute path"),
        },
    };
    Ok(config.join("systemd/user").join(unit))
}

/// The path of the user unit `unit` in this environment.
fn env_unit_path(unit: &str) -> Result<PathBuf> {
    unit_path(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
        unit,
    )
}

pub fn install(service: &Service) -> Result<()> {
    let exe = std::env::current_exe().context("finding the herder binary")?;
    let search_path = std::env::var("PATH").context("reading PATH")?;
    let path = env_unit_path(service.unit)?;
    let dir = path.parent().context("unit path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::write(&path, unit_file(service, &exe, &search_path)?)
        .with_context(|| format!("writing {}", path.display()))?;
    println!("wrote {}", path.display());

    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", service.unit])?;
    // restart rather than start, so a reinstall picks up a changed unit.
    systemctl(&["restart", service.unit])?;
    println!("{} is running", service.name);
    enable_linger();
    Ok(())
}

pub fn uninstall(service: &Service) -> Result<()> {
    let path = env_unit_path(service.unit)?;
    if !path.exists() {
        println!(
            "{} is not installed ({} not found)",
            service.name,
            path.display()
        );
        return Ok(());
    }
    systemctl(&["disable", "--now", service.unit])?;
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    systemctl(&["daemon-reload"])?;
    println!("removed {}", path.display());
    Ok(())
}

pub fn restart(service: &Service) -> Result<()> {
    systemctl(&["restart", service.unit])?;
    println!("{} restarted", service.name);
    Ok(())
}

pub fn is_installed(service: &Service) -> bool {
    env_unit_path(service.unit).is_ok_and(|path| path.is_file())
}

/// Whether the herder service is running right now.
pub fn is_active() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", UNIT])
        .status()
        .is_ok_and(|status| status.success())
}

/// The herder service's state; `None` on a machine without systemd to ask.
pub fn state() -> Option<State> {
    Command::new("systemctl")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    let installed = is_installed(&Service::daemon());
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
        let unit = unit_file(
            &Service::daemon(),
            Path::new("/home/ann/.local/bin/herder"),
            "/home/ann/.local/bin:/usr/bin",
        )
        .unwrap();
        assert_eq!(
            unit,
            "\
# Written by `herder service install`; rerun it after moving the herder binary or
# changing PATH.
[Unit]
Description=herder daemon
Documentation=https://github.com/herder-sh/herder

[Service]
Type=exec
Environment=\"PATH=/home/ann/.local/bin:/usr/bin\"
ExecStart=\"/home/ann/.local/bin/herder\" daemon
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
"
        );
    }

    #[test]
    fn vault_unit_runs_a_vault_with_its_own_config_and_key() {
        let service = crate::vault::service(Path::new("/home/ann/.local/share/herder-vault"));
        let unit = unit_file(
            &service,
            Path::new("/home/ann/.local/bin/herder"),
            "/usr/bin",
        )
        .unwrap();
        assert_eq!(service.unit, "herder-vault.service");
        assert_eq!(
            unit,
            "\
# Written by `herder vault install`; rerun it after moving the herder binary or
# changing PATH.
[Unit]
Description=herder vault
Documentation=https://github.com/herder-sh/herder

[Service]
Type=exec
Environment=\"PATH=/usr/bin\"
EnvironmentFile=/home/ann/.local/share/herder-vault/vault.env
ExecStart=\"/home/ann/.local/bin/herder\" daemon --vault --config /home/ann/.local/share/herder-vault/vault.toml
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
"
        );
    }

    #[test]
    fn vault_paths_are_quoted_for_systemd() {
        let service = crate::vault::service(Path::new("/home/a b/100%"));
        let unit = unit_file(&service, Path::new("/bin/herder"), "/usr/bin").unwrap();
        assert!(
            unit.contains("EnvironmentFile=/home/a b/100%%/vault.env\n"),
            "{unit}"
        );
        assert!(
            unit.contains(r#"--config "/home/a b/100%%/vault.toml""#),
            "{unit}"
        );
    }

    #[test]
    fn exec_path_is_quoted_for_systemd() {
        let unit = unit_file(
            &Service::daemon(),
            Path::new("/opt/my apps/100%/$x/a\"b\\c"),
            "/usr/bin",
        )
        .unwrap();
        assert!(
            unit.contains(r#"ExecStart="/opt/my apps/100%%/$$x/a\"b\\c" daemon"#),
            "{unit}"
        );
    }

    #[test]
    fn path_is_quoted_for_systemd() {
        let unit = unit_file(
            &Service::daemon(),
            Path::new("/bin/herder"),
            "/opt/100%/$x/a\"b\\c:/usr/bin",
        )
        .unwrap();
        assert!(
            unit.contains(r#"Environment="PATH=/opt/100%%/$x/a\"b\\c:/usr/bin""#),
            "{unit}"
        );
    }

    #[test]
    fn relative_exe_is_rejected() {
        assert!(unit_file(&Service::daemon(), Path::new("bin/herder"), "/usr/bin").is_err());
    }

    #[test]
    fn unit_path_follows_xdg_config_home() {
        let path = unit_path(
            Some(OsStr::new("/x/cfg")),
            Some(OsStr::new("/home/ann")),
            UNIT,
        )
        .unwrap();
        assert_eq!(path, Path::new("/x/cfg/systemd/user/herder.service"));
    }

    #[test]
    fn unit_path_falls_back_to_home_config() {
        let path = unit_path(Some(OsStr::new("rel")), Some(OsStr::new("/home/ann")), UNIT).unwrap();
        assert_eq!(
            path,
            Path::new("/home/ann/.config/systemd/user/herder.service")
        );
        assert!(unit_path(None, None, UNIT).is_err());
    }
}
