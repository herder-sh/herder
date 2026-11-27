//! Replication: a host streams its session journals to the hub, which keeps them durable.
//!
//! The hub is the same `herder` binary in hub mode. A host's daemon connects to it the way
//! a client connects to a daemon: one TLS WebSocket with a pinned certificate, paired once with
//! a one-time code, carrying JSON text frames. [`HostMessage`] goes from host to hub and
//! [`HubMessage`] from hub to host; both are internally tagged on `"type"`.
//!
//! The exchange:
//! 1. The host sends [`HostMessage::Hello`]. The hub answers [`HubMessage::Hello`] with,
//!    for every session of that host it holds, the last seq it holds durably. Those cursors are
//!    the only source of truth for where to resume: the host keeps no cursor of its own, so an
//!    ack lost in flight, or a hub restored from an older backup, never causes a gap.
//! 2. The host sends a [`HostMessage::Session`] for each of its sessions, then, per session,
//!    [`HostMessage::Batch`]es of the events after the hub's cursor, and from then on each
//!    new event as it is appended. The host may send further batches before earlier ones are
//!    acknowledged.
//! 3. The hub handles messages in order. It acknowledges a batch only once its events are
//!    durable, with a cumulative [`HubMessage::Ack`].
//!
//! The images and files prompts carried, their attachments, travel ahead of the events that
//! name them: before each batch, the host sends a [`HostMessage::Attachment`] for every
//! attachment of a `user_message` in it. The hub keeps each durably before it handles the
//! next message, so the batch's ack covers the attachments too, and a batch re-sent after a
//! reconnect brings them again. Re-sending one is idempotent: the hub holds one attachment
//! per session and attachment id, and skips a re-send with the same content hash. Only bytes
//! the host still has are sent.
//!
//! Attachments are backed up only when the host's hello gives an `attachments_cap`; without
//! one the host sends none, and the hub drops any it gets. Within the cap the hub evicts
//! the host's oldest attachments to make room for a new one, and drops one bigger than the
//! cap. Neither fails the connection: a session whose attachments are not held is recovered
//! without them.
//!
//! The hub drops archived sessions after its retention period. Its hello still gives a
//! cursor for each, at the seq it held, so the host does not send it again; it takes the
//! session back once the host's summary shows it unarchived.
//!
//! Re-sending is idempotent: events at seqs the hub already holds are compared with what it
//! holds and, when equal, skipped and acknowledged again. A batch that leaves a gap, or that
//! holds a different event at a seq the hub already has, changes nothing and is answered
//! with [`HubMessage::Rejected`].
//!
//! The hub never writes to a host's sessions: [`HubMessage`] carries no events and no
//! commands, only acknowledgements, errors, answers to the host's secret requests and
//! requests to open a relay stream, whose bytes the hub cannot read. Only durable journal events and the images
//! they name are replicated; deltas, snapshots, terminals and resource usage never are.
//!
//! Events travel as stored ([`JournalRecord`]), with the body as raw JSON, so the hub keeps
//! event types newer than its own build intact instead of decoding them to
//! [`EventBody::Unknown`].
//!
//! Secrets (see [`crate::SecretInfo`]) travel over the same connection, on the host's request
//! only: [`HostMessage::ListSecrets`], [`HostMessage::FetchSecrets`] and
//! [`HostMessage::FetchHostSecrets`], each answered once,
//! under its `request_id`, in any order relative to acks. The hub answers for the sets
//! granted to this connection's host, and nothing else.
//!
//! The connection also carries the hub's requests for relay streams: a host whose hello sets
//! `relay` is sent a [`HubMessage::RelayOpen`] whenever a client asks the hub for a stream
//! to it, and answers by opening a connection of its own to the hub (see [`crate::relay`]).
//! The stream's bytes never travel on this connection, so a long replay to a client cannot
//! hold up replication, nor replication a client.
//!
//! Evolution follows the same rules as the client protocol, under its own
//! [`REPLICATION_VERSION`]; peers with different versions disconnect after the hellos.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Map, Value};

use crate::{
    Attachment, Bytes, Cursor, DeviceId, Event, EventBody, FetchedSecret, HostId, ProjectId,
    PullRequest, SecretError, SecretInfo, SecretRef, Seq, SessionId, SessionStatus, Timestamp,
    UserId,
};

/// Replication protocol version, exchanged in both hellos; peers with different versions
/// disconnect.
pub const REPLICATION_VERSION: u32 = 2;

/// Most events one [`Batch`] may hold; the hub refuses a larger one as a bad request.
pub const MAX_BATCH_EVENTS: usize = 256;

/// A host-to-hub message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMessage {
    /// First message on every connection.
    Hello(HostHello),
    /// A session's current state for the fleet index; sent for every session after the hub's
    /// hello, then whenever any of it changes. Replaces what the hub had for the session.
    Session(SessionSummary),
    /// Consecutive journal events of one session.
    Batch(Batch),
    /// An image a `user_message` of the next batch names; sent just before that batch.
    Attachment(AttachmentData),
    /// List the secrets in the sets granted to this host, without values; answered with
    /// [`HubMessage::SecretList`].
    ListSecrets {
        /// Chosen by the host to match the answer.
        request_id: u64,
    },
    /// Fetch secrets' values for one of this host's sessions; answered with
    /// [`HubMessage::SecretValues`], or [`HubMessage::SecretsRefused`] when any of them
    /// cannot be handed out. The hub records each value it hands out.
    FetchSecrets(SecretFetch),
    /// Fetch secrets' values for this host's own use, from the sets granted to it for that;
    /// answered as [`HostMessage::FetchSecrets`] is. A hub older than this message skips it,
    /// so the host hears nothing back.
    FetchHostSecrets(HostSecretFetch),
    /// A message type newer than this build; skip it.
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Unknown,
}

/// Opening message of a host connection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HostHello {
    /// Must equal the hub's [`REPLICATION_VERSION`].
    pub replication_version: u32,
    /// The host; must be the host this connection's device was paired as.
    pub host_id: HostId,
    /// Display name of the host.
    pub host_name: String,
    /// Build name and version, for logs, e.g. `herder/0.1.0`.
    pub build: String,
    /// One-time code from the hub's pairing, sent by a host that is not paired yet; ignored
    /// once it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_code: Option<String>,
    /// Most bytes of this host's images the hub keeps, oldest evicted first to make room
    /// for a new one; absent when the host backs up no images, so the hub keeps none it
    /// sends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments_cap: Option<u64>,
    /// Whether the host takes relayed connections: it opens a relay stream for every
    /// [`HubMessage::RelayOpen`], and serves the client in it as on a direct connection.
    /// Absent from a host that takes none, which the hub sends no such message.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub relay: bool,
    /// Whether the host takes streams through the hub's Durable Object relay too, when the
    /// hub has one: the hub then sends it [`HubHello::relay_grant`] and
    /// [`HubMessage::RelayGrant`]. Absent from a host that does not.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cloudflare_relay: bool,
}

/// A session as the fleet index lists it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionSummary {
    /// The session.
    pub session_id: SessionId,
    /// Project of the session's repository, as resolved under the host's current config; for
    /// a chat, the local project id of its folder, which no project has.
    pub project_id: ProjectId,
    /// Whether the session is a chat, about no project.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub chat: bool,
    /// Absolute path of the repository on the host.
    pub repo: String,
    /// Branch the session's worktree has checked out, or had when it was removed; absent for
    /// a session that works in its folder itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Current status.
    pub status: SessionStatus,
    /// Every pull request tracked for the session.
    pub prs: Vec<PullRequest>,
    /// Primary session of the task this session is a child of; absent for a top-level session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SessionId>,
    /// Host the parent runs on when it lives on another machine; absent when the parent, if
    /// any, is a session of the same host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_host: Option<HostId>,
    /// Short label of the session's task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The session's current title; absent until it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Seq of the session's latest event on the host; the hub is caught up once it holds it.
    pub head_seq: Seq,
    /// `at` of the session's latest event.
    #[schemars(with = "crate::TimestampSchema")]
    pub updated_at: Timestamp,
    /// The bot that started the session; absent when the apps or an agent did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<crate::SessionOrigin>,
}

/// Events of one session with consecutive seqs, oldest first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Batch {
    /// Session whose journal holds the events.
    pub session_id: SessionId,
    /// One to [`MAX_BATCH_EVENTS`] events, each seq one more than the one before.
    pub events: Vec<JournalRecord>,
}

/// The bytes of an image or file a prompt of a session carried.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AttachmentData {
    /// Session whose prompt carried it.
    pub session_id: SessionId,
    /// The image or file as the `user_message` names it. The hub refuses it as a bad request
    /// unless `size` is the length of `data` and, for an image, `media_type` is one of
    /// [`crate::IMAGE_MEDIA_TYPES`] and `size` at most [`crate::MAX_IMAGE_BYTES`], for a file,
    /// `size` is at most [`crate::MAX_FILE_BYTES`]; or when it holds different bytes under its
    /// id.
    pub attachment: Attachment,
    /// The image's or file's bytes.
    pub data: Bytes,
}

/// A host's request for secrets' values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SecretFetch {
    /// Chosen by the host to match the answer.
    pub request_id: u64,
    /// The session of this host the values are for, as the hub's audit records it.
    pub session_id: SessionId,
    /// The secrets, as the session named them; at least one.
    pub secrets: Vec<SecretRef>,
}

/// A host's request for secrets' values for its own use.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HostSecretFetch {
    /// Chosen by the host to match the answer.
    pub request_id: u64,
    /// What the values are for, such as `bot:trip1-team`, as the hub's audit records it;
    /// refused unless [`crate::is_valid_secret_purpose`] accepts it.
    pub purpose: String,
    /// The secrets, named as a session would name them among the sets granted for host use;
    /// at least one.
    pub secrets: Vec<SecretRef>,
}

/// One event of a session's journal, exactly as the host stored it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct JournalRecord {
    /// Position in the session's journal, gap-free from 1.
    pub seq: Seq,
    /// When the host recorded the event.
    #[schemars(with = "crate::TimestampSchema")]
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

/// A hub-to-host message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubMessage {
    /// Answer to the host's hello.
    Hello(HubHello),
    /// Every event of the session up to and including `after_seq` is durable on the hub.
    /// Cumulative: a later ack covers every earlier one.
    Ack(Cursor),
    /// A batch was refused and changed nothing; `cursor` is the last seq the hub holds.
    Rejected {
        /// The session and the last seq the hub holds of it.
        cursor: Cursor,
        /// Why the batch was refused.
        reason: RejectReason,
    },
    /// The connection failed, such as a bad hello or a malformed frame; the hub closes it.
    Error {
        /// What went wrong.
        error: ReplicationError,
    },
    /// The secrets in the sets granted to this host, answering [`HostMessage::ListSecrets`].
    SecretList {
        /// The request's id.
        request_id: u64,
        /// The secrets, without values, ordered by set, then name.
        secrets: Vec<SecretInfo>,
    },
    /// Every secret a [`HostMessage::FetchSecrets`] or [`HostMessage::FetchHostSecrets`] asked
    /// for, with its value.
    SecretValues {
        /// The request's id.
        request_id: u64,
        /// One per secret asked for, in the order asked.
        secrets: Vec<FetchedSecret>,
    },
    /// A [`HostMessage::FetchSecrets`] or [`HostMessage::FetchHostSecrets`] was refused and
    /// handed nothing out.
    SecretsRefused {
        /// The request's id.
        request_id: u64,
        /// Why.
        error: SecretError,
    },
    /// Every provider login of the hub's CLIProxyAPI, ordered by provider, then label; sent
    /// right after the hello and whenever one changes, only to a host the hub told where the
    /// proxy is (`HubHello.cliproxyapi`). Each is an account on the host, run through the
    /// proxy pinned to it; one not listed any more is no longer an account.
    ProviderLogins {
        /// The logins.
        logins: Vec<crate::ProviderLogin>,
    },
    /// A client asked the hub for a stream to this host: open a relay connection to the
    /// hub for [`crate::RelayRoute::Stream`] with this `stream_id`. Sent only to a host
    /// whose hello set `relay`. The hub waits a few seconds for it, then refuses the client;
    /// a host that will not take the stream just does not answer.
    RelayOpen {
        /// The stream; unique on this hub.
        stream_id: String,
        /// The hub's id for the device asking. The host uses it only to cap and log
        /// connections that have not authenticated yet: it knows the device by the
        /// certificate it presents inside the stream, never by this.
        device_id: DeviceId,
    },
    /// A fresh token for the hub's Durable Object relay, or `None` once the relay is gone;
    /// sent to a host whose hello set `cloudflare_relay`, before the one it holds expires and
    /// whenever the relay changes.
    RelayGrant {
        /// The relay and the token.
        grant: Option<crate::RelayGrant>,
    },
    /// A message type newer than this build; skip it.
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Unknown,
}

/// The hub's answer to a host's hello.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HubHello {
    /// The hub's [`REPLICATION_VERSION`]; on a mismatch both sides disconnect.
    pub replication_version: u32,
    /// Build name and version, for logs, e.g. `herder/0.1.0`.
    pub build: String,
    /// For every session of this host the hub holds, the last seq it holds durably. A session
    /// not listed has nothing on the hub yet. The host resumes each session right after its
    /// cursor here. Empty when the replication versions differ.
    ///
    /// A cursor beyond the host's own latest seq means the host lost events the hub holds,
    /// such as after restoring an older backup; its next events at those seqs would conflict,
    /// so the host does not replicate that session until someone resolves it.
    pub acked: Vec<Cursor>,
    /// Where this host reaches the hub's CLIProxyAPI, such as `https://cpa.example.com:8317`;
    /// sent only when the hub runs one and holds this host's client key, the secret
    /// `CLIPROXYAPI_KEY` granted to the host for its own use. An endpoint account
    /// with this `endpoint` and that `key_secret` runs on the hub's provider logins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cliproxyapi: Option<String>,
    /// Whether the hub forwards this host to its CLIProxyAPI over relay connections
    /// ([`crate::RelayRoute::Proxy`]); sent, without `cliproxyapi`, by a hub that runs one,
    /// holds this host's client key and is configured with no address hosts reach the proxy
    /// at. The host then points its CLIs at a listener of its own on loopback, which forwards
    /// each connection through the hub, and runs the hub's provider logins as it does with
    /// `cliproxyapi`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cliproxyapi_relay: bool,
    /// A token for the hub's Durable Object relay, sent to a host whose hello set
    /// `cloudflare_relay` while the relay is up: the host keeps a control WebSocket to it,
    /// `<url>/hosts/<host id>`, and opens `<url>/streams/<stream id>` for each stream it
    /// asks for (`docs/remote-access.md`). Renewed with [`HubMessage::RelayGrant`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_grant: Option<crate::RelayGrant>,
}

/// Why the hub refused a batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    /// The batch starts after the seq following the hub's cursor; re-send from that seq.
    Gap,
    /// The batch holds an event at a seq the hub already has, and it differs from what the
    /// hub has. The host's journal diverged from what it replicated earlier; stop
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
    /// The hub failed.
    Internal,
}
