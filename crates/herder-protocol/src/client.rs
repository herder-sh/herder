//! Messages a client sends to the daemon.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AccountId, Answer, ApprovalDecision, ApprovalId, Bytes, CommandId, PermissionMode, QuestionId,
    Seq, SessionId, TerminalId,
};

/// A client-to-daemon message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// First message on every connection.
    Hello(ClientHello),
    /// Stream a session's events after a cursor, then live; replaces any existing subscription.
    Subscribe(Cursor),
    /// Stop streaming a session.
    Unsubscribe {
        /// Session to stop.
        session_id: SessionId,
    },
    /// Ask the daemon to change something.
    Command(Command),
}

/// Opening message of a client connection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClientHello {
    /// Must equal the daemon's [`crate::PROTOCOL_VERSION`].
    pub protocol_version: u32,
    /// Client name and version, for logs, e.g. `herder-tui/0.1.0`.
    pub client: String,
    /// Subscriptions to restore, each resuming after its last seen event.
    pub resume: Vec<Cursor>,
}

/// A position in a session's journal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Cursor {
    /// The session.
    pub session_id: SessionId,
    /// Last event seq the client holds; 0 for none.
    pub after_seq: Seq,
}

/// A request to change state, applied at most once per `id`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Command {
    /// Idempotency key: a resend with the same id is answered without being applied again.
    pub id: CommandId,
    /// What to do.
    pub body: CommandBody,
}

/// What a command asks for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommandBody {
    /// Create a session on a new worktree and branch of a repository.
    CreateSession {
        /// Absolute path of the repository on the host.
        repo: String,
        /// Branch to create; the daemon picks a name when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        /// Account to run on.
        account_id: AccountId,
        /// Model to use; the provider's default when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Starting permission mode.
        permission_mode: PermissionMode,
    },
    /// Start a turn with a prompt.
    SendPrompt {
        /// Target session.
        session_id: SessionId,
        /// Prompt text.
        text: String,
    },
    /// Stop the running turn.
    Interrupt {
        /// Target session.
        session_id: SessionId,
    },
    /// Change the model within the current provider.
    SetModel {
        /// Target session.
        session_id: SessionId,
        /// New model, in the provider's own naming.
        model: String,
    },
    /// Change the permission mode.
    SetPermissionMode {
        /// Target session.
        session_id: SessionId,
        /// New permission mode.
        mode: PermissionMode,
    },
    /// Answer a pending approval request as the user, whoever it is routed to.
    AnswerApproval {
        /// Target session.
        session_id: SessionId,
        /// Request to answer.
        approval_id: ApprovalId,
        /// The answer.
        decision: ApprovalDecision,
    },
    /// Answer a pending question as the user, whoever it is routed to.
    AnswerQuestion {
        /// Session that asked.
        session_id: SessionId,
        /// Question to answer.
        question_id: QuestionId,
        /// The answer.
        answer: Answer,
    },
    /// Move to another account of the same provider.
    SwitchAccount {
        /// Target session.
        session_id: SessionId,
        /// New account.
        account_id: AccountId,
    },
    /// Move to an account of another provider, replaying the transcript.
    SwitchProvider {
        /// Target session.
        session_id: SessionId,
        /// New account; its provider is the new provider.
        account_id: AccountId,
        /// Model to use; the new provider's default when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    /// Track a pull request for the session.
    LinkPr {
        /// Target session.
        session_id: SessionId,
        /// Number of the pull request in the session's repository.
        number: u64,
    },
    /// Stop tracking a pull request for the session.
    UnlinkPr {
        /// Target session.
        session_id: SessionId,
        /// Number of the pull request in the session's repository.
        number: u64,
    },
    /// Open a shell in the session's worktree and attach to it; owners only.
    OpenTerminal {
        /// Target session.
        session_id: SessionId,
        /// Width in columns.
        cols: u16,
        /// Height in rows.
        rows: u16,
    },
    /// Start streaming a terminal's output; owners only.
    AttachTerminal {
        /// Target terminal.
        terminal_id: TerminalId,
    },
    /// Stop streaming a terminal's output; the shell keeps running.
    DetachTerminal {
        /// Target terminal.
        terminal_id: TerminalId,
    },
    /// Change a terminal's size.
    ResizeTerminal {
        /// Target terminal.
        terminal_id: TerminalId,
        /// Width in columns.
        cols: u16,
        /// Height in rows.
        rows: u16,
    },
    /// Write bytes to a terminal's input.
    TerminalInput {
        /// Target terminal.
        terminal_id: TerminalId,
        /// Bytes to write.
        data: Bytes,
    },
}
