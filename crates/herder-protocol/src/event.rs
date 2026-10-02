//! Durable session events: the records of herder's append-only per-session journal.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AccountId, ApprovalId, ItemId, PermissionMode, Provider, QuestionId, Seq, SessionId, Timestamp,
    TurnId, UserId,
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
///
/// A task is a primary session that spawns child sessions. Each child is a full session with
/// its own journal; the primary's journal records `child_spawned` and `child_reported`. A
/// child's approvals and questions are journaled in the child's own journal, whoever answers
/// them.
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
        /// Primary session of the task this session is a child of; absent for a top-level session.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<SessionId>,
        /// Short label of the session's task, shown in the task tree.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<String>,
    },
    /// The session's worktree had a branch checked out that the session had not had before.
    /// The session owns it from then on, including after its worktree is removed. The branch
    /// in `session_created` is the session's first and gets no event of its own.
    BranchCheckedOut {
        /// The branch, as a short name such as `herder/1a2b3c4d`.
        branch: String,
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
    /// The agent is blocked until someone allows or denies a tool call.
    ApprovalRequested {
        /// The request, answered by this id.
        approval_id: ApprovalId,
        /// Turn that is blocked.
        turn_id: TurnId,
        /// Tool call item awaiting permission.
        tool_call_id: ItemId,
        /// One-line description of what the agent wants to do.
        summary: String,
        /// Who is asked first; a user can always answer, whatever the route.
        #[serde(default)]
        routed_to: Route,
        /// Why a child's request went straight to the user; absent when it follows the default route.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<EscalationReason>,
    },
    /// A request routed to the primary session now waits for the user.
    ApprovalEscalated {
        /// The escalated request.
        approval_id: ApprovalId,
        /// Why it was escalated.
        reason: EscalationReason,
        /// What the primary session told the user about it, as Markdown; absent when it said nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// A user or the primary session answered an approval request, or the daemon closed it
    /// as `expired`.
    ApprovalResolved {
        /// The resolved request.
        approval_id: ApprovalId,
        /// How it was resolved.
        decision: ApprovalOutcome,
        /// Who answered; `by` names the user when it is a user. An `expired` request has no `by`
        /// and is `user`.
        #[serde(default)]
        answered_by: Answerer,
    },
    /// The agent is blocked until someone answers a question; cleared when its turn ends.
    QuestionAsked {
        /// The question, answered by this id.
        question_id: QuestionId,
        /// Turn that is blocked.
        turn_id: TurnId,
        /// The question, as Markdown.
        text: String,
        /// Answers to pick from; empty for a free-text answer.
        choices: Vec<String>,
        /// Who is asked first; a user can always answer, whatever the route.
        routed_to: Route,
        /// Why a child's question went straight to the user; absent when it follows the default route.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<EscalationReason>,
    },
    /// A question routed to the primary session now waits for the user.
    QuestionEscalated {
        /// The escalated question.
        question_id: QuestionId,
        /// Why it was escalated.
        reason: EscalationReason,
        /// What the primary session told the user about it, as Markdown; absent when it said nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// A user or the primary session answered a question.
    QuestionAnswered {
        /// The answered question.
        question_id: QuestionId,
        /// The answer.
        answer: Answer,
        /// Who answered; `by` names the user when it is a user.
        answered_by: Answerer,
    },
    /// This session spawned a child session for part of its task.
    ChildSpawned {
        /// The child, whose `session_created` names this session as its parent.
        child_session_id: SessionId,
        /// Short label of the child's task.
        task: String,
    },
    /// A child session finished a turn and reported back.
    ChildReported {
        /// The child.
        child_session_id: SessionId,
        /// The child's turn that ended.
        turn_id: TurnId,
        /// The child's final assistant message of that turn, or a short status when it failed.
        summary: String,
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
    /// The agent is working, or waiting for its primary session to answer an approval or question.
    Running,
    /// A prompt waits for the host to have capacity for another turn; the turn starts, and the
    /// status becomes `running`, once it does. Survives a daemon restart, and the prompt keeps
    /// its place in the host's queue.
    WaitingForCapacity,
    /// Blocked on a user: an approval or question routed or escalated to the user, or a failed turn to act on.
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

/// An answer to an approval request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Run the tool call once.
    Allow,
    /// Refuse the tool call.
    Deny,
}

/// How an approval request was resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalOutcome {
    /// The tool call ran once.
    Allow,
    /// The tool call was refused.
    Deny,
    /// Nobody answered before its turn ended or the daemon restarted; the agent no longer waits
    /// for it, and asks again in a later turn if it still needs to.
    Expired,
}

impl From<ApprovalDecision> for ApprovalOutcome {
    fn from(decision: ApprovalDecision) -> Self {
        match decision {
            ApprovalDecision::Allow => ApprovalOutcome::Allow,
            ApprovalDecision::Deny => ApprovalOutcome::Deny,
        }
    }
}

/// Who an approval request or question is put to first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    /// The primary session of the asking child's task.
    Primary,
    /// A user.
    #[default]
    User,
}

/// Why a child's approval request or question goes to the user instead of its primary session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EscalationReason {
    /// The primary session marked it as the user's decision.
    MarkedByPrimary,
    /// It exceeds what the primary session may decide.
    ExceedsAuthority,
    /// The primary session did not answer in time.
    Timeout,
}

/// Who answered an approval request or question.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Answerer {
    /// A user, named by the event's `by`.
    #[default]
    User,
    /// The primary session of the asking child's task.
    Primary {
        /// The primary session.
        session_id: SessionId,
    },
}

/// An answer to a question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Answer {
    /// A free-text answer.
    Text {
        /// Answer text.
        text: String,
    },
    /// One of the question's choices.
    Choice {
        /// Position of the choice in the question's `choices`, from 0.
        index: u32,
    },
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
