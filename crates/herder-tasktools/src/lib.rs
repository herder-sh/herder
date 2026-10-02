//! The task tools: what a session's agent calls to run a task as a primary session with child
//! sessions. The daemon serves them to every agent over MCP; this crate defines them as data
//! and holds no logic beyond checking the shape of arguments.
//!
//! - [`tools_list`] is the MCP `tools/list` result: name, description, `inputSchema` and
//!   `outputSchema` of every tool, generated from the types in this crate.
//! - [`ToolCall::parse`] turns a `tools/call` request into typed arguments.
//! - [`CallToolResult`] is a `tools/call` result: [`CallToolResult::success`] for a tool's
//!   output, [`ToolError`] for a failure the agent can act on.

mod tools;

use schemars::generate::SchemaSettings;
use schemars::{JsonSchema, Schema};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use tools::{
    AnswerArgs, AnswerInput, AnswerOutput, ChildStatus, EscalateArgs, EscalateInput,
    EscalateOutput, Request, RequestRef, SendInput, SendOutput, SpawnInput, SpawnOutput,
    StatusInput, StatusOutput, WaitForInput, WaitForOutput,
};

/// One of the task tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tool {
    /// Start a child session.
    Spawn,
    /// Prompt an idle child.
    Send,
    /// Snapshot of the children.
    Status,
    /// Block until a child reports or needs an answer.
    WaitFor,
    /// Answer a child's question or approval request.
    Answer,
    /// Hand a child's question or approval request to the user.
    Escalate,
}

impl Tool {
    /// Every tool, in `tools/list` order.
    pub const ALL: [Tool; 6] = [
        Tool::Spawn,
        Tool::Send,
        Tool::Status,
        Tool::WaitFor,
        Tool::Answer,
        Tool::Escalate,
    ];

    /// Name the agent calls the tool by.
    pub fn name(self) -> &'static str {
        match self {
            Tool::Spawn => "spawn",
            Tool::Send => "send",
            Tool::Status => "status",
            Tool::WaitFor => "wait_for",
            Tool::Answer => "answer",
            Tool::Escalate => "escalate",
        }
    }

    /// The tool called `name`; `None` for an unknown name, which MCP answers with a protocol
    /// error rather than a tool error.
    pub fn from_name(name: &str) -> Option<Tool> {
        Tool::ALL.into_iter().find(|tool| tool.name() == name)
    }

    /// What the agent reads to decide when to call the tool and what it gets back.
    pub fn description(self) -> &'static str {
        match self {
            Tool::Spawn => {
                "Start a child session to work on part of your task in parallel. The child is a \
                 separate agent with its own git worktree and branch of this repository, and it \
                 does not see your conversation, so `prompt` must say everything it needs. \
                 Returns at once with the child's id and branch while the child works in the \
                 background; call wait_for to get its report. Children cannot spawn children, \
                 and a task has a limit on children (5 unless the user changed it). When this \
                 machine is too loaded for another agent, fails with `host_busy` and \
                 `retry_after_secs`: keep working or call wait_for, then retry."
            }
            Tool::Send => {
                "Send a follow-up prompt to a child: a correction, the next step, or a reply to \
                 its report. An idle child starts its next turn on it at once; a working child \
                 gets it after its current turn ends, in the order sent, and `queued` is then \
                 true. Either way its report comes through wait_for. To unblock a child waiting \
                 on a question or approval, use answer instead."
            }
            Tool::Status => {
                "Snapshot of your children: each one's task, branch, status, latest report, and \
                 the questions and approval requests waiting for your answer. Returns at once; \
                 use wait_for to block until something changes."
            }
            Tool::WaitFor => {
                "Block until a child needs you: it finished a turn (`report`), or it asked a \
                 question or requested an approval that waits for your answer (`request`). \
                 Returns the oldest such event that no earlier wait_for returned, at once if one \
                 is already waiting; each event is returned once. Pass `child` to wait for that \
                 child only, or omit it to wait for any. Returns `timeout` when `timeout_secs` \
                 pass first, and `idle` at once when no awaited child is working and nothing \
                 waits for you."
            }
            Tool::Answer => {
                "Answer a question or approval request that a child put to you, unblocking it. \
                 For a question pass `question_id` with `text`, or `choice` when it lists \
                 choices; for an approval pass `approval_id` with `decision`. Your answer is \
                 final and the child acts on it, so escalate instead when the decision belongs \
                 to the user. Fails with `already_resolved` when someone answered first."
            }
            Tool::Escalate => {
                "Hand a question or approval request that a child put to you over to the user, \
                 when the decision is theirs rather than yours: product or scope choices, \
                 credentials, anything destructive or outside the task. The child stays blocked \
                 until the user answers. Pass `question_id` or `approval_id`, and a `note` with \
                 what the user should know to decide."
            }
        }
    }

    /// JSON Schema of the tool's arguments.
    pub fn input_schema(self) -> Schema {
        match self {
            Tool::Spawn => tool_schema::<SpawnInput>(),
            Tool::Send => tool_schema::<SendInput>(),
            Tool::Status => tool_schema::<StatusInput>(),
            Tool::WaitFor => tool_schema::<WaitForInput>(),
            Tool::Answer => tool_schema::<AnswerInput>(),
            Tool::Escalate => tool_schema::<EscalateInput>(),
        }
    }

    /// JSON Schema of the tool's successful result, sent as `structuredContent`.
    pub fn output_schema(self) -> Schema {
        match self {
            Tool::Spawn => tool_schema::<SpawnOutput>(),
            Tool::Send => tool_schema::<SendOutput>(),
            Tool::Status => tool_schema::<StatusOutput>(),
            Tool::WaitFor => tool_schema::<WaitForOutput>(),
            Tool::Answer => tool_schema::<AnswerOutput>(),
            Tool::Escalate => tool_schema::<EscalateOutput>(),
        }
    }

    /// The tool as listed by `tools/list`.
    pub fn definition(self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().into(),
            description: self.description().into(),
            input_schema: self.input_schema(),
            output_schema: self.output_schema(),
        }
    }
}

/// A self-contained schema for a tool's arguments or result: subschemas inlined, and the
/// root's `title` and `description` (the Rust type's name and doc) dropped since the tool's
/// own description covers them. MCP requires the root to be an object schema, which an
/// internally tagged enum's `oneOf` alone does not say.
fn tool_schema<T: JsonSchema>() -> Schema {
    let mut schema = SchemaSettings::draft2020_12()
        .with(|settings| {
            settings.inline_subschemas = true;
            settings.meta_schema = None;
        })
        .into_generator()
        .into_root_schema_for::<T>();
    schema.remove("title");
    schema.remove("description");
    schema.insert("type".into(), "object".into());
    schema
}

/// A tool as listed by MCP `tools/list`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    /// Name the agent calls the tool by.
    pub name: String,
    /// What the tool does, for the agent.
    pub description: String,
    /// JSON Schema of the arguments.
    pub input_schema: Schema,
    /// JSON Schema of `structuredContent` in a successful result.
    pub output_schema: Schema,
}

/// MCP `tools/list` result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolsList {
    /// Every task tool.
    pub tools: Vec<ToolDefinition>,
}

/// The `tools/list` result the daemon serves to every agent.
pub fn tools_list() -> ToolsList {
    ToolsList {
        tools: Tool::ALL.into_iter().map(Tool::definition).collect(),
    }
}

/// A `tools/call` request with its arguments checked against the tool's schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolCall {
    /// `spawn`.
    Spawn(SpawnInput),
    /// `send`.
    Send(SendInput),
    /// `status`.
    Status(StatusInput),
    /// `wait_for`.
    WaitFor(WaitForInput),
    /// `answer`.
    Answer(AnswerInput),
    /// `escalate`.
    Escalate(EscalateInput),
}

impl ToolCall {
    /// Parses a call's `arguments`; MCP lets a client omit them, which reads as `{}`.
    /// Malformed arguments fail with [`ErrorCode::InvalidArguments`].
    pub fn parse(tool: Tool, arguments: Option<Value>) -> Result<ToolCall, ToolError> {
        let arguments = arguments.unwrap_or_else(|| Value::Object(Map::new()));
        let call = match tool {
            Tool::Spawn => serde_json::from_value(arguments).map(ToolCall::Spawn),
            Tool::Send => serde_json::from_value(arguments).map(ToolCall::Send),
            Tool::Status => serde_json::from_value(arguments).map(ToolCall::Status),
            Tool::WaitFor => serde_json::from_value(arguments).map(ToolCall::WaitFor),
            Tool::Answer => serde_json::from_value(arguments).map(ToolCall::Answer),
            Tool::Escalate => serde_json::from_value(arguments).map(ToolCall::Escalate),
        };
        call.map_err(|error| ToolError::new(ErrorCode::InvalidArguments, error.to_string()))
    }

    /// The tool called.
    pub fn tool(&self) -> Tool {
        match self {
            ToolCall::Spawn(_) => Tool::Spawn,
            ToolCall::Send(_) => Tool::Send,
            ToolCall::Status(_) => Tool::Status,
            ToolCall::WaitFor(_) => Tool::WaitFor,
            ToolCall::Answer(_) => Tool::Answer,
            ToolCall::Escalate(_) => Tool::Escalate,
        }
    }
}

/// Stable code of a tool error; agents and tests match on it, so codes are never renamed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The arguments do not match the tool's schema.
    InvalidArguments,
    /// `spawn` would exceed the task's limit on children.
    LimitExceeded,
    /// `spawn` was called by a child; children cannot spawn children.
    DepthExceeded,
    /// `spawn` asked for a provider outside the task's providers or a permission mode above the
    /// caller's own.
    NotAllowed,
    /// The session, question or approval request belongs to another session's task.
    NotYourChild,
    /// No such session, question or approval request.
    NotFound,
    /// The question or approval request was already answered, by a user or by the caller.
    AlreadyResolved,
    /// `spawn` found this host without capacity for another session's agent; retry after
    /// [`ToolError::retry_after_secs`].
    HostBusy,
    /// The daemon failed.
    Internal,
}

/// A failed tool call, returned to the agent as an MCP tool error so it can read it and adapt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolError {
    /// What failed.
    pub code: ErrorCode,
    /// What happened and what to do instead, for the agent.
    pub message: String,
    /// Seconds to wait before calling again; set for [`ErrorCode::HostBusy`] only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u32>,
}

impl ToolError {
    /// A tool error with its code and message.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retry_after_secs: None,
        }
    }

    /// A [`ErrorCode::HostBusy`] error telling the agent to retry after `retry_after_secs`.
    pub fn host_busy(retry_after_secs: u32, message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::HostBusy,
            message: message.into(),
            retry_after_secs: Some(retry_after_secs),
        }
    }
}

/// MCP `tools/call` result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallToolResult {
    /// The result as text, for clients that ignore `structuredContent`: the same JSON.
    pub content: Vec<TextContent>,
    /// The tool's output, matching its `outputSchema`; absent on an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    /// Whether the call failed.
    #[serde(default)]
    pub is_error: bool,
}

/// MCP text content block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "text")]
pub struct TextContent {
    /// The text.
    pub text: String,
}

impl CallToolResult {
    /// A successful result carrying a tool's output, both as `structuredContent` and as text.
    pub fn success<T: Serialize>(output: &T) -> Result<Self, serde_json::Error> {
        let value = serde_json::to_value(output)?;
        Ok(Self {
            content: vec![TextContent {
                text: value.to_string(),
            }],
            structured_content: Some(value),
            is_error: false,
        })
    }
}

impl From<ToolError> for CallToolResult {
    /// An error result whose text is the error as JSON, `{"code": ..., "message": ...}`, with
    /// `retry_after_secs` when set. It carries no `structuredContent`, which would have to match
    /// the tool's `outputSchema`.
    fn from(error: ToolError) -> Self {
        let mut json = serde_json::json!({ "code": error.code, "message": error.message });
        if let Some(secs) = error.retry_after_secs {
            json["retry_after_secs"] = secs.into();
        }
        let text = json.to_string();
        Self {
            content: vec![TextContent { text }],
            structured_content: None,
            is_error: true,
        }
    }
}
