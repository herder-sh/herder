use std::sync::Mutex as StdMutex;

use herder_tasktools::{CallToolResult, tools_list};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio::task::JoinHandle;

use super::*;

struct Daemon {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    mcp: Arc<Mcp>,
    shutdown: CancellationToken,
}

fn daemon(tools: Arc<dyn ToolHandler>) -> Daemon {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_owned();
    let shutdown = CancellationToken::new();
    let config = Config {
        data_dir: dir.clone(),
        herder: PathBuf::from("/usr/bin/herder"),
        tools,
    };
    let mcp = Mcp::start(config, shutdown.clone()).unwrap();
    Daemon {
        _tmp: tmp,
        dir,
        mcp,
        shutdown,
    }
}

/// An MCP client talking to a shim, as the vendor CLI does.
struct Client {
    input: DuplexStream,
    output: BufReader<DuplexStream>,
    shim: JoinHandle<Result<()>>,
    next_id: u64,
}

fn connect(daemon: &Daemon, session: &str) -> Client {
    let (input, shim_input) = tokio::io::duplex(1 << 16);
    let (shim_output, output) = tokio::io::duplex(1 << 16);
    let dir = daemon.dir.clone();
    let session = SessionId::new(session);
    let shim = tokio::spawn(async move { shim(&dir, &session, shim_input, shim_output).await });
    Client {
        input,
        output: BufReader::new(output),
        shim,
        next_id: 0,
    }
}

impl Client {
    async fn write(&mut self, message: &Value) {
        let mut line = message.to_string();
        line.push('\n');
        self.input.write_all(line.as_bytes()).await.unwrap();
    }

    async fn read(&mut self) -> Value {
        let mut line = String::new();
        self.output.read_line(&mut line).await.unwrap();
        assert!(!line.is_empty(), "the shim closed its output");
        serde_json::from_str(&line).unwrap()
    }

    /// Sends a request and returns its whole response.
    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await;
        let response = self.read().await;
        assert_eq!(response["id"], id, "{response}");
        response
    }

    async fn call(&mut self, name: &str, arguments: Value) -> CallToolResult {
        let response = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await;
        serde_json::from_value(response["result"].clone()).unwrap()
    }

    /// Closes the client's end and waits for the shim to exit.
    async fn close(self) -> Result<()> {
        drop(self.input);
        self.shim.await.unwrap()
    }
}

fn error_code(result: &CallToolResult) -> String {
    assert!(result.is_error, "{result:?}");
    let text: Value = serde_json::from_str(&result.content[0].text).unwrap();
    text["code"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn a_session_lists_and_calls_the_tools_through_the_shim() {
    let daemon = daemon(Arc::new(Unimplemented));
    let session = SessionId::new("01J0SESSION");
    let server = daemon.mcp.grant(&session).unwrap();
    assert_eq!(server.command, PathBuf::from("/usr/bin/herder"));
    assert_eq!(
        server.args,
        [
            "mcp",
            "--data-dir",
            daemon.dir.to_str().unwrap(),
            "--session",
            "01J0SESSION"
        ]
    );
    let mut client = connect(&daemon, "01J0SESSION");

    let init = client
        .request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" },
            }),
        )
        .await;
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "herder");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    client
        .write(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
        .await;

    let list = client.request("tools/list", json!({})).await;
    assert_eq!(
        list["result"],
        serde_json::to_value(tools_list()).unwrap(),
        "tools/list serves herder-tasktools as is"
    );
    let status = client.call("status", json!({})).await;
    assert_eq!(
        status,
        CallToolResult::success(&StatusOutput {
            children: Vec::new()
        })
        .unwrap()
    );
    let spawn = client
        .call("spawn", json!({ "task": "t", "prompt": "p" }))
        .await;
    assert_eq!(error_code(&spawn), "internal");
    assert!(
        spawn.content[0].text.contains("not implemented"),
        "{spawn:?}"
    );

    let ping = client.request("ping", Value::Null).await;
    assert_eq!(ping["result"], json!({}));
    client.close().await.unwrap();
}

#[tokio::test]
async fn protocol_errors_are_json_rpc_errors_and_bad_arguments_tool_errors() {
    let daemon = daemon(Arc::new(Unimplemented));
    daemon.mcp.grant(&SessionId::new("S1")).unwrap();
    let mut client = connect(&daemon, "S1");

    let init = client
        .request("initialize", json!({ "protocolVersion": "1999-01-01" }))
        .await;
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    let unknown = client
        .request("tools/call", json!({ "name": "rm_rf", "arguments": {} }))
        .await;
    assert_eq!(unknown["error"]["code"], -32602, "{unknown}");
    let method = client.request("resources/list", json!({})).await;
    assert_eq!(method["error"]["code"], -32601, "{method}");
    let bad = client.call("status", json!({ "children": "all" })).await;
    assert_eq!(error_code(&bad), "invalid_arguments");
    client.write(&json!("not an object")).await;
    assert_eq!(client.read().await["error"]["code"], -32600);
    client.input.write_all(b"{not json\n").await.unwrap();
    assert_eq!(client.read().await["error"]["code"], -32700);
    client.close().await.unwrap();
}

#[tokio::test]
async fn a_wrong_or_missing_token_is_refused() {
    let daemon = daemon(Arc::new(Unimplemented));
    let session = SessionId::new("S1");
    daemon.mcp.grant(&session).unwrap();
    let path = token_path(&daemon.dir, &session).unwrap();
    std::fs::write(&path, "00".repeat(32)).unwrap();
    let err = connect(&daemon, "S1").close().await.unwrap_err();
    assert!(err.to_string().contains("refused"), "{err:#}");

    // A token file the daemon never granted, for another session, fares no better.
    let other = token_path(&daemon.dir, &SessionId::new("S2")).unwrap();
    std::fs::write(&other, "00".repeat(32)).unwrap();
    let err = connect(&daemon, "S2").close().await.unwrap_err();
    assert!(err.to_string().contains("refused"), "{err:#}");

    let err = connect(&daemon, "S3").close().await.unwrap_err();
    assert!(err.to_string().contains("reading"), "{err:#}");
    let err = connect(&daemon, "../S1").close().await.unwrap_err();
    assert!(err.to_string().contains("invalid session id"), "{err:#}");
}

#[tokio::test]
async fn a_new_grant_or_a_revoke_ends_calls_on_the_old_token() {
    let daemon = daemon(Arc::new(Unimplemented));
    let session = SessionId::new("S1");
    daemon.mcp.grant(&session).unwrap();
    let mut old = connect(&daemon, "S1");
    assert!(!old.call("status", json!({})).await.is_error);

    daemon.mcp.grant(&session).unwrap();
    let response = old
        .request("tools/call", json!({ "name": "status", "arguments": {} }))
        .await;
    assert_eq!(response["error"]["code"], -32001, "{response}");
    let mut new = connect(&daemon, "S1");
    assert!(!new.call("status", json!({})).await.is_error);

    daemon.mcp.revoke(&session);
    let response = new
        .request("tools/call", json!({ "name": "status", "arguments": {} }))
        .await;
    assert_eq!(response["error"]["code"], -32001, "{response}");
    assert!(!token_path(&daemon.dir, &session).unwrap().exists());
    old.close().await.unwrap();
    new.close().await.unwrap();
}

/// Records who each call was attributed to.
#[derive(Default)]
struct Recorder(StdMutex<Vec<(SessionId, ToolCall)>>);

impl ToolHandler for Arc<Recorder> {
    fn call(&self, caller: SessionId, call: ToolCall) -> ToolFuture {
        self.0.lock().unwrap().push((caller, call));
        Box::pin(std::future::ready(
            CallToolResult::success(&json!({})).unwrap(),
        ))
    }
}

#[tokio::test]
async fn calls_are_attributed_to_the_tokens_session_never_to_arguments() {
    let recorder = Arc::new(Recorder::default());
    let daemon = daemon(Arc::new(Arc::clone(&recorder)));
    daemon.mcp.grant(&SessionId::new("PRIMARY")).unwrap();
    let mut client = connect(&daemon, "PRIMARY");
    client
        .call("send", json!({ "child": "OTHER", "text": "hi" }))
        .await;
    let calls = recorder.0.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, SessionId::new("PRIMARY"));
    client.close().await.unwrap();
}

/// Never answers, like a `wait_for` with nothing to report.
struct Blocks;

impl ToolHandler for Blocks {
    fn call(&self, _caller: SessionId, _call: ToolCall) -> ToolFuture {
        Box::pin(std::future::pending())
    }
}

#[tokio::test]
async fn a_blocked_call_does_not_hold_up_the_others_or_the_shims_exit() {
    let daemon = daemon(Arc::new(Blocks));
    daemon.mcp.grant(&SessionId::new("S1")).unwrap();
    let mut client = connect(&daemon, "S1");
    client
        .write(&json!({
            "jsonrpc": "2.0", "id": "wait", "method": "tools/call",
            "params": { "name": "wait_for", "arguments": { "timeout_secs": 600 } },
        }))
        .await;
    let ping = client.request("ping", json!({})).await;
    assert_eq!(ping["result"], json!({}));
    client.close().await.unwrap();
    daemon.shutdown.cancel();
}
