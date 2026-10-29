//! `fake_daemon --demo`: a small fleet doing what herder does today, with invented content, for
//! the screenshots on herder.sh (the `screenshots` workflow).
//!
//! Three machines, `devbox`, `studio` and `laptop`, each with a clone of `acme/api`, `acme/web`
//! and `acme/infra` (one bare origin per repository, which git reaches as GitHub), and Claude
//! and Codex accounts on a scripted adapter: each prompt plays the script in `fixtures/demo` that
//! [`SCRIPTS`] names for its account, else a short reply. Once they run, the demo sets up:
//!
//! - on `devbox`, a session whose Claude subagents work in parallel, a Codex session waiting on
//!   an approval, a session that hit the Work account's limit and failed over to Personal, a
//!   primary with three children it spawned over herder's MCP server, and a finished session;
//! - a session that started on `devbox` and was handed off to `studio`, onto its Team account,
//!   where it finished the work;
//! - on `laptop`, a finished session and a Claude session waiting on a multi-select question.
//!
//! On `devbox`, its bots' people have started conversations from Slack ([`BOT_SESSIONS`]): a
//! DM, a mention in a channel, a chat, and one waiting on an approval.
//!
//! The ledger session publishes a wide HTML mockup during its turn ([`MOCKUP`]) with a public
//! link from a krowk of the demo's own ([`DemoKrowk`]).
//!
//! The subagents' session and the primary have pull requests ([`PRS`]), which a GitHub of the
//! demo's own serves ([`DemoGitHub`]).
//!
//! Each machine also has fake provider CLIs ([`CLIS`]): their versions differ between machines,
//! `devbox` and `studio` can install and update them, and adding an account runs a login that
//! waits for Enter, then reports the account logged in. Claude Code's vendor publishes a newer
//! release than any machine has ([`LATEST`]), and the machines have MCP servers of the user's
//! ([`MCP_SERVERS`]): stdio and HTTP ones, a disabled one and one for a single project. Their
//! skill library is one repository ([`SKILLS`]), with one skill turned off on `studio`.
//!
//! The user owns `devbox` and `studio` and is a member on `laptop`, which someone else owns.
//! `devbox` backs up to a vault, `vault`, which the user owns too.
//!
//! [`Demo::start`] returns once all of it is in place, with the link a device paired with every
//! machine shares, as `--share` prints.
//!
//! `fake_daemon --busy` adds the load the apps are profiled under (`apple/scripts/profile-mac.sh`):
//! on `devbox`, a session with [`BUSY_TURNS`] finished turns, about 2,000 items, whose next turn
//! keeps streaming.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use herder_adapters::{
    Adapter, AdapterCommand, AdapterEvent, AdapterSession, Capabilities, StartFuture, StartRequest,
};
use herder_client_core::{Client, ConnectionState, PairResult, PairingUri};
use herder_daemon::auth::{Auth, PAIRING_TTL, User};
use herder_daemon::bots::{Bots, Runtime as BotsRuntime, runner};
use herder_daemon::config::Retention;
use herder_daemon::login::{self, Logins};
use herder_daemon::mcp;
use herder_daemon::projects::{
    Discovery, OnSessionsChanged, Overrides, ProjectEntry, ProjectsConfig,
};
use herder_daemon::providers::{InstallRecipe, Providers};
use herder_daemon::prs::{self, Fetched, GetFuture, GitHub, GraphqlFuture};
use herder_daemon::resources::{Admission, ReadHost, Reading, ResourcesConfig};
use herder_daemon::session::publish::{self, Publisher, UploadFuture};
use herder_daemon::session::{AccountConfig, Accounts, Adapters, EventSink, SessionManager, Setup};
use herder_daemon::settings::Settings;
use herder_daemon::skills::{Skills, SkillsSink};
use herder_daemon::terminal::Terminals;
use herder_daemon::vault::{
    self, HostSecrets, Link, LinkSetup, SecretsKey, VaultStore, WakeOnEvent,
};
use herder_daemon::worktree::{Worktrees, checkpoint};
use herder_daemon::ws::{Host, Server, Tls};
use herder_daemon::{Config, Hub};
use herder_protocol::{
    AccountId, BotId, BotKind, BotPlace, Bytes, CommandBody, CommandResult, EventBody, HostId,
    Item, ItemBody, ItemId, PermissionMode, Provider, Role, SessionId, SessionOrigin,
    SessionStatus, Timestamp, TurnId, UserId,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// How long any step of setting the demo up may take.
const TIMEOUT: Duration = Duration::from_secs(60);

const AGENTS: &str =
    "Audit how the webhook handlers retry failed deliveries and fix anything unsafe.";
const LEDGER: &str =
    "Move invoices onto the new ledger tables, with a migration that is safe to rerun.";
/// The page the ledger session publishes: a mockup laid out 1440 px wide.
const MOCKUP: &str = include_str!("../../fixtures/demo/invoices-mockup.html");
const DATES: &str = "Upgrade date-fns to v4 and fix whatever breaks.";
const BROWSERS: &str = "Run the checkout e2e suite in more browsers.";
const RETENTION: &str =
    "Roll out the 30-day log retention policy: Terraform, the Loki Helm values and the runbook.";
const TERRAFORM: &str = "Set the log bucket lifecycle to 30 days in terraform/modules/logs and \
                         plan it against production. Report the plan's summary.";
const HELM: &str = "Lower Loki's retention_period to 30 days in every values file under \
                    charts/loki and check the rendered manifests.";
const RUNBOOK: &str = "Write docs/runbooks/logs.md: how to restore logs older than 30 days from \
                       the cold storage archive.";
const FLAKY: &str = "The checkout e2e test fails about one CI run in five. Find out why.";
const ROLLBACK: &str = "Staging is throwing 500s since the last deploy, roll it back.";
const SAFARI: &str = "Carry on here: fix it and run the checkout suite in WebKit until it passes \
                      20 times in a row.";

/// The busy session's turns: each [`NEXT_STEP`] plays [`BUSY_ROUNDS`] rounds of work and
/// finishes, then [`KEEP_GOING`] plays a round every second for an hour.
const BUSY_TURNS: usize = 100;
const BUSY_ROUNDS: usize = 7;
const NEXT_STEP: &str = "Next step.";
const KEEP_GOING: &str = "Keep going.";

/// The script each prompt plays, by account; other prompts get a short reply.
/// The pull requests [`DemoGitHub`] serves: number, title, head branch and state (`open`,
/// `draft`, `merged` or `closed`).
const PRS: &[(u64, &str, &str, &str)] = &[
    (
        208,
        "Cap webhook retries and persist before acking",
        "webhook-retries",
        "open",
    ),
    (
        212,
        "Log bucket: 30-day lifecycle",
        "log-retention-terraform",
        "merged",
    ),
    (
        214,
        "Loki: retention_period to 30 days",
        "log-retention-helm",
        "open",
    ),
    (
        215,
        "Runbook: restoring logs from cold storage",
        "log-retention-runbook",
        "open",
    ),
    (
        216,
        "Log retention: staging first",
        "log-retention",
        "draft",
    ),
];

const SCRIPTS: &[(&str, &str, &str)] = &[
    ("claude-personal", AGENTS, "agents.jsonl"),
    ("claude-work", LEDGER, "limit.jsonl"),
    ("claude-personal", LEDGER, "failover.jsonl"),
    ("codex", DATES, "approval.jsonl"),
    ("claude-personal", BROWSERS, "question.jsonl"),
    ("claude-personal", RETENTION, "task.jsonl"),
    ("claude-personal", TERRAFORM, "child-terraform.jsonl"),
    ("claude-personal", HELM, "child-helm.jsonl"),
    ("claude-personal", RUNBOOK, "child-runbook.jsonl"),
    ("claude-personal", FLAKY, "handoff-devbox.jsonl"),
    ("claude-team", SAFARI, "handoff-studio.jsonl"),
    ("codex", ROLLBACK, "bot-approval.jsonl"),
    ("codex", FINISHED[0].4, "done.jsonl"),
    ("claude-personal", FINISHED[1].4, "done.jsonl"),
    ("codex", FINISHED[2].4, "done.jsonl"),
];

/// The finished sessions that fill out the lists: machine, account, project, title, prompt.
const FINISHED: &[(&str, &str, &str, &str, &str)] = &[
    (
        "devbox",
        "codex",
        "api",
        "Rate-limit the public API per key",
        "Add per-key rate limits to the public API.",
    ),
    (
        "laptop",
        "claude-personal",
        "web",
        "Tidy the README badges",
        "Replace the broken README badges.",
    ),
    (
        "studio",
        "codex",
        "infra",
        "Pin the Terraform providers",
        "Pin every Terraform provider to a minor version.",
    ),
];

/// The conversations people started from Slack through `devbox`'s bots: bot, who wrote, the
/// channel they wrote in (`None` for a DM), the repository (`None` for a chat), account, title
/// and prompt.
type BotSession = (
    &'static str,
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    &'static str,
    &'static str,
    &'static str,
);
const BOT_SESSIONS: &[BotSession] = &[
    (
        "trash-can",
        "Sam",
        None,
        Some("api"),
        "claude-personal",
        "Why invoice exports time out",
        "Why do invoice exports time out for our biggest accounts?",
    ),
    (
        "trash-can",
        "Kim",
        Some("web"),
        Some("web"),
        "claude-personal",
        "Pricing page shows USD in the EU",
        "The pricing page shows USD to visitors in the EU, can you check why?",
    ),
    (
        "trash-can",
        "Alex",
        None,
        None,
        "claude-personal",
        "Partial refunds in the refund policy",
        "What does our refund policy say about partial refunds?",
    ),
    (
        "acme-ops",
        "Sam",
        None,
        Some("infra"),
        "codex",
        "Roll back the staging deploy",
        ROLLBACK,
    ),
];

/// The skills of the machines' library: name and description.
const SKILLS: &[(&str, &str)] = &[
    (
        "release-notes",
        "Draft release notes from the pull requests merged since the last tag.",
    ),
    (
        "review-pr",
        "Review a pull request for correctness, tests and naming.",
    ),
    (
        "migrate-db",
        "Write a reversible database migration and run it both ways.",
    ),
    (
        "triage-ci",
        "Find why a CI run failed and propose the smallest fix.",
    ),
];

/// The skill turned off on `studio`, by its owner.
const STUDIO_OFF: &str = "migrate-db";

/// The repositories every machine has a clone of, as `acme/<name>`.
const REPOS: &[&str] = &["api", "web", "infra"];

/// An account: its id, provider and label.
type Account = (&'static str, &'static str, &'static str);

/// A provider CLI on a machine: the provider and its version, `None` where it is not installed.
type Cli = (&'static str, Option<&'static str>);

/// Each machine's provider CLIs, and whether herder can install and update them there.
const CLIS: &[(&str, &[Cli], bool)] = &[
    (
        "devbox",
        &[
            ("claude", Some("2.1.14 (Claude Code)")),
            ("codex", Some("codex-cli 0.46.0")),
            ("cursor", Some("2025.10.02-7a1b2c")),
        ],
        true,
    ),
    (
        "studio",
        &[
            ("claude", Some("2.1.9 (Claude Code)")),
            ("codex", Some("codex-cli 0.44.0")),
            ("cursor", None),
        ],
        true,
    ),
    (
        "laptop",
        &[
            ("claude", Some("2.1.14 (Claude Code)")),
            ("codex", Some("codex-cli 0.41.0")),
        ],
        false,
    ),
];

/// The newest release each provider's vendor publishes, newer than any machine has; updating
/// Claude Code installs it.
const LATEST: &[(&str, &str)] = &[("claude", "2.1.20")];

/// Each machine's MCP servers, as its `daemon.toml` has them; `laptop` has none yet.
const MCP_SERVERS: &[(&str, &str)] = &[
    (
        "devbox",
        r#"
[[mcp_servers]]
name = "github"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_HOST = "github.com" }
secrets = ["GITHUB_PERSONAL_ACCESS_TOKEN"]

[[mcp_servers]]
name = "linear"
url = "https://mcp.linear.app/mcp"
headers = { Authorization = "Bearer lin_demo_0000" }

[[mcp_servers]]
name = "postgres"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres", "postgresql://localhost/acme_dev"]
project = "github.com/acme/api"

[[mcp_servers]]
name = "sentry"
url = "https://mcp.sentry.dev/sse"
transport = "sse"
enabled = false
"#,
    ),
    (
        "studio",
        r#"
[[mcp_servers]]
name = "linear"
url = "https://mcp.linear.app/mcp"
headers = { Authorization = "Bearer lin_demo_0000" }
"#,
    ),
];

/// Each machine's chat bots, as its `daemon.toml` has them. Their tokens are in no vault, so
/// they wait for one and never reach Slack.
const BOTS: &[(&str, &str)] = &[(
    "devbox",
    r#"
[[bots]]
bot_id = "trash-can"
kind = "slack"
name = "trash-can"
token = "bot-trash-can/BOT_TOKEN"
app_token = "bot-trash-can/APP_TOKEN"

[[bots.allow]]
external_user_id = "U04SAM0001"
display_name = "Sam"
dm = true
channels = true
approve = true
notify = true

[[bots.allow]]
external_user_id = "U04ALEX002"
display_name = "Alex"
dm = true
channels = true

[[bots.allow]]
external_user_id = "U04KIM0003"
display_name = "Kim"
channels = true

[bots.defaults]
project_id = "github.com/acme/api"
provider = "claude"
model = "sonnet"
permission_mode = "auto_edit"

[[bots.channel_bindings]]
channel_id = "C07WEB0001"
project_id = "github.com/acme/web"

[[bots.channel_bindings]]
channel_id = "C07OPS0002"
project_id = "github.com/acme/infra"

[[bots]]
bot_id = "acme-ops"
kind = "slack"
name = "acme-ops"
token = "bot-acme-ops/BOT_TOKEN"
app_token = "bot-acme-ops/APP_TOKEN"

[[bots.allow]]
external_user_id = "U04SAM0001"
display_name = "Sam"
dm = true
approve = true
"#,
)];

/// The machines: name, cores, memory in GiB, accounts.
const MACHINES: &[(&str, u32, u64, &[Account])] = &[
    (
        "devbox",
        32,
        128,
        &[
            ("claude-work", "claude", "Work"),
            ("claude-personal", "claude", "Personal"),
            ("codex", "codex", "Codex"),
        ],
    ),
    (
        "studio",
        24,
        96,
        &[
            ("claude-team", "claude", "Team"),
            ("codex", "codex", "Codex"),
        ],
    ),
    (
        "laptop",
        12,
        36,
        &[("claude-personal", "claude", "Personal")],
    ),
];

/// The machine the demo's user is a member on: another user owns it.
const MEMBER_ON: &str = "laptop";

/// The machine that backs up to the demo's vault.
const BACKS_UP: &str = "devbox";

/// The demo's vault.
const VAULT: &str = "vault";

/// The running demo; [`Demo::stop`] stops it and removes everything it created.
pub struct Demo {
    /// The link a device paired with every machine shares.
    pub link: String,
    daemons: Vec<Daemon>,
    vault: DemoVault,
    _tmp: TempDir,
}

impl Demo {
    /// Starts the machines and sets the demo up on them; with `busy`, the busy session too.
    pub async fn start(busy: bool) -> Result<Self> {
        let tmp = tempfile::tempdir()?;
        let origins = origins(&tmp.path().join("origins"))?;
        let mut daemons = Vec::new();
        for &(name, cores, memory, accounts) in MACHINES {
            let dir = tmp.path().join(name);
            daemons.push(Daemon::start(&dir, name, cores, memory, accounts, &origins).await?);
        }
        let profile = tmp.path().join("profile");
        std::fs::create_dir(&profile)?;
        let client = Client::open(profile.display().to_string(), "demo".into())?;
        for daemon in &daemons {
            for result in client.pair(daemon.link.clone()).await? {
                if let PairResult::Failed { error, .. } = result {
                    bail!("the demo device did not pair: {error}");
                }
            }
        }
        let changes = client.changes();
        tokio::time::timeout(TIMEOUT, async {
            while !client
                .machines()
                .iter()
                .all(|machine| machine.connection == ConnectionState::Connected)
            {
                if !changes.next().await {
                    break;
                }
            }
        })
        .await
        .context("the demo device did not connect")?;
        let demo = Scenario {
            client: &client,
            daemons: &daemons,
        };
        demo.set_up().await?;
        if busy {
            demo.busy().await?;
        }
        let vault = DemoVault::start(&tmp.path().join(VAULT)).await?;
        demo.back_up(&vault).await?;
        let shared = client.share().await?;
        if let Some(skipped) = shared.skipped.first() {
            bail!("{} was not shared: {}", skipped.host_id, skipped.error);
        }
        Ok(Self {
            link: shared.link.to_string(),
            daemons,
            vault,
            _tmp: tmp,
        })
    }

    /// Stops every machine and the vault, and waits until they have.
    pub async fn stop(self) -> Result<()> {
        for daemon in self.daemons {
            daemon.shutdown.cancel();
            daemon.server.await?;
        }
        self.vault.shutdown.cancel();
        Ok(self.vault.server.await?)
    }
}

/// A session of the demo, as its machine's journal has it.
#[cfg(test)]
pub struct Scene {
    pub host: String,
    pub title: String,
    pub origin: Option<SessionOrigin>,
    pub status: SessionStatus,
    pub events: Vec<EventBody>,
}

#[cfg(test)]
impl Demo {
    /// Every session of every machine.
    pub async fn scenes(&self) -> Result<Vec<Scene>> {
        let mut scenes = Vec::new();
        for daemon in &self.daemons {
            for head in daemon.sessions.sessions().await? {
                let events: Vec<EventBody> = daemon
                    .sessions
                    .read_since(&head.session_id, 0, usize::MAX)
                    .await?
                    .into_iter()
                    .map(|event| event.body)
                    .collect();
                let mut title = String::new();
                let mut status = SessionStatus::Idle;
                for body in &events {
                    match body {
                        EventBody::TitleChanged { title: new, .. } => title.clone_from(new),
                        EventBody::SessionStatusChanged { status: new, .. } => status = *new,
                        _ => {}
                    }
                }
                scenes.push(Scene {
                    host: daemon.id.to_string(),
                    title,
                    origin: head.origin,
                    status,
                    events,
                });
            }
        }
        Ok(scenes)
    }
}

/// Sets the demo's sessions up through a paired client, as a user would.
struct Scenario<'a> {
    client: &'a Client,
    daemons: &'a [Daemon],
}

impl Scenario<'_> {
    /// Pairs the demo's device with `vault` as its owner and links [`BACKS_UP`] to it, as the
    /// apps' Back Up does, then waits until the vault holds what the machine has.
    async fn back_up(&self, vault: &DemoVault) -> Result<()> {
        for result in self.client.pair(vault.link.clone()).await? {
            if let PairResult::Failed { error, .. } = result {
                bail!("the demo device did not pair with the vault: {error}");
            }
        }
        let vault_id = HostId::new(VAULT);
        let CommandResult::HostPairing { code, .. } = self
            .client
            .send(
                vault_id.clone(),
                CommandBody::PairVaultHost {
                    host_name: BACKS_UP.into(),
                },
            )
            .await?
        else {
            bail!("the vault did not mint a host code");
        };
        self.client
            .send(
                HostId::new(BACKS_UP),
                CommandBody::LinkVault {
                    addresses: vec![vault.address.clone()],
                    fingerprint: vault.fingerprint.clone(),
                    pairing_code: code,
                    backup: true,
                    secrets: true,
                },
            )
            .await?;
        let changes = self.client.changes();
        tokio::time::timeout(TIMEOUT, async {
            while !self.client.machines().iter().any(|machine| {
                machine.host_id == vault_id
                    && machine.vault.as_ref().is_some_and(|status| {
                        status.hosts.iter().any(|host| {
                            host.host_id == HostId::new(BACKS_UP) && host.last_event_at.is_some()
                        })
                    })
            }) {
                if !changes.next().await {
                    break;
                }
            }
        })
        .await
        .context("the vault did not catch up with the machine")
    }

    async fn set_up(&self) -> Result<()> {
        // A turn that fails over between accounts, and the turn before a handoff, first: both
        // finish, and the handoff forks once its turn is checkpointed.
        let ledger = self
            .session(
                "devbox",
                "claude-work",
                "api",
                "ledger-invoices",
                "Move invoices onto the ledger",
            )
            .await?;
        self.prompt("devbox", &ledger, LEDGER).await?;
        // Published during its turn, before any other session exists: its last activity stays
        // the turn's end, so the Board orders every task as it would without it. Its MCP server
        // answers once the turn has started.
        let devbox = self.daemon("devbox")?;
        devbox
            .until(&ledger, |events| {
                events
                    .iter()
                    .any(|body| matches!(body, EventBody::TurnStarted { .. }))
            })
            .await?;
        devbox
            .tool(
                &ledger,
                "publish",
                json!({ "title": "Invoices on the ledger — list (mockup)", "html": MOCKUP }),
            )
            .await?;
        let flaky = self
            .session(
                "devbox",
                "claude-personal",
                "web",
                "fix-flaky-checkout",
                "Fix the flaky checkout e2e test",
            )
            .await?;
        self.prompt("devbox", &flaky, FLAKY).await?;
        for (name, account, repo, title, prompt) in FINISHED {
            let branch = title.to_lowercase().replace(' ', "-");
            let session = self.session(name, account, repo, &branch, title).await?;
            self.prompt(name, &session, prompt).await?;
        }
        let mut bot_sessions = Vec::new();
        for bot_session in BOT_SESSIONS {
            bot_sessions.push(self.bot_session(bot_session).await?);
        }

        let agents = self
            .session(
                "devbox",
                "claude-personal",
                "api",
                "webhook-retries",
                "Audit webhook retries",
            )
            .await?;
        self.prompt("devbox", &agents, AGENTS).await?;
        let dates = self
            .session(
                "devbox",
                "codex",
                "web",
                "date-fns-v4",
                "Upgrade date-fns to v4",
            )
            .await?;
        self.prompt("devbox", &dates, DATES).await?;
        let browsers = self
            .session(
                "laptop",
                "claude-personal",
                "web",
                "checkout-browsers",
                "Run checkout e2e in more browsers",
            )
            .await?;
        self.prompt("laptop", &browsers, BROWSERS).await?;
        self.client
            .send(
                HostId::new("studio"),
                CommandBody::SetSkillEnabled {
                    name: STUDIO_OFF.into(),
                    enabled: false,
                },
            )
            .await?;

        let retention = self
            .session(
                "devbox",
                "claude-personal",
                "infra",
                "log-retention",
                "Roll out 30-day log retention",
            )
            .await?;
        self.prompt("devbox", &retention, RETENTION).await?;
        for (session_id, number) in [
            (&agents, 208),
            (&retention, 212),
            (&retention, 214),
            (&retention, 215),
            (&retention, 216),
        ] {
            let linked = self
                .client
                .send(
                    HostId::new("devbox"),
                    CommandBody::LinkPr {
                        session_id: session_id.clone(),
                        number,
                    },
                )
                .await?;
            ensure!(
                matches!(linked, CommandResult::Applied),
                "devbox did not link #{number}: {linked:?}"
            );
        }
        let devbox = self.daemon("devbox")?;
        devbox
            .until(&retention, |events| {
                events
                    .iter()
                    .any(|body| matches!(body, EventBody::TurnStarted { .. }))
            })
            .await?;
        for (task, prompt) in [
            ("Terraform: 30-day lifecycle on the log bucket", TERRAFORM),
            ("Helm: Loki retention to 30 days", HELM),
            ("Runbook: restoring logs from cold storage", RUNBOOK),
        ] {
            devbox.spawn(&retention, task, prompt).await?;
        }

        devbox.until(&ledger, finished(1)).await?;
        devbox.until(&flaky, finished(1)).await?;
        let forked = self
            .client
            .fork_session(
                HostId::new("devbox"),
                flaky.clone(),
                HostId::new("studio"),
                Some(AccountId::new("claude-team")),
            )
            .await?;
        let CommandResult::SessionForked { session_id, .. } = forked else {
            bail!("studio did not fork the session: {forked:?}");
        };
        // The work goes on on studio, so the original is put away.
        self.client
            .send(
                HostId::new("devbox"),
                CommandBody::ArchiveSession { session_id: flaky },
            )
            .await?;
        self.prompt("studio", &session_id, SAFARI).await?;
        // Its journal starts with the original's finished turn.
        self.daemon("studio")?
            .until(&session_id, finished(2))
            .await?;
        // The children report to the primary before the device looks.
        devbox
            .until(&retention, |events| {
                events
                    .iter()
                    .any(|body| matches!(body, EventBody::ChildReported { .. }))
            })
            .await?;
        let rollback = bot_sessions.pop().context("no bot sessions")?;
        for session in &bot_sessions {
            devbox.until(session, finished(1)).await?;
        }
        devbox
            .until(&rollback, |events| {
                events
                    .iter()
                    .any(|body| matches!(body, EventBody::ApprovalRequested { .. }))
            })
            .await?;
        self.daemon("laptop")?
            .until(&browsers, |events| {
                events
                    .iter()
                    .any(|body| matches!(body, EventBody::QuestionAsked { .. }))
            })
            .await?;
        Ok(())
    }

    /// A session with a long transcript whose last turn keeps streaming.
    async fn busy(&self) -> Result<()> {
        let session = self
            .session(
                "devbox",
                "claude-personal",
                "api",
                "streaming-load",
                "A long transcript, still streaming",
            )
            .await?;
        let devbox = self.daemon("devbox")?;
        // One at a time: prompts sent while a turn runs queue up and merge.
        for turns in 1..=BUSY_TURNS {
            self.prompt("devbox", &session, NEXT_STEP).await?;
            devbox.until(&session, finished(turns)).await?;
        }
        self.prompt("devbox", &session, KEEP_GOING).await
    }

    fn daemon(&self, name: &str) -> Result<&Daemon> {
        self.daemons
            .iter()
            .find(|daemon| daemon.id.as_str() == name)
            .with_context(|| format!("no machine {name}"))
    }

    /// Creates a session on `account` of machine `host`, in its clone of `acme/<repo>`, on a new
    /// `branch`, titled `title`.
    async fn session(
        &self,
        host: &str,
        account: &str,
        repo: &str,
        branch: &str,
        title: &str,
    ) -> Result<SessionId> {
        let daemon = self.daemon(host)?;
        let created = self
            .client
            .send(
                daemon.id.clone(),
                CommandBody::CreateSession {
                    repo: Some(daemon.repos[repo].clone()),
                    project_id: None,
                    branch: Some(branch.into()),
                    account_id: Some(AccountId::new(account)),
                    provider: None,
                    model: None,
                    permission_mode: Some(PermissionMode::AutoEdit),
                    failover_pin: None,
                    chat: false,
                },
            )
            .await?;
        let CommandResult::SessionCreated { session_id } = created else {
            bail!("{host} did not create a session: {created:?}");
        };
        self.client
            .send(
                daemon.id.clone(),
                CommandBody::RenameSession {
                    session_id: session_id.clone(),
                    title: title.into(),
                },
            )
            .await?;
        Ok(session_id)
    }

    /// Starts a conversation on `devbox` as its bot does for a message in Slack, titles it and
    /// sends it its prompt.
    async fn bot_session(
        &self,
        &(bot, by, channel, repo, account, title, prompt): &BotSession,
    ) -> Result<SessionId> {
        let daemon = self.daemon("devbox")?;
        let origin = SessionOrigin {
            bot_id: BotId::new(bot),
            kind: BotKind::Slack,
            by: by.into(),
            place: channel.map_or(BotPlace::Direct, |name| BotPlace::Channel {
                name: name.into(),
            }),
        };
        let user = UserId::new(format!("via {bot}, {by}"));
        let command = CommandBody::CreateSession {
            repo: repo.map(|repo| daemon.repos[repo].clone()),
            project_id: None,
            branch: None,
            account_id: Some(AccountId::new(account)),
            provider: None,
            model: None,
            permission_mode: Some(PermissionMode::AutoEdit),
            failover_pin: None,
            chat: repo.is_none(),
        };
        let fail = |err: herder_protocol::ErrorInfo| anyhow::anyhow!("devbox: {}", err.message);
        let created = daemon
            .sessions
            .handle_from(user.clone(), command, Some(origin))
            .await
            .map_err(fail)?;
        let CommandResult::SessionCreated { session_id } = created else {
            bail!("devbox did not create a session: {created:?}");
        };
        for command in [
            CommandBody::RenameSession {
                session_id: session_id.clone(),
                title: title.into(),
            },
            CommandBody::SendPrompt {
                session_id: session_id.clone(),
                text: prompt.into(),
                images: Vec::new(),
                files: Vec::new(),
            },
        ] {
            daemon
                .sessions
                .handle(user.clone(), command)
                .await
                .map_err(fail)?;
        }
        Ok(session_id)
    }

    async fn prompt(&self, host: &str, session_id: &SessionId, text: &str) -> Result<()> {
        self.client
            .send(
                HostId::new(host),
                CommandBody::SendPrompt {
                    session_id: session_id.clone(),
                    text: text.into(),
                    images: Vec::new(),
                    files: Vec::new(),
                },
            )
            .await?;
        Ok(())
    }
}

/// Whether a session is idle after `turns` finished turns.
fn finished(turns: usize) -> impl Fn(&[EventBody]) -> bool {
    move |events| {
        let completed = events
            .iter()
            .filter(|body| matches!(body, EventBody::TurnCompleted { .. }))
            .count();
        let status = events.iter().rev().find_map(|body| match body {
            EventBody::SessionStatusChanged { status, .. } => Some(*status),
            _ => None,
        });
        completed >= turns && status == Some(SessionStatus::Idle)
    }
}

/// The demo's vault, which [`BACKS_UP`] backs up to.
struct DemoVault {
    /// A pairing link for its owner.
    link: String,
    address: String,
    fingerprint: String,
    shutdown: CancellationToken,
    server: JoinHandle<()>,
}

impl DemoVault {
    async fn start(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir.join("tls"))?;
        let tls = Tls::load_or_create(&dir.join("tls"), VAULT)?;
        let auth = Arc::new(Auth::open(dir)?);
        let store = VaultStore::open(dir.join("vault.db"))?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?.to_string();
        let fingerprint = tls.fingerprint().to_owned();
        let link = PairingUri {
            hosts: vec![address.clone()],
            fingerprint: fingerprint.clone(),
            code: auth.mint("sam", None, PAIRING_TTL)?.code,
        }
        .to_string();
        let host = Host {
            id: HostId::new(VAULT),
            name: VAULT.into(),
        };
        let server = vault::Server::new(
            tls,
            auth,
            store,
            host,
            vault::LIVENESS_TIMEOUT,
            Retention::default(),
            Some(SecretsKey::new(&[7; 32])),
        );
        let shutdown = CancellationToken::new();
        let server = tokio::spawn(server.run(vec![listener], shutdown.clone()));
        Ok(Self {
            link,
            address,
            fingerprint,
            shutdown,
            server,
        })
    }
}

/// Writes the users file a daemon opens with `pat` as its owner, as though they had paired,
/// so the demo's user can be paired as a member. The file's shape is the daemon's own
/// (`herder_daemon::auth`).
fn owned_by_another(dir: &Path) -> Result<()> {
    let users = json!({
        "version": 1,
        "users": [User {
            user_id: UserId::new("pat"),
            name: "pat".into(),
            role: Role::Owner,
            created_at: Timestamp::now(),
            host: false,
        }],
        "devices": [],
    });
    std::fs::write(dir.join("auth.json"), serde_json::to_vec(&users)?)?;
    Ok(())
}

/// A running machine.
struct Daemon {
    id: HostId,
    link: String,
    /// Its clone of each repository, by name.
    repos: HashMap<&'static str, String>,
    sessions: SessionManager,
    data_dir: PathBuf,
    shutdown: CancellationToken,
    server: JoinHandle<()>,
}

impl Daemon {
    async fn start(
        dir: &Path,
        name: &str,
        cores: u32,
        memory_gib: u64,
        accounts: &[Account],
        origins: &Path,
    ) -> Result<Self> {
        let shutdown = CancellationToken::new();
        std::fs::create_dir_all(dir.join("tls"))?;
        let mut repos = HashMap::new();
        for repo in REPOS {
            repos.insert(*repo, clone(origins, repo, &dir.join("src"))?);
        }
        let tls = Tls::load_or_create(&dir.join("tls"), name)?;
        if name == MEMBER_ON {
            owned_by_another(dir)?;
        }
        let auth = Arc::new(Auth::open(dir)?);
        let hub = Arc::new(Hub::default());
        let scripted = Arc::new(Scripted {
            dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/demo"),
        });
        let mut adapters = Adapters::new();
        adapters.register(Provider::Claude, scripted.clone());
        adapters.register(Provider::Codex, scripted);
        let mut configs = Accounts::new();
        for (account, provider, label) in accounts {
            configs.insert(
                AccountId::new(*account),
                AccountConfig {
                    provider: match *provider {
                        "codex" => Provider::Codex,
                        _ => Provider::Claude,
                    },
                    label: (*label).into(),
                    config_dir: Some(dir.join("accounts").join(account)),
                    fallback: false,
                },
            );
        }
        let turns = AtomicU64::new(0);
        let sessions_changed = Arc::new(tokio::sync::Notify::new());
        // Woken as the journal grows, for the vault the machine may be linked to.
        let journal_grew = Arc::new(tokio::sync::Notify::new());
        let setup = Setup {
            store: herder_store::Store::open(dir.join("herder.db"))?,
            adapters,
            accounts: configs,
            sink: Arc::new(WakeOnEvent {
                next: Arc::new(OnSessionsChanged {
                    next: Arc::clone(&hub) as Arc<dyn EventSink>,
                    notify: Arc::clone(&sessions_changed),
                }),
                notify: Arc::clone(&journal_grew),
            }),
            turn_ids: Box::new(move || {
                TurnId::new(format!("turn-{}", turns.fetch_add(1, Ordering::SeqCst) + 1))
            }),
            worktrees: Worktrees::new(dir.join("worktrees")),
            attachments: dir.join("attachments"),
            chats: dir.join("chats"),
        };
        let sessions = SessionManager::open(setup, shutdown.clone()).await?;
        sessions.checkpoint_turns(checkpoint::Config {
            dir: dir.join("checkpoints"),
            keep: checkpoint::KEEP,
            push_timeout: TIMEOUT,
        })?;
        // The MCP socket's path must stay short, so it goes in the machine's own directory.
        sessions.serve_mcp(mcp::Config {
            data_dir: dir.to_owned(),
            herder: PathBuf::from("herder"),
        })?;
        sessions.publish_with(Arc::new(DemoKrowk))?;
        sessions
            .track_prs(prs::Config {
                data_dir: dir.to_owned(),
                herder: PathBuf::from("herder"),
                github: Arc::new(DemoGitHub),
                fast: prs::FAST,
                slow: prs::SLOW,
                follow_ups: false,
            })
            .await?;
        let skills = Arc::new(Skills::open(
            dir,
            &sessions.providers(),
            Arc::clone(&hub) as Arc<dyn SkillsSink>,
        )?);
        skills
            .command(CommandBody::SetSkillsRepo {
                url: path(&origins.join("skills.git"))?,
            })
            .await
            .map_err(|err| anyhow::anyhow!("cannot set the skill library: {}", err.message))?;
        sessions.deliver_skills(skills)?;
        let resources = ResourcesConfig {
            max_turns: Some(16),
            ..ResourcesConfig::default()
        };
        let reading = Reading {
            memory_total: memory_gib << 30,
            memory_available: (memory_gib << 30) * 3 / 5,
            load_1m: f64::from(cores) / 5.0,
            cpu_percent: 18.0,
            pressure: None,
        };
        let admission = Arc::new(Admission::new(
            resources.budget(cores),
            Box::new(Steady(reading)),
        ));
        sessions.admit_turns(Arc::clone(&admission))?;
        let config_file = dir.join("daemon.toml");
        let mcp_servers = MCP_SERVERS
            .iter()
            .find(|(machine, _)| *machine == name)
            .map_or("", |(_, servers)| servers);
        let bots = BOTS
            .iter()
            .find(|(machine, _)| *machine == name)
            .map_or("", |(_, bots)| bots);
        std::fs::write(
            &config_file,
            format!("[resources]\nmax_turns = 16\n{mcp_servers}{bots}"),
        )?;
        let config = Config::load_file(&config_file)?;
        let settings = Arc::new(Settings::new(
            &config,
            Some(Arc::clone(&admission)),
            shutdown.clone(),
        ));
        sessions.use_mcp_servers(settings.mcp_servers())?;
        tokio::spawn({
            let (hub, shutdown) = (Arc::clone(&hub), shutdown.clone());
            async move { admission.run(&hub, shutdown).await }
        });
        let host = Host {
            id: HostId::new(name),
            name: name.into(),
        };
        tokio::spawn(
            Discovery {
                host: host.id.clone(),
                config: Arc::new(Overrides::new(
                    config_file.clone(),
                    dir.join("project-icons"),
                    // Every machine has every project, sessions or not.
                    ProjectsConfig {
                        dir: dir.join("src"),
                        entries: REPOS
                            .iter()
                            .map(|repo| ProjectEntry {
                                paths: vec![dir.join("src").join(repo)],
                                ..ProjectEntry::default()
                            })
                            .collect(),
                        ..ProjectsConfig::default()
                    },
                )),
                hub: Arc::clone(&hub),
                sessions: sessions.clone(),
                sessions_changed,
                data_dir: dir.join("data"),
            }
            .run(shutdown.clone()),
        );
        let secrets = Arc::new(HostSecrets::default());
        sessions.use_secrets(Arc::clone(&secrets))?;
        let vault_link = Arc::new(Link::start(
            LinkSetup {
                config_file: config_file.clone(),
                host: host.clone(),
                sessions: sessions.clone(),
                changed: journal_grew,
                data_dir: dir.to_owned(),
                shutdown: shutdown.clone(),
                secrets: Arc::clone(&secrets),
                local: None,
            },
            None,
        )?);
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let role = (name == MEMBER_ON).then_some(Role::Member);
        let link = PairingUri {
            hosts: vec![listener.local_addr()?.to_string()],
            fingerprint: tls.fingerprint().to_owned(),
            code: auth.mint("sam", role, PAIRING_TTL)?.code,
        };
        let terminals = Terminals::new(Arc::clone(&hub), PathBuf::from("/bin/sh"));
        let (binaries, recipes) = clis(dir, name)?;
        let logins = Logins::new(
            login::programs(&binaries, &Default::default()),
            config_file,
            sessions.clone(),
        );
        let mut providers = Providers::new(binaries, Default::default());
        for (provider, recipe) in recipes {
            providers = providers.with_recipe(provider, recipe);
        }
        for (provider, version) in LATEST {
            providers = providers.with_latest(Provider::from((*provider).to_owned()), version);
        }
        providers.publish_to(Arc::clone(&hub));
        let server = Server::new(
            tls,
            auth,
            hub,
            sessions.clone(),
            terminals,
            logins,
            host.clone(),
        );
        server.manage_settings(settings)?;
        server.link_vault(vault_link)?;
        server.manage_providers(providers)?;
        let bots = Arc::new(Bots::new(&config));
        bots.start(
            BotsRuntime {
                host: runner::Host {
                    host_id: host.id.clone(),
                    sessions: sessions.clone(),
                    names: Arc::new(|_| None),
                },
                secrets: Arc::clone(&secrets),
                events: tokio::sync::broadcast::channel(1).0,
                shutdown: shutdown.clone(),
            },
            &config.bots,
        )?;
        server.manage_bots(bots)?;
        let server = tokio::spawn(server.run(vec![listener], shutdown.clone()));
        Ok(Self {
            id: host.id,
            link: link.to_string(),
            repos,
            sessions,
            data_dir: dir.to_owned(),
            shutdown,
            server,
        })
    }

    /// Waits until the events of `session_id` satisfy `done`.
    async fn until(
        &self,
        session_id: &SessionId,
        done: impl Fn(&[EventBody]) -> bool,
    ) -> Result<()> {
        tokio::time::timeout(TIMEOUT, async {
            loop {
                let events: Vec<EventBody> = self
                    .sessions
                    .read_since(session_id, 0, usize::MAX)
                    .await?
                    .into_iter()
                    .map(|event| event.body)
                    .collect();
                if done(&events) {
                    return Ok(());
                }
                if let Some(EventBody::SessionStatusChanged {
                    status: SessionStatus::Error,
                    ..
                }) = events
                    .iter()
                    .rev()
                    .find(|body| matches!(body, EventBody::SessionStatusChanged { .. }))
                {
                    bail!("session {session_id} on {} failed: {events:?}", self.id);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .with_context(|| format!("waiting on session {session_id} on {}", self.id))?
    }

    /// Spawns a child of `primary` through herder's MCP server, as the primary's CLI would.
    async fn spawn(&self, primary: &SessionId, task: &str, prompt: &str) -> Result<()> {
        self.tool(primary, "spawn", json!({ "task": task, "prompt": prompt }))
            .await
    }

    /// Calls herder's MCP tool `name` as `session` would.
    async fn tool(&self, session: &SessionId, name: &str, arguments: Value) -> Result<()> {
        let (mut input, shim_input) = tokio::io::duplex(1 << 16);
        let (shim_output, output) = tokio::io::duplex(1 << 16);
        let (dir, session) = (self.data_dir.clone(), session.clone());
        let shim =
            tokio::spawn(async move { mcp::shim(&dir, &session, shim_input, shim_output).await });
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        input.write_all(format!("{request}\n").as_bytes()).await?;
        let mut line = String::new();
        tokio::time::timeout(TIMEOUT, BufReader::new(output).read_line(&mut line))
            .await
            .with_context(|| format!("{name} did not answer"))??;
        drop(input);
        let response: Value = serde_json::from_str(&line)?;
        ensure!(
            response["result"]["isError"] != json!(true) && response.get("error").is_none(),
            "{name} failed: {response}"
        );
        shim.await??;
        Ok(())
    }
}

/// A host that always reads the same.
struct Steady(Reading);

impl ReadHost for Steady {
    fn read(&self) -> Result<Reading> {
        Ok(self.0.clone())
    }
}

/// A bare origin per repository in [`REPOS`], each with a commit that holds a README, and the
/// skill library's, `skills.git`, with a folder per skill in [`SKILLS`].
fn origins(dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let seed = dir.join("skills-seed");
    std::fs::create_dir(&seed)?;
    git(&seed, &["init", "--quiet", "--initial-branch=main"])?;
    for (name, description) in SKILLS {
        let folder = seed.join(name);
        std::fs::create_dir(&folder)?;
        std::fs::write(
            folder.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\n{description}\n"),
        )?;
    }
    git(&seed, &["add", "--all"])?;
    git(&seed, &["commit", "--quiet", "-m", "Add the skills"])?;
    let bare = dir.join("skills.git");
    git(
        dir,
        &["clone", "--quiet", "--bare", &path(&seed)?, &path(&bare)?],
    )?;
    for repo in REPOS {
        let seed = dir.join(format!("{repo}-seed"));
        std::fs::create_dir(&seed)?;
        git(&seed, &["init", "--quiet", "--initial-branch=main"])?;
        std::fs::write(seed.join("README.md"), format!("# acme/{repo}\n"))?;
        git(&seed, &["add", "README.md"])?;
        git(&seed, &["commit", "--quiet", "-m", "Initial commit"])?;
        let bare = dir.join(format!("{repo}.git"));
        git(
            dir,
            &["clone", "--quiet", "--bare", &path(&seed)?, &path(&bare)?],
        )?;
    }
    Ok(dir.to_owned())
}

/// Clones the origin of `repo` into `<dir>/<repo>`, with GitHub's `acme/<repo>` as its origin,
/// which git rewrites to the bare one so checkpoints reach it; returns the clone's path.
/// Writes the fake provider CLIs of machine `name` into `dir`: each one prints its version,
/// runs a login that waits for Enter and then reports its config dir logged in, and answers
/// its provider's status check. Returns them, by provider, with the recipes that install or
/// update them where the machine has any: the recipe writes the version the CLI then prints.
fn clis(dir: &Path, name: &str) -> Result<(HashMap<Provider, PathBuf>, Vec<Recipe>)> {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin)?;
    let (_, clis, recipes) = CLIS
        .iter()
        .find(|(machine, _, _)| *machine == name)
        .context("a machine without CLIs")?;
    // Writes the version and the CLI's binary, as a vendor installer would. Claude's warns of a
    // second, npm-installed copy, as its real updater does, for the apps to explain. It takes
    // long enough that a machine's second update is still queued once Activity opens.
    let install = bin.join("install.sh");
    std::fs::write(
        &install,
        "bin=$(dirname \"$0\")\n\
         echo \"Installing $1 $2...\"\n\
         if [ \"$1\" = claude ]; then\n\
         echo 'Warning: Multiple installations found'\n\
         echo '- npm-global at /opt/homebrew/bin/claude'\n\
         echo '- native at ~/.local/bin/claude (currently running)'\n\
         fi\n\
         sleep 8\n\
         printf '%s\\n' \"$2\" > \"$bin/$1.version\"\n\
         printf '#!/bin/sh\\nexec /bin/sh \"%s\" \"$@\"\\n' \"$bin/$1.sh\" > \"$bin/$1\"\n\
         chmod +x \"$bin/$1\"\n\
         echo Done.\n",
    )?;
    let mut binaries = HashMap::new();
    let mut installs = Vec::new();
    for provider in ["claude", "codex", "cursor", "grok", "opencode"] {
        let (parsed, var, latest) = match provider {
            "claude" => (
                Provider::Claude,
                "CLAUDE_CONFIG_DIR",
                "2.1.20 (Claude Code)",
            ),
            "codex" => (Provider::Codex, "CODEX_HOME", "codex-cli 0.46.0"),
            "cursor" => (Provider::Cursor, "CURSOR_CONFIG_DIR", "2025.10.02-7a1b2c"),
            "grok" => (Provider::Grok, "GROK_HOME", "0.1.0"),
            _ => (Provider::Opencode, "XDG_DATA_HOME", "0.15.2"),
        };
        let path = bin.join(provider);
        let version = bin.join(format!("{provider}.version"));
        let script = bin.join(format!("{provider}.sh"));
        // A CLI that is not installed is one whose binary does not exist.
        binaries.insert(parsed.clone(), path.clone());
        let installed = clis
            .iter()
            .find(|(cli, _)| *cli == provider)
            .and_then(|(_, v)| *v);
        if *recipes && clis.iter().any(|(cli, _)| *cli == provider) {
            installs.push((
                parsed,
                InstallRecipe {
                    program: PathBuf::from("/bin/sh"),
                    args: vec![
                        install.display().to_string(),
                        provider.into(),
                        latest.into(),
                    ],
                },
            ));
        }
        std::fs::write(&script, cli(var, &version))?;
        if let Some(installed) = installed {
            std::fs::write(&version, format!("{installed}\n"))?;
            std::fs::write(
                &path,
                format!("#!/bin/sh\nexec /bin/sh '{}' \"$@\"\n", script.display()),
            )?;
            std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
        }
    }
    Ok((binaries, installs))
}

/// A provider and the recipe that installs or updates it.
type Recipe = (Provider, InstallRecipe);

/// A fake provider CLI whose config dir is in `var` and whose version is in the file `version`.
fn cli(var: &str, version: &Path) -> String {
    format!(
        r#"dir="${{{var}:-$HOME/.herder-demo-login}}"
case "$*" in
  --version) cat '{version}' ;;
  "auth status --json"|"status --format json")
    if [ -f "$dir/.demo-login" ]; then echo '{{"loggedIn":true,"isAuthenticated":true}}'
    else echo '{{"loggedIn":false,"isAuthenticated":false}}'; fi ;;
  "login status"|"auth list") [ -f "$dir/.demo-login" ] ;;
  *)
    echo 'Opening your browser to sign in...'
    echo
    echo 'If it did not open, visit:'
    echo '  https://login.example.com/device?code=DEMO-4821'
    echo
    printf 'Press Enter once you have signed in. '
    read -r _
    mkdir -p "$dir" && touch "$dir/.demo-login"
    echo 'Signed in.' ;;
esac
"#,
        version = version.display(),
    )
}

fn clone(origins: &Path, repo: &str, dir: &Path) -> Result<String> {
    std::fs::create_dir_all(dir)?;
    let bare = path(&origins.join(format!("{repo}.git")))?;
    let clone = dir.join(repo);
    git(dir, &["clone", "--quiet", &bare, &path(&clone)?])?;
    let github = format!("git@github.com:acme/{repo}.git");
    git(&clone, &["remote", "set-url", "origin", &github])?;
    git(
        &clone,
        &["config", &format!("url.{bare}.insteadOf"), &github],
    )?;
    path(&clone)
}

fn path(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .with_context(|| format!("{} is not UTF-8", path.display()))
}

fn git(dir: &Path, args: &[&str]) -> Result<()> {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=herder",
            "-c",
            "user.email=herder@example.com",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .status()?;
    ensure!(status.success(), "git {args:?} failed");
    Ok(())
}

/// Plays the script [`SCRIPTS`] names for each prompt, with the account the session's config
/// dir names.
struct Scripted {
    dir: PathBuf,
}

impl Adapter for Scripted {
    fn accepts_images(&self) -> bool {
        true
    }

    fn start(&self, request: StartRequest) -> StartFuture {
        let account = request
            .config_dir
            .as_deref()
            .and_then(Path::file_name)
            .and_then(OsStr::to_str)
            .unwrap_or_default()
            .to_owned();
        let dir = self.dir.clone();
        Box::pin(async move {
            let (commands, received) = mpsc::unbounded_channel();
            let (events, rx) = mpsc::channel(64);
            tokio::spawn(play(dir, account, request.cwd, received, events));
            Ok(AdapterSession {
                capabilities: Capabilities {
                    native_model_switch: true,
                    native_permission_mode_switch: true,
                    reports_usage: true,
                    native_resume: false,
                },
                commands,
                events: rx,
            })
        })
    }
}

/// One step of a demo script: an event to emit, a pause, a wait for an answer or an interrupt,
/// or a file to write in the worktree.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Step {
    Emit(AdapterEvent),
    SleepMs(u64),
    Hold,
    Write { path: PathBuf, text: String },
}

/// Runs a session: each prompt plays its script, until the daemon shuts the session down.
async fn play(
    dir: PathBuf,
    account: String,
    cwd: PathBuf,
    mut commands: mpsc::UnboundedReceiver<AdapterCommand>,
    events: mpsc::Sender<AdapterEvent>,
) {
    let mut error = None;
    while let Some(command) = commands.recv().await {
        match command {
            AdapterCommand::SendPrompt { turn_id, text, .. } => {
                let steps = match script(&dir, &account, &text, &turn_id) {
                    Ok(steps) => steps,
                    Err(err) => {
                        error = Some(herder_protocol::TurnError {
                            class: herder_protocol::ErrorClass::Fatal,
                            message: format!("{err:#}"),
                        });
                        break;
                    }
                };
                if !run(steps, &turn_id, &cwd, &mut commands, &events).await {
                    break;
                }
            }
            AdapterCommand::Shutdown => break,
            _ => {}
        }
    }
    // The reader may be gone already; there is no one left to tell.
    let _ = events.send(AdapterEvent::Exited { error }).await;
}

/// Plays `steps` of turn `turn_id`; `false` once the session is to stop.
async fn run(
    steps: Vec<Step>,
    turn_id: &TurnId,
    cwd: &Path,
    commands: &mut mpsc::UnboundedReceiver<AdapterCommand>,
    events: &mpsc::Sender<AdapterEvent>,
) -> bool {
    for step in steps {
        match step {
            Step::Emit(event) => {
                if events.send(event).await.is_err() {
                    return false;
                }
            }
            Step::SleepMs(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
            Step::Write { path, text } => {
                let path = cwd.join(path);
                let written = path
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| std::fs::write(&path, text));
                if written.is_err() {
                    return false;
                }
            }
            Step::Hold => loop {
                match commands.recv().await {
                    Some(
                        AdapterCommand::AnswerApproval { .. }
                        | AdapterCommand::AnswerQuestion { .. },
                    ) => break,
                    Some(AdapterCommand::Interrupt) => {
                        let turn_id = turn_id.clone();
                        return events
                            .send(AdapterEvent::TurnInterrupted { turn_id })
                            .await
                            .is_ok();
                    }
                    Some(AdapterCommand::Shutdown) | None => return false,
                    Some(_) => {}
                }
            },
        }
    }
    true
}

/// The steps a prompt plays: its script with `$turn` replaced by the turn's id and each
/// `$in_<n>m` by the time `n` minutes from now, else a short reply.
fn script(dir: &Path, account: &str, text: &str, turn_id: &TurnId) -> Result<Vec<Step>> {
    match text {
        NEXT_STEP => return Ok(work(turn_id, BUSY_ROUNDS, 0)),
        KEEP_GOING => return Ok(work(turn_id, 3600, 1000)),
        _ => {}
    }
    let Some((_, _, file)) = SCRIPTS
        .iter()
        .find(|(scripted, prompt, _)| *scripted == account && *prompt == text)
    else {
        return Ok(reply(turn_id));
    };
    let path = dir.join(file);
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let raw = times(&raw.replace("$turn", turn_id.as_str()))?;
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| serde_json::from_str(line).with_context(|| format!("{file}: {line}")))
        .collect()
}

/// `raw` with each `$in_<n>m` replaced by the time `n` minutes from now.
fn times(raw: &str) -> Result<String> {
    let now = Timestamp::now().as_second();
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find("$in_") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 4..];
        let digits = after.find('m').context("`$in_` without `m`")?;
        let minutes: i64 = after[..digits].parse()?;
        out.push_str(&Timestamp::from_second(now + minutes * 60)?.to_string());
        rest = &after[digits + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// A turn that answers in a line and ends.
fn reply(turn_id: &TurnId) -> Vec<Step> {
    let item = Item {
        agent_message: None,
        follow_up: None,
        parent_call_id: None,
        id: ItemId::new(format!("{turn_id}-reply")),
        turn_id: turn_id.clone(),
        body: ItemBody::AssistantMessage {
            text: "Done.".into(),
        },
    };
    vec![
        Step::Emit(AdapterEvent::TurnStarted {
            turn_id: turn_id.clone(),
        }),
        Step::Emit(AdapterEvent::ItemCompleted { item }),
        Step::Emit(AdapterEvent::TurnCompleted {
            turn_id: turn_id.clone(),
            usage: None,
        }),
    ]
}

/// A turn of `rounds` rounds of work, `pause_ms` apart: each a Markdown message, a command and its
/// output, as a long session's transcript has them.
fn work(turn_id: &TurnId, rounds: usize, pause_ms: u64) -> Vec<Step> {
    let item = |id: String, body: ItemBody| {
        Step::Emit(AdapterEvent::ItemCompleted {
            item: Item {
                agent_message: None,
                follow_up: None,
                parent_call_id: None,
                id: ItemId::new(id),
                turn_id: turn_id.clone(),
                body,
            },
        })
    };
    let mut steps = vec![Step::Emit(AdapterEvent::TurnStarted {
        turn_id: turn_id.clone(),
    })];
    for round in 1..=rounds {
        let call = format!("{turn_id}-{round}-call");
        steps.push(item(
            format!("{turn_id}-{round}-message"),
            ItemBody::AssistantMessage {
                text: format!(
                    "Round {round}: the retry worker reads `max_attempts` from the queue config, \
                     so the cap holds for **every** handler.\n\n\
                     - `src/jobs/retry.rs` backs off exponentially, with jitter\n\
                     - `src/webhooks/stripe.rs` acknowledges before it persists\n\n\
                     ```rust\nlet delay = base * 2u32.pow(attempt.min(8)) + jitter();\n```"
                ),
            },
        ));
        steps.push(item(
            call.clone(),
            ItemBody::ToolCall {
                name: "Bash".into(),
                input: json!({ "command": "cargo test -p webhooks retry" }),
            },
        ));
        steps.push(item(
            format!("{turn_id}-{round}-result"),
            ItemBody::ToolResult {
                call_id: ItemId::new(call),
                output: "running 24 tests\n........................\n\
                         test result: ok. 24 passed; 0 failed; finished in 1.82s"
                    .into(),
                is_error: false,
            },
        ));
        if pause_ms > 0 {
            steps.push(Step::SleepMs(pause_ms));
        }
    }
    steps.push(Step::Emit(AdapterEvent::TurnCompleted {
        turn_id: turn_id.clone(),
        usage: None,
    }));
    steps
}

/// GitHub as the demo has it: `acme`'s repositories with the pull requests in [`PRS`], their
/// checks passing and no review threads.
struct DemoGitHub;

impl DemoGitHub {
    fn body(path: &str) -> Option<Value> {
        let parts: Vec<&str> = path.strip_prefix("repos/acme/")?.split('/').collect();
        match parts.as_slice() {
            [repo, "pulls", number] => {
                let number: u64 = number.parse().ok()?;
                let (_, title, branch, state) = PRS.iter().find(|pr| pr.0 == number)?;
                Some(json!({
                    "number": number,
                    "html_url": format!("https://github.com/acme/{repo}/pull/{number}"),
                    "title": title,
                    "state": if matches!(*state, "open" | "draft") { "open" } else { "closed" },
                    "draft": *state == "draft",
                    "merged_at": (*state == "merged").then_some("2026-10-20T09:00:00Z"),
                    "mergeable": true,
                    "created_at": "2026-10-20T08:00:00Z",
                    "head": {"sha": format!("{number:040x}"), "ref": branch},
                }))
            }
            [_, "commits", _, runs] if runs.starts_with("check-runs") => Some(json!({
                "total_count": 1,
                "check_runs": [{"name": "ci", "status": "completed", "conclusion": "success"}],
            })),
            _ => None,
        }
    }
}

/// Gives every artifact a krowk-shaped public link, uploading nothing.
struct DemoKrowk;

impl Publisher for DemoKrowk {
    fn upload(&self, name: String, _media_type: &'static str, _data: Bytes) -> UploadFuture {
        Box::pin(async move {
            Ok(publish::Link {
                url: format!("https://krowk.com/a/demo-{name}"),
                expires_at: None,
            })
        })
    }
}

impl GitHub for DemoGitHub {
    fn get<'a>(&'a self, _host: &'a str, path: &'a str, _etag: Option<&'a str>) -> GetFuture<'a> {
        let fetched = match Self::body(path) {
            Some(body) => Fetched::Modified { etag: None, body },
            None => Fetched::NotFound,
        };
        Box::pin(async move { Ok(fetched) })
    }

    fn graphql<'a>(
        &'a self,
        _host: &'a str,
        _query: &'a str,
        _variables: &'a Value,
    ) -> GraphqlFuture<'a> {
        Box::pin(async {
            Ok(json!({"repository": {"pullRequest": {"reviewThreads": {"nodes": []}}}}))
        })
    }
}
