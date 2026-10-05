//! Messages a client sends to the daemon.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AccountId, Answer, ApprovalDecision, ApprovalId, AttachmentId, Bytes, CommandId,
    DaemonSettings, Event, HostId, Image, PermissionMode, ProjectId, PromptId, Provider,
    QuestionId, Seq, SessionId, TerminalId, UsagePeriod,
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

/// Where a relayed history comes from: the host the client read it from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Relay {
    /// The host the session runs on.
    pub host_id: HostId,
    /// The session's project, as that host lists it; the fork works in this host's clone.
    pub project_id: ProjectId,
}

/// A part of a relayed history; see `upload_history`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HistoryPart {
    /// Events of the session's journal, in seq order, following the parts before.
    Events {
        /// The events.
        events: Vec<Event>,
    },
    /// An image a prompt of the session carried, as `get_attachment` answers for it.
    Image {
        /// The image, as the prompt's `user_message` names it.
        attachment_id: AttachmentId,
        /// Its bytes.
        image: Image,
    },
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
    /// Fork a session onto this daemon's host: copy its history into a new session that goes
    /// on here, in a new worktree on a new branch restored from the session's latest
    /// checkpoint; owners only. With `relay`, the history is the one the caller uploaded with
    /// `upload_history`; without, the session is looked up on this daemon, else in the vault
    /// it replicates to, whether its own host is up or gone. The original is left as it is.
    /// Answered with `session_forked`; the fork's journal marks it with the event
    /// `session_forked`, `by` the forking user. A task's child cannot be forked.
    ForkSession {
        /// The session to fork, as this daemon, its vault or the relayed history has it.
        session_id: SessionId,
        /// Account the fork runs on; when absent, the session's account if this host has it,
        /// else its project's default account, else this host's first account of its provider.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account_id: Option<AccountId>,
        /// Where the history the caller uploaded comes from; absent to look the session up
        /// here or in the vault.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        relay: Option<Relay>,
    },
    /// Upload part of the history of another host's session, which the client read from that
    /// host, for a `fork_session` with `relay` to fork; owners only. Parts add up per user
    /// and session until that fork takes them; events starting at seq 1 start the upload over.
    /// Parts not forked within ten minutes of the last one are dropped. Each part stays well
    /// under a WebSocket message's size limit: a batch of events, or one image.
    UploadHistory {
        /// The session the history is of.
        session_id: SessionId,
        /// The part.
        part: HistoryPart,
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
    /// Drop a prompt from the session's queue without running it. Refused with `conflict`
    /// once it has started, and `not_found` for an unknown prompt.
    RemoveQueued {
        /// Target session.
        session_id: SessionId,
        /// The prompt, as the session's `queue` lists it.
        prompt_id: PromptId,
    },
    /// Move a prompt within the session's queue. Refused as `remove_queued` is, for either
    /// prompt.
    MoveQueued {
        /// Target session.
        session_id: SessionId,
        /// The prompt to move.
        prompt_id: PromptId,
        /// The queued prompt it is to run just before; absent moves it to the end.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<PromptId>,
    },
    /// Run a queued prompt next, ahead of the rest, which stay queued in order: interrupts the
    /// running turn, or cancels a retry waiting for a usage limit to reset, as `interrupt`
    /// does. Refused as `remove_queued` is.
    SendQueuedNow {
        /// Target session.
        session_id: SessionId,
        /// The prompt to run next.
        prompt_id: PromptId,
    },
    /// Merge queued prompts into one, so they run as one turn. The merged prompt keeps the
    /// first listed prompt's id and place in the queue; the rest leave it. Its text is theirs,
    /// in the listed order, with a blank line between them, and its images are theirs, in the
    /// same order, with each prompt's `[Image #N]` markers renumbered to count across the
    /// merged prompt. Refused as `remove_queued` is, for any listed prompt; and with
    /// `bad_request` for fewer than two prompts, a prompt listed twice, a prompt an agent sent,
    /// prompts different users sent, or images over [`crate::MAX_PROMPT_IMAGE_BYTES`] together.
    MergeQueued {
        /// Target session.
        session_id: SessionId,
        /// The prompts to merge, as the session's `queue` lists them, in the order their texts
        /// are to follow each other.
        prompt_ids: Vec<PromptId>,
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
    /// Add an account: run the provider's own login in its config dir, in a login terminal
    /// this connection is attached to; owners only. The account joins the account list once
    /// the provider reports the dir logged in, which may already be so.
    AddAccount {
        /// Id of the new account; unique on this daemon.
        account_id: AccountId,
        /// Provider to log in to.
        provider: Provider,
        /// Display label; the id when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// Absolute config dir on the host, or one starting with `~/`; the daemon picks one
        /// in the home directory when absent. It may hold a login already, but not be another
        /// account's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config_dir: Option<String>,
        /// Width in columns.
        cols: u16,
        /// Height in rows.
        rows: u16,
    },
    /// Update an existing account's label and config directory; owners only. The id and
    /// provider stay fixed. Directory changes require all sessions on this daemon archived.
    /// Omit config_dir to use the provider's default login. Answered with applied and accounts.
    SetAccountSettings {
        /// Account to configure.
        account_id: AccountId,
        /// Display label (non-empty).
        label: String,
        /// Config directory on the host; never credentials.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config_dir: Option<String>,
    },
    /// Change how many turns the host runs at once, live; owners only. Raising it starts
    /// waiting turns at once; lowering it stops no running turn, only new ones from starting.
    /// The daemon keeps it as `max_turns` in the `[resources]` table of its config and sends
    /// every client the new `host_resources`. Refused with `bad_request` outside 1 to
    /// [`MAX_TURNS_LIMIT`](crate::MAX_TURNS_LIMIT), and with `unsupported` on a vault.
    SetResourceLimits {
        /// Most turns running at once.
        max_turns: u32,
    },
    /// Read the daemon's settings as its config file holds them; owners only. Answered with
    /// `settings`. It changes nothing, so a resend is answered afresh.
    GetSettings,
    /// Change the daemon's settings to `settings`; owners only. Only the values that differ
    /// from the config file's are written, in place, keeping the rest of the file as it is.
    /// Answered with `settings`: a change to `resources.max_turns` applies at once, the rest
    /// once the daemon restarts. Refused with `bad_request`, changing nothing, when a value is
    /// out of range, names an unknown account, or `listen` is not an address of this machine.
    SetSettings {
        /// Every setting, as it is to be.
        settings: Box<DaemonSettings>,
    },
    /// Stop the daemon cleanly and start it again, so its config file applies in full;
    /// owners only. Answered with `applied` before it stops; clients reconnect as after any
    /// restart, and turns that were running resume as they do then.
    RestartDaemon,
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
        /// Colour drawn behind the project's icon, as `#rrggbb`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon_background: Option<String>,
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
    /// Set or clear a project's uploaded icon on this host, kept in the daemon's data dir;
    /// owners only. An uploaded icon wins over the `[[project]]` entry's `icon` and over the
    /// files found in the clone. Answered with `applied`; the project list follows with the
    /// new `icon` and `icon_uploaded`. Refused with `bad_request` when the media type is not
    /// one of [`crate::PROJECT_ICON_MEDIA_TYPES`], or the data is empty or over
    /// [`crate::MAX_PROJECT_ICON_BYTES`].
    SetProjectIcon {
        /// The project, one of this daemon's.
        project_id: ProjectId,
        /// The image to use; absent clears the upload, so the icon is found in the clone again.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<Image>,
    },
    /// Fetch a project's icon, the image its `icon` names; owners and members alike. Answered
    /// with `project_icon`, or refused with `not_found` when the project has none. It changes
    /// nothing, so a resend is answered afresh.
    GetProjectIcon {
        /// The project, one of this daemon's.
        project_id: ProjectId,
    },
    /// Add up the tokens and cost of the turns completed on this daemon's host over `period`,
    /// per account and model; owners and members alike. Answered with `usage_summary`. Each
    /// daemon answers for its own host; clients add the answers of their machines up. It
    /// changes nothing, so a resend is answered afresh.
    GetUsageSummary {
        /// The period, ending now.
        period: UsagePeriod,
    },
    /// Say where this daemon backs its sessions up; owners only. Answered with `vault_link`.
    /// It changes nothing, so a resend is answered afresh.
    GetVaultLink,
    /// Back this host's sessions up to a vault from now on, without a restart; owners only.
    /// The daemon connects to the vault at the first of `addresses` that answers, pairs with
    /// `pairing_code` and replicates every session there; only once the vault accepted it
    /// does it keep the vault as the `[vault]` table of its config. Refused with `conflict`
    /// while it backs up to a vault already, and with `unsupported` on a vault.
    LinkVault {
        /// The vault's addresses as `host:port`, tried in order.
        addresses: Vec<String>,
        /// SHA-256 of the vault's TLS certificate, lowercase hex.
        fingerprint: String,
        /// A host-only code from the vault's `pair_vault_host`.
        pairing_code: String,
    },
    /// Stop backing this host's sessions up: stop replicating and remove the `[vault]` table
    /// of its config; owners only. What the vault holds stays there. Refused with `not_found`
    /// when it backs up nowhere.
    UnlinkVault,
    /// Mint a one-time code, as `herder pair` does, that pairs another device as the caller's
    /// own user with the caller's role, so a shared code never grants more than the sharer
    /// has; owners and members alike. Answered with `device_pairing`.
    PairDevice,
    /// On a vault, mint a one-time code that pairs a host to replicate here and only that,
    /// as `herder pair --host` does; owners only. Answered with `host_pairing`.
    PairVaultHost {
        /// The host's name; the user its device acts as on the vault.
        host_name: String,
    },
    /// On a vault, unpair every device that replicates as `host_id`, closing its connections;
    /// owners only. The host's sessions stay on the vault. Refused with `not_found` when no
    /// paired device replicates as it.
    RevokeVaultHost {
        /// The host.
        host_id: HostId,
    },
    /// Use the git repository at `url` as the skill library: the daemon replaces its checkout
    /// with a clone of it; owners only. Answered with `applied`; `skills_status` follows.
    SetSkillsRepo {
        /// The repository's git URL, as `git clone` takes it.
        url: String,
    },
    /// Add a skill to the library, or replace one, with exactly `files`, committed and pushed;
    /// owners only. Answered with `applied` once pushed; `skills_status` follows, and the
    /// client sends `pull_skills` to its other machines. Refused with `bad_request` for a name
    /// [`crate::is_valid_skill_name`] refuses, files without a top-level `SKILL.md`, an
    /// invalid path, or files over [`crate::MAX_SKILL_BYTES`] together; with `not_found` when
    /// no library is set.
    PutSkill {
        /// The skill's name, its folder in the library.
        name: String,
        /// Every file of the skill's folder.
        files: Vec<crate::SkillFile>,
    },
    /// Remove a skill from the library, committed and pushed; owners only. Answered as
    /// `put_skill` is; refused with `not_found` for a skill the library does not have.
    DeleteSkill {
        /// The skill.
        name: String,
    },
    /// Copy a skill folder out of another git repository into the library, named after the
    /// folder, committed and pushed; owners only. Answered as `put_skill` is; refused with
    /// `bad_request` when the folder has no `SKILL.md` or its name is not a valid skill name.
    ImportSkill {
        /// The repository to copy it from, as `git clone` takes it.
        git_url: String,
        /// The skill's folder within that repository; its top when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// Pull the library into the daemon's checkout now; owners only. Answered with `applied`
    /// once pulled; `skills_status` follows with the new head, or the pull's error.
    PullSkills,
    /// Enable or disable a library skill on this machine only; owners only. A disabled skill
    /// reaches no provider here. Answered with `applied`; `skills_status` follows. Refused with
    /// `not_found` for a skill the library does not have.
    SetSkillEnabled {
        /// The skill.
        name: String,
        /// Whether it is to be enabled.
        enabled: bool,
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
