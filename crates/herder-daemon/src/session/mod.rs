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
//! `idle` → `running` when a turn starts; `needs_you` while an approval is pending or after a
//! failed turn; `error` after the agent exited with an error; `idle` once a turn ends with
//! nothing queued. Every change is journaled as `session_status_changed`.
//!
//! # Restart
//!
//! Sessions are read from the store. A turn left open by a daemon that stopped is closed with
//! a `transient` `turn_failed` when the manager opens. A session's adapter starts lazily on its
//! next prompt, seeded with the journal's items.

mod actor;
mod journal;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

/// A provider account on this host, as the session manager needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountConfig {
    /// Provider the account belongs to; picks the adapter.
    pub provider: Provider,
    /// The account's config dir, handed to the adapter.
    pub config_dir: PathBuf,
}

/// Every account sessions may run on.
pub type Accounts = HashMap<AccountId, AccountConfig>;

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

    fn get(&self, provider: &Provider) -> Option<Arc<dyn Adapter>> {
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
    actors: Mutex<HashMap<SessionId, mpsc::UnboundedSender<SessionCommand>>>,
    shutdown: CancellationToken,
}

impl SessionManager {
    /// Opens the manager on `setup`, closing turns left open by a previous daemon. Sessions stop
    /// their adapters once `shutdown` is cancelled.
    pub async fn open(setup: Setup, shutdown: CancellationToken) -> anyhow::Result<Self> {
        let journal = Journal::new(setup.store, setup.sink);
        for session in journal.sessions().await? {
            if matches!(
                session.status,
                SessionStatus::Running | SessionStatus::NeedsYou
            ) {
                actor::close_abandoned_turn(&journal, &session).await?;
            }
        }
        Ok(Self {
            inner: Arc::new(Inner {
                journal,
                adapters: setup.adapters,
                accounts: setup.accounts,
                turn_ids: setup.turn_ids,
                actors: Mutex::new(HashMap::new()),
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
                };
                return self.create(by, request).await;
            }
            CommandBody::SendPrompt { session_id, text } => {
                (session_id, Request::SendPrompt { text })
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
            CommandBody::AnswerQuestion { .. }
            | CommandBody::SwitchAccount { .. }
            | CommandBody::SwitchProvider { .. }
            | CommandBody::LinkPr { .. }
            | CommandBody::UnlinkPr { .. }
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
        self.send(session_id, by, request).await
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

    async fn create(&self, by: UserId, request: CreateRequest) -> Result<CommandResult, ErrorInfo> {
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
        let (worktree, branch) = existing_worktree(&request.repo, request.branch)?;
        let session_id = SessionId::new(ulid::Ulid::new().to_string());
        let body = EventBody::SessionCreated {
            repo: request.repo,
            worktree,
            branch,
            provider: account.provider.clone(),
            account_id: request.account_id,
            // Empty until the adapter reports the provider's default.
            model: request.model.unwrap_or_default(),
            permission_mode: request.permission_mode,
            parent: None,
            task: None,
        };
        inner
            .journal
            .record(session_id.clone(), Some(by), body)
            .await
            .map_err(internal)?;
        match inner.journal.heads().await {
            Ok(heads) => inner.journal.sink().sessions_changed(&heads),
            Err(err) => warn!("cannot list sessions after creating {session_id}: {err:#}"),
        }
        Ok(CommandResult::SessionCreated { session_id })
    }

    async fn send(
        &self,
        session_id: SessionId,
        by: UserId,
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
            .ok_or_else(|| {
                error(
                    ErrorCode::NotFound,
                    format!("session {session_id} does not exist"),
                )
            })?;
        let (commands, queue) = mpsc::unbounded_channel();
        let actor = Actor::new(session, self.inner.clone());
        tokio::spawn(actor.run(queue, inner.shutdown.clone()));
        actors.insert(session_id.clone(), commands.clone());
        Ok(commands)
    }
}

/// A `create_session` command.
struct CreateRequest {
    repo: String,
    branch: Option<String>,
    account_id: AccountId,
    model: Option<String>,
    permission_mode: herder_protocol::PermissionMode,
}

/// Seam for worktree and branch creation (P1.5): for now the repository directory is used as
/// the worktree as it is, and no branch is created or recorded.
fn existing_worktree(repo: &str, branch: Option<String>) -> Result<(String, String), ErrorInfo> {
    if branch.is_some() {
        return Err(error(
            ErrorCode::Unsupported,
            "creating a branch is not supported yet; omit `branch`",
        ));
    }
    let path = Path::new(repo);
    if !path.is_absolute() || !path.is_dir() {
        return Err(error(
            ErrorCode::BadRequest,
            format!("{repo} is not an absolute path to a directory"),
        ));
    }
    Ok((repo.to_owned(), String::new()))
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
