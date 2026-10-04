//! The protocol and client-core types the API passes through, declared to UniFFI as remote
//! records, enums and custom types. Each declaration repeats its type's definition, which the
//! compiler checks against the real one; the rustdoc of the real type is the contract.

use std::collections::HashMap;

use herder_client_core::{
    ConnectionQuality, ConnectionState, Machine, NewAccount, PairResult, PairingLink, PairingUri,
    SessionUpdate, SharedLink, SkippedMachine, TerminalEvent,
};
use herder_protocol::{
    Account, AccountId, AgentMessage, Answer, Answerer, ApprovalDecision, ApprovalId,
    ApprovalOutcome, Attachment, AttachmentId, Bytes, CiStatus, CommandBody, CommandResult,
    Constraint, Container, ContainerState, DirectoryEntry, ErrorClass, ErrorCode, ErrorInfo,
    EscalationReason, Event, EventBody, FailoverSettings, FleetHost, HostId, HostReplication,
    HostResources, HostUsage, Image, Item, ItemBody, ItemId, LinkedVault, Mergeable,
    PermissionMode, PrState, Pressure, Project, ProjectId, PromptId, Provider, PullRequest,
    QuestionId, QueuedPrompt, ReviewStatus, Role, Route, SessionHead, SessionId, SessionStatus,
    SessionUsage, Terminal, TerminalId, TerminalPurpose, Timestamp, TitleSource, TurnError, TurnId,
    UsageWindow, UserId, VaultStatus, VaultVolume,
};
use serde_json::Value as Json;

macro_rules! string_ids {
    ($($id:ident),+ $(,)?) => {$(
        uniffi::custom_type!($id, String, {
            remote,
            lower: |id| id.as_str().to_owned(),
            try_lift: |id| Ok($id::new(id)),
        });
    )+};
}

string_ids!(
    SessionId,
    TurnId,
    ItemId,
    ApprovalId,
    QuestionId,
    HostId,
    AccountId,
    UserId,
    TerminalId,
    ProjectId,
    AttachmentId,
    PromptId,
);

// A provider is its wire name; unknown names stay verbatim, as on the wire.
uniffi::custom_type!(Provider, String, {
    remote,
    lower: |provider| provider.as_str().to_owned(),
    try_lift: |name| Ok(Provider::from(name)),
});

// An RFC 3339 UTC timestamp, as on the wire.
uniffi::custom_type!(Timestamp, String, {
    remote,
    lower: |at| at.to_string(),
    try_lift: |at| Ok(at.parse()?),
});

uniffi::custom_type!(Bytes, Vec<u8>, {
    remote,
    lower: |bytes| bytes.0,
    try_lift: |bytes| Ok(Bytes(bytes)),
});

// A tool call's arguments, as JSON text.
uniffi::custom_type!(Json, String, {
    remote,
    lower: |value| value.to_string(),
    try_lift: |json| Ok(serde_json::from_str(&json)?),
});

// client-core

#[uniffi::remote(Record)]
pub struct Machine {
    pub host_id: HostId,
    pub name: String,
    pub addresses: Vec<String>,
    pub fingerprint: String,
    pub connection: ConnectionState,
    pub quality: ConnectionQuality,
    pub role: Option<Role>,
    pub sessions: Vec<SessionHead>,
    pub hosts: Vec<FleetHost>,
    pub projects: Vec<Project>,
    pub accounts: Vec<Account>,
    pub failover: FailoverSettings,
    pub terminals: Vec<Terminal>,
    pub resources: Option<HostResources>,
    pub session_usage: HashMap<SessionId, SessionUsage>,
    // Defaulted, so Swift and Kotlin code that builds a machine, as previews and tests do,
    // need not name it.
    #[uniffi(default = None)]
    pub vault: Option<VaultStatus>,
}

#[uniffi::remote(Enum)]
pub enum ConnectionState {
    Connecting,
    Connected,
    Disconnected { error: String },
}

#[uniffi::remote(Record)]
pub struct ConnectionQuality {
    pub connected_since: Option<Timestamp>,
    pub reconnects: u32,
    pub last_rtt_ms: Option<u32>,
    pub average_rtt_ms: Option<u32>,
    pub min_rtt_ms: Option<u32>,
    pub max_rtt_ms: Option<u32>,
    pub missed_pongs: u32,
}

#[uniffi::remote(Record)]
pub struct SessionUpdate {
    pub events: Vec<Event>,
    pub streaming: Vec<Item>,
}

#[uniffi::remote(Record)]
pub struct NewAccount {
    pub account_id: AccountId,
    pub provider: Provider,
    pub label: Option<String>,
    pub config_dir: Option<String>,
}

#[uniffi::remote(Record)]
pub struct PairingUri {
    pub hosts: Vec<String>,
    pub fingerprint: String,
    pub code: String,
}

#[uniffi::remote(Record)]
pub struct PairingLink {
    pub machines: Vec<PairingUri>,
}

#[uniffi::remote(Enum)]
pub enum PairResult {
    Paired {
        machine: Machine,
    },
    Failed {
        addresses: Vec<String>,
        error: String,
    },
}

#[uniffi::remote(Record)]
pub struct SharedLink {
    pub link: PairingLink,
    pub shared: Vec<HostId>,
    pub skipped: Vec<SkippedMachine>,
    pub expires_at: Timestamp,
}

#[uniffi::remote(Record)]
pub struct SkippedMachine {
    pub host_id: HostId,
    pub error: String,
}

#[uniffi::remote(Enum)]
pub enum TerminalEvent {
    Output { data: Vec<u8> },
    Reattached,
    Closed { exit_code: Option<i32> },
}

// Events

#[uniffi::remote(Record)]
pub struct Event {
    pub session_id: SessionId,
    pub seq: u64,
    pub at: Timestamp,
    pub by: Option<UserId>,
    pub body: EventBody,
}

#[uniffi::remote(Enum)]
pub enum EventBody {
    SessionCreated {
        repo: String,
        worktree: String,
        branch: String,
        provider: Provider,
        account_id: AccountId,
        model: String,
        permission_mode: PermissionMode,
        parent: Option<SessionId>,
        task: Option<String>,
        max_children: Option<u32>,
        failover_pin: Option<bool>,
    },
    BranchCheckedOut {
        branch: String,
    },
    SessionStatusChanged {
        status: SessionStatus,
        retry_at: Option<Timestamp>,
    },
    TurnStarted {
        turn_id: TurnId,
    },
    TurnCompleted {
        turn_id: TurnId,
    },
    TurnInterrupted {
        turn_id: TurnId,
    },
    TurnFailed {
        turn_id: TurnId,
        error: TurnError,
    },
    ItemAdded {
        item: Item,
    },
    ApprovalRequested {
        approval_id: ApprovalId,
        turn_id: TurnId,
        tool_call_id: ItemId,
        summary: String,
        routed_to: Route,
        reason: Option<EscalationReason>,
    },
    ApprovalEscalated {
        approval_id: ApprovalId,
        reason: EscalationReason,
        note: Option<String>,
    },
    ApprovalResolved {
        approval_id: ApprovalId,
        decision: ApprovalOutcome,
        answered_by: Answerer,
    },
    QuestionAsked {
        question_id: QuestionId,
        turn_id: TurnId,
        text: String,
        choices: Vec<String>,
        routed_to: Route,
        reason: Option<EscalationReason>,
    },
    QuestionEscalated {
        question_id: QuestionId,
        reason: EscalationReason,
        note: Option<String>,
    },
    QuestionAnswered {
        question_id: QuestionId,
        answer: Answer,
        answered_by: Answerer,
    },
    ChildSpawned {
        child_session_id: SessionId,
        task: String,
    },
    ChildReported {
        child_session_id: SessionId,
        turn_id: TurnId,
        summary: String,
    },
    ModelSwitched {
        model: String,
    },
    AccountSwitched {
        account_id: AccountId,
    },
    ProviderSwitched {
        provider: Provider,
        account_id: AccountId,
        model: String,
    },
    PermissionModeChanged {
        mode: PermissionMode,
    },
    PrLinked {
        pr: PullRequest,
    },
    PrUpdated {
        pr: PullRequest,
    },
    PrUnlinked {
        number: u64,
    },
    TitleChanged {
        title: String,
        source: TitleSource,
    },
    SessionForked {
        from_session: SessionId,
        from_host: HostId,
    },
    Unknown,
}

#[uniffi::remote(Enum)]
pub enum TitleSource {
    Auto,
    AiRequested,
    User,
}

#[uniffi::remote(Enum)]
pub enum SessionStatus {
    Idle,
    Running,
    WaitingForCapacity,
    NeedsYou,
    Error,
    Archived,
    Moved,
    Unknown,
}

#[uniffi::remote(Record)]
pub struct TurnError {
    pub class: ErrorClass,
    pub message: String,
}

#[uniffi::remote(Enum)]
pub enum ErrorClass {
    LimitReached,
    Auth,
    Transient,
    Fatal,
}

#[uniffi::remote(Record)]
pub struct AgentMessage {
    pub sender_session_id: SessionId,
    pub message_id: String,
    pub hop_count: u32,
    /// Maximum permissions this message may exercise, retained while queued.
    pub permission_ceiling: PermissionMode,
}

#[uniffi::remote(Record)]
pub struct Item {
    pub agent_message: Option<AgentMessage>,
    pub parent_call_id: Option<ItemId>,
    pub id: ItemId,
    pub turn_id: TurnId,
    pub body: ItemBody,
}

#[uniffi::remote(Enum)]
pub enum ItemBody {
    UserMessage {
        text: String,
        attachments: Vec<Attachment>,
    },
    AssistantMessage {
        text: String,
    },
    Reasoning {
        text: String,
    },
    ToolCall {
        name: String,
        input: Json,
    },
    ToolResult {
        call_id: ItemId,
        output: String,
        is_error: bool,
    },
    Unknown,
}

#[uniffi::remote(Record)]
pub struct Attachment {
    pub attachment_id: AttachmentId,
    pub media_type: String,
    pub size: u64,
}

#[uniffi::remote(Enum)]
pub enum ApprovalDecision {
    Allow,
    Deny,
}

#[uniffi::remote(Enum)]
pub enum ApprovalOutcome {
    Allow,
    Deny,
    Expired,
}

#[uniffi::remote(Enum)]
pub enum Route {
    Primary,
    User,
}

#[uniffi::remote(Enum)]
pub enum EscalationReason {
    MarkedByPrimary,
    ExceedsAuthority,
    Timeout,
}

#[uniffi::remote(Enum)]
pub enum Answerer {
    User,
    Primary { session_id: SessionId },
}

#[uniffi::remote(Enum)]
pub enum Answer {
    Text { text: String },
    Choice { index: u32 },
}

#[uniffi::remote(Record)]
pub struct PullRequest {
    pub number: u64,
    pub url: String,
    pub title: String,
    pub head_branch: Option<String>,
    pub state: PrState,
    pub ci: CiStatus,
    pub review: ReviewStatus,
    pub mergeable: Mergeable,
}

#[uniffi::remote(Enum)]
pub enum PrState {
    Draft,
    Open,
    Merged,
    Closed,
}

#[uniffi::remote(Enum)]
pub enum CiStatus {
    None,
    Pending,
    Passing,
    Failing,
}

#[uniffi::remote(Enum)]
pub enum ReviewStatus {
    None,
    Required,
    Approved,
    ChangesRequested,
}

#[uniffi::remote(Enum)]
pub enum Mergeable {
    Clean,
    Conflicting,
    Unknown,
}

// Commands

#[uniffi::remote(Enum)]
pub enum PermissionMode {
    ReadOnly,
    Ask,
    AutoEdit,
    FullAccess,
}

#[uniffi::remote(Enum)]
pub enum CommandBody {
    CreateSession {
        repo: Option<String>,
        project_id: Option<ProjectId>,
        branch: Option<String>,
        account_id: Option<AccountId>,
        provider: Option<Provider>,
        model: Option<String>,
        permission_mode: Option<PermissionMode>,
        max_children: Option<u32>,
        failover_pin: Option<bool>,
    },
    ArchiveSession {
        session_id: SessionId,
        force: bool,
    },
    UnarchiveSession {
        session_id: SessionId,
    },
    RenameSession {
        session_id: SessionId,
        title: String,
    },
    RetitleSession {
        session_id: SessionId,
    },
    ForkSession {
        session_id: SessionId,
        account_id: Option<AccountId>,
    },
    SendPrompt {
        session_id: SessionId,
        text: String,
        images: Vec<Image>,
    },
    GetAttachment {
        session_id: SessionId,
        attachment_id: AttachmentId,
    },
    Interrupt {
        session_id: SessionId,
    },
    RemoveQueued {
        session_id: SessionId,
        prompt_id: PromptId,
    },
    MoveQueued {
        session_id: SessionId,
        prompt_id: PromptId,
        before: Option<PromptId>,
    },
    SendQueuedNow {
        session_id: SessionId,
        prompt_id: PromptId,
    },
    SetModel {
        session_id: SessionId,
        model: String,
    },
    SetPermissionMode {
        session_id: SessionId,
        mode: PermissionMode,
    },
    AnswerApproval {
        session_id: SessionId,
        approval_id: ApprovalId,
        decision: ApprovalDecision,
    },
    AnswerQuestion {
        session_id: SessionId,
        question_id: QuestionId,
        answer: Answer,
    },
    SwitchAccount {
        session_id: SessionId,
        account_id: AccountId,
    },
    SwitchProvider {
        session_id: SessionId,
        account_id: AccountId,
        model: Option<String>,
    },
    LinkPr {
        session_id: SessionId,
        number: u64,
    },
    UnlinkPr {
        session_id: SessionId,
        number: u64,
    },
    ComposeDown {
        session_id: SessionId,
        project: String,
    },
    OpenTerminal {
        session_id: SessionId,
        cols: u16,
        rows: u16,
    },
    ListDirectory {
        path: String,
    },
    AddProject {
        path: String,
    },
    SetProjectSettings {
        project_id: ProjectId,
        default_permission_mode: Option<PermissionMode>,
        default_account: Option<AccountId>,
        setup_command: Option<String>,
    },
    RemoveProject {
        project_id: ProjectId,
    },
    GetProjectIcon {
        project_id: ProjectId,
    },
    GetVaultLink,
    LinkVault {
        addresses: Vec<String>,
        fingerprint: String,
        pairing_code: String,
    },
    UnlinkVault,
    PairDevice,
    PairVaultHost {
        host_name: String,
    },
    RevokeVaultHost {
        host_id: HostId,
    },
    SetAccountSettings {
        account_id: AccountId,
        label: String,
        config_dir: Option<String>,
    },
    AddAccount {
        account_id: AccountId,
        provider: Provider,
        label: Option<String>,
        config_dir: Option<String>,
        cols: u16,
        rows: u16,
    },
    AttachTerminal {
        terminal_id: TerminalId,
    },
    DetachTerminal {
        terminal_id: TerminalId,
    },
    ResizeTerminal {
        terminal_id: TerminalId,
        cols: u16,
        rows: u16,
    },
    TerminalInput {
        terminal_id: TerminalId,
        data: Bytes,
    },
}

#[uniffi::remote(Record)]
pub struct Image {
    pub media_type: String,
    pub data: Bytes,
}

#[uniffi::remote(Enum)]
pub enum CommandResult {
    Applied,
    SessionCreated {
        session_id: SessionId,
    },
    TerminalOpened {
        terminal_id: TerminalId,
    },
    Attachment {
        media_type: String,
        data: Bytes,
    },
    Directory {
        path: String,
        entries: Vec<DirectoryEntry>,
    },
    ProjectAdded {
        project_id: ProjectId,
    },
    ProjectIcon {
        icon: String,
        media_type: String,
        data: Bytes,
    },
    VaultLink {
        is_vault: bool,
        vault: Option<LinkedVault>,
        volume: Option<VaultVolume>,
    },
    DevicePairing {
        code: String,
        fingerprint: String,
        addresses: Vec<String>,
        expires_at: Timestamp,
    },
    HostPairing {
        code: String,
        expires_at: Timestamp,
    },
    SessionForked {
        session_id: SessionId,
        account_id: AccountId,
        forked_from: SessionId,
        from_host_id: HostId,
    },
}

#[uniffi::remote(Record)]
pub struct LinkedVault {
    pub address: String,
    pub fingerprint: String,
}

#[uniffi::remote(Record)]
pub struct DirectoryEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_repo: bool,
}

#[uniffi::remote(Record)]
pub struct ErrorInfo {
    pub code: ErrorCode,
    pub message: String,
}

#[uniffi::remote(Enum)]
pub enum ErrorCode {
    BadRequest,
    Forbidden,
    NotFound,
    Conflict,
    Unsupported,
    ReadOnly,
    Internal,
}

// Machine lists

#[uniffi::remote(Record)]
pub struct FleetHost {
    pub host_id: HostId,
    pub host_name: String,
    pub online: bool,
    pub last_seen: Timestamp,
    // Defaults to nil so Swift and Kotlin code building a `FleetHost` need not name it.
    #[uniffi(default)]
    pub usage: Option<HostUsage>,
}

#[uniffi::remote(Record)]
pub struct HostUsage {
    pub sessions: u32,
    pub attachment_bytes: u64,
    pub attachments_cap: Option<u64>,
}

#[uniffi::remote(Record)]
pub struct VaultVolume {
    pub total_bytes: u64,
    pub used_bytes: u64,
}

#[uniffi::remote(Record)]
pub struct VaultStatus {
    pub sessions: u64,
    pub events: u64,
    pub storage_bytes: u64,
    pub hosts: Vec<HostReplication>,
}

#[uniffi::remote(Record)]
pub struct HostReplication {
    pub host_id: HostId,
    pub sessions: u64,
    pub events: u64,
    pub last_event_at: Option<Timestamp>,
    pub lag_ms: Option<u64>,
}

#[uniffi::remote(Enum)]
pub enum Role {
    Owner,
    Member,
}

#[uniffi::remote(Record)]
pub struct SessionHead {
    pub session_id: SessionId,
    pub host_id: Option<HostId>,
    pub head_seq: u64,
    pub status: SessionStatus,
    pub parent: Option<SessionId>,
    pub task: Option<String>,
    pub title: Option<String>,
    pub project_id: Option<ProjectId>,
    pub account_id: AccountId,
    pub children_need_you: u32,
    pub queue: Vec<QueuedPrompt>,
}

#[uniffi::remote(Record)]
pub struct QueuedPrompt {
    pub prompt_id: PromptId,
    pub text: String,
    pub images: u32,
    pub by: Option<UserId>,
    pub agent_message: Option<AgentMessage>,
}

#[uniffi::remote(Record)]
pub struct Account {
    pub account_id: AccountId,
    pub provider: Provider,
    pub label: String,
    pub config_dir: Option<String>,
    pub usage: Vec<UsageWindow>,
}

#[uniffi::remote(Record)]
pub struct FailoverSettings {
    pub pin: bool,
}

#[uniffi::remote(Record)]
pub struct UsageWindow {
    pub window: String,
    pub used_percent: f64,
    pub resets_at: Option<Timestamp>,
}

#[uniffi::remote(Record)]
pub struct Terminal {
    pub terminal_id: TerminalId,
    pub purpose: TerminalPurpose,
}

#[uniffi::remote(Enum)]
pub enum TerminalPurpose {
    Shell { session_id: SessionId },
    Login { account_id: AccountId },
}

#[uniffi::remote(Record)]
pub struct Project {
    pub project_id: ProjectId,
    pub name: String,
    pub paths: Vec<String>,
    pub default_permission_mode: Option<PermissionMode>,
    pub default_account: Option<AccountId>,
    pub setup_command: Option<String>,
    // Defaults to nil so Swift and Kotlin code building a `Project` need not name it.
    #[uniffi(default)]
    pub icon: Option<String>,
}

// Resources

#[uniffi::remote(Record)]
pub struct HostResources {
    pub cpu_cores: u32,
    pub cpu_percent: f64,
    pub load_1m: f64,
    pub memory_total_bytes: u64,
    pub memory_available_bytes: u64,
    pub pressure: Option<Pressure>,
    pub running_turns: u32,
    pub max_turns: u32,
    pub waiting_turns: u32,
    pub constraint: Option<Constraint>,
}

#[uniffi::remote(Record)]
pub struct Pressure {
    pub cpu_some: f64,
    pub memory_some: f64,
    pub memory_full: f64,
    pub io_some: f64,
}

#[uniffi::remote(Enum)]
pub enum Constraint {
    MaxTurns,
    Memory,
    Load,
    Pressure,
}

#[uniffi::remote(Record)]
pub struct SessionUsage {
    pub cpu_percent: f64,
    pub memory_bytes: u64,
    pub processes: u32,
    pub containers: Vec<Container>,
}

#[uniffi::remote(Record)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub compose_project: Option<String>,
    pub image: String,
    pub state: ContainerState,
}

#[uniffi::remote(Enum)]
pub enum ContainerState {
    Created,
    Running,
    Paused,
    Restarting,
    Removing,
    Exited,
    Dead,
}
