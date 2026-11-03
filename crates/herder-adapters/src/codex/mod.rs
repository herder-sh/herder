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
//! An endpoint account's CLI holds no login: the daemon adds [`endpoint_env`] to the request's
//! environment, the API server's base URL and its client key. Seeing them, [`command`] adds
//! [`endpoint_args`], `-c` overrides that register a model provider `herder` at
//! `<base URL>/v1` speaking the OpenAI Responses API (`wire_api = "responses"`, which a
//! CLIProxyAPI serves at `/v1/responses`) and make it the thread's provider. Codex reads the
//! key from the variable the provider's `env_key` names, so it never shows on a command line,
//! and the user's `config.toml` is never written. Such a provider needs no OpenAI login, so
//! `account/read` reports none required.
//!
//! herder's MCP server and the user's ([`StartRequest::mcp_servers`]) go in `thread/start`'s
//! (or `thread/resume`'s) `config` as `mcp_servers.<name>` overrides, next to the servers of
//! the account's `config.toml`; Codex reaches no SSE server, so those are left out.
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
//! Before all that, an HTTP 401, as an endpoint answers a client key it refuses, is
//! [`ErrorClass::Auth`], and a connection failure with no HTTP status (`additionalDetails` starting
//! `Connection failed`) is [`ErrorClass::Unreachable`]: Codex could not reach the API at all.
//!
//! Codex never fails a turn on such a failure: it retries it without end, reporting each try
//! as a retried `error` ("Reconnecting... waiting for network"). On an endpoint account the
//! adapter gives up after [`UNREACHABLE_RETRIES`] of them in a row, about 25 seconds: it
//! interrupts the turn and fails it as unreachable, so the session can move on. A login
//! account keeps Codex's own wait, for a network that is down for everyone.

mod session;
mod wire;

use std::collections::BTreeMap;
use std::path::PathBuf;

use herder_protocol::{ErrorClass, HERDER_MCP_SERVER, PermissionMode, TurnError};
use serde_json::{Map, Value, json};
use tokio::process::Command;

use crate::transport::Transport;
use crate::{AccountUsage, Adapter, McpTransport, StartFuture, StartRequest};

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
/// `CODEX_HOME` when the account has a config dir, in the session's worktree. An endpoint
/// account's also gets [`endpoint_args`].
pub fn command(program: &std::path::Path, request: &StartRequest) -> Command {
    let mut command = request.command(program);
    command
        .arg("app-server")
        .args(endpoint_args(&request.env))
        .env_clear()
        .envs(&request.env)
        .current_dir(&request.cwd);
    if let Some(dir) = &request.config_dir {
        command.env("CODEX_HOME", dir);
    }
    command
}

/// The variable an endpoint account's base URL is handed to the adapter in; [`command`]
/// turns it into [`endpoint_args`].
pub const ENDPOINT_URL_ENV: &str = "HERDER_ENDPOINT_URL";

/// The variable an endpoint account's client key is in, which the `herder` provider's
/// `env_key` names: Codex sends it as a bearer token.
pub const ENDPOINT_KEY_ENV: &str = "HERDER_ENDPOINT_KEY";

/// The model provider an endpoint account's Codex runs on.
const ENDPOINT_PROVIDER: &str = "herder";

/// How many retried connection failures in a row an endpoint account's turn gets before the
/// adapter gives up on it as unreachable.
pub const UNREACHABLE_RETRIES: u32 = 3;

/// The variables that point `codex` at the API server `url` with client key `key`, instead of
/// its login; [`command`] reads the URL back into [`endpoint_args`].
pub fn endpoint_env(url: &str, key: &str) -> [(&'static str, String); 2] {
    [
        (ENDPOINT_URL_ENV, url.to_owned()),
        (ENDPOINT_KEY_ENV, key.to_owned()),
    ]
}

/// The `-c` overrides that run `codex` on the endpoint `env` names in [`ENDPOINT_URL_ENV`];
/// none for a login account. Values are TOML strings; the key stays in `env`.
pub fn endpoint_args(env: &BTreeMap<String, String>) -> Vec<String> {
    let Some(url) = env.get(ENDPOINT_URL_ENV) else {
        return Vec::new();
    };
    let base_url = format!("{}/v1", url.trim_end_matches('/'));
    let provider = format!("model_providers.{ENDPOINT_PROVIDER}");
    [
        ("model_provider".to_owned(), ENDPOINT_PROVIDER),
        (format!("{provider}.name"), ENDPOINT_PROVIDER),
        (format!("{provider}.base_url"), &base_url),
        (format!("{provider}.env_key"), ENDPOINT_KEY_ENV),
        (format!("{provider}.wire_api"), "responses"),
    ]
    .into_iter()
    .flat_map(|(key, value)| ["-c".to_owned(), format!("{key}={}", toml_string(value))])
    .collect()
}

/// `value` as a TOML basic string; a JSON string is one.
fn toml_string(value: &str) -> String {
    Value::from(value).to_string()
}

/// Codex's limit on one herder tool call, in seconds: `wait_for` blocks up to 600 s, plus a
/// minute of slack; Codex's default is 60 s.
const MCP_TOOL_TIMEOUT_SECS: u64 = 660;

/// The thread config that registers herder's MCP server as `herder` and the user's under their
/// names, as dotted `mcp_servers.<name>` keys, so the servers in the account's own
/// `config.toml` stay. Codex reaches no SSE server, so those are left out. It goes to Codex
/// in `thread/start` on stdin, keeping variables and headers off the command line.
fn mcp_config(request: &StartRequest) -> Map<String, Value> {
    let herder = request.mcp.iter().map(|mcp| {
        let server = json!({
            "command": mcp.command.to_string_lossy(),
            "args": mcp.args,
            "tool_timeout_sec": MCP_TOOL_TIMEOUT_SECS,
        });
        (HERDER_MCP_SERVER.to_owned(), server)
    });
    let user = request.mcp_servers.iter().filter_map(|server| {
        let config = match &server.transport {
            McpTransport::Stdio { command, args, env } => {
                json!({"command": command, "args": args, "env": env})
            }
            McpTransport::Http { url, headers } => json!({"url": url, "http_headers": headers}),
            McpTransport::Sse { .. } => return None,
        };
        Some((server.name.clone(), config))
    });
    herder
        .chain(user)
        .map(|(name, server)| (format!("mcp_servers.{name}"), server))
        .collect()
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

/// The HTTP status `error`'s `codexErrorInfo` carries, as in
/// `{"httpConnectionFailed": {"httpStatusCode": 401}}`; `Some(None)` for one with none.
fn http_status(error: &CodexTurnError) -> Option<Option<u64>> {
    let detail = error
        .codex_error_info
        .as_ref()?
        .as_object()?
        .values()
        .next()?;
    Some(detail.get("httpStatusCode").and_then(Value::as_u64))
}

/// Whether `error` is Codex failing to reach the API at all, with no HTTP answer.
fn unreachable(error: &CodexTurnError) -> bool {
    http_status(error) == Some(None)
        && error
            .additional_details
            .as_deref()
            .is_some_and(|details| details.starts_with("Connection failed"))
}

/// Classifies a Codex turn error; see the module docs.
fn classify(error: &CodexTurnError) -> TurnError {
    let class = match error.kind() {
        _ if unreachable(error) => ErrorClass::Unreachable,
        _ if http_status(error) == Some(Some(401)) => ErrorClass::Auth,
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
            mcp_servers: Vec::new(),
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
    fn an_endpoint_account_runs_on_a_herder_provider_with_its_key_off_the_command_line() {
        let mut env = BTreeMap::from([("PATH".to_owned(), "/usr/bin".to_owned())]);
        env.extend(
            endpoint_env("http://127.0.0.1:8317/", "cpa-client-key").map(|(k, v)| (k.into(), v)),
        );
        let request = StartRequest {
            config_dir: Some(PathBuf::from("/home/u/.codex-fleet")),
            env,
            cwd: PathBuf::from("/worktrees/s1"),
            model: None,
            permission_mode: PermissionMode::Ask,
            seed: Vec::new(),
            resume: None,
            mcp: None,
            mcp_servers: Vec::new(),
            launcher: Vec::new(),
            skills: None,
        };
        let command = command(std::path::Path::new("codex"), &request);
        let command = command.as_std();
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert_eq!(
            args,
            [
                "app-server",
                "-c",
                r#"model_provider="herder""#,
                "-c",
                r#"model_providers.herder.name="herder""#,
                "-c",
                r#"model_providers.herder.base_url="http://127.0.0.1:8317/v1""#,
                "-c",
                r#"model_providers.herder.env_key="HERDER_ENDPOINT_KEY""#,
                "-c",
                r#"model_providers.herder.wire_api="responses""#,
            ]
        );
        assert!(!args.iter().any(|arg| arg.contains("cpa-client-key")));
        let mut envs: Vec<_> = command.get_envs().collect();
        envs.sort();
        assert_eq!(
            envs,
            [
                (
                    OsStr::new("CODEX_HOME"),
                    Some(OsStr::new("/home/u/.codex-fleet"))
                ),
                (
                    OsStr::new("HERDER_ENDPOINT_KEY"),
                    Some(OsStr::new("cpa-client-key"))
                ),
                (
                    OsStr::new("HERDER_ENDPOINT_URL"),
                    Some(OsStr::new("http://127.0.0.1:8317/"))
                ),
                (OsStr::new("PATH"), Some(OsStr::new("/usr/bin"))),
            ]
        );
    }

    #[test]
    fn a_connection_failure_is_unreachable_and_a_refused_key_auth() {
        let failed = |info: serde_json::Value, details: &str| {
            let mut error = error(info);
            error.additional_details = Some(details.into());
            classify(&error).class
        };
        let disconnected = json!({"responseStreamDisconnected": {"httpStatusCode": null}});
        assert_eq!(
            failed(
                disconnected.clone(),
                "Connection failed: error sending request"
            ),
            ErrorClass::Unreachable
        );
        assert_eq!(
            failed(disconnected, "stream closed before response.completed"),
            ErrorClass::Transient
        );
        assert_eq!(
            failed(
                json!({"httpConnectionFailed": {"httpStatusCode": 502}}),
                "Connection failed: bad gateway"
            ),
            ErrorClass::Transient,
            "the server answered"
        );
        assert_eq!(
            failed(
                json!({"httpConnectionFailed": {"httpStatusCode": 401}}),
                "unexpected status 401 Unauthorized: {\"error\":\"Invalid API key\"}"
            ),
            ErrorClass::Auth,
            "an endpoint refused the client key"
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
            mcp_servers: Vec::new(),
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
            mcp_servers: Vec::new(),
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
