//! The daemon's own settings, as clients read and change them.

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AccountId, FailoverSettings, ProjectId, Provider, SecretRef};

/// Every daemon-wide setting of the daemon's config file, as values in effect when the file
/// is loaded: a key the file leaves out shows its default. Accounts, projects and the vault
/// link are not here; they have their own commands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonSettings {
    /// Addresses the daemon listens on, as `ip:port`, at least one and none twice; each an IP
    /// of this machine, or `0.0.0.0` or `[::]` for all of them.
    pub listen: Vec<String>,
    /// Logging.
    pub log: LogSettings,
    /// The CLI run per provider, where it is not the provider's own name on `PATH`.
    pub binaries: Vec<ProviderBinary>,
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
    /// When the daemon prompts an idle agent on its own; the defaults from daemons before it.
    #[serde(default)]
    pub follow_ups: FollowUpSettings,
    /// The user's MCP servers, which every session's CLI gets next to herder's own, in this
    /// order; none from daemons before them.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerSettings>,
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
    /// The projects dir, absolute or starting with `~/`: where `clone_project` clones
    /// repositories unless told otherwise.
    pub dir: String,
    /// Repositories left out though a session ran in them: the clones of removed projects.
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

/// The `[follow_ups]` table: when the daemon prompts an idle agent on its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FollowUpSettings {
    /// Whether an agent is prompted when its pull request's checks fail or pass, it stops
    /// merging cleanly, or a reviewer requests changes.
    pub pr_events: bool,
    /// Seconds an agent may sit idle with its work unfinished before it is prompted to go on;
    /// 0 never prompts it.
    pub stall_after_secs: u64,
    /// Most times a stalled agent is prompted before it is left to a user.
    pub max_stall_nudges: u32,
}

impl Default for FollowUpSettings {
    fn default() -> Self {
        Self {
            pr_events: true,
            stall_after_secs: 0,
            max_stall_nudges: 2,
        }
    }
}

/// The name herder's own MCP server is registered under; no user server may take it.
pub const HERDER_MCP_SERVER: &str = "herder";

/// Whether `name` may name a user's MCP server: 1 to 64 ASCII letters, digits, `-` and `_`,
/// which every provider takes in a tool name, and not [`HERDER_MCP_SERVER`].
pub fn is_valid_mcp_server_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        && name != HERDER_MCP_SERVER
}

/// One `[[mcp_servers]]` entry: an MCP server of the user's that sessions' CLIs connect to,
/// whatever their provider and account. It is passed to each CLI as it starts, so a change
/// reaches a running session when its CLI next starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct McpServerSettings {
    /// What the CLI registers it as, unique among the servers; see
    /// [`is_valid_mcp_server_name`].
    pub name: String,
    /// Whether sessions get it; a disabled server stays configured.
    pub enabled: bool,
    /// The project whose sessions get it; every session, chats too, when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectId>,
    /// How the CLI reaches it.
    pub transport: McpTransport,
}

/// How a CLI reaches an MCP server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    /// A program the CLI runs, speaking MCP on its stdin and stdout.
    Stdio {
        /// The program, a name looked up on `PATH`, an absolute path, or one starting with
        /// `~/`.
        command: String,
        /// Its arguments.
        #[serde(default)]
        args: Vec<String>,
        /// Variables added to its environment, kept in the daemon's config file as given.
        #[serde(default)]
        env: HashMap<String, String>,
        /// Secrets from the host's vault, each added to its environment as the variable of
        /// its name: the program runs through `herder secret run`, so a value is never in
        /// the config file, on a command line or before an agent.
        #[serde(default)]
        secrets: Vec<SecretRef>,
    },
    /// A server at a URL, over MCP's streamable HTTP transport.
    Http {
        /// Its `http` or `https` URL.
        url: String,
        /// Headers sent with every request, such as `Authorization`, kept in the daemon's
        /// config file as given.
        #[serde(default)]
        headers: HashMap<String, String>,
    },
    /// A server at a URL, over MCP's older HTTP with server-sent events transport. Codex
    /// cannot reach one, so its sessions go without it.
    Sse {
        /// Its `http` or `https` URL.
        url: String,
        /// Headers sent with every request, kept in the daemon's config file as given.
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_names_are_tool_name_safe_and_never_herder() {
        for name in ["github", "my_server-2", "A"] {
            assert!(is_valid_mcp_server_name(name), "{name}");
        }
        let long = "x".repeat(65);
        for name in ["", "herder", "a.b", "a b", "ä", long.as_str()] {
            assert!(!is_valid_mcp_server_name(name), "{name}");
        }
    }
}
