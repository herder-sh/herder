//! Tasks: a primary session runs child sessions through the task tools ([`herder_tasktools`]).
//!
//! A child is a full session on this host, in the primary's repository with its own worktree
//! and branch, created with the primary as its `parent`. It starts on the primary's account,
//! provider and failover pin, with the primary's model and permission mode unless `spawn` picks
//! others; its permission mode may never exceed the primary's, and a child cannot spawn (a task
//! is one level deep). A primary may have any number of children. While the host admits no more turns for want of memory, load or
//! pressure ([`crate::resources::admission`]), `spawn` is refused as `host_busy` with a hint to
//! retry after [`RETRY_AFTER_SECS`]; while only the turn limit binds, the child is created and
//! its first turn waits for a slot. While `wait_for` blocks, the primary's turn lends its slot
//! to other turns, so its children can run. Every prompt the primary sends is journaled in the child with no `by`, since the agent sent it.
//!
//! When a child's turn ends, however it ended, the child journals `child_reported` in the
//! primary, with its final assistant message of the turn or a short failure status, and the
//! report joins the primary's queue in [`Tasks`]. `wait_for` takes events from that queue
//! oldest first, each once. The queue lives in memory: a daemon restart drops reports no
//! `wait_for` took, which stay in the primary's journal and in `status`.
//!
//! A child is done once it is idle with nothing queued and every pull request it has is merged,
//! at least one. A child that completes a turn done is archived right after its report is
//! journaled and before `wait_for` hears of it; an idle child is archived as soon as its last
//! pull request merges ([`crate::prs`]). Its worktree is removed days later, its branch kept. A
//! child without pull requests stays idle until archived by hand. A failed or interrupted turn,
//! and a primary, never archive a session. `send` to an archived child unarchives it before the
//! prompt.
//!
//! A child's question or approval request routed to the primary ([`super::routing`]) joins
//! the same queue, and stays in `status.open_questions` until it is answered, escalated, or
//! its turn ends; a request that leaves the primary before a `wait_for` took it leaves the
//! queue too. `answer` and `escalate` go to the child's actor, which settles them in order
//! with any user's answer.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use herder_protocol::{ErrorInfo, EventBody, PermissionMode, SessionId, SessionStatus};
use herder_store::Session;
use herder_tasktools::{
    AnswerInput, AnswerOutput, CallToolResult, ChildStatus, ErrorCode, EscalateInput,
    EscalateOutput, Request, RequestRef, SendInput, SendOutput, SendSessionInput,
    SendSessionOutput, SpawnInput, SpawnOutput, StatusInput, StatusOutput, ToolCall, ToolError,
    WaitForInput, WaitForOutput,
};
use serde::Serialize;
use tokio::sync::{oneshot, watch};
use tokio::time::Instant;

use super::actor::{self, PrimaryAct};
use super::{CreateRequest, Inner, SessionManager};
use crate::mcp::{ToolFuture, ToolHandler};
use crate::resources::admission::RETRY_AFTER_SECS;

/// What every primary session waits for: its children's reports, and which children are
/// working.
pub(crate) struct Tasks {
    state: Mutex<State>,
    /// Bumped on every change, waking every `wait_for` to look again.
    changed: watch::Sender<()>,
}

#[derive(Default)]
struct State {
    /// Each child with a turn running or a prompt queued, with its primary.
    working: HashMap<SessionId, SessionId>,
    /// Each primary's events no `wait_for` returned yet, oldest first, with the child each is
    /// from: reports, and requests routed to the primary.
    events: HashMap<SessionId, VecDeque<(SessionId, WaitForOutput)>>,
    /// Each child's requests waiting for its primary's answer, oldest first, with the ids the
    /// primary sees.
    open: HashMap<SessionId, Vec<Request>>,
}

impl Default for Tasks {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            changed: watch::Sender::new(()),
        }
    }
}

impl Tasks {
    /// Whether `child` of `primary` has a turn running or a prompt queued.
    pub(super) fn set_working(&self, primary: &SessionId, child: &SessionId, working: bool) {
        let mut state = self.lock();
        let changed = if working {
            state
                .working
                .insert(child.clone(), primary.clone())
                .is_none()
        } else {
            state.working.remove(child).is_some()
        };
        drop(state);
        if changed {
            self.changed.send_modify(|()| {});
        }
    }

    /// `child` finished a turn: `output` waits for `primary`'s `wait_for`, and `working` says
    /// whether the child goes on with a queued prompt.
    pub(super) fn report(
        &self,
        primary: &SessionId,
        child: &SessionId,
        output: WaitForOutput,
        working: bool,
    ) {
        let mut state = self.lock();
        state
            .events
            .entry(primary.clone())
            .or_default()
            .push_back((child.clone(), output));
        if working {
            state.working.insert(child.clone(), primary.clone());
        } else {
            state.working.remove(child);
        }
        drop(state);
        self.changed.send_modify(|()| {});
    }

    /// `request` of `child` now waits for `primary`'s answer: through `wait_for` and `status`.
    pub(super) fn route(&self, primary: &SessionId, child: &SessionId, request: &Request) {
        let request = request.clone();
        let mut state = self.lock();
        state
            .open
            .entry(child.clone())
            .or_default()
            .push(request.clone());
        let output = WaitForOutput::Request {
            child: child.clone(),
            request,
        };
        state
            .events
            .entry(primary.clone())
            .or_default()
            .push_back((child.clone(), output));
        drop(state);
        self.changed.send_modify(|()| {});
    }

    /// `request` of `child` no longer waits for `primary`: it was answered or escalated, or its
    /// turn ended. A `wait_for` that has not returned it yet never will.
    pub(super) fn withdraw(&self, primary: &SessionId, child: &SessionId, request: &RequestRef) {
        let matches = |open: &Request| match (open, request) {
            (Request::Question { question_id, .. }, RequestRef::Question(id)) => question_id == id,
            (Request::Approval { approval_id, .. }, RequestRef::Approval(id)) => approval_id == id,
            _ => false,
        };
        let mut state = self.lock();
        if let Some(open) = state.open.get_mut(child) {
            open.retain(|open| !matches(open));
        }
        if let Some(events) = state.events.get_mut(primary) {
            events.retain(|(_, event)| {
                !matches!(event, WaitForOutput::Request { child: from, request }
                    if from == child && matches(request))
            });
        }
    }

    /// `child`'s requests waiting for its primary's answer, oldest first.
    fn open(&self, child: &SessionId) -> Vec<Request> {
        self.lock().open.get(child).cloned().unwrap_or_default()
    }

    /// The oldest event of `primary`'s, from `child` when given, that no earlier call
    /// returned; waits up to `timeout` for one while an awaited child works.
    async fn wait(
        &self,
        primary: &SessionId,
        child: Option<&SessionId>,
        timeout: Duration,
    ) -> WaitForOutput {
        let deadline = Instant::now() + timeout;
        // Subscribed before looking, so a change after the look wakes the wait.
        let mut changed = self.changed.subscribe();
        loop {
            {
                let mut state = self.lock();
                if let Some(output) = state.take(primary, child) {
                    return output;
                }
                if !state.busy(primary, child) {
                    return WaitForOutput::Idle;
                }
            }
            tokio::select! {
                () = tokio::time::sleep_until(deadline) => return WaitForOutput::Timeout,
                changed = changed.changed() => {
                    // The sender lives as long as `self`.
                    if changed.is_err() {
                        return WaitForOutput::Idle;
                    }
                }
            }
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // The state is valid after any panic: every update is a single insert or remove.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl State {
    fn take(&mut self, primary: &SessionId, child: Option<&SessionId>) -> Option<WaitForOutput> {
        let events = self.events.get_mut(primary)?;
        let at = events
            .iter()
            .position(|(from, _)| child.is_none_or(|child| child == from))?;
        events.remove(at).map(|(_, output)| output)
    }

    fn busy(&self, primary: &SessionId, child: Option<&SessionId>) -> bool {
        match child {
            Some(child) => self.working.contains_key(child),
            None => self.working.values().any(|parent| parent == primary),
        }
    }
}

/// The task tools, run by the session manager.
pub(crate) struct TaskTools {
    /// Weak, since the manager owns the MCP server that owns this.
    pub(super) inner: Weak<Inner>,
}

impl ToolHandler for TaskTools {
    fn call(&self, caller: SessionId, call: ToolCall) -> ToolFuture {
        let inner = self.inner.upgrade();
        Box::pin(async move {
            let Some(inner) = inner else {
                return ToolError::new(ErrorCode::Internal, "the daemon is shutting down").into();
            };
            let manager = SessionManager { inner };
            let result = match call {
                ToolCall::Spawn(input) => success(manager.spawn(caller, input).await),
                ToolCall::Send(input) => success(manager.send_child(caller, input).await),
                ToolCall::SendSession(input) => success(manager.send_session(caller, input).await),
                ToolCall::Status(input) => success(manager.child_status(caller, input).await),
                ToolCall::WaitFor(input) => {
                    // The caller's turn uses no CPU while it waits for its children: its slot
                    // goes to them meanwhile.
                    let parked = manager.inner.admission.get().and_then(|a| a.park(&caller));
                    let output = manager.wait_for(caller, input).await;
                    drop(parked);
                    success(output)
                }
                ToolCall::Answer(input) => success(manager.answer(caller, input).await),
                ToolCall::Escalate(input) => success(manager.escalate(caller, input).await),
            };
            result.unwrap_or_else(CallToolResult::from)
        })
    }
}

fn success<T: Serialize>(output: Result<T, ToolError>) -> Result<CallToolResult, ToolError> {
    CallToolResult::success(&output?).map_err(|err| tool_internal(err.to_string()))
}

fn tool_internal(message: impl Into<String>) -> ToolError {
    ToolError::new(ErrorCode::Internal, message)
}

/// A session command's failure as a tool error.
fn tool_error(error: ErrorInfo) -> ToolError {
    use herder_protocol::ErrorCode as Command;
    let code = match error.code {
        Command::NotFound => ErrorCode::NotFound,
        Command::BadRequest | Command::Conflict | Command::Unsupported | Command::Forbidden => {
            ErrorCode::NotAllowed
        }
        _ => ErrorCode::Internal,
    };
    ToolError::new(code, error.message)
}

/// Permission modes from least to most allowed.
pub(super) fn rank(mode: PermissionMode) -> u8 {
    match mode {
        PermissionMode::ReadOnly => 0,
        PermissionMode::Ask => 1,
        PermissionMode::AutoEdit => 2,
        PermissionMode::FullAccess => 3,
    }
}

impl SessionManager {
    async fn spawn(&self, caller: SessionId, input: SpawnInput) -> Result<SpawnOutput, ToolError> {
        let primary = self.caller(&caller).await?;
        // Refuse before creating a child if automated relay depth is exhausted.
        self.agent_message(&caller, String::new())
            .await
            .map_err(tool_error)?;
        if primary.parent.is_some() {
            return Err(ToolError::new(
                ErrorCode::DepthExceeded,
                "you are a child session, and children cannot spawn children; do the work \
                 yourself or report back to your primary session",
            ));
        }
        if let Some(provider) = &input.provider
            && *provider != primary.provider
        {
            return Err(ToolError::new(
                ErrorCode::NotAllowed,
                format!(
                    "children run on your provider, {}; omit `provider`",
                    primary.provider.as_str()
                ),
            ));
        }
        let permission_mode = input.permission_mode.unwrap_or(primary.permission_mode);
        if rank(permission_mode) > rank(primary.permission_mode) {
            return Err(ToolError::new(
                ErrorCode::NotAllowed,
                format!(
                    "a child's permission mode may not exceed yours ({}); pass {} or lower, or \
                     omit it",
                    mode_name(primary.permission_mode),
                    mode_name(primary.permission_mode)
                ),
            ));
        }
        // Admission, after every check on the arguments and before anything is created: the
        // host's capacity.
        let settings = self
            .inner
            .journal
            .settings(caller.clone())
            .await
            .map_err(|err| tool_internal(format!("{err:#}")))?;
        if let Some(admission) = self.inner.admission.get()
            && let Some(constraint) = admission.host_constraint()
        {
            return Err(ToolError::host_busy(
                RETRY_AFTER_SECS,
                format!(
                    "this machine has no room for another agent now: {}. Keep working or call \
                     wait_for, then retry in {RETRY_AFTER_SECS} seconds",
                    admission.explain(constraint)
                ),
            ));
        }
        let model = input.model.unwrap_or_else(|| primary.model.clone());
        let request = CreateRequest {
            repo: primary.repo.clone(),
            branch: None,
            account_id: primary.account_id.clone(),
            model: Some(model).filter(|model| !model.is_empty()),
            permission_mode,
            parent: Some(caller.clone()),
            task: Some(input.task.clone()),
            // The task fails over, or stays put, as one.
            failover_pin: settings.failover_pin,
        };
        let (child, branch) = self
            .create_session(None, request)
            .await
            .map_err(tool_error)?;
        let body = EventBody::ChildSpawned {
            child_session_id: child.clone(),
            host_id: None,
            task: input.task,
        };
        self.inner
            .journal
            .record(caller, None, body)
            .await
            .map_err(|err| tool_internal(format!("{err:#}")))?;
        self.prompt(&child, input.prompt)
            .await
            .map_err(tool_error)?;
        Ok(SpawnOutput { child, branch })
    }

    /// Authenticate provenance from the session journal, not tool arguments. Provider echoes
    /// have no user author or daemon provenance and cannot reset relay depth.
    pub(super) async fn agent_message(
        &self,
        caller: &SessionId,
        message_id: String,
    ) -> Result<herder_protocol::AgentMessage, ErrorInfo> {
        let events = self
            .inner
            .journal
            .all(caller.clone())
            .await
            .map_err(super::internal)?;
        let source = self
            .inner
            .journal
            .session(caller.clone())
            .await
            .map_err(super::internal)?
            .ok_or_else(|| {
                super::error(
                    herder_protocol::ErrorCode::NotFound,
                    "sender does not exist",
                )
            })?;
        let context = events
            .iter()
            .rev()
            .find_map(|event| {
                if let EventBody::ItemAdded { item } = &event.body
                    && item.parent_call_id.is_none()
                    && matches!(item.body, herder_protocol::ItemBody::UserMessage { .. })
                    && (event.by.is_some() || item.agent_message.is_some())
                {
                    Some(
                        item.agent_message
                            .as_ref()
                            .map(|message| (message.hop_count, message.permission_ceiling)),
                    )
                } else {
                    None
                }
            })
            .flatten();
        let (hops, inherited) = context.unwrap_or((0, source.permission_mode));
        let permission_ceiling = if rank(inherited) < rank(source.permission_mode) {
            inherited
        } else {
            source.permission_mode
        };
        if hops >= 8 {
            return Err(super::error(
                herder_protocol::ErrorCode::Forbidden,
                "agent message relay limit reached; wait for a human prompt",
            ));
        }
        Ok(herder_protocol::AgentMessage {
            sender_session_id: caller.clone(),
            message_id,
            hop_count: hops + 1,
            permission_ceiling,
        })
    }

    async fn send_session(
        &self,
        caller: SessionId,
        input: SendSessionInput,
    ) -> Result<SendSessionOutput, ToolError> {
        if caller == input.session_id {
            return Err(ToolError::new(
                ErrorCode::NotAllowed,
                "cannot send a message to yourself",
            ));
        }
        if input.text.trim().is_empty()
            || input.text.len() > 64 * 1024
            || input.message_id.is_empty()
            || input.message_id.len() > 128
        {
            return Err(ToolError::new(
                ErrorCode::InvalidArguments,
                "text must be nonempty and at most 64 KiB; message_id must be 1–128 bytes",
            ));
        }
        let source = self.caller(&caller).await?;
        if matches!(
            source.status,
            SessionStatus::Archived | SessionStatus::Moved
        ) {
            return Err(ToolError::new(
                ErrorCode::NotAllowed,
                "the sending session is read-only",
            ));
        }
        let message = self
            .agent_message(&caller, input.message_id)
            .await
            .map_err(tool_error)?;
        self.deliver_agent_message(&input.session_id, input.text, message)
            .await
            .map_err(tool_error)
    }

    async fn send_child(
        &self,
        caller: SessionId,
        input: SendInput,
    ) -> Result<SendOutput, ToolError> {
        let child = self.child(&caller, &input.child).await?;
        // A finished child was archived; the follow-up brings it back on its branch.
        if child.status == SessionStatus::Archived {
            self.send(input.child.clone(), None, actor::Request::Unarchive)
                .await
                .map_err(tool_error)?;
        }
        let queued = self
            .prompt(&input.child, input.text)
            .await
            .map_err(tool_error)?;
        Ok(SendOutput { queued })
    }

    async fn child_status(
        &self,
        caller: SessionId,
        input: StatusInput,
    ) -> Result<StatusOutput, ToolError> {
        let journal = &self.inner.journal;
        let mut children = journal
            .children(caller.clone())
            .await
            .map_err(|err| tool_internal(format!("{err:#}")))?;
        if let Some(wanted) = &input.children {
            for id in wanted {
                if !children.iter().any(|child| child.session_id == *id) {
                    self.child(&caller, id).await?;
                }
            }
            children.retain(|child| wanted.contains(&child.session_id));
        }
        let mut reports = HashMap::new();
        for event in journal
            .all(caller)
            .await
            .map_err(|err| tool_internal(format!("{err:#}")))?
        {
            if let EventBody::ChildReported {
                child_session_id,
                summary,
                ..
            } = event.body
            {
                reports.insert(child_session_id, summary);
            }
        }
        let children = children
            .into_iter()
            .map(|child| ChildStatus {
                last_report: reports.remove(&child.session_id),
                open_questions: self.inner.tasks.open(&child.session_id),
                task: child.task.unwrap_or_default(),
                child: child.session_id,
                branch: child.branch,
                status: child.status,
            })
            .collect();
        Ok(StatusOutput { children })
    }

    async fn wait_for(
        &self,
        caller: SessionId,
        input: WaitForInput,
    ) -> Result<WaitForOutput, ToolError> {
        if !(1..=600).contains(&input.timeout_secs) {
            return Err(ToolError::new(
                ErrorCode::InvalidArguments,
                "`timeout_secs` must be from 1 to 600",
            ));
        }
        if let Some(child) = &input.child {
            self.child(&caller, child).await?;
        }
        let timeout = Duration::from_secs(input.timeout_secs.into());
        Ok(self
            .inner
            .tasks
            .wait(&caller, input.child.as_ref(), timeout)
            .await)
    }

    async fn answer(
        &self,
        caller: SessionId,
        input: AnswerInput,
    ) -> Result<AnswerOutput, ToolError> {
        let child = match &input {
            AnswerInput::Question { child, .. } | AnswerInput::Approval { child, .. } => {
                child.clone()
            }
        };
        self.act(caller, child, PrimaryAct::Answer(input)).await?;
        Ok(AnswerOutput {})
    }

    async fn escalate(
        &self,
        caller: SessionId,
        input: EscalateInput,
    ) -> Result<EscalateOutput, ToolError> {
        let EscalateInput {
            child,
            request,
            note,
        } = input;
        let note = note.filter(|note| !note.trim().is_empty());
        self.act(caller, child, PrimaryAct::Escalate { request, note })
            .await?;
        Ok(EscalateOutput {})
    }

    /// Has `child`, when it is one of `caller`'s children, apply `act` from `caller`.
    async fn act(
        &self,
        caller: SessionId,
        child: SessionId,
        act: PrimaryAct,
    ) -> Result<(), ToolError> {
        self.child(&caller, &child).await?;
        let (done, result) = oneshot::channel();
        let request = actor::Request::FromPrimary {
            primary: caller,
            act,
            done,
        };
        self.send(child, None, request).await.map_err(tool_error)?;
        result
            .await
            .map_err(|_| tool_internal("the child session stopped"))?
    }

    /// The calling session; it holds a token, so it exists.
    async fn caller(&self, caller: &SessionId) -> Result<Session, ToolError> {
        self.inner
            .journal
            .session(caller.clone())
            .await
            .map_err(|err| tool_internal(format!("{err:#}")))?
            .ok_or_else(|| tool_internal(format!("the calling session {caller} does not exist")))
    }

    /// `child`, when it is one of `caller`'s children.
    async fn child(&self, caller: &SessionId, child: &SessionId) -> Result<Session, ToolError> {
        let session = self
            .inner
            .journal
            .session(child.clone())
            .await
            .map_err(|err| tool_internal(format!("{err:#}")))?
            .ok_or_else(|| {
                ToolError::new(
                    ErrorCode::NotFound,
                    format!("session {child} does not exist"),
                )
            })?;
        if session.parent.as_ref() != Some(caller) {
            return Err(ToolError::new(
                ErrorCode::NotYourChild,
                format!("session {child} is not one of your children"),
            ));
        }
        Ok(session)
    }
}

fn mode_name(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::ReadOnly => "read_only",
        PermissionMode::Ask => "ask",
        PermissionMode::AutoEdit => "auto_edit",
        PermissionMode::FullAccess => "full_access",
    }
}
