//! One task per live session: owns the adapter session, applies commands in order, journals
//! what the agent does.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use herder_adapters::{
    AdapterCommand, AdapterEvent, AdapterSession, Capabilities, StartRequest, transcript,
};
use herder_protocol::{
    AccountId, Answer, Answerer, ApprovalDecision, ApprovalId, ApprovalOutcome, Attachment,
    CommandResult, ErrorClass, ErrorCode, ErrorInfo, EscalationReason, EventBody, FollowUp, Image,
    Item, ItemBody, ItemId, MAX_PROMPT_ATTACHMENT_BYTES, PermissionMode, PrState, PromptFile,
    PromptId, QuestionId, Route, SessionId, SessionStatus, Timestamp, TurnError, TurnId, UserId,
};
use herder_store::{NativeSession, QueuedPrompt, Session};
use herder_tasktools::{self as tasktools, AnswerInput, RequestRef, ToolError, WaitForOutput};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::attachments;
use super::journal::Journal;
use super::merge;
use super::routing::{Escalation, PRIMARY_TIMEOUT, within_authority};
use super::setup::{self, Outcome};
use super::tasks::Tasks;
use super::titles;
use super::{AccountConfig, Inner, error};
use crate::handoff;
use crate::resources::{OomKill, Permit, Ticket, processes};
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
    SendAgentMessage {
        text: String,
        message: herder_protocol::AgentMessage,
        done: oneshot::Sender<herder_tasktools::SendSessionOutput>,
    },
    /// Starts a turn with a follow-up prompt from herder; refused unless the session is idle.
    FollowUp {
        text: String,
        follow_up: FollowUp,
    },
    /// Queues a prompt, keeping its images and files; `queued` learns whether it waits behind
    /// a running turn.
    SendPrompt {
        text: String,
        images: Vec<Image>,
        files: Vec<PromptFile>,
        queued: Option<oneshot::Sender<bool>>,
    },
    Interrupt,
    /// Drops a prompt from the queue without running it.
    RemoveQueued {
        prompt_id: PromptId,
    },
    /// Moves a queued prompt just before another, or to the end.
    MoveQueued {
        prompt_id: PromptId,
        before: Option<PromptId>,
    },
    /// Runs a queued prompt next, interrupting the running turn.
    SendQueuedNow {
        prompt_id: PromptId,
    },
    /// Merges queued prompts into the first of them.
    MergeQueued {
        prompt_ids: Vec<PromptId>,
    },
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
    /// Stops the session and makes it read-only, leaving the worktree for
    /// [`Request::RemoveWorktree`].
    Archive,
    /// Archives a child that is done ([`Actor::done`]); does nothing otherwise.
    ArchiveIfDone,
    /// Removes an archived session's worktree, keeping its branches; does nothing once the
    /// session is not archived.
    RemoveWorktree,
    /// Adds an archived session's worktree back and makes the session writable again.
    Unarchive,
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
    /// Another host took the session over: stop it and make it read-only here.
    MovedAway,
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
    prompt_id: PromptId,
    agent_message: Option<herder_protocol::AgentMessage>,
    /// Why herder sent it, for a follow-up. Follow-ups are not saved with the queue: one lost
    /// to a restart is sent again if its pull request still calls for it.
    follow_up: Option<FollowUp>,
    by: Option<UserId>,
    text: String,
    /// The images and files it carries, kept apart ([`attachments`]).
    attachments: Vec<Attachment>,
    /// Whether another immediate failover retry is disabled for this prompt.
    retry: bool,
    retry_at: Option<Timestamp>,
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
    /// The turn the adapter is running: one a prompt started, or one the CLI started on its
    /// own while no prompt ran (without a `prompt`).
    turn: Option<TurnId>,
    /// A turn the CLI started on its own just as `turn`'s prompt was sent: the adapter holds
    /// the prompt until it ends.
    cli_turn: Option<TurnId>,
    /// Agents the adapter runs in the background. They outlive their turn, so the session stays
    /// `running` between turns while any work.
    background: u32,
    /// The host's admission of the running turn, or of the next one while it waits to start.
    permit: Option<Permit>,
    /// Where the next turn's permit arrives while the host has no room for it.
    waiting: Option<oneshot::Receiver<Permit>>,
    /// The running turn's prompt, for a failover retry.
    prompt: Option<Prompt>,
    /// Prompts waiting for the running turn to end, oldest first.
    queue: VecDeque<Prompt>,
    /// Prompts that left the queue to start a turn since the actor started: editing one is a
    /// conflict, not an unknown prompt.
    started: HashSet<PromptId>,
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
    /// Monotonic deadline derived once from the persisted wall-clock reset.
    retry_deadline: Option<Instant>,
    /// Prompts journaled so far, counted once titles need it.
    prompts: Option<u64>,
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
    LimitReset,
    Stop,
}

impl Actor {
    pub(super) fn new(session: Session, inner: Arc<Inner>) -> Self {
        Self {
            inner,
            session,
            adapter: None,
            turn: None,
            cli_turn: None,
            background: 0,
            permit: None,
            waiting: None,
            prompt: None,
            queue: VecDeque::new(),
            started: HashSet::new(),
            last_reply: None,
            approvals: Vec::new(),
            questions: HashMap::new(),
            tool_calls: HashMap::new(),
            setup: None,
            needs_setup: false,
            saved: Vec::new(),
            retry_deadline: None,
            prompts: None,
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
                        prompt_id: prompt.prompt_id.clone(),
                        agent_message: prompt.agent_message.clone(),
                        follow_up: None,
                        by: prompt.by.clone(),
                        text: prompt.text.clone(),
                        attachments: prompt.attachments.clone(),
                        retry: prompt.retry,
                        retry_at: prompt.retry_at,
                    })
                    .collect();
                self.saved = saved;
            }
            Err(err) => warn!(%session_id, "cannot read the queued prompts: {err:#}"),
        }
        if matches!(
            self.session.status,
            SessionStatus::Archived | SessionStatus::Moved
        ) {
            self.queue.clear();
            self.retry_deadline = None;
        }
        self.save_queue().await;
        if let Some(at) = self.queue.front().and_then(|prompt| prompt.retry_at) {
            self.arm_retry(at).await;
        }
        self.start_next().await;
    }

    /// Saves the queue to the store when it changed, so it survives a restart.
    async fn save_queue(&mut self) {
        if let Err(err) = self.persist_queue().await {
            warn!(session_id = %self.session.session_id, "cannot save queued prompts: {err:#}");
        }
    }

    async fn persist_queue(&mut self) -> Result<()> {
        let queue: Vec<QueuedPrompt> = self
            .queue
            .iter()
            .filter(|prompt| prompt.follow_up.is_none())
            .map(|prompt| QueuedPrompt {
                prompt_id: prompt.prompt_id.clone(),
                agent_message: prompt.agent_message.clone(),
                by: prompt.by.clone(),
                text: prompt.text.clone(),
                attachments: prompt.attachments.clone(),
                retry: prompt.retry,
                retry_at: prompt.retry_at,
            })
            .collect();
        if queue != self.saved {
            self.inner
                .journal
                .set_queued_prompts(self.session.session_id.clone(), queue.clone())
                .await?;
            self.saved = queue;
        }
        Ok(())
    }

    pub(super) async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<SessionCommand>,
        shutdown: CancellationToken,
    ) {
        self.restore().await;
        loop {
            let next = {
                let reset = self.retry_deadline;
                let limit_reset = async {
                    match reset {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                };
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
                    () = limit_reset => Next::LimitReset,
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
                Next::LimitReset => {
                    self.retry_deadline = None;
                    if let Some(prompt) = self.queue.front_mut() {
                        prompt.retry_at = None;
                        // A reset is a fresh opportunity, including reactive failover.
                        prompt.retry = false;
                    }
                    self.save_queue().await;
                    self.log(EventBody::SessionStatusChanged {
                        status: SessionStatus::WaitingForCapacity,
                        retry_at: None,
                    })
                    .await;
                    self.start_next().await;
                }
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
        if let Request::Unarchive = request {
            self.unarchive(by).await?;
            return Ok(CommandResult::Applied);
        }
        if let Request::RemoveWorktree = request {
            self.remove_worktree().await?;
            return Ok(CommandResult::Applied);
        }
        if self.session.status == SessionStatus::Archived {
            return Err(error(
                ErrorCode::Conflict,
                "the session is archived and read-only",
            ));
        }
        if self.session.status == SessionStatus::Moved {
            if matches!(request, Request::MovedAway) {
                return Ok(CommandResult::Applied);
            }
            return Err(error(
                ErrorCode::Conflict,
                "another host took the session over; it is read-only here",
            ));
        }
        match request {
            Request::SendAgentMessage {
                text,
                message,
                done,
            } => {
                if super::tasks::rank(self.session.permission_mode)
                    > super::tasks::rank(message.permission_ceiling)
                {
                    return Err(error(
                        ErrorCode::Forbidden,
                        "recipient permissions exceed sender permissions",
                    ));
                }
                let matches = |other: &herder_protocol::AgentMessage| {
                    other.sender_session_id == message.sender_session_id
                        && other.message_id == message.message_id
                };
                let existing = self
                    .queue
                    .iter()
                    .find(|p| p.agent_message.as_ref().is_some_and(&matches))
                    .map(|p| (p.text.clone(), true));
                let existing = match existing {
                    Some(value) => Some(value),
                    None => self
                        .inner
                        .journal
                        .agent_message_text(
                            self.session.session_id.clone(),
                            message.sender_session_id.clone(),
                            message.message_id.clone(),
                        )
                        .await
                        .map_err(super::internal)?
                        .map(|text| (text, false)),
                };
                let existing = match existing {
                    Some(value) => Some(value),
                    None => self
                        .inner
                        .journal
                        .all(self.session.session_id.clone())
                        .await
                        .map_err(super::internal)?
                        .into_iter()
                        .find_map(|event| {
                            if let EventBody::ItemAdded { item } = event.body
                                && item.agent_message.as_ref().is_some_and(&matches)
                                && let ItemBody::UserMessage { text, .. } = item.body
                            {
                                Some((text, false))
                            } else {
                                None
                            }
                        }),
                };
                if let Some((old_text, queued)) = existing {
                    if old_text != text {
                        return Err(error(
                            ErrorCode::Conflict,
                            "message_id was used for different text",
                        ));
                    }
                    let _ = done.send(herder_tasktools::SendSessionOutput {
                        queued,
                        duplicate: true,
                    });
                    return Ok(CommandResult::Applied);
                }
                if self.queue.len() >= 256 {
                    return Err(error(ErrorCode::Conflict, "recipient prompt queue is full"));
                }
                let queued = self.turn.is_some() || !self.queue.is_empty();
                self.queue.push_back(Prompt {
                    prompt_id: new_prompt_id(),
                    agent_message: Some(message),
                    follow_up: None,
                    by: None,
                    text,
                    attachments: Vec::new(),
                    retry: false,
                    retry_at: None,
                });
                // A successful tool result is a durable acceptance, not a best-effort write.
                if let Err(err) = self.persist_queue().await {
                    self.queue.pop_back();
                    return Err(super::internal(err));
                }
                let _ = done.send(herder_tasktools::SendSessionOutput {
                    queued,
                    duplicate: false,
                });
            }
            Request::FollowUp { text, follow_up } => {
                let idle = self.session.status == SessionStatus::Idle
                    && self.turn.is_none()
                    && self.queue.is_empty()
                    && self.setup.is_none()
                    && self.retry_deadline.is_none();
                if !idle {
                    return Err(error(ErrorCode::Conflict, "the session is not idle"));
                }
                self.queue.push_back(Prompt {
                    prompt_id: new_prompt_id(),
                    agent_message: None,
                    follow_up: Some(follow_up),
                    by: None,
                    text,
                    attachments: Vec::new(),
                    retry: false,
                    retry_at: None,
                });
            }
            Request::SendPrompt {
                text,
                images,
                files,
                queued,
            } => {
                let attachments = self.keep(images, files).await?;
                self.cancel_retry(false).await;
                let busy = self.turn.is_some() || !self.queue.is_empty();
                self.queue.push_back(Prompt {
                    prompt_id: new_prompt_id(),
                    agent_message: None,
                    follow_up: None,
                    by,
                    text,
                    attachments,
                    retry: false,
                    retry_at: None,
                });
                if let Some(queued) = queued {
                    let _ = queued.send(busy);
                }
            }
            Request::Interrupt if self.retry_deadline.is_some() => self.cancel_retry(true).await,
            Request::Interrupt => match (&self.turn, &self.adapter, &self.setup) {
                (_, _, Some(setup)) => setup.cancel.cancel(),
                (Some(_), Some(adapter), _) => {
                    let _ = adapter.commands.send(AdapterCommand::Interrupt);
                }
                _ => return Err(error(ErrorCode::Conflict, "no turn is running")),
            },
            Request::RemoveQueued { prompt_id } => {
                let index = self.queued(&prompt_id)?;
                self.queue.remove(index);
            }
            Request::MoveQueued { prompt_id, before } => {
                let index = self.queued(&prompt_id)?;
                if let Some(before) = &before {
                    self.queued(before)?;
                }
                if before.as_ref() != Some(&prompt_id)
                    && let Some(prompt) = self.queue.remove(index)
                {
                    let at = match &before {
                        Some(before) => self.queued(before)?,
                        None => self.queue.len(),
                    };
                    self.queue.insert(at, prompt);
                }
            }
            Request::SendQueuedNow { prompt_id } => {
                self.queued(&prompt_id)?;
                // A retry waiting for a usage limit to reset gives way, as on an interrupt.
                self.cancel_retry(false).await;
                let index = self.queued(&prompt_id)?;
                if let Some(prompt) = self.queue.remove(index) {
                    self.queue.push_front(prompt);
                }
                if let (Some(_), Some(adapter)) = (&self.turn, &self.adapter) {
                    let _ = adapter.commands.send(AdapterCommand::Interrupt);
                }
            }
            Request::MergeQueued { prompt_ids } => self.merge_queued(&prompt_ids)?,
            Request::SetModel { model } => {
                if model != self.session.model {
                    let command = AdapterCommand::SetModel {
                        model: model.clone(),
                    };
                    self.change(|caps| caps.native_model_switch, command)
                        .await?;
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
                self.cancel_retry(true).await;
            }
            Request::SetPermissionMode { mode } => {
                if mode != self.session.permission_mode {
                    let command = AdapterCommand::SetPermissionMode { mode };
                    self.change(|caps| caps.native_permission_mode_switch, command)
                        .await?;
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
            Request::Archive => self.archive(by).await?,
            Request::ArchiveIfDone => {
                if self.turn.is_none() && self.done().await {
                    self.archive(None).await?;
                }
            }
            Request::Switch { account_id, to } => {
                self.switch(by, account_id, to).await?;
                self.cancel_retry(true).await;
            }
            Request::SetUp { command, timeout } => self.set_up(command, timeout).await,
            Request::MovedAway => self.moved_away().await,
            Request::FromPrimary { .. } | Request::Unarchive | Request::RemoveWorktree => {}
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
                ..
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
                ..
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

    async fn archive(&mut self, by: Option<UserId>) -> Result<(), ErrorInfo> {
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
        self.record_branches().await;
        if let Some(prs) = self.inner.prs.get()
            && self.session.branch.is_some()
        {
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
        if let Some(skills) = self.inner.skills.get() {
            skills.session_archived(&self.session.session_id);
        }
        let status = SessionStatus::Archived;
        self.record(
            by,
            EventBody::SessionStatusChanged {
                status,
                retry_at: None,
            },
        )
        .await
        .map_err(super::internal)?;
        self.session.status = status;
        // Prompts waiting for capacity can never run now.
        self.queue.clear();
        self.retry_deadline = None;
        self.waiting = None;
        self.permit = None;
        Ok(())
    }

    /// Where the queued prompt `prompt_id` is in the queue; refused once it started, as
    /// a prompt queued again to retry its turn has.
    fn queued(&self, prompt_id: &PromptId) -> Result<usize, ErrorInfo> {
        match self.queue.iter().position(|p| p.prompt_id == *prompt_id) {
            Some(index) if !self.queue[index].retry => Ok(index),
            Some(_) => Err(started(prompt_id)),
            None if self.started.contains(prompt_id) => Err(started(prompt_id)),
            None => Err(error(
                ErrorCode::NotFound,
                format!("prompt {prompt_id} is not queued"),
            )),
        }
    }

    /// Merges the queued prompts `prompt_ids` into the first of them, as `merge_queued` asks.
    fn merge_queued(&mut self, prompt_ids: &[PromptId]) -> Result<(), ErrorInfo> {
        if prompt_ids.len() < 2 {
            return Err(error(
                ErrorCode::BadRequest,
                "merging takes at least two prompts",
            ));
        }
        let mut indices = Vec::with_capacity(prompt_ids.len());
        for prompt_id in prompt_ids {
            let index = self.queued(prompt_id)?;
            if indices.contains(&index) {
                return Err(error(
                    ErrorCode::BadRequest,
                    format!("prompt {prompt_id} is listed twice"),
                ));
            }
            indices.push(index);
        }
        let prompts: Vec<&Prompt> = indices.iter().map(|&index| &self.queue[index]).collect();
        if prompts.iter().any(|prompt| prompt.agent_message.is_some()) {
            return Err(error(
                ErrorCode::BadRequest,
                "a prompt an agent sent cannot be merged: it would lose who sent it",
            ));
        }
        let by = &prompts[0].by;
        if prompts.iter().any(|prompt| prompt.by != *by) {
            return Err(error(
                ErrorCode::BadRequest,
                "prompts different users sent cannot be merged: it would lose who sent them",
            ));
        }
        let attachments: Vec<Attachment> = prompts
            .iter()
            .flat_map(|prompt| prompt.attachments.iter().cloned())
            .collect();
        let bytes: u64 = attachments.iter().map(|attachment| attachment.size).sum();
        if bytes > MAX_PROMPT_ATTACHMENT_BYTES as u64 {
            return Err(error(
                ErrorCode::BadRequest,
                format!(
                    "the merged prompt's images and files would have more than \
                     {MAX_PROMPT_ATTACHMENT_BYTES} bytes together"
                ),
            ));
        }
        let text = merge::merge_texts(prompts.iter().map(|prompt| {
            let images = prompt
                .attachments
                .iter()
                .filter(|attachment| attachment.name.is_none());
            (prompt.text.as_str(), images.count())
        }));
        let first = indices[0];
        self.queue[first].text = text;
        self.queue[first].attachments = attachments;
        // From the back, so each index still names its prompt.
        let mut rest = indices[1..].to_vec();
        rest.sort_unstable();
        for index in rest.into_iter().rev() {
            self.queue.remove(index);
        }
        Ok(())
    }

    /// Checks a prompt's `images` and `files` and keeps them; images are refused when the
    /// session's adapter cannot take them.
    async fn keep(
        &self,
        images: Vec<Image>,
        files: Vec<PromptFile>,
    ) -> Result<Vec<Attachment>, ErrorInfo> {
        if images.is_empty() && files.is_empty() {
            return Ok(Vec::new());
        }
        let provider = &self.session.provider;
        let takes_images = self
            .inner
            .adapters
            .get(provider)
            .is_some_and(|adapter| adapter.accepts_images());
        if !images.is_empty() && !takes_images {
            return Err(error(
                ErrorCode::Unsupported,
                format!("{} cannot take images with a prompt", provider.as_str()),
            ));
        }
        attachments::validate(&images, &files)?;
        let session_id = &self.session.session_id;
        attachments::save(&self.inner.attachments, session_id, images, files).await
    }

    /// Removes the worktree of a session still archived, keeping its branches.
    async fn remove_worktree(&mut self) -> Result<(), ErrorInfo> {
        if self.session.status != SessionStatus::Archived {
            return Ok(());
        }
        // The reflog goes with the worktree: journal what it knows first.
        self.record_branches().await;
        let session = &self.session;
        self.inner
            .worktrees
            .remove(Path::new(&session.repo), Path::new(&session.worktree))
            .await
            .map_err(super::worktree_error)
    }

    /// Brings an archived session back: its worktree as archive left it, or, once removed,
    /// added back at the path it had on the session's own branch; and makes the session
    /// writable again.
    async fn unarchive(&mut self, by: Option<UserId>) -> Result<(), ErrorInfo> {
        match self.session.status {
            SessionStatus::Archived => {}
            SessionStatus::Moved => {
                return Err(error(
                    ErrorCode::Conflict,
                    "another host took the session over; it is read-only here",
                ));
            }
            _ => return Err(error(ErrorCode::Conflict, "the session is not archived")),
        }
        let session = &self.session;
        match &session.branch {
            Some(branch) => self
                .inner
                .worktrees
                .reopen(
                    Path::new(&session.repo),
                    Path::new(&session.worktree),
                    branch,
                )
                .await
                .map_err(super::worktree_error)?,
            None if !Path::new(&session.worktree).is_dir() => {
                return Err(error(
                    ErrorCode::Conflict,
                    format!("{} no longer exists", session.worktree),
                ));
            }
            None => {}
        }
        if let Some(prs) = self.inner.prs.get()
            && session.branch.is_some()
        {
            prs.install(&session.session_id, Path::new(&session.worktree))
                .await;
        }
        let status = SessionStatus::Idle;
        self.record(
            by,
            EventBody::SessionStatusChanged {
                status,
                retry_at: None,
            },
        )
        .await
        .map_err(super::internal)?;
        self.session.status = status;
        Ok(())
    }

    /// Another host took the session over and goes on with it: stops the CLI, the
    /// setup command and whatever the session left running, fails the open turn and makes the
    /// session read-only here. The worktree stays as the session left it.
    async fn moved_away(&mut self) {
        let mut open = self.turn.take();
        if let Some(setup) = self.setup.take() {
            setup.cancel.cancel();
            open = open.or(Some(setup.turn_id));
        }
        if let Some(adapter) = self.adapter.take() {
            let _ = tokio::time::timeout(EXIT_GRACE, stop(adapter)).await;
        }
        self.void_requests().await;
        if let Some(turn_id) = open {
            let error = TurnError {
                class: ErrorClass::Fatal,
                message: "another host took the session over".to_owned(),
            };
            self.log(EventBody::TurnFailed { turn_id, error }).await;
        }
        if let Some(scopes) = self.inner.scopes.get() {
            scopes.stop(&self.session.session_id).await;
        }
        if let Some(mcp) = self.inner.mcp.get() {
            mcp.revoke(&self.session.session_id);
        }
        self.queue.clear();
        self.retry_deadline = None;
        self.prompt = None;
        self.waiting = None;
        self.permit = None;
        self.set_status(SessionStatus::Moved).await;
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
        if self.turn.is_some() {
            return Err(error(
                ErrorCode::Conflict,
                "a turn is running; interrupt it or wait for it to end before switching",
            ));
        }
        if let Some(adapter) = self.adapter.take() {
            let _ = tokio::time::timeout(EXIT_GRACE, stop(adapter)).await;
            self.background_gone().await;
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
            match worktree::branches(Path::new(&session.worktree), session.branch.as_deref()).await
            {
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
        // A folder without a commit has nothing to checkpoint onto.
        if self.session.branch.is_none() {
            return;
        }
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
    async fn change(
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
            self.background_gone().await;
        }
        Ok(())
    }

    /// Starts queued prompts while no turn runs and the host admits them, starting the adapter
    /// when it is not running; `waiting_for_capacity` while the host has no room.
    async fn start_next(&mut self) {
        while self.turn.is_none() && self.setup.is_none() && !self.queue.is_empty() {
            if self.retry_deadline.is_some() {
                return;
            }
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
            self.started.insert(prompt.prompt_id.clone());
            let original = prompt.clone();
            // The stored queue keeps the prompt until its transcript item takes it off, in one
            // transaction: a restart while the CLI starts runs it then, and never twice.
            let Prompt {
                prompt_id,
                agent_message,
                follow_up,
                by,
                text,
                attachments,
                retry,
                retry_at,
            } = prompt;
            self.set_status(SessionStatus::Running).await;
            let turn_id = (self.inner.turn_ids)();
            if agent_message.as_ref().is_some_and(|message| {
                super::tasks::rank(self.session.permission_mode)
                    > super::tasks::rank(message.permission_ceiling)
            }) {
                if let Err(err) = self.user_message(&turn_id, &original).await {
                    warn!("cannot journal rejected agent message: {err:#}");
                    self.queue.push_front(original);
                    self.set_status(SessionStatus::Error).await;
                    return;
                }
                self.save_queue().await;
                self.log(EventBody::TurnStarted {
                    turn_id: turn_id.clone(),
                })
                .await;
                let error =
                    fatal("Recipient permissions now exceed the sending agent's authority.".into());
                let summary = failed(&error);
                self.log(EventBody::TurnFailed {
                    turn_id: turn_id.clone(),
                    error,
                })
                .await;
                self.permit = None;
                self.report(turn_id, summary, false).await;
                self.set_status(SessionStatus::NeedsYou).await;
                continue;
            }
            if self.adapter.is_none() {
                match self.start_adapter().await {
                    Ok(adapter) => self.adapter = Some(adapter),
                    Err(error) => {
                        if let Err(err) = self.user_message(&turn_id, &original).await {
                            warn!("cannot journal prompt: {err:#}");
                            self.queue.push_front(original);
                            self.set_status(SessionStatus::Error).await;
                            return;
                        }
                        self.save_queue().await;
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
                        self.report(turn_id, summary, false).await;
                        continue;
                    }
                }
            }
            let images = self.images(&attachments).await;
            let prompt_text = self.with_files(&text, &attachments);
            if let Err(err) = self.user_message(&turn_id, &original).await {
                warn!("cannot journal prompt: {err:#}");
                self.queue.push_front(original);
                self.set_status(SessionStatus::Error).await;
                return;
            }
            self.save_queue().await;
            if let Some(adapter) = &self.adapter {
                // A closed channel means the CLI is gone; its `exited` fails this turn.
                let _ = adapter.commands.send(AdapterCommand::SendPrompt {
                    agent_sender: agent_message
                        .as_ref()
                        .map(|message| message.sender_session_id.clone()),
                    turn_id: turn_id.clone(),
                    text: prompt_text,
                    images,
                });
            }
            self.turn = Some(turn_id);
            self.prompt = Some(Prompt {
                prompt_id,
                agent_message,
                follow_up,
                by,
                text,
                attachments,
                retry,
                retry_at,
            });
            self.last_reply = None;
        }
    }

    /// The images `attachments` name, read back for the agent; one that cannot be read is
    /// logged and left out, as the turn can go on without it.
    async fn images(&self, attachments: &[Attachment]) -> Vec<Image> {
        let mut images = Vec::with_capacity(attachments.len());
        for attachment in attachments.iter().filter(|a| a.name.is_none()) {
            let session_id = &self.session.session_id;
            match attachments::load(&self.inner.attachments, session_id, attachment).await {
                Ok(data) => images.push(Image {
                    media_type: attachment.media_type.clone(),
                    data,
                }),
                Err(err) => warn!(%session_id, "leaving an image out: {}", err.message),
            }
        }
        images
    }

    /// `text` as the agent gets it: with a note naming where each file `attachments` name is
    /// on this host, which the CLI runs on, so any agent can read them.
    fn with_files(&self, text: &str, attachments: &[Attachment]) -> String {
        let session_id = &self.session.session_id;
        let paths: Vec<_> = attachments
            .iter()
            .filter(|attachment| attachment.name.is_some())
            .filter_map(|attachment| {
                attachments::path(&self.inner.attachments, session_id, attachment)
            })
            .map(|path| std::path::absolute(&path).unwrap_or(path))
            .collect();
        attachments::with_files(text, &paths)
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
            agent_message: None,
            follow_up: None,
            parent_call_id: None,
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
            agent_message: None,
            follow_up: None,
            parent_call_id: None,
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
            self.log(EventBody::TurnCompleted {
                turn_id,
                usage: None,
            })
            .await;
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
        self.retry_deadline = None;
        self.set_status(SessionStatus::Error).await;
        self.report(turn_id, summary, false).await;
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

    /// Starts the session's CLI on its account: resuming the CLI's own session when a
    /// same-provider account switch carried it over ([`handoff::native`]), else, or when that
    /// fails, seeded with the journal's transcript.
    async fn start_adapter(&self) -> Result<AdapterSession, TurnError> {
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
        let mut env: BTreeMap<String, String> = std::env::vars().collect();
        // Marks everything the CLI starts as the session's, for archive to find.
        env.insert(
            processes::SESSION_ENV.to_owned(),
            session.session_id.to_string(),
        );
        if let Some(prs) = self.inner.prs.get() {
            prs.add_hooks_to_env(&mut env, &session.session_id);
        }
        if let Some(skills) = self.inner.skills.get() {
            skills
                .session_started(
                    &session.session_id,
                    &session.provider,
                    &session.account_id,
                    Path::new(&session.worktree),
                )
                .await;
        }
        if let Some(native_id) = self.carry_over(account.config_dir.as_deref(), &env).await {
            let request = self.start_request(&account, env.clone(), Vec::new(), Some(native_id))?;
            match adapter.start(request).await {
                Ok(started) => return Ok(started),
                Err(err) => warn!(
                    session_id = %session.session_id,
                    "cannot resume the CLI's own session, replaying the transcript: {}",
                    err.message
                ),
            }
        }
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
        let request = self.start_request(&account, env, seed, None)?;
        adapter.start(request).await
    }

    /// The CLI session to resume on the session's account, whose config dir is `to`: the last
    /// one its provider reported on another account, once its transcript is copied over. `None`
    /// when there is none or it cannot be carried over; `env` is the CLI's environment.
    async fn carry_over(
        &self,
        to: Option<&Path>,
        env: &BTreeMap<String, String>,
    ) -> Option<String> {
        let session = &self.session;
        let native = match self
            .inner
            .journal
            .native_session(session.session_id.clone())
            .await
        {
            Ok(native) => native?,
            Err(err) => {
                warn!(session_id = %session.session_id, "cannot read the CLI's session id: {err:#}");
                return None;
            }
        };
        if native.provider != session.provider || native.account_id == session.account_id {
            return None;
        }
        let from = self.inner.account(&native.account_id)?;
        let from = transcript::config_dir(&session.provider, from.config_dir.as_deref(), env)?;
        let to = transcript::config_dir(&session.provider, to, env)?;
        let provider = session.provider.clone();
        let id = native.native_id.clone();
        let copied = tokio::task::spawn_blocking(move || {
            handoff::native::carry_over(&provider, &from, &to, &id)
        })
        .await;
        match copied {
            Ok(Ok(())) => Some(native.native_id),
            Ok(Err(err)) => {
                warn!(
                    session_id = %session.session_id,
                    "cannot carry the CLI's session over from {}: {err:#}", native.account_id
                );
                None
            }
            Err(err) => {
                warn!(session_id = %session.session_id, "the transcript copy panicked: {err}");
                None
            }
        }
    }

    /// A start request for the session's CLI on `account`, with its own launcher.
    fn start_request(
        &self,
        account: &AccountConfig,
        env: BTreeMap<String, String>,
        seed: Vec<Item>,
        resume: Option<String>,
    ) -> Result<StartRequest, TurnError> {
        let session = &self.session;
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
        let skills = self.inner.skills.get().and_then(|skills| {
            let config_dir =
                transcript::config_dir(&session.provider, account.config_dir.as_deref(), &env);
            skills.launch(&session.provider, config_dir.as_deref())
        });
        Ok(StartRequest {
            config_dir: account.config_dir.clone(),
            env,
            cwd: PathBuf::from(&session.worktree),
            model: Some(session.model.clone()).filter(|model| !model.is_empty()),
            permission_mode: session.permission_mode,
            seed,
            resume,
            mcp,
            launcher,
            skills,
        })
    }

    async fn adapter_event(&mut self, event: Option<AdapterEvent>) {
        let Some(event) = event else {
            return self.exited(None).await;
        };
        if let Some(cli_turn) = &self.cli_turn
            && let Some(end) = turn_end(&event, cli_turn)
        {
            return self.cli_turn_ended(end).await;
        }
        match event {
            AdapterEvent::TurnStarted { turn_id } => {
                match &self.turn {
                    // The CLI started a turn on its own, such as its reply to a background
                    // agent's result: it runs like a prompted one, but no user asked for it, so
                    // it has no prompt and nothing to retry on another account.
                    None => {
                        self.turn = Some(turn_id.clone());
                        self.prompt = None;
                        self.last_reply = None;
                        self.set_status(SessionStatus::Running).await;
                        self.sync_working();
                    }
                    Some(turn) if *turn != turn_id => self.cli_turn = Some(turn_id.clone()),
                    Some(_) => {}
                }
                self.log(EventBody::TurnStarted { turn_id }).await;
            }
            AdapterEvent::TurnCompleted { turn_id, usage } => {
                let summary = self
                    .last_reply
                    .take()
                    .unwrap_or_else(|| "The turn ended without a final message.".to_owned());
                let body = EventBody::TurnCompleted {
                    turn_id: turn_id.clone(),
                    usage,
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
                        self.background = 0;
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
            AdapterEvent::ItemStarted { mut item } => {
                item.agent_message = None;
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
            AdapterEvent::ItemCompleted { mut item } => {
                item.agent_message = None;
                match &item.body {
                    ItemBody::AssistantMessage { text }
                        if self.turn.as_ref() == Some(&item.turn_id)
                            && item.parent_call_id.is_none() =>
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
                self.cancel_retry(true).await;
            }
            AdapterEvent::PermissionModeChanged { mode } => {
                if mode != self.session.permission_mode {
                    self.log(EventBody::PermissionModeChanged { mode }).await;
                    self.session.permission_mode = mode;
                }
            }
            AdapterEvent::SessionIdentified { native_id } => {
                // Kept with the account it ran on, so a later start can find its transcript.
                let native = NativeSession {
                    provider: self.session.provider.clone(),
                    account_id: self.session.account_id.clone(),
                    native_id,
                };
                let session_id = self.session.session_id.clone();
                if let Err(err) = self
                    .inner
                    .journal
                    .set_native_session(session_id.clone(), native)
                    .await
                {
                    warn!(%session_id, "cannot save the CLI's session id: {err:#}");
                }
            }
            AdapterEvent::BackgroundAgents { running } => {
                self.background = running;
                self.settle_background().await;
            }
            // Shell commands left running in the background do not keep the session busy.
            AdapterEvent::BackgroundCommands { .. } => {}
            AdapterEvent::Exited { error } => self.exited(error).await,
        }
    }

    /// The adapter was stopped, and its background agents with it.
    async fn background_gone(&mut self) {
        if std::mem::take(&mut self.background) > 0 {
            self.settle_background().await;
        }
    }

    /// `running` while background agents work and nothing else gives the session a status of
    /// its own (a turn, its setup or a queued prompt), `idle` again once none do.
    async fn settle_background(&mut self) {
        if self.turn.is_some() || self.setup.is_some() || !self.queue.is_empty() {
            return;
        }
        match self.session.status {
            SessionStatus::Idle if self.background > 0 => {
                self.set_status(SessionStatus::Running).await;
            }
            SessionStatus::Running if self.background == 0 => {
                self.set_status(SessionStatus::Idle).await;
            }
            _ => {}
        }
    }

    /// Journals the end of a turn the CLI ran on its own ahead of the sent prompt, whose turn
    /// starts next. A spent limit is noted for the account; the prompt's own turn meets it too
    /// and fails over from there.
    async fn cli_turn_ended(&mut self, end: EventBody) {
        let turn_id = self.cli_turn.take();
        self.inner.turn_ended_on(&self.session.account_id, &end);
        self.void_requests().await;
        self.log(end).await;
        if let Some(turn_id) = turn_id {
            self.record_branches().await;
            self.checkpoint(&turn_id).await;
        }
        self.settle().await;
    }

    /// Journals the end of the running turn and reports it as `summary`, then starts the next
    /// queued prompt or settles on `settled`. A child that completed its turn and is done
    /// ([`Self::done`]) is archived once it reported.
    async fn turn_ended(
        &mut self,
        turn_id: TurnId,
        body: EventBody,
        settled: SessionStatus,
        summary: String,
    ) {
        let completed = matches!(body, EventBody::TurnCompleted { .. });
        self.inner.turn_ended_on(&self.session.account_id, &body);
        self.void_requests().await;
        self.log(body).await;
        self.record_branches().await;
        self.checkpoint(&turn_id).await;
        self.turn = None;
        // The next turn asks again, behind every turn already waiting.
        self.permit = None;
        self.prompt = None;
        if self.queue.is_empty() {
            // Background agents work on past the turn, and the session with them.
            let status = match settled {
                SessionStatus::Idle if self.background > 0 => SessionStatus::Running,
                settled => settled,
            };
            self.set_status(status).await;
        }
        let archive = completed && self.done().await;
        self.report(turn_id, summary, archive).await;
        if self.prompts == Some(titles::REFRESH_AFTER) {
            titles::auto(&self.inner, self.session.session_id.clone());
        }
        self.start_next().await;
    }

    /// Whether the session is a child whose work is done: idle with nothing queued, and every
    /// pull request it has merged, at least one. A child without pull requests stays live until
    /// archived by hand.
    async fn done(&self) -> bool {
        if self.session.parent.is_none()
            || self.session.status != SessionStatus::Idle
            || !self.queue.is_empty()
            || self.setup.is_some()
        {
            return false;
        }
        match self
            .inner
            .journal
            .prs(self.session.session_id.clone())
            .await
        {
            Ok(prs) => !prs.is_empty() && prs.iter().all(|pr| pr.state == PrState::Merged),
            Err(err) => {
                warn!(
                    session_id = %self.session.session_id,
                    "cannot read the child's pull requests: {err:#}"
                );
                false
            }
        }
    }

    /// Whether the session stays on its account when it hits a limit: as it was created, else
    /// as the daemon's default says.
    async fn pinned(&self) -> bool {
        let session_id = self.session.session_id.clone();
        match self.inner.journal.settings(session_id).await {
            Ok(settings) => settings.failover_pin.unwrap_or_else(|| self.inner.pinned()),
            Err(err) => {
                warn!("cannot read the session's failover pin: {err:#}");
                self.inner.pinned()
            }
        }
    }

    /// Restore a durable wall-clock deadline into the runtime's monotonic clock.
    async fn arm_retry(&mut self, at: Timestamp) {
        let delay = at.duration_since(Timestamp::now());
        let delay = Duration::try_from(delay).unwrap_or_default();
        self.retry_deadline = Some(Instant::now() + delay);
        self.log(EventBody::SessionStatusChanged {
            status: SessionStatus::WaitingForCapacity,
            retry_at: Some(at),
        })
        .await;
        self.session.status = SessionStatus::WaitingForCapacity;
    }

    /// A new user action replaces the scheduled retry, preserving other queued prompts.
    async fn cancel_retry(&mut self, settle: bool) {
        if self.retry_deadline.take().is_some() {
            self.queue.pop_front();
            self.save_queue().await;
            if settle {
                self.set_status(SessionStatus::Idle).await;
            } else {
                self.log(EventBody::SessionStatusChanged {
                    status: SessionStatus::WaitingForCapacity,
                    retry_at: None,
                })
                .await;
            }
        }
    }

    async fn limit_reached(&mut self, turn_id: TurnId, error: TurnError) {
        let failing = self.session.account_id.clone();
        self.inner.limit_hit(&failing);
        let prompt = self.prompt.take();
        let target = match prompt {
            Some(ref prompt) if !prompt.retry && !self.pinned().await => self
                .inner
                .available_account(&self.session.provider, Some(&failing)),
            _ => None,
        };
        if target.is_none()
            && let Some(prompt) = prompt.clone()
            && let Some(at) = self.inner.limit_reset(&failing)
        {
            self.void_requests().await;
            self.log(EventBody::TurnFailed {
                turn_id: turn_id.clone(),
                error,
            })
            .await;
            self.record_branches().await;
            self.checkpoint(&turn_id).await;
            self.turn = None;
            self.permit = None;
            self.waiting = None;
            if let Some(adapter) = self.adapter.take() {
                let _ = tokio::time::timeout(EXIT_GRACE, stop(adapter)).await;
            }
            self.background = 0;
            self.queue.push_front(Prompt {
                retry: true,
                retry_at: Some(at),
                ..prompt
            });
            self.save_queue().await;
            // Do not promise an automatic retry unless it was stored.
            if self.saved.first().and_then(|prompt| prompt.retry_at) == Some(at) {
                self.arm_retry(at).await;
            } else {
                self.queue.pop_front();
                self.set_status(SessionStatus::NeedsYou).await;
            }
            return;
        }
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
        // An account switch keeps the provider and the model: the retry starts the next
        // account's CLI on the session's current model.
        if let Err(err) = self.switch(None, account_id.clone(), Switch::Account).await {
            warn!(
                session_id = %self.session.session_id,
                "cannot fail over to {account_id}: {}", err.message
            );
            self.permit = None;
            if self.queue.is_empty() {
                self.set_status(SessionStatus::NeedsYou).await;
            }
            self.report(turn_id, failed(&error), false).await;
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
    /// the transcript. A CLI killed for memory between turns, such as while its background
    /// agents built, fails a turn of its own to say so, or the session would just go quiet.
    async fn exited(&mut self, error: Option<TurnError>) {
        self.adapter = None;
        let background = std::mem::take(&mut self.background) > 0;
        let oom = match &error {
            Some(error) => self.out_of_memory(error).await,
            None => None,
        };
        self.prompt = None;
        self.cli_turn = None;
        let mut open = self.turn.take();
        if open.is_none() && oom.is_some() {
            let turn_id = (self.inner.turn_ids)();
            self.log(EventBody::TurnStarted {
                turn_id: turn_id.clone(),
            })
            .await;
            open = Some(turn_id);
        }
        let error = oom.or(error);
        self.permit = None;
        self.void_requests().await;
        let mut summary = None;
        if let Some(turn_id) = &open {
            let error = error.clone().unwrap_or_else(|| TurnError {
                class: ErrorClass::Transient,
                message: "the agent exited during the turn".to_owned(),
            });
            summary = Some(failed(&error));
            self.log(EventBody::TurnFailed {
                turn_id: turn_id.clone(),
                error,
            })
            .await;
            self.record_branches().await;
            self.checkpoint(turn_id).await;
        }
        if self.queue.is_empty() {
            if error.is_some() {
                self.set_status(SessionStatus::Error).await;
            } else if open.is_some() {
                self.set_status(SessionStatus::NeedsYou).await;
            }
        }
        if background {
            self.settle_background().await;
        }
        if let (Some(turn_id), Some(summary)) = (open, summary) {
            self.report(turn_id, summary, false).await;
        }
        self.start_next().await;
    }

    /// The error to journal instead of `error`, when the CLI failed because the kernel's OOM
    /// killer or systemd-oomd killed in its scope: `error` names neither, as the CLI only saw a
    /// signal.
    async fn out_of_memory(&self, error: &TurnError) -> Option<TurnError> {
        if error.class != ErrorClass::Fatal {
            return None;
        }
        let scopes = self.inner.scopes.get()?;
        let kill = scopes.oom_killed(&self.session.session_id).await?;
        let limit = scopes.limits(self.session.parent.is_some()).memory_max / (1024 * 1024);
        warn!(
            session_id = %self.session.session_id,
            ?kill,
            "the agent CLI failed after an OOM kill in its scope: {}", error.message
        );
        let cause = match kill {
            OomKill::Kernel => format!(
                "the kernel killed it, or a process it ran, at its session's {limit} MiB limit"
            ),
            OomKill::Oomd => "systemd-oomd stopped it and everything it ran, as the host was \
                              short of memory"
                .to_owned(),
        };
        Some(TurnError {
            class: ErrorClass::Fatal,
            message: format!(
                "the agent ran out of memory: {cause} ({}). The next prompt restarts the agent \
                 from the session's transcript",
                error.message
            ),
        })
    }

    /// A child's turn ended: journals `child_reported` in its primary session, archives the
    /// child when `archive`, and hands the report to the primary's `wait_for`. Archived before
    /// `wait_for` hears of it, so the primary finds the child's slot free once it does. Nothing
    /// for a top-level session.
    async fn report(&mut self, turn_id: TurnId, summary: String, archive: bool) {
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
        if archive && let Err(err) = self.archive(None).await {
            warn!(session_id = %child, "cannot archive the finished child: {}", err.message);
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

    /// Journals `prompt` as the user message that starts `turn_id`.
    async fn user_message(&mut self, turn_id: &TurnId, prompt: &Prompt) -> Result<()> {
        let item = Item {
            agent_message: prompt.agent_message.clone(),
            follow_up: prompt.follow_up.clone(),
            parent_call_id: None,
            id: ItemId::new(ulid::Ulid::new().to_string()),
            turn_id: turn_id.clone(),
            body: ItemBody::UserMessage {
                text: prompt.text.clone(),
                attachments: prompt.attachments.clone(),
            },
        };
        self.inner
            .journal
            .record_prompt(
                self.session.session_id.clone(),
                prompt.by.clone(),
                EventBody::ItemAdded { item },
                prompt.prompt_id.clone(),
            )
            .await?;
        if !titles::enabled(&self.inner) {
            return Ok(());
        }
        self.prompts = match self.prompts {
            Some(prompts) => Some(prompts + 1),
            None => self.count_prompts().await,
        };
        if self.prompts == Some(1) {
            titles::auto(&self.inner, self.session.session_id.clone());
        }
        Ok(())
    }

    /// The prompts the journal holds; `None` when it cannot be read.
    async fn count_prompts(&self) -> Option<u64> {
        let session_id = &self.session.session_id;
        match self.inner.journal.all(session_id.clone()).await {
            Ok(events) => {
                let prompts = events.iter().filter(|event| {
                    matches!(
                        &event.body,
                        EventBody::ItemAdded { item }
                            if matches!(item.body, ItemBody::UserMessage { .. })
                    )
                });
                u64::try_from(prompts.count()).ok()
            }
            Err(err) => {
                warn!(%session_id, "cannot count the session's prompts: {err:#}");
                None
            }
        }
    }

    async fn set_status(&mut self, status: SessionStatus) {
        if status != self.session.status {
            self.log(EventBody::SessionStatusChanged {
                status,
                retry_at: None,
            })
            .await;
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

/// The journal event for `event` when it ends the turn `turn_id`.
fn turn_end(event: &AdapterEvent, turn_id: &TurnId) -> Option<EventBody> {
    match event {
        AdapterEvent::TurnCompleted {
            turn_id: ended,
            usage,
        } if ended == turn_id => Some(EventBody::TurnCompleted {
            turn_id: ended.clone(),
            usage: usage.clone(),
        }),
        AdapterEvent::TurnInterrupted { turn_id: ended } if ended == turn_id => {
            Some(EventBody::TurnInterrupted {
                turn_id: ended.clone(),
            })
        }
        AdapterEvent::TurnFailed {
            turn_id: ended,
            error,
        } if ended == turn_id => Some(EventBody::TurnFailed {
            turn_id: ended.clone(),
            error: error.clone(),
        }),
        _ => None,
    }
}

/// Asks the CLI to exit and waits until it has.
async fn stop(mut adapter: AdapterSession) {
    let _ = adapter.commands.send(AdapterCommand::Shutdown);
    while adapter.events.recv().await.is_some() {}
}

/// Closes a turn left open by a daemon that stopped mid-turn, or by a handoff, failing it with
/// `why`, expiring its open approvals, and settles the session's status: `then` once it closed
/// a turn.
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
    why: &str,
    then: SessionStatus,
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
                message: why.to_owned(),
            };
            let summary = failed(&error);
            let body = EventBody::TurnFailed {
                turn_id: turn_id.clone(),
                error,
            };
            journal.record(id.clone(), None, body).await?;
            (then, Some((turn_id, summary)))
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
            .record(
                id.clone(),
                None,
                EventBody::SessionStatusChanged {
                    status,
                    retry_at: None,
                },
            )
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
            EventBody::TurnCompleted { turn_id, .. } if unfinished.as_ref() == Some(turn_id) => {
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

/// A start failure that retrying cannot fix.
fn fatal(message: String) -> TurnError {
    TurnError {
        class: ErrorClass::Fatal,
        message,
    }
}

/// A new id for a queued prompt.
pub(super) fn new_prompt_id() -> PromptId {
    PromptId::new(ulid::Ulid::new().to_string())
}

fn started(prompt_id: &PromptId) -> ErrorInfo {
    error(
        ErrorCode::Conflict,
        format!("prompt {prompt_id} has started and is no longer queued"),
    )
}
