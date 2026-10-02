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
//! queued, so every turn's items stay together in the journal. Queued prompts live in memory
//! only and are lost when the daemon stops.
//!
//! # Status
//!
//! `idle` → `running` when a turn starts; `needs_you` while an approval or a question is
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
//! no further commands.
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
//! # Questions
//!
//! A question the agent asks is journaled as `question_asked`, routed like an approval request
//! but never beyond the primary's authority, and blocks the turn until a user's
//! `answer_question` or the primary's `answer` answers it, whichever comes first. A turn's end
//! drops its open questions.
//!
//! # Restart
//!
//! Sessions are read from the store. A turn left open by a daemon that stopped is closed with
//! a `transient` `turn_failed` when the manager opens, after expiring its open approvals the
//! same way as at a turn's end: the CLI that asked is gone. A session's adapter starts lazily on its
//! next prompt, seeded with the journal's items, condensed to fit the model
//! ([`crate::handoff`]).

mod actor;
pub(crate) mod journal;
mod routing;
mod tasks;

pub use routing::{Escalation, Notifier};

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use herder_adapters::Adapter;
use herder_protocol::{
    AccountId, CommandBody, CommandResult, ErrorCode, ErrorInfo, Event, EventBody, Item, ItemId,
    Provider, SessionHead, SessionId, SessionStatus, TurnId, UserId,
};
use herder_store::Store;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use actor::{Actor, Request, SessionCommand};
use journal::Journal;
pub use tasks::TaskLimits;
use tasks::{TaskTools, Tasks};

use crate::mcp::{self, Mcp};
use crate::prs::{self, PrTracker};
use crate::resources::Scopes;
use crate::worktree::{self, Worktrees};

/// Where a session manager publishes what clients should see. Calls for one session arrive in
/// order; implementations must not block.
pub trait EventSink: Send + Sync + 'static {
    /// A durable event, after it is stored.
    fn event(&self, event: &Event);
    /// An item started streaming: its state so far. Later deltas append to it.
    fn snapshot(&self, session_id: &SessionId, item: &Item);
    /// Text appended to a streaming item; ephemeral, never journaled.
    fn delta(&self, session_id: &SessionId, item_id: &ItemId, text: &str);
    /// The session list changed (a session was created); carries the new list.
    fn sessions_changed(&self, sessions: &[SessionHead]);
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
    /// Whether sessions may fail over to this account when theirs hits a limit; opt-in.
    pub failover: bool,
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
}

/// Creates, lists and drives every session on this host. Cheap to clone.
#[derive(Clone)]
pub struct SessionManager {
    inner: Arc<Inner>,
}

struct Inner {
    journal: Journal,
    adapters: Adapters,
    accounts: Accounts,
    turn_ids: TurnIds,
    worktrees: Worktrees,
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
    shutdown: CancellationToken,
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
                SessionStatus::Running | SessionStatus::NeedsYou
            ) {
                actor::close_abandoned_turn(&journal, &tasks, &session).await?;
            }
        }
        Ok(Self {
            inner: Arc::new(Inner {
                journal,
                adapters: setup.adapters,
                accounts: setup.accounts,
                turn_ids: setup.turn_ids,
                worktrees: setup.worktrees,
                actors: Mutex::new(HashMap::new()),
                prs: OnceLock::new(),
                mcp: OnceLock::new(),
                tasks,
                notifier: OnceLock::new(),
                scopes: OnceLock::new(),
                shutdown,
            }),
        })
    }

    /// Applies a command from `by`, from whichever client sent it. Effects arrive at the sink.
    ///
    /// Command-id idempotency is the caller's: this applies every call.
    pub async fn handle(
        &self,
        by: UserId,
        command: CommandBody,
    ) -> Result<CommandResult, ErrorInfo> {
        let (session_id, request) = match command {
            CommandBody::CreateSession {
                repo,
                branch,
                account_id,
                model,
                permission_mode,
            } => {
                let request = CreateRequest {
                    repo,
                    branch,
                    account_id,
                    model,
                    permission_mode,
                    parent: None,
                    task: None,
                };
                let (session_id, _) = self.create_session(Some(by), request).await?;
                return Ok(CommandResult::SessionCreated { session_id });
            }
            CommandBody::SendPrompt { session_id, text } => {
                (session_id, Request::SendPrompt { text, queued: None })
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
            CommandBody::SwitchAccount { .. }
            | CommandBody::SwitchProvider { .. }
            | CommandBody::OpenTerminal { .. }
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

    /// Announces every child request that goes to the user instead of its primary session to
    /// `notifier` from now on ([`routing`]); once per manager. Without one, nothing is
    /// announced beyond the journal.
    pub fn notify_escalations(&self, notifier: Arc<dyn Notifier>) -> anyhow::Result<()> {
        self.inner
            .notifier
            .set(notifier)
            .map_err(|_| anyhow::anyhow!("escalations have a notifier already"))
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

    /// Every account sessions may run on, as clients see them.
    pub fn accounts(&self) -> Vec<herder_protocol::Account> {
        crate::accounts::list(&self.inner.accounts)
    }

    /// Every session with its latest seq, ordered by session id.
    pub async fn sessions(&self) -> anyhow::Result<Vec<SessionHead>> {
        self.inner.journal.heads().await
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

    /// Creates a session with its worktree; returns its id and branch.
    async fn create_session(
        &self,
        by: Option<UserId>,
        request: CreateRequest,
    ) -> Result<(SessionId, String), ErrorInfo> {
        let inner = &self.inner;
        let account = inner.accounts.get(&request.account_id).ok_or_else(|| {
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
        };
        inner
            .journal
            .record(session_id.clone(), by, body)
            .await
            .map_err(internal)?;
        if let Some(prs) = inner.prs.get() {
            prs.install(&session_id, &worktree.path).await;
        }
        match inner.journal.heads().await {
            Ok(heads) => inner.journal.sink().sessions_changed(&heads),
            Err(err) => warn!("cannot list sessions after creating {session_id}: {err:#}"),
        }
        Ok((session_id, worktree.branch))
    }

    /// Sends `session_id` a prompt from its primary's agent; returns whether it waits behind a
    /// running turn.
    async fn prompt(&self, session_id: &SessionId, text: String) -> Result<bool, ErrorInfo> {
        let (queued, busy) = oneshot::channel();
        let request = Request::SendPrompt {
            text,
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
