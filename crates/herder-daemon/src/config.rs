//! Daemon configuration, loaded from a TOML file.
//!
//! # Listening
//!
//! `listen` is the address the daemon accepts clients on, or a list of them; every address is
//! bound, and pairing links name each one other devices can reach:
//!
//! ```toml
//! listen = "0.0.0.0:7447"                               # every interface; the default
//! listen = ["127.0.0.1:7447", "100.124.135.114:7447"] # loopback and a Tailscale address only
//! ```
//!
//! A list may not be empty or name an address twice.
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
//!
//! [providers.claude]
//! binary = "/opt/claude/bin/claude" # the CLI to run; looked up on `PATH` by default
//! ```
//!
//! # Failover
//!
//! A session whose turn hits its account's usage limit rotates to the available account of its
//! own provider with the most room left, and retries the turn there on the same model. Every
//! account takes part; a failover never changes the provider or the model:
//!
//! ```toml
//! [failover]
//! pin = false # true keeps every session on its account
//! ```
//!
//! # Titles
//!
//! A small model titles each session from its conversation ([`crate::session::titles`]), by
//! running the provider's own CLI once on an account's config dir:
//!
//! ```toml
//! [titles]
//! enabled = true          # false gives sessions only the titles users type
//! provider = "claude"     # claude or codex; the titling account's when absent
//! model = "haiku"         # in that provider's naming; haiku for claude, gpt-6-luna for codex
//! account = "claude-main" # an `[[accounts]]` id; the session's own account when absent
//! ```
//!
//! Without `account`, a session is titled on its own account, or, when `provider` is not the
//! session's, on that provider's available account with the most room left. A session whose
//! provider cannot title gets no generated title.
//!
//! # Follow-ups
//!
//! The daemon can prompt an idle agent on its own when its pull request changes, or when it
//! stops with its work unfinished:
//!
//! ```toml
//! [follow_ups]
//! pr_events = true      # prompt on failed or passed checks, conflicts and requested changes
//! stall_after_secs = 0  # idle seconds before a stalled agent is prompted; 0 never prompts it
//! max_stall_nudges = 2  # most prompts a stalled agent gets before it is left to a user
//! ```
//!
//! # Resources
//!
//! The `[resources]` table sets the limits every session's CLI runs under and the budget turns
//! are admitted within; see [`ResourcesConfig`] for its keys and defaults.
//!
//! # Settings from a client
//!
//! An owner changes every daemon-wide key from a client ([`set_settings`]): only the keys whose
//! value changed are written, in place, keeping the rest of the file as written.
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
//! pointed there is not Claude's default login, so omit `config_dir` for that. An account added
//! from a client naming `~/.claude` is saved without one.
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
//! exclude = ["~/Projects/old"] # repositories left out wherever they are found
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
//! default_permission_mode = "ask"  # read_only, ask, auto_edit or full_access
//! setup_command = "make bootstrap" # run in each new worktree
//! icon = "design/mark.svg"         # the project's icon, relative to its clone; found in the
//!                                  # repository when absent or missing
//! icon_background = "#ffffff"      # drawn behind the icon, for one that needs it
//! ```
//!
//! Owners add `[[project]]` entries and change their `name`, `default_permission_mode`,
//! `default_account`, `setup_command` and `icon_background` from a client too ([`add_project`],
//! [`set_project_settings`]), and remove projects ([`remove_project`]): a removed project's
//! clones leave every entry's `paths` and go into `exclude`; the rest of the file is kept as
//! written.
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
//! attachments = true           # back up prompt images too; off by default
//! attachments_cap = 1073741824 # most bytes of images the vault keeps; 1 GiB by default
//! ```
//!
//! With `attachments`, the vault keeps this host's images up to `attachments_cap` bytes,
//! evicting the oldest first; without it the host sends none.
//!
//! A vault's own `[vault]` table sets how long it keeps archived sessions:
//!
//! ```toml
//! mode = "vault"
//!
//! [vault]
//! archive_retention_days = 90 # archived sessions of online hosts are dropped after this
//! ```

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use herder_protocol::{
    AccountId, BackupSettings, DaemonSettings, FollowUpSettings, LogSettings, PermissionMode,
    ProjectDiscovery, ProjectId, Provider, ProviderBinary, ResourceSettings, TaskSettings,
    TitleSettings,
};
use serde::Deserialize;

use crate::accounts;
use crate::projects::{ProjectEntry, ProjectsConfig};
use crate::resources::ResourcesConfig;
use crate::session::{AccountConfig, Accounts, FailoverConfig, TaskLimits, TitlesConfig};

/// Port the daemon listens on unless configured otherwise.
pub const DEFAULT_PORT: u16 = 7447;

/// Resolved daemon configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The config file: the one read, or where one is created when an account is added.
    pub path: PathBuf,
    /// Addresses the daemon listens on, as configured: at least one, none twice.
    pub listen: Vec<SocketAddr>,
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
    /// How sessions are titled.
    pub titles: TitlesConfig,
    /// When the daemon prompts an idle agent on its own.
    pub follow_ups: FollowUpsConfig,
    /// Limits for the systemd scopes sessions run in.
    pub resources: ResourcesConfig,
    /// Where projects are discovered, and their overrides.
    pub projects: ProjectsConfig,
    /// Whether this daemon runs sessions or is the vault.
    pub mode: Mode,
    /// The vault a host replicates its sessions to, if any.
    pub vault: Option<VaultConfig>,
    /// What a vault keeps, and for how long; read in vault mode only.
    pub retention: Retention,
}

/// Bytes of images a vault keeps per host unless the host's `[vault]` table says otherwise.
pub const DEFAULT_ATTACHMENTS_CAP: u64 = 1 << 30;

/// What a vault keeps: its `[vault]` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    /// Days an archived session of an online host stays on the vault after its latest event.
    pub archive_days: u32,
}

impl Default for Retention {
    fn default() -> Self {
        Self { archive_days: 90 }
    }
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

/// The `[vault]` table of a host: where it replicates to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultConfig {
    /// The vault's address, as `host:port`.
    pub address: String,
    /// SHA-256 of the vault's TLS certificate, lowercase hex.
    pub fingerprint: String,
    /// One-time code from `herder pair` on the vault, for the first connection.
    pub pairing_code: Option<String>,
    /// Whether prompt images are backed up too.
    pub attachments: bool,
    /// Most bytes of this host's images the vault keeps, when `attachments` is on.
    pub attachments_cap: u64,
}

impl VaultConfig {
    /// Backs up to the vault at `address`, pinned to `fingerprint`, images off.
    pub fn new(address: String, fingerprint: String, pairing_code: Option<String>) -> Self {
        Self {
            address,
            fingerprint,
            pairing_code,
            attachments: false,
            attachments_cap: DEFAULT_ATTACHMENTS_CAP,
        }
    }

    /// The cap the host's hello gives the vault: `None` when images are not backed up.
    pub fn images_cap(&self) -> Option<u64> {
        self.attachments.then_some(self.attachments_cap)
    }
}

/// The `[vault]` table as written: a host's or a vault's keys.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct VaultFile {
    address: Option<String>,
    fingerprint: Option<String>,
    pairing_code: Option<String>,
    attachments: Option<bool>,
    attachments_cap: Option<u64>,
    archive_retention_days: Option<u32>,
}

/// When the daemon prompts an idle agent on its own: the `[follow_ups]` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FollowUpsConfig {
    /// Whether an agent is prompted when its pull request's checks fail or pass, it stops
    /// merging cleanly, or a reviewer requests changes.
    pub pr_events: bool,
    /// Seconds an agent may sit idle with its work unfinished before it is prompted; 0 never
    /// prompts it.
    pub stall_after_secs: u64,
    /// Most times a stalled agent is prompted before it is left to a user.
    pub max_stall_nudges: u32,
}

impl Default for FollowUpsConfig {
    fn default() -> Self {
        let FollowUpSettings {
            pr_events,
            stall_after_secs,
            max_stall_nudges,
        } = FollowUpSettings::default();
        Self {
            pr_events,
            stall_after_secs,
            max_stall_nudges,
        }
    }
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
    listen: Listen,
    data_dir: Option<PathBuf>,
    log: LogConfig,
    accounts: Vec<AccountFile>,
    providers: BTreeMap<String, ProviderFile>,
    tasks: TaskLimits,
    failover: FailoverConfig,
    titles: TitlesFile,
    follow_ups: FollowUpsConfig,
    resources: ResourcesConfig,
    projects: ProjectsFile,
    project: Vec<ProjectFile>,
    mode: Mode,
    vault: Option<VaultFile>,
}

impl Default for ConfigFile {
    fn default() -> Self {
        Self {
            listen: Listen::One(SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::UNSPECIFIED,
                DEFAULT_PORT,
            ))),
            data_dir: None,
            log: LogConfig::default(),
            accounts: Vec::new(),
            providers: BTreeMap::new(),
            tasks: TaskLimits::default(),
            failover: FailoverConfig::default(),
            titles: TitlesFile::default(),
            follow_ups: FollowUpsConfig::default(),
            resources: ResourcesConfig::default(),
            projects: ProjectsFile::default(),
            project: Vec::new(),
            mode: Mode::default(),
            vault: None,
        }
    }
}

/// `listen` as written: one address or a list.
#[derive(Debug, Deserialize)]
#[serde(
    untagged,
    expecting = "an address such as \"0.0.0.0:7447\", or a list of them"
)]
enum Listen {
    One(SocketAddr),
    Many(Vec<SocketAddr>),
}

/// The addresses `listen` names: at least one, none twice.
fn resolve_listen(listen: Listen) -> Result<Vec<SocketAddr>> {
    let addresses = match listen {
        Listen::One(address) => vec![address],
        Listen::Many(addresses) => addresses,
    };
    ensure!(!addresses.is_empty(), "`listen` names no address");
    let mut seen = HashSet::new();
    for address in &addresses {
        ensure!(seen.insert(address), "`listen` names {address} twice");
    }
    Ok(addresses)
}

/// The `[titles]` table as written.
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TitlesFile {
    enabled: bool,
    provider: Option<String>,
    model: Option<String>,
    account: Option<String>,
}

impl Default for TitlesFile {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: None,
            model: None,
            account: None,
        }
    }
}

/// The `[projects]` table as written.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ProjectsFile {
    roots: Vec<PathBuf>,
    exclude: Vec<PathBuf>,
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
    default_permission_mode: Option<PermissionMode>,
    setup_command: Option<String>,
    icon: Option<PathBuf>,
    icon_background: Option<String>,
}

/// One `[[accounts]]` entry as written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountFile {
    id: String,
    provider: String,
    label: Option<String>,
    config_dir: Option<PathBuf>,
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
        resolve(path, file, &env)
    }

    /// Loads the config file at `path`, or the defaults when there is none, as the daemon
    /// would at its next start.
    pub fn load_file(path: &Path) -> Result<Self> {
        let file = read(path)?.unwrap_or_default();
        resolve(path.to_owned(), file, &|key| std::env::var_os(key))
    }

    /// The daemon-wide settings, as clients see them.
    pub fn settings(&self) -> DaemonSettings {
        let path = |path: &PathBuf| path.display().to_string();
        let mut binaries: Vec<_> = self
            .binaries
            .iter()
            .map(|(provider, binary)| ProviderBinary {
                provider: provider.clone(),
                binary: path(binary),
            })
            .collect();
        binaries.sort_by(|a, b| a.provider.as_str().cmp(b.provider.as_str()));
        let resources = &self.resources;
        let vault = self.vault.as_ref();
        DaemonSettings {
            listen: self.listen.iter().map(ToString::to_string).collect(),
            log: LogSettings {
                level: self.log.level.clone(),
                format: match self.log.format {
                    LogFormat::Pretty => herder_protocol::LogFormat::Pretty,
                    LogFormat::Json => herder_protocol::LogFormat::Json,
                },
            },
            binaries,
            tasks: TaskSettings {
                max_children: self.tasks.max_children,
            },
            failover: self.failover.settings(),
            titles: TitleSettings {
                enabled: self.titles.enabled,
                provider: self.titles.provider.clone(),
                model: self.titles.model.clone(),
                account: self.titles.account.clone(),
            },
            follow_ups: FollowUpSettings {
                pr_events: self.follow_ups.pr_events,
                stall_after_secs: self.follow_ups.stall_after_secs,
                max_stall_nudges: self.follow_ups.max_stall_nudges,
            },
            resources: ResourceSettings {
                memory_max_percent: resources.memory_max_percent,
                memory_high_percent: resources.memory_high_percent,
                cpu_weight: resources.cpu_weight,
                child_cpu_weight: resources.child_cpu_weight,
                nice: resources.nice,
                max_turns: resources.max_turns,
                min_memory_available_mib: resources.min_memory_available_mib,
                max_memory_pressure: resources.max_memory_pressure,
                max_load_percent: resources.max_load_percent,
            },
            projects: ProjectDiscovery {
                roots: self.projects.roots.iter().map(path).collect(),
                exclude: self.projects.exclude.iter().map(path).collect(),
                setup_timeout_secs: self.projects.setup_timeout.as_secs(),
            },
            backup: BackupSettings {
                attachments: vault.is_some_and(|vault| vault.attachments),
                attachments_cap: vault
                    .map_or(DEFAULT_ATTACHMENTS_CAP, |vault| vault.attachments_cap),
                archive_retention_days: self.retention.archive_days,
            },
        }
    }
}

/// Validates the file as written, read from `path`, and resolves the configuration it says.
fn resolve(
    path: PathBuf,
    file: ConfigFile,
    env: &impl Fn(&str) -> Option<OsString>,
) -> Result<Config> {
    let data_dir = match file.data_dir {
        Some(dir) => dir,
        None => xdg_dir(env, "XDG_DATA_HOME", ".local/share")?.join("herder"),
    };
    let listen = resolve_listen(file.listen)?;
    file.resources.validate()?;
    let accounts = resolve_accounts(file.accounts, env)?;
    let projects = resolve_projects(file.projects, file.project, &accounts, env)?;
    let titles = resolve_titles(file.titles, &accounts)?;
    let (vault, retention) = resolve_vault(file.mode, file.vault)?;
    Ok(Config {
        path,
        listen,
        data_dir,
        log: file.log,
        accounts,
        binaries: resolve_binaries(file.providers, env)?,
        tasks: file.tasks,
        failover: file.failover,
        titles,
        follow_ups: file.follow_ups,
        resources: file.resources,
        projects,
        mode: file.mode,
        vault,
        retention,
    })
}

/// Validates the `[vault]` table: a host's says where it replicates to, a vault's what it
/// keeps.
fn resolve_vault(mode: Mode, table: Option<VaultFile>) -> Result<(Option<VaultConfig>, Retention)> {
    let Some(table) = table else {
        return Ok((None, Retention::default()));
    };
    if mode == Mode::Vault {
        ensure!(
            table.address.is_none()
                && table.fingerprint.is_none()
                && table.pairing_code.is_none()
                && table.attachments.is_none()
                && table.attachments_cap.is_none(),
            "a vault does not replicate to another vault; its [vault] table takes \
             archive_retention_days only"
        );
        let mut retention = Retention::default();
        if let Some(days) = table.archive_retention_days {
            ensure!(days > 0, "vault.archive_retention_days must be at least 1");
            retention.archive_days = days;
        }
        return Ok((None, retention));
    }
    ensure!(
        table.archive_retention_days.is_none(),
        "vault.archive_retention_days is set on the vault, not on a host"
    );
    let (Some(address), Some(fingerprint)) = (table.address, table.fingerprint) else {
        bail!("the [vault] table needs the vault's address and fingerprint");
    };
    ensure!(
        fingerprint.len() == 64 && fingerprint.chars().all(|c| c.is_ascii_hexdigit()),
        "vault.fingerprint must be the 64 hex digits `herder pair` prints"
    );
    let vault = VaultConfig {
        attachments: table.attachments.unwrap_or(false),
        attachments_cap: table.attachments_cap.unwrap_or(DEFAULT_ATTACHMENTS_CAP),
        ..VaultConfig::new(
            address,
            fingerprint.to_ascii_lowercase(),
            table.pairing_code,
        )
    };
    Ok((Some(vault), Retention::default()))
}

/// Validates the `[titles]` table.
fn resolve_titles(table: TitlesFile, accounts: &Accounts) -> Result<TitlesConfig> {
    let titles = |provider: &Provider| matches!(provider, Provider::Claude | Provider::Codex);
    let provider = table.provider.map(Provider::from);
    if let Some(provider) = &provider {
        ensure!(
            titles(provider),
            "titles.provider: herder titles sessions with claude or codex, not {}",
            provider.as_str()
        );
    }
    let account = table.account.map(AccountId::new);
    if let Some(id) = &account {
        let config = accounts
            .get(id)
            .with_context(|| format!("titles.account: {id} is not an account"))?;
        ensure!(
            titles(&config.provider),
            "titles.account: herder titles sessions with claude or codex, and {id} is a {} \
             account",
            config.provider.as_str()
        );
        if let Some(provider) = &provider {
            ensure!(
                *provider == config.provider,
                "titles.account: {id} is not a {} account",
                provider.as_str()
            );
        }
    }
    ensure!(
        table
            .model
            .as_ref()
            .is_none_or(|model| !model.trim().is_empty()),
        "titles.model is empty"
    );
    Ok(TitlesConfig {
        enabled: table.enabled,
        provider,
        model: table.model,
        account,
    })
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
    let exclude = table
        .exclude
        .iter()
        .map(|path| {
            Ok(resolve_path(path, env)
                .context("projects.exclude")?
                .components()
                .collect())
        })
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
            if let Some(icon) = &entry.icon {
                ensure!(
                    icon.is_relative(),
                    "{which}: icon must be relative to the project's clone"
                );
            }
            if let Some(colour) = &entry.icon_background {
                ensure!(
                    is_rgb_hex(colour),
                    "{which}: icon_background {colour:?} is not a #rrggbb colour"
                );
            }
            Ok(ProjectEntry {
                name: entry.name,
                remotes,
                paths,
                default_account,
                default_permission_mode: entry.default_permission_mode,
                setup_command: entry.setup_command,
                icon: entry.icon,
                icon_background: entry.icon_background,
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
        exclude,
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
    let _write = CONFIG_WRITE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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

/// Update an account without reading its login files, validating the complete account list
/// before replacing the daemon config. The caller serializes this with other account writes.
pub(crate) fn set_account_settings(
    path: &Path,
    account_id: &AccountId,
    previous: &AccountConfig,
    label: &str,
    config_dir: Option<&str>,
    may_change_directory: bool,
) -> Result<AccountConfig> {
    let _write = CONFIG_WRITE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ensure!(!label.trim().is_empty(), "account label must not be empty");
    let text = std::fs::read_to_string(path)?;
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    let table = doc
        .get_mut("accounts")
        .and_then(toml_edit::Item::as_array_of_tables_mut)
        .and_then(|entries| {
            entries.iter_mut().find(|entry| {
                entry.get("id").and_then(toml_edit::Item::as_str) == Some(account_id.as_str())
            })
        })
        .context("account no longer exists in the daemon config")?;
    ensure!(
        table.get("provider").and_then(toml_edit::Item::as_str) == Some(previous.provider.as_str()),
        "account provider changed on disk; restart the daemon first"
    );
    table.insert("label", toml_edit::value(label.trim()));
    match config_dir {
        Some(dir) => {
            table.insert("config_dir", toml_edit::value(dir));
        }
        None => {
            table.remove("config_dir");
        }
    }
    let text = doc.to_string();
    let file: ConfigFile = toml::from_str(&text)?;
    let accounts = resolve_accounts(file.accounts, &|key| std::env::var_os(key))?;
    let account = accounts
        .get(account_id)
        .context("account disappeared from config")?
        .clone();
    ensure!(
        may_change_directory || account.config_dir == previous.config_dir,
        "archive all sessions on this machine before changing a login directory"
    );
    write_atomically(path, text.as_bytes())?;
    Ok(account)
}

/// The `[projects]` table and `[[project]]` entries of the config file at `path`, resolved as
/// [`Config::load`] resolves them; the defaults when the file does not exist.
pub fn read_projects(path: &Path) -> Result<ProjectsConfig> {
    let env = |key: &str| std::env::var_os(key);
    let file = read(path)?.unwrap_or_default();
    let accounts = resolve_accounts(file.accounts, &env)?;
    resolve_projects(file.projects, file.project, &accounts, &env)
}

/// Adds `clone` to the paths of the `[[project]]` entry `entry` of the config file at `path`,
/// counted in file order, or declares it in a new entry when `None`, and takes it out of
/// `[projects] exclude`; returns the projects as the file now resolves them. See
/// [`edit_config`].
pub fn add_project(path: &Path, entry: Option<usize>, clone: &Path) -> Result<ProjectsConfig> {
    let clone = clone
        .to_str()
        .with_context(|| format!("{} is not UTF-8", clone.display()))?
        .to_owned();
    edit_config(path, |doc, env| {
        // A clone of a removed project comes back.
        if let Some(exclude) = doc
            .get_mut("projects")
            .and_then(|table| table.get_mut("exclude"))
            .and_then(toml_edit::Item::as_array_mut)
        {
            exclude.retain(|path| !names(path, Path::new(&clone), env));
        }
        let paths = project_table(doc, entry, path)?
            .entry("paths")
            .or_insert_with(|| toml_edit::value(toml_edit::Array::new()))
            .as_array_mut()
            .context("its paths are not an array")?;
        paths.push(clone);
        Ok(())
    })
}

/// Removes the clones `clones` of a project, as discovery found them, from the config file at
/// `path`: drops them from the `paths` of every `[[project]]` entry, and each entry that is
/// left with neither `remotes` nor `paths`, and adds them to `[projects] exclude`; returns the
/// projects as the file now resolves them. The rest of the file is kept as written, and only
/// a file that still loads replaces it.
pub fn remove_project(path: &Path, clones: &[PathBuf]) -> Result<ProjectsConfig> {
    let clones = clones
        .iter()
        .map(|clone| {
            clone
                .to_str()
                .with_context(|| format!("{} is not UTF-8", clone.display()))
        })
        .collect::<Result<Vec<_>>>()?;
    edit_config(path, |doc, env| {
        let removed = |value: &toml_edit::Value| {
            clones
                .iter()
                .any(|clone| names(value, Path::new(clone), env))
        };
        if let Some(entries) = doc
            .get_mut("project")
            .and_then(toml_edit::Item::as_array_of_tables_mut)
        {
            for table in entries.iter_mut() {
                if let Some(paths) = table
                    .get_mut("paths")
                    .and_then(toml_edit::Item::as_array_mut)
                {
                    paths.retain(|path| !removed(path));
                }
            }
            entries.retain(|table| {
                let empty = |key| {
                    table
                        .get(key)
                        .and_then(toml_edit::Item::as_array)
                        .is_none_or(toml_edit::Array::is_empty)
                };
                !(empty("remotes") && empty("paths"))
            });
        }
        let exclude = doc
            .entry("projects")
            .or_insert(toml_edit::table())
            .as_table_like_mut()
            .context("projects is not a table")?
            .entry("exclude")
            .or_insert(toml_edit::value(toml_edit::Array::new()))
            .as_array_mut()
            .context("projects.exclude is not an array")?;
        for clone in &clones {
            if !exclude
                .iter()
                .any(|path| names(path, Path::new(clone), env))
            {
                exclude.push(*clone);
            }
        }
        Ok(())
    })
}

/// Whether `text` is a colour as `#rrggbb`, in either case.
pub fn is_rgb_hex(text: &str) -> bool {
    text.strip_prefix('#')
        .is_some_and(|hex| hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Whether the path string `value` of the config file names `path`, once resolved.
fn names(value: &toml_edit::Value, path: &Path, env: &dyn Fn(&str) -> Option<OsString>) -> bool {
    value
        .as_str()
        .and_then(|text| resolve_path(Path::new(text), &env).ok())
        .is_some_and(|resolved| resolved.components().eq(path.components()))
}

/// A project's settings as a client sets them; `None` leaves a setting out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectSettings {
    /// Display name; the last segment of the project id when absent.
    pub name: Option<String>,
    /// Permission mode new sessions start in when none is given.
    pub default_permission_mode: Option<PermissionMode>,
    /// Account new sessions use when none is given.
    pub default_account: Option<AccountId>,
    /// Shell command run in each new worktree.
    pub setup_command: Option<String>,
    /// Colour drawn behind the project's icon, as `#rrggbb`.
    pub icon_background: Option<String>,
}

/// Replaces the settings of the `[[project]]` entry `entry` of the config file at `path`,
/// counted in file order, or adds an entry declaring `clone` with them when `None`; returns
/// the projects as the file now resolves them. See [`edit_config`].
pub fn set_project_settings(
    path: &Path,
    entry: Option<usize>,
    clone: &Path,
    settings: &ProjectSettings,
) -> Result<ProjectsConfig> {
    let clone = clone
        .to_str()
        .with_context(|| format!("{} is not UTF-8", clone.display()))?
        .to_owned();
    edit_config(path, |doc, _| {
        let table = project_table(doc, entry, path)?;
        if entry.is_none() {
            table.insert(
                "paths",
                toml_edit::value(toml_edit::Array::from_iter([clone])),
            );
        }
        let mode = settings.default_permission_mode.map(|mode| match mode {
            PermissionMode::ReadOnly => "read_only",
            PermissionMode::Ask => "ask",
            PermissionMode::AutoEdit => "auto_edit",
            PermissionMode::FullAccess => "full_access",
        });
        let account = settings.default_account.as_ref().map(AccountId::as_str);
        for (key, value) in [
            ("name", settings.name.as_deref()),
            ("default_permission_mode", mode),
            ("default_account", account),
            ("setup_command", settings.setup_command.as_deref()),
            ("icon_background", settings.icon_background.as_deref()),
        ] {
            match value {
                Some(value) => {
                    table.insert(key, toml_edit::value(value));
                }
                None => {
                    table.remove(key);
                }
            }
        }
        Ok(())
    })
}

/// Sets the `[vault]` table of the config file at `path` to `vault`, or removes it when
/// `None`; the rest of the file is kept as written, and only a file that still loads replaces
/// it. A `pairing_code` is never written: the host is paired by the time it is kept.
pub fn set_vault(path: &Path, vault: Option<&VaultConfig>) -> Result<()> {
    edit_config(path, |doc, _| {
        match vault {
            Some(vault) => {
                let mut table = toml_edit::Table::new();
                table.insert("address", toml_edit::value(vault.address.as_str()));
                table.insert("fingerprint", toml_edit::value(vault.fingerprint.as_str()));
                if vault.attachments {
                    table.insert("attachments", toml_edit::value(true));
                }
                if vault.attachments_cap != DEFAULT_ATTACHMENTS_CAP {
                    let cap = i64::try_from(vault.attachments_cap).unwrap_or(i64::MAX);
                    table.insert("attachments_cap", toml_edit::value(cap));
                }
                doc.insert("vault", toml_edit::Item::Table(table));
            }
            None => {
                doc.remove("vault");
            }
        }
        Ok(())
    })?;
    Ok(())
}

/// Changes the config file at `path` from the settings `old` it holds to `new`: only the keys
/// whose value changed are written, keeping a comment after each, so the file keeps every key
/// it leaves at its default and the rest as written. A key set back to its default stays in
/// the file; an optional one cleared is removed. Only a file that still loads replaces it.
/// Returns the configuration the new file resolves to.
pub fn set_settings(path: &Path, old: &DaemonSettings, new: &DaemonSettings) -> Result<Config> {
    edit_file(path, |doc, _| {
        let mut edit = Edit { doc, path };
        if old.listen != new.listen {
            // One address stays a plain string, as most files write it.
            let listen = match new.listen.as_slice() {
                [one] => one.as_str().into(),
                many => toml_edit::Array::from_iter(many).into(),
            };
            edit.set(&[], "listen", Some(listen))?;
        }
        if old.log.level != new.log.level {
            edit.set(&["log"], "level", Some(new.log.level.as_str().into()))?;
        }
        if old.log.format != new.log.format {
            let format = match new.log.format {
                herder_protocol::LogFormat::Pretty => "pretty",
                herder_protocol::LogFormat::Json => "json",
            };
            edit.set(&["log"], "format", Some(format.into()))?;
        }
        for provider in old
            .binaries
            .iter()
            .chain(&new.binaries)
            .map(|b| &b.provider)
        {
            let binary = |settings: &DaemonSettings| {
                settings
                    .binaries
                    .iter()
                    .find(|binary| binary.provider == *provider)
                    .map(|binary| binary.binary.clone())
            };
            let (was, is) = (binary(old), binary(new));
            if was != is {
                edit.set(
                    &["providers", provider.as_str()],
                    "binary",
                    is.map(Into::into),
                )?;
            }
        }
        if old.tasks.max_children != new.tasks.max_children {
            edit.set(
                &["tasks"],
                "max_children",
                Some(int(new.tasks.max_children)?),
            )?;
        }
        if old.failover.pin != new.failover.pin {
            edit.set(&["failover"], "pin", Some(new.failover.pin.into()))?;
        }
        let (was, is) = (&old.titles, &new.titles);
        if was.enabled != is.enabled {
            edit.set(&["titles"], "enabled", Some(is.enabled.into()))?;
        }
        if was.provider != is.provider {
            let provider = is.provider.as_ref().map(|p| p.as_str().into());
            edit.set(&["titles"], "provider", provider)?;
        }
        if was.model != is.model {
            edit.set(&["titles"], "model", is.model.as_deref().map(Into::into))?;
        }
        if was.account != is.account {
            let account = is.account.as_ref().map(|id| id.as_str().into());
            edit.set(&["titles"], "account", account)?;
        }
        let (was, is) = (&old.follow_ups, &new.follow_ups);
        if was.pr_events != is.pr_events {
            edit.set(&["follow_ups"], "pr_events", Some(is.pr_events.into()))?;
        }
        if was.stall_after_secs != is.stall_after_secs {
            let secs = int(is.stall_after_secs)?;
            edit.set(&["follow_ups"], "stall_after_secs", Some(secs))?;
        }
        if was.max_stall_nudges != is.max_stall_nudges {
            let nudges = int(is.max_stall_nudges)?;
            edit.set(&["follow_ups"], "max_stall_nudges", Some(nudges))?;
        }
        let (was, is) = (&old.resources, &new.resources);
        for (key, changed, value) in [
            (
                "memory_max_percent",
                was.memory_max_percent != is.memory_max_percent,
                int(is.memory_max_percent)?,
            ),
            (
                "memory_high_percent",
                was.memory_high_percent != is.memory_high_percent,
                int(is.memory_high_percent)?,
            ),
            (
                "cpu_weight",
                was.cpu_weight != is.cpu_weight,
                int(is.cpu_weight)?,
            ),
            (
                "child_cpu_weight",
                was.child_cpu_weight != is.child_cpu_weight,
                int(is.child_cpu_weight)?,
            ),
            ("nice", was.nice != is.nice, int(is.nice)?),
            (
                "min_memory_available_mib",
                was.min_memory_available_mib != is.min_memory_available_mib,
                int(is.min_memory_available_mib)?,
            ),
            (
                "max_memory_pressure",
                was.max_memory_pressure != is.max_memory_pressure,
                int(is.max_memory_pressure)?,
            ),
            (
                "max_load_percent",
                was.max_load_percent != is.max_load_percent,
                int(is.max_load_percent)?,
            ),
        ] {
            if changed {
                edit.set(&["resources"], key, Some(value))?;
            }
        }
        if was.max_turns != is.max_turns {
            let max_turns = is.max_turns.map(int).transpose()?;
            edit.set(&["resources"], "max_turns", max_turns)?;
        }
        let (was, is) = (&old.projects, &new.projects);
        if was.roots != is.roots {
            let roots = toml_edit::Array::from_iter(&is.roots);
            edit.set(&["projects"], "roots", Some(roots.into()))?;
        }
        if was.exclude != is.exclude {
            let exclude = toml_edit::Array::from_iter(&is.exclude);
            edit.set(&["projects"], "exclude", Some(exclude.into()))?;
        }
        if was.setup_timeout_secs != is.setup_timeout_secs {
            let secs = int(is.setup_timeout_secs)?;
            edit.set(&["projects"], "setup_timeout_secs", Some(secs))?;
        }
        let (was, is) = (&old.backup, &new.backup);
        if was.attachments != is.attachments {
            edit.set(&["vault"], "attachments", Some(is.attachments.into()))?;
        }
        if was.attachments_cap != is.attachments_cap {
            let cap = int(is.attachments_cap)?;
            edit.set(&["vault"], "attachments_cap", Some(cap))?;
        }
        if was.archive_retention_days != is.archive_retention_days {
            let days = int(is.archive_retention_days)?;
            edit.set(&["vault"], "archive_retention_days", Some(days))?;
        }
        Ok(())
    })
}

/// `value` as a TOML integer.
fn int<T: TryInto<i64>>(value: T) -> Result<toml_edit::Value> {
    let value: i64 = value
        .try_into()
        .map_err(|_| anyhow::anyhow!("a value is too large for the config file"))?;
    Ok(value.into())
}

/// Edits keys of the config file at `path`, parsed as `doc`.
struct Edit<'a> {
    doc: &'a mut toml_edit::DocumentMut,
    path: &'a Path,
}

impl Edit<'_> {
    /// Sets `key` of the table at `tables` to `value`, keeping the comments around the value
    /// it replaces, or removes it when `None`; adds missing tables, and removes a table the
    /// removal leaves empty.
    fn set(&mut self, tables: &[&str], key: &str, value: Option<toml_edit::Value>) -> Result<()> {
        let path = self.path;
        let mut table = self.doc.as_table_mut() as &mut dyn toml_edit::TableLike;
        for (depth, name) in tables.iter().enumerate() {
            if value.is_none() && !table.contains_key(name) {
                return Ok(());
            }
            let mut new = toml_edit::Table::new();
            // `[providers.claude]` needs no `[providers]` header of its own.
            new.set_implicit(depth + 1 < tables.len());
            table = table
                .entry(name)
                .or_insert(toml_edit::Item::Table(new))
                .as_table_like_mut()
                .with_context(|| {
                    format!(
                        "{} in {} is not a table",
                        tables[..=depth].join("."),
                        path.display()
                    )
                })?;
        }
        match value {
            // Replaced in place, so comments on the line above the key stay too.
            Some(mut value) => match table.get_mut(key).and_then(toml_edit::Item::as_value_mut) {
                Some(previous) => {
                    *value.decor_mut() = previous.decor().clone();
                    *previous = value;
                }
                None => {
                    table.insert(key, toml_edit::Item::Value(value));
                }
            },
            None => {
                table.remove(key);
                if table.is_empty()
                    && let Some((last, parents)) = tables.split_last()
                {
                    self.remove_empty(parents, last);
                }
            }
        }
        Ok(())
    }

    /// Removes the empty table `name` under `parents`.
    fn remove_empty(&mut self, parents: &[&str], name: &str) {
        let mut table = self.doc.as_table_mut() as &mut dyn toml_edit::TableLike;
        for parent in parents {
            match table
                .get_mut(parent)
                .and_then(toml_edit::Item::as_table_like_mut)
            {
                Some(next) => table = next,
                None => return,
            }
        }
        table.remove(name);
    }
}

/// The `[[project]]` entry `entry` of `doc`, the config file at `path`, counted in file order,
/// or a new entry appended when `None`.
fn project_table<'a>(
    doc: &'a mut toml_edit::DocumentMut,
    entry: Option<usize>,
    path: &Path,
) -> Result<&'a mut toml_edit::Table> {
    let entries = doc
        .entry("project")
        .or_insert(toml_edit::Item::ArrayOfTables(
            toml_edit::ArrayOfTables::new(),
        ))
        .as_array_of_tables_mut()
        .with_context(|| format!("project in {} is not an array of tables", path.display()))?;
    match entry {
        Some(index) => entries
            .get_mut(index)
            .with_context(|| format!("{} has no project entry {}", path.display(), index + 1)),
        None => {
            entries.push(toml_edit::Table::new());
            let last = entries.len() - 1;
            entries
                .get_mut(last)
                .context("the new project entry is gone")
        }
    }
}

/// Rewrites the config file at `path` with `edit` applied, keeping the rest of the file as
/// written; creates the file when it does not exist. `edit` gets the environment paths in the
/// file resolve with. The file is replaced atomically, and only if it still loads: an edit that
/// breaks it, such as a path another entry has, fails and changes nothing. Returns the projects
/// as the new file resolves them.
fn edit_config(
    path: &Path,
    edit: impl FnOnce(&mut toml_edit::DocumentMut, &dyn Fn(&str) -> Option<OsString>) -> Result<()>,
) -> Result<ProjectsConfig> {
    Ok(edit_file(path, edit)?.projects)
}

/// Edits the config file at `path` as [`edit_config`] does, returning the whole configuration
/// the new file resolves to.
fn edit_file(
    path: &Path,
    edit: impl FnOnce(&mut toml_edit::DocumentMut, &dyn Fn(&str) -> Option<OsString>) -> Result<()>,
) -> Result<Config> {
    let _write = CONFIG_WRITE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let env = |key: &str| std::env::var_os(key);
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            return Err(err).with_context(|| format!("reading config file {}", path.display()));
        }
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("parsing config file {}", path.display()))?;
    edit(&mut doc, &env)?;
    let text = doc.to_string();
    let file: ConfigFile =
        toml::from_str(&text).with_context(|| format!("changing {}", path.display()))?;
    let config = resolve(path.to_owned(), file, &env)?;
    write_atomically(path, text.as_bytes())
        .with_context(|| format!("writing config file {}", path.display()))?;
    Ok(config)
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

/// Where `provider`'s CLI keeps the login it uses without a config dir, when pointing its config
/// dir variable there is another login: `~/.claude` for Claude, whose default login keeps its
/// settings in `~/.claude.json` instead.
pub(crate) fn default_login_dir(
    provider: &Provider,
    env: &impl Fn(&str) -> Option<OsString>,
) -> Result<Option<PathBuf>> {
    if *provider != Provider::Claude {
        return Ok(None);
    }
    resolve_path(Path::new("~/.claude"), env).map(Some)
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
            ensure!(
                default_login_dir(&provider, env)?.as_ref() != Some(dir),
                "account {id}: config_dir {} is where {} keeps its default login, \
                 which its config dir variable does not reach; omit config_dir to use it",
                dir.display(),
                provider.as_str()
            );
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
        };
        accounts.insert(AccountId::new(id), account);
    }
    Ok(accounts)
}

// All daemon config mutations share this lock, including project edits and login completion.
static CONFIG_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
        assert_eq!(config.listen, ["0.0.0.0:7447".parse().unwrap()]);
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
        assert_eq!(config.listen, ["127.0.0.1:9000".parse().unwrap()]);
        assert_eq!(config.data_dir, Path::new("/srv/data/herder"));
    }

    #[test]
    fn listen_takes_a_list_of_addresses() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write(
            tmp.path(),
            "listen = [\"127.0.0.1:7447\", \"100.124.135.114:7447\", \"[::1]:7447\"]\n",
        );
        let config = Config::load_with_env(Some(&path), env(&[("HOME", "/h")])).unwrap();
        let expected: Vec<SocketAddr> = ["127.0.0.1:7447", "100.124.135.114:7447", "[::1]:7447"]
            .iter()
            .map(|addr| addr.parse().unwrap())
            .collect();
        assert_eq!(config.listen, expected);
    }

    #[test]
    fn listen_rejects_an_empty_list_duplicates_and_non_addresses() {
        let tmp = tempfile::tempdir().unwrap();
        for (text, error) in [
            ("listen = []\n", "`listen` names no address"),
            (
                "listen = [\"127.0.0.1:7447\", \"127.0.0.1:7447\"]\n",
                "`listen` names 127.0.0.1:7447 twice",
            ),
            ("listen = \"localhost\"\n", "or a list of them"),
        ] {
            let path = write(tmp.path(), text);
            let err = Config::load_with_env(Some(&path), env(&[("HOME", "/h")])).unwrap_err();
            assert!(format!("{err:#}").contains(error), "{text}: {err:#}");
        }
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
            pin = true
            [follow_ups]
            pr_events = false
            stall_after_secs = 900
            max_stall_nudges = 3
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
                listen: vec!["[::1]:8000".parse().unwrap()],
                data_dir: PathBuf::from("/var/lib/herder"),
                log: LogConfig {
                    level: "debug".to_owned(),
                    format: LogFormat::Json,
                },
                accounts: Accounts::new(),
                binaries: HashMap::new(),
                tasks: TaskLimits { max_children: 2 },
                failover: FailoverConfig { pin: true },
                titles: TitlesConfig::default(),
                follow_ups: FollowUpsConfig {
                    pr_events: false,
                    stall_after_secs: 900,
                    max_stall_nudges: 3,
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
                retention: Retention::default(),
            }
        );
    }

    #[test]
    fn the_vault_table_is_set_and_removed_keeping_the_rest_of_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("daemon.toml");
        let original = "# my daemon\nlisten = \"127.0.0.1:7447\"  # loopback only\n\n\
                        [log]\nlevel = \"debug\"\n";
        std::fs::write(&path, original).unwrap();
        let vault = VaultConfig::new(
            "vault.lan:7447".into(),
            "ab".repeat(32),
            Some("ABCDE-FGHJK".into()),
        );
        set_vault(&path, Some(&vault)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(original), "{text}");
        assert!(!text.contains("ABCDE"), "the code is never kept: {text}");
        let config = Config::load_with_env(Some(&path), env(&[("HOME", "/home/dev")])).unwrap();
        assert_eq!(
            config.vault,
            Some(VaultConfig {
                pairing_code: None,
                ..vault.clone()
            })
        );

        // Setting it again replaces it; removing it gives the file back as it was.
        let moved = VaultConfig {
            address: "10.0.0.9:7447".into(),
            ..vault
        };
        set_vault(&path, Some(&moved)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("[vault]").count(), 1, "{text}");
        assert!(text.contains("10.0.0.9:7447"), "{text}");
        set_vault(&path, None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);

        // A host with no config file gets one.
        let fresh = tmp.path().join("new/daemon.toml");
        set_vault(&fresh, Some(&moved)).unwrap();
        let config = Config::load_with_env(Some(&fresh), env(&[("HOME", "/home/dev")])).unwrap();
        assert_eq!(config.vault.unwrap().address, "10.0.0.9:7447");
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
            Some(VaultConfig::new(
                "vault:7447".into(),
                "ab".repeat(32),
                Some("ABCDE-FGHJK".into())
            ))
        );
        // Images are not backed up unless the host says so.
        assert_eq!(config.vault.unwrap().images_cap(), None);

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
    fn images_and_retention_in_the_vault_table() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("daemon.toml");
        let load = |text: String| {
            std::fs::write(&path, text).unwrap();
            Config::load_with_env(Some(&path), env(&[("HOME", "/home/dev")]))
        };
        let host = format!(
            "[vault]\naddress = \"vault:7447\"\nfingerprint = \"{}\"\n",
            "ab".repeat(32)
        );
        let vault = load(format!("{host}attachments = true\n"))
            .unwrap()
            .vault
            .unwrap();
        assert_eq!(vault.images_cap(), Some(DEFAULT_ATTACHMENTS_CAP));
        let vault = load(format!(
            "{host}attachments = true\nattachments_cap = 5000\n"
        ))
        .unwrap()
        .vault
        .unwrap();
        assert_eq!(vault.images_cap(), Some(5000));
        let vault = load(format!("{host}attachments_cap = 5000\n"))
            .unwrap()
            .vault
            .unwrap();
        assert_eq!(vault.images_cap(), None);
        let err = load(format!("{host}archive_retention_days = 3\n")).unwrap_err();
        assert!(format!("{err:#}").contains("on the vault"), "{err:#}");

        // Written back as set, and only what differs from the defaults.
        let both = VaultConfig {
            attachments: true,
            attachments_cap: 5000,
            ..vault.clone()
        };
        set_vault(&path, Some(&both)).unwrap();
        let config = Config::load_with_env(Some(&path), env(&[("HOME", "/home/dev")])).unwrap();
        assert_eq!(config.vault, Some(both));
        let plain = VaultConfig::new(vault.address.clone(), vault.fingerprint.clone(), None);
        set_vault(&path, Some(&plain)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("attachments"), "{text}");

        let config = load("mode = \"vault\"\n".into()).unwrap();
        assert_eq!(config.retention.archive_days, 90);
        let config = load("mode = \"vault\"\n[vault]\narchive_retention_days = 30\n".into());
        assert_eq!(config.unwrap().retention.archive_days, 30);
        assert!(load("mode = \"vault\"\n[vault]\narchive_retention_days = 0\n".into()).is_err());
        assert!(load("mode = \"vault\"\n[vault]\nattachments = true\n".into()).is_err());
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
            "[follow_ups]\nnudges = 2\n",
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
    fn a_missing_file_shows_the_default_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = Config::load_file(&tmp.path().join("daemon.toml"))
            .unwrap()
            .settings();
        assert_eq!(settings.listen, ["0.0.0.0:7447"]);
        assert_eq!(settings.log.format, herder_protocol::LogFormat::Pretty);
        assert_eq!(settings.tasks.max_children, 5);
        assert!(settings.titles.enabled);
        assert_eq!(settings.resources.max_turns, None);
        assert_eq!(settings.resources.nice, 10);
        assert_eq!(settings.projects.setup_timeout_secs, 600);
        assert_eq!(settings.backup.attachments_cap, DEFAULT_ATTACHMENTS_CAP);
        assert_eq!(settings.backup.archive_retention_days, 90);
        assert!(settings.follow_ups.pr_events);
        assert_eq!(settings.follow_ups.stall_after_secs, 0);
        assert_eq!(settings.follow_ups.max_stall_nudges, 2);
    }

    #[test]
    fn settings_write_only_the_keys_that_changed_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("daemon.toml");
        let kept = "# my daemon\nlisten = \"127.0.0.1:7777\" # loopback only\n\n\
                    [resources]\nnice = 5\nmax_turns = 2 # two at most\n\n\
                    [failover]\npin = true\n";
        std::fs::write(&path, kept).unwrap();
        let old = Config::load_file(&path).unwrap().settings();
        let mut new = old.clone();
        new.listen = vec!["127.0.0.1:7448".into()];
        new.resources.max_turns = Some(6);
        new.titles.model = Some("haiku".into());
        new.follow_ups.stall_after_secs = 600;
        new.binaries.push(ProviderBinary {
            provider: Provider::Claude,
            binary: "/opt/claude/bin/claude".into(),
        });
        let config = set_settings(&path, &old, &new).unwrap();
        assert_eq!(config.settings(), new);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# my daemon\nlisten = \"127.0.0.1:7448\" # loopback only\n\n\
             [resources]\nnice = 5\nmax_turns = 6 # two at most\n\n\
             [failover]\npin = true\n\n\
             [providers.claude]\nbinary = \"/opt/claude/bin/claude\"\n\n\
             [titles]\nmodel = \"haiku\"\n\n\
             [follow_ups]\nstall_after_secs = 600\n"
        );

        // Unchanged settings write nothing.
        let text = std::fs::read_to_string(&path).unwrap();
        set_settings(&path, &new, &new).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn a_cleared_setting_leaves_the_file_and_an_emptied_table_with_it() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("daemon.toml");
        std::fs::write(
            &path,
            "[resources]\nmax_turns = 2\n\n[providers.claude]\nbinary = \"/opt/claude\"\n",
        )
        .unwrap();
        let old = Config::load_file(&path).unwrap().settings();
        let mut new = old.clone();
        new.resources.max_turns = None;
        new.binaries.clear();
        set_settings(&path, &old, &new).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
    }

    #[test]
    fn settings_the_daemon_would_not_load_change_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("daemon.toml");
        std::fs::write(&path, "# mine\n").unwrap();
        let old = Config::load_file(&path).unwrap().settings();
        let mut cases = Vec::new();
        let mut new = old.clone();
        new.resources.nice = 30;
        cases.push((new, "resources.nice"));
        let mut new = old.clone();
        new.titles.account = Some(AccountId::new("nobody"));
        cases.push((new, "titles.account"));
        let mut new = old.clone();
        new.backup.archive_retention_days = 30;
        cases.push((new, "archive_retention_days"));
        for (new, key) in cases {
            let err = set_settings(&path, &old, &new).unwrap_err();
            assert!(format!("{err:#}").contains(key), "{key}: {err:#}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "# mine\n");
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
            ("max_turns = 65", "max_turns"),
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
                    }
                ),
                (
                    AccountId::new("claude-work"),
                    AccountConfig {
                        provider: Provider::Claude,
                        label: "claude-work".into(),
                        config_dir: Some(home.path().join(".claude-work")),
                    }
                ),
                (
                    AccountId::new("codex"),
                    AccountConfig {
                        provider: Provider::Codex,
                        label: "codex".into(),
                        config_dir: Some(PathBuf::from("/srv/codex")),
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
    fn titles_are_on_by_default_and_take_a_provider_model_and_account() {
        let tmp = tempfile::tempdir().unwrap();
        let config =
            Config::load_with_env(Some(&write(tmp.path(), "")), env(&[("HOME", "/h")])).unwrap();
        assert_eq!(config.titles, TitlesConfig::default());
        assert!(config.titles.enabled);

        let path = write(
            tmp.path(),
            r#"
            [[accounts]]
            id = "codex-main"
            provider = "codex"
            config_dir = "/codex"

            [titles]
            enabled = false
            provider = "codex"
            model = "gpt-6-luna"
            account = "codex-main"
            "#,
        );
        let config = Config::load_with_env(Some(&path), env(&[("HOME", "/h")])).unwrap();
        assert_eq!(
            config.titles,
            TitlesConfig {
                enabled: false,
                provider: Some(Provider::Codex),
                model: Some("gpt-6-luna".to_owned()),
                account: Some(AccountId::new("codex-main")),
            }
        );
    }

    #[test]
    fn invalid_titles_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let accounts = "[[accounts]]\nid = \"main\"\nprovider = \"claude\"\n\
                        [[accounts]]\nid = \"grok\"\nprovider = \"grok\"\n";
        for (titles, expected) in [
            ("provider = \"grok\"", "with claude or codex, not grok"),
            ("account = \"nobody\"", "nobody is not an account"),
            ("account = \"grok\"", "grok is a grok account"),
            (
                "provider = \"codex\"\naccount = \"main\"",
                "main is not a codex account",
            ),
            ("model = \" \"", "titles.model is empty"),
            ("color = \"red\"", "unknown field"),
        ] {
            let path = write(tmp.path(), &format!("{accounts}[titles]\n{titles}\n"));
            let err = Config::load_with_env(Some(&path), env(&[("HOME", "/h")])).unwrap_err();
            assert!(format!("{err:#}").contains(expected), "{titles}: {err:#}");
        }
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
            // Every account takes part in rotation: there is nothing to opt in to.
            (claude("a", "failover = true"), "unknown field `failover`"),
            (
                "[providers.nope]\nbinary = \"x\"\n".to_owned(),
                "herder cannot run nope",
            ),
            ("[providers.claude]\n".to_owned(), "missing field `binary`"),
            // Failover stays on the session's provider: there is nothing to fall back to.
            (
                "[failover]\nproviders = [\"codex\"]\n".to_owned(),
                "unknown field `providers`",
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
            r##"
            [[accounts]]
            id = "claude-main"
            provider = "claude"

            [projects]
            roots = ["~/Projects", "/srv/src"]
            setup_timeout_secs = 90
            exclude = ["~/Projects/old/"]

            [[project]]
            name = "herder"
            remotes = ["git@github.com:herder-sh/herder.git", "https://gitlab.com/mirror/herder/"]
            paths = ["~/old/herder"]
            default_account = "claude-main"
            default_permission_mode = "auto_edit"
            setup_command = "make bootstrap"
            icon = "design/mark.svg"
            icon_background = "#FFFFFF"

            [[project]]
            paths = ["/srv/scratch"]
            "##,
        )
        .unwrap();
        assert_eq!(
            config.projects,
            ProjectsConfig {
                roots: vec![home.path().join("Projects"), PathBuf::from("/srv/src")],
                setup_timeout: std::time::Duration::from_secs(90),
                exclude: vec![home.path().join("Projects/old")],
                entries: vec![
                    ProjectEntry {
                        name: Some("herder".to_owned()),
                        remotes: vec![
                            ProjectId::new("github.com/herder-sh/herder"),
                            ProjectId::new("gitlab.com/mirror/herder"),
                        ],
                        paths: vec![home.path().join("old/herder")],
                        default_account: Some(AccountId::new("claude-main")),
                        default_permission_mode: Some(PermissionMode::AutoEdit),
                        setup_command: Some("make bootstrap".to_owned()),
                        icon: Some(PathBuf::from("design/mark.svg")),
                        icon_background: Some("#FFFFFF".to_owned()),
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
            (
                "[[project]]\npaths = [\"/a\"]\nicon = \"/etc/logo.png\"\n",
                "icon must be relative to the project's clone",
            ),
            (
                "[[project]]\npaths = [\"/a\"]\nicon_background = \"white\"\n",
                "icon_background \"white\" is not a #rrggbb colour",
            ),
            (
                "[[project]]\npaths = [\"/a\"]\nicon_background = \"#fff\"\n",
                "is not a #rrggbb colour",
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

    #[test]
    fn projects_are_added_and_set_keeping_the_rest_of_the_file() {
        let home = tempfile::tempdir().unwrap();
        let original = "# my daemon\n[[accounts]]\nid = \"main\" # the default login\n\
                        provider = \"claude\"\n\n[[project]]\nname = \"herder\" # renamed\n\
                        remotes = [\"git@github.com:herder-sh/herder.git\"]\n";
        let path = write(home.path(), original);

        let projects = add_project(&path, None, Path::new("/src/app")).unwrap();
        assert_eq!(projects.entries.len(), 2);
        assert_eq!(projects.entries[1].paths, [PathBuf::from("/src/app")]);
        let projects = add_project(&path, Some(0), Path::new("/src/herder")).unwrap();
        assert_eq!(projects.entries[0].paths, [PathBuf::from("/src/herder")]);

        let settings = ProjectSettings {
            name: Some("The app".into()),
            default_permission_mode: Some(PermissionMode::FullAccess),
            default_account: Some(AccountId::new("main")),
            setup_command: Some("make \"setup\"".into()),
            icon_background: Some("#1a1a1a".into()),
        };
        set_project_settings(&path, Some(1), Path::new("/src/app"), &settings).unwrap();
        let projects = set_project_settings(&path, None, Path::new("/src/new"), &settings).unwrap();
        for entry in &projects.entries[1..] {
            assert_eq!(
                (
                    entry.name.as_deref(),
                    entry.default_permission_mode,
                    entry.default_account.clone(),
                    entry.setup_command.as_deref(),
                    entry.icon_background.as_deref()
                ),
                (
                    Some("The app"),
                    Some(PermissionMode::FullAccess),
                    Some(AccountId::new("main")),
                    Some("make \"setup\""),
                    Some("#1a1a1a")
                )
            );
        }
        assert_eq!(projects.entries[2].paths, [PathBuf::from("/src/new")]);
        // Absent settings are cleared.
        let projects =
            set_project_settings(&path, Some(1), Path::new("/src/app"), &Default::default())
                .unwrap();
        assert_eq!(
            (
                &projects.entries[1].name,
                projects.entries[1].default_permission_mode,
                &projects.entries[1].default_account,
                &projects.entries[1].setup_command,
                &projects.entries[1].icon_background
            ),
            (&None, None, &None, &None, &None)
        );

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with(
                "# my daemon\n[[accounts]]\nid = \"main\" # the default login\n\
                 provider = \"claude\"\n\n[[project]]\nname = \"herder\" # renamed\n"
            ),
            "{text}"
        );
        let home = home.path().to_str().unwrap();
        let config = Config::load_with_env(Some(&path), env(&[("HOME", home)])).unwrap();
        assert_eq!(config.projects, projects);
        assert_eq!(read_projects(&path).unwrap(), projects);
    }

    #[test]
    fn a_project_edit_that_does_not_fit_leaves_the_file_alone() {
        let home = tempfile::tempdir().unwrap();
        let original = "[[project]]\npaths = [\"/src/app\"]\n";
        let path = write(home.path(), original);
        let err = add_project(&path, None, Path::new("/src/app")).unwrap_err();
        assert!(format!("{err:#}").contains("in another entry"), "{err:#}");
        let settings = ProjectSettings {
            default_account: Some(AccountId::new("nobody")),
            ..ProjectSettings::default()
        };
        let err =
            set_project_settings(&path, Some(0), Path::new("/src/app"), &settings).unwrap_err();
        assert!(format!("{err:#}").contains("is not an account"), "{err:#}");
        assert!(set_project_settings(&path, Some(3), Path::new("/x"), &settings).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);

        // A missing file is created.
        let path = home.path().join("new/daemon.toml");
        let projects = add_project(&path, None, Path::new("/src/app")).unwrap();
        assert_eq!(projects.entries[0].paths, [PathBuf::from("/src/app")]);
        assert_eq!(read_projects(&path).unwrap(), projects);
    }

    #[test]
    fn a_removed_project_leaves_the_entries_and_is_excluded() {
        let home = tempfile::tempdir().unwrap();
        let original = "# mine\n[[project]]\nname = \"herder\"\n\
                        remotes = [\"git@github.com:herder-sh/herder.git\"]\n\
                        paths = [\"/src/herder\", \"/src/fork\"]\n\n\
                        [[project]]\npaths = [\"/src/app/\"]\n";
        let path = write(home.path(), original);
        let projects = remove_project(
            &path,
            &[PathBuf::from("/src/app"), PathBuf::from("/src/herder")],
        )
        .unwrap();
        // The entry with remotes keeps its name; the one left empty goes.
        assert_eq!(
            projects.entries,
            [ProjectEntry {
                name: Some("herder".into()),
                remotes: vec![ProjectId::new("github.com/herder-sh/herder")],
                paths: vec![PathBuf::from("/src/fork")],
                ..ProjectEntry::default()
            }]
        );
        assert_eq!(
            projects.exclude,
            [PathBuf::from("/src/app"), PathBuf::from("/src/herder")]
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with("# mine\n")
        );
        // Removing again excludes nothing twice; adding takes it out of the list.
        let projects = remove_project(&path, &[PathBuf::from("/src/app")]).unwrap();
        assert_eq!(projects.exclude.len(), 2);
        let projects = add_project(&path, None, Path::new("/src/app")).unwrap();
        assert_eq!(projects.exclude, [PathBuf::from("/src/herder")]);
        assert_eq!(projects.entries[1].paths, [PathBuf::from("/src/app")]);
        assert_eq!(read_projects(&path).unwrap(), projects);
    }

    fn added(provider: Provider, dir: &Path) -> AccountConfig {
        AccountConfig {
            provider,
            label: "Work \"2\"".into(),
            config_dir: Some(dir.to_owned()),
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
        assert_eq!(config.listen, ["127.0.0.1:9000".parse().unwrap()]);
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
    #[test]
    fn account_settings_preserve_config_and_reject_unsafe_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.toml");
        let original = "# keep this comment\n[[accounts]]\nid = 'work'\nprovider = 'codex'\nlabel = 'Old'\n\n[[accounts]]\nid = 'other'\nprovider = 'codex'\nconfig_dir = '/tmp/herder-other'\n";
        std::fs::write(&path, original).unwrap();
        let id = AccountId::new("work");
        let previous = AccountConfig {
            provider: Provider::Codex,
            label: "Old".into(),
            config_dir: None,
        };
        let renamed = set_account_settings(&path, &id, &previous, " Work ", None, false).unwrap();
        assert_eq!(renamed.label, "Work");
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with("# keep this comment")
        );
        let saved = std::fs::read_to_string(&path).unwrap();
        for (id, label, directory, allowed) in [
            ("work", "", None, true),
            ("missing", "Work", None, true),
            ("work", "Work", Some("relative"), true),
            ("work", "Work", Some("/tmp/herder-other"), true),
            ("work", "Work", Some("/tmp/herder-new"), false),
        ] {
            assert!(
                set_account_settings(
                    &path,
                    &AccountId::new(id),
                    &renamed,
                    label,
                    directory,
                    allowed
                )
                .is_err()
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
        }
        let updated =
            set_account_settings(&path, &id, &renamed, "Work", Some("/tmp/herder-new"), true)
                .unwrap();
        assert_eq!(updated.config_dir, Some(PathBuf::from("/tmp/herder-new")));
        let loaded: ConfigFile = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            resolve_accounts(loaded.accounts, &|key| std::env::var_os(key)).unwrap()[&id],
            updated
        );
    }
}
