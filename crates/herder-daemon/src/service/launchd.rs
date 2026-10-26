//! The herder service on macOS: a launchd LaunchAgent that starts at login and is restarted
//! whenever it exits.
//!
//! A LaunchAgent rather than a LaunchDaemon: it runs as the user in their login session, where
//! the vendor CLIs find their logins in the user's keychain.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::{Service, State};

pub const LABEL: &str = "sh.herder.daemon";

/// Where the vendor CLIs are usually installed, put on the service's `PATH` after the
/// installing shell's own entries. `~` is the user's home.
const EXTRA_PATH: &[&str] = &[
    "~/.local/bin",
    "~/.cargo/bin",
    "/opt/homebrew/bin",
    "/opt/homebrew/sbin",
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
];

/// The LaunchAgent that runs `exe` with `service`'s arguments, `path` as its `PATH` and `env`
/// in its environment, logging to `log`.
///
/// launchd starts agents with a bare `PATH` (`/usr/bin:/bin:/usr/sbin:/sbin`), which leaves out
/// where agent CLIs are installed, so the plist carries one. The daemon logs to stderr; without
/// a log file launchd drops it.
fn plist(
    service: &Service,
    exe: &Path,
    path: &str,
    env: &[(String, String)],
    log: &Path,
) -> Result<String> {
    let exe = absolute_utf8(exe)?;
    let log = absolute_utf8(log)?;
    let mut arguments = format!("\t\t<string>{}</string>\n", escape_xml(exe));
    for arg in &service.args {
        arguments.push_str(&format!("\t\t<string>{}</string>\n", escape_xml(arg)));
    }
    let mut variables = format!(
        "\t\t<key>PATH</key>\n\t\t<string>{}</string>\n",
        escape_xml(path)
    );
    for (name, value) in env {
        variables.push_str(&format!(
            "\t\t<key>{}</key>\n\t\t<string>{}</string>\n",
            escape_xml(name),
            escape_xml(value)
        ));
    }
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Written by `{installer}`; rerun it after moving the herder binary or
     changing PATH. -->
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>ProgramArguments</key>
	<array>
{arguments}	</array>
	<key>EnvironmentVariables</key>
	<dict>
{variables}	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#,
        installer = service.installer,
        label = escape_xml(service.label),
        log = escape_xml(log),
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

fn escape_xml(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            c => escaped.push(c),
        }
    }
    escaped
}

/// The installing shell's `PATH`, then each of [`EXTRA_PATH`] it lacks.
fn service_path(shell_path: &str, home: &Path) -> String {
    let home = home.to_string_lossy();
    let mut dirs: Vec<String> = Vec::new();
    let extra = EXTRA_PATH.iter().map(|dir| match dir.strip_prefix('~') {
        Some(rest) => format!("{home}{rest}"),
        None => (*dir).to_owned(),
    });
    for dir in shell_path.split(':').map(str::to_owned).chain(extra) {
        if !dir.is_empty() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs.join(":")
}

pub fn home() -> Result<PathBuf> {
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) if home.is_absolute() => Ok(home),
        _ => bail!("cannot find the home dir: HOME is not set to an absolute path"),
    }
}

/// `~/Library/LaunchAgents/<label>.plist`, where launchd loads user agents from at login.
fn plist_path(home: &Path, label: &str) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{label}.plist"))
}

pub fn log_path(home: &Path, service: &Service) -> PathBuf {
    home.join("Library/Logs/herder").join(service.log)
}

/// The user's login session, which LaunchAgents run in.
fn domain() -> String {
    format!("gui/{}", nix::unistd::getuid())
}

/// The service labelled `label` in the user's login session.
fn target(label: &str) -> String {
    format!("{}/{label}", domain())
}

/// The herder service in the user's login session.
fn service() -> String {
    target(LABEL)
}

pub fn install(service: &Service) -> Result<()> {
    let exe = crate::herder_binary()?;
    let home = home()?;
    let search_path = service_path(&std::env::var("PATH").context("reading PATH")?, &home);
    let env = match &service.env_file {
        Some(file) => super::read_env_file(file)?,
        None => Vec::new(),
    };
    let path = plist_path(&home, service.label);
    let log = log_path(&home, service);
    for dir in [path.parent(), log.parent()].into_iter().flatten() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    write_private(&path, &plist(service, &exe, &search_path, &env, &log)?)?;
    println!("wrote {}", path.display());

    // Unload a running service first, so a reinstall picks up a changed plist.
    bootout(service.label)?;
    // Undo an earlier `launchctl disable`, which would keep the agent from loading.
    launchctl(&["enable", &target(service.label)])?;
    launchctl(&["bootstrap", &domain(), &path.to_string_lossy()])?;
    println!(
        "{} is running and starts again at every login",
        service.name
    );
    println!("it logs to {}", log.display());
    Ok(())
}

/// Writes `contents` to `path` readable by this user only: a plist may carry a service's
/// private environment.
fn write_private(path: &Path, contents: &str) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    // `mode` applies only to a new file.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .and_then(|()| file.write_all(contents.as_bytes()))
        .with_context(|| format!("writing {}", path.display()))
}

pub fn uninstall(service: &Service) -> Result<()> {
    let path = plist_path(&home()?, service.label);
    if !path.exists() {
        println!(
            "{} is not installed ({} not found)",
            service.name,
            path.display()
        );
        return Ok(());
    }
    bootout(service.label)?;
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    println!("removed {}", path.display());
    Ok(())
}

/// `launchctl print`, whose exit code is the answer (0 = loaded, 113 = not loaded).
pub fn status() -> Result<ExitStatus> {
    Command::new("launchctl")
        .args(["print", &service()])
        .status()
        .context("running launchctl")
}

pub fn restart(service: &Service) -> Result<()> {
    launchctl(&["kickstart", "-k", &target(service.label)])?;
    println!("{} restarted", service.name);
    Ok(())
}

pub fn is_installed(service: &Service) -> bool {
    home().is_ok_and(|home| plist_path(&home, service.label).is_file())
}

/// Whether launchd runs this process as `service`: it names the job in `XPC_SERVICE_NAME`.
pub fn runs_this_process(service: &Service) -> bool {
    std::env::var("XPC_SERVICE_NAME").is_ok_and(|name| name == service.label)
}

/// Whether the herder service is running right now.
pub fn is_active() -> bool {
    print(LABEL).is_some_and(|out| running(&out))
}

pub fn state() -> State {
    let installed = is_installed(&Service::daemon());
    let disabled = Command::new("launchctl")
        .args(["print-disabled", &domain()])
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|out| disabled(&String::from_utf8_lossy(&out.stdout)));
    State {
        installed,
        // An installed agent loads at every login unless it is disabled.
        enabled: installed && !disabled,
        active: is_active(),
    }
}

/// `launchctl print` of the service labelled `label`; `None` while it is not loaded.
fn print(label: &str) -> Option<String> {
    let Output { status, stdout, .. } = Command::new("launchctl")
        .args(["print", &target(label)])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    status
        .success()
        .then(|| String::from_utf8_lossy(&stdout).into_owned())
}

/// Whether `launchctl print` says the service's process is running.
fn running(print: &str) -> bool {
    print.lines().any(|line| line.trim() == "state = running")
}

/// Whether `launchctl print-disabled` lists the herder service as disabled. macOS 13 and later
/// print `"sh.herder.daemon" => disabled`; earlier versions `=> true`.
fn disabled(print_disabled: &str) -> bool {
    let quoted = format!("\"{LABEL}\"");
    print_disabled.lines().any(|line| {
        line.trim()
            .strip_prefix(&quoted)
            .and_then(|rest| rest.trim().strip_prefix("=>"))
            .is_some_and(|value| matches!(value.trim(), "disabled" | "true"))
    })
}

/// Unloads the service if it is loaded, and waits until launchd has let go of it: bootstrapping
/// it again before then fails.
fn bootout(label: &str) -> Result<()> {
    if print(label).is_none() {
        return Ok(());
    }
    launchctl(&["bootout", &target(label)])?;
    for _ in 0..100 {
        if print(label).is_none() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!("launchd did not unload {label} within 10s")
}

fn launchctl(args: &[&str]) -> Result<()> {
    let status = Command::new("launchctl")
        .args(args)
        .status()
        .context("running launchctl")?;
    if !status.success() {
        bail!("launchctl {} failed ({status})", args.join(" "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_runs_the_daemon_at_load_and_keeps_it_alive() {
        let plist = plist(
            &Service::daemon(),
            Path::new("/Users/ann/.local/bin/herder"),
            "/Users/ann/.local/bin:/usr/bin",
            &[],
            Path::new("/Users/ann/Library/Logs/herder/daemon.log"),
        )
        .unwrap();
        assert_eq!(
            plist,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Written by `herder service install`; rerun it after moving the herder binary or
     changing PATH. -->
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>sh.herder.daemon</string>
	<key>ProgramArguments</key>
	<array>
		<string>/Users/ann/.local/bin/herder</string>
		<string>daemon</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>PATH</key>
		<string>/Users/ann/.local/bin:/usr/bin</string>
	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>StandardOutPath</key>
	<string>/Users/ann/Library/Logs/herder/daemon.log</string>
	<key>StandardErrorPath</key>
	<string>/Users/ann/Library/Logs/herder/daemon.log</string>
</dict>
</plist>
"#
        );
    }

    #[test]
    fn vault_plist_runs_a_vault_with_its_key_in_its_environment() {
        let service =
            crate::vault::install::service(Path::new("/Users/ann/.local/share/herder-vault"));
        let home = Path::new("/Users/ann");
        let plist = plist(
            &service,
            Path::new("/Users/ann/.local/bin/herder"),
            "/usr/bin",
            &[("HERDER_VAULT_SECRETS_KEY".to_owned(), "a2V5".to_owned())],
            &log_path(home, &service),
        )
        .unwrap();
        assert_eq!(
            plist_path(home, service.label),
            Path::new("/Users/ann/Library/LaunchAgents/sh.herder.vault.plist")
        );
        assert_eq!(
            plist,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Written by `herder vault install`; rerun it after moving the herder binary or
     changing PATH. -->
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>sh.herder.vault</string>
	<key>ProgramArguments</key>
	<array>
		<string>/Users/ann/.local/bin/herder</string>
		<string>daemon</string>
		<string>--vault</string>
		<string>--config</string>
		<string>/Users/ann/.local/share/herder-vault/vault.toml</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>PATH</key>
		<string>/usr/bin</string>
		<key>HERDER_VAULT_SECRETS_KEY</key>
		<string>a2V5</string>
	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>StandardOutPath</key>
	<string>/Users/ann/Library/Logs/herder/vault.log</string>
	<key>StandardErrorPath</key>
	<string>/Users/ann/Library/Logs/herder/vault.log</string>
</dict>
</plist>
"#
        );
    }

    #[test]
    fn plists_are_written_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.plist");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&path, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn plist_strings_are_escaped() {
        let plist = plist(
            &Service::daemon(),
            Path::new("/Apps/R&D/<x>/herder"),
            "/a\"b/'c':/usr/bin",
            &[("K&Y".to_owned(), "a<b".to_owned())],
            Path::new("/l&g"),
        )
        .unwrap();
        assert!(
            plist.contains("<string>/Apps/R&amp;D/&lt;x&gt;/herder</string>"),
            "{plist}"
        );
        assert!(
            plist.contains("<string>/a&quot;b/&apos;c&apos;:/usr/bin</string>"),
            "{plist}"
        );
        assert!(plist.contains("<string>/l&amp;g</string>"), "{plist}");
        assert!(
            plist.contains("<key>K&amp;Y</key>\n\t\t<string>a&lt;b</string>"),
            "{plist}"
        );
    }

    #[test]
    fn relative_paths_are_rejected() {
        let daemon = Service::daemon();
        let log = Path::new("/l");
        assert!(plist(&daemon, Path::new("bin/herder"), "/usr/bin", &[], log).is_err());
        assert!(
            plist(
                &daemon,
                Path::new("/bin/herder"),
                "/usr/bin",
                &[],
                Path::new("l")
            )
            .is_err()
        );
    }

    #[test]
    fn path_keeps_the_shell_path_and_adds_where_clis_live() {
        let path = service_path(
            "/Users/ann/.nvm/bin:/usr/bin::/Users/ann/.local/bin",
            Path::new("/Users/ann"),
        );
        assert_eq!(
            path,
            "/Users/ann/.nvm/bin:/usr/bin:/Users/ann/.local/bin:/Users/ann/.cargo/bin:\
             /opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/bin:/usr/sbin:/sbin"
        );
    }

    #[test]
    fn paths_live_under_the_users_library() {
        let home = Path::new("/Users/ann");
        assert_eq!(
            plist_path(home, LABEL),
            Path::new("/Users/ann/Library/LaunchAgents/sh.herder.daemon.plist")
        );
        assert_eq!(
            log_path(home, &Service::daemon()),
            Path::new("/Users/ann/Library/Logs/herder/daemon.log")
        );
    }

    #[test]
    fn running_reads_the_service_state() {
        let print =
            "gui/501/sh.herder.daemon = {\n\tactive count = 1\n\tstate = running\n\tpid = 42\n}\n";
        assert!(running(print));
        let stopped = "gui/501/sh.herder.daemon = {\n\tstate = not running\n}\n";
        assert!(!running(stopped));
    }

    #[test]
    fn disabled_reads_both_print_disabled_formats() {
        let modern = "disabled services = {\n\t\"com.apple.x\" => enabled\n\t\"sh.herder.daemon\" => disabled\n}\n";
        assert!(disabled(modern));
        let old = "disabled services = {\n\t\"sh.herder.daemon\" => true\n}\n";
        assert!(disabled(old));
        let enabled = "disabled services = {\n\t\"sh.herder.daemon\" => enabled\n\t\"sh.herder.daemonx\" => disabled\n}\n";
        assert!(!disabled(enabled));
        assert!(!disabled(""));
    }
}
