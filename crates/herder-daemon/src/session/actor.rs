//! One task per live session: owns the adapter session, applies commands in order, journals
//! what the agent does.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use herder_adapters::{AdapterCommand, AdapterEvent, AdapterSession, Capabilities, StartRequest};
use herder_protocol::{
    Answer, Answerer, ApprovalDecision, ApprovalId, ApprovalOutcome, CommandResult, ErrorClass,
    ErrorCode, ErrorInfo, EventBody, Item, ItemBody, ItemId, PermissionMode, QuestionId, Route,
    SessionStatus, TurnError, TurnId, UserId,
};
use herder_store::Session;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::journal::Journal;
use super::{Inner, error};
use crate::worktree;

/// How long a stopping session waits for its CLI to exit.
const EXIT_GRACE: Duration = Duration::from_secs(5);

/// A command for one session, from the user `by`, answered on `reply`.
pub(super) struct SessionCommand {
    pub(super) by: UserId,
    pub(super) request: Request,
    pub(super) reply: oneshot::Sender<Result<CommandResult, ErrorInfo>>,
}

/// What a session command asks for.
pub(super) enum Request {
    SendPrompt {
        text: String,
    },
    Interrupt,
    SetModel {
        model: String,
    },
    SetPermissionMode {
        mode: PermissionMode,
    },
    /// The first answer to an open approval applies; any later one is refused.
    AnswerApproval {
        approval_id: ApprovalId,
        decision: ApprovalDecision,
    },
    /// Passed to the adapter as given. Routing questions to the primary session is P2b.4.
    AnswerQuestion {
        question_id: QuestionId,
        answer: Answer,
    },
    /// Removes the worktree, keeping its branches, and makes the session read-only.
    Archive {
        force: bool,
    },
}

pub(super) struct Actor {
    inner: Arc<Inner>,
    /// The session's projection, kept current as this actor journals changes.
    session: Session,
    adapter: Option<AdapterSession>,
    /// The turn the adapter is running.
    turn: Option<TurnId>,
    /// Prompts waiting for the running turn to end, oldest first.
    queue: VecDeque<(UserId, String)>,
    /// Approval requests of the running turn not yet answered, oldest first, with who each is
    /// put to.
    approvals: Vec<(ApprovalId, Route)>,
    /// Questions of the running turn not yet answered, with how many choices each offers.
    questions: HashMap<QuestionId, usize>,
}

enum Next {
    Command(SessionCommand),
    Adapter(Option<AdapterEvent>),
    Stop,
}

impl Actor {
    pub(super) fn new(session: Session, inner: Arc<Inner>) -> Self {
        Self {
            inner,
            session,
            adapter: None,
            turn: None,
            queue: VecDeque::new(),
            approvals: Vec::new(),
            questions: HashMap::new(),
        }
    }

    pub(super) async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<SessionCommand>,
        shutdown: CancellationToken,
    ) {
        loop {
            let next = {
                let adapter_event = async {
                    match self.adapter.as_mut() {
                        Some(adapter) => adapter.events.recv().await,
                        None => std::future::pending().await,
                    }
                };
                tokio::select! {
                    () = shutdown.cancelled() => Next::Stop,
                    command = commands.recv() => command.map_or(Next::Stop, Next::Command),
                    event = adapter_event => Next::Adapter(event),
                }
            };
            match next {
                Next::Command(command) => {
                    let result = self.apply(command.by, command.request).await;
                    // The client may be gone; the command still applied.
                    let _ = command.reply.send(result);
                    self.start_next().await;
                }
                Next::Adapter(event) => self.adapter_event(event).await,
                Next::Stop => {
                    if let Some(adapter) = self.adapter.take() {
                        let _ = tokio::time::timeout(EXIT_GRACE, stop(adapter)).await;
                    }
                    return;
                }
            }
        }
    }

    async fn apply(&mut self, by: UserId, request: Request) -> Result<CommandResult, ErrorInfo> {
        if self.session.status == SessionStatus::Archived {
            return Err(error(
                ErrorCode::Conflict,
                "the session is archived and read-only",
            ));
        }
        match request {
            Request::SendPrompt { text } => {
                self.queue.push_back((by, text));
            }
            Request::Interrupt => match (&self.turn, &self.adapter) {
                (Some(_), Some(adapter)) => {
                    let _ = adapter.commands.send(AdapterCommand::Interrupt);
                }
                _ => return Err(error(ErrorCode::Conflict, "no turn is running")),
            },
            Request::SetModel { model } => {
                if model != self.session.model {
                    let command = AdapterCommand::SetModel {
                        model: model.clone(),
                    };
                    self.change(|caps| caps.native_model_switch, command)?;
                    self.record(
                        Some(by),
                        EventBody::ModelSwitched {
                            model: model.clone(),
                        },
                    )
                    .await
                    .map_err(super::internal)?;
                    self.session.model = model;
                }
            }
            Request::SetPermissionMode { mode } => {
                if mode != self.session.permission_mode {
                    let command = AdapterCommand::SetPermissionMode { mode };
                    self.change(|caps| caps.native_permission_mode_switch, command)?;
                    self.record(Some(by), EventBody::PermissionModeChanged { mode })
                        .await
                        .map_err(super::internal)?;
                    self.session.permission_mode = mode;
                }
            }
            Request::AnswerApproval {
                approval_id,
                decision,
            } => self.answer_approval(by, approval_id, decision).await?,
            Request::AnswerQuestion {
                question_id,
                answer,
            } => {
                let Some(&choices) = self.questions.get(&question_id) else {
                    return Err(error(
                        ErrorCode::NotFound,
                        format!("question {question_id} is not pending"),
                    ));
                };
                if let Answer::Choice { index } = answer
                    && index as usize >= choices
                {
                    return Err(error(
                        ErrorCode::BadRequest,
                        format!("question {question_id} has no choice {index}"),
                    ));
                }
                self.questions.remove(&question_id);
                if let Some(adapter) = &self.adapter {
                    let _ = adapter.commands.send(AdapterCommand::AnswerQuestion {
                        question_id: question_id.clone(),
                        answer: answer.clone(),
                    });
                }
                let body = EventBody::QuestionAnswered {
                    question_id,
                    answer,
                    answered_by: Answerer::User,
                };
                self.record(Some(by), body).await.map_err(super::internal)?;
                self.settle().await;
            }
            Request::Archive { force } => self.archive(by, force).await?,
        }
        Ok(CommandResult::Applied)
    }

    /// Applies the first answer to an open approval: journals it, then lets the agent go on.
    /// The actor takes one command at a time, so of concurrent answers exactly one gets here
    /// while the approval is open.
    async fn answer_approval(
        &mut self,
        by: UserId,
        approval_id: ApprovalId,
        decision: ApprovalDecision,
    ) -> Result<(), ErrorInfo> {
        let Some(open) = self.approvals.iter().position(|(id, _)| *id == approval_id) else {
            return Err(self.not_open(&approval_id).await);
        };
        let body = EventBody::ApprovalResolved {
            approval_id: approval_id.clone(),
            decision: decision.into(),
            answered_by: Answerer::User,
        };
        self.record(Some(by), body).await.map_err(super::internal)?;
        self.approvals.remove(open);
        if let Some(adapter) = &self.adapter {
            // A closed channel means the CLI is gone; its `exited` fails the turn.
            let _ = adapter.commands.send(AdapterCommand::AnswerApproval {
                approval_id,
                decision,
            });
        }
        self.settle().await;
        Ok(())
    }

    /// Why an answer to `approval_id`, which is not open, is refused: already resolved, or
    /// never requested.
    async fn not_open(&self, approval_id: &ApprovalId) -> ErrorInfo {
        let journal = match self
            .inner
            .journal
            .all(self.session.session_id.clone())
            .await
        {
            Ok(journal) => journal,
            Err(err) => return super::internal(err),
        };
        let resolved = journal.iter().any(|event| {
            matches!(&event.body, EventBody::ApprovalResolved { approval_id: id, .. } if id == approval_id)
        });
        if resolved {
            error(
                ErrorCode::Conflict,
                format!("approval {approval_id} is already resolved"),
            )
        } else {
            error(
                ErrorCode::NotFound,
                format!("approval {approval_id} does not exist"),
            )
        }
    }

    /// `needs_you` while an open approval or question waits for a user, `running` once none
    /// does.
    async fn settle(&mut self) {
        let for_user = self
            .approvals
            .iter()
            .any(|(_, route)| *route == Route::User);
        let status = if for_user || !self.questions.is_empty() {
            SessionStatus::NeedsYou
        } else {
            SessionStatus::Running
        };
        self.set_status(status).await;
    }

    /// Journals every open approval as expired: the turn that asked has ended, so no answer
    /// can reach the agent any more.
    async fn void_approvals(&mut self) {
        for (approval_id, _) in std::mem::take(&mut self.approvals) {
            self.log(voided(approval_id)).await;
        }
    }

    async fn archive(&mut self, by: UserId, force: bool) -> Result<(), ErrorInfo> {
        if self.turn.is_some() {
            return Err(error(
                ErrorCode::Conflict,
                "a turn is running; interrupt it before archiving",
            ));
        }
        // The reflog goes with the worktree: journal what it knows first.
        self.record_branches().await;
        let session = &self.session;
        self.inner
            .worktrees
            .remove(
                Path::new(&session.repo),
                Path::new(&session.worktree),
                force,
            )
            .await
            .map_err(super::worktree_error)?;
        if let Some(prs) = self.inner.prs.get() {
            let session = &self.session;
            prs.uninstall(
                &session.session_id,
                Path::new(&session.repo),
                Path::new(&session.worktree),
            )
            .await;
        }
        if let Some(adapter) = self.adapter.take() {
            let _ = tokio::time::timeout(EXIT_GRACE, stop(adapter)).await;
        }
        let status = SessionStatus::Archived;
        self.record(Some(by), EventBody::SessionStatusChanged { status })
            .await
            .map_err(super::internal)?;
        self.session.status = status;
        Ok(())
    }

    /// Journals each branch the worktree has had checked out that the session does not own
    /// yet, in the order first checked out. A failure is logged: the next call catches up.
    async fn record_branches(&self) {
        let session = &self.session;
        let checked_out =
            match worktree::branches(Path::new(&session.worktree), &session.branch).await {
                Ok(branches) => branches,
                Err(err) => {
                    warn!(session_id = %session.session_id, "cannot list branches: {err}");
                    return;
                }
            };
        let owned = match self
            .inner
            .journal
            .branches(session.session_id.clone())
            .await
        {
            Ok(owned) => owned,
            Err(err) => {
                warn!(session_id = %session.session_id, "cannot read branches: {err:#}");
                return;
            }
        };
        for branch in checked_out {
            if !owned.contains(&branch) {
                self.log(EventBody::BranchCheckedOut { branch }).await;
            }
        }
    }

    /// Prepares a setting change: sends `command` when the running adapter applies it natively,
    /// otherwise stops an idle adapter so the next start picks the setting up.
    fn change(
        &mut self,
        native: impl Fn(&Capabilities) -> bool,
        command: AdapterCommand,
    ) -> Result<(), ErrorInfo> {
        let Some(adapter) = &self.adapter else {
            return Ok(());
        };
        if native(&adapter.capabilities) {
            let _ = adapter.commands.send(command);
            return Ok(());
        }
        if self.turn.is_some() {
            return Err(error(
                ErrorCode::Conflict,
                "this provider cannot change that while a turn runs",
            ));
        }
        if let Some(adapter) = self.adapter.take() {
            tokio::spawn(stop(adapter));
        }
        Ok(())
    }

    /// Starts queued prompts while no turn runs, starting the adapter when it is not running.
    async fn start_next(&mut self) {
        while self.turn.is_none() {
            let Some((by, text)) = self.queue.pop_front() else {
                return;
            };
            self.set_status(SessionStatus::Running).await;
            let turn_id = (self.inner.turn_ids)();
            if self.adapter.is_none() {
                match self.start_adapter().await {
                    Ok(adapter) => self.adapter = Some(adapter),
                    Err(error) => {
                        self.user_message(by, &turn_id, text).await;
                        self.log(EventBody::TurnStarted {
                            turn_id: turn_id.clone(),
                        })
                        .await;
                        self.log(EventBody::TurnFailed { turn_id, error }).await;
                        if self.queue.is_empty() {
                            self.set_status(SessionStatus::NeedsYou).await;
                        }
                        continue;
                    }
                }
            }
            self.user_message(by, &turn_id, text.clone()).await;
            if let Some(adapter) = &self.adapter {
                // A closed channel means the CLI is gone; its `exited` fails this turn.
                let _ = adapter.commands.send(AdapterCommand::SendPrompt {
                    turn_id: turn_id.clone(),
                    text,
                });
            }
            self.turn = Some(turn_id);
        }
    }

    async fn start_adapter(&self) -> Result<AdapterSession, TurnError> {
        let fatal = |message: String| TurnError {
            class: ErrorClass::Fatal,
            message,
        };
        let session = &self.session;
        let account = self
            .inner
            .accounts
            .get(&session.account_id)
            .ok_or_else(|| fatal(format!("account {} is not configured", session.account_id)))?;
        let adapter = self.inner.adapters.get(&session.provider).ok_or_else(|| {
            fatal(format!(
                "no adapter runs {} sessions",
                session.provider.as_str()
            ))
        })?;
        let seed = self
            .inner
            .journal
            .all(session.session_id.clone())
            .await
            .map_err(|err| TurnError {
                class: ErrorClass::Transient,
                message: format!("reading the transcript: {err:#}"),
            })?
            .into_iter()
            .filter_map(|event| match event.body {
                EventBody::ItemAdded { item } => Some(item),
                _ => None,
            })
            .collect();
        let request = StartRequest {
            config_dir: account.config_dir.clone(),
            env: std::env::vars().collect(),
            cwd: PathBuf::from(&session.worktree),
            model: Some(session.model.clone()).filter(|model| !model.is_empty()),
            permission_mode: session.permission_mode,
            seed,
        };
        adapter.start(request).await
    }

    async fn adapter_event(&mut self, event: Option<AdapterEvent>) {
        let Some(event) = event else {
            return self.exited(None).await;
        };
        match event {
            AdapterEvent::TurnStarted { turn_id } => {
                self.log(EventBody::TurnStarted { turn_id }).await;
            }
            AdapterEvent::TurnCompleted { turn_id } => {
                self.turn_ended(EventBody::TurnCompleted { turn_id }, SessionStatus::Idle)
                    .await;
            }
            AdapterEvent::TurnInterrupted { turn_id } => {
                self.turn_ended(EventBody::TurnInterrupted { turn_id }, SessionStatus::Idle)
                    .await;
            }
            AdapterEvent::TurnFailed { turn_id, error } => {
                let body = EventBody::TurnFailed { turn_id, error };
                self.turn_ended(body, SessionStatus::NeedsYou).await;
            }
            AdapterEvent::ItemStarted { item } => {
                self.inner
                    .journal
                    .sink()
                    .snapshot(&self.session.session_id, &item);
            }
            AdapterEvent::ItemDelta { item_id, text } => {
                self.inner
                    .journal
                    .sink()
                    .delta(&self.session.session_id, &item_id, &text);
            }
            AdapterEvent::ItemCompleted { item } => {
                self.log(EventBody::ItemAdded { item }).await;
            }
            AdapterEvent::ApprovalRequested {
                approval_id,
                turn_id,
                tool_call_id,
                summary,
            } => {
                // Routing to the primary session is P2b.4; until then every request goes to
                // the user.
                let routed_to = Route::User;
                self.log(EventBody::ApprovalRequested {
                    approval_id: approval_id.clone(),
                    turn_id,
                    tool_call_id,
                    summary,
                    routed_to,
                    reason: None,
                })
                .await;
                self.approvals.push((approval_id, routed_to));
                self.settle().await;
            }
            AdapterEvent::QuestionAsked {
                question_id,
                turn_id,
                text,
                choices,
            } => {
                self.questions.insert(question_id.clone(), choices.len());
                self.log(EventBody::QuestionAsked {
                    question_id,
                    turn_id,
                    text,
                    choices,
                    routed_to: Route::User,
                    reason: None,
                })
                .await;
                self.set_status(SessionStatus::NeedsYou).await;
            }
            AdapterEvent::UsageReported { windows } => {
                // Account usage is published by the accounts component, not journaled.
                debug!(session_id = %self.session.session_id, ?windows, "usage reported");
            }
            AdapterEvent::ModelChanged { model } => {
                if model != self.session.model {
                    self.log(EventBody::ModelSwitched {
                        model: model.clone(),
                    })
                    .await;
                    self.session.model = model;
                }
            }
            AdapterEvent::PermissionModeChanged { mode } => {
                if mode != self.session.permission_mode {
                    self.log(EventBody::PermissionModeChanged { mode }).await;
                    self.session.permission_mode = mode;
                }
            }
            AdapterEvent::Exited { error } => self.exited(error).await,
        }
    }

    /// Journals the end of the running turn, then starts the next queued prompt or settles
    /// on `settled`.
    async fn turn_ended(&mut self, body: EventBody, settled: SessionStatus) {
        self.void_approvals().await;
        self.log(body).await;
        self.record_branches().await;
        self.turn = None;
        self.questions.clear();
        if self.queue.is_empty() {
            self.set_status(settled).await;
        } else {
            self.start_next().await;
        }
    }

    /// The CLI is gone: fails a turn it left open; the next prompt starts it again.
    async fn exited(&mut self, error: Option<TurnError>) {
        self.adapter = None;
        let open = self.turn.take();
        self.void_approvals().await;
        self.questions.clear();
        if let Some(turn_id) = &open {
            let error = error.clone().unwrap_or_else(|| TurnError {
                class: ErrorClass::Transient,
                message: "the agent exited during the turn".to_owned(),
            });
            let turn_id = turn_id.clone();
            self.log(EventBody::TurnFailed { turn_id, error }).await;
            self.record_branches().await;
        }
        if !self.queue.is_empty() {
            self.start_next().await;
        } else if error.is_some() {
            self.set_status(SessionStatus::Error).await;
        } else if open.is_some() {
            self.set_status(SessionStatus::NeedsYou).await;
        }
    }

    async fn user_message(&mut self, by: UserId, turn_id: &TurnId, text: String) {
        let item = Item {
            id: ItemId::new(ulid::Ulid::new().to_string()),
            turn_id: turn_id.clone(),
            body: ItemBody::UserMessage { text },
        };
        if let Err(err) = self.record(Some(by), EventBody::ItemAdded { item }).await {
            warn!(session_id = %self.session.session_id, "cannot journal a prompt: {err:#}");
        }
    }

    async fn set_status(&mut self, status: SessionStatus) {
        if status != self.session.status {
            self.log(EventBody::SessionStatusChanged { status }).await;
            self.session.status = status;
        }
    }

    async fn record(&self, by: Option<UserId>, body: EventBody) -> Result<herder_protocol::Event> {
        self.inner
            .journal
            .record(self.session.session_id.clone(), by, body)
            .await
    }

    /// Journals an event the agent or the daemon caused; a failure is logged, since there is
    /// no client to report it to.
    async fn log(&self, body: EventBody) {
        if let Err(err) = self.record(None, body).await {
            warn!(session_id = %self.session.session_id, "cannot journal an event: {err:#}");
        }
    }
}

/// Asks the CLI to exit and waits until it has.
async fn stop(mut adapter: AdapterSession) {
    let _ = adapter.commands.send(AdapterCommand::Shutdown);
    while adapter.events.recv().await.is_some() {}
}

/// Closes a turn left open by a daemon that stopped mid-turn, expiring its open approvals, and
/// settles the session's status.
///
/// An open approval expires, not left open: the CLI that asked is gone with the old daemon,
/// so no answer can reach it, and the next turn starts a new CLI that asks afresh if it still
/// needs to.
pub(super) async fn close_abandoned_turn(journal: &Journal, session: &Session) -> Result<()> {
    let mut open = None;
    let mut approvals = Vec::new();
    for event in journal.all(session.session_id.clone()).await? {
        match event.body {
            EventBody::TurnStarted { turn_id } => open = Some(turn_id),
            EventBody::TurnCompleted { .. }
            | EventBody::TurnInterrupted { .. }
            | EventBody::TurnFailed { .. } => open = None,
            EventBody::ApprovalRequested { approval_id, .. } => approvals.push(approval_id),
            EventBody::ApprovalResolved { approval_id, .. } => {
                approvals.retain(|id| *id != approval_id);
            }
            _ => {}
        }
    }
    let id = &session.session_id;
    for approval_id in approvals {
        journal
            .record(id.clone(), None, voided(approval_id))
            .await?;
    }
    let status = match open {
        Some(turn_id) => {
            let error = TurnError {
                class: ErrorClass::Transient,
                message: "the daemon stopped during this turn".to_owned(),
            };
            journal
                .record(id.clone(), None, EventBody::TurnFailed { turn_id, error })
                .await?;
            SessionStatus::NeedsYou
        }
        None if session.status == SessionStatus::Running => SessionStatus::Idle,
        None => return Ok(()),
    };
    if status != session.status {
        journal
            .record(id.clone(), None, EventBody::SessionStatusChanged { status })
            .await?;
    }
    Ok(())
}

/// An approval the daemon closed because no answer can reach the agent any more.
fn voided(approval_id: ApprovalId) -> EventBody {
    EventBody::ApprovalResolved {
        approval_id,
        decision: ApprovalOutcome::Expired,
        answered_by: Answerer::User,
    }
}
