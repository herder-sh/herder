//! Starting an ACP session and running it: commands in, `session/update`s out as events.

use std::collections::HashMap;
use std::time::Duration;

use herder_protocol::{
    ApprovalDecision, ApprovalId, ErrorClass, Item, ItemBody, ItemId, PermissionMode, TurnError,
    TurnId,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::mpsc;

use super::classify::classify;
use super::profile::AgentProfile;
use super::rpc::{Incoming, Rpc};
use super::schema::{
    self, ConfigOption, ConfigOptionsResponse, ContentBlock, Error, InitializeResponse,
    NewSessionResponse, Outcome, PermissionOption, PermissionOptionKind, PromptResponse,
    RequestPermissionRequest, SessionNotification, SessionUpdate, StopReason, ToolCallContent,
    ToolCallFields, ToolCallStatus, ToolKind,
};
use crate::transport::{Exit, Transport};
use crate::{AdapterCommand, AdapterEvent, AdapterSession, Capabilities, StartRequest};

/// Events buffered before the session waits for the daemon to read.
const EVENT_BUFFER: usize = 64;

/// How long `initialize` and `session/new` may take before the start is given up.
const START_TIMEOUT: Duration = Duration::from_secs(120);

/// JSON-RPC "method not found", for agent requests herder does not serve.
const METHOD_NOT_FOUND: i32 = -32601;

/// JSON-RPC "invalid params".
const INVALID_PARAMS: i32 = -32602;

fn error(class: ErrorClass, message: impl Into<String>) -> TurnError {
    TurnError {
        class,
        message: message.into(),
    }
}

/// An agent's JSON-RPC error as a turn error, classified.
fn agent_error(method: &str, err: &Error) -> TurnError {
    let data = err.data.as_ref().map(Value::to_string).unwrap_or_default();
    let class = classify(err.code, &format!("{} {data}", err.message));
    error(class, format!("{method}: {}", err.message))
}

/// Why the agent went away without being asked.
fn exit_error(exit: Exit) -> TurnError {
    match exit {
        Exit::Code(Some(code)) => error(
            ErrorClass::Transient,
            format!("the agent exited unexpectedly with code {code}"),
        ),
        Exit::Code(None) => error(ErrorClass::Transient, "the agent was killed by a signal"),
        Exit::Failed(message) => error(ErrorClass::Fatal, message),
    }
}

/// Sends a request and waits for its answer, declining agent requests in the meantime.
async fn call<T: DeserializeOwned>(
    rpc: &mut Rpc,
    method: &str,
    params: Value,
) -> Result<T, TurnError> {
    let id = rpc.request(method, params).await;
    loop {
        match rpc.recv().await {
            Some(Incoming::Response {
                id: answered,
                outcome,
            }) if answered == id => {
                let result = outcome.map_err(|err| agent_error(method, &err))?;
                return serde_json::from_value(result).map_err(|err| {
                    error(
                        ErrorClass::Fatal,
                        format!("{method}: unexpected response: {err}"),
                    )
                });
            }
            Some(Incoming::Request { id, method, .. }) => decline(rpc, id, &method).await,
            Some(_) => {}
            None => return Err(exit_error(rpc.exit().await)),
        }
    }
}

async fn decline(rpc: &mut Rpc, id: Value, method: &str) {
    let error = Error {
        code: METHOD_NOT_FOUND,
        message: format!("herder does not serve {method}"),
        data: None,
    };
    rpc.respond_error(id, error).await;
}

/// The agent's model selector: its id and current value.
fn model_option(options: &[ConfigOption]) -> Option<(String, String)> {
    options.iter().find_map(|option| {
        let is_model = option.category.as_deref() == Some("model") || option.id == "model";
        match (&option.current_value, option.kind.as_deref()) {
            (Value::String(current), Some("select")) if is_model => {
                Some((option.id.clone(), current.clone()))
            }
            _ => None,
        }
    })
}

/// Runs `initialize` and `session/new`, sets the starting model, and spawns the session.
pub(super) async fn start(
    profile: &AgentProfile,
    request: StartRequest,
    transport: Transport,
) -> Result<AdapterSession, TurnError> {
    let mut rpc = Rpc::new(transport);
    let setup = async {
        let init: InitializeResponse = call(&mut rpc, "initialize", schema::initialize()).await?;
        if init.protocol_version != schema::PROTOCOL_VERSION {
            return Err(error(
                ErrorClass::Fatal,
                format!(
                    "the agent speaks ACP {}, herder speaks {}",
                    init.protocol_version,
                    schema::PROTOCOL_VERSION
                ),
            ));
        }
        let new: NewSessionResponse =
            call(&mut rpc, "session/new", schema::new_session(&request.cwd)).await?;
        let mut option = model_option(&new.config_options);
        let mut model = option.as_ref().map(|(_, current)| current.clone());
        if let Some(wanted) = &request.model {
            if profile.passes_model(&request) {
                model = Some(wanted.clone());
            } else if let Some((config_id, current)) = &mut option {
                if current != wanted {
                    let set = schema::set_config_option(&new.session_id, config_id, wanted);
                    let set: ConfigOptionsResponse =
                        call(&mut rpc, "session/set_config_option", set).await?;
                    *current = model_option(&set.config_options)
                        .map_or_else(|| wanted.clone(), |(_, current)| current);
                }
                model = Some(current.clone());
            } else {
                return Err(error(
                    ErrorClass::Fatal,
                    format!("{} offers no way to choose the model", profile.program),
                ));
            }
        }
        Ok((
            new.session_id,
            option.map(|(config_id, _)| config_id),
            model,
        ))
    };
    let (session_id, model_config, model) = tokio::time::timeout(START_TIMEOUT, setup)
        .await
        .map_err(|_| error(ErrorClass::Transient, "the agent did not start in time"))??;

    let capabilities = Capabilities {
        native_model_switch: model_config.is_some(),
        native_permission_mode_switch: true,
        reports_usage: false,
    };
    let (commands, command_rx) = mpsc::unbounded_channel();
    let (event_tx, events) = mpsc::channel(EVENT_BUFFER);
    if let Some(model) = &model {
        // The receiver is still in hand and the buffer is empty, so this cannot fail.
        let _ = event_tx.try_send(AdapterEvent::ModelChanged {
            model: model.clone(),
        });
    }
    let session = Session {
        rpc,
        events: event_tx,
        session_id,
        mode: request.permission_mode,
        model_config,
        model,
        seed: render_seed(&request.seed),
        turn: None,
        approvals: HashMap::new(),
        model_requests: HashMap::new(),
        next_item: 0,
        next_approval: 0,
    };
    tokio::spawn(session.run(command_rx));
    Ok(AdapterSession {
        capabilities,
        commands,
        events,
    })
}

/// The seed transcript as text to put in front of the first prompt, if there is one.
fn render_seed(seed: &[Item]) -> Option<String> {
    let entries: Vec<String> = seed
        .iter()
        .filter_map(|item| match &item.body {
            ItemBody::UserMessage { text } => Some(format!("[user]\n{text}")),
            ItemBody::AssistantMessage { text } => Some(format!("[assistant]\n{text}")),
            ItemBody::ToolCall { name, input } => Some(format!("[tool call: {name}]\n{input}")),
            ItemBody::ToolResult { output, .. } => Some(format!("[tool result]\n{output}")),
            ItemBody::Reasoning { .. } | ItemBody::Unknown => None,
        })
        .collect();
    (!entries.is_empty()).then(|| {
        format!(
            "This conversation continues one from another session. Its transcript so far:\n\n\
             <transcript>\n{}\n</transcript>\n\n",
            entries.join("\n\n")
        )
    })
}

/// A streamed text item still open.
struct Text {
    id: ItemId,
    reasoning: bool,
    message_id: Option<String>,
    text: String,
}

/// A tool call of the running turn, as merged from its updates.
struct Tool {
    id: ItemId,
    name: String,
    kind: ToolKind,
    input: Value,
    content: Vec<ToolCallContent>,
    raw_output: Option<Value>,
    /// Its `tool_call` item was sent.
    called: bool,
    /// Its `tool_result` item was sent.
    finished: bool,
}

struct Turn {
    id: TurnId,
    /// The `session/prompt` request.
    request: u64,
    interrupted: bool,
    text: Option<Text>,
    tools: HashMap<String, Tool>,
}

/// A permission request waiting for the daemon's answer.
struct Approval {
    request: Value,
    options: Vec<PermissionOption>,
}

struct Session {
    rpc: Rpc,
    events: mpsc::Sender<AdapterEvent>,
    session_id: String,
    mode: PermissionMode,
    /// The model config option's id, when the agent has one.
    model_config: Option<String>,
    model: Option<String>,
    /// Transcript for the first prompt; taken by it.
    seed: Option<String>,
    turn: Option<Turn>,
    approvals: HashMap<ApprovalId, Approval>,
    /// `session/set_config_option` requests for the model, by id.
    model_requests: HashMap<u64, String>,
    next_item: u64,
    next_approval: u64,
}

/// How a permission request is answered without asking.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Allow,
    Deny,
    Ask,
}

fn verdict(mode: PermissionMode, kind: ToolKind) -> Verdict {
    match (kind, mode) {
        (ToolKind::Read | ToolKind::Search | ToolKind::Think, _) => Verdict::Allow,
        (_, PermissionMode::ReadOnly) => Verdict::Deny,
        (_, PermissionMode::FullAccess) => Verdict::Allow,
        (ToolKind::Edit | ToolKind::Delete | ToolKind::Move, PermissionMode::AutoEdit) => {
            Verdict::Allow
        }
        _ => Verdict::Ask,
    }
}

/// The option that carries `decision`, preferring the one-time kind; `cancelled` when the agent
/// offers none.
fn outcome(options: &[PermissionOption], decision: ApprovalDecision) -> Outcome {
    let kinds = match decision {
        ApprovalDecision::Allow => [
            PermissionOptionKind::AllowOnce,
            PermissionOptionKind::AllowAlways,
        ],
        ApprovalDecision::Deny => [
            PermissionOptionKind::RejectOnce,
            PermissionOptionKind::RejectAlways,
        ],
    };
    kinds
        .iter()
        .find_map(|kind| options.iter().find(|option| option.kind == *kind))
        .map_or(Outcome::Cancelled, |option| Outcome::Selected {
            option_id: option.option_id.clone(),
        })
}

/// A tool's output as text: its text content, else its raw output.
fn tool_output(tool: &Tool) -> String {
    let parts: Vec<String> = tool
        .content
        .iter()
        .filter_map(|content| match content {
            ToolCallContent::Content {
                content: ContentBlock::Text { text },
            } => Some(text.clone()),
            ToolCallContent::Diff { path } => Some(format!("edited {path}")),
            _ => None,
        })
        .collect();
    if !parts.is_empty() {
        return parts.join("\n");
    }
    match &tool.raw_output {
        Some(Value::String(output)) => output.clone(),
        Some(Value::Object(fields)) => match fields.get("output") {
            Some(Value::String(output)) => output.clone(),
            _ => Value::Object(fields.clone()).to_string(),
        },
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

impl Session {
    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<AdapterCommand>) {
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(AdapterCommand::Shutdown) | None => return self.shutdown().await,
                    Some(command) => self.command(command).await,
                },
                message = self.rpc.recv() => match message {
                    Some(message) => self.incoming(message).await,
                    None => {
                        let error = exit_error(self.rpc.exit().await);
                        return self.end(Some(error)).await;
                    }
                },
                () = self.events.closed() => return self.shutdown().await,
            }
        }
    }

    async fn emit(&self, event: AdapterEvent) {
        // A daemon that stopped reading is noticed by `run`, which then shuts down.
        let _ = self.events.send(event).await;
    }

    fn item_id(&mut self) -> ItemId {
        self.next_item += 1;
        ItemId::new(format!("item-{}", self.next_item))
    }

    async fn shutdown(mut self) {
        self.rpc.close().await;
        self.end(None).await;
    }

    /// Closes an open turn as failed, then reports the exit.
    async fn end(mut self, error: Option<TurnError>) {
        if let Some(turn_id) = self.turn.as_ref().map(|turn| turn.id.clone()) {
            self.close_text().await;
            let failure = error.clone().unwrap_or_else(|| TurnError {
                class: ErrorClass::Transient,
                message: "the session was shut down during the turn".into(),
            });
            self.emit(AdapterEvent::TurnFailed {
                turn_id,
                error: failure,
            })
            .await;
        }
        self.emit(AdapterEvent::Exited { error }).await;
    }

    async fn command(&mut self, command: AdapterCommand) {
        match command {
            AdapterCommand::SendPrompt { turn_id, text } => self.prompt(turn_id, text).await,
            AdapterCommand::Interrupt => self.interrupt().await,
            AdapterCommand::SetModel { model } => self.set_model(model).await,
            AdapterCommand::SetPermissionMode { mode } => {
                self.mode = mode;
                self.emit(AdapterEvent::PermissionModeChanged { mode })
                    .await;
            }
            AdapterCommand::AnswerApproval {
                approval_id,
                decision,
            } => {
                if let Some(approval) = self.approvals.remove(&approval_id) {
                    let answer = outcome(&approval.options, decision).response();
                    self.rpc.respond(approval.request, answer).await;
                }
            }
            // Handled by `run`.
            AdapterCommand::Shutdown => {}
        }
    }

    async fn prompt(&mut self, turn_id: TurnId, text: String) {
        if self.turn.is_some() {
            return;
        }
        let text = match self.seed.take() {
            Some(seed) => seed + &text,
            None => text,
        };
        let prompt = schema::prompt(&self.session_id, &text);
        let request = self.rpc.request("session/prompt", prompt).await;
        self.turn = Some(Turn {
            id: turn_id.clone(),
            request,
            interrupted: false,
            text: None,
            tools: HashMap::new(),
        });
        self.emit(AdapterEvent::TurnStarted { turn_id }).await;
    }

    async fn interrupt(&mut self) {
        let Some(turn) = &mut self.turn else { return };
        if turn.interrupted {
            return;
        }
        turn.interrupted = true;
        self.rpc
            .notify("session/cancel", schema::cancel(&self.session_id))
            .await;
        for (_, approval) in std::mem::take(&mut self.approvals) {
            self.rpc
                .respond(approval.request, Outcome::Cancelled.response())
                .await;
        }
    }

    async fn set_model(&mut self, model: String) {
        let Some(config_id) = &self.model_config else {
            // Only sent when the capability says so; report what is in effect.
            if let Some(current) = self.model.clone() {
                self.emit(AdapterEvent::ModelChanged { model: current })
                    .await;
            }
            return;
        };
        let set = schema::set_config_option(&self.session_id, config_id, &model);
        let request = self.rpc.request("session/set_config_option", set).await;
        self.model_requests.insert(request, model);
    }

    async fn incoming(&mut self, message: Incoming) {
        match message {
            Incoming::Response { id, outcome } => self.response(id, outcome).await,
            Incoming::Request { id, method, params } if method == "session/request_permission" => {
                match serde_json::from_value::<RequestPermissionRequest>(params) {
                    Ok(request) => self.permission(id, request).await,
                    Err(err) => {
                        let error = Error {
                            code: INVALID_PARAMS,
                            message: format!("invalid session/request_permission: {err}"),
                            data: None,
                        };
                        self.rpc.respond_error(id, error).await;
                    }
                }
            }
            Incoming::Request { id, method, .. } => decline(&mut self.rpc, id, &method).await,
            Incoming::Notification { method, params } if method == "session/update" => {
                // An update this build cannot read is skipped, not fatal.
                if let Ok(notification) = serde_json::from_value::<SessionNotification>(params)
                    && notification.session_id == self.session_id
                {
                    self.update(notification.update).await;
                }
            }
            Incoming::Notification { .. } => {}
        }
    }

    async fn response(&mut self, id: u64, outcome: Result<Value, Error>) {
        if let Some(wanted) = self.model_requests.remove(&id) {
            return self.model_set(wanted, outcome).await;
        }
        if self.turn.as_ref().is_none_or(|turn| turn.request != id) {
            return;
        }
        self.close_text().await;
        let Some(turn) = self.turn.take() else { return };
        // The agent is done with the turn; whatever it still asked is moot.
        self.approvals.clear();
        let turn_id = turn.id;
        let event = match outcome {
            _ if turn.interrupted => AdapterEvent::TurnInterrupted { turn_id },
            Ok(result) => match serde_json::from_value::<PromptResponse>(result) {
                Ok(response) => match response.stop_reason {
                    StopReason::Cancelled => AdapterEvent::TurnInterrupted { turn_id },
                    StopReason::Refusal => AdapterEvent::TurnFailed {
                        turn_id,
                        error: error(ErrorClass::Fatal, "the agent refused the prompt"),
                    },
                    _ => AdapterEvent::TurnCompleted { turn_id },
                },
                Err(err) => AdapterEvent::TurnFailed {
                    turn_id,
                    error: error(
                        ErrorClass::Fatal,
                        format!("session/prompt: unexpected response: {err}"),
                    ),
                },
            },
            Err(err) => AdapterEvent::TurnFailed {
                turn_id,
                error: agent_error("session/prompt", &err),
            },
        };
        self.emit(event).await;
    }

    async fn model_set(&mut self, wanted: String, outcome: Result<Value, Error>) {
        let response = outcome
            .ok()
            .and_then(|result| serde_json::from_value::<ConfigOptionsResponse>(result).ok());
        if let Some(response) = response {
            self.model =
                Some(model_option(&response.config_options).map_or(wanted, |(_, current)| current));
        }
        // On failure the model is unchanged, and that is what is reported.
        if let Some(model) = self.model.clone() {
            self.emit(AdapterEvent::ModelChanged { model }).await;
        }
    }

    async fn update(&mut self, update: SessionUpdate) {
        match update {
            SessionUpdate::AgentMessageChunk(chunk) => {
                self.chunk(false, chunk.message_id, chunk.content).await;
            }
            SessionUpdate::AgentThoughtChunk(chunk) => {
                self.chunk(true, chunk.message_id, chunk.content).await;
            }
            SessionUpdate::ToolCall(fields) | SessionUpdate::ToolCallUpdate(fields) => {
                self.tool_update(fields).await;
            }
            SessionUpdate::ConfigOptionUpdate { config_options } => {
                if let Some((_, current)) = model_option(&config_options)
                    && self.model.as_ref() != Some(&current)
                {
                    self.model = Some(current.clone());
                    self.emit(AdapterEvent::ModelChanged { model: current })
                        .await;
                }
            }
            SessionUpdate::Other => {}
        }
    }

    /// Appends a message or thought chunk, starting a new item when the stream switches.
    async fn chunk(&mut self, reasoning: bool, message_id: Option<String>, content: ContentBlock) {
        let ContentBlock::Text { text: chunk } = content else {
            return;
        };
        let Some(turn) = &self.turn else { return };
        let continues = turn
            .text
            .as_ref()
            .is_some_and(|text| text.reasoning == reasoning && text.message_id == message_id);
        if !continues {
            self.close_text().await;
            let id = self.item_id();
            let Some(turn) = &mut self.turn else { return };
            let body = if reasoning {
                ItemBody::Reasoning {
                    text: String::new(),
                }
            } else {
                ItemBody::AssistantMessage {
                    text: String::new(),
                }
            };
            let item = Item {
                id: id.clone(),
                turn_id: turn.id.clone(),
                body,
            };
            turn.text = Some(Text {
                id,
                reasoning,
                message_id,
                text: String::new(),
            });
            self.emit(AdapterEvent::ItemStarted { item }).await;
        }
        let Some(text) = self.turn.as_mut().and_then(|turn| turn.text.as_mut()) else {
            return;
        };
        text.text.push_str(&chunk);
        let event = AdapterEvent::ItemDelta {
            item_id: text.id.clone(),
            text: chunk,
        };
        self.emit(event).await;
    }

    /// Completes the open text item, if any.
    async fn close_text(&mut self) {
        let Some(turn) = &mut self.turn else { return };
        let Some(text) = turn.text.take() else { return };
        let body = if text.reasoning {
            ItemBody::Reasoning { text: text.text }
        } else {
            ItemBody::AssistantMessage { text: text.text }
        };
        let item = Item {
            id: text.id,
            turn_id: turn.id.clone(),
            body,
        };
        self.emit(AdapterEvent::ItemCompleted { item }).await;
    }

    /// Merges a tool call or its update and sends the items it makes final. Returns the key of
    /// the tool call, or `None` outside a turn.
    async fn tool_update(&mut self, fields: ToolCallFields) -> Option<String> {
        let key = fields.tool_call_id.clone();
        let known = self.turn.as_ref()?.tools.contains_key(&key);
        if !known {
            let id = self.item_id();
            let name = fields
                .name
                .clone()
                .or_else(|| fields.title.clone())
                .unwrap_or_else(|| "tool".into());
            let tool = Tool {
                id,
                name,
                kind: ToolKind::Other,
                input: Value::Null,
                content: Vec::new(),
                raw_output: None,
                called: false,
                finished: false,
            };
            self.turn.as_mut()?.tools.insert(key.clone(), tool);
        }
        let tool = self.turn.as_mut()?.tools.get_mut(&key)?;
        if let Some(kind) = fields.kind {
            tool.kind = kind;
        }
        if let Some(name) = fields.name.filter(|_| !tool.called) {
            tool.name = name;
        }
        if let Some(input) = fields.raw_input.filter(|_| !tool.called) {
            tool.input = input;
        }
        if let Some(content) = fields.content {
            tool.content = content;
        }
        if let Some(output) = fields.raw_output {
            tool.raw_output = Some(output);
        }
        match fields.status {
            Some(ToolCallStatus::InProgress) => self.send_call(&key).await,
            Some(status @ (ToolCallStatus::Completed | ToolCallStatus::Failed)) => {
                self.send_call(&key).await;
                self.send_result(&key, status == ToolCallStatus::Failed)
                    .await;
            }
            _ => {}
        }
        Some(key)
    }

    /// Sends the tool call's item, once.
    async fn send_call(&mut self, key: &str) {
        let needed = self
            .turn
            .as_ref()
            .and_then(|turn| turn.tools.get(key))
            .is_some_and(|tool| !tool.called);
        if !needed {
            return;
        }
        self.close_text().await;
        let Some(turn) = &mut self.turn else { return };
        let Some(tool) = turn.tools.get_mut(key) else {
            return;
        };
        tool.called = true;
        let item = Item {
            id: tool.id.clone(),
            turn_id: turn.id.clone(),
            body: ItemBody::ToolCall {
                name: tool.name.clone(),
                input: tool.input.clone(),
            },
        };
        self.emit(AdapterEvent::ItemCompleted { item }).await;
    }

    /// Sends the tool call's result item, once.
    async fn send_result(&mut self, key: &str, is_error: bool) {
        let needed = self
            .turn
            .as_ref()
            .and_then(|turn| turn.tools.get(key))
            .is_some_and(|tool| !tool.finished);
        if !needed {
            return;
        }
        let id = self.item_id();
        let Some(turn) = &mut self.turn else { return };
        let Some(tool) = turn.tools.get_mut(key) else {
            return;
        };
        tool.finished = true;
        let item = Item {
            id,
            turn_id: turn.id.clone(),
            body: ItemBody::ToolResult {
                call_id: tool.id.clone(),
                output: tool_output(tool),
                is_error,
            },
        };
        self.emit(AdapterEvent::ItemCompleted { item }).await;
    }

    async fn permission(&mut self, request_id: Value, request: RequestPermissionRequest) {
        let live = self.turn.as_ref().is_some_and(|turn| !turn.interrupted);
        if !live || request.session_id != self.session_id {
            return self
                .rpc
                .respond(request_id, Outcome::Cancelled.response())
                .await;
        }
        let summary = request.tool_call.title.clone();
        let Some(key) = self.tool_update(request.tool_call).await else {
            return;
        };
        self.send_call(&key).await;
        let Some(turn) = &self.turn else { return };
        let Some(tool) = turn.tools.get(&key) else {
            return;
        };
        let (turn_id, tool_call_id) = (turn.id.clone(), tool.id.clone());
        let summary = summary.unwrap_or_else(|| tool.name.clone());
        let decision = match verdict(self.mode, tool.kind) {
            Verdict::Allow => ApprovalDecision::Allow,
            Verdict::Deny => ApprovalDecision::Deny,
            Verdict::Ask => {
                self.next_approval += 1;
                let approval_id = ApprovalId::new(format!("approval-{}", self.next_approval));
                let approval = Approval {
                    request: request_id,
                    options: request.options,
                };
                self.approvals.insert(approval_id.clone(), approval);
                return self
                    .emit(AdapterEvent::ApprovalRequested {
                        approval_id,
                        turn_id,
                        tool_call_id,
                        summary,
                    })
                    .await;
            }
        };
        let answer = outcome(&request.options, decision).response();
        self.rpc.respond(request_id, answer).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdicts_follow_the_permission_mode() {
        use PermissionMode::*;
        use Verdict::{Allow, Ask as Asks, Deny};
        let table = [
            (ToolKind::Read, [Allow, Allow, Allow, Allow]),
            (ToolKind::Edit, [Deny, Asks, Allow, Allow]),
            (ToolKind::Execute, [Deny, Asks, Asks, Allow]),
            (ToolKind::Fetch, [Deny, Asks, Asks, Allow]),
            (ToolKind::Other, [Deny, Asks, Asks, Allow]),
        ];
        for (kind, verdicts) in table {
            for (mode, expected) in [ReadOnly, Ask, AutoEdit, FullAccess]
                .into_iter()
                .zip(verdicts)
            {
                assert_eq!(verdict(mode, kind), expected, "{kind:?} in {mode:?}");
            }
        }
    }

    #[test]
    fn outcome_prefers_the_one_time_option() {
        let option = |id: &str, kind| PermissionOption {
            option_id: id.into(),
            kind,
        };
        let options = [
            option("always", PermissionOptionKind::AllowAlways),
            option("once", PermissionOptionKind::AllowOnce),
            option("never", PermissionOptionKind::RejectAlways),
        ];
        let selected = |id: &str| Outcome::Selected {
            option_id: id.into(),
        };
        assert_eq!(outcome(&options, ApprovalDecision::Allow), selected("once"));
        assert_eq!(outcome(&options, ApprovalDecision::Deny), selected("never"));
        assert_eq!(
            outcome(&options[..2], ApprovalDecision::Deny),
            Outcome::Cancelled
        );
        assert_eq!(
            selected("once").response(),
            serde_json::json!({"outcome": {"outcome": "selected", "optionId": "once"}})
        );
    }

    #[test]
    fn seed_renders_messages_and_tools() {
        let turn = TurnId::new("t");
        let item = |id: &str, body| Item {
            id: ItemId::new(id),
            turn_id: turn.clone(),
            body,
        };
        let seed = [
            item("1", ItemBody::UserMessage { text: "hi".into() }),
            item("2", ItemBody::Reasoning { text: "hmm".into() }),
            item(
                "3",
                ItemBody::ToolCall {
                    name: "bash".into(),
                    input: serde_json::json!({"command": "ls"}),
                },
            ),
            item(
                "4",
                ItemBody::ToolResult {
                    call_id: ItemId::new("3"),
                    output: "a.txt".into(),
                    is_error: false,
                },
            ),
            item(
                "5",
                ItemBody::AssistantMessage {
                    text: "done".into(),
                },
            ),
        ];
        assert_eq!(render_seed(&[]), None);
        assert_eq!(
            render_seed(&seed).unwrap(),
            "This conversation continues one from another session. Its transcript so far:\n\n\
             <transcript>\n[user]\nhi\n\n[tool call: bash]\n{\"command\":\"ls\"}\n\n\
             [tool result]\na.txt\n\n[assistant]\ndone\n</transcript>\n\n"
        );
    }
}
