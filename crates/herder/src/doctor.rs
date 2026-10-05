//! `herder doctor`: checks that this machine is ready to run herder, one line per check, with
//! how to fix each one that does not pass. Run it until it passes.
//!
//! Logins are checked with each provider's own status command, run under the account's config
//! dir, as the login flow does ([`herder_daemon::login`]); herder never reads the dir.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{self, ErrorKind};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output, Stdio};
use std::time::{Duration, Instant};

use herder_daemon::Config;
use herder_daemon::config::VaultConfig;
use herder_daemon::login::{self, LoginProgram};
use herder_daemon::session::AccountConfig;
use herder_protocol::{AccountId, Provider};

use crate::service;

/// How long a CLI's `--version` or `gh auth status` may take.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a TCP connect to the daemon or the vault may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// How a check came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pass,
    Warn,
    Fail,
}

/// One line of the checklist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub status: Status,
    /// What was checked, such as `git` or `account claude-main`.
    pub subject: String,
    /// What was found.
    pub detail: String,
    /// How to fix it, when it did not pass.
    pub hint: Option<String>,
}

impl Check {
    fn pass(subject: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(Status::Pass, subject, detail, None)
    }

    fn warn(subject: impl Into<String>, detail: impl Into<String>, hint: String) -> Self {
        Self::new(Status::Warn, subject, detail, Some(hint))
    }

    fn fail(subject: impl Into<String>, detail: impl Into<String>, hint: String) -> Self {
        Self::new(Status::Fail, subject, detail, Some(hint))
    }

    fn new(
        status: Status,
        subject: impl Into<String>,
        detail: impl Into<String>,
        hint: Option<String>,
    ) -> Self {
        Self {
            status,
            subject: subject.into(),
            detail: detail.into(),
            hint,
        }
    }
}

/// The programs checked on `PATH`; tests point them at fakes.
struct Tools {
    git: PathBuf,
    gh: PathBuf,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            git: PathBuf::from("git"),
            gh: PathBuf::from("gh"),
        }
    }
}

/// Runs every check against the config at `explicit`, else the daemon's default, and prints
/// them; exits with 1 when one failed.
pub fn run(explicit: Option<PathBuf>) -> ExitCode {
    let checks = checks(explicit.as_deref(), &Tools::default(), service::state());
    print!("{}", render(&checks));
    exit_code(&checks)
}

/// Every check, in order. Those that need the config are left out when it cannot be read.
fn checks(explicit: Option<&Path>, tools: &Tools, service: Option<service::State>) -> Vec<Check> {
    let (check, config) = config_check(explicit);
    let mut checks = vec![check];
    if let Some(config) = &config {
        checks.extend(provider_checks(config));
    }
    checks.extend(tool_checks(tools));
    checks.push(service_check(service));
    if let Some(config) = &config {
        checks.extend(config.listen.iter().map(|&address| listen_check(address)));
        checks.extend(config.vault.as_ref().map(vault_check));
    }
    checks
}

/// 1 when a check failed, else 0: warnings do not fail.
fn exit_code(checks: &[Check]) -> ExitCode {
    if checks.iter().any(|check| check.status == Status::Fail) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// One line per check, then a summary line.
fn render(checks: &[Check]) -> String {
    let mut out = String::new();
    for check in checks {
        let marker = match check.status {
            Status::Pass => '✓',
            Status::Warn => '!',
            Status::Fail => '✗',
        };
        let _ = write!(out, "{marker} {}: {}", check.subject, check.detail);
        if let Some(hint) = &check.hint {
            let _ = write!(out, " — {hint}");
        }
        out.push('\n');
    }
    let count = |status| checks.iter().filter(|c| c.status == status).count();
    let (failed, warned) = (count(Status::Fail), count(Status::Warn));
    if failed == 0 && warned == 0 {
        let _ = writeln!(out, "all {} checks passed", checks.len());
    } else {
        let plural = if warned == 1 { "" } else { "s" };
        let _ = writeln!(out, "{failed} failed, {warned} warning{plural}");
    }
    out
}

/// The config file exists and parses. Without `explicit`, a missing default file is a failure,
/// as no account is set up yet, but the defaults the daemon runs on are returned for the
/// checks that follow.
fn config_check(explicit: Option<&Path>) -> (Check, Option<Config>) {
    const SUBJECT: &str = "config";
    if let Some(path) = explicit
        && !path.exists()
    {
        let check = Check::fail(
            SUBJECT,
            format!("{} not found", path.display()),
            "create it, or pass the daemon's config file with --config".into(),
        );
        return (check, None);
    }
    let config = match Config::load(explicit) {
        Ok(config) => config,
        Err(err) => {
            let check = Check::fail(
                SUBJECT,
                format!("does not parse: {err:#}"),
                "fix the file; `herder daemon` refuses to start on it too".into(),
            );
            return (check, None);
        }
    };
    let path = config.path.display().to_string();
    let check = if config.path.is_file() {
        let accounts = config.accounts.len();
        let plural = if accounts == 1 { "" } else { "s" };
        Check::pass(SUBJECT, format!("{path}, {accounts} account{plural}"))
    } else {
        Check::fail(
            SUBJECT,
            format!("{path} not found"),
            "pair a herder client (run: herder pair) and add an account from it, which writes \
             the file"
                .into(),
        )
    };
    (check, Some(config))
}

/// For each provider with accounts, its CLI runs; then each of its accounts is logged in. The
/// accounts of a provider whose CLI does not run are not checked: fix the CLI first.
fn provider_checks(config: &Config) -> Vec<Check> {
    let providers: BTreeMap<&str, &Provider> = config
        .accounts
        .values()
        .map(|account| (account.provider.as_str(), &account.provider))
        .collect();
    let logins = login::programs(&config.binaries);
    let mut checks = Vec::new();
    for (name, provider) in providers {
        let cli = cli_check(config, provider);
        let runs = cli.status == Status::Pass;
        checks.push(cli);
        if !runs {
            continue;
        }
        let accounts = config
            .accounts
            .iter()
            .filter(|(_, a)| a.provider == *provider);
        for (id, account) in accounts {
            checks.push(account_check(id, account, logins.get(provider), name));
        }
    }
    checks
}

/// `provider`'s CLI is found and `<binary> --version` runs.
fn cli_check(config: &Config, provider: &Provider) -> Check {
    let name = provider.as_str();
    let subject = format!("{name} CLI");
    let Some(program) = herder_daemon::accounts::program(provider, &config.binaries) else {
        return Check::fail(
            subject,
            format!("herder cannot run {name} sessions"),
            format!("remove the {name} accounts from {}", config.path.display()),
        );
    };
    let shown = program.display();
    match output(&program, &["--version"]) {
        Ok(out) if out.status.success() => {
            Check::pass(subject, format!("{shown} {}", first_line(&out)))
        }
        Ok(out) => Check::fail(
            subject,
            format!(
                "`{shown} --version` failed ({}): {}",
                out.status,
                first_line(&out)
            ),
            format!("reinstall {name}'s CLI"),
        ),
        Err(err) if err.kind() == ErrorKind::NotFound => {
            let configured = config.binaries.contains_key(provider);
            let detail = if configured {
                format!("{shown} not found")
            } else {
                format!("{shown} not found on PATH")
            };
            let hint = if configured {
                format!(
                    "install it there, or fix [providers.{name}] binary in {}",
                    config.path.display()
                )
            } else {
                format!(
                    "install {name}'s CLI, or set [providers.{name}] binary in {}",
                    config.path.display()
                )
            };
            Check::fail(subject, detail, hint)
        }
        Err(err) => Check::fail(
            subject,
            format!("cannot run `{shown} --version`: {err}"),
            format!("reinstall {name}'s CLI"),
        ),
    }
}

/// The account is logged in, as its provider's own status check reports.
fn account_check(
    id: &AccountId,
    account: &AccountConfig,
    login: Option<&LoginProgram>,
    provider: &str,
) -> Check {
    let subject = format!("account {id} ({provider})");
    let dir = account.config_dir.as_deref();
    let location = dir.map_or_else(
        || "the CLI's default config dir".to_owned(),
        |dir| dir.display().to_string(),
    );
    let Some(login) = login else {
        return Check::warn(
            subject,
            format!("herder cannot check {provider} logins"),
            format!("make sure {provider} is logged in in {location}"),
        );
    };
    let relogin = || {
        format!(
            "log in again from a herder client (log in again on the account), or run: {}",
            login_command(login, dir)
        )
    };
    if let Some(dir) = dir
        && !dir.is_dir()
    {
        return Check::fail(
            subject,
            format!("config dir {} does not exist", dir.display()),
            format!(
                "create it and log in, run: mkdir -p {} && {}",
                quote(&dir.display().to_string()),
                login_command(login, Some(dir))
            ),
        );
    }
    match login::logged_in(login, dir) {
        Ok(true) => Check::pass(subject, format!("logged in, {location}")),
        Ok(false) => Check::fail(subject, format!("not logged in, {location}"), relogin()),
        Err(err) => Check::fail(subject, format!("cannot tell: {err}"), relogin()),
    }
}

/// The shell command that runs `login` in `dir`: the provider's own login, which herder runs
/// when it adds an account.
fn login_command(login: &LoginProgram, dir: Option<&Path>) -> String {
    let mut words = Vec::new();
    if let Some(dir) = dir {
        let dir = quote(&dir.display().to_string());
        words.extend(login.config_env.iter().map(|var| format!("{var}={dir}")));
    }
    words.push(quote(&login.program.display().to_string()));
    words.extend(login.args.iter().map(|arg| quote(arg)));
    words.join(" ")
}

/// `word` quoted for a POSIX shell when it needs it.
fn quote(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-~+=:,@%".contains(c));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// git is on `PATH`; `gh`, which herder tracks pull requests with, is too and logged in. A
/// missing or logged-out `gh` only warns, as herder runs sessions without it.
fn tool_checks(tools: &Tools) -> Vec<Check> {
    let git = match output(&tools.git, &["--version"]) {
        Ok(out) if out.status.success() => Check::pass("git", first_line(&out)),
        Ok(out) => Check::fail(
            "git",
            format!("`git --version` failed ({})", out.status),
            "reinstall git".into(),
        ),
        Err(err) if err.kind() == ErrorKind::NotFound => {
            Check::fail("git", "not found on PATH", "install git".into())
        }
        Err(err) => Check::fail(
            "git",
            format!("cannot run it: {err}"),
            "reinstall git".into(),
        ),
    };
    let gh = match output(&tools.gh, &["--version"]) {
        Ok(out) if out.status.success() => {
            let version = first_line(&out);
            match output(&tools.gh, &["auth", "status"]) {
                Ok(auth) if auth.status.success() => {
                    Check::pass("gh", format!("{version}, logged in"))
                }
                _ => Check::warn(
                    "gh",
                    format!("{version}, not logged in; herder cannot track pull requests"),
                    "run: gh auth login".into(),
                ),
            }
        }
        Ok(_) | Err(_) => Check::warn(
            "gh",
            "not found on PATH; herder cannot track pull requests",
            "install the GitHub CLI, then run: gh auth login".into(),
        ),
    };
    vec![git, gh]
}

/// The systemd user service is installed, enabled and running. Without systemd it only warns:
/// the daemon then runs under whatever this machine starts it with.
fn service_check(state: Option<service::State>) -> Check {
    const SUBJECT: &str = "service";
    let Some(state) = state else {
        return Check::warn(
            SUBJECT,
            "no systemd on this machine",
            "start the daemon some other way, such as: herder daemon".into(),
        );
    };
    if !state.installed {
        Check::fail(
            SUBJECT,
            "not installed",
            "run: herder service install".into(),
        )
    } else if !state.enabled {
        Check::fail(
            SUBJECT,
            "installed but not enabled, so it does not start at boot",
            "run: herder service install".into(),
        )
    } else if !state.active {
        Check::fail(
            SUBJECT,
            "installed and enabled but not running",
            "run: herder service restart; see why it stopped with: journalctl --user -u herder.service"
                .into(),
        )
    } else {
        Check::pass(SUBJECT, "installed, enabled and running")
    }
}

/// The daemon accepts TCP connections on `address`, or on loopback when it listens on every
/// interface.
fn listen_check(address: SocketAddr) -> Check {
    let subject = format!("daemon on {address}");
    let mut target = address;
    if target.ip().is_unspecified() {
        target.set_ip(match address {
            SocketAddr::V4(_) => std::net::Ipv4Addr::LOCALHOST.into(),
            SocketAddr::V6(_) => std::net::Ipv6Addr::LOCALHOST.into(),
        });
    }
    match TcpStream::connect_timeout(&target, CONNECT_TIMEOUT) {
        Ok(_) => Check::pass(subject, "answers"),
        Err(err) => Check::fail(
            subject,
            format!("does not answer: {err}"),
            "start the daemon, run: herder service install (or herder daemon)".into(),
        ),
    }
}

/// The vault this host replicates to accepts TCP connections.
fn vault_check(vault: &VaultConfig) -> Check {
    let subject = format!("vault {}", vault.address);
    let hint = || {
        "check the vault's daemon runs and that this machine can reach its address (network, \
         firewall)"
            .to_owned()
    };
    let addresses = match vault.address.to_socket_addrs() {
        Ok(addresses) => addresses,
        Err(err) => return Check::fail(subject, format!("cannot resolve it: {err}"), hint()),
    };
    let mut last = None;
    for address in addresses {
        match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
            Ok(_) => return Check::pass(subject, "reachable"),
            Err(err) => last = Some(err),
        }
    }
    let detail = last.map_or_else(
        || "resolves to no address".to_owned(),
        |err| format!("unreachable: {err}"),
    );
    Check::fail(subject, detail, hint())
}

/// Runs `program` with `args` to completion, or kills it after [`COMMAND_TIMEOUT`].
fn output(program: &Path, args: &[&str]) -> io::Result<Output> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                ErrorKind::TimedOut,
                format!("no answer within {} s", COMMAND_TIMEOUT.as_secs()),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The first non-empty line `out` printed, on stdout, else on stderr.
fn first_line(out: &Output) -> String {
    [&out.stdout, &out.stderr]
        .into_iter()
        .flat_map(|bytes| {
            String::from_utf8_lossy(bytes)
                .lines()
                .map(str::trim)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .find(|line| !line.is_empty())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// Installs `script` as the executable `to` through `cp`, so no file this process holds
    /// open for writing is run (see `tests/release.rs`: that fails with "Text file busy").
    fn install(dir: &Path, name: &str, script: &str) -> PathBuf {
        let source = dir.join(format!("{name}.source"));
        std::fs::write(&source, script).unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();
        let to = dir.join(name);
        let status = Command::new("cp").arg(&source).arg(&to).status().unwrap();
        assert!(status.success());
        to
    }

    /// A claude that answers `--version`, and `auth status --json` from whether its config
    /// dir holds `logged-in`.
    const FAKE_CLAUDE: &str = r#"#!/bin/sh
case "$1" in
  --version) echo "2.1.0 (Claude Code)" ;;
  auth) if [ -e "$CLAUDE_CONFIG_DIR/logged-in" ]; then v=true; else v=false; fi
        echo "{\"loggedIn\": $v}" ;;
esac
"#;

    fn check(status: Status, subject: &str) -> Check {
        Check::new(status, subject, "detail", None)
    }

    fn statuses(checks: &[Check]) -> Vec<(Status, &str)> {
        checks
            .iter()
            .map(|c| (c.status, c.subject.as_str()))
            .collect()
    }

    #[test]
    fn exits_non_zero_only_when_a_check_failed() {
        let pass = check(Status::Pass, "a");
        let warn = check(Status::Warn, "b");
        let fail = check(Status::Fail, "c");
        assert_eq!(exit_code(&[pass.clone(), warn.clone()]), ExitCode::SUCCESS);
        assert_eq!(exit_code(&[pass, warn, fail]), ExitCode::FAILURE);
    }

    #[test]
    fn renders_a_line_per_check_with_hints_and_a_summary() {
        let checks = [
            Check::pass("git", "git version 2.47.0"),
            Check::warn("gh", "not found on PATH", "install it".into()),
            Check::fail(
                "service",
                "not installed",
                "run: herder service install".into(),
            ),
        ];
        assert_eq!(
            render(&checks),
            "✓ git: git version 2.47.0\n\
             ! gh: not found on PATH — install it\n\
             ✗ service: not installed — run: herder service install\n\
             1 failed, 1 warning\n"
        );
        assert_eq!(
            render(&[Check::pass("git", "ok")]),
            "✓ git: ok\nall 1 checks passed\n"
        );
    }

    #[test]
    fn a_missing_config_fails_and_skips_the_checks_that_need_it() {
        let dir = tempfile::tempdir().unwrap();
        let tools = Tools {
            git: install(dir.path(), "git", "#!/bin/sh\necho 'git version 2.47.0'\n"),
            gh: dir.path().join("no-gh"),
        };
        let missing = dir.path().join("daemon.toml");
        let checks = checks(Some(&missing), &tools, None);
        assert_eq!(
            statuses(&checks),
            [
                (Status::Fail, "config"),
                (Status::Pass, "git"),
                (Status::Warn, "gh"),
                (Status::Warn, "service"),
            ]
        );
        assert!(checks[0].detail.contains("not found"), "{:?}", checks[0]);
        assert_eq!(exit_code(&checks), ExitCode::FAILURE);
    }

    #[test]
    fn a_config_that_does_not_parse_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.toml");
        std::fs::write(&path, "listen = 7\n").unwrap();
        let (check, config) = config_check(Some(&path));
        assert_eq!(check.status, Status::Fail);
        assert!(check.detail.starts_with("does not parse"), "{check:?}");
        assert!(config.is_none());
    }

    /// A config with two claude accounts, one logged in, and a codex account whose CLI is
    /// missing.
    fn accounts_config(dir: &Path) -> Config {
        let claude = install(dir, "claude", FAKE_CLAUDE);
        let (logged_in, logged_out) = (dir.join("in"), dir.join("out"));
        std::fs::create_dir_all(&logged_in).unwrap();
        std::fs::create_dir_all(&logged_out).unwrap();
        std::fs::write(logged_in.join("logged-in"), "").unwrap();
        let path = dir.join("daemon.toml");
        std::fs::write(
            &path,
            format!(
                r#"
data_dir = "{dir}/data"

[[accounts]]
id = "claude-in"
provider = "claude"
config_dir = "{dir}/in"

[[accounts]]
id = "claude-out"
provider = "claude"
config_dir = "{dir}/out"

[[accounts]]
id = "claude-gone"
provider = "claude"
config_dir = "{dir}/gone"

[[accounts]]
id = "codex-main"
provider = "codex"

[providers.claude]
binary = "{claude}"

[providers.codex]
binary = "{dir}/no-codex"
"#,
                dir = dir.display(),
                claude = claude.display(),
            ),
        )
        .unwrap();
        let (check, config) = config_check(Some(&path));
        assert_eq!(check.status, Status::Pass, "{check:?}");
        assert!(check.detail.ends_with("4 accounts"), "{check:?}");
        config.unwrap()
    }

    #[test]
    fn checks_each_providers_cli_and_each_accounts_login() {
        let dir = tempfile::tempdir().unwrap();
        let config = accounts_config(dir.path());
        let checks = provider_checks(&config);
        assert_eq!(
            statuses(&checks),
            [
                (Status::Pass, "claude CLI"),
                (Status::Fail, "account claude-gone (claude)"),
                (Status::Pass, "account claude-in (claude)"),
                (Status::Fail, "account claude-out (claude)"),
                (Status::Fail, "codex CLI"),
            ]
        );
        assert!(
            checks[0].detail.ends_with("2.1.0 (Claude Code)"),
            "{:?}",
            checks[0]
        );
        let claude = config.binaries[&Provider::Claude].display().to_string();
        let out = dir.path().join("out");
        assert_eq!(
            checks[3].hint.as_deref(),
            Some(
                format!(
                    "log in again from a herder client (log in again on the account), or run: \
                     CLAUDE_CONFIG_DIR={} {claude}",
                    out.display()
                )
                .as_str()
            )
        );
        assert!(
            checks[1].detail.contains("does not exist"),
            "{:?}",
            checks[1]
        );
        assert!(
            checks[4].detail.ends_with("no-codex not found"),
            "{:?}",
            checks[4]
        );
        assert!(
            checks[4]
                .hint
                .as_deref()
                .is_some_and(|h| h.contains("[providers.codex] binary")),
            "{:?}",
            checks[4]
        );
    }

    #[test]
    fn login_commands_quote_what_the_shell_would_split() {
        let login = &login::programs(&Default::default())[&Provider::Codex];
        assert_eq!(
            login_command(login, Some(Path::new("/home/ann/my codex"))),
            "CODEX_HOME='/home/ann/my codex' codex login --device-auth"
        );
        assert_eq!(login_command(login, None), "codex login --device-auth");
        assert_eq!(quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn gh_missing_or_logged_out_only_warns() {
        let dir = tempfile::tempdir().unwrap();
        let git = install(dir.path(), "git", "#!/bin/sh\necho 'git version 2.47.0'\n");
        let logged_out = install(
            dir.path(),
            "gh",
            "#!/bin/sh\n[ \"$1\" = --version ] && { echo 'gh version 2.60.0'; exit 0; }\nexit 1\n",
        );
        let checks = tool_checks(&Tools {
            git: git.clone(),
            gh: logged_out,
        });
        assert_eq!(
            statuses(&checks),
            [(Status::Pass, "git"), (Status::Warn, "gh")]
        );
        assert_eq!(checks[1].hint.as_deref(), Some("run: gh auth login"));
        let logged_in = install(dir.path(), "gh-ok", "#!/bin/sh\necho 'gh version 2.60.0'\n");
        let checks = tool_checks(&Tools { git, gh: logged_in });
        assert_eq!(checks[1].status, Status::Pass);
        assert_eq!(checks[1].detail, "gh version 2.60.0, logged in");
        let checks = tool_checks(&Tools {
            git: dir.path().join("no-git"),
            gh: dir.path().join("no-gh"),
        });
        assert_eq!(
            statuses(&checks),
            [(Status::Fail, "git"), (Status::Warn, "gh")]
        );
    }

    #[test]
    fn service_fails_until_installed_enabled_and_running_and_warns_without_systemd() {
        let state = |installed, enabled, active| {
            service_check(Some(service::State {
                installed,
                enabled,
                active,
            }))
        };
        assert_eq!(state(true, true, true).status, Status::Pass);
        for (check, hint) in [
            (state(false, false, false), "run: herder service install"),
            (state(true, false, true), "run: herder service install"),
        ] {
            assert_eq!(check.status, Status::Fail);
            assert_eq!(check.hint.as_deref(), Some(hint));
        }
        let stopped = state(true, true, false);
        assert_eq!(stopped.status, Status::Fail);
        assert!(
            stopped
                .hint
                .unwrap()
                .starts_with("run: herder service restart")
        );
        assert_eq!(service_check(None).status, Status::Warn);
    }

    #[test]
    fn listen_and_vault_checks_connect() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let open = listener.local_addr().unwrap();
        let closed = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        assert_eq!(listen_check(open).status, Status::Pass);
        assert_eq!(listen_check(closed).status, Status::Fail);
        let every = SocketAddr::from(([0, 0, 0, 0], open.port()));
        assert_eq!(listen_check(every).status, Status::Pass);
        let vault = |address: SocketAddr| {
            vault_check(&VaultConfig::new(address.to_string(), "ab".into(), None))
        };
        assert_eq!(vault(open).status, Status::Pass);
        assert_eq!(vault(closed).status, Status::Fail);
    }
}
