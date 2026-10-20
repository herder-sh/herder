//! The part of the ACP v1 schema this adapter speaks, as the agent sends it.
//!
//! Names and wire shapes follow <https://agentclientprotocol.com/protocol/schema>. Parsing is
//! lenient: unknown fields are ignored and unknown enum values fall back to `Other`, so an agent
//! newer than this file still works.
//!
//! These are written out here rather than taken from the `agent-client-protocol-schema` crate,
//! which enables `serde_json`'s `preserve_order` for the whole workspace and so changes the
//! bytes of herder-protocol's committed JSON Schema.

use std::collections::BTreeMap;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use herder_protocol::{HERDER_MCP_SERVER, Image};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{McpServer, McpTransport, UserMcpServer};

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
    #[serde(default)]
    pub mcp_capabilities: McpCapabilities,
}

/// The MCP transports an agent connects to beyond stdio, which every agent must.
#[derive(Default, Deserialize)]
pub(super) struct McpCapabilities {
    #[serde(default)]
    pub http: bool,
    #[serde(default)]
    pub sse: bool,
}

/// Content a prompt may carry beyond text; ACP agents all take text and resource links.
#[derive(Default, Deserialize)]
pub(super) struct PromptCapabilities {
    #[serde(default)]
    pub image: bool,
}

/// `session/new` params: herder's MCP server as `herder`, then the user's under their names,
/// leaving out those over a transport the agent did not say it takes.
pub(super) fn new_session(
    cwd: &Path,
    herder: Option<&McpServer>,
    servers: &[UserMcpServer],
    takes: &McpCapabilities,
) -> Value {
    let pairs = |map: &BTreeMap<String, String>| -> Vec<Value> {
        map.iter()
            .map(|(name, value)| json!({"name": name, "value": value}))
            .collect()
    };
    let herder = herder.map(|mcp| {
        json!({
            "name": HERDER_MCP_SERVER,
            "command": mcp.command,
            "args": mcp.args,
            "env": [],
        })
    });
    let user = servers.iter().filter_map(|server| {
        let name = &server.name;
        Some(match &server.transport {
            McpTransport::Stdio { command, args, env } => {
                json!({"name": name, "command": command, "args": args, "env": pairs(env)})
            }
            McpTransport::Http { url, headers } if takes.http => {
                json!({"type": "http", "name": name, "url": url, "headers": pairs(headers)})
            }
            McpTransport::Sse { url, headers } if takes.sse => {
                json!({"type": "sse", "name": name, "url": url, "headers": pairs(headers)})
            }
            McpTransport::Http { .. } | McpTransport::Sse { .. } => return None,
        })
    });
    let servers: Vec<Value> = herder.into_iter().chain(user).collect();
    json!({"cwd": cwd, "mcpServers": servers})
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn servers_over_a_transport_the_agent_does_not_take_are_left_out() {
        let url = |kind: &str| format!("https://example.com/{kind}");
        let servers = [
            UserMcpServer {
                name: "local".into(),
                transport: McpTransport::Stdio {
                    command: "server".into(),
                    args: Vec::new(),
                    env: BTreeMap::new(),
                },
            },
            UserMcpServer {
                name: "remote".into(),
                transport: McpTransport::Http {
                    url: url("mcp"),
                    headers: BTreeMap::new(),
                },
            },
            UserMcpServer {
                name: "events".into(),
                transport: McpTransport::Sse {
                    url: url("sse"),
                    headers: BTreeMap::new(),
                },
            },
        ];
        let names = |takes: McpCapabilities| -> Vec<String> {
            let params = new_session(Path::new("/w"), None, &servers, &takes);
            params["mcpServers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|server| server["name"].as_str().unwrap().to_owned())
                .collect()
        };
        let none = McpCapabilities::default();
        assert_eq!(names(none), ["local"]);
        let http = McpCapabilities {
            http: true,
            sse: false,
        };
        assert_eq!(names(http), ["local", "remote"]);
        let both = McpCapabilities {
            http: true,
            sse: true,
        };
        assert_eq!(names(both), ["local", "remote", "events"]);
    }
}
