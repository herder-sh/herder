//! One task per live session: owns the adapter session, applies commands in order, journals
//! what the agent does.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use herder_adapters::{AdapterCommand, AdapterEvent, AdapterSession, Capabilities, StartRequest};
use herder_protocol::{
    AccountId, Answer, Answerer, ApprovalDecision, ApprovalId, ApprovalOutcome, CommandResult,
    ErrorClass, ErrorCode, ErrorInfo, EscalationReason, EventBody, Item, ItemBody, ItemId,
    PermissionMode, QuestionId, Route, SessionId, SessionStatus, TurnError, TurnId, UserId,
};
use herder_store::{QueuedPrompt, Session};
use herder_tasktools::{self as tasktools, AnswerInput, RequestRef, ToolError, WaitForOutput};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::journal::Journal;
use super::routing::{Escalation, PRIMARY_TIMEOUT, within_authority};
use super::setup::{self, Outcome};
use super::tasks::Tasks;
use super::{Inner, error};
use crate::handoff;
use crate::resources::{Permit, Ticket, processes};
use crate::worktree::{self, checkpoint};

/// How long a stopping session waits for its CLI to exit.
const EXIT_GRACE: Duration = Duration::from_secs(5);

/// The tool name of the setup command's tool call item, which herder makes, not the agent.
const SETUP_TOOL: &str = "herder_setup";

/// A command for one session, from the user `by` (`None` for the primary session's agent),
/// answered on `reply`.
pub(super) struct SessionCommand {
    pub(super) by: Option<UserId>,
    pub(super) request: Request,
    pub(super) reply: oneshot::Sender<Result<CommandResult, ErrorInfo>>,
}

/// What a session command asks for.
pub(super) enum Request {
    /// Queues a prompt; `queued` learns whether it waits behind a running turn.
    SendPrompt {
        text: String,
        queued: Option<oneshot::Sender<bool>>,
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
    /// The first answer to an open question applies.
    AnswerQuestion {
        question_id: QuestionId,
        answer: Answer,
    },
    /// The primary session's agent answers or escalates one of this child's requests; `done`
    /// learns the outcome, as the tool reports it.
    FromPrimary {
        primary: SessionId,
        act: PrimaryAct,
        done: oneshot::Sender<Result<(), ToolError>>,
    },
    /// Removes the worktree, keeping its branches, and makes the session read-only.
    Archive {
        force: bool,
    },
    /// Moves the session to another account between turns, replaying its transcript.
    Switch {
        account_id: AccountId,
        to: Switch,
    },
    /// Runs the project's setup command in the new worktree, for at most `timeout`; no turn
    /// starts until it succeeded.
    SetUp {
        command: String,
        timeout: Duration,
    },
}

/// What a switch to another account changes.
pub(super) enum Switch {
    /// Another account of the same provider.
    Account,
    /// An account of another provider, on `model` or the provider's default.
    Provider { model: Option<String> },
}

/// What a primary session does with a child's request, with the adapter's ids.
pub(super) enum PrimaryAct {
    Answer(AnswerInput),
    Escalate {
        request: RequestRef,
        note: Option<String>,
    },
}

/// A prompt waiting for its turn, or the running turn's.
#[derive(Clone, PartialEq)]
struct Prompt {
    by: Option<UserId>,
    text: String,
    /// Whether it retries a turn that hit a limit, on the account failover moved to.
    retry: bool,
}

/// The setup command running in the worktree, as the tool call `call_id` of the turn `turn_id`.
struct SetUp {
    turn_id: TurnId,
    call_id: ItemId,
    command: String,
    /// Kills it.
    cancel: CancellationToken,
    done: oneshot::Receiver<Outcome>,
}

/// An open question or approval request: who it waits for.
struct Open {
    route: Route,
    /// When it goes to the user if the primary session has not answered it.
    deadline: Option<Instant>,
    /// The request as the primary session and the notifier see it.
    request: tasktools::Request,
}

pub(super) struct Actor {
    inner: Arc<Inner>,
    /// The session's projection, kept current as this actor journals changes.
    session: Session,
    adapter: Option<AdapterSession>,
    /// The turn the adapter is running.
    turn: Option<TurnId>,
    /// The host's admission of the running turn, or of the next one while it waits to start.
    permit: Option<Permit>,
    /// Where the next turn's permit arrives while the host has no room for it.
    waiting: Option<oneshot::Receiver<Permit>>,
    /// The running turn's prompt, for a failover retry.
    prompt: Option<Prompt>,
    /// Prompts waiting for the running turn to end, oldest first.
    queue: VecDeque<Prompt>,
    /// The latest assistant message of the running turn: a child's report when it ends.
    last_reply: Option<String>,
    /// Approval requests of the running turn not yet answered, oldest first.
    approvals: Vec<(ApprovalId, Open)>,
    /// Questions of the running turn not yet answered, with how many choices each offers.
    questions: HashMap<QuestionId, (usize, Open)>,
    /// A child's tool calls of the running turn, by item: what its approval requests ask for.
    tool_calls: HashMap<ItemId, (String, Value)>,
    /// The setup command, while it runs.
    setup: Option<SetUp>,
    /// Whether the worktree's setup failed, or was cut short by a restart, and has not
    /// succeeded since: it runs again before the next prompt starts the agent.
    needs_setup: bool,
    /// The queue as last saved to the store.
    saved: Vec<QueuedPrompt>,
}

enum Next {
    Command(SessionCommand),
    Adapter(Option<AdapterEvent>),
    /// The host admitted the next turn; an error means its permit was withdrawn.
    Admitted(Result<Permit, oneshot::error::RecvError>),
    /// The setup command ended; an error means its task is gone.
    SetUp(Result<Outcome, oneshot::error::RecvError>),
    /// A request routed to the primary session ran out of time.
    Overdue,
    Stop,
}

impl Actor {
    pub(super) fn new(session: Session, inner: Arc<Inner>) -> Self {
        Self {
            inner,
            session,
            adapter: None,
            turn: None,
            permit: None,
            waiting: None,
            prompt: None,
            queue: VecDeque::new(),
            last_reply: None,
            approvals: Vec::new(),
            questions: HashMap::new(),
            tool_calls: HashMap::new(),
            setup: None,
            needs_setup: false,
            saved: Vec::new(),
        }
    }

    /// Picks up what a previous daemon left: the prompts it had queued, and whether the
    /// worktree's setup is still to succeed. The queued prompts start as the host admits them.
    async fn restore(&mut self) {
        let journal = &self.inner.journal;
        let session_id = self.session.session_id.clone();
        match journal.all(session_id.clone()).await {
            Ok(events) => self.needs_setup = setup_unfinished(&events),
            Err(err) => warn!(%session_id, "cannot read the journal: {err:#}"),
        }
        match journal.queued_prompts(session_id.clone()).await {
            Ok(saved) => {
                self.queue = saved
                    .iter()
                    .map(|prompt| Prompt {
                        by: prompt.by.clone(),
                        text: prompt.text.clone(),
                        retry: prompt.retry,
                    })
                    .collect();
                self.saved = saved;
            }
            Err(err) => warn!(%session_id, "cannot read the queued prompts: {err:#}"),
        }
        if self.session.status == SessionStatus::Archived {
            self.queue.clear();
        }
        self.save_queue().await;
        self.start_next().await;
    }

    /// Saves the queue to the store when it changed, so it survives a restart.
    async fn save_queue(&mut self) {
        let queue: Vec<QueuedPrompt> = self
            .queue
            .iter()
            .map(|prompt| QueuedPrompt {
                by: prompt.by.clone(),
                text: prompt.text.clone(),
                retry: prompt.retry,
            })
            .collect();
        if queue == self.saved {
            return;
        }
        let session_id = self.session.session_id.clone();
        match self
            .inner
            .journal
            .set_queued_prompts(session_id.clone(), queue.clone())
            .await
        {
            Ok(()) => self.saved = queue,
            Err(err) => warn!(%session_id, "cannot save the queued prompts: {err:#}"),
        }
    }

    pub(super) async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<SessionCommand>,
        shutdown: CancellationToken,
    ) {
        self.restore().await;
        loop {
            let next = {
                let deadline = self.next_deadline();
                let overdue = async {
                    match deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                };
                let (adapter, waiting, setup) =
                    (&mut self.adapter, &mut self.waiting, &mut self.setup);
                let adapter_event = async {
                    match adapter.as_mut() {
                        Some(adapter) => adapter.events.recv().await,
                        None => std::future::pending().await,
                    }
                };
                let admitted = async {
                    match waiting.as_mut() {
                        Some(waiting) => waiting.await,
                        None => std::future::pending().await,
                    }
                };
                let set_up = async {
                    match setup.as_mut() {
                        Some(setup) => (&mut setup.done).await,
                        None => std::future::pending().await,
                    }
                };
                tokio::select! {
                    () = shutdown.cancelled() => Next::Stop,
                    command = commands.recv() => command.map_or(Next::Stop, Next::Command),
                    event = adapter_event => Next::Adapter(event),
                    permit = admitted => Next::Admitted(permit),
                    outcome = set_up => Next::SetUp(outcome),
                    () = overdue => Next::Overdue,
                }
            };
            match next {
                Next::Command(command) => {
                    let result = self.apply(command.by, command.request).await;
                    // Before the reply, so an accepted prompt survives a restart.
                    self.save_queue().await;
                    // Before the reply, so a `wait_for` after `spawn` or `send` sees the child
                    // working.
                    self.sync_working();
                    // The client may be gone; the command still applied.
                    let _ = command.reply.send(result);
                    self.start_next().await;
                }
                Next::Adapter(event) => self.adapter_event(event).await,
                Next::Admitted(permit) => {
                    self.waiting = None;
                    self.permit = permit.ok();
                    self.start_next().await;
                    if self.turn.is_none() {
                        // Nothing was left to start with it.
                        self.permit = None;
                    }
                }
                Next::Overdue => self.escalate_overdue().await,
                Next::SetUp(outcome) => self.set_up_ended(outcome).await,
                Next::Stop => {
                    self.save_queue().await;
                    if let Some(setup) = self.setup.take() {
                        setup.cancel.cancel();
                    }
                    if let Some(adapter) = self.adapter.take() {
                        let _ = tokio::time::timeout(EXIT_GRACE, stop(adapter)).await;
                    }
                    return;
                }
            }
            self.save_queue().await;
        }
    }

    async fn apply(
        &mut self,
        by: Option<UserId>,
        request: Request,
    ) -> Result<CommandResult, ErrorInfo> {
        // Before the archive check: an archived child's requests are all resolved, which the
        // primary should hear as such.
        if let Request::FromPrimary { primary, act, done } = request {
            let _ = done.send(self.primary_act(primary, act).await);
            return Ok(CommandResult::Applied);
        }
        if self.session.status == SessionStatus::Archived {
            return Err(error(
                ErrorCode::Conflict,
                "the session is archived and read-only",
            ));
        }
        match request {
            Request::SendPrompt { text, queued } => {
                let busy = self.turn.is_some() || !self.queue.is_empty();
                self.queue.push_back(Prompt {
                    by,
                    text,
                    retry: false,
                });
                if let Some(queued) = queued {
                    let _ = queued.send(busy);
                }
            }
            Request::Interrupt => match (&self.turn, &self.adapter, &self.setup) {
                (_, _, Some(setup)) => setup.cancel.cancel(),
                (Some(_), Some(adapter), _) => {
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
                        by,
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
                    self.record(by, EventBody::PermissionModeChanged { mode })
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
                let Some((choices, _)) = self.questions.get(&question_id) else {
                    return Err(error(
                        ErrorCode::NotFound,
                        format!("question {question_id} is not pending"),
                    ));
                };
                if let Answer::Choice { index } = answer
                    && index as usize >= *choices
                {
                    return Err(error(
                        ErrorCode::BadRequest,
                        format!("question {question_id} has no choice {index}"),
                    ));
                }
                self.answer_question(by, question_id, answer, Answerer::User)
                    .await
                    .map_err(super::internal)?;
            }
            Request::Archive { force } => self.archive(by, force).await?,
            Request::Switch { account_id, to } => self.switch(by, account_id, to).await?,
            Request::SetUp { command, timeout } => self.set_up(command, timeout).await,
            Request::FromPrimary { .. } => {}
        }
        Ok(CommandResult::Applied)
    }

    /// Applies the first answer to an open approval: journals it, then lets the agent go on.
    /// The actor takes one command at a time, so of concurrent answers exactly one gets here
    /// while the approval is open.
    async fn answer_approval(
        &mut self,
        by: Option<UserId>,
        approval_id: ApprovalId,
        decision: ApprovalDecision,
    ) -> Result<(), ErrorInfo> {
        let Some(open) = self.approvals.iter().position(|(id, _)| *id == approval_id) else {
            return Err(self.not_open(&approval_id).await);
        };
        self.resolve_approval(by, open, decision, Answerer::User)
            .await
            .map_err(super::internal)
    }

    /// Journals the answer to the open approval at `open`, then lets the agent go on.
    async fn resolve_approval(
        &mut self,
        by: Option<UserId>,
        open: usize,
        decision: ApprovalDecision,
        answered_by: Answerer,
    ) -> Result<()> {
        let approval_id = self.approvals[open].0.clone();
        let body = EventBody::ApprovalResolved {
            approval_id: approval_id.clone(),
            decision: decision.into(),
            answered_by,
        };
        self.record(by, body).await?;
        let (approval_id, open) = self.approvals.remove(open);
        self.withdraw(&open, RequestRef::Approval(approval_id.clone()));
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

    /// Sends the answer to an open question to the agent and journals it.
    async fn answer_question(
        &mut self,
        by: Option<UserId>,
        question_id: QuestionId,
        answer: Answer,
        answered_by: Answerer,
    ) -> Result<()> {
        let Some((_, open)) = self.questions.remove(&question_id) else {
            return Ok(());
        };
        self.withdraw(&open, RequestRef::Question(question_id.clone()));
        if let Some(adapter) = &self.adapter {
            let _ = adapter.commands.send(AdapterCommand::AnswerQuestion {
                question_id: question_id.clone(),
                answer: answer.clone(),
            });
        }
        let body = EventBody::QuestionAnswered {
            question_id,
            answer,
            answered_by,
        };
        self.record(by, body).await?;
        self.settle().await;
        Ok(())
    }

    /// Applies an `answer` or `escalate` of the primary session `primary`.
    async fn primary_act(&mut self, primary: SessionId, act: PrimaryAct) -> Result<(), ToolError> {
        use herder_tasktools::ErrorCode as Tool;
        let internal = |err: anyhow::Error| ToolError::new(Tool::Internal, format!("{err:#}"));
        let request = match &act {
            PrimaryAct::Answer(AnswerInput::Question { question_id, .. }) => {
                RequestRef::Question(question_id.clone())
            }
            PrimaryAct::Answer(AnswerInput::Approval { approval_id, .. }) => {
                RequestRef::Approval(approval_id.clone())
            }
            PrimaryAct::Escalate { request, .. } => request.clone(),
        };
        let route = match &request {
            RequestRef::Question(id) => self.questions.get(id).map(|(_, open)| open.route),
            RequestRef::Approval(id) => self
                .approvals
                .iter()
                .find(|(approval_id, _)| approval_id == id)
                .map(|(_, open)| open.route),
        };
        match route {
            None => return Err(self.not_open_to_primary(&request).await),
            Some(Route::User) => {
                return Err(ToolError::new(
                    Tool::NotAllowed,
                    format!(
                        "{} waits for the user, who decides it; you cannot answer or \
                         escalate it",
                        describe(&request)
                    ),
                ));
            }
            Some(Route::Primary) => {}
        }
        let answered_by = Answerer::Primary {
            session_id: primary,
        };
        match act {
            PrimaryAct::Answer(AnswerInput::Question {
                question_id,
                answer,
            }) => {
                let choices = self.questions.get(&question_id).map_or(0, |(n, _)| *n);
                if let Answer::Choice { index } = answer
                    && index as usize >= choices
                {
                    let message = if choices == 0 {
                        format!("question {question_id} takes a free-text answer: pass `text`")
                    } else {
                        format!(
                            "question {question_id} has no choice {index}; pass 0 to {}",
                            choices - 1
                        )
                    };
                    return Err(ToolError::new(Tool::InvalidArguments, message));
                }
                self.answer_question(None, question_id, answer, answered_by)
                    .await
                    .map_err(internal)
            }
            PrimaryAct::Answer(AnswerInput::Approval {
                approval_id,
                decision,
            }) => {
                let Some(open) = self.approvals.iter().position(|(id, _)| *id == approval_id)
                else {
                    return Ok(());
                };
                self.resolve_approval(None, open, decision, answered_by)
                    .await
                    .map_err(internal)
            }
            PrimaryAct::Escalate { request, note } => {
                self.escalate(&request, EscalationReason::MarkedByPrimary, note)
                    .await;
                Ok(())
            }
        }
    }

    /// Why the primary cannot act on `request`, which is not open: resolved, or never asked.
    async fn not_open_to_primary(&self, request: &RequestRef) -> ToolError {
        use herder_tasktools::ErrorCode as Tool;
        let journal = match self
            .inner
            .journal
            .all(self.session.session_id.clone())
            .await
        {
            Ok(journal) => journal,
            Err(err) => return ToolError::new(Tool::Internal, format!("{err:#}")),
        };
        let asked = journal.iter().any(|event| match (&event.body, request) {
            (EventBody::ApprovalRequested { approval_id, .. }, RequestRef::Approval(id)) => {
                approval_id == id
            }
            (EventBody::QuestionAsked { question_id, .. }, RequestRef::Question(id)) => {
                question_id == id
            }
            _ => false,
        });
        if asked {
            ToolError::new(
                Tool::AlreadyResolved,
                format!(
                    "{} is already resolved: answered, or its turn ended",
                    describe(request)
                ),
            )
        } else {
            ToolError::new(
                Tool::NotFound,
                format!("{} does not exist", describe(request)),
            )
        }
    }

    /// Hands `request`, open and routed to the primary session, to the user for `reason`.
    async fn escalate(
        &mut self,
        request: &RequestRef,
        reason: EscalationReason,
        note: Option<String>,
    ) {
        let open = match request {
            RequestRef::Question(id) => self.questions.get_mut(id).map(|(_, open)| open),
            RequestRef::Approval(id) => self
                .approvals
                .iter_mut()
                .find(|(approval_id, _)| approval_id == id)
                .map(|(_, open)| open),
        };
        let Some(open) = open else { return };
        open.route = Route::User;
        open.deadline = None;
        let seen = open.request.clone();
        let body = match request {
            RequestRef::Question(id) => EventBody::QuestionEscalated {
                question_id: id.clone(),
                reason,
                note: note.clone(),
            },
            RequestRef::Approval(id) => EventBody::ApprovalEscalated {
                approval_id: id.clone(),
                reason,
                note: note.clone(),
            },
        };
        self.log(body).await;
        if let Some(primary) = &self.session.parent {
            self.inner
                .tasks
                .withdraw(primary, &self.session.session_id, request);
        }
        self.notify(seen, reason, note);
        self.settle().await;
    }

    /// Hands every request whose time for the primary session ran out to the user.
    async fn escalate_overdue(&mut self) {
        let now = Instant::now();
        let overdue = |open: &Open| open.deadline.is_some_and(|deadline| deadline <= now);
        let mut requests: Vec<RequestRef> = self
            .approvals
            .iter()
            .filter(|(_, open)| overdue(open))
            .map(|(id, _)| RequestRef::Approval(id.clone()))
            .collect();
        requests.extend(
            self.questions
                .iter()
                .filter(|(_, (_, open))| overdue(open))
                .map(|(id, _)| RequestRef::Question(id.clone())),
        );
        for request in requests {
            self.escalate(&request, EscalationReason::Timeout, None)
                .await;
        }
    }

    /// When the earliest request routed to the primary session goes to the user.
    fn next_deadline(&self) -> Option<Instant> {
        let approvals = self.approvals.iter().map(|(_, open)| open);
        let questions = self.questions.values().map(|(_, open)| open);
        approvals
            .chain(questions)
            .filter_map(|open| open.deadline)
            .min()
    }

    /// Who a new request of this session goes to, and why it skips the primary session.
    async fn route(&self, request: &tasktools::Request, tool_call: Option<&ItemId>) -> Open {
        let (route, deadline) = match (&self.session.parent, request, tool_call) {
            (None, ..) => (Route::User, None),
            (Some(_), tasktools::Request::Approval { .. }, tool_call) => {
                let within = match tool_call.and_then(|id| self.tool_calls.get(id)) {
                    Some((name, input)) => {
                        let worktree = Path::new(&self.session.worktree);
                        within_authority(&self.session.provider, name, input, worktree).await
                    }
                    None => false,
                };
                if within {
                    (Route::Primary, Some(Instant::now() + PRIMARY_TIMEOUT))
                } else {
                    (Route::User, None)
                }
            }
            (Some(_), tasktools::Request::Question { .. }, _) => {
                (Route::Primary, Some(Instant::now() + PRIMARY_TIMEOUT))
            }
        };
        Open {
            route,
            deadline,
            request: request.clone(),
        }
    }

    /// Puts a child's new request, routed as `open` says, to its primary session, or tells
    /// the notifier it went straight to the user.
    fn put(&self, open: &Open) {
        let Some(primary) = &self.session.parent else {
            return;
        };
        match open.route {
            Route::Primary => {
                self.inner
                    .tasks
                    .route(primary, &self.session.session_id, &open.request);
            }
            Route::User => {
                let reason = EscalationReason::ExceedsAuthority;
                self.notify(open.request.clone(), reason, None);
            }
        }
    }

    fn notify(&self, request: tasktools::Request, reason: EscalationReason, note: Option<String>) {
        let (Some(notifier), Some(primary)) = (self.inner.notifier.get(), &self.session.parent)
        else {
            return;
        };
        notifier.escalated(&Escalation {
            primary: primary.clone(),
            child: self.session.session_id.clone(),
            request,
            reason,
            note,
        });
    }

    /// `request`, routed as `open` says, waits for its primary session no more.
    fn withdraw(&self, open: &Open, request: RequestRef) {
        if let (Route::Primary, Some(primary)) = (open.route, &self.session.parent) {
            self.inner
                .tasks
                .withdraw(primary, &self.session.session_id, &request);
        }
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
    /// does; a request routed to the primary session leaves a child `running`.
    async fn settle(&mut self) {
        let for_user = self
            .approvals
            .iter()
            .map(|(_, open)| open)
            .chain(self.questions.values().map(|(_, open)| open))
            .any(|open| open.route == Route::User);
        let status = if for_user {
            SessionStatus::NeedsYou
        } else {
            SessionStatus::Running
        };
        self.set_status(status).await;
    }

    /// Journals every open approval as expired and drops every open question: the turn that
    /// asked has ended, so no answer can reach the agent any more.
    async fn void_requests(&mut self) {
        for (approval_id, open) in std::mem::take(&mut self.approvals) {
            self.withdraw(&open, RequestRef::Approval(approval_id.clone()));
            self.log(voided(approval_id)).await;
        }
        for (question_id, (_, open)) in std::mem::take(&mut self.questions) {
            self.withdraw(&open, RequestRef::Question(question_id));
        }
        self.tool_calls.clear();
    }

    async fn archive(&mut self, by: Option<UserId>, force: bool) -> Result<(), ErrorInfo> {
        if self.turn.is_some() {
            return Err(error(
                ErrorCode::Conflict,
                "a turn is running; interrupt it before archiving",
            ));
        }
        if self.setup.is_some() {
            return Err(error(
                ErrorCode::Conflict,
                "the setup command is running; interrupt it before archiving",
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
        // Whatever the agent left running, such as a dev server, goes with the session.
        if let Some(scopes) = self.inner.scopes.get() {
            scopes.stop(&self.session.session_id).await;
        }
        if let Some(mcp) = self.inner.mcp.get() {
            mcp.revoke(&self.session.session_id);
        }
        let status = SessionStatus::Archived;
        self.record(by, EventBody::SessionStatusChanged { status })
            .await
            .map_err(super::internal)?;
        self.session.status = status;
        // Prompts waiting for capacity can never run now.
        self.queue.clear();
        self.waiting = None;
        self.permit = None;
        Ok(())
    }

    /// Moves the session to `account_id` as `to` says, between turns: stops the current CLI
    /// and journals the switch, so the next prompt starts the new account's CLI seeded with
    /// the transcript.
    async fn switch(
        &mut self,
        by: Option<UserId>,
        account_id: AccountId,
        to: Switch,
    ) -> Result<(), ErrorInfo> {
        let account = self.inner.account(&account_id).ok_or_else(|| {
            error(
                ErrorCode::NotFound,
                format!("account {account_id} does not exist"),
            )
        })?;
        let provider = account.provider.clone();
        let same_provider = provider == self.session.provider;
        let body = match to {
            Switch::Account if !same_provider => {
                return Err(error(
                    ErrorCode::BadRequest,
                    format!(
                        "account {account_id} runs {}, not {}; switch the provider instead",
                        provider.as_str(),
                        self.session.provider.as_str()
                    ),
                ));
            }
            Switch::Account if account_id == self.session.account_id => return Ok(()),
            Switch::Account => EventBody::AccountSwitched {
                account_id: account_id.clone(),
            },
            Switch::Provider { .. } if same_provider => {
                return Err(error(
                    ErrorCode::BadRequest,
                    format!(
                        "account {account_id} runs {} already; switch the account instead",
                        provider.as_str()
                    ),
                ));
            }
            Switch::Provider { model } => EventBody::ProviderSwitched {
                provider: provider.clone(),
                account_id: account_id.clone(),
                // Empty until the new adapter reports the provider's default.
                model: model.unwrap_or_default(),
            },
        };
        if self.inner.adapters.get(&provider).is_none() {
            return Err(error(
                ErrorCode::Unsupported,
                format!("no adapter runs {} sessions", provider.as_str()),
            ));
        }
        if let Some(primary) = &self.session.parent {
            let primary = self
                .inner
                .journal
                .session(primary.clone())
                .await
                .map_err(super::internal)?
                .ok_or_else(|| super::not_found(primary))?;
            if account_id != primary.account_id && !account.failover {
                return Err(error(
                    ErrorCode::BadRequest,
                    format!(
                        "account {account_id} is outside the task's failover chain: a child \
                         session runs on its primary's account or on one that opted in to \
                         failover"
                    ),
                ));
            }
        }
        if self.turn.is_some() {
            return Err(error(
                ErrorCode::Conflict,
                "a turn is running; interrupt it or wait for it to end before switching",
            ));
        }
        if let Some(adapter) = self.adapter.take() {
            let _ = tokio::time::timeout(EXIT_GRACE, stop(adapter)).await;
        }
        self.record(by, body.clone())
            .await
            .map_err(super::internal)?;
        self.session.account_id = account_id;
        if let EventBody::ProviderSwitched { model, .. } = body {
            self.session.provider = provider;
            self.session.model = model;
        }
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

    /// Commits the worktree as the checkpoint after `turn_id`, when checkpoints are on, and
    /// publishes it in the background. A failure is logged: the turn ended all the same.
    async fn checkpoint(&self, turn_id: &TurnId) {
        let Some(config) = self.inner.checkpoints.get() else {
            return;
        };
        let session_id = &self.session.session_id;
        let worktree = PathBuf::from(&self.session.worktree);
        if let Err(err) = checkpoint::snapshot(config, &worktree, session_id, turn_id).await {
            warn!(%session_id, "cannot checkpoint the worktree: {err}");
            return;
        }
        let (config, session_id, turn_id) = (config.clone(), session_id.clone(), turn_id.clone());
        tokio::spawn(async move {
            if let Err(err) = checkpoint::publish(&config, &worktree, &session_id, &turn_id).await {
                warn!(%session_id, "cannot publish the checkpoint: {err}");
            }
        });
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

    /// Starts queued prompts while no turn runs and the host admits them, starting the adapter
    /// when it is not running; `waiting_for_capacity` while the host has no room.
    async fn start_next(&mut self) {
        while self.turn.is_none() && self.setup.is_none() && !self.queue.is_empty() {
            if self.needs_setup && self.adapter.is_none() {
                match self
                    .inner
                    .setup_command(Path::new(&self.session.repo))
                    .await
                {
                    Some((command, timeout)) => return self.set_up(command, timeout).await,
                    // The project has no setup command any more.
                    None => self.needs_setup = false,
                }
            }
            if !self.admitted() {
                self.set_status(SessionStatus::WaitingForCapacity).await;
                return;
            }
            let Some(prompt) = self.queue.pop_front() else {
                return;
            };
            // Before the prompt is journaled, so a restart never runs it twice.
            self.save_queue().await;
            let Prompt { by, text, retry } = prompt;
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
                        let summary = failed(&error);
                        self.log(EventBody::TurnFailed {
                            turn_id: turn_id.clone(),
                            error,
                        })
                        .await;
                        self.permit = None;
                        if self.queue.is_empty() {
                            self.set_status(SessionStatus::NeedsYou).await;
                        }
                        self.report(turn_id, summary).await;
                        continue;
                    }
                }
            }
            self.user_message(by.clone(), &turn_id, text.clone()).await;
            if let Some(adapter) = &self.adapter {
                // A closed channel means the CLI is gone; its `exited` fails this turn.
                let _ = adapter.commands.send(AdapterCommand::SendPrompt {
                    turn_id: turn_id.clone(),
                    text: text.clone(),
                });
            }
            self.turn = Some(turn_id);
            self.prompt = Some(Prompt { by, text, retry });
            self.last_reply = None;
        }
    }

    /// Starts the setup command in the worktree, in the session's scope, as a turn of its own:
    /// `turn_started` and a tool call item now, the result and the turn's end once it ends.
    async fn set_up(&mut self, command: String, timeout: Duration) {
        let session_id = self.session.session_id.clone();
        let turn_id = (self.inner.turn_ids)();
        self.set_status(SessionStatus::Running).await;
        self.log(EventBody::TurnStarted {
            turn_id: turn_id.clone(),
        })
        .await;
        let call = Item {
            id: ItemId::new(ulid::Ulid::new().to_string()),
            turn_id: turn_id.clone(),
            body: ItemBody::ToolCall {
                name: SETUP_TOOL.to_owned(),
                input: serde_json::json!({ "command": command }),
            },
        };
        let call_id = call.id.clone();
        self.log(EventBody::ItemAdded { item: call }).await;
        let launcher = match self.inner.scopes.get() {
            Some(scopes) => {
                let limits = scopes.limits(self.session.parent.is_some());
                scopes.launch(&session_id, &limits)
            }
            None => Vec::new(),
        };
        // Marks what it leaves running, such as a dev server, as the session's.
        let env = [(processes::SESSION_ENV.to_owned(), session_id.to_string())];
        let cwd = PathBuf::from(&self.session.worktree);
        let cancel = CancellationToken::new();
        let (outcome, done) = oneshot::channel();
        tokio::spawn({
            let (command, cancel) = (command.clone(), cancel.clone());
            async move {
                let ended = setup::run(&command, &cwd, &launcher, &env, timeout, cancel).await;
                let _ = outcome.send(ended);
            }
        });
        self.setup = Some(SetUp {
            turn_id,
            call_id,
            command,
            cancel,
            done,
        });
    }

    /// Journals how the setup command ended. Success lets queued prompts start; a failure
    /// leaves the session `error` with the output's tail and drops the prompts queued behind
    /// it, which were meant for a worktree that is not set up. The next prompt runs it again
    /// before it starts the agent.
    async fn set_up_ended(&mut self, outcome: Result<Outcome, oneshot::error::RecvError>) {
        let Some(setup) = self.setup.take() else {
            return;
        };
        let outcome = outcome.unwrap_or_else(|_| Outcome {
            output: String::new(),
            failure: Some("stopped unexpectedly".to_owned()),
        });
        let message = outcome.error_message(&setup.command);
        let result = Item {
            id: ItemId::new(ulid::Ulid::new().to_string()),
            turn_id: setup.turn_id.clone(),
            body: ItemBody::ToolResult {
                call_id: setup.call_id,
                output: outcome.output,
                is_error: message.is_some(),
            },
        };
        self.log(EventBody::ItemAdded { item: result }).await;
        let turn_id = setup.turn_id;
        self.needs_setup = message.is_some();
        let Some(message) = message else {
            self.log(EventBody::TurnCompleted { turn_id }).await;
            if self.queue.is_empty() {
                self.set_status(SessionStatus::Idle).await;
            }
            return self.start_next().await;
        };
        let error = TurnError {
            class: ErrorClass::Fatal,
            message,
        };
        let summary = failed(&error);
        self.log(EventBody::TurnFailed {
            turn_id: turn_id.clone(),
            error,
        })
        .await;
        self.queue.clear();
        self.set_status(SessionStatus::Error).await;
        self.report(turn_id, summary).await;
    }

    /// Whether the next turn may start: it holds a permit or the host grants one now. Otherwise
    /// the turn waits in the host's line, and its permit arrives in the actor's loop.
    fn admitted(&mut self) -> bool {
        if self.waiting.is_some() {
            return false;
        }
        if self.permit.is_some() {
            return true;
        }
        let Some(admission) = self.inner.admission.get() else {
            return true;
        };
        match admission.request(&self.session.session_id) {
            Ticket::Admitted(permit) => {
                self.permit = Some(permit);
                true
            }
            Ticket::Waiting(waiting) => {
                self.waiting = Some(waiting);
                false
            }
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
            .account(&session.account_id)
            .ok_or_else(|| fatal(format!("account {} is not configured", session.account_id)))?;
        let adapter = self.inner.adapters.get(&session.provider).ok_or_else(|| {
            fatal(format!(
                "no adapter runs {} sessions",
                session.provider.as_str()
            ))
        })?;
        let items = self
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
        let seed = handoff::transcript(items, handoff::budget(&session.provider, &session.model));
        let mcp = match self.inner.mcp.get() {
            Some(mcp) => Some(mcp.grant(&session.session_id).map_err(|err| {
                fatal(format!(
                    "granting the session its herder MCP token: {err:#}"
                ))
            })?),
            None => None,
        };
        let launcher = match self.inner.scopes.get() {
            Some(scopes) => {
                let limits = scopes.limits(session.parent.is_some());
                scopes.launch(&session.session_id, &limits)
            }
            None => Vec::new(),
        };
        let mut env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
        // Marks everything the CLI starts as the session's, for archive to find.
        env.insert(
            processes::SESSION_ENV.to_owned(),
            session.session_id.to_string(),
        );
        let request = StartRequest {
            config_dir: account.config_dir.clone(),
            env,
            cwd: PathBuf::from(&session.worktree),
            model: Some(session.model.clone()).filter(|model| !model.is_empty()),
            permission_mode: session.permission_mode,
            seed,
            mcp,
            launcher,
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
                let summary = self
                    .last_reply
                    .take()
                    .unwrap_or_else(|| "The turn ended without a final message.".to_owned());
                let body = EventBody::TurnCompleted {
                    turn_id: turn_id.clone(),
                };
                self.turn_ended(turn_id, body, SessionStatus::Idle, summary)
                    .await;
            }
            AdapterEvent::TurnInterrupted { turn_id } => {
                let body = EventBody::TurnInterrupted {
                    turn_id: turn_id.clone(),
                };
                let summary = "The turn was interrupted.".to_owned();
                self.turn_ended(turn_id, body, SessionStatus::Idle, summary)
                    .await;
            }
            AdapterEvent::TurnFailed { turn_id, error } => {
                if error.class == ErrorClass::LimitReached {
                    return self.limit_reached(turn_id, error).await;
                }
                let mut settled = SessionStatus::NeedsYou;
                let error = match self.out_of_memory(&error).await {
                    Some(oom) => {
                        // The CLI is dying: the next prompt starts a new one, seeded with the
                        // transcript, instead of reaching this one.
                        if let Some(adapter) = self.adapter.take() {
                            tokio::spawn(stop(adapter));
                        }
                        settled = SessionStatus::Error;
                        oom
                    }
                    None => error,
                };
                let summary = failed(&error);
                let body = EventBody::TurnFailed {
                    turn_id: turn_id.clone(),
                    error,
                };
                self.turn_ended(turn_id, body, settled, summary).await;
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
                match &item.body {
                    ItemBody::AssistantMessage { text }
                        if self.turn.as_ref() == Some(&item.turn_id) =>
                    {
                        self.last_reply = Some(text.clone());
                    }
                    ItemBody::ToolCall { name, input } if self.session.parent.is_some() => {
                        self.tool_calls
                            .insert(item.id.clone(), (name.clone(), input.clone()));
                    }
                    _ => {}
                }
                self.log(EventBody::ItemAdded { item }).await;
            }
            AdapterEvent::ApprovalRequested {
                approval_id,
                turn_id,
                tool_call_id,
                summary,
            } => {
                let request = tasktools::Request::Approval {
                    approval_id: approval_id.clone(),
                    summary: summary.clone(),
                };
                let open = self.route(&request, Some(&tool_call_id)).await;
                // Journaled before the primary can see it, so an answer finds it asked.
                let reason = (self.session.parent.is_some() && open.route == Route::User)
                    .then_some(EscalationReason::ExceedsAuthority);
                self.log(EventBody::ApprovalRequested {
                    approval_id: approval_id.clone(),
                    turn_id,
                    tool_call_id,
                    summary,
                    routed_to: open.route,
                    reason,
                })
                .await;
                self.put(&open);
                self.approvals.push((approval_id, open));
                self.settle().await;
            }
            AdapterEvent::QuestionAsked {
                question_id,
                turn_id,
                text,
                choices,
            } => {
                let request = tasktools::Request::Question {
                    question_id: question_id.clone(),
                    text: text.clone(),
                    choices: choices.clone(),
                };
                let open = self.route(&request, None).await;
                self.log(EventBody::QuestionAsked {
                    question_id: question_id.clone(),
                    turn_id,
                    text,
                    choices: choices.clone(),
                    routed_to: open.route,
                    reason: None,
                })
                .await;
                self.put(&open);
                self.questions.insert(question_id, (choices.len(), open));
                self.settle().await;
            }
            AdapterEvent::UsageReported { windows } => {
                // Account usage is published to clients, not journaled.
                self.inner.report_usage(&self.session.account_id, windows);
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

    /// Journals the end of the running turn and reports it as `summary`, then starts the next
    /// queued prompt or settles on `settled`.
    async fn turn_ended(
        &mut self,
        turn_id: TurnId,
        body: EventBody,
        settled: SessionStatus,
        summary: String,
    ) {
        self.void_requests().await;
        self.log(body).await;
        self.record_branches().await;
        self.checkpoint(&turn_id).await;
        self.turn = None;
        // The next turn asks again, behind every turn already waiting.
        self.permit = None;
        self.prompt = None;
        if self.queue.is_empty() {
            self.set_status(settled).await;
        }
        self.report(turn_id, summary).await;
        self.start_next().await;
    }

    /// The running turn hit the account's limit: fails over to the next eligible account and
    /// retries the turn's prompt there, unless the session is pinned, the turn was a retry
    /// already, or no account is eligible; then the session needs the user.
    async fn limit_reached(&mut self, turn_id: TurnId, error: TurnError) {
        let failing = self.session.account_id.clone();
        self.inner.limit_hit(&failing);
        let prompt = self.prompt.take().filter(|prompt| !prompt.retry);
        let target = match prompt {
            Some(_) if !self.inner.pinned() => {
                self.inner.failover_target(&self.session.provider, &failing)
            }
            _ => None,
        };
        let (Some(prompt), Some(account_id)) = (prompt, target) else {
            let summary = failed(&error);
            let body = EventBody::TurnFailed {
                turn_id: turn_id.clone(),
                error,
            };
            return self
                .turn_ended(turn_id, body, SessionStatus::NeedsYou, summary)
                .await;
        };
        self.void_requests().await;
        self.log(EventBody::TurnFailed {
            turn_id: turn_id.clone(),
            error: error.clone(),
        })
        .await;
        self.record_branches().await;
        self.checkpoint(&turn_id).await;
        self.turn = None;
        let to = if self.inner.account(&account_id).map(|a| a.provider)
            == Some(self.session.provider.clone())
        {
            Switch::Account
        } else {
            Switch::Provider { model: None }
        };
        if let Err(err) = self.switch(None, account_id.clone(), to).await {
            warn!(
                session_id = %self.session.session_id,
                "cannot fail over to {account_id}: {}", err.message
            );
            self.permit = None;
            if self.queue.is_empty() {
                self.set_status(SessionStatus::NeedsYou).await;
            }
            self.report(turn_id, failed(&error)).await;
            return self.start_next().await;
        }
        // The retry keeps the failed turn's permit: it is the same work, moved.
        self.queue.push_front(Prompt {
            retry: true,
            ..prompt
        });
        self.start_next().await;
    }

    /// The CLI is gone: fails a turn it left open; the next prompt starts it again, seeded with
    /// the transcript.
    async fn exited(&mut self, error: Option<TurnError>) {
        self.adapter = None;
        let error = match error {
            Some(error) => Some(self.out_of_memory(&error).await.unwrap_or(error)),
            None => None,
        };
        self.prompt = None;
        let open = self.turn.take();
        self.permit = None;
        self.void_requests().await;
        let mut summary = None;
        if let Some(turn_id) = &open {
            let error = error.clone().unwrap_or_else(|| TurnError {
                class: ErrorClass::Transient,
                message: "the agent exited during the turn".to_owned(),
            });
            summary = Some(failed(&error));
            let turn_id = turn_id.clone();
            self.log(EventBody::TurnFailed { turn_id, error }).await;
            self.record_branches().await;
        }
        if self.queue.is_empty() {
            if error.is_some() {
                self.set_status(SessionStatus::Error).await;
            } else if open.is_some() {
                self.set_status(SessionStatus::NeedsYou).await;
            }
        }
        if let (Some(turn_id), Some(summary)) = (open, summary) {
            self.report(turn_id, summary).await;
        }
        self.start_next().await;
    }

    /// The error to journal instead of `error`, when the CLI failed because the kernel's OOM
    /// killer killed a process in its scope: `error` names neither, as the CLI only saw a
    /// signal.
    async fn out_of_memory(&self, error: &TurnError) -> Option<TurnError> {
        if error.class != ErrorClass::Fatal {
            return None;
        }
        let scopes = self.inner.scopes.get()?;
        if !scopes.oom_killed(&self.session.session_id).await {
            return None;
        }
        let limit = scopes.limits(self.session.parent.is_some()).memory_max / (1024 * 1024);
        warn!(
            session_id = %self.session.session_id,
            "the agent CLI failed after an OOM kill in its scope: {}", error.message
        );
        Some(TurnError {
            class: ErrorClass::Fatal,
            message: format!(
                "the agent ran out of memory: the kernel killed it, or a process it ran, at its \
                 session's {limit} MiB limit ({}). The next prompt restarts the agent from the \
                 session's transcript",
                error.message
            ),
        })
    }

    /// A child's turn ended: journals `child_reported` in its primary session and hands the
    /// report to the primary's `wait_for`. Nothing for a top-level session.
    async fn report(&mut self, turn_id: TurnId, summary: String) {
        let Some(parent) = self.session.parent.clone() else {
            return;
        };
        let child = self.session.session_id.clone();
        let body = EventBody::ChildReported {
            child_session_id: child.clone(),
            turn_id: turn_id.clone(),
            summary: summary.clone(),
        };
        if let Err(err) = self.inner.journal.record(parent.clone(), None, body).await {
            warn!(session_id = %child, "cannot journal a report to {parent}: {err:#}");
        }
        let output = WaitForOutput::Report {
            child: child.clone(),
            turn_id,
            summary,
            status: self.session.status,
        };
        let working = self.turn.is_some() || !self.queue.is_empty();
        self.inner.tasks.report(&parent, &child, output, working);
    }

    /// Tells the task registry whether this child has a turn running or a prompt queued.
    fn sync_working(&self) {
        if let Some(parent) = &self.session.parent {
            let working = self.turn.is_some() || !self.queue.is_empty();
            self.inner
                .tasks
                .set_working(parent, &self.session.session_id, working);
        }
    }

    async fn user_message(&mut self, by: Option<UserId>, turn_id: &TurnId, text: String) {
        let item = Item {
            id: ItemId::new(ulid::Ulid::new().to_string()),
            turn_id: turn_id.clone(),
            body: ItemBody::UserMessage { text },
        };
        if let Err(err) = self.record(by, EventBody::ItemAdded { item }).await {
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
///
/// A child reports the closed turn to its primary session, as for any turn's end.
pub(super) async fn close_abandoned_turn(
    journal: &Journal,
    tasks: &Tasks,
    session: &Session,
) -> Result<()> {
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
    let (status, report) = match open {
        Some(turn_id) => {
            let error = TurnError {
                class: ErrorClass::Transient,
                message: "the daemon stopped during this turn".to_owned(),
            };
            let summary = failed(&error);
            let body = EventBody::TurnFailed {
                turn_id: turn_id.clone(),
                error,
            };
            journal.record(id.clone(), None, body).await?;
            (SessionStatus::NeedsYou, Some((turn_id, summary)))
        }
        None if matches!(
            session.status,
            SessionStatus::Running | SessionStatus::WaitingForCapacity
        ) =>
        {
            // Queued prompts start again once the manager resumes; their status is theirs.
            if !journal.queued_prompts(id.clone()).await?.is_empty() {
                return Ok(());
            }
            (SessionStatus::Idle, None)
        }
        None => return Ok(()),
    };
    if status != session.status {
        journal
            .record(id.clone(), None, EventBody::SessionStatusChanged { status })
            .await?;
    }
    if let (Some(parent), Some((turn_id, summary))) = (&session.parent, report) {
        let body = EventBody::ChildReported {
            child_session_id: id.clone(),
            turn_id: turn_id.clone(),
            summary: summary.clone(),
        };
        journal.record(parent.clone(), None, body).await?;
        let output = WaitForOutput::Report {
            child: id.clone(),
            turn_id,
            summary,
            status,
        };
        tasks.report(parent, id, output, false);
    }
    Ok(())
}

/// Whether the latest setup command in `journal` has not succeeded: it failed, or its turn was
/// closed by a restart.
fn setup_unfinished(journal: &[herder_protocol::Event]) -> bool {
    let mut unfinished = None;
    for event in journal {
        match &event.body {
            EventBody::ItemAdded { item } => {
                if let ItemBody::ToolCall { name, .. } = &item.body
                    && name == SETUP_TOOL
                {
                    unfinished = Some(item.turn_id.clone());
                }
            }
            EventBody::TurnCompleted { turn_id } if unfinished.as_ref() == Some(turn_id) => {
                unfinished = None;
            }
            _ => {}
        }
    }
    unfinished.is_some()
}

/// A failed turn's report.
pub(super) fn failed(error: &TurnError) -> String {
    format!("The turn failed: {}", error.message)
}

/// `request` for an error message.
fn describe(request: &RequestRef) -> String {
    match request {
        RequestRef::Question(id) => format!("question {id}"),
        RequestRef::Approval(id) => format!("approval request {id}"),
    }
}

/// An approval the daemon closed because no answer can reach the agent any more.
fn voided(approval_id: ApprovalId) -> EventBody {
    EventBody::ApprovalResolved {
        approval_id,
        decision: ApprovalOutcome::Expired,
        answered_by: Answerer::User,
    }
}
