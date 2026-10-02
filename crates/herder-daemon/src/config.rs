//! Daemon configuration, loaded from a TOML file.
//!
//! # Accounts
//!
//! Each `[[accounts]]` entry is one provider login on this machine:
//!
//! ```toml
//! [[accounts]]
//! id = "claude-main"            # unique on this daemon; clients and sessions name it
//! provider = "claude"           # claude, codex, cursor, grok or opencode
//! label = "Main"                # shown to clients; the id when absent
//! config_dir = "~/.claude-main" # the CLI's own default location when absent
//! failover = false              # whether sessions may fail over to it on a limit; opt-in
//!
//! [providers.claude]
//! binary = "/opt/claude/bin/claude" # the CLI to run; looked up on `PATH` by default
//! ```
//!
//! # Failover
//!
//! A session whose turn hits its account's usage limit moves to another account that opted in
//! with `failover = true` and retries the turn there: first an account of its own provider,
//! then one of each provider the `[failover]` table lists, in order:
//!
//! ```toml
//! [failover]
//! providers = ["codex", "cursor"] # after the session's own provider; none by default
//! pin = false                     # true keeps every session on its account
//! ```
//!
//! # Resources
//!
//! The `[resources]` table sets the limits every session's CLI runs under and the budget turns
//! are admitted within; see [`ResourcesConfig`] for its keys and defaults.
//!
//! Accounts added from a client ([`crate::login`]) are appended to this file as new
//! `[[accounts]]` entries, which creates it when it does not exist yet; the rest of the file is
//! kept as written.
//!
//! `config_dir` is handed to the CLI as its config dir variable (`CLAUDE_CONFIG_DIR`,
//! `CODEX_HOME`, ...); without one the variable is not set and the CLI uses its default. It may
//! start with `~/`, must otherwise be absolute, and need not exist yet: logging in creates it.
//! herder never looks inside. Two accounts of one provider cannot share a config dir, as they
//! would be one login, and a Claude account cannot name `~/.claude`: `CLAUDE_CONFIG_DIR`
//! pointed there is not Claude's default login, so omit `config_dir` for that.
//!
//! # Tasks
//!
//! The `[tasks]` table limits every task a session runs through the task tools:
//!
//! ```toml
//! [tasks]
//! max_children = 5 # live (not archived) children a primary may have at once
//! ```
//!
//! # Projects
//!
//! The daemon finds repositories in its sessions, under the `[projects]` roots and at the
//! paths `[[project]]` entries declare; a project is identified by its `origin` remote. Each
//! `[[project]]` entry overrides one project; see [`crate::projects`]:
//!
//! ```toml
//! [projects]
//! roots = ["~/Projects"]   # scanned 3 levels deep; none by default
//! setup_timeout_secs = 600 # how long a setup command may run; 10 minutes by default
//!
//! [[project]]
//! name = "herder"                  # the last segment of the id when absent
//! remotes = [                      # merged into one project; the first gives its id
//!   "git@github.com:herder-sh/herder.git",
//!   "https://gitlab.com/mirror/herder",
//! ]
//! paths = ["~/src/herder-old"]     # clones of it whatever their remote; without `remotes`,
//!                                  # this declares the project by path
//! default_account = "claude-main"  # an `[[accounts]]` id
//! setup_command = "make bootstrap" # run in each new worktree
//! ```
//!
//! An entry needs `remotes` or `paths`. No remote or path may appear in two entries.
//!
//! `setup_command` runs with `sh -c` once in every new session worktree of the project, in the
//! session's resource scope, before the agent's first turn; see [`crate::session`].
//!
//! # Vault
//!
//! `mode = "vault"` (or `herder daemon --vault`) runs the daemon as a vault: it runs no
//! sessions and keeps the journals hosts replicate to it; see [`crate::vault`]. The default,
//! `mode = "host"`, runs sessions. A host replicates every session to the vault its `[vault]`
//! table names, paired the way a client pairs with a daemon:
//!
//! ```toml
//! [vault]
//! address = "vault.example.com:7447"
//! fingerprint = "3f9a..."    # the vault's certificate SHA-256, as `herder pair` prints it
//! pairing_code = "ABCDE-FGHJK" # from `herder pair` on the vault; only read until paired
//! ```

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use herder_protocol::{AccountId, ProjectId, Provider};
use serde::Deserialize;

use crate::accounts;
use crate::projects::{ProjectEntry, ProjectsConfig};
use crate::resources::ResourcesConfig;
use crate::session::{AccountConfig, Accounts, FailoverConfig, TaskLimits};

/// Port the daemon listens on unless configured otherwise.
pub const DEFAULT_PORT: u16 = 7447;

/// Resolved daemon configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The config file: the one read, or where one is created when an account is added.
    pub path: PathBuf,
    /// Address the daemon listens on.
    pub listen: SocketAddr,
    /// Directory holding everything the daemon persists.
    pub data_dir: PathBuf,
    /// Logging settings.
    pub log: LogConfig,
    /// The provider accounts sessions may run on.
    pub accounts: Accounts,
    /// The CLI to run per provider, where it is not the provider's own name on `PATH`.
    pub binaries: HashMap<Provider, PathBuf>,
    /// Limits on every task.
    pub tasks: TaskLimits,
    /// How sessions fail over when their account hits a limit.
    pub failover: FailoverConfig,
    /// Limits for the systemd scopes sessions run in.
    pub resources: ResourcesConfig,
    /// Where projects are discovered, and their overrides.
    pub projects: ProjectsConfig,
    /// Whether this daemon runs sessions or is the vault.
    pub mode: Mode,
    /// The vault a host replicates its sessions to, if any.
    pub vault: Option<VaultConfig>,
}

/// What a daemon runs as.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Runs sessions for clients.
    #[default]
    Host,
    /// Keeps the journals hosts replicate to it; see [`crate::vault`].
    Vault,
}

/// The `[vault]` table: where a host replicates to.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultConfig {
    /// The vault's address, as `host:port`.
    pub address: String,
    /// SHA-256 of the vault's TLS certificate, lowercase hex.
    pub fingerprint: String,
    /// One-time code from `herder pair` on the vault, for the first connection.
    #[serde(default)]
    pub pairing_code: Option<String>,
}

/// Logging settings: the `[log]` table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogConfig {
    /// A `tracing` env-filter directive, e.g. `info` or `herder_daemon=debug,info`.
    pub level: String,
    /// Output format.
    pub format: LogFormat,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: "info".to_owned(),
            format: LogFormat::Pretty,
        }
    }
}

/// Log output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Human-readable lines, coloured when stderr is a terminal.
    Pretty,
    /// One JSON object per line.
    Json,
}

/// The file as written; `data_dir` stays optional because its default depends on the environment.
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ConfigFile {
    listen: SocketAddr,
    data_dir: Option<PathBuf>,
    log: LogConfig,
    accounts: Vec<AccountFile>,
    providers: BTreeMap<String, ProviderFile>,
    tasks: TaskLimits,
    failover: FailoverConfig,
    resources: ResourcesConfig,
    projects: ProjectsFile,
    project: Vec<ProjectFile>,
    mode: Mode,
    vault: Option<VaultConfig>,
}

impl Default for ConfigFile {
    fn default() -> Self {
        Self {
            listen: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, DEFAULT_PORT)),
            data_dir: None,
            log: LogConfig::default(),
            accounts: Vec::new(),
            providers: BTreeMap::new(),
            tasks: TaskLimits::default(),
            failover: FailoverConfig::default(),
            resources: ResourcesConfig::default(),
            projects: ProjectsFile::default(),
            project: Vec::new(),
            mode: Mode::default(),
            vault: None,
        }
    }
}

/// The `[projects]` table as written.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ProjectsFile {
    roots: Vec<PathBuf>,
    setup_timeout_secs: Option<u64>,
}

/// One `[[project]]` entry as written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectFile {
    name: Option<String>,
    #[serde(default)]
    remotes: Vec<String>,
    #[serde(default)]
    paths: Vec<PathBuf>,
    default_account: Option<String>,
    setup_command: Option<String>,
}

/// One `[[accounts]]` entry as written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountFile {
    id: String,
    provider: String,
    label: Option<String>,
    config_dir: Option<PathBuf>,
    #[serde(default)]
    failover: bool,
}

/// One `[providers.<name>]` table as written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderFile {
    binary: PathBuf,
}

impl Config {
    /// Loads the configuration from the process environment.
    ///
    /// `explicit` is the path given by `--config` or `HERDER_CONFIG`; it must exist. Without it,
    /// `$XDG_CONFIG_HOME/herder/daemon.toml` is read, and a missing file means defaults.
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        Self::load_with_env(explicit, |key| std::env::var_os(key))
    }

    fn load_with_env(
        explicit: Option<&Path>,
        env: impl Fn(&str) -> Option<OsString>,
    ) -> Result<Self> {
        let (path, file) = match explicit {
            Some(path) => {
                let file = read(path)?
                    .with_context(|| format!("config file {} not found", path.display()))?;
                (path.to_owned(), file)
            }
            None => {
                let path = xdg_dir(&env, "XDG_CONFIG_HOME", ".config")?.join("herder/daemon.toml");
                let file = read(&path)?.unwrap_or_default();
                (path, file)
            }
        };
        let data_dir = match file.data_dir {
            Some(dir) => dir,
            None => xdg_dir(&env, "XDG_DATA_HOME", ".local/share")?.join("herder"),
        };
        file.resources.validate()?;
        for provider in &file.failover.providers {
            supported(provider.as_str().to_owned()).context("failover.providers")?;
        }
        let accounts = resolve_accounts(file.accounts, &env)?;
        let projects = resolve_projects(file.projects, file.project, &accounts, &env)?;
        if let Some(vault) = &file.vault {
            ensure!(
                file.mode == Mode::Host,
                "a vault does not replicate to another vault; remove the [vault] table"
            );
            ensure!(
                vault.fingerprint.len() == 64
                    && vault.fingerprint.chars().all(|c| c.is_ascii_hexdigit()),
                "vault.fingerprint must be the 64 hex digits `herder pair` prints"
            );
        }
        Ok(Self {
            path,
            listen: file.listen,
            data_dir,
            log: file.log,
            accounts,
            binaries: resolve_binaries(file.providers, &env)?,
            tasks: file.tasks,
            failover: file.failover,
            resources: file.resources,
            projects,
            mode: file.mode,
            vault: file.vault.map(|vault| VaultConfig {
                fingerprint: vault.fingerprint.to_ascii_lowercase(),
                ..vault
            }),
        })
    }
}

/// Validates the `[projects]` table and the `[[project]]` entries.
fn resolve_projects(
    table: ProjectsFile,
    entries: Vec<ProjectFile>,
    accounts: &Accounts,
    env: &impl Fn(&str) -> Option<OsString>,
) -> Result<ProjectsConfig> {
    let roots = table
        .roots
        .iter()
        .map(|root| resolve_path(root, env).context("projects.roots"))
        .collect::<Result<_>>()?;
    let mut remotes_seen = HashSet::new();
    let mut paths_seen = HashSet::new();
    let entries = entries
        .into_iter()
        .enumerate()
        .map(|(index, entry)| {
            let which = match &entry.name {
                Some(name) => format!("project {name:?}"),
                None => format!("project entry {}", index + 1),
            };
            ensure!(
                !entry.remotes.is_empty() || !entry.paths.is_empty(),
                "{which}: needs remotes or paths"
            );
            let remotes = entry
                .remotes
                .iter()
                .map(|url| {
                    let id = ProjectId::from_remote(url)
                        .with_context(|| format!("{which}: {url:?} is not a remote URL"))?;
                    ensure!(
                        remotes_seen.insert(id.clone()),
                        "{which}: remote {id} is in another entry too"
                    );
                    Ok(id)
                })
                .collect::<Result<_>>()?;
            let paths = entry
                .paths
                .iter()
                .map(|path| {
                    let path =
                        resolve_path(path, env).with_context(|| format!("{which}: paths"))?;
                    // Drops a trailing slash, so the path compares and prints as discovered.
                    let path: PathBuf = path.components().collect();
                    ensure!(
                        paths_seen.insert(path.clone()),
                        "{which}: path {} is in another entry too",
                        path.display()
                    );
                    Ok(path)
                })
                .collect::<Result<_>>()?;
            let default_account = entry.default_account.map(AccountId::new);
            if let Some(account) = &default_account {
                ensure!(
                    accounts.contains_key(account),
                    "{which}: default_account {account} is not an account"
                );
            }
            Ok(ProjectEntry {
                name: entry.name,
                remotes,
                paths,
                default_account,
                setup_command: entry.setup_command,
            })
        })
        .collect::<Result<_>>()?;
    let setup_timeout = match table.setup_timeout_secs {
        Some(0) => bail!("projects.setup_timeout_secs must be at least 1"),
        Some(secs) => std::time::Duration::from_secs(secs),
        None => crate::projects::SETUP_TIMEOUT,
    };
    Ok(ProjectsConfig {
        roots,
        setup_timeout,
        entries,
    })
}

/// Appends an `[[accounts]]` entry for `account` to the config file at `path`, creating the
/// file if it does not exist. The file is replaced atomically, and only if it still loads with
/// the new entry as `account_id`: an id or config dir already in use fails, changing nothing.
pub fn append_account(path: &Path, account_id: &AccountId, account: &AccountConfig) -> Result<()> {
    append_account_with_env(path, account_id, account, |key| std::env::var_os(key))
}

fn append_account_with_env(
    path: &Path,
    account_id: &AccountId,
    account: &AccountConfig,
    env: impl Fn(&str) -> Option<OsString>,
) -> Result<()> {
    let quote = |text: &str| toml::Value::String(text.to_owned()).to_string();
    let mut entry = format!(
        "[[accounts]]\nid = {}\nprovider = {}\nlabel = {}\n",
        quote(account_id.as_str()),
        quote(account.provider.as_str()),
        quote(&account.label),
    );
    if let Some(dir) = &account.config_dir {
        let dir = dir
            .to_str()
            .with_context(|| format!("config dir {} is not UTF-8", dir.display()))?;
        entry.push_str(&format!("config_dir = {}\n", quote(dir)));
    }
    if account.failover {
        entry.push_str("failover = true\n");
    }
    let mut text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            return Err(err).with_context(|| format!("reading config file {}", path.display()));
        }
    };
    if !text.is_empty() {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push('\n');
    }
    text.push_str(&entry);
    let file: ConfigFile =
        toml::from_str(&text).with_context(|| format!("adding to {}", path.display()))?;
    let accounts = resolve_accounts(file.accounts, &env)?;
    ensure!(
        accounts.get(account_id) == Some(account),
        "{} does not end in a table the new account can follow",
        path.display()
    );
    write_atomically(path, text.as_bytes())
        .with_context(|| format!("writing config file {}", path.display()))
}

/// Replaces `path` with `data` through a temporary file in its directory, which is created if
/// needed; a new file is owner-only, an existing one keeps its permissions.
fn write_atomically(path: &Path, data: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mode = match std::fs::metadata(path) {
        Ok(meta) => meta.permissions().mode() & 0o7777,
        Err(err) if err.kind() == io::ErrorKind::NotFound => 0o600,
        Err(err) => return Err(err),
    };
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.herder-tmp"));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)?;
        // `mode` is masked by the umask on create; set it exactly.
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        file.write_all(data)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        std::fs::File::open(dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Validates the `[[accounts]]` entries and resolves their config dirs.
fn resolve_accounts(
    entries: Vec<AccountFile>,
    env: &impl Fn(&str) -> Option<OsString>,
) -> Result<Accounts> {
    let mut accounts = Accounts::new();
    let mut logins = HashSet::new();
    for entry in entries {
        let id = entry.id;
        ensure!(
            !accounts.contains_key(&AccountId::new(&id)),
            "account id {id} is used twice"
        );
        ensure!(valid_id(&id), "account id {id:?} {ID_RULE}");
        let provider = supported(entry.provider).with_context(|| format!("account {id}"))?;
        let config_dir = entry
            .config_dir
            .map(|dir| resolve_path(&dir, env))
            .transpose()
            .with_context(|| format!("account {id}: config_dir"))?;
        if let Some(dir) = &config_dir {
            ensure!(
                !dir.exists() || dir.is_dir(),
                "account {id}: config_dir {} is not a directory",
                dir.display()
            );
            if provider == Provider::Claude {
                let default = resolve_path(Path::new("~/.claude"), env)?;
                ensure!(
                    *dir != default,
                    "account {id}: config_dir {} is where claude keeps its default login, \
                     which CLAUDE_CONFIG_DIR does not reach; omit config_dir to use it",
                    dir.display()
                );
            }
        }
        ensure!(
            logins.insert((provider.clone(), config_dir.clone())),
            "account {id}: another {} account already uses {}",
            provider.as_str(),
            config_dir
                .as_ref()
                .map_or("the default location".to_owned(), |dir| dir
                    .display()
                    .to_string())
        );
        let account = AccountConfig {
            provider,
            label: entry.label.unwrap_or_else(|| id.clone()),
            config_dir,
            failover: entry.failover,
        };
        accounts.insert(AccountId::new(id), account);
    }
    Ok(accounts)
}

/// What an account id may contain.
pub(crate) const ID_RULE: &str = "must be letters, digits, '-', '_' or '.'";

/// Whether `id` may name an account: see [`ID_RULE`].
pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Validates the `[providers.<name>]` tables into the binary each provider runs.
fn resolve_binaries(
    tables: BTreeMap<String, ProviderFile>,
    env: &impl Fn(&str) -> Option<OsString>,
) -> Result<HashMap<Provider, PathBuf>> {
    tables
        .into_iter()
        .map(|(name, table)| {
            let provider = supported(name)?;
            let binary = if table.binary.components().count() == 1 {
                // A bare name, looked up on `PATH`.
                table.binary
            } else {
                resolve_path(&table.binary, env)
                    .with_context(|| format!("providers.{}.binary", provider.as_str()))?
            };
            Ok((provider, binary))
        })
        .collect()
}

/// The provider named `name`, if herder can run its sessions.
fn supported(name: String) -> Result<Provider> {
    let provider = Provider::from(name);
    ensure!(
        accounts::runs(&provider),
        "herder cannot run {} sessions; the providers are {}",
        provider.as_str(),
        accounts::PROVIDERS
            .map(|p| p.as_str().to_owned())
            .join(", ")
    );
    Ok(provider)
}

/// `path` with a leading `~/` replaced by `$HOME`; it must then be absolute.
pub(crate) fn resolve_path(
    path: &Path,
    env: &impl Fn(&str) -> Option<OsString>,
) -> Result<PathBuf> {
    let path = match path.strip_prefix("~") {
        Ok(rest) => match env("HOME").map(PathBuf::from) {
            Some(home) if home.is_absolute() => home.join(rest),
            _ => bail!("cannot expand ~ in {}: $HOME is not set", path.display()),
        },
        Err(_) => path.to_owned(),
    };
    ensure!(
        path.is_absolute(),
        "{} must be absolute or start with ~/",
        path.display()
    );
    Ok(path)
}

/// Reads and parses a config file; `None` when it does not exist.
fn read(path: &Path) -> Result<Option<ConfigFile>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("reading config file {}", path.display()));
        }
    };
    let file =
        toml::from_str(&text).with_context(|| format!("invalid config file {}", path.display()))?;
    Ok(Some(file))
}

/// An XDG base directory: `$var` when set to an absolute path, else `$HOME/<fallback>`.
fn xdg_dir(env: &impl Fn(&str) -> Option<OsString>, var: &str, fallback: &str) -> Result<PathBuf> {
    if let Some(dir) = env(var).map(PathBuf::from).filter(|dir| dir.is_absolute()) {
        return Ok(dir);
    }
    match env("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
    {
        Some(home) => Ok(home.join(fallback)),
        None => bail!("cannot locate ${var}: neither it nor $HOME is set to an absolute path"),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key| {
            vars.iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| OsString::from(v))
        }
    }

    fn write(dir: &Path, text: &str) -> PathBuf {
        let path = dir.join("daemon.toml");
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn missing_default_file_gives_defaults() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path().to_str().unwrap();
        let config = Config::load_with_env(None, env(&[("HOME", home)])).unwrap();
        assert_eq!(config.listen, "0.0.0.0:7447".parse().unwrap());
        assert_eq!(config.data_dir, Path::new(home).join(".local/share/herder"));
        assert_eq!(config.log, LogConfig::default());
        assert_eq!(config.log.format, LogFormat::Pretty);
        assert_eq!(config.tasks.max_children, 5);
    }

    #[test]
    fn xdg_dirs_are_honoured() {
        let tmp = tempfile::tempdir().unwrap();
        let config_home = tmp.path().join("cfg");
        std::fs::create_dir_all(config_home.join("herder")).unwrap();
        std::fs::write(
            config_home.join("herder/daemon.toml"),
            "listen = \"127.0.0.1:9000\"\n",
        )
        .unwrap();
        let config = Config::load_with_env(
            None,
            env(&[
                ("HOME", "/nonexistent"),
                ("XDG_CONFIG_HOME", config_home.to_str().unwrap()),
                ("XDG_DATA_HOME", "/srv/data"),
            ]),
        )
        .unwrap();
        assert_eq!(config.listen, "127.0.0.1:9000".parse().unwrap());
        assert_eq!(config.data_dir, Path::new("/srv/data/herder"));
    }

    #[test]
    fn relative_xdg_dirs_are_ignored() {
        let config =
            Config::load_with_env(None, env(&[("HOME", "/h"), ("XDG_DATA_HOME", "rel")])).unwrap();
        assert_eq!(config.data_dir, Path::new("/h/.local/share/herder"));
    }

    #[test]
    fn file_overrides_every_field() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write(
            tmp.path(),
            r#"
            listen = "[::1]:8000"
            data_dir = "/var/lib/herder"

            [log]
            level = "debug"
            format = "json"

            [tasks]
            max_children = 2
            [failover]
            providers = ["codex", "cursor"]
            pin = true
            [resources]
            memory_max_percent = 25
            memory_high_percent = 90
            cpu_weight = 200
            child_cpu_weight = 20
            nice = 5
            max_turns = 2
            min_memory_available_mib = 1024
            max_memory_pressure = 30
            max_load_percent = 200
            "#,
        );
        let config = Config::load_with_env(Some(&path), env(&[])).unwrap();
        assert_eq!(
            config,
            Config {
                path: path.clone(),
                listen: "[::1]:8000".parse().unwrap(),
                data_dir: PathBuf::from("/var/lib/herder"),
                log: LogConfig {
                    level: "debug".to_owned(),
                    format: LogFormat::Json,
                },
                accounts: Accounts::new(),
                binaries: HashMap::new(),
                tasks: TaskLimits { max_children: 2 },
                failover: FailoverConfig {
                    providers: vec![Provider::Codex, Provider::Cursor],
                    pin: true,
                },
                resources: ResourcesConfig {
                    memory_max_percent: 25,
                    memory_high_percent: 90,
                    cpu_weight: 200,
                    child_cpu_weight: 20,
                    nice: 5,
                    max_turns: Some(2),
                    min_memory_available_mib: 1024,
                    max_memory_pressure: 30,
                    max_load_percent: 200,
                },
                projects: ProjectsConfig::default(),
                mode: Mode::Host,
                vault: None,
            }
        );
    }

    #[test]
    fn vault_mode_and_a_hosts_vault_table() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("daemon.toml");
        std::fs::write(&path, "mode = \"vault\"\n").unwrap();
        let config = Config::load_with_env(Some(&path), env(&[("HOME", "/home/dev")])).unwrap();
        assert_eq!((config.mode, config.vault), (Mode::Vault, None));

        let fingerprint = "AB".repeat(32);
        std::fs::write(
            &path,
            format!(
                "[vault]\naddress = \"vault:7447\"\nfingerprint = \"{fingerprint}\"\n\
                 pairing_code = \"ABCDE-FGHJK\"\n"
            ),
        )
        .unwrap();
        let config = Config::load_with_env(Some(&path), env(&[("HOME", "/home/dev")])).unwrap();
        assert_eq!(config.mode, Mode::Host);
        assert_eq!(
            config.vault,
            Some(VaultConfig {
                address: "vault:7447".into(),
                fingerprint: "ab".repeat(32),
                pairing_code: Some("ABCDE-FGHJK".into()),
            })
        );

        std::fs::write(
            &path,
            "[vault]\naddress = \"vault:7447\"\nfingerprint = \"abc\"\n",
        )
        .unwrap();
        let err = Config::load_with_env(Some(&path), env(&[("HOME", "/home/dev")])).unwrap_err();
        assert!(format!("{err:#}").contains("vault.fingerprint"), "{err:#}");

        std::fs::write(
            &path,
            format!(
                "mode = \"vault\"\n[vault]\naddress = \"v:1\"\nfingerprint = \"{fingerprint}\"\n"
            ),
        )
        .unwrap();
        assert!(Config::load_with_env(Some(&path), env(&[("HOME", "/home/dev")])).is_err());
    }

    #[test]
    fn partial_log_table_keeps_other_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write(tmp.path(), "[log]\nformat = \"json\"\n");
        let config = Config::load_with_env(Some(&path), env(&[("HOME", "/h")])).unwrap();
        assert_eq!(config.log.level, "info");
        assert_eq!(config.log.format, LogFormat::Json);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        for text in [
            "port = 1\n",
            "[log]\ncolour = true\n",
            "[tls]\n",
            "[tasks]\nmax_depth = 2\n",
        ] {
            let path = write(tmp.path(), text);
            let err = Config::load_with_env(Some(&path), env(&[("HOME", "/h")])).unwrap_err();
            assert!(
                format!("{err:#}").contains("unknown field"),
                "{text}: {err:#}"
            );
        }
    }

    #[test]
    fn out_of_range_resources_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        for (text, key) in [
            ("memory_max_percent = 0", "memory_max_percent"),
            ("memory_high_percent = 101", "memory_high_percent"),
            ("child_cpu_weight = 0", "child_cpu_weight"),
            ("nice = 20", "nice"),
            ("max_turns = 0", "max_turns"),
            ("max_memory_pressure = 0", "max_memory_pressure"),
            ("max_load_percent = 0", "max_load_percent"),
        ] {
            let path = write(tmp.path(), &format!("[resources]\n{text}\n"));
            let err = Config::load_with_env(Some(&path), env(&[("HOME", "/h")])).unwrap_err();
            assert!(format!("{err:#}").contains(key), "{text}: {err:#}");
        }
    }

    #[test]
    fn unknown_log_format_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write(tmp.path(), "[log]\nformat = \"xml\"\n");
        assert!(Config::load_with_env(Some(&path), env(&[("HOME", "/h")])).is_err());
    }

    #[test]
    fn missing_explicit_file_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err =
            Config::load_with_env(Some(&tmp.path().join("nope.toml")), env(&[("HOME", "/h")]))
                .unwrap_err();
        assert!(err.to_string().contains("not found"), "{err:#}");
    }

    /// Loads `text` as the config file with `$HOME` at `home`; the error text on failure.
    fn load(home: &Path, text: &str) -> Result<Config, String> {
        let path = write(home, text);
        Config::load_with_env(Some(&path), env(&[("HOME", home.to_str().unwrap())]))
            .map_err(|err| format!("{err:#}"))
    }

    #[test]
    fn accounts_are_loaded_with_defaults_and_home_expanded() {
        let home = tempfile::tempdir().unwrap();
        let config = load(
            home.path(),
            r#"
            [[accounts]]
            id = "claude-main"
            provider = "claude"
            label = "Main"

            [[accounts]]
            id = "claude-work"
            provider = "claude"
            config_dir = "~/.claude-work"
            failover = true

            [[accounts]]
            id = "codex"
            provider = "codex"
            config_dir = "/srv/codex"
            "#,
        )
        .unwrap();
        assert_eq!(
            config.accounts,
            Accounts::from([
                (
                    AccountId::new("claude-main"),
                    AccountConfig {
                        provider: Provider::Claude,
                        label: "Main".into(),
                        config_dir: None,
                        failover: false,
                    }
                ),
                (
                    AccountId::new("claude-work"),
                    AccountConfig {
                        provider: Provider::Claude,
                        label: "claude-work".into(),
                        config_dir: Some(home.path().join(".claude-work")),
                        failover: true,
                    }
                ),
                (
                    AccountId::new("codex"),
                    AccountConfig {
                        provider: Provider::Codex,
                        label: "codex".into(),
                        config_dir: Some(PathBuf::from("/srv/codex")),
                        failover: false,
                    }
                ),
            ])
        );
        assert!(config.binaries.is_empty());
    }

    #[test]
    fn provider_binaries_are_bare_names_or_resolved_paths() {
        let home = tempfile::tempdir().unwrap();
        let config = load(
            home.path(),
            r#"
            [providers.claude]
            binary = "~/bin/claude"
            [providers.codex]
            binary = "codex-nightly"
            [providers.opencode]
            binary = "/opt/opencode"
            "#,
        )
        .unwrap();
        assert_eq!(
            config.binaries,
            HashMap::from([
                (Provider::Claude, home.path().join("bin/claude")),
                (Provider::Codex, PathBuf::from("codex-nightly")),
                (Provider::Opencode, PathBuf::from("/opt/opencode")),
            ])
        );
    }

    #[test]
    fn invalid_accounts_are_rejected() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("file"), "").unwrap();
        let account = |fields: &[&str]| format!("[[accounts]]\n{}\n", fields.join("\n"));
        let claude = |id: &str, extra: &str| {
            account(&[&format!("id = {id:?}"), "provider = \"claude\"", extra])
        };
        let cases = [
            (
                claude("a", "") + &account(&["id = \"a\"", "provider = \"codex\""]),
                "account id a is used twice",
            ),
            (
                account(&["id = \"a\"", "provider = \"gemini\""]),
                "herder cannot run gemini sessions",
            ),
            (
                account(&["id = \"a\"", "provider = \"nope\""]),
                "herder cannot run nope sessions",
            ),
            (claude("a b", ""), "must be letters"),
            (claude("", ""), "must be letters"),
            (
                claude("a", "config_dir = \"rel/dir\""),
                "must be absolute or start with ~/",
            ),
            (
                claude("a", "config_dir = \"~/.claude\""),
                "omit config_dir to use it",
            ),
            (claude("a", "config_dir = \"~/file\""), "is not a directory"),
            (
                claude("a", "") + &claude("b", ""),
                "another claude account already uses the default location",
            ),
            (
                claude("a", "config_dir = \"/x\"") + &claude("b", "config_dir = \"/x/\""),
                "another claude account already uses /x",
            ),
            (account(&["id = \"a\""]), "missing field `provider`"),
            (claude("a", "token = \"x\""), "unknown field `token`"),
            (
                "[providers.nope]\nbinary = \"x\"\n".to_owned(),
                "herder cannot run nope",
            ),
            ("[providers.claude]\n".to_owned(), "missing field `binary`"),
            (
                "[failover]\nproviders = [\"nope\"]\n".to_owned(),
                "herder cannot run nope",
            ),
        ];
        for (text, expected) in cases {
            let err = load(home.path(), &text).unwrap_err();
            assert!(err.contains(expected), "{text}: {err}");
        }
    }

    #[test]
    fn accounts_of_different_providers_may_both_use_the_default_location() {
        let home = tempfile::tempdir().unwrap();
        let config = load(
            home.path(),
            "[[accounts]]\nid = \"a\"\nprovider = \"claude\"\n\
             [[accounts]]\nid = \"b\"\nprovider = \"codex\"\n",
        )
        .unwrap();
        assert_eq!(config.accounts.len(), 2);
    }

    #[test]
    fn projects_are_loaded_with_remotes_normalised_and_home_expanded() {
        let home = tempfile::tempdir().unwrap();
        let config = load(
            home.path(),
            r#"
            [[accounts]]
            id = "claude-main"
            provider = "claude"

            [projects]
            roots = ["~/Projects", "/srv/src"]
            setup_timeout_secs = 90

            [[project]]
            name = "herder"
            remotes = ["git@github.com:herder-sh/herder.git", "https://gitlab.com/mirror/herder/"]
            paths = ["~/old/herder"]
            default_account = "claude-main"
            setup_command = "make bootstrap"

            [[project]]
            paths = ["/srv/scratch"]
            "#,
        )
        .unwrap();
        assert_eq!(
            config.projects,
            ProjectsConfig {
                roots: vec![home.path().join("Projects"), PathBuf::from("/srv/src")],
                setup_timeout: std::time::Duration::from_secs(90),
                entries: vec![
                    ProjectEntry {
                        name: Some("herder".to_owned()),
                        remotes: vec![
                            ProjectId::new("github.com/herder-sh/herder"),
                            ProjectId::new("gitlab.com/mirror/herder"),
                        ],
                        paths: vec![home.path().join("old/herder")],
                        default_account: Some(AccountId::new("claude-main")),
                        setup_command: Some("make bootstrap".to_owned()),
                    },
                    ProjectEntry {
                        paths: vec![PathBuf::from("/srv/scratch")],
                        ..ProjectEntry::default()
                    },
                ],
            }
        );
    }

    #[test]
    fn invalid_projects_are_rejected() {
        let home = tempfile::tempdir().unwrap();
        let cases = [
            (
                "[[project]]\nname = \"x\"\n",
                "project \"x\": needs remotes or paths",
            ),
            (
                "[[project]]\nremotes = [\"/local/path\"]\n",
                "is not a remote URL",
            ),
            (
                "[[project]]\nremotes = [\"git@github.com:o/r.git\"]\n\
                 [[project]]\nremotes = [\"https://github.com/o/r\"]\n",
                "project entry 2: remote github.com/o/r is in another entry too",
            ),
            (
                "[[project]]\npaths = [\"/a\"]\n[[project]]\npaths = [\"/a/\"]\n",
                "path /a is in another entry too",
            ),
            (
                "[[project]]\npaths = [\"rel\"]\n",
                "must be absolute or start with ~/",
            ),
            (
                "[[project]]\npaths = [\"/a\"]\ndefault_account = \"nope\"\n",
                "default_account nope is not an account",
            ),
            ("[projects]\nroots = [\"rel\"]\n", "projects.roots"),
            ("[projects]\ndepth = 2\n", "unknown field `depth`"),
            (
                "[projects]\nsetup_timeout_secs = 0\n",
                "projects.setup_timeout_secs must be at least 1",
            ),
            (
                "[[project]]\npaths = [\"/a\"]\nid = \"x\"\n",
                "unknown field `id`",
            ),
        ];
        for (text, expected) in cases {
            let err = load(home.path(), text).unwrap_err();
            assert!(err.contains(expected), "{text}: {err}");
        }
    }

    fn added(provider: Provider, dir: &Path) -> AccountConfig {
        AccountConfig {
            provider,
            label: "Work \"2\"".into(),
            config_dir: Some(dir.to_owned()),
            failover: false,
        }
    }

    #[test]
    fn an_added_account_is_appended_keeping_the_rest_of_the_file() {
        let home = tempfile::tempdir().unwrap();
        let vars = [("HOME", home.path().to_str().unwrap())];
        let env = || env(&vars);
        let original = "# my daemon\nlisten = \"127.0.0.1:9000\"\n\n[[accounts]]\nid = \"main\"\n\
                        provider = \"claude\" # the default login\n\n[providers.codex]\nbinary = \"codex\"";
        let path = write(home.path(), original);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let dir = home.path().join(".claude-work");
        let account = added(Provider::Claude, &dir);
        append_account_with_env(&path, &AccountId::new("work"), &account, env()).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(original), "{text}");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o640);
        let config = Config::load_with_env(Some(&path), env()).unwrap();
        assert_eq!(config.accounts[&AccountId::new("work")], account);
        assert_eq!(config.accounts.len(), 2);
        assert_eq!(config.listen, "127.0.0.1:9000".parse().unwrap());
        assert_eq!(config.binaries[&Provider::Codex], PathBuf::from("codex"));
    }

    #[test]
    fn adding_an_account_creates_a_missing_file() {
        let home = tempfile::tempdir().unwrap();
        let vars = [("HOME", home.path().to_str().unwrap())];
        let env = || env(&vars);
        let path = home.path().join("herder/daemon.toml");
        let account = added(Provider::Codex, &home.path().join(".codex-2"));
        append_account_with_env(&path, &AccountId::new("codex-2"), &account, env()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let config = Config::load_with_env(Some(&path), env()).unwrap();
        assert_eq!(config.accounts[&AccountId::new("codex-2")], account);
        assert_eq!(config.path, path);
    }

    #[test]
    fn an_account_that_does_not_fit_leaves_the_file_alone() {
        let home = tempfile::tempdir().unwrap();
        let vars = [("HOME", home.path().to_str().unwrap())];
        let env = || env(&vars);
        let original = "[[accounts]]\nid = \"a\"\nprovider = \"codex\"\nconfig_dir = \"/x\"\n";
        let path = write(home.path(), original);
        let cases = [
            ("a", added(Provider::Codex, Path::new("/y")), "used twice"),
            (
                "b",
                added(Provider::Codex, Path::new("/x")),
                "already uses /x",
            ),
        ];
        for (id, account, expected) in cases {
            let err =
                append_account_with_env(&path, &AccountId::new(id), &account, env()).unwrap_err();
            assert!(format!("{err:#}").contains(expected), "{id}: {err:#}");
        }
        // An inline accounts array cannot be followed by a table of it.
        let inline = write(home.path(), "accounts = []\n");
        let account = added(Provider::Codex, Path::new("/z"));
        assert!(append_account_with_env(&inline, &AccountId::new("c"), &account, env()).is_err());
        assert_eq!(std::fs::read_to_string(&inline).unwrap(), "accounts = []\n");
        assert_eq!(
            std::fs::read_dir(home.path()).unwrap().count(),
            1,
            "no temporary file is left behind"
        );
    }
}
