//! Durable session events: the records of herder's append-only per-session journal.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AccountId, ApprovalId, ItemId, PermissionMode, Provider, Seq, SessionId, Timestamp, TurnId,
    UserId,
};

/// One journal record: `seq` orders it within its session, `by` names the user who caused it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Event {
    /// Session whose journal holds this event.
    pub session_id: SessionId,
    /// Position in the session's journal, gap-free from 1.
    pub seq: Seq,
    /// When the daemon recorded the event.
    pub at: Timestamp,
    /// User whose command caused the event; absent when the agent or the daemon caused it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<UserId>,
    /// What happened.
    pub body: EventBody,
}

/// What a durable event records.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventBody {
    /// The session exists: one repo, one worktree and branch, on this host.
    SessionCreated {
        /// Absolute path of the repository on the host.
        repo: String,
        /// Absolute path of the session's worktree on the host.
        worktree: String,
        /// Branch the session owns.
        branch: String,
        /// Provider of the starting account.
        provider: Provider,
        /// Starting account.
        account_id: AccountId,
        /// Starting model, in the provider's own naming.
        model: String,
        /// Starting permission mode.
        permission_mode: PermissionMode,
    },
    /// The session's status changed.
    SessionStatusChanged {
        /// New status.
        status: SessionStatus,
    },
    /// The agent started working on a prompt.
    TurnStarted {
        /// The new turn.
        turn_id: TurnId,
    },
    /// The turn finished normally.
    TurnCompleted {
        /// The finished turn.
        turn_id: TurnId,
    },
    /// The turn was stopped by an interrupt command.
    TurnInterrupted {
        /// The stopped turn.
        turn_id: TurnId,
    },
    /// The turn ended with an error.
    TurnFailed {
        /// The failed turn.
        turn_id: TurnId,
        /// Why it failed.
        error: TurnError,
    },
    /// A complete item joined the transcript.
    ItemAdded {
        /// The item, in its final form.
        item: Item,
    },
    /// The agent is blocked until a user allows or denies a tool call.
    ApprovalRequested {
        /// The request, answered by this id.
        approval_id: ApprovalId,
        /// Turn that is blocked.
        turn_id: TurnId,
        /// Tool call item awaiting permission.
        tool_call_id: ItemId,
        /// One-line description of what the agent wants to do.
        summary: String,
    },
    /// A user answered an approval request.
    ApprovalResolved {
        /// The answered request.
        approval_id: ApprovalId,
        /// The answer.
        decision: ApprovalDecision,
    },
    /// The model changed within the same provider.
    ModelSwitched {
        /// New model, in the provider's own naming.
        model: String,
    },
    /// The account changed within the same provider; `by` is absent for a failover.
    AccountSwitched {
        /// New account.
        account_id: AccountId,
    },
    /// The session moved to another provider by transcript replay; `by` is absent for a failover.
    ProviderSwitched {
        /// New provider.
        provider: Provider,
        /// New account, of that provider.
        account_id: AccountId,
        /// New model, in that provider's own naming.
        model: String,
    },
    /// The permission mode changed.
    PermissionModeChanged {
        /// New permission mode.
        mode: PermissionMode,
    },
    /// A pull request is now tracked for the session; `by` is absent when auto-detected.
    PrLinked {
        /// The pull request as first seen.
        pr: PullRequest,
    },
    /// A tracked pull request changed; carries its full new state.
    PrUpdated {
        /// The pull request as now seen.
        pr: PullRequest,
    },
    /// A pull request is no longer tracked for the session.
    PrUnlinked {
        /// Number of the pull request in the session's repository.
        number: u64,
    },
    /// An event type newer than this build; skip it.
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Unknown,
}

/// Where a session stands, as shown in lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    /// Waiting for a prompt.
    Idle,
    /// The agent is working.
    Running,
    /// Blocked on a user: an approval, or a failed turn to act on.
    NeedsYou,
    /// Cannot continue without intervention.
    Error,
    /// Put away by a user; read-only.
    Archived,
    /// Handed off to another host; read-only here.
    Moved,
    /// A status newer than this build.
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Unknown,
}

/// Why a turn failed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TurnError {
    /// What kind of failure, which decides what can be done about it.
    pub class: ErrorClass,
    /// Human-readable detail from the provider or daemon.
    pub message: String,
}

/// Kind of turn failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// The account hit a usage limit; the only class that may trigger failover.
    LimitReached,
    /// The account's provider login is missing or expired.
    Auth,
    /// A temporary failure; retrying may succeed.
    Transient,
    /// A permanent failure; retrying will not help.
    Fatal,
}

/// One entry of the transcript.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Item {
    /// The item.
    pub id: ItemId,
    /// Turn the item belongs to.
    pub turn_id: TurnId,
    /// The item's content.
    pub body: ItemBody,
}

/// Content of a transcript item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ItemBody {
    /// A prompt from a user; the author is the event's `by`.
    UserMessage {
        /// Prompt text.
        text: String,
    },
    /// Text the agent wrote to the user.
    AssistantMessage {
        /// Message text, as Markdown.
        text: String,
    },
    /// The agent's visible reasoning, when the provider exposes it.
    Reasoning {
        /// Reasoning text.
        text: String,
    },
    /// The agent invoked a tool.
    ToolCall {
        /// Tool name, in the provider's own naming.
        name: String,
        /// Tool arguments, as the provider sent them.
        input: serde_json::Value,
    },
    /// A tool finished.
    ToolResult {
        /// Tool call item this result answers.
        call_id: ItemId,
        /// Tool output text.
        output: String,
        /// Whether the tool reported failure.
        is_error: bool,
    },
    /// An item type newer than this build; skip it.
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Unknown,
}

/// A user's answer to an approval request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Run the tool call once.
    Allow,
    /// Refuse the tool call.
    Deny,
}

/// A GitHub pull request tracked for a session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequest {
    /// Number in the session's repository.
    pub number: u64,
    /// Web URL.
    pub url: String,
    /// Title.
    pub title: String,
    /// Lifecycle state.
    pub state: PrState,
    /// Combined status of required checks.
    pub ci: CiStatus,
    /// Review verdict.
    pub review: ReviewStatus,
    /// Whether it can merge cleanly.
    pub mergeable: Mergeable,
}

/// Lifecycle state of a pull request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    /// Draft, not ready for review.
    Draft,
    /// Open for review.
    Open,
    /// Merged.
    Merged,
    /// Closed without merging.
    Closed,
}

/// Combined status of a pull request's checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CiStatus {
    /// No checks reported.
    None,
    /// Checks still running.
    Pending,
    /// Every check passed.
    Passing,
    /// At least one check failed.
    Failing,
}

/// Review verdict on a pull request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    /// No review required or given.
    None,
    /// Waiting for a required review.
    Required,
    /// Approved.
    Approved,
    /// A reviewer requested changes.
    ChangesRequested,
}

/// Whether a pull request can merge cleanly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Mergeable {
    /// Merges cleanly.
    Clean,
    /// Has conflicts with its base.
    Conflicting,
    /// GitHub has not computed it yet.
    Unknown,
}
