//! Messages the daemon sends to a client.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AccountId, Bytes, CommandId, DeviceId, Event, HostId, Item, ItemId, Provider, Seq, SessionId,
    TerminalId, UserId,
};

/// A daemon-to-client message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// First message on every connection, answering the client's hello.
    Hello(ServerHello),
    /// Every session on this daemon; sent after hello and whenever a session is created.
    Sessions {
        /// Sessions with their latest seq.
        sessions: Vec<SessionHead>,
    },
    /// Every account on this daemon; sent after hello and whenever the set changes.
    Accounts {
        /// The accounts.
        accounts: Vec<Account>,
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

/// A user's role on a daemon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Full control, including terminals.
    Owner,
    /// Drives sessions; never sees or opens a terminal.
    Member,
}

/// A session and the seq of its latest event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionHead {
    /// The session.
    pub session_id: SessionId,
    /// Seq of its latest event.
    pub head_seq: Seq,
}

/// A provider login on this host, used through its own config dir.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Account {
    /// The account.
    pub account_id: AccountId,
    /// Provider the account belongs to.
    pub provider: Provider,
    /// Display label chosen by the owner.
    pub label: String,
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
    /// A terminal was opened and this connection attached to it.
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
    /// A referenced session, account, approval or terminal does not exist.
    NotFound,
    /// Not possible in the current state, e.g. a prompt while a turn runs.
    Conflict,
    /// The provider cannot do it.
    Unsupported,
    /// The daemon failed.
    Internal,
}
