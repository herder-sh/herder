//! JSON-RPC 2.0 over a [`Transport`]'s lines, as ACP frames it: one message per line.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use super::schema::Error;
use crate::transport::{Exit, Transport};

/// How long a closed stdin may take to end the agent before it is killed.
const EXIT_GRACE: Duration = Duration::from_secs(5);

/// One message from the agent.
#[derive(Debug)]
pub(super) enum Incoming {
    /// The answer to one of our requests.
    Response {
        id: u64,
        outcome: Result<Value, Error>,
    },
    /// A request from the agent, answered with [`Rpc::respond`].
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// A notification from the agent.
    Notification { method: String, params: Value },
}

/// A message as sent, before telling its kinds apart.
#[derive(Deserialize)]
struct Raw {
    id: Option<Value>,
    method: Option<String>,
    #[serde(default)]
    params: Value,
    result: Option<Value>,
    error: Option<Error>,
}

/// The JSON-RPC connection to the agent.
pub(super) struct Rpc {
    /// `None` once closed, or once the agent stopped reading.
    stdin: Option<mpsc::Sender<String>>,
    stdout: mpsc::Receiver<String>,
    exit: Option<oneshot::Receiver<Exit>>,
    next_id: u64,
}

/// `value` as JSON. Everything sent here has string keys only, which is all serializing to a
/// `Value` needs to succeed.
pub(super) fn to_value(value: impl Serialize) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

impl Rpc {
    pub(super) fn new(transport: Transport) -> Self {
        Self {
            stdin: Some(transport.stdin),
            stdout: transport.stdout,
            exit: Some(transport.exit),
            next_id: 1,
        }
    }

    /// Sends a request and returns its id.
    pub(super) async fn request(&mut self, method: &str, params: impl Serialize) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.write(
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": to_value(params)}),
        )
        .await;
        id
    }

    pub(super) async fn notify(&mut self, method: &str, params: impl Serialize) {
        self.write(json!({"jsonrpc": "2.0", "method": method, "params": to_value(params)}))
            .await;
    }

    pub(super) async fn respond(&mut self, id: Value, result: impl Serialize) {
        self.write(json!({"jsonrpc": "2.0", "id": id, "result": to_value(result)}))
            .await;
    }

    pub(super) async fn respond_error(&mut self, id: Value, error: Error) {
        self.write(json!({"jsonrpc": "2.0", "id": id, "error": to_value(error)}))
            .await;
    }

    async fn write(&mut self, message: Value) {
        if let Some(stdin) = &self.stdin
            && stdin.send(message.to_string()).await.is_err()
        {
            // The agent stopped reading; its end of output follows.
            self.stdin = None;
        }
    }

    /// The next message, or `None` at the agent's end of output. Lines that are not JSON-RPC
    /// messages, such as stray logging, are skipped.
    pub(super) async fn recv(&mut self) -> Option<Incoming> {
        loop {
            let line = self.stdout.recv().await?;
            let Ok(raw) = serde_json::from_str::<Raw>(&line) else {
                continue;
            };
            let message = match (raw.id, raw.method) {
                (Some(id), Some(method)) => Incoming::Request {
                    id,
                    method,
                    params: raw.params,
                },
                (None, Some(method)) => Incoming::Notification {
                    method,
                    params: raw.params,
                },
                (Some(id), None) => {
                    let Some(id) = id.as_u64() else { continue };
                    let outcome = match raw.error {
                        Some(error) => Err(error),
                        None => Ok(raw.result.unwrap_or(Value::Null)),
                    };
                    Incoming::Response { id, outcome }
                }
                (None, None) => continue,
            };
            return Some(message);
        }
    }

    /// How the agent ended, once its output has closed.
    pub(super) async fn exit(&mut self) -> Exit {
        match self.exit.take() {
            Some(exit) => exit
                .await
                .unwrap_or_else(|_| Exit::Failed("the agent's exit was never reported".into())),
            None => Exit::Failed("the agent's exit was already taken".into()),
        }
    }

    /// Closes stdin, which ends an ACP agent, and waits briefly for it to go; one that does not
    /// is killed when the transport drops.
    pub(super) async fn close(&mut self) {
        self.stdin = None;
        let drain = async {
            while self.stdout.recv().await.is_some() {}
            self.exit().await
        };
        // An agent that outlives the grace period is killed by dropping its exit.
        let _ = tokio::time::timeout(EXIT_GRACE, drain).await;
    }
}
