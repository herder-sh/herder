//! Tasks: a primary session runs child sessions through the task tools ([`herder_tasktools`]).
//!
//! A child is a full session on this host, in the primary's repository with its own worktree
//! and branch, created with the primary as its `parent`. It starts on the primary's account and
//! provider, with the primary's model and permission mode unless `spawn` picks others; its
//! permission mode may never exceed the primary's, and a child cannot spawn (a task is one
//! level deep). Every prompt the primary sends is journaled in the child with no `by`, since
//! the agent sent it.
//!
//! When a child's turn ends, however it ended, the child journals `child_reported` in the
//! primary, with its final assistant message of the turn or a short failure status, and the
//! report joins the primary's queue in [`Tasks`]. `wait_for` takes events from that queue
//! oldest first, each once. The queue lives in memory: a daemon restart drops reports no
//! `wait_for` took, which stay in the primary's journal and in `status`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use herder_protocol::{ErrorInfo, EventBody, PermissionMode, SessionId};
use herder_store::Session;
use herder_tasktools::{
    CallToolResult, ChildStatus, ErrorCode, SendInput, SendOutput, SpawnInput, SpawnOutput,
    StatusInput, StatusOutput, ToolCall, ToolError, WaitForInput, WaitForOutput,
};
use serde::Serialize;
use tokio::sync::watch;
use tokio::time::Instant;

use super::{CreateRequest, Inner, SessionManager};
use crate::mcp::{ToolFuture, ToolHandler};

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
    /// from. Requests routed to the primary (P2b.4) join the same queue.
    events: HashMap<SessionId, VecDeque<(SessionId, WaitForOutput)>>,
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
                ToolCall::Status(input) => success(manager.child_status(caller, input).await),
                ToolCall::WaitFor(input) => success(manager.wait_for(caller, input).await),
                call @ (ToolCall::Answer(_) | ToolCall::Escalate(_)) => Err(ToolError::new(
                    ErrorCode::Internal,
                    format!(
                        "not implemented: this herder cannot run `{}` yet; children's \
                         questions and approvals go to the user",
                        call.tool().name()
                    ),
                )),
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
        Command::BadRequest | Command::Conflict | Command::Unsupported => ErrorCode::NotAllowed,
        _ => ErrorCode::Internal,
    };
    ToolError::new(code, error.message)
}

/// Permission modes from least to most allowed.
fn rank(mode: PermissionMode) -> u8 {
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
        // Admission goes here, after every check on the arguments and before anything is
        // created: the task's child limit (P2b.5) and the host's capacity, refused as
        // `host_busy` (P2c.3).
        let model = input.model.unwrap_or_else(|| primary.model.clone());
        let request = CreateRequest {
            repo: primary.repo.clone(),
            branch: None,
            account_id: primary.account_id.clone(),
            model: Some(model).filter(|model| !model.is_empty()),
            permission_mode,
            parent: Some(caller.clone()),
            task: Some(input.task.clone()),
        };
        let (child, branch) = self
            .create_session(None, request)
            .await
            .map_err(tool_error)?;
        let body = EventBody::ChildSpawned {
            child_session_id: child.clone(),
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

    async fn send_child(
        &self,
        caller: SessionId,
        input: SendInput,
    ) -> Result<SendOutput, ToolError> {
        self.child(&caller, &input.child).await?;
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
                task: child.task.unwrap_or_default(),
                child: child.session_id,
                branch: child.branch,
                status: child.status,
                // Children's questions and approvals go to the user until P2b.4.
                open_questions: Vec::new(),
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
