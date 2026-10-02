//! The provider adapter contract, and a scripted fake adapter for tests.
//!
//! An [`Adapter`] drives one vendor CLI. [`Adapter::start`] runs it for one session and resolves
//! once it accepts prompts, yielding an [`AdapterSession`]: a command channel in, one ordered
//! event channel out. Every vendor shape fits behind it: a long-lived stream-json process
//! (Claude), JSON-RPC over stdio (Codex app-server, ACP agents).
//!
//! Vendor adapters drive their CLI through a [`transport::Transport`], which tests swap for a
//! recorded [`fixture::Fixture`].
//!
//! Adapter events are not journal events. The daemon turns them into journal events, which is
//! why they reuse the [`herder_protocol`] types the journal is made of.
//!
//! # Event order
//!
//! - A turn is [`AdapterEvent::TurnStarted`], then any items, approval requests and questions of
//!   that turn, then exactly one of `TurnCompleted`, `TurnInterrupted` or `TurnFailed`. One turn
//!   at a time. A turn's end voids its pending approvals and questions.
//! - A streamed item is `ItemStarted`, then `ItemDelta`s, then `ItemCompleted` with its final
//!   body. An item that does not stream is a lone `ItemCompleted`.
//! - `ApprovalRequested` follows the `ItemCompleted` of the tool call it names.
//! - `QuestionAsked` blocks the turn until the daemon sends `AnswerQuestion`. An adapter whose
//!   CLI cannot ask never sends it, and so never receives `AnswerQuestion`.
//! - `UsageReported`, `ModelChanged` and `PermissionModeChanged` may come at any time.
//! - `Exited` is the last event, always sent, after which the channel closes. A turn still open
//!   when the process dies is first closed with `TurnFailed`.
//!
//! # Ids
//!
//! The daemon mints [`TurnId`]s and passes them in [`AdapterCommand::SendPrompt`]. The adapter
//! mints [`ItemId`]s, [`ApprovalId`]s and [`QuestionId`]s, unique within the session.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use herder_protocol::{
    Answer, ApprovalDecision, ApprovalId, Item, ItemId, PermissionMode, QuestionId, TurnError,
    TurnId, UsageWindow,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

pub mod acp;
pub mod claude;
pub mod codex;
pub mod fake;
pub mod fixture;
pub mod record;
pub mod transport;

/// A vendor CLI that herder can run sessions on, one implementation per CLI shape.
///
/// The daemon holds one per provider as `Box<dyn Adapter>` and never branches on the provider:
/// whatever differs is in [`Capabilities`].
pub trait Adapter: Send + Sync {
    /// Starts the CLI for one session; must be polled inside a tokio runtime.
    ///
    /// Resolves once the CLI accepts prompts. A failure to get there, such as a missing login,
    /// is classified like a turn failure, since starting is part of running the next turn.
    fn start(&self, request: StartRequest) -> StartFuture;
}

/// What [`Adapter::start`] returns: owns everything it needs, so the daemon can spawn it.
pub type StartFuture = Pin<Box<dyn Future<Output = Result<AdapterSession, TurnError>> + Send>>;

/// Everything an adapter needs to run one session.
#[derive(Clone, Debug, PartialEq)]
pub struct StartRequest {
    /// The account's config dir; the adapter points the CLI at it (`CLAUDE_CONFIG_DIR`,
    /// `CODEX_HOME`, ...) and never reads what is inside. Absent for an account in the CLI's
    /// default location: the variable is then not set, so `env` decides.
    pub config_dir: Option<PathBuf>,
    /// The CLI's complete environment; the adapter adds nothing but the config dir variable.
    pub env: BTreeMap<String, String>,
    /// The session's worktree, the CLI's working directory.
    pub cwd: PathBuf,
    /// Model, in the provider's own naming; the provider's default when absent.
    pub model: Option<String>,
    /// Starting permission mode.
    pub permission_mode: PermissionMode,
    /// Transcript to replay as context before the first prompt; empty for a fresh session.
    pub seed: Vec<Item>,
    /// herder's MCP server for this session, which the adapter registers with the CLI as
    /// `herder`, next to the user's own servers; absent when the daemon serves none.
    pub mcp: Option<McpServer>,
}

/// A stdio MCP server: the command the CLI spawns, speaking MCP on its stdin and stdout. It
/// carries no secret, so it is safe on a command line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServer {
    /// The executable.
    pub command: PathBuf,
    /// Its arguments.
    pub args: Vec<String>,
}

/// A running session of a vendor CLI.
#[derive(Debug)]
pub struct AdapterSession {
    /// What this session can do natively.
    pub capabilities: Capabilities,
    /// Commands in. Dropping every sender is the same as [`AdapterCommand::Shutdown`].
    pub commands: mpsc::UnboundedSender<AdapterCommand>,
    /// Events out, in order; closes after [`AdapterEvent::Exited`]. Bounded, so a daemon that
    /// stops reading stalls the CLI instead of growing memory.
    pub events: mpsc::Receiver<AdapterEvent>,
}

/// What a session can do natively; the daemon decides from these, never from the provider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Accepts [`AdapterCommand::SetModel`]; otherwise the daemon restarts with a seed.
    pub native_model_switch: bool,
    /// Accepts [`AdapterCommand::SetPermissionMode`]; otherwise the daemon restarts with a seed.
    pub native_permission_mode_switch: bool,
    /// Sends [`AdapterEvent::UsageReported`].
    pub reports_usage: bool,
}

/// A command from the daemon. The daemon only sends what the session's state allows, such as
/// one prompt at a time, and only what its [`Capabilities`] accept.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdapterCommand {
    /// Start a turn with a prompt, while no turn runs.
    SendPrompt {
        /// Id of the new turn, minted by the daemon.
        turn_id: TurnId,
        /// Prompt text.
        text: String,
    },
    /// Stop the running turn; answered by `TurnInterrupted`, voiding any pending approval.
    Interrupt,
    /// Change the model; answered by `ModelChanged`.
    SetModel {
        /// New model, in the provider's own naming.
        model: String,
    },
    /// Change the permission mode; answered by `PermissionModeChanged`.
    SetPermissionMode {
        /// New permission mode.
        mode: PermissionMode,
    },
    /// Answer a pending approval request.
    AnswerApproval {
        /// The request.
        approval_id: ApprovalId,
        /// The answer.
        decision: ApprovalDecision,
    },
    /// Answer a pending question; sent only for a question the adapter asked.
    AnswerQuestion {
        /// The question.
        question_id: QuestionId,
        /// The answer; a `choice` indexes the question's `choices`.
        answer: Answer,
    },
    /// Stop the CLI; answered by `Exited`.
    Shutdown,
}

/// Something the CLI did, in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdapterEvent {
    /// The agent started working on a prompt.
    TurnStarted {
        /// The turn, as given in `SendPrompt`.
        turn_id: TurnId,
    },
    /// The turn finished normally.
    TurnCompleted {
        /// The finished turn.
        turn_id: TurnId,
    },
    /// The turn stopped because of `Interrupt`.
    TurnInterrupted {
        /// The stopped turn.
        turn_id: TurnId,
    },
    /// The turn ended with an error, classified by the adapter.
    TurnFailed {
        /// The failed turn.
        turn_id: TurnId,
        /// Why; `LimitReached` is the only class that may trigger failover.
        error: TurnError,
    },
    /// An item began streaming; `ItemDelta`s append to its text.
    ItemStarted {
        /// The item so far.
        item: Item,
    },
    /// Text appended to a started item.
    ItemDelta {
        /// The started item.
        item_id: ItemId,
        /// Text to append.
        text: String,
    },
    /// An item is final.
    ItemCompleted {
        /// The item in its final form.
        item: Item,
    },
    /// The agent is blocked until the daemon sends `AnswerApproval`.
    ApprovalRequested {
        /// The request, minted by the adapter.
        approval_id: ApprovalId,
        /// Turn that is blocked.
        turn_id: TurnId,
        /// Completed tool call item awaiting permission.
        tool_call_id: ItemId,
        /// One-line description of what the agent wants to do.
        summary: String,
    },
    /// The agent is blocked until the daemon sends `AnswerQuestion`, such as Claude's
    /// `AskUserQuestion` tool.
    QuestionAsked {
        /// The question, minted by the adapter.
        question_id: QuestionId,
        /// Turn that is blocked.
        turn_id: TurnId,
        /// The question, as Markdown.
        text: String,
        /// Answers to pick from; empty for a free-text answer.
        choices: Vec<String>,
    },
    /// The provider reported the account's limit windows.
    UsageReported {
        /// Every window it reported.
        windows: Vec<UsageWindow>,
    },
    /// The effective model is now known or changed, by `SetModel` or by the provider itself.
    ModelChanged {
        /// The model, in the provider's own naming.
        model: String,
    },
    /// The permission mode changed, by `SetPermissionMode` or by the agent itself.
    PermissionModeChanged {
        /// The new mode.
        mode: PermissionMode,
    },
    /// The CLI is gone; always the last event.
    Exited {
        /// Why, when it was not asked to stop.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<TurnError>,
    },
}
