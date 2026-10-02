//! The slice of the `codex app-server` protocol the adapter uses, as serde types.
//!
//! Hand-written from the schema snapshot in `schemas/codex/` (see its `VERSION`), keeping only
//! the methods and fields herder reads or writes. Unknown fields are ignored, so additions on
//! the Codex side do not break parsing; `tests/codex.rs` validates every fixture line against
//! the snapshot, so a removal or rename shows up as a test failure once the snapshot is
//! regenerated.
//!
//! The wire format is JSON-RPC 2.0 without the `"jsonrpc"` member, one message per line.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A request herder sends.
#[derive(Debug, Serialize)]
pub struct Request<'a, P> {
    pub id: u64,
    pub method: &'a str,
    pub params: P,
}

/// A notification herder sends.
#[derive(Debug, Serialize)]
pub struct Notification<'a> {
    pub method: &'a str,
}

/// A successful response to a server request.
#[derive(Debug, Serialize)]
pub struct Response<R> {
    pub id: Value,
    pub result: R,
}

/// An error response to a server request.
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub id: Value,
    pub error: RpcError,
}

/// A JSON-RPC error object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

/// JSON-RPC "method not found", for server requests herder does not handle.
pub const METHOD_NOT_FOUND: i64 = -32601;

/// Any line from the server, before it is told apart by which members it has.
#[derive(Debug, Deserialize)]
pub struct Incoming {
    #[serde(default)]
    pub id: Option<Value>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<RpcError>,
}

// ---- Requests herder sends ----

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams<'a> {
    pub client_info: ClientInfo<'a>,
    pub capabilities: Option<()>,
}

#[derive(Debug, Serialize)]
pub struct ClientInfo<'a> {
    pub name: &'a str,
    pub title: Option<&'a str>,
    pub version: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountReadParams {
    pub refresh_token: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartParams<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<&'a str>,
    pub cwd: &'a str,
    pub approval_policy: AskForApproval,
    pub sandbox: SandboxMode,
}

/// `thread/resume`: reopens a thread from its rollout file, which Codex finds by id under
/// `CODEX_HOME/sessions`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadResumeParams<'a> {
    pub thread_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<&'a str>,
    pub cwd: &'a str,
    pub approval_policy: AskForApproval,
    pub sandbox: SandboxMode,
    /// herder has the history already; only the thread is wanted.
    pub exclude_turns: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadInjectItemsParams<'a> {
    pub thread_id: &'a str,
    pub items: Vec<Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartParams<'a> {
    pub thread_id: &'a str,
    pub input: [UserInput<'a>; 1],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<&'a str>,
    pub approval_policy: AskForApproval,
    pub sandbox_policy: SandboxPolicy,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UserInput<'a> {
    Text {
        text: &'a str,
        text_elements: [(); 0],
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnInterruptParams<'a> {
    pub thread_id: &'a str,
    pub turn_id: &'a str,
}

/// When Codex asks before acting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AskForApproval {
    /// Asks before anything outside a small set of known-safe read commands.
    Untrusted,
    /// Never asks; what the sandbox refuses fails.
    Never,
}

/// Sandbox for `thread/start`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

/// Sandbox for `turn/start`: the same three modes, in their policy form.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SandboxPolicy {
    #[serde(rename_all = "camelCase")]
    ReadOnly {
        network_access: bool,
    },
    #[serde(rename_all = "camelCase")]
    WorkspaceWrite {
        writable_roots: [(); 0],
        network_access: bool,
        exclude_tmpdir_env_var: bool,
        exclude_slash_tmp: bool,
    },
    DangerFullAccess,
}

/// The answer to an approval request; both approval kinds share these values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalAnswer {
    Accept,
    Decline,
}

#[derive(Debug, Serialize)]
pub struct ApprovalResult {
    pub decision: ApprovalAnswer,
}

// ---- Responses herder reads ----

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountReadResult {
    pub account: Option<Value>,
    pub requires_openai_auth: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitsReadResult {
    pub rate_limits: RateLimitSnapshot,
    #[serde(default)]
    pub rate_limits_by_limit_id: Option<std::collections::BTreeMap<String, RateLimitSnapshot>>,
}

/// What `thread/start` and `thread/resume` both answer with.
#[derive(Debug, Deserialize)]
pub struct ThreadStartResult {
    pub thread: ThreadRef,
    pub model: String,
}

#[derive(Debug, Deserialize)]
pub struct ThreadRef {
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct TurnStartResult {
    pub turn: Turn,
}

// ---- Notifications herder reads ----

#[derive(Debug, Deserialize)]
pub struct TurnNotification {
    pub turn: Turn,
}

#[derive(Debug, Deserialize)]
pub struct Turn {
    pub id: String,
    pub status: TurnStatus,
    #[serde(default)]
    pub error: Option<CodexTurnError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    Completed,
    Interrupted,
    Failed,
    InProgress,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexTurnError {
    pub message: String,
    /// A string such as `"usageLimitExceeded"`, or a one-key object such as
    /// `{"httpConnectionFailed": {"httpStatusCode": 502}}`.
    #[serde(default)]
    pub codex_error_info: Option<Value>,
    #[serde(default)]
    pub additional_details: Option<String>,
}

impl CodexTurnError {
    /// The `codexErrorInfo` variant name, whichever of its two shapes it has.
    pub fn kind(&self) -> Option<&str> {
        match self.codex_error_info.as_ref()? {
            Value::String(kind) => Some(kind),
            Value::Object(map) => map.keys().next().map(String::as_str),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorNotification {
    pub error: CodexTurnError,
    pub will_retry: bool,
}

#[derive(Debug, Deserialize)]
pub struct ItemNotification {
    pub item: ThreadItem,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeltaNotification {
    pub item_id: String,
    pub delta: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitsUpdated {
    pub rate_limits: RateLimitSnapshot,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitSnapshot {
    #[serde(default)]
    pub limit_id: Option<String>,
    #[serde(default)]
    pub primary: Option<RateLimitWindow>,
    #[serde(default)]
    pub secondary: Option<RateLimitWindow>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitWindow {
    pub used_percent: f64,
    #[serde(default)]
    pub window_duration_mins: Option<i64>,
    /// Unix seconds.
    #[serde(default)]
    pub resets_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRerouted {
    pub to_model: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerRequestResolved {
    pub request_id: Value,
}

/// A transcript item, for the item types herder maps; the rest are `Other` and skipped.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ThreadItem {
    AgentMessage {
        id: String,
        text: String,
    },
    Reasoning {
        id: String,
        #[serde(default)]
        summary: Vec<String>,
        #[serde(default)]
        content: Vec<String>,
    },
    #[serde(rename_all = "camelCase")]
    CommandExecution {
        id: String,
        command: String,
        cwd: String,
        status: ToolStatus,
        #[serde(default)]
        aggregated_output: Option<String>,
        #[serde(default)]
        exit_code: Option<i64>,
    },
    FileChange {
        id: String,
        changes: Vec<FileChange>,
        status: ToolStatus,
    },
    McpToolCall {
        id: String,
        server: String,
        tool: String,
        status: ToolStatus,
        #[serde(default)]
        arguments: Value,
        #[serde(default)]
        result: Option<Value>,
        #[serde(default)]
        error: Option<McpError>,
    },
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub kind: Value,
    pub diff: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolStatus {
    InProgress,
    Completed,
    Failed,
    Declined,
}

#[derive(Debug, Deserialize)]
pub struct McpError {
    pub message: String,
}

// ---- Server requests herder answers ----

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandApproval {
    pub item_id: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChangeApproval {
    pub item_id: String,
    #[serde(default)]
    pub reason: Option<String>,
}
