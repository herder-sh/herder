//! One running `codex app-server`: the startup handshake, then the loop that turns commands
//! into requests and server messages into adapter events.

use std::collections::HashMap;
use std::time::Duration;

use herder_protocol::{
    ApprovalDecision, ApprovalId, ErrorClass, Item, ItemBody, ItemId, PermissionMode, Timestamp,
    TurnError, TurnId, UsageWindow,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use super::wire::{self, Incoming, RpcError, ThreadItem, ToolStatus};
use super::{classify, policy, sandbox_policy};
use crate::transport::{Exit, Transport};
use crate::{AdapterCommand, AdapterEvent, AdapterSession, Capabilities, StartRequest};

/// Events buffered before the session waits for the daemon to read.
const EVENT_BUFFER: usize = 64;

/// How long a stopping app-server gets to exit on its own before it is killed.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// The client name Codex sees in `initialize`.
const CLIENT_NAME: &str = "herder";

fn fatal(message: impl Into<String>) -> TurnError {
    TurnError {
        class: ErrorClass::Fatal,
        message: message.into(),
    }
}

/// Runs the handshake over `transport`, then hands the session to its own task.
pub(super) async fn start(
    transport: Transport,
    request: StartRequest,
) -> Result<AdapterSession, TurnError> {
    let Transport {
        stdin,
        mut stdout,
        mut exit,
    } = transport;
    let (event_tx, events) = mpsc::channel(EVENT_BUFFER);
    let mut session = Session {
        stdin: Some(stdin),
        events: event_tx,
        thread_id: String::new(),
        model: request.model.clone(),
        mode: request.permission_mode,
        next_request: 0,
        pending: HashMap::new(),
        turn: None,
        items: HashMap::new(),
        approvals: HashMap::new(),
        next_item: 0,
        next_approval: 0,
    };
    session
        .handshake(&request, &mut stdout, &mut exit)
        .await
        .map_err(|err| match err {
            StartError::Turn(error) => error,
            StartError::Gone(reason) => {
                fatal(format!("codex app-server stopped while starting: {reason}"))
            }
        })?;
    let (commands, command_rx) = mpsc::unbounded_channel();
    tokio::spawn(session.run(command_rx, stdout, exit));
    Ok(AdapterSession {
        capabilities: Capabilities {
            native_model_switch: true,
            native_permission_mode_switch: true,
            reports_usage: true,
        },
        commands,
        events,
    })
}

/// Why starting failed.
enum StartError {
    /// Codex answered, and the answer means the session cannot run.
    Turn(TurnError),
    /// The app-server went away; why, as far as is known.
    Gone(String),
}

/// What an in-flight request was for, so its response can be acted on.
enum Pending {
    /// `turn/start` for the open turn.
    TurnStart,
    /// Anything whose response needs nothing done.
    Ignored,
}

/// The turn the daemon started and Codex has not finished.
struct OpenTurn {
    /// herder's id for it.
    id: TurnId,
    /// Codex's id for it, once `turn/start` answered or `turn/started` arrived.
    codex_id: Option<String>,
    /// Whether `TurnStarted` went out.
    started: bool,
    /// The last non-retried `error` notification, in case the failed turn carries none.
    error: Option<TurnError>,
    /// An interrupt asked for before Codex's id was known.
    interrupt: bool,
}

/// A Codex item herder has emitted something for, keyed by Codex's item id.
enum Tracked {
    /// A streaming message or reasoning summary, with its text so far.
    Streaming {
        id: ItemId,
        reasoning: bool,
        text: String,
    },
    /// A tool call already emitted as a completed `tool_call` item.
    ToolCall { id: ItemId, summary: String },
}

struct Session {
    /// `None` once closed, which asks the app-server to exit.
    stdin: Option<mpsc::Sender<String>>,
    events: mpsc::Sender<AdapterEvent>,
    thread_id: String,
    model: Option<String>,
    mode: PermissionMode,
    next_request: u64,
    pending: HashMap<u64, Pending>,
    turn: Option<OpenTurn>,
    items: HashMap<String, Tracked>,
    /// Open approval requests and the JSON-RPC id to answer each on.
    approvals: HashMap<ApprovalId, Value>,
    next_item: u64,
    next_approval: u64,
}

impl Session {
    // ---- Startup ----

    async fn handshake(
        &mut self,
        request: &StartRequest,
        stdout: &mut mpsc::Receiver<String>,
        exit: &mut oneshot::Receiver<Exit>,
    ) -> Result<(), StartError> {
        let initialize = wire::InitializeParams {
            client_info: wire::ClientInfo {
                name: CLIENT_NAME,
                title: None,
                version: env!("CARGO_PKG_VERSION"),
            },
            capabilities: None,
        };
        self.call(stdout, exit, "initialize", initialize)
            .await?
            .map_err(|err| refused("initialize", &err))?;
        self.send(&wire::Notification {
            method: "initialized",
        })
        .await;

        let account = self
            .call(
                stdout,
                exit,
                "account/read",
                wire::AccountReadParams {
                    refresh_token: false,
                },
            )
            .await?
            .map_err(|err| refused("account/read", &err))?;
        let account: wire::AccountReadResult = decode("account/read", account)?;
        if account.account.is_none() && account.requires_openai_auth {
            return Err(StartError::Turn(TurnError {
                class: ErrorClass::Auth,
                message: "codex is not logged in for this account".into(),
            }));
        }

        // Accounts without ChatGPT plan limits, such as API keys, refuse this; that is fine.
        if let Ok(limits) = self
            .call(stdout, exit, "account/rateLimits/read", ())
            .await?
        {
            let limits: wire::RateLimitsReadResult = decode("account/rateLimits/read", limits)?;
            let windows = match limits.rate_limits_by_limit_id {
                Some(by_id) if !by_id.is_empty() => by_id.values().flat_map(windows).collect(),
                _ => windows(&limits.rate_limits),
            };
            self.usage(windows).await;
        }

        let (approval_policy, sandbox) = policy(self.mode);
        let cwd = request.cwd.to_string_lossy();
        let model = self.model.clone();
        let thread = self
            .call(
                stdout,
                exit,
                "thread/start",
                wire::ThreadStartParams {
                    model: model.as_deref(),
                    cwd: &cwd,
                    approval_policy,
                    sandbox,
                },
            )
            .await?
            .map_err(|err| refused("thread/start", &err))?;
        let thread: wire::ThreadStartResult = decode("thread/start", thread)?;
        self.thread_id = thread.thread.id;
        self.emit(AdapterEvent::ModelChanged {
            model: thread.model,
        })
        .await;

        let items = seed_items(&request.seed);
        if !items.is_empty() {
            let thread_id = self.thread_id.clone();
            let params = wire::ThreadInjectItemsParams {
                thread_id: &thread_id,
                items,
            };
            self.call(stdout, exit, "thread/inject_items", params)
                .await?
                .map_err(|err| refused("thread/inject_items", &err))?;
        }
        Ok(())
    }

    /// Sends a request and handles everything else the app-server says until it answers.
    async fn call(
        &mut self,
        stdout: &mut mpsc::Receiver<String>,
        exit: &mut oneshot::Receiver<Exit>,
        method: &str,
        params: impl Serialize,
    ) -> Result<Result<Value, RpcError>, StartError> {
        let id = self.request(method, params, Pending::Ignored).await;
        self.pending.remove(&id);
        loop {
            let Some(line) = stdout.recv().await else {
                return Err(StartError::Gone(gone(exit).await));
            };
            let Ok(message) = serde_json::from_str::<Incoming>(&line) else {
                continue;
            };
            if message.method.is_none() && message.id == Some(Value::from(id)) {
                return Ok(match message.error {
                    Some(error) => Err(error),
                    None => Ok(message.result.unwrap_or(Value::Null)),
                });
            }
            self.handle(message).await;
        }
    }

    // ---- Running ----

    async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<AdapterCommand>,
        mut stdout: mpsc::Receiver<String>,
        mut exit: oneshot::Receiver<Exit>,
    ) {
        let asked_to_stop = loop {
            tokio::select! {
                command = commands.recv() => match command {
                    None | Some(AdapterCommand::Shutdown) => break true,
                    Some(command) => self.command(command).await,
                },
                line = stdout.recv() => match line {
                    Some(line) => self.line(&line).await,
                    None => break false,
                },
            }
        };
        if asked_to_stop {
            // Closing stdin is how an app-server is asked to exit.
            self.stdin = None;
            let _ = tokio::time::timeout(STOP_TIMEOUT, async {
                while stdout.recv().await.is_some() {}
            })
            .await;
            // Exited or killed when `exit` drops: either way it is gone. Only a broken
            // transport, such as a replay that did not match, is worth reporting.
            let error = match tokio::time::timeout(STOP_TIMEOUT, &mut exit).await {
                Ok(Ok(Exit::Failed(message))) => Some(fatal(message)),
                _ => None,
            };
            self.fail_open_turn(fatal("codex app-server stopped")).await;
            self.emit(AdapterEvent::Exited { error }).await;
        } else {
            let error = fatal(gone(&mut exit).await);
            self.fail_open_turn(error.clone()).await;
            self.emit(AdapterEvent::Exited { error: Some(error) }).await;
        }
    }

    async fn command(&mut self, command: AdapterCommand) {
        match command {
            AdapterCommand::SendPrompt { turn_id, text } => {
                self.turn = Some(OpenTurn {
                    id: turn_id,
                    codex_id: None,
                    started: false,
                    error: None,
                    interrupt: false,
                });
                let (approval_policy, sandbox) = policy(self.mode);
                let thread_id = self.thread_id.clone();
                let model = self.model.clone();
                let params = wire::TurnStartParams {
                    thread_id: &thread_id,
                    input: [wire::UserInput::Text {
                        text: &text,
                        text_elements: [],
                    }],
                    model: model.as_deref(),
                    approval_policy,
                    sandbox_policy: sandbox_policy(sandbox),
                };
                self.request("turn/start", params, Pending::TurnStart).await;
            }
            AdapterCommand::Interrupt => {
                let codex_id = match &mut self.turn {
                    Some(turn) if turn.codex_id.is_none() => {
                        turn.interrupt = true;
                        None
                    }
                    Some(turn) => turn.codex_id.clone(),
                    None => None,
                };
                if let Some(codex_id) = codex_id {
                    self.interrupt(&codex_id).await;
                }
            }
            AdapterCommand::SetModel { model } => {
                self.model = Some(model.clone());
                self.emit(AdapterEvent::ModelChanged { model }).await;
            }
            AdapterCommand::SetPermissionMode { mode } => {
                self.mode = mode;
                self.emit(AdapterEvent::PermissionModeChanged { mode })
                    .await;
            }
            AdapterCommand::AnswerApproval {
                approval_id,
                decision,
            } => {
                if let Some(id) = self.approvals.remove(&approval_id) {
                    let decision = match decision {
                        ApprovalDecision::Allow => wire::ApprovalAnswer::Accept,
                        ApprovalDecision::Deny => wire::ApprovalAnswer::Decline,
                    };
                    self.send(&wire::Response {
                        id,
                        result: wire::ApprovalResult { decision },
                    })
                    .await;
                }
            }
            // This adapter never sends `QuestionAsked`, so there is nothing to answer.
            AdapterCommand::AnswerQuestion { .. } => {}
            // Handled by `run`, which owns stopping.
            AdapterCommand::Shutdown => {}
        }
    }

    async fn interrupt(&mut self, codex_id: &str) {
        let thread_id = self.thread_id.clone();
        let params = wire::TurnInterruptParams {
            thread_id: &thread_id,
            turn_id: codex_id,
        };
        self.request("turn/interrupt", params, Pending::Ignored)
            .await;
    }

    async fn line(&mut self, line: &str) {
        // The app-server writes only JSON-RPC on stdout; anything else is not for herder.
        if let Ok(message) = serde_json::from_str::<Incoming>(line) {
            self.handle(message).await;
        }
    }

    async fn handle(&mut self, message: Incoming) {
        match (message.id, message.method) {
            (Some(id), Some(method)) => self.server_request(id, &method, message.params).await,
            (None, Some(method)) => self.notification(&method, message.params).await,
            (Some(id), None) => self.response(&id, message.result, message.error).await,
            (None, None) => {}
        }
    }

    async fn response(&mut self, id: &Value, result: Option<Value>, error: Option<RpcError>) {
        let Some(pending) = id.as_u64().and_then(|id| self.pending.remove(&id)) else {
            return;
        };
        match pending {
            Pending::TurnStart => match (error, result) {
                (Some(error), _) => {
                    self.end_turn(TurnEnd::Failed(fatal(error.message))).await;
                }
                (None, result) => {
                    let started = result.and_then(|result| {
                        serde_json::from_value::<wire::TurnStartResult>(result).ok()
                    });
                    if let Some(started) = started {
                        self.turn_known(started.turn.id).await;
                    }
                }
            },
            Pending::Ignored => {}
        }
    }

    async fn notification(&mut self, method: &str, params: Value) {
        if !self.is_ours(&params) {
            return;
        }
        match method {
            "turn/started" => {
                if let Some(notification) = parse::<wire::TurnNotification>(params) {
                    self.turn_known(notification.turn.id).await;
                    self.ensure_started().await;
                }
            }
            "turn/completed" => {
                if let Some(notification) = parse::<wire::TurnNotification>(params) {
                    self.turn_completed(notification.turn).await;
                }
            }
            "error" => {
                if let Some(notification) = parse::<wire::ErrorNotification>(params)
                    && !notification.will_retry
                    && let Some(turn) = &mut self.turn
                {
                    turn.error = Some(classify(&notification.error));
                }
            }
            "item/started" => {
                if let Some(notification) = parse::<wire::ItemNotification>(params) {
                    self.item_started(notification.item).await;
                }
            }
            "item/completed" => {
                if let Some(notification) = parse::<wire::ItemNotification>(params) {
                    self.item_completed(notification.item).await;
                }
            }
            "item/agentMessage/delta" | "item/reasoning/summaryTextDelta" => {
                if let Some(delta) = parse::<wire::DeltaNotification>(params) {
                    self.delta(delta).await;
                }
            }
            "account/rateLimits/updated" => {
                if let Some(update) = parse::<wire::RateLimitsUpdated>(params) {
                    self.usage(windows(&update.rate_limits)).await;
                }
            }
            "model/rerouted" => {
                if let Some(rerouted) = parse::<wire::ModelRerouted>(params) {
                    self.emit(AdapterEvent::ModelChanged {
                        model: rerouted.to_model,
                    })
                    .await;
                }
            }
            "serverRequest/resolved" => {
                if let Some(resolved) = parse::<wire::ServerRequestResolved>(params) {
                    self.approvals.retain(|_, id| *id != resolved.request_id);
                }
            }
            _ => {}
        }
    }

    async fn server_request(&mut self, id: Value, method: &str, params: Value) {
        let approval = match method {
            "item/commandExecution/requestApproval" => {
                parse::<wire::CommandApproval>(params).map(|approval| {
                    let command = approval.command.unwrap_or_default();
                    let call =
                        tool_call_body("shell", json!({"command": command, "cwd": approval.cwd}));
                    let summary = with_reason(format!("Run {command}"), approval.reason);
                    (approval.item_id, call, summary)
                })
            }
            "item/fileChange/requestApproval" => {
                parse::<wire::FileChangeApproval>(params).map(|approval| {
                    let call = tool_call_body("apply_patch", json!({}));
                    let summary = with_reason("Edit files".to_owned(), approval.reason);
                    (approval.item_id, call, summary)
                })
            }
            _ => None,
        };
        let Some((item_id, call, fallback_summary)) = approval else {
            let error = RpcError {
                code: wire::METHOD_NOT_FOUND,
                message: format!("herder does not handle {method}"),
            };
            self.send(&wire::ErrorResponse { id, error }).await;
            return;
        };
        let Some(turn_id) = self.turn.as_ref().map(|turn| turn.id.clone()) else {
            // No turn is waiting on it; refusing is the only answer that cannot do harm.
            let result = wire::ApprovalResult {
                decision: wire::ApprovalAnswer::Decline,
            };
            self.send(&wire::Response { id, result }).await;
            return;
        };
        self.ensure_started().await;
        let (tool_call_id, summary) = match self.items.get(&item_id) {
            Some(Tracked::ToolCall { id, summary }) => (id.clone(), summary.clone()),
            _ => {
                let call_id = self
                    .tool_call(item_id, call, fallback_summary.clone())
                    .await;
                (call_id, fallback_summary)
            }
        };
        self.next_approval += 1;
        let approval_id = ApprovalId::new(format!("approval-{}", self.next_approval));
        self.approvals.insert(approval_id.clone(), id);
        self.emit(AdapterEvent::ApprovalRequested {
            approval_id,
            turn_id,
            tool_call_id,
            summary,
        })
        .await;
    }

    // ---- Turns ----

    /// Records Codex's id for the open turn, sending an interrupt that was waiting for it.
    async fn turn_known(&mut self, codex_id: String) {
        let Some(turn) = &mut self.turn else { return };
        if turn.codex_id.is_some() {
            return;
        }
        turn.codex_id = Some(codex_id.clone());
        if turn.interrupt {
            self.interrupt(&codex_id).await;
        }
    }

    /// Sends `TurnStarted` for the open turn unless it went out already.
    async fn ensure_started(&mut self) {
        let Some(turn) = &mut self.turn else { return };
        if turn.started {
            return;
        }
        turn.started = true;
        let turn_id = turn.id.clone();
        self.emit(AdapterEvent::TurnStarted { turn_id }).await;
    }

    async fn turn_completed(&mut self, turn: wire::Turn) {
        let end = match turn.status {
            wire::TurnStatus::Completed | wire::TurnStatus::InProgress => TurnEnd::Completed,
            wire::TurnStatus::Interrupted => TurnEnd::Interrupted,
            wire::TurnStatus::Failed => {
                let reported = self.turn.as_mut().and_then(|open| open.error.take());
                TurnEnd::Failed(
                    turn.error
                        .as_ref()
                        .map(classify)
                        .or(reported)
                        .unwrap_or_else(|| fatal("codex turn failed")),
                )
            }
        };
        self.end_turn(end).await;
    }

    async fn fail_open_turn(&mut self, error: TurnError) {
        if self.turn.is_some() {
            self.end_turn(TurnEnd::Failed(error)).await;
        }
    }

    /// Closes the open turn: completes what is still streaming, voids open approvals, and
    /// sends the turn's last event.
    async fn end_turn(&mut self, end: TurnEnd) {
        self.ensure_started().await;
        let Some(turn) = self.turn.take() else { return };
        let mut streaming: Vec<(ItemId, bool, String)> = self
            .items
            .drain()
            .filter_map(|(_, tracked)| match tracked {
                Tracked::Streaming {
                    id,
                    reasoning,
                    text,
                } => Some((id, reasoning, text)),
                Tracked::ToolCall { .. } => None,
            })
            .collect();
        streaming.sort_by(|a, b| a.0.cmp(&b.0));
        for (id, reasoning, text) in streaming {
            let body = streamed_body(reasoning, text);
            self.emit_item(id, turn.id.clone(), body).await;
        }
        self.approvals.clear();
        let turn_id = turn.id;
        self.emit(match end {
            TurnEnd::Completed => AdapterEvent::TurnCompleted { turn_id },
            TurnEnd::Interrupted => AdapterEvent::TurnInterrupted { turn_id },
            TurnEnd::Failed(error) => AdapterEvent::TurnFailed { turn_id, error },
        })
        .await;
    }

    // ---- Items ----

    async fn item_started(&mut self, item: ThreadItem) {
        let Some(turn_id) = self.turn.as_ref().map(|turn| turn.id.clone()) else {
            return;
        };
        self.ensure_started().await;
        match item {
            ThreadItem::AgentMessage { id, text } => self.stream(id, turn_id, false, text).await,
            ThreadItem::Reasoning {
                id,
                summary,
                content,
            } => {
                let text = reasoning_text(summary, content);
                self.stream(id, turn_id, true, text).await;
            }
            item => {
                if let Some((codex_id, call, summary)) = tool_call(&item) {
                    self.tool_call(codex_id, call, summary).await;
                }
            }
        }
    }

    async fn item_completed(&mut self, item: ThreadItem) {
        let Some(turn_id) = self.turn.as_ref().map(|turn| turn.id.clone()) else {
            return;
        };
        self.ensure_started().await;
        let (codex_id, body) = match item {
            ThreadItem::AgentMessage { id, text } => (id, ItemBody::AssistantMessage { text }),
            ThreadItem::Reasoning {
                id,
                summary,
                content,
            } => {
                let text = reasoning_text(summary, content);
                (id, ItemBody::Reasoning { text })
            }
            item => {
                let Some((codex_id, call, summary)) = tool_call(&item) else {
                    return;
                };
                let call_id = match self.items.remove(&codex_id) {
                    Some(Tracked::ToolCall { id, .. }) => id,
                    _ => self.tool_call(codex_id.clone(), call, summary).await,
                };
                self.items.remove(&codex_id);
                let (output, is_error) = tool_result(&item);
                let id = self.mint_item();
                self.emit_item(
                    id,
                    turn_id,
                    ItemBody::ToolResult {
                        call_id,
                        output,
                        is_error,
                    },
                )
                .await;
                return;
            }
        };
        let id = match self.items.remove(&codex_id) {
            Some(Tracked::Streaming { id, .. }) => id,
            _ => self.mint_item(),
        };
        self.emit_item(id, turn_id, body).await;
    }

    async fn stream(&mut self, codex_id: String, turn_id: TurnId, reasoning: bool, text: String) {
        let id = self.mint_item();
        self.items.insert(
            codex_id,
            Tracked::Streaming {
                id: id.clone(),
                reasoning,
                text: text.clone(),
            },
        );
        let item = Item {
            id,
            turn_id,
            body: streamed_body(reasoning, text),
        };
        self.emit(AdapterEvent::ItemStarted { item }).await;
    }

    async fn delta(&mut self, delta: wire::DeltaNotification) {
        let Some(Tracked::Streaming { id, text, .. }) = self.items.get_mut(&delta.item_id) else {
            return;
        };
        text.push_str(&delta.delta);
        let item_id = id.clone();
        self.emit(AdapterEvent::ItemDelta {
            item_id,
            text: delta.delta,
        })
        .await;
    }

    /// Emits a completed `tool_call` item for Codex item `codex_id` and tracks it.
    async fn tool_call(&mut self, codex_id: String, body: ItemBody, summary: String) -> ItemId {
        let id = self.mint_item();
        self.items.insert(
            codex_id,
            Tracked::ToolCall {
                id: id.clone(),
                summary,
            },
        );
        if let Some(turn_id) = self.turn.as_ref().map(|turn| turn.id.clone()) {
            self.emit_item(id.clone(), turn_id, body).await;
        }
        id
    }

    fn mint_item(&mut self) -> ItemId {
        self.next_item += 1;
        ItemId::new(format!("item-{}", self.next_item))
    }

    async fn emit_item(&mut self, id: ItemId, turn_id: TurnId, body: ItemBody) {
        let item = Item { id, turn_id, body };
        self.emit(AdapterEvent::ItemCompleted { item }).await;
    }

    // ---- Plumbing ----

    /// Whether a notification is about this session's thread; those that name none are.
    fn is_ours(&self, params: &Value) -> bool {
        match params.get("threadId").and_then(Value::as_str) {
            Some(thread_id) => thread_id == self.thread_id,
            None => true,
        }
    }

    async fn usage(&mut self, windows: Vec<UsageWindow>) {
        if !windows.is_empty() {
            self.emit(AdapterEvent::UsageReported { windows }).await;
        }
    }

    async fn request(&mut self, method: &str, params: impl Serialize, pending: Pending) -> u64 {
        let id = self.next_request;
        self.next_request += 1;
        self.pending.insert(id, pending);
        self.send(&wire::Request { id, method, params }).await;
        id
    }

    async fn send(&mut self, message: &impl Serialize) {
        // Serializing these plain structs into JSON cannot fail.
        let Ok(line) = serde_json::to_string(message) else {
            return;
        };
        if let Some(stdin) = &self.stdin {
            // A closed stdin means the app-server is gone; its end of output says so.
            let _ = stdin.send(line).await;
        }
    }

    async fn emit(&mut self, event: AdapterEvent) {
        // A daemon that stopped reading has stopped caring; it drops the commands next.
        let _ = self.events.send(event).await;
    }
}

/// How a turn ended.
enum TurnEnd {
    Completed,
    Interrupted,
    Failed(TurnError),
}

fn refused(method: &str, error: &RpcError) -> StartError {
    StartError::Turn(fatal(format!("codex {method}: {}", error.message)))
}

fn decode<T: DeserializeOwned>(method: &str, value: Value) -> Result<T, StartError> {
    serde_json::from_value(value).map_err(|err| {
        StartError::Turn(fatal(format!("codex {method}: unexpected response: {err}")))
    })
}

fn parse<T: DeserializeOwned>(params: Value) -> Option<T> {
    serde_json::from_value(params).ok()
}

/// Why the app-server is gone, from how it exited.
async fn gone(exit: &mut oneshot::Receiver<Exit>) -> String {
    match tokio::time::timeout(STOP_TIMEOUT, exit).await {
        Ok(Ok(Exit::Code(Some(code)))) => format!("codex app-server exited with code {code}"),
        Ok(Ok(Exit::Code(None))) => "codex app-server was killed by a signal".into(),
        Ok(Ok(Exit::Failed(message))) => message,
        Ok(Err(_)) | Err(_) => "codex app-server closed its output".into(),
    }
}

fn streamed_body(reasoning: bool, text: String) -> ItemBody {
    if reasoning {
        ItemBody::Reasoning { text }
    } else {
        ItemBody::AssistantMessage { text }
    }
}

/// A reasoning item's visible text: its summary, or the raw content when there is none.
fn reasoning_text(summary: Vec<String>, content: Vec<String>) -> String {
    if summary.is_empty() {
        content.join("\n\n")
    } else {
        summary.join("\n\n")
    }
}

fn tool_call_body(name: &str, input: Value) -> ItemBody {
    ItemBody::ToolCall {
        name: name.to_owned(),
        input,
    }
}

fn with_reason(summary: String, reason: Option<String>) -> String {
    match reason {
        Some(reason) if !reason.is_empty() => format!("{summary}: {reason}"),
        _ => summary,
    }
}

/// The `tool_call` body and approval summary of a tool item; `None` for other items.
fn tool_call(item: &ThreadItem) -> Option<(String, ItemBody, String)> {
    match item {
        ThreadItem::CommandExecution {
            id, command, cwd, ..
        } => Some((
            id.clone(),
            tool_call_body("shell", json!({"command": command, "cwd": cwd})),
            format!("Run {command}"),
        )),
        ThreadItem::FileChange { id, changes, .. } => {
            let paths: Vec<&str> = changes.iter().map(|change| change.path.as_str()).collect();
            Some((
                id.clone(),
                tool_call_body("apply_patch", json!({"changes": changes})),
                format!("Edit {}", paths.join(", ")),
            ))
        }
        ThreadItem::McpToolCall {
            id,
            server,
            tool,
            arguments,
            ..
        } => Some((
            id.clone(),
            tool_call_body(&format!("{server}.{tool}"), arguments.clone()),
            format!("Call {server}.{tool}"),
        )),
        _ => None,
    }
}

/// A finished tool item's output and whether it failed.
fn tool_result(item: &ThreadItem) -> (String, bool) {
    match item {
        ThreadItem::CommandExecution {
            status,
            aggregated_output,
            exit_code,
            ..
        } => (
            aggregated_output.clone().unwrap_or_default(),
            *status != ToolStatus::Completed || exit_code.is_some_and(|code| code != 0),
        ),
        ThreadItem::FileChange { status, .. } => {
            let output = match status {
                ToolStatus::Completed => "applied",
                ToolStatus::Failed => "failed",
                ToolStatus::Declined => "declined",
                ToolStatus::InProgress => "",
            };
            (output.to_owned(), *status != ToolStatus::Completed)
        }
        ThreadItem::McpToolCall {
            status,
            result,
            error,
            ..
        } => match error {
            Some(error) => (error.message.clone(), true),
            None => (
                result.as_ref().map(Value::to_string).unwrap_or_default(),
                *status != ToolStatus::Completed,
            ),
        },
        _ => (String::new(), false),
    }
}

/// The limit windows of one snapshot, named by their length.
fn windows(snapshot: &wire::RateLimitSnapshot) -> Vec<UsageWindow> {
    [
        ("primary", &snapshot.primary),
        ("secondary", &snapshot.secondary),
    ]
    .into_iter()
    .filter_map(|(slot, window)| {
        let window = window.as_ref()?;
        let length = match window.window_duration_mins {
            Some(300) => "five_hour".to_owned(),
            Some(1440) => "daily".to_owned(),
            Some(10080) => "weekly".to_owned(),
            Some(minutes) => format!("{minutes}_minute"),
            None => slot.to_owned(),
        };
        let name = match snapshot.limit_id.as_deref() {
            Some(limit) if limit != "codex" => format!("{limit}.{length}"),
            _ => length,
        };
        Some(UsageWindow {
            window: name,
            used_percent: window.used_percent,
            resets_at: window
                .resets_at
                .and_then(|seconds| Timestamp::from_second(seconds).ok()),
        })
    })
    .collect()
}

/// The seed transcript as Responses API items for `thread/inject_items`. Tool calls and
/// results become assistant text: their Codex call ids are gone, and the model only needs to
/// know what happened.
fn seed_items(seed: &[Item]) -> Vec<Value> {
    let message = |role: &str, kind: &str, text: &str| json!({"type": "message", "role": role, "content": [{"type": kind, "text": text}]});
    seed.iter()
        .filter_map(|item| match &item.body {
            ItemBody::UserMessage { text } => Some(message("user", "input_text", text)),
            ItemBody::AssistantMessage { text } => Some(message("assistant", "output_text", text)),
            ItemBody::ToolCall { name, input } => Some(message(
                "assistant",
                "output_text",
                &format!("[called tool {name} with {input}]"),
            )),
            ItemBody::ToolResult {
                output, is_error, ..
            } => {
                let label = if *is_error {
                    "tool failed"
                } else {
                    "tool result"
                };
                Some(message(
                    "assistant",
                    "output_text",
                    &format!("[{label}: {output}]"),
                ))
            }
            ItemBody::Reasoning { .. } | ItemBody::Unknown => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_are_named_by_length() {
        let snapshot: wire::RateLimitSnapshot = serde_json::from_value(json!({
            "limitId": "codex",
            "primary": {"usedPercent": 25, "windowDurationMins": 300, "resetsAt": 1791052121},
            "secondary": {"usedPercent": 3.5, "windowDurationMins": 10080, "resetsAt": null}
        }))
        .unwrap();
        assert_eq!(
            windows(&snapshot),
            [
                UsageWindow {
                    window: "five_hour".into(),
                    used_percent: 25.0,
                    resets_at: Some(Timestamp::from_second(1791052121).unwrap()),
                },
                UsageWindow {
                    window: "weekly".into(),
                    used_percent: 3.5,
                    resets_at: None,
                },
            ]
        );
        let other: wire::RateLimitSnapshot = serde_json::from_value(json!({
            "limitId": "gpt-6-astra",
            "primary": {"usedPercent": 1, "windowDurationMins": 90},
            "secondary": null
        }))
        .unwrap();
        assert_eq!(windows(&other)[0].window, "gpt-6-astra.90_minute");
    }

    #[test]
    fn seed_becomes_messages() {
        let turn = TurnId::new("turn-1");
        let item = |body| Item {
            id: ItemId::new("i"),
            turn_id: turn.clone(),
            body,
        };
        let seed = [
            item(ItemBody::UserMessage { text: "hi".into() }),
            item(ItemBody::Reasoning { text: "hmm".into() }),
            item(ItemBody::AssistantMessage {
                text: "hello".into(),
            }),
            item(ItemBody::ToolCall {
                name: "shell".into(),
                input: json!({"command": "ls"}),
            }),
            item(ItemBody::ToolResult {
                call_id: ItemId::new("i"),
                output: "a.txt".into(),
                is_error: false,
            }),
        ];
        assert_eq!(
            seed_items(&seed),
            [
                json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}),
                json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "hello"}]}),
                json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "[called tool shell with {\"command\":\"ls\"}]"}]}),
                json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "[tool result: a.txt]"}]}),
            ]
        );
    }
}
