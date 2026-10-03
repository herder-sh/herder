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
//! was read from, are gone. `archive_session` ([`SessionManager::archive`]) removes the
//! worktree, keeps its branches and journals the `archived` status; an archived session takes
//! no further commands but `unarchive_session`, which adds the worktree back at the path it had,
//! on the session's own branch, and journals the `idle` status.
//!
//! # Images
//!
//! A prompt may carry images when its session's adapter takes them
//! ([`herder_adapters::Adapter::accepts_images`]); otherwise it is refused as `unsupported`.
//! They are checked and kept as files when the prompt arrives ([`attachments`]), journaled as
//! the attachments of its `user_message`, and handed to the agent with the prompt's text when
//! its turn starts. `get_attachment` reads one back; it changes nothing, so its answer is not
//! remembered ([`changes_nothing`]). A transcript replayed into another CLI keeps each prompt's
//! attachments, and the adapter names every image in a line of text: the seed carries no
//! bytes, and the vault keeps none either, so a recovered session's earlier images are
//! references only.
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
//! When the agent's CLI fails after the kernel's OOM killer killed a process in its scope
//! ([`Scopes::oom_killed`]), the turn fails with an error that says so and the session is
//! `error`; the CLI is stopped, and the next prompt starts a new one seeded with the transcript,
//! as after a restart.
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
//! starts that account's CLI on it. Every account takes part; none opts in. With no available
//! account, with the session pinned (its `failover_pin`, else [`FailoverConfig::pin`]), or when
//! the retry hits a limit too, the session is `needs_you` with the limit error; when the retry
//! fails otherwise, as when the account rejects the model, it is `needs_you` with that error,
//! and nothing else is tried.
//!
//! # Recovery
//!
//! A session whose host died can go on on another host from the journal its vault holds
//! ([`SessionManager::recover`], see [`crate::vault`]); it keeps its id. When the host it
//! came from returns, that copy is stopped and made `moved`, read-only
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
pub(crate) mod journal;
mod recover;
mod routing;
mod setup;
mod tasks;

pub use recover::Recovered;
pub use routing::{Escalation, Notifier};

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

use anyhow::Context;
use herder_adapters::Adapter;
use herder_protocol::{
    Account, AccountId, AttachmentId, CommandBody, CommandId, CommandResult, ErrorCode, ErrorInfo,
    Event, EventBody, HostId, Item, ItemId, JournalRecord, PermissionMode, Project, ProjectId,
    Provider, Seq, SessionHead, SessionId, SessionStatus, SessionSummary, Timestamp, TurnId,
    UsageWindow, UserId,
};
use herder_store::Store;
use tokio::sync::{Mutex, Notify, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use actor::{Actor, Request, SessionCommand, Switch};
pub use failover::FailoverConfig;
use failover::Limits;
use journal::Journal;
pub use tasks::TaskLimits;
use tasks::{TaskTools, Tasks};

use crate::config::ProjectSettings;
use crate::mcp::{self, Mcp};
use crate::projects::{self, Overrides};
use crate::prs::{self, PrTracker};
use crate::resources::{Admission, Docker, Scopes};
use crate::usage::{self, Usage};
use crate::worktree::{self, Worktrees, checkpoint};

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
    turn_ids: TurnIds,
    worktrees: Worktrees,
    /// Where prompts' images are kept ([`attachments`]).
    attachments: PathBuf,
    actors: Mutex<HashMap<SessionId, mpsc::UnboundedSender<SessionCommand>>>,
    /// Pull request tracking, once started.
    prs: OnceLock<Arc<PrTracker>>,
    /// herder's MCP server, once started.
    mcp: OnceLock<Arc<Mcp>>,
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
    /// Asks the usage poller, once started, to refresh accounts not read lately.
    refresh_usage: Arc<Notify>,
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
    shutdown: CancellationToken,
}

impl Inner {
    /// Merges `windows` into the account's usage and, when that changed it, publishes every
    /// account.
    pub(super) fn report_usage(&self, account_id: &AccountId, windows: Vec<UsageWindow>) {
        if let Some(usage) = self.usage.report(account_id, windows) {
            let accounts = crate::accounts::list(&self.accounts_lock(), &usage);
            self.journal.sink().accounts_changed(&accounts);
        }
    }

    /// Notes that `account_id` hit its limit just now; failover passes it over until it resets.
    pub(super) fn limit_hit(&self, account_id: &AccountId) {
        let usage = self.usage.all();
        let windows = usage.get(account_id).map_or(&[][..], Vec::as_slice);
        self.limits.hit(account_id, windows, Timestamp::now());
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
            usage: &self.usage.all(),
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
                actor::close_abandoned_turn(&journal, &tasks, &session).await?;
            }
        }
        Ok(Self {
            inner: Arc::new(Inner {
                journal,
                adapters: setup.adapters,
                accounts: RwLock::new(setup.accounts),
                turn_ids: setup.turn_ids,
                worktrees: setup.worktrees,
                attachments: setup.attachments,
                actors: Mutex::new(HashMap::new()),
                prs: OnceLock::new(),
                mcp: OnceLock::new(),
                tasks,
                notifier: OnceLock::new(),
                scopes: OnceLock::new(),
                usage: Usage::default(),
                refresh_usage: Arc::new(Notify::new()),
                admission: OnceLock::new(),
                docker: OnceLock::new(),
                failover: OnceLock::new(),
                limits: Limits::default(),
                projects: OnceLock::new(),
                checkpoints: OnceLock::new(),
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
        let (session_id, request) = match command {
            CommandBody::CreateSession {
                repo,
                project_id,
                branch,
                account_id,
                provider,
                model,
                permission_mode,
                max_children,
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
                    max_children,
                    failover_pin,
                };
                let (session_id, _) = self.create_session(Some(by), request).await?;
                return Ok(CommandResult::SessionCreated { session_id });
            }
            CommandBody::SendPrompt {
                session_id,
                text,
                images,
            } => (
                session_id,
                Request::SendPrompt {
                    text,
                    images,
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
            CommandBody::SetProjectSettings {
                project_id,
                default_permission_mode,
                default_account,
                setup_command,
            } => {
                let settings = ProjectSettings {
                    default_permission_mode,
                    default_account,
                    setup_command,
                };
                return self.set_project_settings(&project_id, settings).await;
            }
            CommandBody::Interrupt { session_id } => (session_id, Request::Interrupt),
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
            CommandBody::ArchiveSession { session_id, force } => {
                return self.archive(by, session_id, force).await;
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
            | CommandBody::AddAccount { .. }
            | CommandBody::AttachTerminal { .. }
            | CommandBody::DetachTerminal { .. }
            | CommandBody::ResizeTerminal { .. }
            | CommandBody::TerminalInput { .. } => {
                return Err(error(
                    ErrorCode::Unsupported,
                    "the session manager does not handle this command yet",
                ));
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
        let tracker =
            PrTracker::start(inner.journal.clone(), config, inner.shutdown.clone()).await?;
        let _ = inner.prs.set(Arc::clone(&tracker));
        Ok(tracker)
    }

    /// Starts herder's MCP server ([`crate::mcp`]) with the task tools ([`tasks`]), enforcing
    /// `limits`, until the manager's shutdown, and registers it with every session's CLI from
    /// its next start; once per manager.
    pub fn serve_mcp(&self, config: mcp::Config, limits: TaskLimits) -> anyhow::Result<()> {
        let inner = &self.inner;
        if inner.mcp.get().is_some() {
            anyhow::bail!("the MCP server runs already");
        }
        let tools = Arc::new(TaskTools {
            inner: Arc::downgrade(inner),
            limits,
        });
        let _ = inner
            .mcp
            .set(Mcp::start(config, tools, inner.shutdown.clone())?);
        Ok(())
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

    /// The bytes of the image `attachment_id` a prompt of `session_id` carried.
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
        let image = attachments::fetch(&self.inner.attachments, session_id, attachment_id).await?;
        Ok(CommandResult::Attachment {
            media_type: image.media_type,
            data: image.data,
        })
    }

    /// Declares the repository at `path` as a project ([`Overrides::add`]).
    async fn add_project(&self, path: &str) -> Result<CommandResult, ErrorInfo> {
        let (host, overrides) = self.projects()?;
        let repo = crate::browse::absolute(path)?;
        let added = tokio::task::spawn_blocking(move || {
            if !projects::scan::is_repo(&repo) {
                return Err(error(
                    ErrorCode::BadRequest,
                    format!("{} is not the top of a git repository", repo.display()),
                ));
            }
            overrides.add(&host, &repo).map_err(internal)
        })
        .await
        .map_err(|err| error(ErrorCode::Internal, format!("{err}")))?;
        let project_id = added?;
        Ok(CommandResult::ProjectAdded { project_id })
    }

    /// Replaces the settings of `project_id`, one of the listed projects
    /// ([`Overrides::set`]).
    async fn set_project_settings(
        &self,
        project_id: &ProjectId,
        settings: ProjectSettings,
    ) -> Result<CommandResult, ErrorInfo> {
        let (_, overrides) = self.projects()?;
        let project = self
            .inner
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
            })?;
        if let Some(account_id) = &settings.default_account
            && self.inner.account(account_id).is_none()
        {
            return Err(error(
                ErrorCode::NotFound,
                format!("account {account_id} does not exist"),
            ));
        }
        tokio::task::spawn_blocking(move || overrides.set(&project, &settings).map_err(internal))
            .await
            .map_err(|err| error(ErrorCode::Internal, format!("{err}")))??;
        Ok(CommandResult::Applied)
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

    /// Archives `session_id` for `by`: removes its worktree, keeping its branches, and makes it
    /// read-only. Refuses while a turn runs, and while the worktree has uncommitted or untracked
    /// changes unless `force`.
    pub async fn archive(
        &self,
        by: UserId,
        session_id: SessionId,
        force: bool,
    ) -> Result<CommandResult, ErrorInfo> {
        self.send(session_id, Some(by), Request::Archive { force })
            .await
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
        let checked_out = worktree::branches(Path::new(&session.worktree), &session.branch)
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
        inner.refresh_usage.notify_one();
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
            move |account_id, windows| {
                if let Some(inner) = weak.upgrade() {
                    inner.report_usage(account_id, windows);
                }
            },
            inner.shutdown.clone(),
        ));
        Ok(())
    }

    /// Asks for fresh usage of every account not read lately ([`usage::Config::fresh`]), as
    /// when a client opens; it arrives as an account list change.
    pub fn refresh_usage(&self) {
        self.inner.refresh_usage.notify_one();
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

    /// Creates a session with its worktree; returns its id and branch.
    async fn create_session(
        &self,
        by: Option<UserId>,
        request: CreateRequest,
    ) -> Result<(SessionId, String), ErrorInfo> {
        let inner = &self.inner;
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
            task: request.task,
            max_children: request.max_children,
            failover_pin: request.failover_pin,
        };
        inner
            .journal
            .record(session_id.clone(), by, body)
            .await
            .map_err(internal)?;
        if let Some(prs) = inner.prs.get() {
            prs.install(&session_id, &worktree.path).await;
        }
        // Before the reply, so the session's first prompt queues behind the setup.
        if let Some((command, timeout)) = inner.setup_command(&repo).await {
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
        let (queued, busy) = oneshot::channel();
        let request = Request::SendPrompt {
            text,
            images: Vec::new(),
            queued: Some(queued),
        };
        self.send(session_id.clone(), None, request).await?;
        busy.await
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
    /// The session's own limit on live children.
    max_children: Option<u32>,
    /// The session's own failover pin.
    failover_pin: Option<bool>,
}

/// Whether `command` only reads: it changes nothing, so its answer, which may be large, is
/// never remembered and a resend is answered afresh.
pub fn changes_nothing(command: &CommandBody) -> bool {
    matches!(
        command,
        CommandBody::GetAttachment { .. } | CommandBody::ListDirectory { .. }
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
