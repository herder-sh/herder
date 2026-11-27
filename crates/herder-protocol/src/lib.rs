//! Wire and protocol types shared by every herder component, and the JSON Schema generated from them.
//!
//! One WebSocket carries JSON text frames: [`ClientMessage`] from client to daemon and
//! [`ServerMessage`] from daemon to client. Both are internally tagged on `"type"`.
//!
//! Evolution rules (protocol version 5):
//! - Adding a variant, or an optional field, is compatible and keeps [`PROTOCOL_VERSION`].
//! - Renaming or removing anything, or changing a field's type, bumps [`PROTOCOL_VERSION`].
//! - Receivers ignore unknown fields. Enums that grow over time ([`ServerMessage`],
//!   [`EventBody`], [`ItemBody`], [`SessionStatus`]) decode unknown tags to an `Unknown` variant
//!   that is absent from the schema and never sent; [`Provider`] keeps unknown names verbatim.

mod attachment;
mod bots;
mod bytes;
mod client;
mod event;
mod ids;
mod people;
mod project;
mod provider_logins;
pub mod relay;
pub mod replication;
mod resources;
mod secrets;
mod server;
mod settings;
mod skills;
mod types;
mod usage;

pub use attachment::{
    Attachment, FILE_MEDIA_TYPE, IMAGE_MEDIA_TYPES, IMAGE_NOT_BACKED_UP, Image, MAX_FILE_BYTES,
    MAX_FILE_NAME_BYTES, MAX_IMAGE_BYTES, MAX_PROMPT_ATTACHMENT_BYTES, PromptFile,
};
pub use bots::{
    Bot, BotDefaults, BotError, BotFact, BotKind, BotMemory, BotPlace, BotState, BotStatus,
    BotUser, ChannelBinding, MAX_BOT_FACT_CHARS, MAX_BOT_FACTS, MAX_BOT_INSTRUCTIONS_BYTES,
    MAX_BOT_MEMORY_BYTES, SessionOrigin, ThreadLink,
};
pub use bytes::Bytes;
pub use client::{ClientHello, ClientMessage, Command, CommandBody, Cursor, HistoryPart, Relay};
pub use event::{
    AgentMessage, Answer, Answerer, ApprovalDecision, ApprovalOutcome, CiStatus, ErrorClass,
    EscalationReason, Event, EventBody, FollowUp, FollowUpReason, Item, ItemBody, Mergeable,
    PrState, PullRequest, ReviewStatus, Route, SessionStatus, TitleSource, TurnError,
};
pub use ids::{
    AccountId, ApprovalId, AttachmentId, BotId, CommandId, DeviceId, FactId, HostId, ItemId,
    PersonId, PromptId, QuestionId, SessionId, TerminalId, TurnId, UserId,
};
pub use people::{
    AssertionResponse, LeftoverDecision, NewPerson, PASSKEY_CHALLENGE_SECS, PASSKEY_RP_ID,
    PasskeyAssertion, PasskeyChallenge, PasskeyCreation, PasskeyKey, PasskeyRegistration,
    PersonCard, PersonInfo, PersonPasskey, RefusedPasskey, RegistrationResponse, SignIn,
    SignInRefusal,
};
pub use project::{MAX_PROJECT_ICON_BYTES, PROJECT_ICON_MEDIA_TYPES, Project, ProjectId};
pub use provider_logins::{ProviderLogin, ProviderLoginState};
pub use relay::{RelayError, RelayErrorCode, RelayRoute};
pub use replication::{
    AttachmentData, Batch, HostHello, HostMessage, HostSecretFetch, HubHello, HubMessage,
    JournalRecord, MAX_BATCH_EVENTS, REPLICATION_VERSION, RawEventBody, RejectReason,
    ReplicationError, ReplicationErrorCode, SecretFetch, SessionSummary,
};
pub use resources::{Constraint, Container, ContainerState, HostResources, Pressure, SessionUsage};
pub use secrets::{
    FetchedSecret, HostSecretsState, MAX_SECRET_AUDIT_ENTRIES, MAX_SECRET_DESCRIPTION_CHARS,
    MAX_SECRET_NAME_BYTES, MAX_SECRET_VALUE_BYTES, SecretAccess, SecretError, SecretErrorCode,
    SecretInfo, SecretRef, SecretRequest, SecretSet, SecretStoreRequest, SecretValue,
    is_valid_secret_name, is_valid_secret_purpose, is_valid_secret_set_name,
};
pub use server::{
    Account, ApprovalRequest, AwsDeviceCode, AwsProfile, CommandResult, DirectoryEntry, ErrorCode,
    ErrorInfo, FailoverSettings, FleetHost, HostReplication, HostUsage, HubStatus, HubVolume,
    LinkedHub, ProviderStatus, QueuedPrompt, Role, ServerHello, ServerMessage, SessionHead,
    Terminal, TerminalPurpose, UsageWindow, UserDevice, UserInfo,
};
pub use settings::{
    BackupSettings, DaemonSettings, FollowUpSettings, HERDER_MCP_SERVER, LogFormat, LogSettings,
    McpServerSettings, McpTransport, ProjectDiscovery, ProviderBinary, ResourceSettings,
    TitleSettings, UpdateStage, UpdateWhen, is_valid_mcp_server_name,
};
pub use skills::{
    AccountSkills, LibrarySkill, MAX_SKILL_BYTES, MAX_SKILL_NAME_CHARS, ProviderReload,
    SessionSkill, SkillFile, SkillReload, SkillSource, SkillsStatus, is_valid_skill_name,
};
pub use types::{PermissionMode, Provider, clean_title};
pub use usage::{FailoverTotal, TurnUsage, UsagePeriod, UsageTotal};

/// Wire protocol version, exchanged in both hellos; peers with different versions disconnect.
pub const PROTOCOL_VERSION: u32 = 6;

/// Most bytes one WebSocket message a client sends may have. A client does not send a larger
/// one: it fails the command it carries instead. A daemon sent one refuses it unread.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// Most events `get_latest_events` answers with: enough for an app's first screen of a
/// session.
pub const LATEST_EVENTS: usize = 100;

/// Most bytes of JSON the events `get_latest_events` answers with take together, unless the
/// latest alone takes more: a few screenfuls, which a link of 25 kB a second carries in under
/// three seconds.
pub const LATEST_EVENTS_BYTES: usize = 64 * 1024;

/// Most characters a session title may have.
pub const MAX_TITLE_CHARS: usize = 80;

/// Most turns a host may be set to run at once with `set_resource_limits`.
pub const MAX_TURNS_LIMIT: u32 = 64;

/// Per-session sequence number of a durable event: starts at 1 and increases by 1 per event.
pub type Seq = u64;

/// Point in time on the wire, an RFC 3339 UTC timestamp.
pub type Timestamp = jiff::Timestamp;

/// Schema of a [`Timestamp`] field, named with `#[schemars(with = "TimestampSchema")]`.
///
/// schemars implements `JsonSchema` only for jiff 0.2, so this spells out the same inline
/// schema it gives that `Timestamp`, keeping the published schema unchanged.
pub struct TimestampSchema;

impl schemars::JsonSchema for TimestampSchema {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Timestamp".into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        "jiff::Timestamp".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "format": "date-time"
        })
    }
}

/// JSON Schema for every message a client sends.
pub fn client_schema() -> schemars::Schema {
    schemars::schema_for!(ClientMessage)
}

/// JSON Schema for every message the daemon sends.
pub fn server_schema() -> schemars::Schema {
    schemars::schema_for!(ServerMessage)
}

/// JSON Schema for every message a host sends the hub.
pub fn host_schema() -> schemars::Schema {
    schemars::schema_for!(HostMessage)
}

/// JSON Schema for every message the hub sends a host.
pub fn hub_schema() -> schemars::Schema {
    schemars::schema_for!(HubMessage)
}

/// JSON Schema for the body of the hub's refusal of a relay connection.
pub fn relay_error_schema() -> schemars::Schema {
    schemars::schema_for!(RelayError)
}
