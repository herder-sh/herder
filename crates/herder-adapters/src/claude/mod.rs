//! The Claude adapter: drives the unmodified `claude` binary over stream-json on stdio.
//!
//! One long-lived `claude -p --input-format stream-json --output-format stream-json` process
//! per session. Each prompt is one `user` line in; each turn ends with one `result` line out.
//! Everything else rides Claude Code's control protocol, the one the Agent SDK speaks:
//! `control_request` / `control_response` lines in both directions, each answered by its
//! `request_id`.
//!
//! - Starting sends `initialize` and resolves once it is answered. A seed transcript is then
//!   sent as one `user` line with `shouldQuery: false`, which joins the context without running
//!   a turn, and start waits for its `result`.
//! - Approvals: with `--permission-prompt-tool stdio` the CLI asks herder through a
//!   `can_use_tool` control request whenever its permission mode would prompt. herder answers
//!   `{"behavior": "allow"}` or `{"behavior": "deny", "message": ..}`. On interrupt the CLI
//!   withdraws an unanswered one with `control_cancel_request`.
//! - A prompt with images is one `user` line whose content is the images, as base64 `image`
//!   blocks, then the text as a `text` block; without images the content is the text alone.
//! - A `$name` mention of a skill in a prompt becomes `/name`, which the CLI expands. The
//!   skills are those the latest `system` `init` line listed; before the first turn, whose
//!   `init` comes only after its prompt, they are every command the `initialize` answer
//!   listed, as it does not tell skills apart from the CLI's own commands.
//! - Interrupt is the `interrupt` control request; the turn's `result` then ends it.
//! - Model and permission mode switch natively, with `set_model` and `set_permission_mode`.
//! - The CLI's session id comes on the `system` `init` line of the first turn and is reported
//!   as [`AdapterEvent::SessionIdentified`]. [`StartRequest::resume`] is passed as
//!   `--resume <id>`: the CLI loads that session's transcript from
//!   `<CLAUDE_CONFIG_DIR>/projects/<cwd key>/<id>.jsonl` and continues it with full context.
//!   A session it cannot find makes it exit, which fails the start.
//!
//! [`AdapterEvent::SessionIdentified`]: crate::AdapterEvent::SessionIdentified
//!
//! The process runs with exactly [`StartRequest::env`] plus `CLAUDE_CONFIG_DIR` set to the
//! account's config dir, when it has one, in the session's worktree. The adapter never looks inside that dir:
//! the CLI's own login is the only credential involved.
//!
//! # herder's MCP server
//!
//! [`StartRequest::mcp`] is passed inline as `--mcp-config`, a stdio server named `herder`, so
//! the agent sees its tools as `mcp__herder__spawn` and so on. `--mcp-config` adds to the
//! servers the user configured; `--strict-mcp-config` is never passed. The tools are allowed
//! with `--allowedTools mcp__herder`: they are herder's own and act only through herder, which
//! enforces its own limits, so asking the user before each `status` or `wait_for` would only
//! get in the way, and `dontAsk` would refuse them outright.
//!
//! # Steering delegation to `spawn`
//!
//! With herder's server comes a PreToolUse hook, passed inline as `--settings`, which adds to
//! the user's own settings and hooks rather than replacing them. It runs the herder binary
//! (the server's own command) as `herder hook claude-pre-tool-use` on every `Agent` call, and
//! [`pre_tool_use`] denies those with `isolation: "worktree"`: such a subagent builds inside
//! this session's memory limit, and its work and pull requests never show in herder. The
//! reason tells the model to call `mcp__herder__spawn` instead, or, in a child session, where
//! spawn is refused, to do the work itself. Other `Agent` calls, such as read-only `Explore`
//! lookups, are left alone. Child sessions get the same server and hook: the adapter cannot
//! tell them apart, and a worktree subagent of a child has the same problems.
//!
//! # Permission modes
//!
//! | herder        | `--permission-mode` | What Claude Code does                                  |
//! | ------------- | ------------------- | ------------------------------------------------------ |
//! | `read_only`   | `dontAsk`           | reads and pre-approved tools; denies whatever would ask |
//! | `ask`         | `default`           | reads freely, asks before edits, commands and network   |
//! | `auto_edit`   | `acceptEdits`       | also edits and runs common filesystem commands          |
//! | `full_access` | `bypassPermissions` | asks only when a safety check holds a call (below)      |
//!
//! `plan` is not `read_only`: it is a planning workflow whose exit (`ExitPlanMode`) turns
//! writes back on. The CLI always gets `--allow-dangerously-skip-permissions`, without which
//! a running session refuses to switch to `bypassPermissions`. When the agent changes mode
//! itself, the CLI reports it in a `system` `status` line and the adapter maps it back:
//! `plan` and `dontAsk` as `read_only`, `default` (shown as "manual") as `ask`. `auto`, where
//! a classifier approves instead of a person, has no herder mode and is not reported.
//!
//! `bypassPermissions` skips every permission prompt except those Claude Code's own safety
//! checks hold back (`decision_reason_type` `safetyCheck`, alone or among a compound
//! command's `subcommandResults`), and those still arrive as `can_use_tool`. One is the
//! dangerous `rm` check: it resolves a glob target such as `rm -f dir/*` against the shell's
//! working directory from before the command, ignoring a `cd` earlier in the same command, so
//! `cd <worktree> && rm -f docs/*` asks whenever an earlier call left the shell in a
//! subdirectory. Others hold `&` background jobs and writes to Claude's own settings. In
//! `full_access` the adapter allows these itself, so a session does not stop on them: the user
//! chose a mode that never asks. Still asked as in any mode:
//!
//! - Calls that need a person's answer rather than a permission (`requires_user_interaction`),
//!   such as `AskUserQuestion`.
//! - Removals the dangerous `rm` check flags for their target, its `decision_reason` reading
//!   `Dangerous rm operation on critical path: ..` (a system directory such as `/` or the
//!   home directory) or `.. on working directory or its ancestor: ..`, or that it could not
//!   analyse at all. Only `.. on statically-unresolvable target: ..`, the stale-`cd` case
//!   above, is allowed. A compound command reports one reason, the first held subcommand's.
//!
//! Claude Code's hard refusals never reach herder at all: deny rules, and calls its checks
//! deny outright, such as PowerShell `Remove-Item` on a protected system path.
//!
//! # Items
//!
//! Text and thinking stream with `--include-partial-messages`: a `content_block_start`,
//! deltas, then the `assistant` line carrying the finished block. An item is started on its
//! first non-empty delta, so thinking that the API only summarises as empty is never shown.
//! A `tool_use` block becomes a completed `tool_call` item, and the `tool_result` block in the
//! following `user` line its `tool_result` item. Lines from subagents (`parent_tool_use_id`
//! set) emit completed items with `parent_call_id` pointing to the spawning tool call, in
//! that call's turn: a background agent's keep coming after its turn ended, between turns too.
//! Nested streaming deltas are skipped; completed child blocks never change the parent stream.
//! A permission request whose tool call was omitted first emits its call so the approval has an item to name. The user's own prompt is not echoed. Items still streaming when a turn
//! ends, as on interrupt, are completed with the text received so far.
//!
//! # Background tasks
//!
//! A tool call can start a background task: an agent (the `Agent` tool, which Claude Code may
//! background whether or not the call asked for `run_in_background`), or a `Bash` command run
//! in the background. Its `tool_result` only acknowledges the launch, and the CLI announces the
//! task with a `system` `task_started` line naming the call's `tool_use_id`. When the task
//! ends, the CLI writes a prompt of its own: a `<task-notification>` block with the task's
//! `task-id`, `tool-use-id`, `output-file`, `status` (`completed`, `failed` or `stopped`),
//! `summary` and, for an agent, its `result`, each XML-escaped. `--replay-user-messages` puts
//! that prompt on stdout as a `user` line whose `origin.kind` is `task-notification` (it also
//! echoes herder's own prompts, `origin.kind` `human` or none, which the adapter skips).
//!
//! The notification becomes a `tool_result` item for the call that started the task, its
//! output the `result` (else the `summary`) and `is_error` set for `failed`, so a client links
//! the task's real outcome to its call by `call_id`, as for any tool. The calls that started a
//! task are remembered for the rest of the session; a notification for a call this process did
//! not see, as after a restart, adds no item.
//!
//! An agent's task runs from its `task_started` to its notification; `BackgroundAgents`
//! reports how many run each time that changes. The count drops only once the notification's
//! turn is open, not at the `system` `task_notification` line the CLI writes just before, so
//! the session never looks idle between the agent's end and the reply to it. Any other task,
//! such as a `Bash` command run in the background, is counted apart in `BackgroundCommands`,
//! from its `task_started` to its notification the same way: it does not keep the session
//! `running`, as a dev server runs for as long as the session.
//!
//! When the task ends while a turn runs, the CLI folds the notification into that turn. When
//! none runs, the CLI starts a turn itself: the adapter then sends `TurnStarted` with an id it
//! mints (a ULID, like the daemon's), the notification's item, the reply, and the turn's end,
//! as for any turn; `Interrupt` stops it. Any other top-level output while no turn runs opens
//! such a turn the same way. A prompt the daemon sends while the CLI's own turn runs is held
//! until that turn's `result`, then sent and started, so turns never overlap.
//!
//! # Questions
//!
//! `AskUserQuestion` arrives as a `can_use_tool` request too, after its `tool_call` item. One
//! call carries 1 to 4 questions; each becomes its own `QuestionAsked`, in order, with the
//! question as text, the options' descriptions as a list below it, and the options' labels as
//! `choices`. The call is answered once every one of its questions is: an allow whose
//! `updatedInput` is the call's input plus `answers`, mapping each question's text to the
//! chosen label or the free-text answer, the shape the Agent SDK documents. Claude Code then
//! returns the answers as the tool's result. Multi-select questions take one choice, or free
//! text that names several, which the CLI expects joined with `", "`; their text says so. A
//! choice the question does not have, or an answer to a question that is not open, is
//! ignored. Input without a question to show is denied. On interrupt the CLI withdraws the
//! call with `control_cancel_request`, voiding its questions.
//!
//! # Errors
//!
//! A turn whose `result` is an error is classified by the API error on the last `assistant`
//! line (its `error` field), else by the result's HTTP status:
//!
//! - `rate_limit` (429) is [`ErrorClass::LimitReached`] when the account's limit is spent: a
//!   `rate_limit_event` with status `rejected` came in the turn, or the text is not the
//!   CLI's "Server is temporarily limiting requests (not your usage limit)", which is
//!   [`ErrorClass::Transient`].
//! - `authentication_failed`, `oauth_org_not_allowed` (401, 403) are [`ErrorClass::Auth`].
//! - `overloaded`, `server_error` (5xx) are [`ErrorClass::Transient`].
//! - Anything else, `billing_error` included, is [`ErrorClass::Fatal`].
//!
//! A turn interrupted by herder ends `TurnInterrupted` whatever its result says.
//!
//! # Usage
//!
//! The CLI sends a `rate_limit_event` with every API response: `rate_limit_info` holds a
//! `status` (`allowed`, `allowed_warning`, `rejected`), `rateLimitType`, `resetsAt` (Unix
//! seconds), overage fields, and `unifiedWindows` with each window's `utilization` (0 to 1)
//! and `resetsAt`. Each event is reported as usage, its windows named as the CLI names them
//! (`five_hour`, `seven_day`).
//!
//! Those carry no per-model weekly windows, and an idle account sends none at all.
//! [`ClaudeAdapter::read_usage`] covers both: a short-lived `claude` that answers the
//! `get_usage` control request (the structured data behind `/usage`, from claude.ai's usage
//! endpoint) and exits, without a prompt or a turn. It runs with `--safe-mode`, so none of the
//! user's plugins, hooks or MCP servers start for it. Its `rate_limits` give `five_hour`,
//! `seven_day` and `seven_day_<model>` windows (utilization 0 to 100, ISO 8601 `resets_at`);
//! the per-model ones in `model_scoped` are named the same way, `seven_day_fable` for Fable.
//! The CLI reads its own login to answer; herder sees only the numbers.
//!
//! A completed turn's tokens come from its `result` line's `usage`. Its cost comes from
//! `total_cost_usd`, which adds up over the life of the process, so a turn costs what it grew
//! by since the last `result`, side calls on other models included. A `result` without it
//! gets the price table's estimate on the model last reported.

mod hooks;
mod session;
mod usage;
mod wire;

use std::path::{Path, PathBuf};

use herder_protocol::{ErrorClass, PermissionMode, TurnError};
use tokio::process::Command;

pub use hooks::{PRE_TOOL_USE_ARGS, pre_tool_use};

use crate::transport::Transport;
use crate::{AccountUsage, Adapter, McpServer, StartFuture, StartRequest};

/// The Claude Code version the wire format and fixtures were taken from.
pub const CLAUDE_VERSION: &str = "2.1.286";

/// Runs sessions on the `claude` CLI.
#[derive(Clone, Debug)]
pub struct ClaudeAdapter {
    /// The `claude` executable, looked up on `PATH` when it is a bare name.
    pub program: PathBuf,
}

impl Default for ClaudeAdapter {
    fn default() -> Self {
        Self {
            program: PathBuf::from("claude"),
        }
    }
}

impl Adapter for ClaudeAdapter {
    fn accepts_images(&self) -> bool {
        true
    }

    fn start(&self, request: StartRequest) -> StartFuture {
        let command = command(&self.program, &request);
        Box::pin(async move {
            let transport = Transport::spawn(command).map_err(|err| TurnError {
                class: ErrorClass::Fatal,
                message: format!("starting claude: {err}"),
            })?;
            start(transport, request).await
        })
    }
}

impl ClaudeAdapter {
    /// The account's email and limit windows, read by a `claude` run for `request` that exits
    /// once it answered; see the module docs.
    pub fn read_usage(
        &self,
        request: StartRequest,
    ) -> impl Future<Output = Result<AccountUsage, TurnError>> + Send + 'static {
        let mut command = command(&self.program, &request);
        // Nothing the user customized is needed to answer, and their hooks must not run.
        command.arg("--safe-mode");
        async move {
            let transport = Transport::spawn(command).map_err(|err| TurnError {
                class: ErrorClass::Fatal,
                message: format!("starting claude: {err}"),
            })?;
            read_usage(transport).await
        }
    }
}

/// Reads the account's email and limit windows over `transport`, which must carry a `claude`
/// started by [`command`], or a recording of one.
pub async fn read_usage(transport: Transport) -> Result<AccountUsage, TurnError> {
    usage::read(transport).await
}

/// Runs a session over `transport`, which must carry a `claude` started by [`command`], or a
/// recording of one from [`Transport::replay`].
pub fn start(transport: Transport, request: StartRequest) -> StartFuture {
    Box::pin(session::start(transport, request))
}

/// The `claude` command for `request`: its environment is exactly the request's plus
/// `CLAUDE_CONFIG_DIR` when the account has a config dir, in the session's worktree, with
/// herder's MCP server registered and herder's skill library added.
pub fn command(program: &Path, request: &StartRequest) -> Command {
    let mut command = request.command(program);
    command
        .args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--replay-user-messages",
            "--permission-prompt-tool",
            "stdio",
            "--allow-dangerously-skip-permissions",
            "--permission-mode",
            mode_flag(request.permission_mode),
        ])
        .env_clear()
        .envs(&request.env)
        .current_dir(&request.cwd);
    if let Some(dir) = &request.config_dir {
        command.env("CLAUDE_CONFIG_DIR", dir);
    }
    // Claude loads the skills in `<dir>/.claude/skills` of every added dir, as plain `/name`.
    if let Some(dir) = &request.skills {
        command.arg("--add-dir").arg(dir);
    }
    if let Some(model) = &request.model {
        command.args(["--model", model]);
    }
    if let Some(id) = &request.resume {
        command.args(["--resume", id]);
    }
    if let Some(mcp) = &request.mcp {
        command
            .arg("--mcp-config")
            .arg(mcp_config(mcp).to_string())
            .args(["--allowedTools", "mcp__herder"])
            .arg("--settings")
            .arg(hooks::settings(mcp).to_string());
    }
    command
}

/// Claude's limit on one herder tool call, in milliseconds: `wait_for` blocks up to 600 s, plus
/// a minute of slack. Set per server, so it overrides any `MCP_TOOL_TIMEOUT` in the
/// environment, which applies to every server and may be set lower than `wait_for` needs.
const MCP_TOOL_TIMEOUT_MS: u64 = 660_000;

/// The `--mcp-config` JSON that registers `mcp` as the `herder` server.
fn mcp_config(mcp: &McpServer) -> serde_json::Value {
    serde_json::json!({
        "mcpServers": {
            "herder": {
                "type": "stdio",
                "command": mcp.command.to_string_lossy(),
                "args": mcp.args,
                "timeout": MCP_TOOL_TIMEOUT_MS,
            }
        }
    })
}

/// Claude Code's permission mode for `mode`; see the module docs for the table.
fn mode_flag(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::ReadOnly => "dontAsk",
        PermissionMode::Ask => "default",
        PermissionMode::AutoEdit => "acceptEdits",
        PermissionMode::FullAccess => "bypassPermissions",
    }
}

/// herder's permission mode for a mode Claude Code reports; `None` for one herder has not.
fn mode_from_flag(mode: &str) -> Option<PermissionMode> {
    match mode {
        "dontAsk" | "plan" => Some(PermissionMode::ReadOnly),
        "default" | "manual" => Some(PermissionMode::Ask),
        "acceptEdits" => Some(PermissionMode::AutoEdit),
        "bypassPermissions" => Some(PermissionMode::FullAccess),
        _ => None,
    }
}

/// What a failed turn left behind to classify it by.
#[derive(Clone, Debug, Default, PartialEq)]
struct Failure {
    /// `error` of the last API-error `assistant` line, such as `rate_limit`.
    api_error: Option<String>,
    /// Text of that line, the CLI's own description of the error.
    text: Option<String>,
    /// The result's `api_error_status`.
    status: Option<u16>,
    /// Whether a `rate_limit_event` said `rejected` during the turn.
    limit_rejected: bool,
    /// The result's own text or errors, when there is no API error text.
    detail: Option<String>,
}

/// The CLI's text for a 429 that is server throttling, not the account's limit.
const THROTTLED: &str = "not your usage limit";

/// Classifies a failed turn; see the module docs.
fn classify(failure: Failure) -> TurnError {
    let kind = failure.api_error.as_deref().or(match failure.status {
        Some(429) => Some("rate_limit"),
        Some(401 | 403) => Some("authentication_failed"),
        Some(500..=599) => Some("server_error"),
        _ => None,
    });
    let message = failure
        .text
        .filter(|text| !text.is_empty())
        .or(failure.detail.filter(|text| !text.is_empty()))
        .unwrap_or_else(|| "claude turn failed".into());
    let class = match kind {
        Some("rate_limit") if failure.limit_rejected || !message.contains(THROTTLED) => {
            ErrorClass::LimitReached
        }
        Some("rate_limit" | "overloaded" | "server_error") => ErrorClass::Transient,
        Some("authentication_failed" | "oauth_org_not_allowed") => ErrorClass::Auth,
        _ => ErrorClass::Fatal,
    };
    TurnError { class, message }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn permission_modes_round_trip_through_their_flags() {
        let modes = [
            (PermissionMode::ReadOnly, "dontAsk"),
            (PermissionMode::Ask, "default"),
            (PermissionMode::AutoEdit, "acceptEdits"),
            (PermissionMode::FullAccess, "bypassPermissions"),
        ];
        for (mode, flag) in modes {
            assert_eq!(mode_flag(mode), flag);
            assert_eq!(mode_from_flag(flag), Some(mode));
        }
        assert_eq!(mode_from_flag("plan"), Some(PermissionMode::ReadOnly));
        assert_eq!(mode_from_flag("manual"), Some(PermissionMode::Ask));
        assert_eq!(mode_from_flag("auto"), None);
    }

    fn failure(api_error: Option<&str>, text: &str, status: Option<u16>) -> Failure {
        Failure {
            api_error: api_error.map(str::to_owned),
            text: Some(text.to_owned()),
            status,
            ..Failure::default()
        }
    }

    #[test]
    fn errors_are_classified_by_api_error_then_status() {
        let cases = [
            (
                failure(
                    Some("rate_limit"),
                    "You've hit your limit · resets 5pm",
                    Some(429),
                ),
                ErrorClass::LimitReached,
            ),
            (
                failure(
                    Some("rate_limit"),
                    "API Error: Server is temporarily limiting requests (not your usage limit) · \
                     Rate limited",
                    Some(429),
                ),
                ErrorClass::Transient,
            ),
            (
                failure(None, "Request rejected", Some(429)),
                ErrorClass::LimitReached,
            ),
            (
                failure(
                    Some("authentication_failed"),
                    "Not logged in · Please run /login",
                    None,
                ),
                ErrorClass::Auth,
            ),
            (
                failure(Some("oauth_org_not_allowed"), "x", Some(403)),
                ErrorClass::Auth,
            ),
            (failure(None, "x", Some(401)), ErrorClass::Auth),
            (
                failure(Some("overloaded"), "x", Some(529)),
                ErrorClass::Transient,
            ),
            (
                failure(Some("server_error"), "x", Some(500)),
                ErrorClass::Transient,
            ),
            (failure(None, "x", Some(503)), ErrorClass::Transient),
            (
                failure(Some("billing_error"), "x", Some(400)),
                ErrorClass::Fatal,
            ),
            (
                failure(Some("invalid_request"), "x", Some(400)),
                ErrorClass::Fatal,
            ),
            (failure(None, "x", None), ErrorClass::Fatal),
        ];
        for (failure, class) in cases {
            assert_eq!(classify(failure.clone()).class, class, "{failure:?}");
        }
    }

    #[test]
    fn a_rejected_limit_wins_over_throttling_text() {
        let mut throttled = failure(Some("rate_limit"), THROTTLED, Some(429));
        throttled.limit_rejected = true;
        assert_eq!(classify(throttled).class, ErrorClass::LimitReached);
    }

    #[test]
    fn the_message_falls_back_to_the_result_detail() {
        let failure = Failure {
            detail: Some("error_max_turns".into()),
            ..Failure::default()
        };
        assert_eq!(
            classify(failure),
            TurnError {
                class: ErrorClass::Fatal,
                message: "error_max_turns".into()
            }
        );
        assert_eq!(classify(Failure::default()).message, "claude turn failed");
    }

    #[test]
    fn command_sets_exactly_the_request_env_and_config_dir() {
        let request = StartRequest {
            config_dir: Some(PathBuf::from("/accounts/work/claude")),
            env: BTreeMap::from([("PATH".to_owned(), "/usr/bin".to_owned())]),
            cwd: PathBuf::from("/worktrees/s1"),
            model: Some("sonnet".into()),
            permission_mode: PermissionMode::AutoEdit,
            seed: Vec::new(),
            resume: None,
            mcp: None,
            launcher: Vec::new(),
            skills: None,
        };
        let command = command(Path::new("claude"), &request);
        let command = command.as_std();
        assert_eq!(command.get_program(), "claude");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--replay-user-messages",
                "--permission-prompt-tool",
                "stdio",
                "--allow-dangerously-skip-permissions",
                "--permission-mode",
                "acceptEdits",
                "--model",
                "sonnet",
            ]
            .map(OsStr::new)
        );
        assert_eq!(command.get_current_dir(), Some(request.cwd.as_path()));
        let mut envs: Vec<_> = command.get_envs().collect();
        envs.sort();
        assert_eq!(
            envs,
            [
                (
                    OsStr::new("CLAUDE_CONFIG_DIR"),
                    Some(OsStr::new("/accounts/work/claude"))
                ),
                (OsStr::new("PATH"), Some(OsStr::new("/usr/bin"))),
            ]
        );
    }

    #[test]
    fn command_resumes_the_requested_session() {
        let request = StartRequest {
            config_dir: Some(PathBuf::from("/accounts/home/claude")),
            env: BTreeMap::new(),
            cwd: PathBuf::from("/worktrees/s1"),
            model: None,
            permission_mode: PermissionMode::Ask,
            seed: Vec::new(),
            resume: Some("499f57af-e4af-4c6c-b348-d47c9b704e70".into()),
            mcp: None,
            launcher: Vec::new(),
            skills: None,
        };
        let command = command(Path::new("claude"), &request);
        let args: Vec<_> = command.as_std().get_args().collect();
        let at = args.iter().position(|arg| *arg == "--resume").unwrap();
        assert_eq!(args[at + 1], "499f57af-e4af-4c6c-b348-d47c9b704e70");
        let without = StartRequest {
            resume: None,
            ..request
        };
        let command = super::command(Path::new("claude"), &without);
        assert!(!command.as_std().get_args().any(|arg| arg == "--resume"));
    }

    #[test]
    fn command_without_a_config_dir_leaves_claude_config_dir_unset() {
        let request = StartRequest {
            config_dir: None,
            env: BTreeMap::from([("PATH".to_owned(), "/usr/bin".to_owned())]),
            cwd: PathBuf::from("/worktrees/s1"),
            model: None,
            permission_mode: PermissionMode::Ask,
            seed: Vec::new(),
            resume: None,
            mcp: None,
            launcher: Vec::new(),
            skills: None,
        };
        let command = command(Path::new("claude"), &request);
        let envs: Vec<_> = command.as_std().get_envs().collect();
        assert_eq!(envs, [(OsStr::new("PATH"), Some(OsStr::new("/usr/bin")))]);
        // Without herder's server there is no spawn to steer to, so no hook either.
        assert!(!command.as_std().get_args().any(|arg| arg == "--settings"));
    }

    #[test]
    fn command_registers_herders_mcp_server() {
        let request = StartRequest {
            config_dir: None,
            env: BTreeMap::new(),
            cwd: PathBuf::from("/worktrees/s1"),
            model: None,
            permission_mode: PermissionMode::Ask,
            seed: Vec::new(),
            resume: None,
            mcp: Some(McpServer {
                command: PathBuf::from("/usr/bin/herder"),
                args: vec!["mcp".into(), "--session".into(), "s1".into()],
            }),
            launcher: Vec::new(),
            skills: None,
        };
        let command = command(Path::new("claude"), &request);
        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert!(!args.contains(&"--strict-mcp-config"));
        let at = args.iter().position(|arg| *arg == "--mcp-config").unwrap();
        let config: serde_json::Value = serde_json::from_str(args[at + 1]).unwrap();
        assert_eq!(
            config,
            serde_json::json!({
                "mcpServers": {
                    "herder": {
                        "type": "stdio",
                        "command": "/usr/bin/herder",
                        "args": ["mcp", "--session", "s1"],
                        "timeout": 660_000,
                    }
                }
            })
        );
        assert_eq!(&args[at + 2..at + 4], ["--allowedTools", "mcp__herder"]);
        assert_eq!(args[at + 4], "--settings");
        let settings: serde_json::Value = serde_json::from_str(args[at + 5]).unwrap();
        assert_eq!(
            settings,
            serde_json::json!({
                "hooks": {
                    "PreToolUse": [{
                        "matcher": "Agent",
                        "hooks": [{
                            "type": "command",
                            "command": "/usr/bin/herder",
                            "args": ["hook", "claude-pre-tool-use"],
                        }]
                    }]
                }
            })
        );
        assert_eq!(args.len(), at + 6);
    }

    #[test]
    fn command_adds_the_skill_library_dir() {
        let request = StartRequest {
            config_dir: None,
            env: BTreeMap::new(),
            cwd: PathBuf::from("/worktrees/s1"),
            model: None,
            permission_mode: PermissionMode::Ask,
            seed: Vec::new(),
            resume: None,
            mcp: None,
            launcher: Vec::new(),
            skills: Some(PathBuf::from("/data/skill-links/claude")),
        };
        let command = command(Path::new("claude"), &request);
        let args: Vec<_> = command.as_std().get_args().collect();
        let at = args.iter().position(|arg| *arg == "--add-dir").unwrap();
        assert_eq!(args[at + 1], "/data/skill-links/claude");
        let without = StartRequest {
            skills: None,
            ..request
        };
        let command = super::command(Path::new("claude"), &without);
        assert!(!command.as_std().get_args().any(|arg| arg == "--add-dir"));
    }

    #[test]
    fn command_runs_behind_the_launcher_and_directly_without_one() {
        let direct = StartRequest {
            config_dir: Some(PathBuf::from("/accounts/work/claude")),
            env: BTreeMap::from([("PATH".to_owned(), "/usr/bin".to_owned())]),
            cwd: PathBuf::from("/worktrees/s1"),
            model: Some("sonnet".into()),
            permission_mode: PermissionMode::Ask,
            seed: Vec::new(),
            resume: None,
            mcp: None,
            launcher: Vec::new(),
            skills: None,
        };
        let launched = StartRequest {
            launcher: crate::testing::launcher(),
            ..direct.clone()
        };
        let program = Path::new("/usr/bin/claude");
        let direct = command(program, &direct);
        assert_eq!(direct.as_std().get_program(), program);
        crate::testing::assert_behind_launcher(&direct, &command(program, &launched));
    }
}
