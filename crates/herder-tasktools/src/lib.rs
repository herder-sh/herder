//! The task tools: what a session's agent calls to run a task as a primary session with child
//! sessions, and to drive the herder daemon it runs in as its apps do. The daemon serves them
//! to every agent over MCP; this crate defines them as data and holds no logic beyond checking
//! the shape of arguments.
//!
//! - [`tools_list`] is the MCP `tools/list` result: name, description, `inputSchema` and
//!   `outputSchema` of every tool, generated from the types in this crate.
//! - [`ToolCall::parse`] turns a `tools/call` request into typed arguments.
//! - [`CallToolResult`] is a `tools/call` result: [`CallToolResult::success`] for a tool's
//!   output, [`ToolError`] for a failure the agent can act on.

mod tools;

use herder_protocol::CommandBody;
use schemars::generate::SchemaSettings;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use tools::{
    AnswerArgs, AnswerInput, AnswerOutput, ChildStatus, CommandInput, EscalateArgs, EscalateInput,
    EscalateOutput, OverviewInput, OverviewOutput, PublishInput, PublishOutput, Request,
    RequestRef, SendInput, SendOutput, SendSessionInput, SendSessionOutput, SpawnInput,
    SpawnOutput, StatusInput, StatusOutput, WaitForInput, WaitForOutput,
};

/// One of the task tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tool {
    /// Start a child session.
    Spawn,
    /// Prompt an idle child.
    Send,
    /// Send to an independent session on this host.
    SendSession,
    /// Snapshot of the children.
    Status,
    /// Block until a child reports or needs an answer.
    WaitFor,
    /// Answer a child's question or approval request.
    Answer,
    /// Hand a child's question or approval request to the user.
    Escalate,
    /// Show an artifact in the caller's thread, with a public link to it.
    Publish,
    /// What the daemon holds: sessions, projects, accounts.
    Overview,
    /// Run a herder command as the session's user.
    Command,
}

impl Tool {
    /// Every tool, in `tools/list` order.
    pub const ALL: [Tool; 10] = [
        Tool::Spawn,
        Tool::Send,
        Tool::SendSession,
        Tool::Status,
        Tool::WaitFor,
        Tool::Answer,
        Tool::Escalate,
        Tool::Publish,
        Tool::Overview,
        Tool::Command,
    ];

    /// Name the agent calls the tool by.
    pub fn name(self) -> &'static str {
        match self {
            Tool::Spawn => "spawn",
            Tool::Send => "send",
            Tool::SendSession => "send_session",
            Tool::Status => "status",
            Tool::WaitFor => "wait_for",
            Tool::Answer => "answer",
            Tool::Escalate => "escalate",
            Tool::Publish => "publish",
            Tool::Overview => "overview",
            Tool::Command => "command",
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
                 separate agent with its own git worktree and branch of this repository (in a \
                 folder that is not a git repository with a commit, it shares the folder with \
                 you and has no branch), and it does not see your conversation, so `prompt` must say everything it needs. \
                 Returns at once with the child's id and branch while the child works in the \
                 background; call wait_for to get its report. Children cannot spawn children; \
                 spawn as many as the work needs, rather than reusing a child for unrelated \
                 work. A child is archived automatically once every pull request it opened is \
                 merged and it is idle: its worktree is removed three days later, its branch \
                 kept. When this \
                 machine is too loaded for another agent, fails with `host_busy` and \
                 `retry_after_secs`: keep working or call wait_for, then retry."
            }
            Tool::SendSession => {
                "Send a message to another existing session on this host. Its agent receives an \
                 attributed prompt through its normal queue; no new session or automatic reply is \
                 created. Supply a stable message_id and reuse it when retrying. Cannot send to \
                 yourself, an archived/moved session, or an agent with higher permissions. \
                 Relay chains are bounded. Do not repeatedly acknowledge or bounce messages. \
                 Existing child send/status/wait_for behavior is unchanged."
            }
            Tool::Send => {
                "Send a follow-up prompt to a child: a correction, the next step, or a reply to \
                 its report. An idle child starts its next turn on it at once; a working child \
                 gets it after its current turn ends, in the order sent, and `queued` is then \
                 true. Either way its report comes through wait_for. An archived child is \
                 unarchived first, with its worktree back on its branch. To unblock a child waiting on a question or approval, \
                 use answer instead."
            }
            Tool::Status => {
                "Snapshot of your children: each one's task, branch, status, latest report, and \
                 the questions and approval requests waiting for your answer. Archived children \
                 are listed too, as `archived`, with their latest report. \
                 Returns at once; use wait_for to block until something changes."
            }
            Tool::WaitFor => {
                "Block until a child needs you: it finished a turn (`report`), or it asked a \
                 question or requested an approval that waits for your answer (`request`). \
                 Returns the oldest such event that no earlier wait_for returned, at once if one \
                 is already waiting; each event is returned once. Pass `child` to wait for that \
                 child only, or omit it to wait for any. Returns `timeout` when `timeout_secs` \
                 pass first, and `idle` at once when no awaited child is working and nothing \
                 waits for you. A child that completed its turn with nothing queued and every \
                 pull request it opened merged is archived by the time you get its report, with \
                 `status` `archived`; otherwise it stays `idle`. A child whose turn failed or \
                 was interrupted, or that waits on a question or approval, is never archived \
                 automatically."
            }
            Tool::Answer => {
                "Answer a question or approval request that a child put to you, unblocking it. \
                 Pass the `child` that asked; for a question add `question_id` with `text`, or \
                 `choice` when it lists choices; for an approval add `approval_id` with \
                 `decision`. Your answer is \
                 final and the child acts on it, so escalate instead when the decision belongs \
                 to the user. Fails with `already_resolved` when someone answered first."
            }
            Tool::Escalate => {
                "Hand a question or approval request that a child put to you over to the user, \
                 when the decision is theirs rather than yours: product or scope choices, \
                 credentials, anything destructive or outside the task. The child stays blocked \
                 until the user answers. Pass the `child` that asked, `question_id` or \
                 `approval_id`, and a `note` with \
                 what the user should know to decide."
            }
            Tool::Publish => {
                "Show an artifact inline in this thread, where the call happens, and get a \
                 public link to it that opens on any device and can be sent to anyone. Use it \
                 to prove your work without being asked: whenever a task has an observable \
                 result, publish it before your final reply, e.g. a screenshot or screen \
                 recording of a UI change, the captured output of a CLI or API change, or the \
                 test run of a refactor. Also use it when something reads better as a visual \
                 than as prose: data as a chart or table, a diagram, side-by-side mocks. Pass \
                 exactly one of `path`, a file on this machine (absolute, or relative to your \
                 working directory) of any type, or `html`, one complete self-contained HTML \
                 document. An HTML page has no network: remote scripts, stylesheets, fonts and \
                 images do not load, so inline all CSS, JavaScript, SVG and data. Support light \
                 and dark with `prefers-color-scheme` and keep it readable in a column about \
                 700px wide and on a phone. At most 10 MiB; shorten or downscale a recording \
                 that is larger. Never publish secrets, credentials or customer data: anyone \
                 with the link can open it. Returns `url`, the public link, and `expires_at`; \
                 `url` is absent when the project keeps artifacts private. Put `url` in your \
                 reply as a Markdown link, never a local file path. Fails with `upload_failed` \
                 when the upload fails; retry later."
            }
            Tool::Overview => {
                "Snapshot of the herder daemon you run in, as its apps see it: `you`, your own \
                 session's id; every session on this host with its status, title, project and \
                 queue; every project; and every account with its usage. Use it to find the ids \
                 the command tool takes. Returns at once."
            }
            Tool::Command => {
                "Run a herder command on the daemon you run in, as the herder apps do: create, \
                 prompt, rename, archive or interrupt sessions (your own included), switch a \
                 session's model, permission mode, account or provider, link pull requests, \
                 manage projects and skills, and read or change the daemon's settings. It runs \
                 as the user who started your session (for a child, your primary's), with that \
                 user's role: commands for the daemon's owners only fail with `not_allowed` for \
                 a member. Commands that open a terminal (terminals, adding or logging in \
                 accounts, installing providers) or pair a device or host are never run for an \
                 agent; ask the user to do those in a herder app. Act only on what the user \
                 asked for: these commands change the user's real sessions and settings. Call \
                 overview first for the ids. Returns the command's result."
            }
        }
    }

    /// JSON Schema of the tool's arguments.
    pub fn input_schema(self) -> Schema {
        match self {
            Tool::Spawn => tool_schema::<SpawnInput>(),
            Tool::Send => tool_schema::<SendInput>(),
            Tool::SendSession => tool_schema::<SendSessionInput>(),
            Tool::Status => tool_schema::<StatusInput>(),
            Tool::WaitFor => tool_schema::<WaitForInput>(),
            Tool::Answer => tool_schema::<AnswerInput>(),
            Tool::Escalate => tool_schema::<EscalateInput>(),
            Tool::Publish => tool_schema::<PublishInput>(),
            Tool::Overview => tool_schema::<OverviewInput>(),
            Tool::Command => tool_schema::<CommandInput>(),
        }
    }

    /// JSON Schema of the tool's successful result, sent as `structuredContent`.
    pub fn output_schema(self) -> Schema {
        match self {
            Tool::Spawn => tool_schema::<SpawnOutput>(),
            Tool::Send => tool_schema::<SendOutput>(),
            Tool::SendSession => tool_schema::<SendSessionOutput>(),
            Tool::Status => tool_schema::<StatusOutput>(),
            Tool::WaitFor => tool_schema::<WaitForOutput>(),
            Tool::Answer => tool_schema::<AnswerOutput>(),
            Tool::Escalate => tool_schema::<EscalateOutput>(),
            Tool::Publish => tool_schema::<PublishOutput>(),
            Tool::Overview => tool_schema::<OverviewOutput>(),
            Tool::Command => json_schema!({
                "type": "object",
                "properties": {
                    "type": {
                        "description": "What the command returned: `applied` when it returns \
                                        nothing more, else its own result with its fields.",
                        "type": "string"
                    }
                },
                "required": ["type"]
            }),
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

/// Every command `command` takes, by its `type`, with the JSON Schema of its arguments. Listing
/// these schemas in `tools/list` would fill an agent's context, so `command` lists the types
/// only and a call with a type's arguments wrong answers with that type's schema.
fn commands() -> Vec<(String, Value)> {
    let schema = tool_schema::<CommandBody>();
    let variants = schema.get("oneOf").and_then(Value::as_array);
    variants
        .into_iter()
        .flatten()
        .filter_map(|variant| {
            let name = variant.pointer("/properties/type/const")?.as_str()?;
            Some((name.to_owned(), variant.clone()))
        })
        .collect()
}

/// The schema `tools/list` gives `command`'s argument: its `type` only, see [`commands`].
fn command_schema(_: &mut SchemaGenerator) -> Schema {
    let types: Vec<String> = commands().into_iter().map(|(name, _)| name).collect();
    json_schema!({
        "type": "object",
        "properties": {
            "type": { "type": "string", "enum": types }
        },
        "required": ["type"]
    })
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
#[derive(Clone, Debug, PartialEq)]
pub enum ToolCall {
    /// `spawn`.
    Spawn(SpawnInput),
    /// `send`.
    Send(SendInput),
    /// `send_session`.
    SendSession(SendSessionInput),
    /// `status`.
    Status(StatusInput),
    /// `wait_for`.
    WaitFor(WaitForInput),
    /// `answer`.
    Answer(AnswerInput),
    /// `escalate`.
    Escalate(EscalateInput),
    /// `publish`.
    Publish(PublishInput),
    /// `overview`.
    Overview(OverviewInput),
    /// `command`.
    Command(CommandInput),
}

impl ToolCall {
    /// Parses a call's `arguments`; MCP lets a client omit them, which reads as `{}`.
    /// Malformed arguments fail with [`ErrorCode::InvalidArguments`].
    pub fn parse(tool: Tool, arguments: Option<Value>) -> Result<ToolCall, ToolError> {
        let arguments = arguments.unwrap_or_else(|| Value::Object(Map::new()));
        let command = arguments
            .pointer("/command/type")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let call = match tool {
            Tool::Spawn => serde_json::from_value(arguments).map(ToolCall::Spawn),
            Tool::Send => serde_json::from_value(arguments).map(ToolCall::Send),
            Tool::SendSession => serde_json::from_value(arguments).map(ToolCall::SendSession),
            Tool::Status => serde_json::from_value(arguments).map(ToolCall::Status),
            Tool::WaitFor => serde_json::from_value(arguments).map(ToolCall::WaitFor),
            Tool::Answer => serde_json::from_value(arguments).map(ToolCall::Answer),
            Tool::Escalate => serde_json::from_value(arguments).map(ToolCall::Escalate),
            Tool::Publish => serde_json::from_value(arguments).map(ToolCall::Publish),
            Tool::Overview => serde_json::from_value(arguments).map(ToolCall::Overview),
            Tool::Command => serde_json::from_value(arguments).map(ToolCall::Command),
        };
        call.map_err(|error| {
            let mut message = error.to_string();
            if tool == Tool::Command
                && let Some((name, schema)) = command
                    .and_then(|name| commands().into_iter().find(|(known, _)| *known == name))
            {
                message.push_str(&format!("; the JSON Schema of `{name}` is {schema}"));
            }
            ToolError::new(ErrorCode::InvalidArguments, message)
        })
    }

    /// The tool called.
    pub fn tool(&self) -> Tool {
        match self {
            ToolCall::Spawn(_) => Tool::Spawn,
            ToolCall::Send(_) => Tool::Send,
            ToolCall::SendSession(_) => Tool::SendSession,
            ToolCall::Status(_) => Tool::Status,
            ToolCall::WaitFor(_) => Tool::WaitFor,
            ToolCall::Answer(_) => Tool::Answer,
            ToolCall::Escalate(_) => Tool::Escalate,
            ToolCall::Publish(_) => Tool::Publish,
            ToolCall::Overview(_) => Tool::Overview,
            ToolCall::Command(_) => Tool::Command,
        }
    }
}

/// Stable code of a tool error; agents and tests match on it, so codes are never renamed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The arguments do not match the tool's schema, or break a limit it states.
    InvalidArguments,
    /// `spawn` was called by a child; children cannot spawn children.
    DepthExceeded,
    /// `spawn` asked for a provider outside the task's providers or a permission mode above the
    /// caller's own, or `command` was refused: beyond the user's role, never run for an agent,
    /// or invalid for the daemon's state.
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
    /// `publish` could not upload the artifact for its public link.
    UploadFailed,
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
