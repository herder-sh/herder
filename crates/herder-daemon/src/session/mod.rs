//! Sessions: create, list and drive them, journaling everything that happens.
//!
//! [`SessionManager`] is the daemon's single entry point for session commands, from any
//! client. Each live session is one task (an actor) that owns the session's
//! [`AdapterSession`](herder_adapters::AdapterSession) and takes commands from one ordered
//! queue, so prompts from several clients land in one ordered history. The actor turns
//! adapter events into journal appends and publishes every stored event, plus the ephemeral
//! snapshots and deltas of streaming items, to an [`EventSink`].
//!
//! # Turns and queued prompts
//!
//! The daemon mints turn ids. A prompt sent while a turn runs is accepted and queued: queued
//! prompts start in arrival order, each as soon as the previous turn ends, however it ended.
//! A prompt is journaled (as a `user_message` item) when its turn starts, not when it is
//! queued, so every turn's items stay together in the journal. Queued prompts are saved in the
//! store, apart from the journal, before the prompt is answered and again as each one starts,
//! so a restart neither loses nor repeats one ([`SessionManager::resume`]).
//!
//! [`SessionManager::handle_once`] remembers each accepted command's result in the store, so
//! a client resending a command after a daemon restart gets its first answer instead of
//! applying it twice.
//!
//! # Status
//!
//! `idle` → `running` when a turn starts, or `waiting_for_capacity` first while the host has no
//! room for it (see Resources); `needs_you` while an approval or a question is
//! pending or after a failed turn; `error` after the agent exited with an error; `idle` once a turn ends with
//! nothing queued. Every change is journaled as `session_status_changed`.
//!
//! # Approvals
//!
//! An approval request is journaled as `approval_requested` and so reaches every subscribed
//! client. A top-level session's requests are routed to the user, a child's to its primary
//! session unless they are the user's to decide ([`routing`]). The session is `needs_you`
//! while any open request is routed or escalated to a user, and back to `running` once none
//! is; a turn may have several open at once. The first answer to an open request wins, a
//! user's `answer_approval` or the primary session's `answer`: it is journaled as
//! `approval_resolved` (`by` the answering user, or `answered_by` the primary with no `by`) and
//! then sent to the agent. A user's later answer is refused as a `conflict` (already
//! resolved), one for an id never requested as `not_found`. When a turn ends, however it ended, its open requests are journaled as
//! `expired` with no `by`, since no answer can reach the agent any more.
//!
//! # Worktrees and archive
//!
//! Creating a session adds its worktree and branch ([`crate::worktree`]). Every other branch
//! checked out in the worktree is journaled as `branch_checked_out` when a turn ends and before
//! the worktree is removed, so the session keeps owning it once the worktree, and the reflog it
//! was read from, are gone. `archive_session` ([`SessionManager::archive`]) stops the session
//! and journals the `archived` status, leaving the worktree as it is: archiving runs no git, so
//! nothing in the worktree can stop it. [`SessionManager::remove_archived_worktrees`], run
//! every [`SWEEP_INTERVAL`], removes the worktree of each session archived for
//! [`KEEP_ARCHIVED_WORKTREE`], whatever it holds, keeping its branches. An archived session
//! takes no further commands but `unarchive_session`, which goes on in the worktree when it is
//! still there, else adds it back at the path it had, on the session's own branch, and
//! journals the `idle` status.
//!
//! # Images and files
//!
//! A prompt may carry images when its session's adapter takes them
//! ([`herder_adapters::Adapter::accepts_images`]); otherwise it is refused as `unsupported`.
//! It may carry files of any type whatever the adapter. Both are checked and kept when the
//! prompt arrives ([`attachments`]), each file in a folder of the session outside its
//! worktree, and journaled as the attachments of its `user_message`. When its turn starts,
//! the images go to the agent with the prompt's text, and the files' paths in a note appended
//! to it: the CLI runs on this host, so any agent can read them. `get_attachment` reads one
//! back; it changes nothing, so its answer is not remembered ([`changes_nothing`]). A
//! transcript replayed into another CLI keeps each prompt's attachments, and the adapter names
//! every image and file in a line of text: the seed carries no bytes, and the vault keeps none
//! either, so a forked session's earlier attachments are references only.
//!
//! # Checkpoints
//!
//! Once [`SessionManager::checkpoint_turns`] runs, every agent turn's end, however it ended,
//! commits the worktree to `refs/herder/<session>/<turn>` before the next turn starts, then
//! pushes it to `origin` or bundles it in the background ([`checkpoint`]). The setup turn
//! makes none. A checkpoint that fails is logged and never fails the turn.
//!
//! # Setup
//!
//! Once [`SessionManager::set_up_worktrees`] runs, a new session whose project has a
//! `setup_command` runs it with `sh -c` once in its new worktree, in the session's resource
//! scope, before any turn. It is journaled as a turn of its own, made by herder (no `by`): a
//! `tool_call` item named `herder_setup` with `{"command": ...}` as its input, then its
//! `tool_result` with the output's tail, stdout and stderr interleaved, and `turn_completed`.
//! Prompts sent meanwhile queue and start once it succeeded. A non-zero exit, a timeout
//! ([`ProjectsConfig::setup_timeout`]) or an `interrupt` kills the command's process group,
//! fails the turn as `fatal` with the output's last lines and leaves the session `error`; the
//! prompts queued behind it are dropped. A later prompt runs it again, as a turn of its own,
//! before it starts the agent, until it succeeds. `archive_session` is refused while it runs.
//! A daemon restart closes a setup it left running like any open turn; the next prompt runs it
//! again the same way.
//!
//! # Pull requests
//!
//! Once [`SessionManager::track_prs`] runs, each new worktree gets herder's git hooks, archive
//! removes them, and `link_pr` / `unlink_pr` go to the tracker ([`crate::prs`]), which journals
//! pull request events alongside the session's actor.
//!
//! # MCP
//!
//! Once [`SessionManager::serve_mcp`] runs, each start of a session's CLI grants it a fresh
//! token for herder's MCP server ([`crate::mcp`]) and registers that server with the CLI;
//! archive withdraws it.
//!
//! # Skills
//!
//! Once [`SessionManager::deliver_skills`] runs, the skill library commands go to the library
//! ([`crate::skills`]), each start of a session's CLI hands it the enabled library skills
//! ([`herder_adapters::StartRequest::skills`]) and sends the session's skills, and archive
//! clears them.
//!
//! # Tasks
//!
//! Through the MCP server's task tools a session becomes a task's primary and spawns child
//! sessions; each child's turn ends with a report to the primary ([`tasks`]).
//!
//! # Resources
//!
//! Once [`SessionManager::limit_resources`] runs, each start of a session's CLI runs it in a
//! systemd scope of its own ([`crate::resources`]); a child's scope gets the smaller CPU
//! weight.
//!
//! Once [`SessionManager::admit_turns`] runs, every turn needs a permit from
//! [`Admission`] before it starts, whichever session it is in. A turn the host has no room for
//! keeps its prompt queued, journals `waiting_for_capacity`, and starts once its permit
//! arrives, in the order the turns asked; a session with prompts left after a turn asks
//! again, behind every turn already waiting. A turn's end frees its permit, and a turn blocked
//! in `wait_for` lends it to other turns until the call returns ([`tasks`]). `spawn` is refused
//! as `host_busy` while memory, load or pressure binds. Waiting prompts are kept across a
//! restart like every queued prompt: the session stays `waiting_for_capacity` and asks again
//! once the manager resumes.
//!
//! An owner changes the turn limit live from a client ([`crate::settings`]): [`Admission`]
//! applies it at once and the daemon's config file keeps it.
//!
//! When the agent's CLI fails after the kernel's OOM killer or systemd-oomd killed in its scope
//! ([`Scopes::oom_killed`]), the turn fails with an error that says so and the session is
//! `error`; the CLI is stopped, and the next prompt starts a new one seeded with the transcript,
//! as after a restart. A CLI killed between turns, such as while its background agents ran,
//! fails a turn herder starts for it (no `by`, no prompt), so the transcript still says so.
//!
//! Once [`SessionManager::track_containers`] runs, owners can bring down a Compose project
//! that one of a session's tracked containers belongs to.
//!
//! # Questions
//!
//! A question the agent asks is journaled as `question_asked`, routed like an approval request
//! but never beyond the primary's authority, and blocks the turn until a user's
//! `answer_question` or the primary's `answer` answers it, whichever comes first. A turn's end
//! drops its open questions.
//!
//! # Switching model, account and provider
//!
//! `set_model` switches natively through the running CLI and journals `model_switched`.
//! `switch_account` (another account of the session's provider) and `switch_provider` (an
//! account of another provider, on the given model or that provider's default) move the
//! session to a fresh CLI instead: they apply only between turns and are refused as a
//! `conflict` while a turn runs, since a CLI cannot be swapped under a turn; interrupt it or
//! wait for it to end. Prompts are never held back for a switch: one sent after it starts on
//! the new account. The switch stops the current CLI, waiting for it to exit, and journals
//! `account_switched` or `provider_switched` `by` the user; the next prompt starts the target
//! account's CLI seeded with the journal's transcript ([`crate::handoff`]), the same way a
//! restart resumes. An account switch resumes the CLI's own session instead, with its full
//! context and tool state, when its adapter reported the session's id
//! ([`herder_adapters::AdapterEvent::SessionIdentified`], kept in the store with the account it
//! ran on): that session's one transcript file is copied into the new account's config dir
//! ([`crate::handoff::native`]) and the CLI started with [`herder_adapters::StartRequest::resume`].
//! When the copy or that start fails, the transcript is replayed after all.
//!
//! # Failover
//!
//! When a turn fails with `limit_reached`, and only then, the session fails over: it rotates
//! to the available account of its provider with the most room left ([`failover`]), switching
//! the same way, journaling the switch with no
//! `by` since the daemon caused it, and retries the failed turn's prompt there, once, ahead of
//! any queued prompt. The failed turn stays journaled and its partial items are replayed with
//! the transcript. A child does not report the failed turn to its primary, only the retry. The
//! account that hit its limit is passed over by every session until it resets. Failover only
//! moves to an account of the session's own provider and keeps the session's model: the retry
//! starts that account's CLI on it. Every account takes part, but a fallback one
//! ([`AccountConfig::fallback`]) only once no other is available. With no available
//! account, with the session pinned (its `failover_pin`, else [`FailoverConfig::pin`]), or when
//! the retry hits a limit too, the session is `needs_you` with the limit error; when the retry
//! fails otherwise, as when the account rejects the model, it is `needs_you` with that error,
//! and nothing else is tried.
//!
//! # Titles
//!
//! `rename_session`, from an owner or a member, journals `title_changed` with source `user`
//! `by` that user, trimmed as [`herder_protocol::clean_title`] accepts it, and resends the
//! session list. It is refused as `bad_request` for an invalid title and as a `conflict` once
//! the session is read-only; renaming to the user title it already has journals nothing.
//!
//! Once [`SessionManager::generate_titles`] runs, a small model titles sessions from their
//! conversation, automatically as `auto` and at a `retitle_session` as `ai_requested`
//! ([`titles`]). Renames and generated titles are journaled one at a time, so a title generated
//! meanwhile never replaces a rename it should not.
//!
//! `retitle_session`, from an owner or a member, is refused the same way for a read-only
//! session, and as `unsupported` while titles are not generated, are disabled, or no CLI can
//! title the session; it is accepted once the run has started, and a run that fails changes
//! nothing.
//!
//! # Forks
//!
//! Any session, of this host or, through the vault, of another one whose host is up or gone,
//! can be forked here: its history goes on in a new session ([`SessionManager::fork`], see
//! [`fork`]), and the original is left as it is. A copy of a session that another host took
//! over under the same id, as the vault shows it, is stopped and made `moved`, read-only
//! ([`SessionManager::moved_away`]).
//!
//! # Restart
//!
//! Sessions are read from the store. A turn left open by a daemon that stopped is closed with
//! a `transient` `turn_failed` when the manager opens, before any prompt it left queued starts, after expiring its open approvals the
//! same way as at a turn's end: the CLI that asked is gone. A session's adapter starts lazily on its
//! next prompt, seeded with the journal's items, condensed to fit the model
//! ([`crate::handoff`]).

mod actor;
mod attachments;
pub mod failover;
pub mod fork;
pub(crate) mod journal;
mod merge;
mod routing;
mod setup;
mod tasks;
pub mod titles;

pub use routing::{Escalation, Notifier};

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

use anyhow::Context;
use herder_adapters::Adapter;
use herder_protocol::{
    Account, AccountId, Attachment, AttachmentId, Bytes, CommandBody, CommandId, CommandResult,
    ErrorClass, ErrorCode, ErrorInfo, Event, EventBody, FollowUp, HostId, Image, Item, ItemId,
    JournalRecord, MAX_PROJECT_ICON_BYTES, MAX_TITLE_CHARS, PROJECT_ICON_MEDIA_TYPES,
    PermissionMode, Project, ProjectId, Provider, Seq, SessionHead, SessionId, SessionStatus,
    SessionSummary, Timestamp, TitleSource, TurnId, UsageWindow, UserId, clean_title,
};
use herder_store::{Session, Store};
use jiff::SignedDuration;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use actor::{Actor, Request, SessionCommand, Switch};
pub use failover::FailoverConfig;
use failover::Limits;
use journal::Journal;
use tasks::{TaskTools, Tasks};
use titles::Titler;
pub use titles::{TitleCli, TitleClis, TitlesConfig};

use crate::config::ProjectSettings;
use crate::mcp::{self, Control, Mcp};
use crate::projects::{self, Overrides};
use crate::prs::{self, PrTracker};
use crate::resources::{Admission, Docker, Scopes};
use crate::skills::Skills;
use crate::stalls::{self, Stalls};
use crate::usage::{self, Usage};
use crate::worktree::{self, Worktrees, checkpoint};

/// How long an archived session's worktree is kept before it is removed.
pub const KEEP_ARCHIVED_WORKTREE: SignedDuration = SignedDuration::from_hours(3 * 24);

/// How often archived sessions' worktrees are checked for removal.
pub const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Where a session manager publishes what clients should see. Calls for one session arrive in
/// order; implementations must not block.
pub trait EventSink: Send + Sync + 'static {
    /// A durable event, after it is stored.
    fn event(&self, event: &Event);
    /// An item started streaming: its state so far. Later deltas append to it.
    fn snapshot(&self, session_id: &SessionId, item: &Item);
    /// Text appended to a streaming item; ephemeral, never journaled.
    fn delta(&self, session_id: &SessionId, item_id: &ItemId, text: &str);
    /// The session list changed: a session was created, or its status, account or project
    /// changed; carries the new list.
    fn sessions_changed(&self, sessions: &[SessionHead]);
    /// An account's usage changed ([`crate::usage`]); carries every account. Calls arrive in
    /// order.
    fn accounts_changed(&self, accounts: &[Account]);
}

/// A provider account on this host: a login the provider's CLI keeps in a config dir.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountConfig {
    /// Provider the account belongs to; picks the adapter.
    pub provider: Provider,
    /// Display label chosen by the owner.
    pub label: String,
    /// The account's config dir, handed to the adapter; `None` is the CLI's default location.
    pub config_dir: Option<PathBuf>,
    /// Whether picking an account for the provider passes it over while any of its other
    /// accounts is available ([`failover`]); naming it still runs on it.
    pub fallback: bool,
}

/// Every account sessions may run on, by id.
pub type Accounts = BTreeMap<AccountId, AccountConfig>;

/// One adapter per provider; sessions pick theirs by provider.
#[derive(Clone, Default)]
pub struct Adapters(HashMap<Provider, Arc<dyn Adapter>>);

impl Adapters {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers the adapter that runs `provider`'s sessions, replacing any earlier one.
    pub fn register(&mut self, provider: Provider, adapter: Arc<dyn Adapter>) {
        self.0.insert(provider, adapter);
    }

    pub(crate) fn get(&self, provider: &Provider) -> Option<Arc<dyn Adapter>> {
        self.0.get(provider).cloned()
    }
}

/// Mints the id of each new turn.
pub type TurnIds = Box<dyn Fn() -> TurnId + Send + Sync>;

/// Turn ids as ULIDs, the daemon's default.
pub fn ulid_turn_ids() -> TurnIds {
    Box::new(|| TurnId::new(ulid::Ulid::new().to_string()))
}

/// What a session manager runs on.
pub struct Setup {
    /// The journal.
    pub store: Store,
    /// Adapters by provider.
    pub adapters: Adapters,
    /// Accounts sessions may run on.
    pub accounts: Accounts,
    /// Where events go.
    pub sink: Arc<dyn EventSink>,
    /// Turn id minting; [`ulid_turn_ids`] outside tests.
    pub turn_ids: TurnIds,
    /// Where session worktrees go.
    pub worktrees: Worktrees,
    /// Where the images prompts carry are kept, a directory per session.
    pub attachments: PathBuf,
}

impl Inner {
    /// The account `account_id`, as it is now.
    pub(crate) fn account(&self, account_id: &AccountId) -> Option<AccountConfig> {
        self.accounts_lock().get(account_id).cloned()
    }

    fn accounts_lock(&self) -> std::sync::RwLockReadGuard<'_, Accounts> {
        // Every update is one insert, so a poisoned map is consistent.
        self.accounts.read().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Creates, lists and drives every session on this host. Cheap to clone.
#[derive(Clone)]
pub struct SessionManager {
    inner: Arc<Inner>,
}

struct Inner {
    journal: Journal,
    adapters: Adapters,
    /// Grows as accounts are added ([`SessionManager::add_account`]); never shrinks.
    accounts: RwLock<Accounts>,
    account_settings: Mutex<()>,
    turn_ids: TurnIds,
    worktrees: Worktrees,
    /// Where prompts' images are kept ([`attachments`]).
    attachments: PathBuf,
    actors: Mutex<HashMap<SessionId, mpsc::UnboundedSender<SessionCommand>>>,
    /// Pull request tracking, once started.
    prs: OnceLock<Arc<PrTracker>>,
    /// Stall watching, once started.
    stalls: OnceLock<Arc<Stalls>>,
    /// herder's MCP server, once started.
    mcp: OnceLock<Arc<Mcp>>,
    /// The daemon as agents drive it through `overview` and `command`, once set.
    control: OnceLock<Arc<dyn Control>>,
    /// Children's reports and requests waiting for their primaries.
    tasks: Tasks,
    /// Where children's requests that go to the user are announced, once set.
    notifier: OnceLock<Arc<dyn Notifier>>,
    /// The scopes sessions' CLIs run in, once set.
    scopes: OnceLock<Arc<Scopes>>,
    /// The containers sessions started, once tracked.
    docker: OnceLock<Arc<Docker>>,
    /// Every account's limit windows.
    usage: Usage,
    /// Asks the usage poller, once started, for fresh usage.
    refresh_usage: Arc<usage::Refresh>,
    /// What admits turns within the host's capacity, once set.
    admission: OnceLock<Arc<Admission>>,
    /// How sessions fail over, once set; the default otherwise.
    failover: OnceLock<FailoverConfig>,
    /// Accounts that hit a limit, until they reset.
    limits: Limits,
    /// This host and its projects' settings, once set.
    projects: OnceLock<(HostId, Arc<Overrides>)>,
    /// Where turn-end checkpoints go, once set.
    checkpoints: OnceLock<checkpoint::Config>,
    /// The skill library CLIs get, once set.
    skills: OnceLock<Arc<Skills>>,
    /// What generates session titles, once set.
    titler: OnceLock<Titler>,
    /// Held while a title is checked and journaled, so renames and generated titles apply one
    /// at a time.
    titling: Mutex<()>,
    /// Where forks find sessions, updated when the vault link changes ([`SessionManager::fork_from`]).
    forks: std::sync::RwLock<Option<Arc<fork::Forks>>>,
    /// Histories clients relay for forks, until the fork takes them.
    uploads: fork::Uploads,
    shutdown: CancellationToken,
}

impl Inner {
    /// Merges `windows` into the account's usage and, when that changed it, publishes every
    /// account.
    /// An account reporting its usage is logged in.
    pub(super) fn report_usage(&self, account_id: &AccountId, windows: Vec<UsageWindow>) {
        self.limits.worked(account_id);
        if let Some(usage) = self.usage.report(account_id, windows) {
            let accounts = crate::accounts::list(&self.accounts_lock(), &usage);
            self.journal.sink().accounts_changed(&accounts);
        }
    }

    /// Takes a usage probe's answer, as [`Self::report_usage`] does a session's report.
    fn probed_usage(&self, account_id: &AccountId, usage: herder_adapters::AccountUsage) {
        self.limits.worked(account_id);
        if let Some(usage) = self.usage.probed(account_id, usage) {
            let accounts = crate::accounts::list(&self.accounts_lock(), &usage);
            self.journal.sink().accounts_changed(&accounts);
        }
    }

    /// Notes that `account_id` hit its limit just now; failover passes it over until it resets.
    pub(super) fn limit_hit(&self, account_id: &AccountId) {
        let usage = self.usage.all();
        let windows = usage.windows.get(account_id).map_or(&[][..], Vec::as_slice);
        self.limits.hit(account_id, windows, Timestamp::now());
    }

    /// Notes what the turn that ended with `end` on `account_id` says about the account: a
    /// completed turn means it works, a failed login that failover passes it over until it
    /// does again, a spent limit that it waits for its reset.
    pub(super) fn turn_ended_on(&self, account_id: &AccountId, end: &EventBody) {
        match end {
            EventBody::TurnCompleted { .. } => self.limits.worked(account_id),
            EventBody::TurnFailed { error, .. } if error.class == ErrorClass::Auth => {
                self.limits.logged_out(account_id);
            }
            EventBody::TurnFailed { error, .. } if error.class == ErrorClass::LimitReached => {
                self.limit_hit(account_id);
            }
            _ => {}
        }
    }

    /// Latest future reset among exhausted windows; unknown reset times need user action.
    pub(super) fn limit_reset(&self, account_id: &AccountId) -> Option<Timestamp> {
        self.usage
            .all()
            .windows
            .get(account_id)?
            .iter()
            .filter(|window| window.used_percent >= 100.0)
            .filter_map(|window| window.resets_at)
            .filter(|at| *at > Timestamp::now())
            .max()
    }

    /// The available account of `provider` with the most room left, other than `except`.
    pub(super) fn available_account(
        &self,
        provider: &Provider,
        except: Option<&AccountId>,
    ) -> Option<AccountId> {
        let accounts = self.accounts_lock();
        let choice = failover::Choice {
            accounts: &accounts,
            adapters: &self.adapters,
            usage: &self.usage.all().windows,
            limits: &self.limits,
            now: Timestamp::now(),
        };
        failover::best(&choice, provider, except)
    }

    /// The setup command of `repo`'s project and how long it may run, if it has one.
    pub(super) async fn setup_command(&self, repo: &Path) -> Option<(String, std::time::Duration)> {
        let (host, overrides) = self.projects.get()?.clone();
        let config = overrides.config();
        let timeout = config.setup_timeout;
        let repo = repo.to_owned();
        let project =
            tokio::task::spawn_blocking(move || projects::of_repo(&host, &repo, &config.entries))
                .await
                .ok()??;
        Some((project.setup_command?, timeout))
    }

    /// Whether sessions stay on their account when it hits a limit.
    pub(super) fn pinned(&self) -> bool {
        self.failover.get().is_some_and(|config| config.pin)
    }
}

impl SessionManager {
    /// Opens the manager on `setup`, closing turns left open by a previous daemon. Sessions stop
    /// their adapters once `shutdown` is cancelled.
    pub async fn open(setup: Setup, shutdown: CancellationToken) -> anyhow::Result<Self> {
        let journal = Journal::new(setup.store, setup.sink);
        let tasks = Tasks::default();
        for session in journal.sessions().await? {
            if matches!(
                session.status,
                SessionStatus::Running
                    | SessionStatus::NeedsYou
                    | SessionStatus::WaitingForCapacity
            ) {
                actor::close_abandoned_turn(
                    &journal,
                    &tasks,
                    &session,
                    "the daemon stopped during this turn",
                    SessionStatus::NeedsYou,
                )
                .await?;
            }
        }
        Ok(Self {
            inner: Arc::new(Inner {
                journal,
                adapters: setup.adapters,
                accounts: RwLock::new(setup.accounts),
                account_settings: Mutex::new(()),
                turn_ids: setup.turn_ids,
                worktrees: setup.worktrees,
                attachments: setup.attachments,
                actors: Mutex::new(HashMap::new()),
                prs: OnceLock::new(),
                stalls: OnceLock::new(),
                mcp: OnceLock::new(),
                control: OnceLock::new(),
                tasks,
                notifier: OnceLock::new(),
                scopes: OnceLock::new(),
                usage: Usage::default(),
                refresh_usage: Arc::default(),
                admission: OnceLock::new(),
                docker: OnceLock::new(),
                failover: OnceLock::new(),
                limits: Limits::default(),
                projects: OnceLock::new(),
                checkpoints: OnceLock::new(),
                skills: OnceLock::new(),
                titler: OnceLock::new(),
                titling: Mutex::new(()),
                forks: std::sync::RwLock::new(None),
                uploads: fork::Uploads::default(),
                shutdown,
            }),
        })
    }

    /// Applies a command from `by`, from whichever client sent it. Effects arrive at the sink.
    ///
    /// Command-id idempotency is the caller's ([`Self::handle_once`]): this applies every call.
    pub async fn handle(
        &self,
        by: UserId,
        command: CommandBody,
    ) -> Result<CommandResult, ErrorInfo> {
        // Restoring an archived session must not race a login-directory edit.
        let _settings = if matches!(&command, CommandBody::UnarchiveSession { .. }) {
            Some(self.inner.account_settings.lock().await)
        } else {
            None
        };
        let (session_id, request) = match command {
            CommandBody::CreateSession {
                repo,
                project_id,
                branch,
                account_id,
                provider,
                model,
                permission_mode,
                failover_pin,
            } => {
                let (repo, project) = self.resolve_repo(repo, project_id)?;
                let account_id =
                    self.pick_account(account_id, provider, &repo, project.as_ref())?;
                let permission_mode = permission_mode
                    .or(project.and_then(|project| project.default_permission_mode))
                    .unwrap_or(PermissionMode::Ask);
                let request = CreateRequest {
                    repo,
                    branch,
                    account_id,
                    model,
                    permission_mode,
                    parent: None,
                    task: None,
                    failover_pin,
                };
                let (session_id, _) = self.create_session(Some(by), request).await?;
                return Ok(CommandResult::SessionCreated { session_id });
            }
            CommandBody::SendPrompt {
                session_id,
                text,
                images,
                files,
            } => (
                session_id,
                Request::SendPrompt {
                    text,
                    images,
                    files,
                    queued: None,
                },
            ),
            CommandBody::GetAttachment {
                session_id,
                attachment_id,
            } => return self.attachment(&session_id, &attachment_id).await,
            CommandBody::UnarchiveSession { session_id } => (session_id, Request::Unarchive),
            CommandBody::ListDirectory { path } => {
                return tokio::task::spawn_blocking(move || crate::browse::list(&path))
                    .await
                    .map_err(|err| error(ErrorCode::Internal, format!("{err}")))?;
            }
            CommandBody::AddProject { path } => return self.add_project(&path).await,
            CommandBody::CloneProject { url, path } => {
                return self.clone_project(&url, path.as_deref()).await;
            }
            CommandBody::UploadHistory { session_id, part } => {
                self.upload_history(by, session_id, part).await?;
                return Ok(CommandResult::Applied);
            }
            CommandBody::ForkSession {
                session_id,
                account_id,
                relay,
            } => {
                let request = fork::Request {
                    session_id,
                    account_id,
                };
                let forked = self.fork(request, relay, Some(by)).await?;
                return Ok(CommandResult::SessionForked {
                    session_id: forked.session_id,
                    account_id: forked.account_id,
                    forked_from: forked.forked_from,
                    from_host_id: forked.from_host_id,
                });
            }
            CommandBody::SetProjectSettings {
                project_id,
                name,
                default_permission_mode,
                default_account,
                setup_command,
                icon_background,
            } => {
                let settings = ProjectSettings {
                    name,
                    default_permission_mode,
                    default_account,
                    setup_command,
                    icon_background,
                };
                return self.set_project_settings(&project_id, settings).await;
            }
            CommandBody::RemoveProject { project_id } => {
                return self.remove_project(&project_id).await;
            }
            CommandBody::SetProjectIcon { project_id, icon } => {
                return self.set_project_icon(&project_id, icon).await;
            }
            CommandBody::GetProjectIcon { project_id } => {
                return self.project_icon(&project_id).await;
            }
            CommandBody::Interrupt { session_id } => (session_id, Request::Interrupt),
            CommandBody::RemoveQueued {
                session_id,
                prompt_id,
            } => (session_id, Request::RemoveQueued { prompt_id }),
            CommandBody::MoveQueued {
                session_id,
                prompt_id,
                before,
            } => (session_id, Request::MoveQueued { prompt_id, before }),
            CommandBody::SendQueuedNow {
                session_id,
                prompt_id,
            } => (session_id, Request::SendQueuedNow { prompt_id }),
            CommandBody::MergeQueued {
                session_id,
                prompt_ids,
            } => (session_id, Request::MergeQueued { prompt_ids }),
            CommandBody::SetModel { session_id, model } => {
                (session_id, Request::SetModel { model })
            }
            CommandBody::SetPermissionMode { session_id, mode } => {
                (session_id, Request::SetPermissionMode { mode })
            }
            CommandBody::AnswerApproval {
                session_id,
                approval_id,
                decision,
            } => (
                session_id,
                Request::AnswerApproval {
                    approval_id,
                    decision,
                },
            ),
            CommandBody::AnswerQuestion {
                session_id,
                question_id,
                answer,
            } => (
                session_id,
                Request::AnswerQuestion {
                    question_id,
                    answer,
                },
            ),
            CommandBody::ArchiveSession { session_id } => {
                return self.archive(by, session_id).await;
            }
            CommandBody::RenameSession { session_id, title } => {
                return self.rename(by, session_id, &title).await;
            }
            CommandBody::RetitleSession { session_id } => {
                return self.retitle(by, session_id).await;
            }
            CommandBody::LinkPr { session_id, number } => {
                return self.prs()?.link(by, session_id, number).await;
            }
            CommandBody::UnlinkPr { session_id, number } => {
                return self.prs()?.unlink(by, session_id, number).await;
            }
            CommandBody::ComposeDown {
                session_id,
                project,
            } => return self.compose_down(&session_id, &project).await,
            CommandBody::SwitchAccount {
                session_id,
                account_id,
            } => (
                session_id,
                Request::Switch {
                    account_id,
                    to: Switch::Account,
                },
            ),
            CommandBody::SwitchProvider {
                session_id,
                account_id,
                model,
            } => (
                session_id,
                Request::Switch {
                    account_id,
                    to: Switch::Provider { model },
                },
            ),
            CommandBody::OpenTerminal { .. }
            | CommandBody::SetAccountSettings { .. }
            | CommandBody::AddAccount { .. }
            | CommandBody::InstallProvider { .. }
            | CommandBody::LogInAccount { .. }
            | CommandBody::AttachTerminal { .. }
            | CommandBody::DetachTerminal { .. }
            | CommandBody::ResizeTerminal { .. }
            | CommandBody::TerminalInput { .. }
            | CommandBody::PairDevice => {
                return Err(error(
                    ErrorCode::Unsupported,
                    "the session manager does not handle this command yet",
                ));
            }
            // The daemon's settings answer these ([`crate::settings`]).
            CommandBody::GetSettings
            | CommandBody::SetSettings { .. }
            | CommandBody::SetResourceLimits { .. }
            | CommandBody::RestartDaemon => {
                return Err(error(
                    ErrorCode::Unsupported,
                    "this daemon's settings are not changed from a client",
                ));
            }
            // The daemon's vault link answers these ([`crate::vault::Link`]).
            CommandBody::GetVaultLink
            | CommandBody::LinkVault { .. }
            | CommandBody::UnlinkVault => {
                return Err(error(
                    ErrorCode::Unsupported,
                    "this daemon cannot back up to a vault",
                ));
            }
            command @ (CommandBody::SetSkillsRepo { .. }
            | CommandBody::PutSkill { .. }
            | CommandBody::DeleteSkill { .. }
            | CommandBody::ImportSkill { .. }
            | CommandBody::PullSkills
            | CommandBody::SetSkillEnabled { .. }) => {
                let skills = self.inner.skills.get().ok_or_else(|| {
                    error(ErrorCode::Unsupported, "this daemon has no skill library")
                })?;
                return skills.command(command).await;
            }
            CommandBody::PairVaultHost { .. } | CommandBody::RevokeVaultHost { .. } => {
                return Err(error(
                    ErrorCode::Unsupported,
                    "this daemon is not a vault; pair hosts on the vault",
                ));
            }
            // Every user sees every session, children included, so owners and members get
            // the same totals.
            CommandBody::GetUsageSummary { period } => {
                let since = period.start(Timestamp::now());
                let totals = self
                    .inner
                    .journal
                    .usage_totals(since)
                    .await
                    .map_err(internal)?;
                let failovers = self
                    .inner
                    .journal
                    .failover_totals(since)
                    .await
                    .map_err(internal)?;
                return Ok(CommandResult::UsageSummary {
                    period,
                    since,
                    totals,
                    failovers,
                });
            }
        };
        self.send(session_id, Some(by), request).await
    }

    /// Applies `by`'s command `command_id` once, across daemon restarts too: a resend of a
    /// command accepted before is answered with its first result and not applied again. A
    /// rejected command changed nothing, so it is not remembered and a resend is tried afresh.
    ///
    /// The result is remembered after the command applied and before it is answered, so a
    /// daemon that stops in between can still apply a resend twice.
    pub async fn handle_once(
        &self,
        by: UserId,
        command_id: CommandId,
        command: CommandBody,
    ) -> Result<CommandResult, ErrorInfo> {
        if changes_nothing(&command) {
            return self.handle(by, command).await;
        }
        let journal = &self.inner.journal;
        let remembered = journal
            .command_result(by.clone(), command_id.clone())
            .await
            .map_err(internal)?;
        if let Some(result) = remembered {
            return Ok(result);
        }
        let result = self.handle(by.clone(), command).await?;
        if let Err(err) = journal
            .record_command_result(by, command_id.clone(), result.clone())
            .await
        {
            warn!(%command_id, "cannot remember an accepted command: {err:#}");
        }
        Ok(result)
    }

    /// Starts every session a previous daemon left prompts queued in, so they run without
    /// waiting for a command: call it once everything sessions run on is set up. The prompts
    /// start in order as the host admits them.
    pub async fn resume(&self) -> anyhow::Result<()> {
        for session_id in self.inner.journal.sessions_with_queued_prompts().await? {
            if let Err(err) = self.actor(&session_id).await {
                warn!(%session_id, "cannot resume the queued prompts: {}", err.message);
            }
        }
        Ok(())
    }

    /// Starts pull request tracking ([`crate::prs`]) for every session, until the manager's
    /// shutdown; once per manager.
    pub async fn track_prs(&self, config: prs::Config) -> anyhow::Result<Arc<PrTracker>> {
        let inner = &self.inner;
        if inner.prs.get().is_some() {
            anyhow::bail!("pull requests are tracked already");
        }
        let tracker = PrTracker::start(
            inner.journal.clone(),
            config,
            self.prompter(),
            self.on_merged(),
            inner.shutdown.clone(),
        )
        .await?;
        let _ = inner.prs.set(Arc::clone(&tracker));
        Ok(tracker)
    }

    /// Starts watching every session for stalls ([`crate::stalls`]) until the manager's
    /// shutdown; once per manager.
    pub fn watch_stalls(&self, config: stalls::Config) -> anyhow::Result<Arc<Stalls>> {
        let inner = &self.inner;
        if inner.stalls.get().is_some() {
            anyhow::bail!("stalls are watched already");
        }
        let watcher = Stalls::start(
            inner.journal.clone(),
            config,
            self.prompter(),
            inner.shutdown.clone(),
        );
        let _ = inner.stalls.set(Arc::clone(&watcher));
        Ok(watcher)
    }

    /// Sends follow-up prompts to idle sessions, while the manager lives.
    fn prompter(&self) -> prs::Prompter {
        let manager = Arc::downgrade(&self.inner);
        Box::new(move |session_id, text, follow_up| {
            let manager = manager.upgrade().map(|inner| Self { inner });
            Box::pin(async move {
                match manager {
                    Some(manager) => manager.follow_up(session_id, text, follow_up).await,
                    None => false,
                }
            })
        })
    }

    /// Tells the manager of merged pull requests, while it lives.
    fn on_merged(&self) -> prs::OnMerged {
        let manager = Arc::downgrade(&self.inner);
        Box::new(move |session_id| {
            let manager = manager.upgrade().map(|inner| Self { inner });
            Box::pin(async move {
                if let Some(manager) = manager {
                    manager.pr_merged(&session_id).await;
                }
            })
        })
    }

    /// One of `session_id`'s pull requests was merged: a child that is done is archived
    /// ([`tasks`]). Only a live child's actor is asked; nothing starts for anything else.
    pub async fn pr_merged(&self, session_id: &SessionId) {
        let live_child = matches!(
            self.inner.journal.session(session_id.clone()).await,
            Ok(Some(session))
                if session.parent.is_some() && session.status == SessionStatus::Idle
        );
        if live_child
            && let Err(err) = self
                .send(session_id.clone(), None, Request::ArchiveIfDone)
                .await
        {
            warn!(%session_id, "cannot archive the merged child: {}", err.message);
        }
    }

    /// Starts herder's MCP server ([`crate::mcp`]) with the task tools ([`tasks`]) until the
    /// manager's shutdown, and registers it with every session's CLI from its next start; once
    /// per manager.
    pub fn serve_mcp(&self, config: mcp::Config) -> anyhow::Result<()> {
        let inner = &self.inner;
        if inner.mcp.get().is_some() {
            anyhow::bail!("the MCP server runs already");
        }
        let tools = Arc::new(TaskTools {
            inner: Arc::downgrade(inner),
        });
        let _ = inner
            .mcp
            .set(Mcp::start(config, tools, inner.shutdown.clone())?);
        Ok(())
    }

    /// Lets agents drive the daemon through `control` with the `overview` and `command`
    /// tools; once per manager. Without it, those tools are unsupported.
    pub fn serve_control(&self, control: Arc<dyn Control>) -> anyhow::Result<()> {
        self.inner
            .control
            .set(control)
            .map_err(|_| anyhow::anyhow!("agents drive the daemon already"))
    }

    /// Runs every session's CLI in a scope of `scopes` from its next start; once per manager.
    /// Without it, CLIs run without resource limits.
    pub fn limit_resources(&self, scopes: Arc<Scopes>) -> anyhow::Result<()> {
        self.inner
            .scopes
            .set(scopes)
            .map_err(|_| anyhow::anyhow!("resources are limited already"))
    }

    /// Lets owners bring down the Compose projects `docker` tracks for sessions; once per
    /// manager. Without it, `compose_down` is unsupported.
    pub fn track_containers(&self, docker: Arc<Docker>) -> anyhow::Result<()> {
        self.inner
            .docker
            .set(docker)
            .map_err(|_| anyhow::anyhow!("containers are tracked already"))
    }

    /// Starts every turn from now on only once `admission` admits it; once per manager.
    /// Without it, turns start as soon as their session is free.
    pub fn admit_turns(&self, admission: Arc<Admission>) -> anyhow::Result<()> {
        self.inner
            .admission
            .set(admission)
            .map_err(|_| anyhow::anyhow!("turns are admitted already"))
    }

    /// Titles sessions as `config` says, with the CLI `clis` names for each provider
    /// ([`titles`]); once per manager. Without it, sessions get only the titles users type and
    /// `retitle_session` is unsupported.
    pub fn generate_titles(&self, config: TitlesConfig, clis: TitleClis) -> anyhow::Result<()> {
        self.inner
            .titler
            .set(Titler { config, clis })
            .map_err(|_| anyhow::anyhow!("titles are generated already"))
    }

    /// Fails sessions over as `config` says ([`failover`]); once per manager. Without it,
    /// sessions are not pinned.
    pub fn configure_failover(&self, config: FailoverConfig) -> anyhow::Result<()> {
        self.inner
            .failover
            .set(config)
            .map_err(|_| anyhow::anyhow!("failover is configured already"))
    }

    /// Runs the setup command of its project, as `projects` on `host` resolves it, in every new
    /// session's worktree from now on, and lets owners change `projects` with `add_project` and
    /// `set_project_settings`; once per manager. Without it, worktrees get no setup and those
    /// commands are unsupported.
    pub fn manage_projects(&self, host: HostId, projects: Arc<Overrides>) -> anyhow::Result<()> {
        self.inner
            .projects
            .set((host, projects))
            .map_err(|_| anyhow::anyhow!("projects are managed already"))
    }

    /// The bytes of `attachment`, an image or file a prompt of `session_id` carried.
    pub(crate) async fn attachment_data(
        &self,
        session_id: &SessionId,
        attachment: &Attachment,
    ) -> Result<Bytes, ErrorInfo> {
        attachments::load(&self.inner.attachments, session_id, attachment).await
    }

    /// The bytes of the image or file `attachment_id` a prompt of `session_id` carried.
    async fn attachment(
        &self,
        session_id: &SessionId,
        attachment_id: &AttachmentId,
    ) -> Result<CommandResult, ErrorInfo> {
        self.inner
            .journal
            .session(session_id.clone())
            .await
            .map_err(internal)?
            .ok_or_else(|| not_found(session_id))?;
        let (media_type, data) =
            attachments::fetch(&self.inner.attachments, session_id, attachment_id).await?;
        Ok(CommandResult::Attachment { media_type, data })
    }

    /// Declares the repository at `path` as a project ([`Overrides::add`]).
    async fn add_project(&self, path: &str) -> Result<CommandResult, ErrorInfo> {
        let (host, overrides) = self.projects()?;
        let repo = crate::browse::absolute(path)?;
        let added = tokio::task::spawn_blocking(move || {
            if !repo.is_dir() {
                return Err(error(
                    ErrorCode::BadRequest,
                    format!("{} is not a folder", repo.display()),
                ));
            }
            overrides.add(&host, &repo).map_err(internal)
        })
        .await
        .map_err(|err| error(ErrorCode::Internal, format!("{err}")))?;
        let project_id = added?;
        Ok(CommandResult::ProjectAdded { project_id })
    }

    /// Clones `url` into the new folder `path`, else into the projects dir ([`crate::projects::clone::folder`]),
    /// and declares the clone as a project
    /// ([`Self::add_project`]).
    async fn clone_project(
        &self,
        url: &str,
        path: Option<&str>,
    ) -> Result<CommandResult, ErrorInfo> {
        let (_, overrides) = self.projects()?;
        let repo = match path {
            Some(path) => crate::browse::absolute(path)?,
            None => {
                let dir = overrides.config().dir;
                if !dir.is_absolute() {
                    return Err(error(
                        ErrorCode::Unsupported,
                        "this host has no projects dir",
                    ));
                }
                crate::projects::clone::folder(&dir, url).ok_or_else(|| {
                    error(
                        ErrorCode::BadRequest,
                        format!("{url:?} names no repository to clone"),
                    )
                })?
            }
        };
        crate::projects::clone::clone(url, &repo).await?;
        let path = repo
            .to_str()
            .ok_or_else(|| error(ErrorCode::BadRequest, "the clone's path is not UTF-8"))?;
        self.add_project(path).await
    }

    /// Replaces the settings of `project_id`, one of the listed projects
    /// ([`Overrides::set`]).
    async fn set_project_settings(
        &self,
        project_id: &ProjectId,
        settings: ProjectSettings,
    ) -> Result<CommandResult, ErrorInfo> {
        let (_, overrides) = self.projects()?;
        let project = self.listed_project(project_id)?;
        if let Some(account_id) = &settings.default_account
            && self.inner.account(account_id).is_none()
        {
            return Err(error(
                ErrorCode::NotFound,
                format!("account {account_id} does not exist"),
            ));
        }
        if let Some(colour) = &settings.icon_background
            && !crate::config::is_rgb_hex(colour)
        {
            return Err(error(
                ErrorCode::BadRequest,
                format!("icon_background {colour:?} is not a #rrggbb colour"),
            ));
        }
        tokio::task::spawn_blocking(move || overrides.set(&project, settings).map_err(internal))
            .await
            .map_err(|err| error(ErrorCode::Internal, format!("{err}")))??;
        Ok(CommandResult::Applied)
    }

    /// Removes `project_id`, one of the listed projects, unless a session not archived runs in
    /// one of its clones ([`Overrides::remove`]).
    async fn remove_project(&self, project_id: &ProjectId) -> Result<CommandResult, ErrorInfo> {
        let (_, overrides) = self.projects()?;
        let project = self.listed_project(project_id)?;
        let live = self
            .inner
            .journal
            .sessions()
            .await
            .map_err(internal)?
            .into_iter()
            .filter(|session| {
                session.status != SessionStatus::Archived && project.paths.contains(&session.repo)
            })
            .count();
        if live > 0 {
            return Err(error(
                ErrorCode::Conflict,
                format!(
                    "project {project_id} has {live} live session{}; archive {} first",
                    if live == 1 { "" } else { "s" },
                    if live == 1 { "it" } else { "them" },
                ),
            ));
        }
        tokio::task::spawn_blocking(move || overrides.remove(&project).map_err(internal))
            .await
            .map_err(|err| error(ErrorCode::Internal, format!("{err}")))??;
        Ok(CommandResult::Applied)
    }

    /// Keeps `icon` as the uploaded icon of `project_id`, one of the listed projects, or
    /// deletes its upload when `None` ([`Overrides::set_icon`]).
    async fn set_project_icon(
        &self,
        project_id: &ProjectId,
        icon: Option<Image>,
    ) -> Result<CommandResult, ErrorInfo> {
        let (_, overrides) = self.projects()?;
        let project = self.listed_project(project_id)?;
        if let Some(icon) = &icon {
            if !PROJECT_ICON_MEDIA_TYPES.contains(&icon.media_type.as_str()) {
                return Err(error(
                    ErrorCode::BadRequest,
                    format!(
                        "a project icon must be one of {}, not {}",
                        PROJECT_ICON_MEDIA_TYPES.join(", "),
                        icon.media_type
                    ),
                ));
            }
            if icon.data.0.is_empty() || icon.data.0.len() > MAX_PROJECT_ICON_BYTES {
                return Err(error(
                    ErrorCode::BadRequest,
                    format!(
                        "a project icon must have 1 to {MAX_PROJECT_ICON_BYTES} bytes, not {}",
                        icon.data.0.len()
                    ),
                ));
            }
        }
        tokio::task::spawn_blocking(move || {
            let icon = icon
                .as_ref()
                .map(|icon| (icon.media_type.as_str(), icon.data.0.as_slice()));
            overrides
                .set_icon(&project.project_id, icon)
                .map_err(internal)
        })
        .await
        .map_err(|err| error(ErrorCode::Internal, format!("{err}")))??;
        Ok(CommandResult::Applied)
    }

    /// The icon of `project_id`, one of the listed projects, read afresh ([`projects::icon`]).
    async fn project_icon(&self, project_id: &ProjectId) -> Result<CommandResult, ErrorInfo> {
        let (_, overrides) = self.projects()?;
        let project = self.listed_project(project_id)?;
        let icon = tokio::task::spawn_blocking(move || {
            projects::icon(&project, &overrides.config().entries, overrides.icons())
        })
        .await
        .map_err(|err| error(ErrorCode::Internal, format!("{err}")))?
        .ok_or_else(|| {
            error(
                ErrorCode::NotFound,
                format!("project {project_id} has no icon"),
            )
        })?;
        Ok(CommandResult::ProjectIcon {
            icon: icon.hash,
            media_type: icon.media_type.to_owned(),
            data: Bytes(icon.data),
        })
    }

    /// `project_id` as discovery last listed it.
    fn listed_project(&self, project_id: &ProjectId) -> Result<Project, ErrorInfo> {
        self.inner
            .journal
            .projects()
            .list
            .iter()
            .find(|project| project.project_id == *project_id)
            .cloned()
            .ok_or_else(|| {
                error(
                    ErrorCode::NotFound,
                    format!("project {project_id} has no clone on this host"),
                )
            })
    }

    fn projects(&self) -> Result<(HostId, Arc<Overrides>), ErrorInfo> {
        self.inner
            .projects
            .get()
            .cloned()
            .ok_or_else(|| error(ErrorCode::Unsupported, "projects are not managed here"))
    }

    /// Checkpoints every session's worktree at each turn's end from now on, into `config`;
    /// once per manager. Without it, no checkpoints are made.
    pub fn checkpoint_turns(&self, config: checkpoint::Config) -> anyhow::Result<()> {
        self.inner
            .checkpoints
            .set(config)
            .map_err(|_| anyhow::anyhow!("checkpoints are configured already"))
    }

    /// Hands `skills`' enabled skills to every session's CLI from its next start, and answers
    /// the skill library commands with it; once per manager. Without it, they are
    /// unsupported.
    pub fn deliver_skills(&self, skills: Arc<Skills>) -> anyhow::Result<()> {
        self.inner
            .skills
            .set(skills)
            .map_err(|_| anyhow::anyhow!("skills are delivered already"))?;
        self.skill_accounts();
        Ok(())
    }

    /// Hands the skills every account's config dir, where its CLI reads the user's skills.
    fn skill_accounts(&self) {
        let Some(skills) = self.inner.skills.get() else {
            return;
        };
        let env: BTreeMap<String, String> = std::env::vars().collect();
        let accounts = self
            .inner
            .accounts_lock()
            .iter()
            .filter_map(|(account_id, account)| {
                herder_adapters::transcript::config_dir(
                    &account.provider,
                    account.config_dir.as_deref(),
                    &env,
                )
                .map(|dir| (account_id.clone(), dir))
            })
            .collect();
        skills.set_accounts(accounts);
    }

    /// Sends the skills of every session not archived, as the daemon starts, before any of
    /// their CLIs does.
    pub async fn list_skills(&self) -> anyhow::Result<()> {
        let Some(skills) = self.inner.skills.get() else {
            return Ok(());
        };
        for session in self.inner.journal.sessions().await? {
            if session.status != herder_protocol::SessionStatus::Archived {
                skills
                    .session_started(
                        &session.session_id,
                        &session.provider,
                        &session.account_id,
                        Path::new(&session.worktree),
                    )
                    .await;
            }
        }
        Ok(())
    }

    /// Pulls the skill library in the background, as when a client opens; the change arrives
    /// as a `skills_status`.
    pub fn refresh_skills(&self) {
        if let Some(skills) = self.inner.skills.get() {
            let skills = Arc::clone(skills);
            tokio::spawn(async move { skills.pull().await });
        }
    }

    /// The providers this manager runs sessions of.
    pub fn providers(&self) -> Vec<Provider> {
        self.inner.adapters.0.keys().cloned().collect()
    }

    /// Announces every child request that goes to the user instead of its primary session to
    /// `notifier` from now on ([`routing`]); once per manager. Without one, nothing is
    /// announced beyond the journal.
    pub fn notify_escalations(&self, notifier: Arc<dyn Notifier>) -> anyhow::Result<()> {
        self.inner
            .notifier
            .set(notifier)
            .map_err(|_| anyhow::anyhow!("escalations have a notifier already"))
    }

    /// Brings down Compose project `project`, if one of `session_id`'s tracked containers
    /// belongs to it.
    async fn compose_down(
        &self,
        session_id: &SessionId,
        project: &str,
    ) -> Result<CommandResult, ErrorInfo> {
        let docker = self
            .inner
            .docker
            .get()
            .ok_or_else(|| error(ErrorCode::Unsupported, "containers are not tracked here"))?;
        let tracked = docker
            .containers(session_id)
            .iter()
            .any(|container| container.compose_project.as_deref() == Some(project));
        if !tracked {
            return Err(error(
                ErrorCode::NotFound,
                format!("session {session_id} has no containers of compose project {project}"),
            ));
        }
        docker.compose_down(project).await.map_err(|err| {
            warn!(session_id = %session_id, project, "{err:#}");
            error(ErrorCode::Internal, format!("{err:#}"))
        })?;
        info!(session_id = %session_id, project, "brought the compose project down");
        Ok(CommandResult::Applied)
    }

    fn prs(&self) -> Result<&PrTracker, ErrorInfo> {
        self.inner.prs.get().map(|prs| &**prs).ok_or_else(|| {
            error(
                ErrorCode::Unsupported,
                "pull request tracking is not running",
            )
        })
    }

    /// Archives `session_id` for `by`: stops it and makes it read-only, leaving its worktree
    /// for [`Self::remove_archived_worktrees`]. Refuses while a turn or the setup command runs.
    pub async fn archive(
        &self,
        by: UserId,
        session_id: SessionId,
    ) -> Result<CommandResult, ErrorInfo> {
        self.send(session_id, Some(by), Request::Archive).await
    }

    /// Removes the worktree of every session archived at least `after` ago whose worktree is
    /// still there, keeping its branches. A failure is logged; the next sweep tries again.
    pub async fn remove_archived_worktrees(&self, after: SignedDuration) -> anyhow::Result<()> {
        let journal = &self.inner.journal;
        let now = Timestamp::now();
        for session in journal.sessions().await? {
            // A session without a branch works in the user's own folder: nothing to remove.
            if session.status != SessionStatus::Archived
                || session.branch.is_none()
                || !Path::new(&session.worktree).exists()
            {
                continue;
            }
            let archived_at = journal
                .all(session.session_id.clone())
                .await?
                .into_iter()
                .rev()
                .find_map(|event| {
                    matches!(
                        event.body,
                        EventBody::SessionStatusChanged {
                            status: SessionStatus::Archived,
                            ..
                        }
                    )
                    .then_some(event.at)
                });
            if !archived_at.is_some_and(|at| now.duration_since(at) >= after) {
                continue;
            }
            let session_id = session.session_id;
            match self
                .send(session_id.clone(), None, Request::RemoveWorktree)
                .await
            {
                Ok(_) => info!(%session_id, "removed the archived session's worktree"),
                Err(err) => {
                    warn!(%session_id, "cannot remove the archived worktree: {}", err.message)
                }
            }
        }
        Ok(())
    }

    /// Runs [`Self::remove_archived_worktrees`] now and every [`SWEEP_INTERVAL`] until
    /// `shutdown`.
    pub async fn sweep_archived_worktrees(self, shutdown: CancellationToken) {
        let mut every = tokio::time::interval(SWEEP_INTERVAL);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                _ = every.tick() => {}
            }
            if let Err(err) = self.remove_archived_worktrees(KEEP_ARCHIVED_WORKTREE).await {
                warn!("cannot sweep archived worktrees: {err:#}");
            }
        }
    }

    /// Sets the title of `session_id` as `by` chose it.
    pub async fn rename(
        &self,
        by: UserId,
        session_id: SessionId,
        title: &str,
    ) -> Result<CommandResult, ErrorInfo> {
        let title = clean_title(title).ok_or_else(|| {
            error(
                ErrorCode::BadRequest,
                format!("a title is one line of 1 to {MAX_TITLE_CHARS} characters"),
            )
        })?;
        let _titling = self.inner.titling.lock().await;
        let session = self.titleable(&session_id).await?;
        if session.title.as_deref() != Some(title)
            || session.title_source != Some(TitleSource::User)
        {
            let body = EventBody::TitleChanged {
                title: title.to_owned(),
                source: TitleSource::User,
            };
            self.inner
                .journal
                .record(session_id, Some(by), body)
                .await
                .map_err(internal)?;
        }
        Ok(CommandResult::Applied)
    }

    /// Starts generating the title of `session_id` again from its conversation, as `by`
    /// asked ([`titles`]).
    pub async fn retitle(
        &self,
        by: UserId,
        session_id: SessionId,
    ) -> Result<CommandResult, ErrorInfo> {
        let session = self.titleable(&session_id).await?;
        titles::request(&self.inner, by, &session)
            .map_err(|why| error(ErrorCode::Unsupported, why))?;
        Ok(CommandResult::Applied)
    }

    /// The session, if it exists and its title may change.
    async fn titleable(&self, session_id: &SessionId) -> Result<Session, ErrorInfo> {
        let session = self
            .inner
            .journal
            .session(session_id.clone())
            .await
            .map_err(internal)?
            .ok_or_else(|| not_found(session_id))?;
        if matches!(
            session.status,
            SessionStatus::Archived | SessionStatus::Moved
        ) {
            return Err(error(
                ErrorCode::Conflict,
                format!("session {session_id} is read-only"),
            ));
        }
        Ok(session)
    }

    /// Every branch `session_id` owns, its own first, in the order first checked out: the
    /// journaled ones, then any its worktree has checked out since the last turn ended.
    pub async fn branches(&self, session_id: &SessionId) -> Result<Vec<String>, ErrorInfo> {
        let journal = &self.inner.journal;
        let session = journal
            .session(session_id.clone())
            .await
            .map_err(internal)?
            .ok_or_else(|| not_found(session_id))?;
        let mut owned = journal
            .branches(session_id.clone())
            .await
            .map_err(internal)?;
        let checked_out =
            worktree::branches(Path::new(&session.worktree), session.branch.as_deref())
                .await
                .map_err(worktree_error)?;
        for branch in checked_out {
            if !owned.contains(&branch) {
                owned.push(branch);
            }
        }
        Ok(owned)
    }

    /// The worktree of `session_id`, for a terminal; refused once the session is read-only.
    pub async fn worktree(&self, session_id: &SessionId) -> Result<PathBuf, ErrorInfo> {
        let session = self
            .inner
            .journal
            .session(session_id.clone())
            .await
            .map_err(internal)?
            .ok_or_else(|| not_found(session_id))?;
        if matches!(
            session.status,
            SessionStatus::Archived | SessionStatus::Moved
        ) {
            return Err(error(
                ErrorCode::Conflict,
                format!("session {session_id} is read-only and has no worktree"),
            ));
        }
        Ok(PathBuf::from(session.worktree))
    }

    /// Every session's worktree, archived ones included: what they started there outlives them.
    pub async fn worktrees(&self) -> anyhow::Result<Vec<(SessionId, PathBuf)>> {
        Ok(self
            .inner
            .journal
            .sessions()
            .await?
            .into_iter()
            .map(|session| (session.session_id, PathBuf::from(session.worktree)))
            .collect())
    }

    /// Every account sessions may run on, as clients see them.
    pub fn accounts(&self) -> Vec<Account> {
        crate::accounts::list(&self.inner.accounts_lock(), &self.inner.usage.all())
    }

    /// How the account `account_id` runs, if it is one.
    pub(crate) fn account(&self, account_id: &AccountId) -> Option<AccountConfig> {
        self.inner.account(account_id)
    }

    /// Serializes directory edits with new sessions. Existing sessions retain their login;
    /// directory changes are allowed only when every session on the machine is archived.
    pub(crate) async fn configure_account(
        &self,
        account_id: &AccountId,
        save: impl FnOnce(&AccountConfig, bool) -> Result<AccountConfig, ErrorInfo>,
    ) -> Result<(), ErrorInfo> {
        let inner = &self.inner;
        let _settings = inner.account_settings.lock().await;
        let previous = inner
            .account(account_id)
            .ok_or_else(|| error(ErrorCode::NotFound, "account does not exist"))?;
        let may_change_directory = inner
            .journal
            .sessions()
            .await
            .map_err(internal)?
            .iter()
            .all(|session| session.status == herder_protocol::SessionStatus::Archived);
        let account = save(&previous, may_change_directory)?;
        if account.config_dir != previous.config_dir {
            inner.usage.forget(account_id);
        }
        inner
            .accounts
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(account_id.clone(), account);
        inner.journal.sink().accounts_changed(&self.accounts());
        inner.refresh_usage.stale();
        self.skill_accounts();
        self.refresh_skills();
        Ok(())
    }

    /// Lets sessions run on a newly added account, announces the new account list and asks for
    /// its usage; `false`, changing nothing, when the id is taken.
    pub fn add_account(&self, account_id: AccountId, account: AccountConfig) -> bool {
        let inner = &self.inner;
        {
            let mut accounts = inner
                .accounts
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            if accounts.contains_key(&account_id) {
                return false;
            }
            accounts.insert(account_id, account);
        }
        inner.journal.sink().accounts_changed(&self.accounts());
        inner.refresh_usage.stale();
        self.skill_accounts();
        self.refresh_skills();
        true
    }

    /// Starts probing every account's usage ([`crate::usage`]) until the manager's shutdown.
    /// Call it once per manager.
    pub fn track_usage(&self, config: usage::Config) -> anyhow::Result<()> {
        let inner = &self.inner;
        std::fs::create_dir_all(&config.dir)
            .with_context(|| format!("creating {}", config.dir.display()))?;
        let weak = Arc::downgrade(inner);
        let accounts = weak.clone();
        tokio::spawn(usage::poll(
            config,
            move || {
                accounts
                    .upgrade()
                    .map(|inner| inner.accounts_lock().clone())
                    .unwrap_or_default()
            },
            Arc::clone(&inner.refresh_usage),
            move |account_id, usage| {
                if let Some(inner) = weak.upgrade() {
                    inner.probed_usage(account_id, usage);
                }
            },
            inner.shutdown.clone(),
        ));
        Ok(())
    }

    /// Asks for fresh usage of every account not read lately ([`usage::Config::fresh`]), as
    /// when a client opens; it arrives as an account list change.
    pub fn refresh_usage(&self) {
        self.inner.refresh_usage.stale();
    }

    /// `account_id` was logged in again ([`crate::login`]): failover may choose it again, and
    /// its usage is read at once, however lately it was read.
    pub fn logged_in_again(&self, account_id: &AccountId) {
        self.inner.limits.worked(account_id);
        self.inner.refresh_usage.account(account_id);
    }

    /// Every session with its latest seq, ordered by session id.
    pub async fn sessions(&self) -> anyhow::Result<Vec<SessionHead>> {
        self.inner.journal.heads().await
    }

    /// Resolves each session's project from the clones in `projects`, and sends the session
    /// list again when that changes a session's project.
    pub async fn set_projects(&self, projects: &[Project]) {
        let journal = &self.inner.journal;
        if !journal.set_projects(projects) {
            return;
        }
        match journal.heads().await {
            Ok(heads) => journal.sink().sessions_changed(&heads),
            Err(err) => warn!("cannot list sessions after the projects changed: {err:#}"),
        }
    }

    /// The repository of every session, each once.
    pub async fn repos(&self) -> anyhow::Result<Vec<PathBuf>> {
        let sessions = self.inner.journal.sessions().await?;
        let repos: BTreeSet<PathBuf> = sessions.into_iter().map(|s| s.repo.into()).collect();
        Ok(repos.into_iter().collect())
    }

    /// Up to `limit` events of `session_id` after `after_seq`, oldest first; for subscriptions.
    pub async fn read_since(
        &self,
        session_id: &SessionId,
        after_seq: u64,
        limit: usize,
    ) -> anyhow::Result<Vec<Event>> {
        self.inner
            .journal
            .read_since(session_id.clone(), after_seq, limit)
            .await
    }

    /// The account a `create_session` runs on: `account_id`, which must be of `provider` when
    /// both are given; else the available account of `provider` with the most room left; else
    /// the default account of `project`, the project of `repo`.
    fn pick_account(
        &self,
        account_id: Option<AccountId>,
        provider: Option<Provider>,
        repo: &str,
        project: Option<&Project>,
    ) -> Result<AccountId, ErrorInfo> {
        let inner = &self.inner;
        match (account_id, provider) {
            (Some(account_id), Some(provider)) => {
                let account = inner.account(&account_id).ok_or_else(|| {
                    error(
                        ErrorCode::NotFound,
                        format!("account {account_id} does not exist"),
                    )
                })?;
                if account.provider != provider {
                    return Err(error(
                        ErrorCode::BadRequest,
                        format!(
                            "account {account_id} runs {}, not {}",
                            account.provider.as_str(),
                            provider.as_str()
                        ),
                    ));
                }
                Ok(account_id)
            }
            (Some(account_id), None) => Ok(account_id),
            (None, Some(provider)) => {
                if let Some(account_id) = inner.available_account(&provider, None) {
                    return Ok(account_id);
                }
                let any = inner
                    .accounts_lock()
                    .values()
                    .any(|account| account.provider == provider);
                Err(if any {
                    error(
                        ErrorCode::Conflict,
                        format!(
                            "every {} account is at its limit; try again once one resets",
                            provider.as_str()
                        ),
                    )
                } else {
                    error(
                        ErrorCode::NotFound,
                        format!("there is no {} account here", provider.as_str()),
                    )
                })
            }
            (None, None) => project
                .and_then(|project| project.default_account.clone())
                .ok_or_else(|| {
                    error(
                        ErrorCode::BadRequest,
                        format!(
                            "pick an account or a provider: the project of {repo} has no \
                             default_account"
                        ),
                    )
                }),
        }
    }

    /// The repository a `create_session` names, by path or by project, and its project.
    fn resolve_repo(
        &self,
        repo: Option<String>,
        project_id: Option<ProjectId>,
    ) -> Result<(String, Option<Project>), ErrorInfo> {
        let projects = self.inner.journal.projects();
        match (repo, project_id) {
            (Some(repo), None) => {
                let project = projects.of_repo(&repo).cloned();
                Ok((repo, project))
            }
            (None, Some(project_id)) => {
                let project = projects
                    .list
                    .iter()
                    .find(|project| project.project_id == project_id)
                    .ok_or_else(|| {
                        error(
                            ErrorCode::NotFound,
                            format!("project {project_id} has no clone on this host"),
                        )
                    })?;
                let repo = project.paths.first().cloned().ok_or_else(|| {
                    error(
                        ErrorCode::NotFound,
                        format!("project {project_id} has no clone on this host"),
                    )
                })?;
                Ok((repo, Some(project.clone())))
            }
            _ => Err(error(
                ErrorCode::BadRequest,
                "name the repository by exactly one of `repo` and `project_id`",
            )),
        }
    }

    /// Up to `limit` events of `session_id` after `after_seq`, exactly as stored; for
    /// replicating to the vault.
    pub async fn read_records_since(
        &self,
        session_id: &SessionId,
        after_seq: Seq,
        limit: usize,
    ) -> anyhow::Result<Vec<JournalRecord>> {
        self.inner
            .journal
            .records_since(session_id.clone(), after_seq, limit)
            .await
    }

    /// Every session as the vault's fleet index lists it, ordered by session id.
    pub async fn summaries(&self, host: &HostId) -> anyhow::Result<Vec<SessionSummary>> {
        self.inner.journal.summaries(host).await
    }

    /// Creates a session with its worktree; returns its id and branch, `None` for a session that
    /// works in its folder itself.
    async fn create_session(
        &self,
        by: Option<UserId>,
        request: CreateRequest,
    ) -> Result<(SessionId, Option<String>), ErrorInfo> {
        let inner = &self.inner;
        let _settings = inner.account_settings.lock().await;
        let account = inner.account(&request.account_id).ok_or_else(|| {
            error(
                ErrorCode::NotFound,
                format!("account {} does not exist", request.account_id),
            )
        })?;
        if inner.adapters.get(&account.provider).is_none() {
            return Err(error(
                ErrorCode::Unsupported,
                format!("no adapter runs {} sessions", account.provider.as_str()),
            ));
        }
        let session_id = SessionId::new(ulid::Ulid::new().to_string());
        let repo = PathBuf::from(&request.repo);
        let worktree = inner
            .worktrees
            .create(
                Path::new(&request.repo),
                &worktree::slug(&session_id),
                request.branch,
            )
            .await
            .map_err(worktree_error)?;
        let account_id = request.account_id.clone();
        let body = EventBody::SessionCreated {
            repo: request.repo,
            worktree: worktree.path.to_string_lossy().into_owned(),
            branch: worktree.branch.clone(),
            provider: account.provider.clone(),
            account_id: request.account_id,
            // Empty until the adapter reports the provider's default.
            model: request.model.unwrap_or_default(),
            permission_mode: request.permission_mode,
            parent: request.parent,
            parent_host: None,
            task: request.task,
            failover_pin: request.failover_pin,
        };
        inner
            .journal
            .record(session_id.clone(), by, body)
            .await
            .map_err(internal)?;
        if let Some(prs) = inner.prs.get()
            && worktree.branch.is_some()
        {
            prs.install(&session_id, &worktree.path).await;
        }
        // So `$` lists them before the first prompt starts the CLI.
        if let Some(skills) = inner.skills.get() {
            skills
                .session_started(&session_id, &account.provider, &account_id, &worktree.path)
                .await;
        }
        // Before the reply, so the session's first prompt queues behind the setup. A session
        // in the folder itself has no new worktree to set up.
        if worktree.branch.is_some()
            && let Some((command, timeout)) = inner.setup_command(&repo).await
        {
            self.send(
                session_id.clone(),
                None,
                Request::SetUp { command, timeout },
            )
            .await?;
        }
        Ok((session_id, worktree.branch))
    }

    /// Sends `session_id` a prompt from its primary's agent; returns whether it waits behind a
    /// running turn.
    async fn prompt(&self, session_id: &SessionId, text: String) -> Result<bool, ErrorInfo> {
        let child = self
            .inner
            .journal
            .session(session_id.clone())
            .await
            .map_err(internal)?
            .ok_or_else(|| error(ErrorCode::NotFound, "session does not exist"))?;
        let caller = child
            .parent
            .ok_or_else(|| error(ErrorCode::Forbidden, "not a child session"))?;
        let message = self
            .agent_message(&caller, ulid::Ulid::new().to_string())
            .await?;
        Ok(self
            .deliver_agent_message(session_id, text, message)
            .await?
            .queued)
    }

    /// Sends the session the follow-up prompt `text` if it is idle; whether it was sent.
    async fn follow_up(&self, session_id: SessionId, text: String, follow_up: FollowUp) -> bool {
        self.send(session_id, None, Request::FollowUp { text, follow_up })
            .await
            .is_ok()
    }

    async fn deliver_agent_message(
        &self,
        session_id: &SessionId,
        text: String,
        message: herder_protocol::AgentMessage,
    ) -> Result<herder_tasktools::SendSessionOutput, ErrorInfo> {
        let (done, response) = oneshot::channel();
        self.send(
            session_id.clone(),
            None,
            Request::SendAgentMessage {
                text,
                message,
                done,
            },
        )
        .await?;
        response
            .await
            .map_err(|_| error(ErrorCode::Internal, "the session stopped"))
    }

    async fn send(
        &self,
        session_id: SessionId,
        by: Option<UserId>,
        request: Request,
    ) -> Result<CommandResult, ErrorInfo> {
        let (reply, answer) = oneshot::channel();
        let command = SessionCommand { by, request, reply };
        self.actor(&session_id)
            .await?
            .send(command)
            .map_err(|_| error(ErrorCode::Internal, "the daemon is shutting down"))?;
        answer
            .await
            .map_err(|_| error(ErrorCode::Internal, "the session stopped"))?
    }

    /// The running actor of `session_id`, started on first use.
    async fn actor(
        &self,
        session_id: &SessionId,
    ) -> Result<mpsc::UnboundedSender<SessionCommand>, ErrorInfo> {
        let inner = &self.inner;
        let mut actors = inner.actors.lock().await;
        if let Some(actor) = actors.get(session_id).filter(|actor| !actor.is_closed()) {
            return Ok(actor.clone());
        }
        let session = inner
            .journal
            .session(session_id.clone())
            .await
            .map_err(internal)?
            .ok_or_else(|| not_found(session_id))?;
        let (commands, queue) = mpsc::unbounded_channel();
        let actor = Actor::new(session, self.inner.clone());
        tokio::spawn(actor.run(queue, inner.shutdown.clone()));
        actors.insert(session_id.clone(), commands.clone());
        Ok(commands)
    }
}

/// A `create_session` command, or a `spawn`.
struct CreateRequest {
    repo: String,
    branch: Option<String>,
    account_id: AccountId,
    model: Option<String>,
    permission_mode: herder_protocol::PermissionMode,
    /// The primary session, for a child.
    parent: Option<SessionId>,
    /// The child's task label.
    task: Option<String>,
    /// The session's own failover pin.
    failover_pin: Option<bool>,
}

/// Whether `command` only reads: it changes nothing, so its answer, which may be large, is
/// never remembered and a resend is answered afresh.
pub fn changes_nothing(command: &CommandBody) -> bool {
    matches!(
        command,
        CommandBody::GetAttachment { .. }
            | CommandBody::ListDirectory { .. }
            | CommandBody::GetProjectIcon { .. }
            | CommandBody::GetVaultLink
            | CommandBody::GetUsageSummary { .. }
    )
}

fn error(code: ErrorCode, message: impl Into<String>) -> ErrorInfo {
    ErrorInfo {
        code,
        message: message.into(),
    }
}

fn internal(err: anyhow::Error) -> ErrorInfo {
    warn!("session command failed: {err:#}");
    error(ErrorCode::Internal, format!("{err:#}"))
}

fn not_found(session_id: &SessionId) -> ErrorInfo {
    error(
        ErrorCode::NotFound,
        format!("session {session_id} does not exist"),
    )
}

fn worktree_error(err: worktree::Error) -> ErrorInfo {
    let code = match err {
        worktree::Error::BadRequest(_) => ErrorCode::BadRequest,
        worktree::Error::Conflict(_) => ErrorCode::Conflict,
        worktree::Error::Git(_) => {
            warn!("git failed: {err}");
            ErrorCode::Internal
        }
    };
    error(code, err.to_string())
}
