//! Messages a client sends to the daemon.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AccountId, Answer, ApprovalDecision, ApprovalId, AttachmentId, Bytes, CommandId, Image,
    PermissionMode, ProjectId, Provider, QuestionId, Seq, SessionId, TerminalId,
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
    /// A barrier: the daemon answers `synced` with the same token once it handled every
    /// message sent before this one, so the lists sent after hello and the replay of every
    /// earlier subscription arrive before the answer.
    Sync {
        /// Chosen by the client to match the answer.
        token: String,
    },
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
    /// One-time code from `herder pair`, sent by a device that is not paired yet; ignored once
    /// it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_code: Option<String>,
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
    /// Create a session on a new worktree and branch of a repository, named by exactly one of
    /// `repo` and `project_id`.
    CreateSession {
        /// Absolute path of the repository on the host.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repo: Option<String>,
        /// Project to work on, in its first clone on the host.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<ProjectId>,
        /// Branch to create; the daemon picks a name when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        /// Account to run on. When absent: the available account of `provider` with the most
        /// room left in its usage windows, when `provider` is set; else the project's
        /// `default_account`, which then must be set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account_id: Option<AccountId>,
        /// Provider to run on, when `account_id` is absent; with `account_id`, it must be that
        /// account's provider.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<Provider>,
        /// Model to use; the provider's default when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Starting permission mode; the project's `default_permission_mode` when absent, else
        /// `ask`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_mode: Option<PermissionMode>,
        /// Most live children the session may have as a task's primary; the daemon's
        /// `[tasks] max_children` when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_children: Option<u32>,
        /// Whether the session stays on its account when it hits a limit instead of rotating
        /// to another account of its provider; the daemon's `[failover] pin` when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failover_pin: Option<bool>,
    },
    /// Archive a session: remove its worktree, keep its branches, and make it read-only.
    ArchiveSession {
        /// Target session.
        session_id: SessionId,
        /// Remove the worktree even when it has uncommitted or untracked changes.
        force: bool,
    },
    /// Bring an archived session back: recreate its worktree, at the path it had, on the
    /// session's own branch as archive kept it, and make the session writable again.
    UnarchiveSession {
        /// Target session.
        session_id: SessionId,
    },
    /// Set the session's title; archived and moved sessions refuse it.
    RenameSession {
        /// Target session.
        session_id: SessionId,
        /// New title, as [`crate::clean_title`] accepts it; stored trimmed.
        title: String,
    },
    /// Generate the session's title again from its conversation so far, replacing any title,
    /// a user's too. Accepted once the generation is started; the new title arrives as a
    /// `title_changed` with source `ai_requested`. Archived and moved sessions refuse it.
    RetitleSession {
        /// Target session.
        session_id: SessionId,
    },
    /// Start a turn with a prompt.
    SendPrompt {
        /// Target session.
        session_id: SessionId,
        /// Prompt text.
        text: String,
        /// Images for the agent to see with the text, together at most
        /// [`crate::MAX_PROMPT_IMAGE_BYTES`]; a provider that cannot take images refuses them
        /// as `unsupported`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<Image>,
    },
    /// Fetch the bytes of an image a prompt of the session carried; answered with
    /// `attachment`. It changes nothing, so a resend is answered afresh.
    GetAttachment {
        /// Session whose prompt carried it.
        session_id: SessionId,
        /// The image, as the prompt's `user_message` names it.
        attachment_id: AttachmentId,
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
    /// Stop and remove the containers and networks of a Compose project among the session's
    /// tracked containers; owners only.
    ComposeDown {
        /// Target session.
        session_id: SessionId,
        /// Compose project of one of the session's containers.
        project: String,
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
    /// Add an account: run the provider's own login in a fresh config dir, in a login terminal
    /// this connection is attached to; owners only. The account joins the account list once
    /// the login exits successfully.
    AddAccount {
        /// Id of the new account; unique on this daemon.
        account_id: AccountId,
        /// Provider to log in to.
        provider: Provider,
        /// Display label; the id when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// Absolute config dir on the host, or one starting with `~/`; the daemon picks one
        /// in the home directory when absent. It must not hold a login yet.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config_dir: Option<String>,
        /// Width in columns.
        cols: u16,
        /// Height in rows.
        rows: u16,
    },
    /// List a folder on the host, to pick a repository; owners only. Answered with
    /// `directory`. It changes nothing, so a resend is answered afresh.
    ListDirectory {
        /// Absolute path of the folder, or one starting with `~/`.
        path: String,
    },
    /// Register a repository on the host as a project, as a `[[project]]` entry of the
    /// daemon's config declaring its path; owners only. Answered with `project_added`. A path
    /// declared already is answered with its project.
    AddProject {
        /// Absolute path of the repository, or one starting with `~/`.
        path: String,
    },
    /// Replace a project's settings on this host, kept in its `[[project]]` entry of the
    /// daemon's config; owners only. An absent setting is cleared.
    SetProjectSettings {
        /// The project, one of this daemon's.
        project_id: ProjectId,
        /// Permission mode new sessions of the project start in when none is given.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default_permission_mode: Option<PermissionMode>,
        /// Account new sessions of the project use when none is given.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default_account: Option<AccountId>,
        /// Shell command run in each new worktree of the project before its session starts.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        setup_command: Option<String>,
    },
    /// Stop managing a project on this host; owners only. Answered with `applied`; the
    /// project list without it follows. Its clones leave the daemon's `[[project]]` entries
    /// and are excluded from discovery, until `add_project` adds one again; nothing on disk is
    /// deleted. A project with live (not archived) sessions is refused with `conflict`;
    /// archived ones keep existing without a project.
    RemoveProject {
        /// The project, one of this daemon's.
        project_id: ProjectId,
    },
    /// Fetch a project's icon, the file its `icon` names; owners and members alike. Answered
    /// with `project_icon`, or refused with `not_found` when the project has none. It changes
    /// nothing, so a resend is answered afresh.
    GetProjectIcon {
        /// The project, one of this daemon's.
        project_id: ProjectId,
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
