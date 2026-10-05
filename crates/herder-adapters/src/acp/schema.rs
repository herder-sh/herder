//! The part of the ACP v1 schema this adapter speaks, as the agent sends it.
//!
//! Names and wire shapes follow <https://agentclientprotocol.com/protocol/schema>. Parsing is
//! lenient: unknown fields are ignored and unknown enum values fall back to `Other`, so an agent
//! newer than this file still works.
//!
//! These are written out here rather than taken from the `agent-client-protocol-schema` crate,
//! which enables `serde_json`'s `preserve_order` for the whole workspace and so changes the
//! bytes of herder-protocol's committed JSON Schema.

use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use herder_protocol::Image;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The only ACP version there is.
pub(super) const PROTOCOL_VERSION: u16 = 1;

/// A JSON-RPC error object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Error {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// `initialize` params: no client file system or terminal; agents use their own tools.
pub(super) fn initialize() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
        "clientInfo": {"name": "herder", "version": env!("CARGO_PKG_VERSION")},
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct InitializeResponse {
    pub protocol_version: u16,
    #[serde(default)]
    pub agent_capabilities: AgentCapabilities,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AgentCapabilities {
    #[serde(default)]
    pub prompt_capabilities: PromptCapabilities,
}

/// Content a prompt may carry beyond text; ACP agents all take text and resource links.
#[derive(Default, Deserialize)]
pub(super) struct PromptCapabilities {
    #[serde(default)]
    pub image: bool,
}

/// `session/new` params.
pub(super) fn new_session(cwd: &Path) -> Value {
    json!({"cwd": cwd, "mcpServers": []})
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct NewSessionResponse {
    pub session_id: String,
    #[serde(default)]
    pub config_options: Vec<ConfigOption>,
}

/// A session config option; only `select` options carry a string current value.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ConfigOption {
    pub id: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub current_value: Value,
}

/// `session/set_config_option` params for a `select` option.
pub(super) fn set_config_option(session_id: &str, config_id: &str, value: &str) -> Value {
    json!({"sessionId": session_id, "configId": config_id, "value": value})
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ConfigOptionsResponse {
    #[serde(default)]
    pub config_options: Vec<ConfigOption>,
}

/// `session/prompt` params: an `image` block per image, then one text block.
pub(super) fn prompt(session_id: &str, text: &str, images: &[Image]) -> Value {
    let blocks: Vec<Value> = images
        .iter()
        .map(|image| {
            json!({
                "type": "image",
                "mimeType": image.media_type,
                "data": STANDARD.encode(&image.data.0),
            })
        })
        .chain([json!({"type": "text", "text": text})])
        .collect();
    json!({"sessionId": session_id, "prompt": blocks})
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PromptResponse {
    pub stop_reason: StopReason,
    /// The turn's tokens, from agents that report them (OpenCode).
    #[serde(default)]
    pub usage: Option<PromptUsage>,
}

/// A `session/prompt` response's `usage`; input excludes the cached tokens, output excludes
/// the thought tokens.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PromptUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub thought_tokens: u64,
    #[serde(default)]
    pub cached_read_tokens: u64,
    #[serde(default)]
    pub cached_write_tokens: u64,
}

/// A `usage_update`'s `cost`: the session's spend so far.
#[derive(Debug, Deserialize)]
pub(super) struct Cost {
    pub amount: f64,
    pub currency: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
    #[serde(other)]
    Other,
}

/// `session/cancel` params.
pub(super) fn cancel(session_id: &str) -> Value {
    json!({"sessionId": session_id})
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SessionNotification {
    pub session_id: String,
    pub update: SessionUpdate,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub(super) enum SessionUpdate {
    AgentMessageChunk(ContentChunk),
    AgentThoughtChunk(ContentChunk),
    ToolCall(ToolCallFields),
    ToolCallUpdate(ToolCallFields),
    #[serde(rename_all = "camelCase")]
    ConfigOptionUpdate {
        config_options: Vec<ConfigOption>,
    },
    /// The context window's fill, and the session's cost so far where the agent knows it.
    UsageUpdate {
        #[serde(default)]
        cost: Option<Cost>,
    },
    #[serde(rename_all = "camelCase")]
    AvailableCommandsUpdate {
        available_commands: Vec<AvailableCommand>,
    },
    #[serde(other)]
    Other,
}

/// A command the agent offers, such as a skill; only its name is used.
#[derive(Debug, Deserialize)]
pub(super) struct AvailableCommand {
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ContentChunk {
    pub content: ContentBlock,
    #[serde(default)]
    pub message_id: Option<String>,
}

/// A content block; only text is used.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ContentBlock {
    Text {
        text: String,
    },
    #[serde(other)]
    Other,
}

/// A `tool_call`, or the fields a `tool_call_update` changes.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ToolCallFields {
    pub tool_call_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: Option<ToolKind>,
    #[serde(default)]
    pub status: Option<ToolCallStatus>,
    #[serde(default)]
    pub content: Option<Vec<ToolCallContent>>,
    #[serde(default)]
    pub raw_input: Option<Value>,
    #[serde(default)]
    pub raw_output: Option<Value>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    SwitchMode,
    #[serde(other)]
    Other,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ToolCallContent {
    Content {
        content: ContentBlock,
    },
    Diff {
        path: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RequestPermissionRequest {
    pub session_id: String,
    pub tool_call: ToolCallFields,
    #[serde(default)]
    pub options: Vec<PermissionOption>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PermissionOption {
    pub option_id: String,
    pub kind: PermissionOptionKind,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum PermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
    #[serde(other)]
    Other,
}

/// `session/request_permission` result.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(super) enum Outcome {
    Cancelled,
    #[serde(rename_all = "camelCase")]
    Selected {
        option_id: String,
    },
}

impl Outcome {
    pub(super) fn response(self) -> Value {
        json!({"outcome": self})
    }
}
