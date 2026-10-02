//! The TLS WebSocket server clients connect to.
//!
//! Each connection runs: TLS, WebSocket upgrade, client hello, server hello, list messages,
//! then subscriptions and commands. A subscription replays the session's journal after the
//! client's cursor, then streams live events from the [`Hub`] without gaps or duplicates.

mod commands;
mod conn;
#[cfg(test)]
mod tests;
mod tls;

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use herder_protocol::{
    CommandBody, CommandResult, DeviceId, ErrorInfo, Event, HostId, Role, Seq, SessionHead,
    SessionId, UserId,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

pub use tls::{Tls, fingerprint};

use crate::hub::Hub;
use crate::session::SessionManager;
use commands::Commands;

/// Who a connection acts as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The user.
    pub user_id: UserId,
    /// The device the connection comes from.
    pub device_id: DeviceId,
    /// The user's role on this daemon.
    pub role: Role,
}

/// The sessions the server gives clients access to: [`SessionManager`] in the daemon.
pub trait Backend: Send + Sync + 'static {
    /// Every session with its latest seq, sent after hello.
    fn sessions(&self) -> impl Future<Output = anyhow::Result<Vec<SessionHead>>> + Send;

    /// Up to `limit` events of a session after `after_seq`, oldest first; for replay.
    fn read_since(
        &self,
        session_id: &SessionId,
        after_seq: Seq,
        limit: usize,
    ) -> impl Future<Output = anyhow::Result<Vec<Event>>> + Send;

    /// Applies a command for `identity`; an error rejects it with nothing changed. Each
    /// command id reaches here once, unless it was rejected.
    fn command(
        &self,
        identity: &Identity,
        command: CommandBody,
    ) -> impl Future<Output = Result<CommandResult, ErrorInfo>> + Send;
}

impl Backend for SessionManager {
    async fn sessions(&self) -> anyhow::Result<Vec<SessionHead>> {
        SessionManager::sessions(self).await
    }

    async fn read_since(
        &self,
        session_id: &SessionId,
        after_seq: Seq,
        limit: usize,
    ) -> anyhow::Result<Vec<Event>> {
        SessionManager::read_since(self, session_id, after_seq, limit).await
    }

    async fn command(
        &self,
        identity: &Identity,
        command: CommandBody,
    ) -> Result<CommandResult, ErrorInfo> {
        self.handle(identity.user_id.clone(), command).await
    }
}

/// The host this daemon runs on, as announced in the server hello.
#[derive(Clone, Debug)]
pub struct Host {
    /// Stable host id.
    pub id: HostId,
    /// Display name.
    pub name: String,
}

/// The WebSocket server.
pub struct Server<B> {
    shared: Arc<Shared<B>>,
}

struct Shared<B> {
    tls: Tls,
    hub: Arc<Hub>,
    backend: B,
    commands: Commands,
    host: Host,
}

impl<B: Backend> Server<B> {
    /// A server for `backend`, whose events reach clients through `hub`.
    pub fn new(tls: Tls, hub: Arc<Hub>, backend: B, host: Host) -> Self {
        Self {
            shared: Arc::new(Shared {
                tls,
                hub,
                backend,
                commands: Commands::default(),
                host,
            }),
        }
    }

    /// Accepts connections on `listener` until `shutdown`, which also closes every connection.
    pub async fn run(self, listener: TcpListener, shutdown: CancellationToken) {
        let hub = Arc::clone(&self.shared.hub);
        let flusher = tokio::spawn({
            let shutdown = shutdown.clone();
            async move { hub.run_flusher(shutdown).await }
        });
        loop {
            let accepted = tokio::select! {
                () = shutdown.cancelled() => break,
                accepted = listener.accept() => accepted,
            };
            match accepted {
                Ok((stream, peer)) => {
                    debug!(%peer, "connection accepted");
                    let shared = Arc::clone(&self.shared);
                    tokio::spawn(conn::run(stream, peer, shared, shutdown.child_token()));
                }
                Err(err) => {
                    // Usually out of file descriptors; back off instead of spinning.
                    warn!("cannot accept a connection: {err}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
        let _ = flusher.await;
    }
}
