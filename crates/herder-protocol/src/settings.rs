//! The daemon's own settings, as clients read and change them.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AccountId, FailoverSettings, Provider};

/// Every daemon-wide setting of the daemon's config file, as values in effect when the file
/// is loaded: a key the file leaves out shows its default. Accounts, projects and the vault
/// link are not here; they have their own commands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonSettings {
    /// Address the daemon listens on, as `ip:port`; an IP of this machine, or `0.0.0.0` or
    /// `[::]` for all of them.
    pub listen: String,
    /// Logging.
    pub log: LogSettings,
    /// The CLI run per provider, where it is not the provider's own name on `PATH`.
    pub binaries: Vec<ProviderBinary>,
    /// Limits on every task.
    pub tasks: TaskSettings,
    /// How sessions fail over when their account hits a limit.
    pub failover: FailoverSettings,
    /// How sessions are titled.
    pub titles: TitleSettings,
    /// What every session's CLI may use, and the budget turns are admitted within.
    pub resources: ResourceSettings,
    /// Where projects are discovered.
    pub projects: ProjectDiscovery,
    /// What is backed up, and kept, on a vault.
    pub backup: BackupSettings,
}

/// The `[log]` table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LogSettings {
    /// A `tracing` env-filter directive, e.g. `info` or `herder_daemon=debug,info`.
    pub level: String,
    /// Output format.
    pub format: LogFormat,
}

/// How the daemon writes its log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    /// Human-readable lines.
    Pretty,
    /// One JSON object per line.
    Json,
}

/// A `[providers.<name>]` table: the CLI to run for a provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderBinary {
    /// The provider.
    pub provider: Provider,
    /// Path of its CLI on the host, absolute or starting with `~/`.
    pub binary: String,
}

/// The `[tasks]` table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskSettings {
    /// Live (not archived) children a task's primary may have at once.
    pub max_children: u32,
}

/// The `[titles]` table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TitleSettings {
    /// Whether sessions get generated titles at all.
    pub enabled: bool,
    /// Provider whose CLI titles, claude or codex; the session's account's when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<Provider>,
    /// Model, in that provider's naming; the provider's quick model when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Account the titling CLI runs on; when absent, the session's own, or else the provider's
    /// available account with the most room left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<AccountId>,
}

/// The `[resources]` table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResourceSettings {
    /// Memory one session may use, in percent of the host's RAM, 1 to 100.
    pub memory_max_percent: u8,
    /// Where a session starts being throttled, in percent of its memory maximum, 1 to 100.
    pub memory_high_percent: u8,
    /// CPU weight of a primary session, 1 to 10000.
    pub cpu_weight: u16,
    /// CPU weight of a child session, 1 to 10000.
    pub child_cpu_weight: u16,
    /// Niceness of every agent CLI, -20 to 19.
    pub nice: i8,
    /// Most turns running at once, 1 to [`MAX_TURNS_LIMIT`](crate::MAX_TURNS_LIMIT); a
    /// quarter of the host's cores, at least 1, when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    /// Least memory available, in MiB, for a new turn to start.
    pub min_memory_available_mib: u64,
    /// Memory pressure (PSI `some avg10`), in percent, at which no new turn starts, 1 to 100.
    pub max_memory_pressure: u8,
    /// One-minute load average, in percent of the cores, at which no new turn starts.
    pub max_load_percent: u16,
}

/// The `[projects]` table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectDiscovery {
    /// Directories scanned for repositories, absolute or starting with `~/`.
    pub roots: Vec<String>,
    /// Repositories left out wherever they are found.
    pub exclude: Vec<String>,
    /// Seconds a project's setup command may run before it is killed.
    pub setup_timeout_secs: u64,
}

/// The values of the `[vault]` table: a host's say what it backs up while linked to a vault,
/// a vault's what it keeps. A value that does not apply to the daemon cannot change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BackupSettings {
    /// On a host linked to a vault: whether prompt images are backed up too.
    pub attachments: bool,
    /// On a host linked to a vault: most bytes of its images the vault keeps.
    pub attachments_cap: u64,
    /// On a vault: days an archived session of an online host is kept after its latest event.
    pub archive_retention_days: u32,
}
