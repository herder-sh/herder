//! Replication: a host streams its session journals to the vault, which keeps them durable.
//!
//! The vault is the same `herder` binary in vault mode. A host's daemon connects to it the way
//! a client connects to a daemon: one TLS WebSocket with a pinned certificate, paired once with
//! a one-time code, carrying JSON text frames. [`HostMessage`] goes from host to vault and
//! [`VaultMessage`] from vault to host; both are internally tagged on `"type"`.
//!
//! The exchange:
//! 1. The host sends [`HostMessage::Hello`]. The vault answers [`VaultMessage::Hello`] with,
//!    for every session of that host it holds, the last seq it holds durably. Those cursors are
//!    the only source of truth for where to resume: the host keeps no cursor of its own, so an
//!    ack lost in flight, or a vault restored from an older backup, never causes a gap.
//! 2. The host sends a [`HostMessage::Session`] for each of its sessions, then, per session,
//!    [`HostMessage::Batch`]es of the events after the vault's cursor, and from then on each
//!    new event as it is appended. The host may send further batches before earlier ones are
//!    acknowledged.
//! 3. The vault handles messages in order. It acknowledges a batch only once its events are
//!    durable, with a cumulative [`VaultMessage::Ack`].
//!
//! Images a prompt carried travel ahead of the events that name them: before each batch, the
//! host sends a [`HostMessage::Attachment`] for every attachment of a `user_message` in it.
//! The vault keeps each durably before it handles the next message, so the batch's ack covers
//! the images too, and a batch re-sent after a reconnect brings them again. Re-sending one is
//! idempotent: the vault holds one image per session and attachment id, and skips a re-send
//! with the same content hash. Only image bytes the host still has are sent.
//!
//! Re-sending is idempotent: events at seqs the vault already holds are compared with what it
//! holds and, when equal, skipped and acknowledged again. A batch that leaves a gap, or that
//! holds a different event at a seq the vault already has, changes nothing and is answered
//! with [`VaultMessage::Rejected`].
//!
//! The vault never writes to a host's sessions: [`VaultMessage`] carries no events and no
//! commands, only acknowledgements and errors. Only durable journal events and the images
//! they name are replicated; deltas, snapshots, terminals and resource usage never are.
//!
//! Events travel as stored ([`JournalRecord`]), with the body as raw JSON, so the vault keeps
//! event types newer than its own build intact instead of decoding them to
//! [`EventBody::Unknown`].
//!
//! Evolution follows the same rules as the client protocol, under its own
//! [`REPLICATION_VERSION`]; peers with different versions disconnect after the hellos.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Map, Value};

use crate::{
    Attachment, Bytes, Cursor, Event, EventBody, HostId, ProjectId, PullRequest, Seq, SessionId,
    SessionStatus, Timestamp, UserId,
};

/// Replication protocol version, exchanged in both hellos; peers with different versions
/// disconnect.
pub const REPLICATION_VERSION: u32 = 1;

/// Most events one [`Batch`] may hold; the vault refuses a larger one as a bad request.
pub const MAX_BATCH_EVENTS: usize = 256;

/// A host-to-vault message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMessage {
    /// First message on every connection.
    Hello(HostHello),
    /// A session's current state for the fleet index; sent for every session after the vault's
    /// hello, then whenever any of it changes. Replaces what the vault had for the session.
    Session(SessionSummary),
    /// Consecutive journal events of one session.
    Batch(Batch),
    /// An image a `user_message` of the next batch names; sent just before that batch.
    Attachment(AttachmentData),
    /// A message type newer than this build; skip it.
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Unknown,
}

/// Opening message of a host connection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HostHello {
    /// Must equal the vault's [`REPLICATION_VERSION`].
    pub replication_version: u32,
    /// The host; must be the host this connection's device was paired as.
    pub host_id: HostId,
    /// Display name of the host.
    pub host_name: String,
    /// Build name and version, for logs, e.g. `herder/0.1.0`.
    pub build: String,
    /// One-time code from the vault's pairing, sent by a host that is not paired yet; ignored
    /// once it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_code: Option<String>,
}

/// A session as the fleet index lists it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionSummary {
    /// The session.
    pub session_id: SessionId,
    /// Project of the session's repository, as resolved under the host's current config.
    pub project_id: ProjectId,
    /// Absolute path of the repository on the host.
    pub repo: String,
    /// Branch the session's worktree has checked out, or had when it was removed.
    pub branch: String,
    /// Current status.
    pub status: SessionStatus,
    /// Every pull request tracked for the session.
    pub prs: Vec<PullRequest>,
    /// Primary session of the task this session is a child of; absent for a top-level session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SessionId>,
    /// Short label of the session's task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The session's current title; absent until it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Seq of the session's latest event on the host; the vault is caught up once it holds it.
    pub head_seq: Seq,
    /// `at` of the session's latest event.
    pub updated_at: Timestamp,
}

/// Events of one session with consecutive seqs, oldest first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Batch {
    /// Session whose journal holds the events.
    pub session_id: SessionId,
    /// One to [`MAX_BATCH_EVENTS`] events, each seq one more than the one before.
    pub events: Vec<JournalRecord>,
}

/// The bytes of an image a prompt of a session carried.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AttachmentData {
    /// Session whose prompt carried the image.
    pub session_id: SessionId,
    /// The image as the `user_message` names it. The vault refuses it as a bad request unless
    /// `media_type` is one of [`crate::IMAGE_MEDIA_TYPES`] and `size` is the length of `data`,
    /// at most [`crate::MAX_IMAGE_BYTES`], or when it holds different bytes under its id.
    pub attachment: Attachment,
    /// The image file's bytes.
    pub data: Bytes,
}

/// One event of a session's journal, exactly as the host stored it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct JournalRecord {
    /// Position in the session's journal, gap-free from 1.
    pub seq: Seq,
    /// When the host recorded the event.
    pub at: Timestamp,
    /// User whose command caused the event; absent when the agent or the daemon caused it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<UserId>,
    /// What happened, as stored.
    pub body: RawEventBody,
}

impl JournalRecord {
    /// The record of a stored event; fails for an `Unknown` body, which has no stored form here.
    pub fn from_event(event: &Event) -> Result<Self, serde_json::Error> {
        Ok(Self {
            seq: event.seq,
            at: event.at,
            by: event.by.clone(),
            body: RawEventBody::encode(&event.body)?,
        })
    }

    /// The event of `session_id` this record holds; its body is `Unknown` when this build
    /// cannot decode it.
    pub fn to_event(&self, session_id: SessionId) -> Event {
        Event {
            session_id,
            seq: self.seq,
            at: self.at,
            by: self.by.clone(),
            body: self.body.decode(),
        }
    }
}

/// An event body as raw JSON: an object with a string `type`, kept verbatim whether or not
/// this build knows the type.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(transparent)]
pub struct RawEventBody(Map<String, Value>);

impl RawEventBody {
    /// The stored form of a body; fails for `Unknown`.
    pub fn encode(body: &EventBody) -> Result<Self, serde_json::Error> {
        Self::from_value(serde_json::to_value(body)?)
    }

    /// A body read back from storage; fails unless it is an object with a string `type`.
    pub fn from_value(value: Value) -> Result<Self, serde_json::Error> {
        match value {
            Value::Object(map) if map.get("type").is_some_and(Value::is_string) => Ok(Self(map)),
            _ => Err(de::Error::custom(
                "event body must be an object with a string `type`",
            )),
        }
    }

    /// The body's `type` tag, such as `item_added`.
    pub fn event_type(&self) -> &str {
        // Every constructor checks that `type` is a string.
        self.0
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The body decoded by this build: `Unknown` for a type it does not know, or a known type
    /// whose shape changed, as the store reads it.
    pub fn decode(&self) -> EventBody {
        serde_json::from_value(Value::Object(self.0.clone())).unwrap_or(EventBody::Unknown)
    }

    /// The body as JSON.
    pub fn as_json(&self) -> &Map<String, Value> {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RawEventBody {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_value(Value::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for RawEventBody {
    fn schema_name() -> Cow<'static, str> {
        "RawEventBody".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "An event body exactly as the host stored it: an `EventBody` of the \
                            client protocol (see `server_message.json`), or a type newer than \
                            the receiver, kept verbatim.",
            "type": "object",
            "required": ["type"],
            "properties": { "type": { "type": "string" } }
        })
    }
}

/// A vault-to-host message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VaultMessage {
    /// Answer to the host's hello.
    Hello(VaultHello),
    /// Every event of the session up to and including `after_seq` is durable on the vault.
    /// Cumulative: a later ack covers every earlier one.
    Ack(Cursor),
    /// A batch was refused and changed nothing; `cursor` is the last seq the vault holds.
    Rejected {
        /// The session and the last seq the vault holds of it.
        cursor: Cursor,
        /// Why the batch was refused.
        reason: RejectReason,
    },
    /// The connection failed, such as a bad hello or a malformed frame; the vault closes it.
    Error {
        /// What went wrong.
        error: ReplicationError,
    },
    /// A message type newer than this build; skip it.
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Unknown,
}

/// The vault's answer to a host's hello.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VaultHello {
    /// The vault's [`REPLICATION_VERSION`]; on a mismatch both sides disconnect.
    pub replication_version: u32,
    /// Build name and version, for logs, e.g. `herder/0.1.0`.
    pub build: String,
    /// For every session of this host the vault holds, the last seq it holds durably. A session
    /// not listed has nothing on the vault yet. The host resumes each session right after its
    /// cursor here. Empty when the replication versions differ.
    ///
    /// A cursor beyond the host's own latest seq means the host lost events the vault holds,
    /// such as after restoring an older backup; its next events at those seqs would conflict,
    /// so the host does not replicate that session until someone resolves it.
    pub acked: Vec<Cursor>,
}

/// Why the vault refused a batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    /// The batch starts after the seq following the vault's cursor; re-send from that seq.
    Gap,
    /// The batch holds an event at a seq the vault already has, and it differs from what the
    /// vault has. The host's journal diverged from what it replicated earlier; stop
    /// replicating the session until someone resolves it.
    Conflict,
}

/// A connection-level failure reported to the host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReplicationError {
    /// What kind of failure.
    pub code: ReplicationErrorCode,
    /// Human-readable detail.
    pub message: String,
}

/// Kind of connection-level failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReplicationErrorCode {
    /// The message was malformed or invalid, or came before the hello.
    BadRequest,
    /// The device is not paired, or the hello names a host it was not paired as.
    Forbidden,
    /// The vault failed.
    Internal,
}
