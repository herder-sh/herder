//! One shim connection: the token handshake, then MCP's JSON-RPC.

use std::sync::Arc;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use herder_protocol::SessionId;
use herder_tasktools::{Tool, ToolCall, tools_list};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, LinesCodec};
use tokio_util::sync::CancellationToken;

use super::{Hello, Mcp, TIMEOUT, Welcome, digest};

/// Longest message accepted, in bytes.
const MAX_MESSAGE: usize = 4 * 1024 * 1024;

/// MCP versions this server speaks, newest first; all serve the same tools.
const VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// What a client that asks for a version not in [`VERSIONS`] gets: the first with
/// `outputSchema` and `structuredContent`.
const DEFAULT_VERSION: &str = "2025-06-18";

/// The `initialize` result's `instructions`, which clients such as Claude Code show the model
/// up front, so it delegates through `spawn` before trying a built-in subagent that the Claude
/// adapter's PreToolUse hook would deny, and proves its work with `publish` unasked.
const INSTRUCTIONS: &str = "herder runs this session. To delegate work that edits files, \
builds, or opens pull requests, call `spawn`: one child per independent piece of work, each \
with a complete prompt, then collect results with `wait_for`. Use `send_session` for \
follow-ups to an existing child. Never delegate such work to a built-in subagent with its own \
worktree (Claude Code's Agent tool with `isolation: \"worktree\"`): it is denied, and its \
work would not show in herder. Built-in subagents for read-only lookups are fine. In a child \
session, where `spawn` is refused, do the work yourself.

The user often reads this session from a phone or another machine, so a local file path or \
localhost URL is useless to them. Whenever your work has an observable result, prove it \
without being asked: publish a screenshot or screen recording of a UI change, the captured \
output of a CLI or API change, or the test run of a refactor with `publish`, before your \
final reply. It shows the artifact in this thread and returns a public link; put that link \
in your reply as a Markdown link, never a local path.";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
/// The connection's token was replaced or withdrawn; the session's CLI restarted or stopped.
const REVOKED: i64 = -32001;

/// Serves one shim until it disconnects or `shutdown`.
pub(super) async fn connection(
    stream: UnixStream,
    mcp: Arc<Mcp>,
    shutdown: CancellationToken,
) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = FramedRead::new(read, LinesCodec::new_with_max_length(MAX_MESSAGE));
    let hello = tokio::time::timeout(TIMEOUT, lines.next())
        .await
        .context("no hello in time")?
        .context("closed before its hello")??;
    let hello: Hello = match serde_json::from_str(&hello) {
        Ok(hello) => hello,
        Err(err) => return refuse(&mut write, format!("invalid hello: {err}")).await,
    };
    let token = digest(&hello.token);
    if !mcp.holds(&hello.session_id, &token) {
        let message = format!("no valid herder MCP token for session {}", hello.session_id);
        return refuse(&mut write, message).await;
    }
    send(&mut write, &Welcome::Ok).await?;

    // Ends the calls still running when the shim goes.
    let done = shutdown.child_token();
    let _done = done.clone().drop_guard();
    let (out, mut outgoing) = mpsc::unbounded_channel::<Value>();
    tokio::spawn(async move {
        while let Some(message) = outgoing.recv().await {
            if send(&mut write, &message).await.is_err() {
                return;
            }
        }
    });
    let caller = Caller {
        session_id: hello.session_id,
        token,
        mcp,
        out,
        done: done.clone(),
    };
    loop {
        let line = tokio::select! {
            () = done.cancelled() => return Ok(()),
            line = lines.next() => match line {
                Some(line) => line?,
                None => return Ok(()),
            },
        };
        caller.message(&line);
    }
}

async fn refuse(write: &mut OwnedWriteHalf, message: String) -> Result<()> {
    send(write, &Welcome::Refused { message }).await
}

async fn send(write: &mut OwnedWriteHalf, message: &impl serde::Serialize) -> Result<()> {
    let mut line = serde_json::to_string(message)?;
    line.push('\n');
    write.write_all(line.as_bytes()).await?;
    Ok(())
}

/// The session a connection belongs to, and where its answers go.
struct Caller {
    session_id: SessionId,
    token: [u8; 32],
    mcp: Arc<Mcp>,
    out: mpsc::UnboundedSender<Value>,
    done: CancellationToken,
}

#[derive(Deserialize)]
struct CallParams {
    name: String,
    #[serde(default)]
    arguments: Option<Value>,
}

impl Caller {
    /// Handles one incoming message; a tool call runs on its own task and answers when done.
    fn message(&self, line: &str) {
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(err) => return self.error(Value::Null, PARSE_ERROR, err.to_string()),
        };
        let Some(object) = message.as_object() else {
            return self.error(Value::Null, INVALID_REQUEST, "expected a JSON-RPC object");
        };
        let (Some(method), Some(id)) = (
            object.get("method").and_then(Value::as_str),
            object.get("id").cloned(),
        ) else {
            // A notification, or a response to a request this server never sends.
            return;
        };
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        match method {
            "initialize" => self.reply(id, initialize(&params)),
            "ping" => self.reply(id, json!({})),
            "tools/list" => match serde_json::to_value(tools_list()) {
                Ok(list) => self.reply(id, list),
                Err(err) => self.error(id, INTERNAL_ERROR, err.to_string()),
            },
            "tools/call" => self.call(id, params),
            _ => self.error(id, METHOD_NOT_FOUND, format!("unknown method {method}")),
        }
    }

    fn call(&self, id: Value, params: Value) {
        let params: CallParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(err) => return self.error(id, INVALID_PARAMS, err.to_string()),
        };
        let Some(tool) = Tool::from_name(&params.name) else {
            return self.error(id, INVALID_PARAMS, format!("unknown tool {}", params.name));
        };
        if !self.mcp.holds(&self.session_id, &self.token) {
            let message = "this herder MCP connection is no longer valid: the session restarted";
            return self.error(id, REVOKED, message);
        }
        let result = match ToolCall::parse(tool, params.arguments) {
            Ok(call) => self.mcp.tools.call(self.session_id.clone(), call),
            Err(error) => Box::pin(std::future::ready(error.into())),
        };
        let (out, done) = (self.out.clone(), self.done.clone());
        tokio::spawn(async move {
            let result = tokio::select! {
                () = done.cancelled() => return,
                result = result => result,
            };
            let message = match serde_json::to_value(result) {
                Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                Err(err) => error_message(id, INTERNAL_ERROR, err.to_string()),
            };
            let _ = out.send(message);
        });
    }

    fn reply(&self, id: Value, result: Value) {
        let _ = self
            .out
            .send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    fn error(&self, id: Value, code: i64, message: impl Into<String>) {
        let _ = self.out.send(error_message(id, code, message.into()));
    }
}

fn error_message(id: Value, code: i64, message: String) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// The `initialize` result: the client's protocol version when this server speaks it.
fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = VERSIONS
        .into_iter()
        .find(|version| Some(*version) == asked)
        .unwrap_or(DEFAULT_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "herder", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}
