//! Wire and protocol types shared by every herder component, and the JSON Schema generated from them.
//!
//! One WebSocket carries JSON text frames: [`ClientMessage`] from client to daemon and
//! [`ServerMessage`] from daemon to client. Both are internally tagged on `"type"`.
//!
//! Evolution rules (protocol version 4):
//! - Adding a variant, or an optional field, is compatible and keeps [`PROTOCOL_VERSION`].
//! - Renaming or removing anything, or changing a field's type, bumps [`PROTOCOL_VERSION`].
//! - Receivers ignore unknown fields. Enums that grow over time ([`ServerMessage`],
//!   [`EventBody`], [`ItemBody`], [`SessionStatus`]) decode unknown tags to an `Unknown` variant
//!   that is absent from the schema and never sent; [`Provider`] keeps unknown names verbatim.

mod attachment;
mod bytes;
mod client;
mod event;
mod ids;
mod project;
pub mod replication;
mod resources;
mod server;
mod types;

pub use attachment::{
    Attachment, IMAGE_MEDIA_TYPES, Image, MAX_IMAGE_BYTES, MAX_PROMPT_IMAGE_BYTES,
};
pub use bytes::Bytes;
pub use client::{ClientHello, ClientMessage, Command, CommandBody, Cursor};
pub use event::{
    Answer, Answerer, ApprovalDecision, ApprovalOutcome, CiStatus, ErrorClass, EscalationReason,
    Event, EventBody, Item, ItemBody, Mergeable, PrState, PullRequest, ReviewStatus, Route,
    SessionStatus, TurnError,
};
pub use ids::{
    AccountId, ApprovalId, AttachmentId, CommandId, DeviceId, HostId, ItemId, QuestionId,
    SessionId, TerminalId, TurnId, UserId,
};
pub use project::{Project, ProjectId};
pub use replication::{
    AttachmentData, Batch, HostHello, HostMessage, JournalRecord, MAX_BATCH_EVENTS,
    REPLICATION_VERSION, RawEventBody, RejectReason, ReplicationError, ReplicationErrorCode,
    SessionSummary, VaultHello, VaultMessage,
};
pub use resources::{Constraint, Container, ContainerState, HostResources, Pressure, SessionUsage};
pub use server::{
    Account, CommandResult, DirectoryEntry, ErrorCode, ErrorInfo, FailoverSettings, FleetHost,
    Role, ServerHello, ServerMessage, SessionHead, Terminal, TerminalPurpose, UsageWindow,
};
pub use types::{PermissionMode, Provider};

/// Wire protocol version, exchanged in both hellos; peers with different versions disconnect.
pub const PROTOCOL_VERSION: u32 = 4;

/// Per-session sequence number of a durable event: starts at 1 and increases by 1 per event.
pub type Seq = u64;

/// Point in time on the wire, an RFC 3339 UTC timestamp.
pub type Timestamp = jiff::Timestamp;

/// JSON Schema for every message a client sends.
pub fn client_schema() -> schemars::Schema {
    schemars::schema_for!(ClientMessage)
}

/// JSON Schema for every message the daemon sends.
pub fn server_schema() -> schemars::Schema {
    schemars::schema_for!(ServerMessage)
}

/// JSON Schema for every message a host sends the vault.
pub fn host_schema() -> schemars::Schema {
    schemars::schema_for!(HostMessage)
}

/// JSON Schema for every message the vault sends a host.
pub fn vault_schema() -> schemars::Schema {
    schemars::schema_for!(VaultMessage)
}
