//! Messages the daemon sends to a client.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AccountId, Bytes, CommandId, DeviceId, Event, HostId, HostResources, Item, ItemId, Project,
    ProjectId, Provider, Seq, SessionId, SessionStatus, SessionUsage, TerminalId, Timestamp,
    UserId,
};

/// A daemon-to-client message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// First message on every connection, answering the client's hello.
    Hello(ServerHello),
    /// Every session on this daemon; sent after hello and whenever a session is created or
    /// its status, account or project changes.
    Sessions {
        /// Sessions with their latest seq.
        sessions: Vec<SessionHead>,
    },
    /// Every host whose sessions a vault lists, with its liveness; sent by a vault only, after
    /// hello and whenever a host connects, disconnects or first replicates. A daemon never
    /// sends it: its sessions all run on its own host.
    Hosts {
        /// The hosts, ordered by host id.
        hosts: Vec<FleetHost>,
    },
    /// Every project with a clone on this daemon's host; sent after hello and whenever any of
    /// it changes. Clients merge the lists of all their daemons by `project_id`.
    Projects {
        /// The projects.
        projects: Vec<Project>,
    },
    /// Every account on this daemon with its usage; sent after hello and whenever any of it changes.
    Accounts {
        /// The accounts.
        accounts: Vec<Account>,
        /// How this daemon's sessions fail over.
        failover: FailoverSettings,
    },
    /// Every open terminal on this daemon; sent to owners only, after hello and whenever the set changes.
    Terminals {
        /// The open terminals.
        terminals: Vec<Terminal>,
    },
    /// The host's load and turn admission; ephemeral, never journaled. Sent after hello, then
    /// whenever it changes, at most once every two seconds.
    HostResources(HostResources),
    /// What one session's processes and containers use; ephemeral, never journaled. Sent after
    /// hello for every session with something running, then whenever its usage changes, at
    /// most once every two seconds per session.
    SessionResources {
        /// The session.
        session_id: SessionId,
        /// Its usage now.
        usage: SessionUsage,
    },
    /// A durable journal event of a subscribed session.
    Event(Event),
    /// The full current state of an in-progress item; later deltas apply on top of it.
    Snapshot {
        /// Session the item belongs to.
        session_id: SessionId,
        /// The item so far.
        item: Item,
    },
    /// Text appended to an in-progress item; ephemeral, never journaled.
    Delta {
        /// Session the item belongs to.
        session_id: SessionId,
        /// Item to append to, introduced by a snapshot.
        item_id: ItemId,
        /// Text to append to the item's text or output.
        text: String,
    },
    /// A terminal's shell exited; sent to owners only, before the terminal list without it.
    TerminalClosed {
        /// The terminal, now gone.
        terminal_id: TerminalId,
        /// The shell's exit status; absent when a signal ended it, as the hang-up on an archive
        /// or a daemon stop usually does.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
    /// Bytes a terminal wrote; ephemeral, never journaled.
    TerminalOutput {
        /// Terminal that wrote them.
        terminal_id: TerminalId,
        /// The bytes.
        data: Bytes,
    },
    /// A command was applied, or had already been applied under the same id.
    CommandAccepted {
        /// The command.
        command_id: CommandId,
        /// What it produced.
        result: CommandResult,
    },
    /// A command was refused and changed nothing.
    CommandRejected {
        /// The command.
        command_id: CommandId,
        /// Why.
        error: ErrorInfo,
    },
    /// A message that was not a command failed, such as a malformed frame or an unknown session.
    Error {
        /// What went wrong.
        error: ErrorInfo,
    },
    /// Answers a [`crate::ClientMessage::Sync`] once everything the daemon sent before it on
    /// this connection is out.
    Synced {
        /// The sync's token.
        token: String,
    },
    /// A message type newer than this build; skip it.
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Unknown,
}

/// Opening message of the daemon on a connection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ServerHello {
    /// The daemon's [`crate::PROTOCOL_VERSION`]; the client disconnects on a mismatch.
    pub protocol_version: u32,
    /// The host this daemon runs on.
    pub host_id: HostId,
    /// Display name of the host.
    pub host_name: String,
    /// The user this connection is authenticated as.
    pub user_id: UserId,
    /// The paired device this connection comes from.
    pub device_id: DeviceId,
    /// The user's role on this daemon.
    pub role: Role,
}

/// A host that replicates to a vault, as the vault lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FleetHost {
    /// The host.
    pub host_id: HostId,
    /// Display name of the host, from its latest replication hello.
    pub host_name: String,
    /// Whether the host's replication connection is open. A host silent for the vault's
    /// liveness timeout is offline; its sessions stay listed, read-only as all on a vault are.
    pub online: bool,
    /// When the vault last heard from the host; for an online host, as of when it connected
    /// or the list was last sent.
    pub last_seen: Timestamp,
}

/// A user's role on a daemon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Full control, including terminals.
    Owner,
    /// Drives sessions; never sees or opens a terminal.
    Member,
}

/// A session as lists show it, and the seq of its latest event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionHead {
    /// The session.
    pub session_id: SessionId,
    /// Host the session runs on, one of the vault's [`ServerMessage::Hosts`]; set by a vault
    /// only, as a daemon's sessions all run on its own host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_id: Option<HostId>,
    /// Seq of its latest event.
    pub head_seq: Seq,
    /// Where the session stands.
    pub status: SessionStatus,
    /// Primary session of the task this session is a child of; absent for a top-level session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SessionId>,
    /// Short label of the session's task, shown in the task tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// Project of the session's repository, as resolved under the daemon's current config;
    /// absent until the daemon's project discovery has seen the repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    /// Account the session runs on now.
    pub account_id: AccountId,
    /// How many of this session's children are `needs_you`; 0 for a child.
    pub children_need_you: u32,
}

/// A provider login on this host, used through its own config dir.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Account {
    /// The account.
    pub account_id: AccountId,
    /// Provider the account belongs to.
    pub provider: Provider,
    /// Display label chosen by the owner.
    pub label: String,
    /// Every limit window the provider last reported; empty until it reports one.
    pub usage: Vec<UsageWindow>,
    /// Whether sessions may fail over to this account when theirs hits a limit.
    pub failover: bool,
}

/// How a daemon's sessions fail over when their account hits a limit.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FailoverSettings {
    /// Whether sessions stay on their account by default; a session created with
    /// `failover_pin` overrides it.
    pub pin: bool,
}

/// Usage of one provider limit window, such as a five-hour or weekly limit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UsageWindow {
    /// Window name, in the provider's own naming.
    pub window: String,
    /// Share of the window's limit used, from 0 to 100.
    pub used_percent: f64,
    /// When the window resets; absent when the provider does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<Timestamp>,
}

/// An open terminal on this host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Terminal {
    /// The terminal.
    pub terminal_id: TerminalId,
    /// What the terminal runs.
    pub purpose: TerminalPurpose,
}

/// What a terminal runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalPurpose {
    /// A shell in a session's worktree.
    Shell {
        /// Session whose worktree the shell runs in.
        session_id: SessionId,
    },
    /// A provider's own login for an account being added.
    Login {
        /// The account being added.
        account_id: AccountId,
    },
}

/// What an accepted command produced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommandResult {
    /// Applied; the effects arrive as events.
    Applied,
    /// A session was created.
    SessionCreated {
        /// The new session.
        session_id: SessionId,
    },
    /// A terminal was opened, by `open_terminal` or `add_account`, and this connection
    /// attached to it.
    TerminalOpened {
        /// The new terminal.
        terminal_id: TerminalId,
    },
}

/// A failure reported to the client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorInfo {
    /// What kind of failure.
    pub code: ErrorCode,
    /// Human-readable detail.
    pub message: String,
}

/// Kind of failure reported to the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The message was malformed or invalid.
    BadRequest,
    /// The user's role does not allow it.
    Forbidden,
    /// A referenced session, account, approval, question or terminal does not exist.
    NotFound,
    /// Not possible in the current state, e.g. a prompt while a turn runs.
    Conflict,
    /// The provider cannot do it.
    Unsupported,
    /// The session is read-only here: a vault's copy of a session that runs on another host,
    /// which the message names.
    ReadOnly,
    /// The daemon failed.
    Internal,
}
