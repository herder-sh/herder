//! The protocol and client-core types the API passes through, declared to UniFFI as remote
//! records, enums and custom types. Each declaration repeats its type's definition, which the
//! compiler checks against the real one; the rustdoc of the real type is the contract.

use std::collections::HashMap;

use herder_client_core::{
    ConnectionQuality, ConnectionState, LoginUri, Machine, NewAccount, OwnUri, PairResult,
    PairingLink, PairingUri, SessionUpdate, SharedLink, SkippedMachine, TerminalEvent,
};
use herder_protocol::{
    Account, AccountId, AccountSkills, AgentMessage, Answer, Answerer, ApprovalDecision,
    ApprovalId, ApprovalOutcome, AssertionResponse, Attachment, AttachmentId, AwsDeviceCode,
    AwsProfile, BackupSettings, Bot, BotDefaults, BotError, BotFact, BotId, BotKind, BotMemory,
    BotPlace, BotState, BotStatus, BotUser, Bytes, ChannelBinding, CiStatus, CommandBody,
    CommandResult, Constraint, Container, ContainerState, DaemonSettings, DeviceId, DirectoryEntry,
    ErrorClass, ErrorCode, ErrorInfo, EscalationReason, Event, EventBody, FactId, FailoverSettings,
    FailoverTotal, FleetHost, FollowUp, FollowUpReason, FollowUpSettings, HistoryPart, HostId,
    HostReplication, HostResources, HostUsage, Image, Item, ItemBody, ItemId, LeftoverDecision,
    LibrarySkill, LinkedVault, LogFormat, LogSettings, McpServerSettings, McpTransport, Mergeable,
    NewPerson, PasskeyAssertion, PasskeyChallenge, PasskeyCreation, PasskeyKey,
    PasskeyRegistration, PermissionMode, PersonCard, PersonId, PersonInfo, PersonPasskey, PrState,
    Pressure, Project, ProjectDiscovery, ProjectId, PromptFile, PromptId, Provider, ProviderBinary,
    ProviderReload, ProviderStatus, PullRequest, QuestionId, QueuedPrompt, RegistrationResponse,
    Relay, ResourceSettings, ReviewStatus, Role, Route, SecretAccess, SecretInfo, SecretRef,
    SecretSet, SecretStoreRequest, SecretValue, SessionHead, SessionId, SessionOrigin,
    SessionSkill, SessionStatus, SessionUsage, SignIn, SkillFile, SkillReload, SkillSource,
    SkillsStatus, Terminal, TerminalId, TerminalPurpose, ThreadLink, Timestamp, TitleSettings,
    TitleSource, TurnError, TurnId, TurnUsage, UpdateStage, UpdateWhen, UsagePeriod, UsageTotal,
    UsageWindow, UserDevice, UserId, UserInfo, VaultStatus, VaultVolume,
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
    PersonId,
    DeviceId,
    TerminalId,
    ProjectId,
    AttachmentId,
    PromptId,
    BotId,
    FactId,
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

// A secret's value, sent to a vault, and back only as an agent's `store_secret` read it; refused as `SecretValue::new` refuses it.
uniffi::custom_type!(SecretValue, String, {
    remote,
    lower: |value| value.expose().to_owned(),
    try_lift: |value| SecretValue::new(value)
        .ok_or_else(|| uniffi::deps::anyhow::anyhow!("a secret's value is 1 to 64 KiB without a NUL")),
});

// A secret as a session names it, `NAME` or `set/NAME`; refused as `SecretRef` parses it.
uniffi::custom_type!(SecretRef, String, {
    remote,
    lower: |secret| secret.to_string(),
    try_lift: |secret| secret.parse().map_err(|err: String| uniffi::deps::anyhow::anyhow!(err)),
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
    #[uniffi(default = None)]
    pub address: Option<String>,
    pub fingerprint: String,
    pub connection: ConnectionState,
    pub quality: ConnectionQuality,
    pub role: Option<Role>,
    #[uniffi(default = None)]
    pub herder_version: Option<String>,
    pub sessions: Vec<SessionHead>,
    pub hosts: Vec<FleetHost>,
    pub projects: Vec<Project>,
    pub accounts: Vec<Account>,
    pub failover: FailoverSettings,
    #[uniffi(default)]
    pub providers: Vec<ProviderStatus>,
    pub terminals: Vec<Terminal>,
    pub resources: Option<HostResources>,
    pub session_usage: HashMap<SessionId, SessionUsage>,
    // Defaulted, so Swift and Kotlin code that builds a machine, as previews and tests do,
    // need not name it.
    #[uniffi(default = None)]
    pub vault: Option<VaultStatus>,
    #[uniffi(default = None)]
    pub skills: Option<SkillsStatus>,
    #[uniffi(default)]
    pub session_skills: HashMap<SessionId, Vec<SessionSkill>>,
    #[uniffi(default = None)]
    pub update: Option<UpdateStage>,
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

#[uniffi::remote(Record)]
pub struct LoginUri {
    pub hosts: Vec<String>,
    pub fingerprint: String,
    pub code: String,
}

#[uniffi::remote(Record)]
pub struct OwnUri {
    pub hosts: Vec<String>,
    pub fingerprint: String,
    pub code: String,
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
        branch: Option<String>,
        provider: Provider,
        account_id: AccountId,
        model: String,
        permission_mode: PermissionMode,
        parent: Option<SessionId>,
        parent_host: Option<HostId>,
        task: Option<String>,
        failover_pin: Option<bool>,
        chat: bool,
        origin: Option<SessionOrigin>,
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
        usage: Option<TurnUsage>,
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
        secret: Option<SecretStoreRequest>,
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
        multi_select: bool,
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
        host_id: Option<HostId>,
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
    ArtifactPublished {
        title: String,
        attachment: Attachment,
        url: Option<String>,
        expires_at: Option<Timestamp>,
    },
    ThreadLinked {
        thread: ThreadLink,
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
    NeedsLogin,
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
pub struct FollowUp {
    pub reason: FollowUpReason,
    pub pr: Option<u64>,
    pub head_sha: Option<String>,
}

#[uniffi::remote(Enum)]
pub enum FollowUpReason {
    CiFailed,
    CiPassed,
    Conflicting,
    ChangesRequested,
    Stalled,
    Unknown,
}

#[uniffi::remote(Record)]
pub struct Item {
    pub agent_message: Option<AgentMessage>,
    // Defaulted, so Swift and Kotlin code that builds an item, as tests do, need not name it.
    #[uniffi(default = None)]
    pub follow_up: Option<FollowUp>,
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
    // Defaulted, so Swift and Kotlin code that builds an image's attachment need not name it.
    #[uniffi(default = None)]
    pub name: Option<String>,
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
    Choices { indices: Vec<u32> },
}

#[uniffi::remote(Record)]
pub struct PullRequest {
    pub number: u64,
    pub url: String,
    pub title: String,
    pub head_branch: Option<String>,
    // Defaulted, so Swift and Kotlin code that builds a pull request, as tests do, need not
    // name them.
    #[uniffi(default = None)]
    pub head_sha: Option<String>,
    #[uniffi(default = None)]
    pub unresolved_threads: Option<u32>,
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
        chat: bool,
        branch: Option<String>,
        account_id: Option<AccountId>,
        provider: Option<Provider>,
        model: Option<String>,
        permission_mode: Option<PermissionMode>,
        failover_pin: Option<bool>,
    },
    ArchiveSession {
        session_id: SessionId,
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
        relay: Option<Relay>,
    },
    UploadHistory {
        session_id: SessionId,
        part: HistoryPart,
    },
    SendPrompt {
        session_id: SessionId,
        text: String,
        images: Vec<Image>,
        files: Vec<PromptFile>,
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
    MergeQueued {
        session_id: SessionId,
        prompt_ids: Vec<PromptId>,
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
        images: Vec<Image>,
        files: Vec<PromptFile>,
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
        assertion: Option<Box<PasskeyAssertion>>,
    },
    ListDirectory {
        path: String,
    },
    AddProject {
        path: String,
    },
    CloneProject {
        url: String,
        path: Option<String>,
    },
    SetProjectSettings {
        project_id: ProjectId,
        name: Option<String>,
        default_permission_mode: Option<PermissionMode>,
        default_account: Option<AccountId>,
        setup_command: Option<String>,
        icon_background: Option<String>,
        private_artifacts: bool,
    },
    RemoveProject {
        project_id: ProjectId,
    },
    SetProjectIcon {
        project_id: ProjectId,
        icon: Option<Image>,
    },
    GetProjectIcon {
        project_id: ProjectId,
    },
    GetUsageSummary {
        period: UsagePeriod,
    },
    GetVaultLink,
    LinkVault {
        addresses: Vec<String>,
        fingerprint: String,
        pairing_code: String,
        backup: bool,
        secrets: bool,
    },
    SetVaultLink {
        backup: bool,
        secrets: bool,
    },
    UnlinkVault,
    InstallVault,
    LinkLocalVault,
    PairDevice {
        user: Option<String>,
        role: Option<Role>,
    },
    ListUsers,
    SetUserRole {
        user_id: UserId,
        role: Role,
    },
    RevokeDevice {
        device_id: DeviceId,
        assertion: Option<Box<PasskeyAssertion>>,
    },
    PasskeyChallenge,
    ListPeople,
    Invite {
        role: Role,
        assertion: Box<PasskeyAssertion>,
    },
    SetPersonRole {
        person_id: PersonId,
        role: Role,
        person: Option<PersonCard>,
        assertion: Box<PasskeyAssertion>,
    },
    RevokePerson {
        person_id: PersonId,
        assertion: Box<PasskeyAssertion>,
    },
    ClaimUsers {
        person: Box<NewPerson>,
        also: Vec<UserId>,
    },
    DecideLeftover {
        user_id: UserId,
        decision: LeftoverDecision,
        assertion: Box<PasskeyAssertion>,
    },
    ApproveLogin {
        code: String,
        assertion: Box<PasskeyAssertion>,
    },
    PairVaultHost {
        host_name: String,
    },
    RevokeVaultHost {
        host_id: HostId,
    },
    CreateSecretSet {
        set: String,
        assertion: Option<Box<PasskeyAssertion>>,
    },
    DeleteSecretSet {
        set: String,
        assertion: Option<Box<PasskeyAssertion>>,
    },
    PutSecret {
        set: String,
        name: String,
        description: String,
        value: SecretValue,
        assertion: Option<Box<PasskeyAssertion>>,
    },
    DeleteSecret {
        set: String,
        name: String,
        assertion: Option<Box<PasskeyAssertion>>,
    },
    GrantSecretSet {
        set: String,
        host_id: HostId,
        host_use: bool,
        assertion: Option<Box<PasskeyAssertion>>,
    },
    RevokeSecretSet {
        set: String,
        host_id: HostId,
        host_use: bool,
        assertion: Option<Box<PasskeyAssertion>>,
    },
    ListSecrets,
    GetSecretToStore {
        session_id: SessionId,
        approval_id: ApprovalId,
    },
    ConfirmSecretStored {
        session_id: SessionId,
        approval_id: ApprovalId,
        set: String,
        assertion: Option<Box<PasskeyAssertion>>,
    },
    ListSecretAudit {
        before: Option<u64>,
    },
    SetVaultHostAccess {
        host_id: HostId,
        read: bool,
    },
    SetSkillsRepo {
        url: String,
    },
    PutSkill {
        name: String,
        files: Vec<SkillFile>,
    },
    DeleteSkill {
        name: String,
    },
    ImportSkill {
        git_url: String,
        path: Option<String>,
    },
    PullSkills,
    SetSkillEnabled {
        name: String,
        enabled: bool,
    },
    SetAccountSettings {
        account_id: AccountId,
        label: String,
        config_dir: Option<String>,
        fallback: Option<bool>,
    },
    SetResourceLimits {
        max_turns: u32,
    },
    GetSettings,
    SetSettings {
        settings: Box<DaemonSettings>,
    },
    RestartDaemon,
    UpdateDaemon {
        when: UpdateWhen,
    },
    AddAccount {
        account_id: AccountId,
        provider: Provider,
        label: Option<String>,
        config_dir: Option<String>,
        cols: u16,
        rows: u16,
    },
    InstallProvider {
        provider: Provider,
        cols: u16,
        rows: u16,
    },
    LogInAccount {
        account_id: AccountId,
        cols: u16,
        rows: u16,
    },
    ListAwsProfiles,
    AwsSsoLogin {
        profile: String,
        cols: u16,
        rows: u16,
    },
    ListBots,
    PutBot {
        bot: Box<Bot>,
    },
    RemoveBot {
        bot_id: BotId,
    },
    GetBotStatus,
    GetBotMemory {
        bot_id: BotId,
    },
    SetBotFact {
        bot_id: BotId,
        fact_id: Option<FactId>,
        text: String,
    },
    DeleteBotFact {
        bot_id: BotId,
        fact_id: FactId,
    },
    ClearBotMemory {
        bot_id: BotId,
    },
    AttachTerminal {
        terminal_id: TerminalId,
        assertion: Option<Box<PasskeyAssertion>>,
    },
    DetachTerminal {
        terminal_id: TerminalId,
    },
    CloseTerminal {
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

#[uniffi::remote(Record)]
pub struct PromptFile {
    pub name: String,
    pub data: Bytes,
}

#[uniffi::remote(Record)]
pub struct Relay {
    pub host_id: HostId,
    pub project_id: ProjectId,
}

#[uniffi::remote(Enum)]
pub enum HistoryPart {
    Events {
        events: Vec<Event>,
    },
    Image {
        attachment_id: AttachmentId,
        image: Image,
    },
    File {
        attachment: Attachment,
        data: Bytes,
    },
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
    AwsProfiles {
        profiles: Vec<AwsProfile>,
    },
    ProjectAdded {
        project_id: ProjectId,
    },
    ProjectIcon {
        icon: String,
        media_type: String,
        data: Bytes,
    },
    UsageSummary {
        period: UsagePeriod,
        since: Timestamp,
        totals: Vec<UsageTotal>,
        failovers: Vec<FailoverTotal>,
    },
    VaultLink {
        is_vault: bool,
        vault: Option<LinkedVault>,
        volume: Option<VaultVolume>,
        local_vault: bool,
    },
    Settings {
        settings: Box<DaemonSettings>,
        restart_required: bool,
        data_dir: String,
        is_vault: bool,
    },
    DevicePairing {
        code: String,
        fingerprint: String,
        addresses: Vec<String>,
        expires_at: Timestamp,
    },
    PasskeyChallenge {
        challenge: PasskeyChallenge,
        registration: Option<PasskeyCreation>,
    },
    People {
        people: Vec<PersonInfo>,
    },
    Invite {
        invite: String,
        role: Role,
        fingerprint: String,
        addresses: Vec<String>,
        expires_at: Timestamp,
    },
    Claimed {
        person_id: PersonId,
        hosts: Vec<String>,
        leftovers: Vec<UserInfo>,
    },
    LoginApproved {
        device_id: DeviceId,
        client: String,
    },
    Users {
        users: Vec<UserInfo>,
    },
    HostPairing {
        code: String,
        expires_at: Timestamp,
    },
    Secrets {
        sets: Vec<SecretSet>,
        secrets: Vec<SecretInfo>,
    },
    SecretAudit {
        entries: Vec<SecretAccess>,
    },
    SecretToStore {
        value: SecretValue,
    },
    Bots {
        bots: Vec<Bot>,
    },
    BotStatus {
        bots: Vec<BotStatus>,
    },
    BotMemory {
        bot_id: BotId,
        memory: BotMemory,
    },
    SessionForked {
        session_id: SessionId,
        account_id: AccountId,
        forked_from: SessionId,
        from_host_id: HostId,
    },
}

#[uniffi::remote(Record)]
pub struct Bot {
    pub bot_id: BotId,
    pub kind: BotKind,
    pub name: String,
    pub token: SecretRef,
    pub app_token: Option<SecretRef>,
    pub allow: Vec<BotUser>,
    pub defaults: BotDefaults,
    pub channel_bindings: Vec<ChannelBinding>,
    pub instructions: Option<String>,
}

#[uniffi::remote(Record)]
pub struct BotMemory {
    pub facts: Vec<BotFact>,
}

#[uniffi::remote(Record)]
pub struct BotFact {
    pub id: FactId,
    pub text: String,
    pub learned_at: Timestamp,
    pub thread: Option<ThreadLink>,
    pub place: Option<BotPlace>,
    pub by: Option<String>,
}

#[uniffi::remote(Enum)]
pub enum BotKind {
    Slack,
    Telegram,
    Unknown,
}

#[uniffi::remote(Record)]
pub struct BotUser {
    pub external_user_id: String,
    pub display_name: String,
    pub dm: bool,
    pub channels: bool,
    pub approve: bool,
    pub notify: bool,
    #[uniffi(default = false)]
    pub owner: bool,
    #[uniffi(default = None)]
    pub user_id: Option<UserId>,
}

#[uniffi::remote(Record)]
pub struct BotDefaults {
    pub project_id: Option<ProjectId>,
    pub provider: Option<Provider>,
    pub account_ids: Vec<AccountId>,
    pub model: Option<String>,
    pub permission_mode: Option<PermissionMode>,
}

#[uniffi::remote(Record)]
pub struct ChannelBinding {
    pub channel_id: String,
    pub project_id: ProjectId,
}

#[uniffi::remote(Record)]
pub struct BotStatus {
    pub bot_id: BotId,
    pub state: BotState,
    pub last_error: Option<BotError>,
}

#[uniffi::remote(Enum)]
pub enum BotState {
    Connecting,
    Connected,
    WaitingForVault,
    TokenRejected,
    Unsupported,
    Unknown,
}

#[uniffi::remote(Record)]
pub struct BotError {
    pub at: Timestamp,
    pub message: String,
}

#[uniffi::remote(Record)]
pub struct ThreadLink {
    pub kind: BotKind,
    pub bot_id: BotId,
    pub channel_id: String,
    pub thread_id: String,
    pub url: Option<String>,
}

#[uniffi::remote(Record)]
pub struct SessionOrigin {
    pub bot_id: BotId,
    pub kind: BotKind,
    pub by: String,
    pub place: BotPlace,
}

#[uniffi::remote(Enum)]
pub enum BotPlace {
    Direct,
    Channel { name: String },
    Unknown,
}

#[uniffi::remote(Record)]
pub struct UserInfo {
    pub user_id: UserId,
    pub name: String,
    pub role: Role,
    pub devices: Vec<UserDevice>,
}

#[uniffi::remote(Record)]
pub struct PersonInfo {
    pub person_id: PersonId,
    pub name: String,
    pub role: Role,
    pub passkeys: Vec<PasskeyKey>,
    pub devices: Vec<UserDevice>,
}

#[uniffi::remote(Record)]
pub struct PersonCard {
    pub name: String,
    pub passkeys: Vec<PasskeyKey>,
}

#[uniffi::remote(Record)]
pub struct PasskeyKey {
    pub credential_id: String,
    pub public_key: String,
}

#[uniffi::remote(Record)]
pub struct PasskeyChallenge {
    pub rp_id: String,
    pub challenge: String,
    pub expires_at: Timestamp,
}

#[uniffi::remote(Record)]
pub struct PasskeyCreation {
    pub rp_id: String,
    pub challenge: String,
    pub person_id: PersonId,
    pub user_handle: String,
    pub expires_at: Timestamp,
}

#[uniffi::remote(Record)]
pub struct PasskeyAssertion {
    pub id: String,
    pub raw_id: String,
    pub response: AssertionResponse,
    pub kind: String,
}

#[uniffi::remote(Record)]
pub struct AssertionResponse {
    pub client_data_json: String,
    pub authenticator_data: String,
    pub signature: String,
    pub user_handle: Option<String>,
}

#[uniffi::remote(Enum)]
pub enum SignIn {
    Passkey { assertion: PasskeyAssertion },
    ClaimOwnership { code: String, person: NewPerson },
    AcceptInvite { invite: String, person: NewPerson },
    StartLogin,
}

#[uniffi::remote(Record)]
pub struct NewPerson {
    pub name: String,
    pub passkey: PersonPasskey,
}

#[uniffi::remote(Enum)]
pub enum PersonPasskey {
    Register {
        registration: PasskeyRegistration,
    },
    Existing {
        person_id: PersonId,
        key: PasskeyKey,
        assertion: PasskeyAssertion,
    },
}

#[uniffi::remote(Record)]
pub struct PasskeyRegistration {
    pub id: String,
    pub raw_id: String,
    pub response: RegistrationResponse,
    pub kind: String,
}

#[uniffi::remote(Record)]
pub struct RegistrationResponse {
    pub client_data_json: String,
    pub attestation_object: String,
}

#[uniffi::remote(Enum)]
pub enum LeftoverDecision {
    ItsMe,
    Remove,
}

#[uniffi::remote(Record)]
pub struct UserDevice {
    pub device_id: DeviceId,
    pub client: String,
    pub paired_at: Timestamp,
    pub last_seen: Option<Timestamp>,
    pub online: bool,
    pub host_only: bool,
}

#[uniffi::remote(Record)]
pub struct SecretSet {
    pub name: String,
    pub hosts: Vec<HostId>,
    pub host_use: Vec<HostId>,
}

#[uniffi::remote(Record)]
pub struct SecretInfo {
    pub set: String,
    pub name: String,
    pub description: String,
    pub updated_at: Timestamp,
}

#[uniffi::remote(Record)]
pub struct SecretStoreRequest {
    pub path: String,
    pub bytes: u64,
    pub fingerprint: String,
    pub set: String,
    pub name: String,
    pub description: String,
    pub delete_file_after: bool,
}

#[uniffi::remote(Record)]
pub struct SecretAccess {
    pub id: u64,
    pub at: Timestamp,
    pub host_id: HostId,
    pub session_id: Option<SessionId>,
    pub purpose: Option<String>,
    pub set: String,
    pub name: String,
}

#[uniffi::remote(Record)]
pub struct LinkedVault {
    pub address: String,
    pub fingerprint: String,
    pub backup: bool,
    pub secrets: bool,
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

// Skills

#[uniffi::remote(Record)]
pub struct SkillFile {
    pub path: String,
    pub data: Bytes,
    pub executable: bool,
}

#[uniffi::remote(Record)]
pub struct SkillsStatus {
    pub repo: Option<String>,
    pub head: Option<String>,
    pub last_pull: Option<Timestamp>,
    pub pull_error: Option<String>,
    pub skills: Vec<LibrarySkill>,
    pub reload: Vec<ProviderReload>,
    #[uniffi(default)]
    pub accounts: Vec<AccountSkills>,
}

#[uniffi::remote(Record)]
pub struct AccountSkills {
    pub account_id: AccountId,
    pub skills: Vec<SessionSkill>,
}

#[uniffi::remote(Record)]
pub struct LibrarySkill {
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub providers: Vec<Provider>,
}

#[uniffi::remote(Record)]
pub struct ProviderReload {
    pub provider: Provider,
    pub reload: SkillReload,
}

#[uniffi::remote(Enum)]
pub enum SkillReload {
    Live,
    NextTurn,
    NextSession,
}

#[uniffi::remote(Record)]
pub struct SessionSkill {
    pub name: String,
    pub description: String,
    pub source: SkillSource,
    pub path: Option<String>,
}

#[uniffi::remote(Enum)]
pub enum SkillSource {
    Library,
    Project,
    Account,
}

#[uniffi::remote(Record)]
pub struct HostReplication {
    pub host_id: HostId,
    pub sessions: u64,
    pub events: u64,
    pub last_event_at: Option<Timestamp>,
    pub lag_ms: Option<u64>,
    pub read: bool,
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
    pub parent_host: Option<HostId>,
    pub task: Option<String>,
    pub title: Option<String>,
    pub project_id: Option<ProjectId>,
    pub chat: bool,
    pub account_id: AccountId,
    pub children_need_you: u32,
    pub queue: Vec<QueuedPrompt>,
    pub threads: Vec<ThreadLink>,
    // Defaulted, so Swift and Kotlin code that builds a session head, as tests do, need not
    // name it.
    #[uniffi(default = None)]
    pub origin: Option<SessionOrigin>,
}

#[uniffi::remote(Record)]
pub struct QueuedPrompt {
    pub prompt_id: PromptId,
    pub text: String,
    pub images: u32,
    // Defaulted, so Swift and Kotlin code that builds a queued prompt, as tests do, need not
    // name it.
    #[uniffi(default = 0)]
    pub files: u32,
    pub by: Option<UserId>,
    pub agent_message: Option<AgentMessage>,
}

#[uniffi::remote(Record)]
pub struct Account {
    pub account_id: AccountId,
    pub provider: Provider,
    pub label: String,
    pub config_dir: Option<String>,
    pub email: Option<String>,
    pub usage: Vec<UsageWindow>,
    // Defaulted, so Swift and Kotlin code that builds an account, as tests do, need not name it.
    #[uniffi(default)]
    pub fallback: bool,
    #[uniffi(default)]
    pub needs_login: Option<Timestamp>,
}

#[uniffi::remote(Record)]
pub struct ProviderStatus {
    pub provider: Provider,
    pub installed: bool,
    pub version: Option<String>,
    // Defaulted, so Swift and Kotlin code that builds a status, as tests do, need not name it.
    #[uniffi(default)]
    pub latest_version: Option<String>,
    pub binary: Option<String>,
    pub can_install: bool,
    pub can_update: bool,
}

#[uniffi::remote(Record)]
pub struct FailoverSettings {
    pub pin: bool,
}

#[uniffi::remote(Record)]
pub struct DaemonSettings {
    pub listen: Vec<String>,
    pub log: LogSettings,
    pub binaries: Vec<ProviderBinary>,
    pub failover: FailoverSettings,
    pub titles: TitleSettings,
    pub resources: ResourceSettings,
    pub projects: ProjectDiscovery,
    pub backup: BackupSettings,
    pub follow_ups: FollowUpSettings,
    pub mcp_servers: Vec<McpServerSettings>,
}

#[uniffi::remote(Record)]
pub struct McpServerSettings {
    pub name: String,
    pub enabled: bool,
    pub project: Option<ProjectId>,
    pub transport: McpTransport,
}

#[uniffi::remote(Enum)]
pub enum McpTransport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
        secrets: Vec<SecretRef>,
    },
    Http {
        url: String,
        headers: HashMap<String, String>,
    },
    Sse {
        url: String,
        headers: HashMap<String, String>,
    },
}

#[uniffi::remote(Record)]
pub struct LogSettings {
    pub level: String,
    pub format: LogFormat,
}

#[uniffi::remote(Enum)]
pub enum LogFormat {
    Pretty,
    Json,
}

#[uniffi::remote(Record)]
pub struct ProviderBinary {
    pub provider: Provider,
    pub binary: String,
}

#[uniffi::remote(Record)]
pub struct TitleSettings {
    pub enabled: bool,
    pub provider: Option<Provider>,
    pub model: Option<String>,
    pub account: Option<AccountId>,
}

#[uniffi::remote(Record)]
pub struct ResourceSettings {
    pub memory_max_percent: u8,
    pub memory_high_percent: u8,
    pub cpu_weight: u16,
    pub child_cpu_weight: u16,
    pub nice: i8,
    pub max_turns: Option<u32>,
    pub min_memory_available_mib: u64,
    pub max_memory_pressure: u8,
    pub max_load_percent: u16,
}

#[uniffi::remote(Record)]
pub struct ProjectDiscovery {
    pub dir: String,
    pub exclude: Vec<String>,
    pub setup_timeout_secs: u64,
}

#[uniffi::remote(Record)]
pub struct BackupSettings {
    pub attachments: bool,
    pub attachments_cap: u64,
    pub archive_retention_days: u32,
}

#[uniffi::remote(Record)]
pub struct FollowUpSettings {
    pub pr_events: bool,
    pub stall_after_secs: u64,
    pub max_stall_nudges: u32,
}

#[uniffi::remote(Record)]
pub struct UsageWindow {
    pub window: String,
    pub used_percent: f64,
    pub resets_at: Option<Timestamp>,
}

#[uniffi::remote(Record)]
pub struct TurnUsage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost_usd: Option<f64>,
    pub cost_estimated: bool,
}

#[uniffi::remote(Enum)]
pub enum UsagePeriod {
    Day,
    Week,
    ThirtyDays,
    Month,
}

#[uniffi::remote(Record)]
pub struct UsageTotal {
    pub account_id: AccountId,
    pub provider: Provider,
    pub model: String,
    pub turns: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost_usd: f64,
    pub cost_estimated: bool,
}

#[uniffi::remote(Record)]
pub struct FailoverTotal {
    pub account_id: AccountId,
    pub limit_hits: u64,
    pub failovers_out: u64,
    pub failovers_in: u64,
}

#[uniffi::remote(Record)]
pub struct Terminal {
    pub terminal_id: TerminalId,
    pub purpose: TerminalPurpose,
}

#[uniffi::remote(Enum)]
pub enum TerminalPurpose {
    Shell {
        session_id: SessionId,
    },
    Login {
        account_id: AccountId,
    },
    Install {
        provider: Provider,
    },
    AwsLogin {
        profile: String,
        device: Option<AwsDeviceCode>,
    },
}

#[uniffi::remote(Record)]
pub struct AwsDeviceCode {
    pub verification_url: String,
    pub user_code: String,
}

#[uniffi::remote(Record)]
pub struct AwsProfile {
    pub name: String,
    pub sso: bool,
}

#[uniffi::remote(Record)]
pub struct Project {
    pub project_id: ProjectId,
    pub name: String,
    pub paths: Vec<String>,
    // Defaults to nil so Swift and Kotlin code building a `Project` need not name it.
    #[uniffi(default)]
    pub remote: Option<String>,
    pub default_permission_mode: Option<PermissionMode>,
    pub default_account: Option<AccountId>,
    pub setup_command: Option<String>,
    // Defaults to nil so Swift and Kotlin code building a `Project` need not name it.
    #[uniffi(default)]
    pub icon: Option<String>,
    #[uniffi(default)]
    pub icon_uploaded: bool,
    #[uniffi(default)]
    pub icon_background: Option<String>,
    #[uniffi(default)]
    pub private_artifacts: bool,
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
pub enum UpdateWhen {
    Now,
    Idle,
}

#[uniffi::remote(Enum)]
pub enum UpdateStage {
    WaitingForIdle { running_turns: u32 },
    Downloading { from: String, to: String },
    Restarting { from: String, to: String },
    UpToDate { version: String },
    Failed { reason: String },
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
