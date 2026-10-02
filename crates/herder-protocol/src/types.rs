//! Small value types shared by commands and events.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

/// Vendor CLI that runs a session; unknown names are kept verbatim so new providers never break a client.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Provider {
    /// Anthropic Claude Code.
    Claude,
    /// OpenAI Codex CLI.
    Codex,
    /// Cursor CLI.
    Cursor,
    /// xAI Grok, over ACP.
    Grok,
    /// OpenCode, over ACP.
    Opencode,
    /// Google Gemini CLI, over ACP.
    Gemini,
    /// A provider this build does not know, by its wire name.
    Other(String),
}

impl Provider {
    /// Wire name of the provider.
    pub fn as_str(&self) -> &str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
            Provider::Cursor => "cursor",
            Provider::Grok => "grok",
            Provider::Opencode => "opencode",
            Provider::Gemini => "gemini",
            Provider::Other(name) => name,
        }
    }
}

impl From<String> for Provider {
    fn from(name: String) -> Self {
        match name.as_str() {
            "claude" => Provider::Claude,
            "codex" => Provider::Codex,
            "cursor" => Provider::Cursor,
            "grok" => Provider::Grok,
            "opencode" => Provider::Opencode,
            "gemini" => Provider::Gemini,
            _ => Provider::Other(name),
        }
    }
}

impl From<Provider> for String {
    fn from(provider: Provider) -> Self {
        provider.as_str().to_owned()
    }
}

impl JsonSchema for Provider {
    fn schema_name() -> Cow<'static, str> {
        "Provider".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "Vendor CLI that runs a session. Open set: accept names not listed in `examples`.",
            "type": "string",
            "examples": ["claude", "codex", "cursor", "grok", "opencode", "gemini"]
        })
    }
}

/// How much the agent may do without asking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Reads only; every write or command is refused.
    ReadOnly,
    /// Asks before every write or command.
    Ask,
    /// Edits files freely; asks before commands.
    AutoEdit,
    /// Does anything without asking.
    FullAccess,
}
