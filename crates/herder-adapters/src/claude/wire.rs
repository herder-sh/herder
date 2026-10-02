//! The stream-json lines herder reads from and writes to `claude`, as far as herder uses them.
//!
//! Taken from Claude Code 2.1.286 and the Agent SDK's `sdk.d.ts`. Fields herder does not read
//! are not modelled, and unknown line, block and request types parse as `Other`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---- Out of the CLI ----

/// One line of the CLI's stdout.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum Incoming {
    System(System),
    Assistant(Assistant),
    User(User),
    StreamEvent(StreamEvent),
    Result(ResultMessage),
    RateLimitEvent {
        rate_limit_info: RateLimitInfo,
    },
    ControlRequest {
        request_id: String,
        request: ControlRequest,
    },
    ControlResponse {
        response: ControlResponse,
    },
    ControlCancelRequest {
        request_id: String,
    },
    #[serde(other)]
    Other,
}

/// `system` lines: `init` at the start of a query, `status` on changes, and many others.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct System {
    /// The effective model, on `init`.
    #[serde(default)]
    pub model: Option<String>,
    /// The permission mode, on `init` and on a `status` that changed it.
    #[serde(default)]
    pub permission_mode: Option<String>,
}

/// A finished content block of the main agent or a subagent.
#[derive(Debug, Deserialize)]
pub(super) struct Assistant {
    pub message: AssistantBody,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
    /// Set when the line is the CLI's report of an API error, such as `rate_limit`.
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct AssistantBody {
    #[serde(default)]
    pub content: Vec<Block>,
}

/// A content block of a message.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum Block {
    Text {
        text: String,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        content: Value,
        #[serde(default)]
        is_error: bool,
    },
    #[serde(other)]
    Other,
}

/// A user-role line: tool results, and the CLI's own notes such as an interrupt marker.
#[derive(Debug, Deserialize)]
pub(super) struct User {
    pub message: UserBody,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct UserBody {
    /// A string or an array of blocks.
    #[serde(default)]
    pub content: Value,
}

/// A raw Messages API streaming event, sent with `--include-partial-messages`.
#[derive(Debug, Deserialize)]
pub(super) struct StreamEvent {
    pub event: ApiEvent,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ApiEvent {
    ContentBlockStart {
        content_block: BlockStart,
    },
    ContentBlockDelta {
        delta: Delta,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum BlockStart {
    Text {},
    Thinking {},
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum Delta {
    #[serde(rename = "text_delta")]
    Text { text: String },
    #[serde(rename = "thinking_delta")]
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    #[serde(other)]
    Other,
}

/// The end of a turn, or of a seed message.
#[derive(Debug, Deserialize)]
pub(super) struct ResultMessage {
    /// `success`, or why the turn stopped early: `error_during_execution`, `error_max_turns`, ...
    pub subtype: String,
    pub is_error: bool,
    /// The final text; the error text when a `success` turn ended on an API error.
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub api_error_status: Option<u16>,
    #[serde(default)]
    pub errors: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RateLimitInfo {
    /// `allowed`, `allowed_warning` or `rejected`.
    pub status: String,
}

/// A request from the CLI to herder.
#[derive(Debug, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub(super) enum ControlRequest {
    CanUseTool(CanUseTool),
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
pub(super) struct CanUseTool {
    pub tool_name: String,
    #[serde(default)]
    pub input: Value,
    pub tool_use_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// The CLI's answer to a request from herder.
#[derive(Debug, Deserialize)]
pub(super) struct ControlResponse {
    /// `success` or `error`.
    pub subtype: String,
    pub request_id: String,
    #[serde(default)]
    pub error: Option<String>,
}

// ---- Into the CLI ----

/// A prompt, or with `should_query: false` context that runs no turn.
#[derive(Serialize)]
pub(super) struct UserLine<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub message: UserMessage<'a>,
    pub parent_tool_use_id: Option<&'a str>,
    pub session_id: &'static str,
    #[serde(rename = "shouldQuery", skip_serializing_if = "Option::is_none")]
    pub should_query: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
}

#[derive(Serialize)]
pub(super) struct UserMessage<'a> {
    pub role: &'static str,
    pub content: &'a str,
}

/// Who wrote a prompt; `human` for herder's users, as the SDK asks hosts to stamp.
#[derive(Serialize)]
pub(super) struct Origin {
    pub kind: &'static str,
}

/// A request from herder to the CLI.
#[derive(Serialize)]
pub(super) struct ControlRequestLine<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub request_id: &'a str,
    pub request: Request<'a>,
}

#[derive(Serialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub(super) enum Request<'a> {
    Initialize,
    Interrupt,
    SetModel { model: &'a str },
    SetPermissionMode { mode: &'a str },
}

/// herder's answer to a request from the CLI.
#[derive(Serialize)]
pub(super) struct ControlResponseLine<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub response: Response<'a>,
}

#[derive(Serialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub(super) enum Response<'a> {
    Success {
        request_id: &'a str,
        response: Permission<'a>,
    },
    Error {
        request_id: &'a str,
        error: &'a str,
    },
}

/// The answer to `can_use_tool`.
#[derive(Serialize)]
#[serde(tag = "behavior", rename_all = "snake_case")]
pub(super) enum Permission<'a> {
    Allow {
        /// The input the tool runs with instead of the one requested.
        #[serde(rename = "updatedInput", skip_serializing_if = "Option::is_none")]
        updated_input: Option<Value>,
    },
    Deny {
        message: &'a str,
    },
}

/// The input of `AskUserQuestion`, as far as herder shows it: 1 to 4 questions, each with 2
/// to 4 options. herder answers with the same input plus `answers`, which maps each
/// question's text to the chosen option's label or the user's own text; a multi-select
/// question's labels are joined with `", "`.
#[derive(Debug, Deserialize)]
pub(super) struct AskUserQuestion {
    pub questions: Vec<Question>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Question {
    pub question: String,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub multi_select: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct QuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(value: &impl Serialize) -> String {
        serde_json::to_string(value).unwrap()
    }

    #[test]
    fn outgoing_lines_have_the_wire_shape() {
        assert_eq!(
            line(&UserLine {
                kind: "user",
                message: UserMessage {
                    role: "user",
                    content: "hi"
                },
                parent_tool_use_id: None,
                session_id: "",
                should_query: None,
                origin: Some(Origin { kind: "human" }),
            }),
            r#"{"type":"user","message":{"role":"user","content":"hi"},"parent_tool_use_id":null,"session_id":"","origin":{"kind":"human"}}"#
        );
        assert_eq!(
            line(&ControlRequestLine {
                kind: "control_request",
                request_id: "herder-2",
                request: Request::SetModel { model: "sonnet" },
            }),
            r#"{"type":"control_request","request_id":"herder-2","request":{"subtype":"set_model","model":"sonnet"}}"#
        );
        assert_eq!(
            line(&ControlResponseLine {
                kind: "control_response",
                response: Response::Success {
                    request_id: "r1",
                    response: Permission::Deny { message: "no" },
                },
            }),
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"r1","response":{"behavior":"deny","message":"no"}}}"#
        );
        assert_eq!(
            line(&Permission::Allow {
                updated_input: None
            }),
            r#"{"behavior":"allow"}"#
        );
        assert_eq!(
            line(&Permission::Allow {
                updated_input: Some(serde_json::json!({"questions": [], "answers": {}}))
            }),
            r#"{"behavior":"allow","updatedInput":{"answers":{},"questions":[]}}"#
        );
    }

    #[test]
    fn unknown_lines_and_blocks_parse_as_other() {
        let other: Incoming = serde_json::from_str(r#"{"type":"keep_alive","uuid":"u"}"#).unwrap();
        assert!(matches!(other, Incoming::Other));
        let assistant: Incoming = serde_json::from_str(
            r#"{"type":"assistant","message":{"content":[{"type":"server_tool_use","id":"x"}]},"parent_tool_use_id":null}"#,
        )
        .unwrap();
        let Incoming::Assistant(assistant) = assistant else {
            panic!("not an assistant line")
        };
        assert!(matches!(assistant.message.content[..], [Block::Other]));
        let request: Incoming = serde_json::from_str(
            r#"{"type":"control_request","request_id":"r","request":{"subtype":"hook_callback","callback_id":"c"}}"#,
        )
        .unwrap();
        assert!(matches!(
            request,
            Incoming::ControlRequest {
                request: ControlRequest::Other,
                ..
            }
        ));
    }
}
