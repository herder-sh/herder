//! herder's MCP server: the task tools ([`herder_tasktools`]), served to each session's agent.
//!
//! # Transport
//!
//! The vendor CLI spawns `herder mcp --data-dir <dir> --session <id>` as a stdio MCP server
//! ([`McpServer`]); that shim connects to `<data_dir>/mcp.sock` and pipes its stdin and stdout
//! through, so every MCP message is handled here, in the daemon. Every CLI herder drives can
//! spawn a stdio server (Claude's `--mcp-config`, Codex's `mcp_servers`, ACP's `session/new`),
//! while HTTP support varies, and a Unix socket in the private data dir is reachable by this
//! user only, with no port to allocate or expose.
//!
//! # Session tokens
//!
//! Each time a session's CLI starts, [`Mcp::grant`] mints a random token and writes it to
//! `<data_dir>/mcp/<id>.token`, readable by this user only. The shim sends it with the session
//! id as its first line; the daemon checks it, answers, and from then on the connection belongs
//! to that session: every tool call on it is the session's, whatever its arguments say. A new
//! grant replaces the session's token, and [`Mcp::revoke`] removes it; either ends calls on
//! connections made with the old one. Neither the token nor anything secret appears on the
//! CLI's command line.
//!
//! # Protocol
//!
//! Newline-delimited JSON-RPC 2.0, as MCP's stdio transport: `initialize`, `ping`, `tools/list`
//! and `tools/call`; notifications are ignored. Calls run concurrently, since `wait_for` blocks,
//! and go to a [`ToolHandler`].

mod rpc;
mod shim;

use std::collections::HashMap;
use std::future::Future;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use herder_adapters::McpServer;
use herder_protocol::SessionId;
use herder_tasktools::{CallToolResult, ToolCall};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::data_dir::write_private;

pub use shim::{run_shim, shim};

/// File name of the socket in the data dir.
pub const SOCKET: &str = "mcp.sock";

/// Directory of the session tokens in the data dir.
const TOKENS: &str = "mcp";

/// Time the shim gets to authenticate.
const TIMEOUT: Duration = Duration::from_secs(5);

/// What the MCP server runs on.
pub struct Config {
    /// The daemon's data dir: the socket is `mcp.sock`, tokens go in `mcp/`.
    pub data_dir: PathBuf,
    /// The herder binary the CLI runs as the shim.
    pub herder: PathBuf,
}

/// Runs task tool calls.
pub trait ToolHandler: Send + Sync + 'static {
    /// Runs `call`, made by the agent of `caller`: the session whose token the connection
    /// presented, never one named in the arguments.
    fn call(&self, caller: SessionId, call: ToolCall) -> ToolFuture;
}

/// What [`ToolHandler::call`] returns.
pub type ToolFuture = Pin<Box<dyn Future<Output = CallToolResult> + Send>>;

/// The running MCP server: grants sessions their tokens and serves their shims.
pub struct Mcp {
    data_dir: PathBuf,
    herder: PathBuf,
    tools: Arc<dyn ToolHandler>,
    /// SHA-256 of each session's current token.
    grants: Mutex<HashMap<SessionId, [u8; 32]>>,
}

impl Mcp {
    /// Binds the socket, replacing a stale one, clears tokens left by an earlier daemon, and
    /// serves shims, running their calls on `tools`, until `shutdown`. The caller holds the
    /// data-dir lock.
    pub fn start(
        config: Config,
        tools: Arc<dyn ToolHandler>,
        shutdown: CancellationToken,
    ) -> Result<Arc<Self>> {
        let tokens = config.data_dir.join(TOKENS);
        match std::fs::remove_dir_all(&tokens) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("removing {}", tokens.display())),
        }
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&tokens)
            .with_context(|| format!("creating {}", tokens.display()))?;
        let path = config.data_dir.join(SOCKET);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("removing {}", path.display())),
        }
        let listener =
            UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
        let mcp = Arc::new(Self {
            data_dir: config.data_dir,
            herder: config.herder,
            tools,
            grants: Mutex::new(HashMap::new()),
        });
        tokio::spawn(serve(listener, Arc::clone(&mcp), shutdown));
        Ok(mcp)
    }

    /// Mints a new token for `session_id`, replacing any earlier one, and returns the server
    /// its CLI should run.
    pub fn grant(&self, session_id: &SessionId) -> Result<McpServer> {
        let path = token_path(&self.data_dir, session_id)?;
        let mut bytes = [0u8; 32];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| anyhow::anyhow!("the system random number generator failed"))?;
        let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let (dir, name) = split(&path)?;
        write_private(dir, name, token.as_bytes())?;
        self.lock().insert(session_id.clone(), digest(&token));
        Ok(McpServer {
            command: self.herder.clone(),
            args: vec![
                "mcp".into(),
                "--data-dir".into(),
                self.data_dir.to_string_lossy().into_owned(),
                "--session".into(),
                session_id.to_string(),
            ],
        })
    }

    /// Withdraws `session_id`'s token; its connections take no further calls.
    pub fn revoke(&self, session_id: &SessionId) {
        self.lock().remove(session_id);
        if let Ok(path) = token_path(&self.data_dir, session_id)
            && let Err(err) = std::fs::remove_file(&path)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            warn!("cannot remove {}: {err}", path.display());
        }
    }

    /// Whether `token`'s digest is `session_id`'s current grant.
    fn holds(&self, session_id: &SessionId, token: &[u8; 32]) -> bool {
        self.lock().get(session_id) == Some(token)
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<SessionId, [u8; 32]>> {
        // The map is valid after any panic: every update is a single insert or remove.
        self.grants.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Only the digest is kept and compared, so comparing leaks nothing about the token.
fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// `<data_dir>/mcp/<id>.token`; ids are ULIDs, and anything else is refused so an id can never
/// name a path elsewhere.
fn token_path(data_dir: &Path, session_id: &SessionId) -> Result<PathBuf> {
    let id = session_id.as_str();
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        bail!("invalid session id {id:?}");
    }
    Ok(data_dir.join(TOKENS).join(format!("{id}.token")))
}

fn split(path: &Path) -> Result<(&Path, &str)> {
    let dir = path.parent().context("token path has no directory")?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("token path has no file name")?;
    Ok((dir, name))
}

/// The shim's first line.
#[derive(Debug, Serialize, Deserialize)]
struct Hello {
    session_id: SessionId,
    token: String,
}

/// The daemon's answer to [`Hello`].
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Welcome {
    /// The connection now belongs to the session.
    Ok,
    /// Refused; the daemon closes the connection.
    Refused { message: String },
}

async fn serve(listener: UnixListener, mcp: Arc<Mcp>, shutdown: CancellationToken) {
    loop {
        let accepted = tokio::select! {
            () = shutdown.cancelled() => return,
            accepted = listener.accept() => accepted,
        };
        match accepted {
            Ok((stream, _)) => {
                let (mcp, shutdown) = (Arc::clone(&mcp), shutdown.clone());
                tokio::spawn(async move {
                    if let Err(err) = rpc::connection(stream, mcp, shutdown).await {
                        debug!("MCP connection failed: {err:#}");
                    }
                });
            }
            Err(err) => {
                warn!("cannot accept an MCP connection: {err}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests;
