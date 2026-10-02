//! One running `claude`: the startup handshake, then the loop that turns commands into
//! stream-json lines and the CLI's lines into adapter events.

use std::collections::HashMap;
use std::time::Duration;

use herder_protocol::{
    Answer, ApprovalDecision, ApprovalId, ErrorClass, Item, ItemBody, ItemId, PermissionMode,
    QuestionId, TurnError, TurnId,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::{mpsc, oneshot};

use super::wire::{
    self, ApiEvent, AskUserQuestion, Block, BlockStart, CanUseTool, ControlRequest, Delta,
    Incoming, Permission, Question, Request, Response,
};
use super::{Failure, classify, mode_flag, mode_from_flag, usage};
use crate::transport::{Exit, Transport};
use crate::{AdapterCommand, AdapterEvent, AdapterSession, Capabilities, StartRequest};

/// Events buffered before the session waits for the daemon to read.
const EVENT_BUFFER: usize = 64;

/// How long a stopping `claude` gets to exit on its own before it is killed.
pub(super) const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// Longest approval summary, in characters.
const SUMMARY_MAX: usize = 200;

/// What the agent is told when herder refuses a tool call.
const DENIED: &str = "The user denied this tool call.";

/// What the agent is told when its `AskUserQuestion` input has no question herder can read.
const UNREADABLE_QUESTIONS: &str = "herder could not read these questions. Ask the user in \
                                    your reply instead, then end your turn.";

/// Heads the seed transcript.
const SEED_PREAMBLE: &str = "This session continues an earlier conversation, replayed below \
                             from herder's log. It is context only and needs no reply.";

pub(super) fn fatal(message: impl Into<String>) -> TurnError {
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
        model: None,
        native_id: None,
        mode: request.permission_mode,
        next_request: 0,
        pending: HashMap::new(),
        turn: None,
        block: None,
        streaming: None,
        tool_calls: HashMap::new(),
        approvals: HashMap::new(),
        asks: Vec::new(),
        next_item: 0,
        next_approval: 0,
        next_question: 0,
    };

    let initialize = session.request(Request::Initialize, Pending::Ignored).await;
    session.pending.remove(&initialize);
    loop {
        let Some(line) = stdout.recv().await else {
            return Err(fatal(format!(
                "claude stopped while starting: {}",
                gone(&mut exit).await
            )));
        };
        let Ok(incoming) = serde_json::from_str::<Incoming>(&line) else {
            continue;
        };
        match incoming {
            Incoming::ControlResponse { response } if response.request_id == initialize => {
                if let Some(error) = response.error.filter(|_| response.subtype == "error") {
                    return Err(fatal(format!("claude refused initialize: {error}")));
                }
                break;
            }
            incoming => session.handle(incoming).await,
        }
    }

    if let Some(seed) = seed_text(&request.seed) {
        session
            .send(&wire::UserLine {
                kind: "user",
                message: wire::UserMessage {
                    role: "user",
                    content: &seed,
                },
                parent_tool_use_id: None,
                session_id: "",
                should_query: Some(false),
                origin: None,
            })
            .await;
        // Context that runs no turn still ends with a result.
        loop {
            let Some(line) = stdout.recv().await else {
                return Err(fatal(format!(
                    "claude stopped while taking the seed: {}",
                    gone(&mut exit).await
                )));
            };
            match serde_json::from_str::<Incoming>(&line) {
                Ok(Incoming::Result(_)) => break,
                Ok(incoming) => session.handle(incoming).await,
                Err(_) => {}
            }
        }
    }

    let (commands, command_rx) = mpsc::unbounded_channel();
    tokio::spawn(session.run(command_rx, stdout, exit));
    Ok(AdapterSession {
        capabilities: Capabilities {
            native_model_switch: true,
            native_permission_mode_switch: true,
            reports_usage: true,
            native_resume: true,
        },
        commands,
        events,
    })
}

/// What an in-flight request from herder was for, so its answer can be acted on.
enum Pending {
    SetModel(String),
    SetMode(PermissionMode),
    Ignored,
}

/// The turn the daemon started and the CLI has not finished.
struct OpenTurn {
    id: TurnId,
    /// Whether herder asked to interrupt it.
    interrupted: bool,
    /// What it failed with, should it fail.
    failure: Failure,
}

/// An `AskUserQuestion` call waiting for an answer to each of its questions.
struct Ask {
    /// The CLI's `request_id` to answer on.
    request_id: String,
    /// The tool's input, which the answers are added to.
    input: Value,
    questions: Vec<AskedQuestion>,
}

struct AskedQuestion {
    id: QuestionId,
    question: Question,
    /// The `answers` value, once the daemon answered.
    answer: Option<String>,
}

/// A text or thinking item being streamed.
struct Streaming {
    id: ItemId,
    reasoning: bool,
    text: String,
}

struct Session {
    /// `None` once closed, which asks `claude` to exit.
    stdin: Option<mpsc::Sender<String>>,
    events: mpsc::Sender<AdapterEvent>,
    /// The model last reported.
    model: Option<String>,
    /// The CLI's session id last reported.
    native_id: Option<String>,
    /// The permission mode last reported or started with.
    mode: PermissionMode,
    next_request: u64,
    pending: HashMap<String, Pending>,
    turn: Option<OpenTurn>,
    /// The kind of the content block streaming, `true` for thinking, before it has an item.
    block: Option<bool>,
    streaming: Option<Streaming>,
    /// Tool call items of the open turn, by Claude's `tool_use` id.
    tool_calls: HashMap<String, ItemId>,
    /// Open approval requests and the CLI's `request_id` to answer each on.
    approvals: HashMap<ApprovalId, String>,
    /// Open `AskUserQuestion` calls, in the order asked.
    asks: Vec<Ask>,
    next_item: u64,
    next_approval: u64,
    next_question: u64,
}

impl Session {
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
                    Some(line) => {
                        // stream-json is all JSON; anything else is not for herder.
                        if let Ok(incoming) = serde_json::from_str(&line) {
                            self.handle(incoming).await;
                        }
                    }
                    None => break false,
                },
            }
        };
        if asked_to_stop {
            // A turn still running would otherwise be finished before stdin is read to its end.
            if self.turn.is_some() {
                self.request(Request::Interrupt, Pending::Ignored).await;
            }
            // Closing stdin is how `claude` is asked to exit.
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
            self.fail_open_turn(fatal("claude stopped")).await;
            self.emit(AdapterEvent::Exited { error }).await;
        } else {
            let error = fatal(gone(&mut exit).await);
            self.fail_open_turn(error.clone()).await;
            self.emit(AdapterEvent::Exited { error: Some(error) }).await;
        }
    }

    // ---- Commands ----

    async fn command(&mut self, command: AdapterCommand) {
        match command {
            AdapterCommand::SendPrompt { turn_id, text } => {
                self.turn = Some(OpenTurn {
                    id: turn_id.clone(),
                    interrupted: false,
                    failure: Failure::default(),
                });
                self.send(&wire::UserLine {
                    kind: "user",
                    message: wire::UserMessage {
                        role: "user",
                        content: &text,
                    },
                    parent_tool_use_id: None,
                    session_id: "",
                    should_query: None,
                    origin: Some(wire::Origin { kind: "human" }),
                })
                .await;
                self.emit(AdapterEvent::TurnStarted { turn_id }).await;
            }
            AdapterCommand::Interrupt => {
                if let Some(turn) = &mut self.turn
                    && !turn.interrupted
                {
                    turn.interrupted = true;
                    self.request(Request::Interrupt, Pending::Ignored).await;
                }
            }
            AdapterCommand::SetModel { model } => {
                let request = Request::SetModel { model: &model };
                let pending = Pending::SetModel(model.clone());
                self.request(request, pending).await;
            }
            AdapterCommand::SetPermissionMode { mode } => {
                let request = Request::SetPermissionMode {
                    mode: mode_flag(mode),
                };
                self.request(request, Pending::SetMode(mode)).await;
            }
            AdapterCommand::AnswerApproval {
                approval_id,
                decision,
            } => {
                if let Some(request_id) = self.approvals.remove(&approval_id) {
                    let permission = match decision {
                        ApprovalDecision::Allow => Permission::Allow {
                            updated_input: None,
                        },
                        ApprovalDecision::Deny => Permission::Deny { message: DENIED },
                    };
                    self.answer(&request_id, permission).await;
                }
            }
            AdapterCommand::AnswerQuestion {
                question_id,
                answer,
            } => self.answer_question(&question_id, answer).await,
            // Handled by `run`, which owns stopping.
            AdapterCommand::Shutdown => {}
        }
    }

    // ---- Lines from the CLI ----

    async fn handle(&mut self, incoming: Incoming) {
        match incoming {
            Incoming::System(system) => {
                if let Some(id) = system.session_id.filter(|id| !id.is_empty())
                    && self.native_id.as_ref() != Some(&id)
                {
                    self.native_id = Some(id.clone());
                    self.emit(AdapterEvent::SessionIdentified { native_id: id })
                        .await;
                }
                if let Some(model) = system.model {
                    self.model_known(model).await;
                }
                if let Some(mode) = system.permission_mode.as_deref().and_then(mode_from_flag) {
                    self.mode_known(mode).await;
                }
            }
            Incoming::StreamEvent(stream) if stream.parent_tool_use_id.is_none() => {
                self.stream_event(stream.event).await;
            }
            Incoming::Assistant(assistant) if assistant.parent_tool_use_id.is_none() => {
                self.assistant(assistant).await;
            }
            Incoming::User(user) if user.parent_tool_use_id.is_none() => {
                self.tool_results(user.message.content).await;
            }
            Incoming::Result(result) => self.result(result).await,
            Incoming::RateLimitEvent { rate_limit_info } => {
                if let Some(turn) = &mut self.turn
                    && rate_limit_info.status == "rejected"
                {
                    turn.failure.limit_rejected = true;
                }
                let windows = usage::event_windows(&rate_limit_info.unified_windows);
                if !windows.is_empty() {
                    self.emit(AdapterEvent::UsageReported { windows }).await;
                }
            }
            Incoming::ControlRequest {
                request_id,
                request,
            } => match request {
                ControlRequest::CanUseTool(request) => self.can_use_tool(request_id, request).await,
                ControlRequest::Other => {
                    let response = Response::Error {
                        request_id: &request_id,
                        error: "herder does not handle this request",
                    };
                    self.send(&wire::ControlResponseLine {
                        kind: "control_response",
                        response,
                    })
                    .await;
                }
            },
            Incoming::ControlResponse { response } => {
                let pending = self.pending.remove(&response.request_id);
                if response.subtype != "success" {
                    return;
                }
                match pending {
                    Some(Pending::SetModel(model)) => self.model_known(model).await,
                    Some(Pending::SetMode(mode)) => self.mode_known(mode).await,
                    Some(Pending::Ignored) | None => {}
                }
            }
            Incoming::ControlCancelRequest { request_id } => {
                self.approvals.retain(|_, open| *open != request_id);
                self.asks.retain(|ask| ask.request_id != request_id);
            }
            Incoming::StreamEvent(_)
            | Incoming::Assistant(_)
            | Incoming::User(_)
            | Incoming::Other => {}
        }
    }

    async fn model_known(&mut self, model: String) {
        if self.model.as_ref() != Some(&model) {
            self.model = Some(model.clone());
            self.emit(AdapterEvent::ModelChanged { model }).await;
        }
    }

    async fn mode_known(&mut self, mode: PermissionMode) {
        if self.mode != mode {
            self.mode = mode;
            self.emit(AdapterEvent::PermissionModeChanged { mode })
                .await;
        }
    }

    // ---- Items ----

    async fn stream_event(&mut self, event: ApiEvent) {
        let Some(turn_id) = self.turn.as_ref().map(|turn| turn.id.clone()) else {
            return;
        };
        match event {
            ApiEvent::ContentBlockStart { content_block } => {
                self.finish_streaming().await;
                self.block = match content_block {
                    BlockStart::Text {} => Some(false),
                    BlockStart::Thinking {} => Some(true),
                    BlockStart::Other => None,
                };
            }
            ApiEvent::ContentBlockDelta { delta } => {
                let (reasoning, text) = match delta {
                    Delta::Text { text } => (false, text),
                    Delta::Thinking { thinking } => (true, thinking),
                    Delta::Other => return,
                };
                if text.is_empty() || self.block != Some(reasoning) {
                    return;
                }
                if self.streaming.is_none() {
                    let id = self.item_id();
                    self.streaming = Some(Streaming {
                        id: id.clone(),
                        reasoning,
                        text: String::new(),
                    });
                    let body = streamed_body(reasoning, String::new());
                    let item = Item { id, turn_id, body };
                    self.emit(AdapterEvent::ItemStarted { item }).await;
                }
                if let Some(streaming) = &mut self.streaming {
                    streaming.text.push_str(&text);
                    let item_id = streaming.id.clone();
                    self.emit(AdapterEvent::ItemDelta { item_id, text }).await;
                }
            }
            ApiEvent::Other => {}
        }
    }

    async fn assistant(&mut self, assistant: wire::Assistant) {
        let Some(turn) = &mut self.turn else { return };
        let turn_id = turn.id.clone();
        if let Some(error) = assistant.error {
            // The CLI's report of an API error: the reason the turn fails, not a reply.
            let text: Vec<String> = assistant
                .message
                .content
                .into_iter()
                .filter_map(|block| match block {
                    Block::Text { text } => Some(text),
                    _ => None,
                })
                .collect();
            turn.failure.api_error = Some(error);
            turn.failure.text = Some(text.join("\n"));
            return;
        }
        for block in assistant.message.content {
            match block {
                Block::Text { text } => self.block_done(false, text).await,
                Block::Thinking { thinking } => self.block_done(true, thinking).await,
                Block::ToolUse { id, name, input } => {
                    self.finish_streaming().await;
                    self.block = None;
                    self.tool_call(id, turn_id.clone(), name, input).await;
                }
                Block::ToolResult { .. } | Block::Other => {}
            }
        }
    }

    /// A text or thinking block is final: completes its streamed item, or emits it whole.
    async fn block_done(&mut self, reasoning: bool, text: String) {
        self.block = None;
        let Some(turn_id) = self.turn.as_ref().map(|turn| turn.id.clone()) else {
            return;
        };
        match self.streaming.take() {
            Some(streaming) if streaming.reasoning == reasoning => {
                let body = streamed_body(reasoning, text);
                self.emit_item(streaming.id, turn_id, body).await;
            }
            other => {
                self.streaming = other;
                self.finish_streaming().await;
                if !text.is_empty() {
                    let id = self.item_id();
                    self.emit_item(id, turn_id, streamed_body(reasoning, text))
                        .await;
                }
            }
        }
    }

    /// Completes the streaming item, if any, with the text received so far.
    async fn finish_streaming(&mut self) {
        let Some(streaming) = self.streaming.take() else {
            return;
        };
        let Some(turn_id) = self.turn.as_ref().map(|turn| turn.id.clone()) else {
            return;
        };
        let body = streamed_body(streaming.reasoning, streaming.text);
        self.emit_item(streaming.id, turn_id, body).await;
    }

    async fn tool_call(
        &mut self,
        tool_use_id: String,
        turn_id: TurnId,
        name: String,
        input: Value,
    ) -> ItemId {
        let id = self.item_id();
        self.tool_calls.insert(tool_use_id, id.clone());
        let body = ItemBody::ToolCall { name, input };
        self.emit_item(id.clone(), turn_id, body).await;
        id
    }

    async fn tool_results(&mut self, content: Value) {
        let Some(turn_id) = self.turn.as_ref().map(|turn| turn.id.clone()) else {
            return;
        };
        let Ok(blocks) = serde_json::from_value::<Vec<Block>>(content) else {
            // A plain string is a prompt echo or a note, not a tool result.
            return;
        };
        for block in blocks {
            let Block::ToolResult {
                tool_use_id,
                content,
                is_error,
            } = block
            else {
                continue;
            };
            let Some(call_id) = self.tool_calls.get(&tool_use_id).cloned() else {
                continue;
            };
            let id = self.item_id();
            let body = ItemBody::ToolResult {
                call_id,
                output: result_text(content),
                is_error,
            };
            self.emit_item(id, turn_id.clone(), body).await;
        }
    }

    // ---- Approvals ----

    async fn can_use_tool(&mut self, request_id: String, request: CanUseTool) {
        let Some(turn_id) = self.turn.as_ref().map(|turn| turn.id.clone()) else {
            // No turn is waiting on it; refusing is the only answer that cannot do harm.
            self.answer(&request_id, Permission::Deny { message: DENIED })
                .await;
            return;
        };
        if request.tool_name == "AskUserQuestion" {
            self.ask(request_id, turn_id, request.input).await;
            return;
        }
        let summary = summary(&request);
        let tool_call_id = match self.tool_calls.get(&request.tool_use_id) {
            Some(id) => id.clone(),
            // A subagent's tool call, which was skipped: emit it now so the approval names it.
            None => {
                self.finish_streaming().await;
                self.tool_call(
                    request.tool_use_id,
                    turn_id.clone(),
                    request.tool_name,
                    request.input,
                )
                .await
            }
        };
        self.next_approval += 1;
        let approval_id = ApprovalId::new(format!("approval-{}", self.next_approval));
        self.approvals.insert(approval_id.clone(), request_id);
        self.emit(AdapterEvent::ApprovalRequested {
            approval_id,
            turn_id,
            tool_call_id,
            summary,
        })
        .await;
    }

    // ---- Questions ----

    /// Asks each question of an `AskUserQuestion` call as its own `QuestionAsked`; the call
    /// is answered once all of them are.
    async fn ask(&mut self, request_id: String, turn_id: TurnId, input: Value) {
        let questions = match AskUserQuestion::deserialize(&input) {
            Ok(ask) if !ask.questions.is_empty() => ask.questions,
            _ => {
                let permission = Permission::Deny {
                    message: UNREADABLE_QUESTIONS,
                };
                self.answer(&request_id, permission).await;
                return;
            }
        };
        let mut asked = Vec::with_capacity(questions.len());
        for question in questions {
            self.next_question += 1;
            let id = QuestionId::new(format!("question-{}", self.next_question));
            self.emit(AdapterEvent::QuestionAsked {
                question_id: id.clone(),
                turn_id: turn_id.clone(),
                text: question_text(&question),
                choices: question
                    .options
                    .iter()
                    .map(|option| option.label.clone())
                    .collect(),
            })
            .await;
            asked.push(AskedQuestion {
                id,
                question,
                answer: None,
            });
        }
        self.asks.push(Ask {
            request_id,
            input,
            questions: asked,
        });
    }

    /// Records an answer; the last one of its call sends them all. An answer to a question
    /// that is not open, or a choice it does not have, is ignored.
    async fn answer_question(&mut self, question_id: &QuestionId, answer: Answer) {
        let Some(at) = self
            .asks
            .iter()
            .position(|ask| ask.questions.iter().any(|q| &q.id == question_id))
        else {
            return;
        };
        let ask = &mut self.asks[at];
        let Some(asked) = ask.questions.iter_mut().find(|q| &q.id == question_id) else {
            return;
        };
        let text = match answer {
            Answer::Text { text } => text,
            Answer::Choice { index } => {
                let option = usize::try_from(index)
                    .ok()
                    .and_then(|index| asked.question.options.get(index));
                let Some(option) = option else { return };
                option.label.clone()
            }
        };
        asked.answer = Some(text);
        if ask.questions.iter().any(|q| q.answer.is_none()) {
            return;
        }
        let ask = self.asks.remove(at);
        let answers: Map<String, Value> = ask
            .questions
            .into_iter()
            .filter_map(|q| Some((q.question.question, Value::String(q.answer?))))
            .collect();
        let mut input = ask.input;
        if let Value::Object(fields) = &mut input {
            fields.insert("answers".into(), Value::Object(answers));
        }
        let permission = Permission::Allow {
            updated_input: Some(input),
        };
        self.answer(&ask.request_id, permission).await;
    }

    async fn answer(&mut self, request_id: &str, permission: Permission<'_>) {
        self.send(&wire::ControlResponseLine {
            kind: "control_response",
            response: Response::Success {
                request_id,
                response: permission,
            },
        })
        .await;
    }

    // ---- Turns ----

    async fn result(&mut self, result: wire::ResultMessage) {
        let Some(turn) = &mut self.turn else { return };
        let end = if result.subtype == "success" && !result.is_error {
            TurnEnd::Completed
        } else if turn.interrupted {
            TurnEnd::Interrupted
        } else {
            let mut failure = std::mem::take(&mut turn.failure);
            failure.status = result.api_error_status;
            failure.detail = result
                .result
                .filter(|text| !text.is_empty())
                .or_else(|| (!result.errors.is_empty()).then(|| result.errors.join("; ")))
                .or(Some(result.subtype));
            TurnEnd::Failed(classify(failure))
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
        self.finish_streaming().await;
        self.block = None;
        let Some(turn) = self.turn.take() else { return };
        self.tool_calls.clear();
        self.approvals.clear();
        self.asks.clear();
        let turn_id = turn.id;
        self.emit(match end {
            TurnEnd::Completed => AdapterEvent::TurnCompleted { turn_id },
            TurnEnd::Interrupted => AdapterEvent::TurnInterrupted { turn_id },
            TurnEnd::Failed(error) => AdapterEvent::TurnFailed { turn_id, error },
        })
        .await;
    }

    // ---- Plumbing ----

    fn item_id(&mut self) -> ItemId {
        self.next_item += 1;
        ItemId::new(format!("item-{}", self.next_item))
    }

    async fn emit_item(&mut self, id: ItemId, turn_id: TurnId, body: ItemBody) {
        let item = Item { id, turn_id, body };
        self.emit(AdapterEvent::ItemCompleted { item }).await;
    }

    async fn emit(&mut self, event: AdapterEvent) {
        // A daemon that stopped reading has nobody left to tell.
        let _ = self.events.send(event).await;
    }

    /// Sends a control request, remembering what its answer is for; returns its id.
    async fn request(&mut self, request: Request<'_>, pending: Pending) -> String {
        self.next_request += 1;
        let request_id = format!("herder-{}", self.next_request);
        self.pending.insert(request_id.clone(), pending);
        self.send(&wire::ControlRequestLine {
            kind: "control_request",
            request_id: &request_id,
            request,
        })
        .await;
        request_id
    }

    async fn send(&mut self, line: &impl Serialize) {
        let Some(stdin) = &self.stdin else { return };
        // Serializing these plain structs cannot fail.
        let Ok(line) = serde_json::to_string(line) else {
            return;
        };
        if stdin.send(line).await.is_err() {
            // `claude` stopped reading; its end of output is about to end the session.
            self.stdin = None;
        }
    }
}

enum TurnEnd {
    Completed,
    Interrupted,
    Failed(TurnError),
}

/// Why `claude` is gone, as a message.
pub(super) async fn gone(exit: &mut oneshot::Receiver<Exit>) -> String {
    match tokio::time::timeout(STOP_TIMEOUT, exit).await {
        Ok(Ok(Exit::Code(Some(code)))) => format!("claude exited with code {code}"),
        Ok(Ok(Exit::Code(None))) => "claude was killed by a signal".into(),
        Ok(Ok(Exit::Failed(message))) => message,
        Ok(Err(_)) | Err(_) => "claude closed its output".into(),
    }
}

fn streamed_body(reasoning: bool, text: String) -> ItemBody {
    if reasoning {
        ItemBody::Reasoning { text }
    } else {
        ItemBody::AssistantMessage { text }
    }
}

/// A tool result's text: a string, or the text blocks of an array.
fn result_text(content: Value) -> String {
    match content {
        Value::String(text) => text,
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// One line saying what a tool call wants to do: the tool and its main argument.
fn summary(request: &CanUseTool) -> String {
    let detail = ["command", "file_path", "path", "url", "pattern"]
        .iter()
        .find_map(|key| request.input.get(*key).and_then(Value::as_str))
        .or(request.description.as_deref())
        .or(request.title.as_deref());
    let summary = match detail {
        Some(detail) => format!("{}: {detail}", request.tool_name),
        None => request.tool_name.clone(),
    };
    let line = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(SUMMARY_MAX) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line,
    }
}

/// A question as Markdown: its text, then what each option means.
fn question_text(question: &Question) -> String {
    let mut text = question.question.clone();
    let described: Vec<String> = question
        .options
        .iter()
        .filter(|option| !option.description.is_empty())
        .map(|option| format!("- **{}**: {}", option.label, option.description))
        .collect();
    if !described.is_empty() {
        text.push_str("\n\n");
        text.push_str(&described.join("\n"));
    }
    if question.multi_select {
        text.push_str(MULTI_SELECT);
    }
    text
}

/// Ends a multi-select question, which the contract answers with one choice or free text.
const MULTI_SELECT: &str = "\n\nMore than one may apply: to pick several, answer with their \
                            names separated by commas.";

/// The seed transcript as one context message; `None` when there is nothing to replay.
fn seed_text(seed: &[Item]) -> Option<String> {
    let entries: Vec<String> = seed
        .iter()
        .filter_map(|item| match &item.body {
            ItemBody::UserMessage { text } => Some(format!("User: {text}")),
            ItemBody::AssistantMessage { text } => Some(format!("Assistant: {text}")),
            ItemBody::ToolCall { name, input } => {
                Some(format!("[Assistant called tool {name} with {input}]"))
            }
            ItemBody::ToolResult {
                output, is_error, ..
            } => {
                let label = if *is_error {
                    "Tool failed"
                } else {
                    "Tool result"
                };
                Some(format!("[{label}: {output}]"))
            }
            ItemBody::Reasoning { .. } | ItemBody::Unknown => None,
        })
        .collect();
    (!entries.is_empty()).then(|| format!("{SEED_PREAMBLE}\n\n{}", entries.join("\n\n")))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn request(tool_name: &str, input: Value, description: Option<&str>) -> CanUseTool {
        CanUseTool {
            tool_name: tool_name.into(),
            input,
            tool_use_id: "toolu_1".into(),
            title: None,
            description: description.map(str::to_owned),
        }
    }

    #[test]
    fn summaries_name_the_tool_and_its_main_argument() {
        let cases = [
            (
                request(
                    "Bash",
                    json!({"command": "touch a\n&& ls", "description": "Make a"}),
                    Some("Make a"),
                ),
                "Bash: touch a && ls",
            ),
            (
                request("Edit", json!({"file_path": "/w/src/lib.rs"}), None),
                "Edit: /w/src/lib.rs",
            ),
            (
                request(
                    "ExitPlanMode",
                    json!({"plan": "x"}),
                    Some("Leave plan mode"),
                ),
                "ExitPlanMode: Leave plan mode",
            ),
            (request("Mystery", json!({}), None), "Mystery"),
        ];
        for (request, summary) in cases {
            assert_eq!(super::summary(&request), summary);
        }
        let long = super::summary(&request("Bash", json!({"command": "é".repeat(300)}), None));
        assert_eq!(long.chars().count(), SUMMARY_MAX + 1);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn tool_result_text_joins_text_blocks() {
        assert_eq!(result_text(json!("hi")), "hi");
        assert_eq!(
            result_text(json!([
                {"type": "text", "text": "a"},
                {"type": "image", "source": {}},
                {"type": "text", "text": "b"}
            ])),
            "a\nb"
        );
        assert_eq!(result_text(Value::Null), "");
    }

    #[test]
    fn seed_text_renders_the_transcript() {
        let item = |body| Item {
            id: ItemId::new("i"),
            turn_id: TurnId::new("t"),
            body,
        };
        assert_eq!(seed_text(&[]), None);
        assert_eq!(
            seed_text(&[item(ItemBody::Reasoning { text: "x".into() })]),
            None
        );
        let seed = [
            item(ItemBody::UserMessage {
                text: "Fix it".into(),
            }),
            item(ItemBody::ToolCall {
                name: "Bash".into(),
                input: json!({"command": "ls"}),
            }),
            item(ItemBody::ToolResult {
                call_id: ItemId::new("i"),
                output: "nope".into(),
                is_error: true,
            }),
            item(ItemBody::AssistantMessage {
                text: "Done.".into(),
            }),
        ];
        assert_eq!(
            seed_text(&seed).unwrap(),
            format!(
                "{SEED_PREAMBLE}\n\nUser: Fix it\n\n[Assistant called tool Bash with \
                 {{\"command\":\"ls\"}}]\n\n[Tool failed: nope]\n\nAssistant: Done."
            )
        );
    }
}
