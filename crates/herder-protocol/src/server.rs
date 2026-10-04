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
    /// its status, account, project or title changes.
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
    /// What a vault holds and how each host's replication stands; sent by a vault only, after
    /// hello and whenever it changes, at most once every two seconds.
    VaultStatus(VaultStatus),
    /// Every project with a clone on this daemon's host; sent after hello and whenever any of
    /// it changes. Clients merge the lists of all their daemons by `project_id`.
    Projects {
        /// The projects.
        projects: Vec<Project>,
    },
    /// Every account on this daemon with its usage; sent after hello and whenever any of it changes.
    Accounts {
        /// The accounts; a session whose account hits a limit may rotate to any other of its
        /// provider.
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
    /// What the host's copies take on the vault; absent from a vault that does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<HostUsage>,
}

/// What one host's copies take on the vault.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HostUsage {
    /// Sessions of the host the vault holds.
    pub sessions: u32,
    /// Bytes of the host's images the vault holds.
    pub attachment_bytes: u64,
    /// Most bytes of images the vault keeps for the host; absent when the host backs up no
    /// images.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments_cap: Option<u64>,
}

/// The disk the vault keeps its database on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VaultVolume {
    /// Size of the volume, in bytes.
    pub total_bytes: u64,
    /// Bytes in use, by the vault or anything else.
    pub used_bytes: u64,
}

impl VaultVolume {
    /// Share of the volume in use above which clients warn that the vault is filling up.
    pub const WARN_RATIO: f64 = 0.8;

    /// Share of the volume in use, from 0 to 1.
    pub fn used_ratio(&self) -> f64 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        self.used_bytes as f64 / self.total_bytes as f64
    }

    /// Whether more than [`Self::WARN_RATIO`] of the volume is in use.
    pub fn nearly_full(&self) -> bool {
        self.used_ratio() > Self::WARN_RATIO
    }
}

/// What a vault holds, in total and per host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VaultStatus {
    /// Sessions the vault lists: every host's, without copies another host took over.
    pub sessions: u64,
    /// Journal events held, of every copy.
    pub events: u64,
    /// Size of the vault's database, in bytes.
    pub storage_bytes: u64,
    /// Each host that replicated here, ordered by host id; the same hosts as
    /// [`ServerMessage::Hosts`].
    pub hosts: Vec<HostReplication>,
}

/// How far one host's replication to a vault got.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HostReplication {
    /// The host.
    pub host_id: HostId,
    /// Sessions of the host held, without copies another host took over.
    pub sessions: u64,
    /// Journal events of the host held.
    pub events: u64,
    /// When the newest event held of the host happened, by the host's clock; absent until one
    /// is held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_at: Option<Timestamp>,
    /// How far the vault was behind the host when it last stored a batch from it: the age of
    /// that batch's newest event, in milliseconds. Absent until the host sent a batch since
    /// the vault started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lag_ms: Option<u64>,
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
    /// The session's current title, from its latest `title_changed`; absent until it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
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
    /// Config directory on the host; absent for the provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<String>,
    /// Every limit window the provider last reported; empty until it reports one.
    pub usage: Vec<UsageWindow>,
}

/// How a daemon's sessions fail over when their account hits a limit: they rotate to the
/// available account of their provider with the most room left, unless pinned.
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
    /// The bytes of an image, answering `get_attachment`.
    Attachment {
        /// The image's media type.
        media_type: String,
        /// The image file's bytes.
        data: Bytes,
    },
    /// A folder's entries, answering `list_directory`.
    Directory {
        /// The folder, as an absolute path with `~` expanded.
        path: String,
        /// Its entries, ordered by name.
        entries: Vec<DirectoryEntry>,
    },
    /// A session was forked onto this daemon's host, answering `fork_session`; the fork is in
    /// the session list.
    SessionForked {
        /// The new session.
        session_id: SessionId,
        /// The account it runs on.
        account_id: AccountId,
        /// The session it was forked from.
        forked_from: SessionId,
        /// The host that session ran on.
        from_host_id: HostId,
    },
    /// A repository is a project of this daemon, answering `add_project`; the project list
    /// with it follows.
    ProjectAdded {
        /// The project the repository belongs to.
        project_id: ProjectId,
    },
    /// A project's icon, answering `get_project_icon`.
    ProjectIcon {
        /// The SHA-256 of `data` as lowercase hex, as a project's `icon` names it.
        icon: String,
        /// One of [`crate::PROJECT_ICON_MEDIA_TYPES`].
        media_type: String,
        /// The image file's bytes, at most [`crate::MAX_PROJECT_ICON_BYTES`].
        data: Bytes,
    },
    /// Where the daemon backs its sessions up, answering `get_vault_link`.
    VaultLink {
        /// Whether the daemon is a vault, which hosts back up to.
        is_vault: bool,
        /// The vault the daemon backs up to; absent when it backs up nowhere, as a vault
        /// never does.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vault: Option<LinkedVault>,
        /// How full a vault's disk is, as of the answer; absent from a daemon, and when the
        /// vault cannot tell.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        volume: Option<VaultVolume>,
    },
    /// A one-time code that pairs another device as the caller's user, answering
    /// `pair_device`: everything a `herder://pair` link names for this daemon.
    DevicePairing {
        /// The code, for the new device's hello.
        code: String,
        /// SHA-256 of the daemon's TLS certificate, lowercase hex.
        fingerprint: String,
        /// Addresses the daemon advertises as `host:port`, most likely reachable first; not
        /// necessarily the one the caller reached it on.
        addresses: Vec<String>,
        /// When the code stops working.
        expires_at: Timestamp,
    },
    /// A one-time code that pairs a host with this vault to replicate and only that,
    /// answering `pair_vault_host`.
    HostPairing {
        /// The code, for `link_vault`.
        code: String,
        /// When the code stops working.
        expires_at: Timestamp,
    },
}

/// The vault a host backs up to, as its `[vault]` table names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LinkedVault {
    /// The vault's address, as `host:port`.
    pub address: String,
    /// SHA-256 of the vault's TLS certificate, lowercase hex.
    pub fingerprint: String,
}

/// One entry of a folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DirectoryEntry {
    /// File name of the entry.
    pub name: String,
    /// Whether it is a folder, following symlinks.
    pub is_dir: bool,
    /// Whether it is a folder at the top of a git repository or worktree.
    pub is_repo: bool,
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
