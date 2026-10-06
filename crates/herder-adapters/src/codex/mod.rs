//! The Codex adapter: drives `codex app-server` over JSON-RPC on stdio.
//!
//! One app-server process per session, one Codex thread in it. Starting runs the handshake
//! (`initialize`, then `initialized`), checks the login with `account/read`, reports the limit
//! windows from `account/rateLimits/read`, and opens the thread with `thread/start`. A seed
//! transcript is replayed into the new thread with `thread/inject_items`. Each prompt is one
//! `turn/start`, which also carries the current model and permission mode, so both switch
//! natively from the next turn on. A prompt's images go in the same `input`, ahead of its
//! text, each as an `image` input whose `url` is a `data:` URL of the bytes, so no file is
//! written for them.
//!
//! The thread id is reported as [`AdapterEvent::SessionIdentified`] once the thread is open.
//! With [`StartRequest::resume`] the thread is reopened with `thread/resume` instead of
//! `thread/start`: Codex finds its rollout file, `CODEX_HOME/sessions/YYYY/MM/DD/
//! rollout-<timestamp>-<thread id>.jsonl`, by id, and the thread continues with its full
//! history. A thread it cannot find fails the start.
//!
//! [`AdapterEvent::SessionIdentified`]: crate::AdapterEvent::SessionIdentified
//!
//! [`CodexAdapter::read_usage`] runs only the handshake and `account/rateLimits/read`, then
//! asks the app-server to exit: the limit windows of an account no session runs on.
//!
//! A turn's tokens are the `last` of every `thread/tokenUsage/updated` for it, one per model
//! call, added up; Codex's `inputTokens` include the cached ones, which herder splits out.
//! Codex reports no cost, so the price table estimates it on the turn's model.
//!
//! The process runs with exactly [`StartRequest::env`] plus `CODEX_HOME` set to the account's
//! config dir, when it has one. The adapter never looks inside that dir.
//!
//! # Permission modes
//!
//! Each [`PermissionMode`] is a Codex approval policy plus sandbox:
//!
//! | herder        | `approvalPolicy` | sandbox              |
//! | ------------- | ---------------- | -------------------- |
//! | `read_only`   | `never`          | `read-only`          |
//! | `ask`         | `untrusted`      | `read-only`          |
//! | `auto_edit`   | `untrusted`      | `workspace-write`    |
//! | `full_access` | `never`          | `danger-full-access` |
//!
//! `read_only` never asks, so whatever the read-only sandbox refuses fails. `ask` asks before
//! anything but the commands Codex itself treats as safe reads (`ls`, `cat`, ...), which run
//! unasked in the read-only sandbox. `auto_edit` lets Codex write inside the worktree and asks
//! before untrusted commands. The sandbox never gets network access, except under
//! `full_access`, which has no sandbox.
//!
//! # Items
//!
//! Agent messages and reasoning summaries stream (`ItemStarted`, deltas, `ItemCompleted`).
//! Command executions, file changes and MCP tool calls become a completed `tool_call` item when
//! Codex starts them, so an approval request can name it, and a `tool_result` item when they
//! finish. The user's own message is not echoed: the daemon already has the prompt. Other item
//! types (plans, web searches, compaction markers, ...) are skipped. Items still streaming when
//! a turn ends, as on interrupt, are completed with the text received so far.
//!
//! # Errors
//!
//! A failed turn is classified by its `codexErrorInfo`: `usageLimitExceeded` is
//! [`ErrorClass::LimitReached`], `unauthorized` is [`ErrorClass::Auth`], overload, HTTP and
//! stream failures and `rateLimitExceeded` (a short-term request rate limit, not the account's
//! usage limit) are [`ErrorClass::Transient`], and anything else is [`ErrorClass::Fatal`].

mod session;
mod wire;

use std::path::PathBuf;

use herder_protocol::{ErrorClass, PermissionMode, TurnError};
use tokio::process::Command;

use crate::transport::Transport;
use crate::{AccountUsage, Adapter, StartFuture, StartRequest};

use wire::{AskForApproval, CodexTurnError, SandboxMode, SandboxPolicy};

/// The Codex CLI version the protocol types and fixtures were taken from.
pub const CODEX_VERSION: &str = "0.159.2";

/// Runs sessions on `codex app-server`.
#[derive(Clone, Debug)]
pub struct CodexAdapter {
    /// The `codex` executable, looked up on `PATH` when it is a bare name.
    pub program: PathBuf,
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self {
            program: PathBuf::from("codex"),
        }
    }
}

impl Adapter for CodexAdapter {
    fn start(&self, request: StartRequest) -> StartFuture {
        let command = command(&self.program, &request);
        Box::pin(async move {
            let transport = Transport::spawn(command).map_err(|err| TurnError {
                class: ErrorClass::Fatal,
                message: format!("starting codex app-server: {err}"),
            })?;
            start(transport, request).await
        })
    }

    fn accepts_images(&self) -> bool {
        true
    }
}

impl CodexAdapter {
    /// The account's email and limit windows, read by a `codex app-server` run for `request`
    /// that is asked to exit once it answered; no thread is opened.
    pub fn read_usage(
        &self,
        request: StartRequest,
    ) -> impl Future<Output = Result<AccountUsage, TurnError>> + Send + 'static {
        let command = command(&self.program, &request);
        async move {
            let transport = Transport::spawn(command).map_err(|err| TurnError {
                class: ErrorClass::Fatal,
                message: format!("starting codex app-server: {err}"),
            })?;
            read_usage(transport, &request).await
        }
    }
}

/// Reads the account's email and limit windows over `transport`, which must carry a
/// `codex app-server` from [`command`], or a recording of one.
pub async fn read_usage(
    transport: Transport,
    request: &StartRequest,
) -> Result<AccountUsage, TurnError> {
    session::read_usage(transport, request).await
}

/// Runs a session over `transport`, which must carry a `codex app-server`: a real one from
/// [`command`], or a recorded one from [`Transport::replay`].
pub fn start(transport: Transport, request: StartRequest) -> StartFuture {
    Box::pin(session::start(transport, request))
}

/// The `codex app-server` command for `request`: its environment is exactly the request's plus
/// `CODEX_HOME` when the account has a config dir, in the session's worktree.
pub fn command(program: &std::path::Path, request: &StartRequest) -> Command {
    let mut command = request.command(program);
    command
        .arg("app-server")
        .env_clear()
        .envs(&request.env)
        .current_dir(&request.cwd);
    if let Some(dir) = &request.config_dir {
        command.env("CODEX_HOME", dir);
    }
    command
}

/// The Codex approval policy and sandbox for `mode`; see the module docs for the table.
fn policy(mode: PermissionMode) -> (AskForApproval, SandboxMode) {
    match mode {
        PermissionMode::ReadOnly => (AskForApproval::Never, SandboxMode::ReadOnly),
        PermissionMode::Ask => (AskForApproval::Untrusted, SandboxMode::ReadOnly),
        PermissionMode::AutoEdit => (AskForApproval::Untrusted, SandboxMode::WorkspaceWrite),
        PermissionMode::FullAccess => (AskForApproval::Never, SandboxMode::DangerFullAccess),
    }
}

/// `sandbox` in the policy form `turn/start` takes.
fn sandbox_policy(sandbox: SandboxMode) -> SandboxPolicy {
    match sandbox {
        SandboxMode::ReadOnly => SandboxPolicy::ReadOnly {
            network_access: false,
        },
        SandboxMode::WorkspaceWrite => SandboxPolicy::WorkspaceWrite {
            writable_roots: [],
            network_access: false,
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
        },
        SandboxMode::DangerFullAccess => SandboxPolicy::DangerFullAccess,
    }
}

/// Classifies a Codex turn error; see the module docs.
fn classify(error: &CodexTurnError) -> TurnError {
    let class = match error.kind() {
        Some("usageLimitExceeded") => ErrorClass::LimitReached,
        Some("unauthorized") => ErrorClass::Auth,
        Some(
            "rateLimitExceeded"
            | "serverOverloaded"
            | "internalServerError"
            | "flexUnavailable"
            | "httpConnectionFailed"
            | "responseStreamConnectionFailed"
            | "responseStreamDisconnected"
            | "responseTooManyFailedAttempts",
        ) => ErrorClass::Transient,
        _ => ErrorClass::Fatal,
    };
    let message = match error.additional_details.as_deref() {
        Some(details) if !details.is_empty() => format!("{} ({details})", error.message),
        _ => error.message.clone(),
    };
    TurnError { class, message }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsStr;

    use serde_json::json;

    use super::*;

    fn error(info: serde_json::Value) -> CodexTurnError {
        serde_json::from_value(json!({
            "message": "boom",
            "codexErrorInfo": info,
            "additionalDetails": null
        }))
        .unwrap()
    }

    #[test]
    fn errors_are_classified_by_codex_error_info() {
        let cases = [
            (json!("usageLimitExceeded"), ErrorClass::LimitReached),
            (json!("unauthorized"), ErrorClass::Auth),
            (json!("rateLimitExceeded"), ErrorClass::Transient),
            (json!("serverOverloaded"), ErrorClass::Transient),
            (
                json!({"httpConnectionFailed": {"httpStatusCode": 502}}),
                ErrorClass::Transient,
            ),
            (
                json!({"responseStreamDisconnected": {"httpStatusCode": null}}),
                ErrorClass::Transient,
            ),
            (json!("contextWindowExceeded"), ErrorClass::Fatal),
            (json!("sessionBudgetExceeded"), ErrorClass::Fatal),
            (json!("other"), ErrorClass::Fatal),
            (json!(null), ErrorClass::Fatal),
        ];
        for (info, class) in cases {
            assert_eq!(classify(&error(info.clone())).class, class, "{info}");
        }
    }

    #[test]
    fn additional_details_join_the_message() {
        let mut error = error(json!("other"));
        error.additional_details = Some("status 500".into());
        assert_eq!(classify(&error).message, "boom (status 500)");
    }

    #[test]
    fn permission_modes_map_to_policy_and_sandbox() {
        let wire = |mode| {
            let (approval, sandbox) = policy(mode);
            (
                serde_json::to_value(approval).unwrap(),
                serde_json::to_value(sandbox).unwrap(),
                serde_json::to_value(sandbox_policy(sandbox)).unwrap(),
            )
        };
        assert_eq!(
            wire(PermissionMode::ReadOnly),
            (
                json!("never"),
                json!("read-only"),
                json!({"type": "readOnly", "networkAccess": false})
            )
        );
        assert_eq!(
            wire(PermissionMode::Ask),
            (
                json!("untrusted"),
                json!("read-only"),
                json!({"type": "readOnly", "networkAccess": false})
            )
        );
        assert_eq!(
            wire(PermissionMode::AutoEdit),
            (
                json!("untrusted"),
                json!("workspace-write"),
                json!({
                    "type": "workspaceWrite",
                    "writableRoots": [],
                    "networkAccess": false,
                    "excludeTmpdirEnvVar": false,
                    "excludeSlashTmp": false
                })
            )
        );
        assert_eq!(
            wire(PermissionMode::FullAccess),
            (
                json!("never"),
                json!("danger-full-access"),
                json!({"type": "dangerFullAccess"})
            )
        );
    }

    #[test]
    fn command_sets_exactly_the_request_env_and_codex_home() {
        let request = StartRequest {
            config_dir: Some(PathBuf::from("/accounts/work/codex")),
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
        let command = command(std::path::Path::new("codex"), &request);
        let command = command.as_std();
        assert_eq!(command.get_program(), "codex");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [OsStr::new("app-server")]
        );
        assert_eq!(command.get_current_dir(), Some(request.cwd.as_path()));
        let mut envs: Vec<_> = command.get_envs().collect();
        envs.sort();
        assert_eq!(
            envs,
            [
                (
                    OsStr::new("CODEX_HOME"),
                    Some(OsStr::new("/accounts/work/codex"))
                ),
                (OsStr::new("PATH"), Some(OsStr::new("/usr/bin"))),
            ]
        );
    }

    #[test]
    fn command_without_a_config_dir_leaves_codex_home_unset() {
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
        let command = command(std::path::Path::new("codex"), &request);
        let envs: Vec<_> = command.as_std().get_envs().collect();
        assert_eq!(envs, [(OsStr::new("PATH"), Some(OsStr::new("/usr/bin")))]);
    }

    #[test]
    fn command_runs_behind_the_launcher_and_directly_without_one() {
        let direct = StartRequest {
            config_dir: Some(PathBuf::from("/accounts/work/codex")),
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
        let launched = StartRequest {
            launcher: crate::testing::launcher(),
            ..direct.clone()
        };
        let program = std::path::Path::new("codex");
        let direct = command(program, &direct);
        assert_eq!(direct.as_std().get_program(), "codex");
        crate::testing::assert_behind_launcher(&direct, &command(program, &launched));
    }
}
