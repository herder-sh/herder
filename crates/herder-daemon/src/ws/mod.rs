//! The TLS WebSocket server clients connect to.
//!
//! Each connection runs: TLS with a device certificate, WebSocket upgrade, client hello,
//! authentication, server hello, list messages, then subscriptions and commands. A subscription replays the session's journal after the
//! client's cursor, then streams live events from the [`Hub`] without gaps or duplicates.

mod commands;
mod conn;
#[cfg(test)]
mod tests;
mod tls;

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use herder_protocol::{
    Account, CommandBody, CommandResult, DeviceId, ErrorInfo, Event, HostId, Role, Seq,
    SessionHead, SessionId, TerminalPurpose, UserId,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

pub use tls::{Tls, fingerprint};

use crate::auth::Auth;
use crate::hub::Hub;
use crate::hub::Outbox;
use crate::login::{Logins, NewAccount};
use crate::session::SessionManager;
use crate::terminal::Terminals;
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

    /// Every account sessions may run on, sent after the sessions.
    fn accounts(&self) -> Vec<Account>;

    /// Asks for fresh account usage, as a client opened; changes go out through the hub.
    fn refresh_usage(&self);

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

    /// The worktree of a session that is not read-only, for a terminal to run in.
    fn worktree(
        &self,
        session_id: &SessionId,
    ) -> impl Future<Output = Result<PathBuf, ErrorInfo>> + Send;
}

impl Backend for SessionManager {
    async fn sessions(&self) -> anyhow::Result<Vec<SessionHead>> {
        SessionManager::sessions(self).await
    }

    fn accounts(&self) -> Vec<Account> {
        SessionManager::accounts(self)
    }

    fn refresh_usage(&self) {
        SessionManager::refresh_usage(self);
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

    async fn worktree(&self, session_id: &SessionId) -> Result<PathBuf, ErrorInfo> {
        SessionManager::worktree(self, session_id).await
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
    auth: Arc<Auth>,
    hub: Arc<Hub>,
    backend: B,
    terminals: Terminals,
    logins: Logins,
    commands: Commands,
    host: Host,
}

impl<B: Backend> Shared<B> {
    /// Applies a command from the connection with `outbox`: terminal commands here, as they act
    /// on the connection, the rest in the backend.
    async fn apply(
        &self,
        identity: &Identity,
        outbox: &Arc<Outbox>,
        command: CommandBody,
    ) -> Result<CommandResult, ErrorInfo> {
        let terminals = &self.terminals;
        match command {
            CommandBody::OpenTerminal {
                session_id,
                cols,
                rows,
            } => {
                let cwd = self.backend.worktree(&session_id).await?;
                let terminal_id = terminals.open(session_id, &cwd, cols, rows, outbox)?;
                return Ok(CommandResult::TerminalOpened { terminal_id });
            }
            CommandBody::AddAccount {
                account_id,
                provider,
                label,
                config_dir,
                cols,
                rows,
            } => {
                let logging_in: Vec<_> = terminals
                    .list()
                    .into_iter()
                    .filter_map(|terminal| match terminal.purpose {
                        TerminalPurpose::Login { account_id } => Some(account_id),
                        TerminalPurpose::Shell { .. } => None,
                    })
                    .collect();
                let account = NewAccount {
                    account_id: &account_id,
                    provider: &provider,
                    label: label.as_deref(),
                    config_dir: config_dir.as_deref(),
                };
                let login = self.logins.start(&account, &logging_in)?;
                let pending = login.pending;
                let terminal_id = terminals.open_login(
                    account_id,
                    login.command,
                    cols,
                    rows,
                    outbox,
                    Box::new(move |exit_code| pending.finish(exit_code)),
                )?;
                return Ok(CommandResult::TerminalOpened { terminal_id });
            }
            CommandBody::AttachTerminal { terminal_id } => {
                terminals.attach(&terminal_id, outbox)?
            }
            CommandBody::DetachTerminal { terminal_id } => {
                terminals.detach(&terminal_id, outbox)?
            }
            CommandBody::ResizeTerminal {
                terminal_id,
                cols,
                rows,
            } => terminals.resize(&terminal_id, outbox, cols, rows)?,
            CommandBody::TerminalInput { terminal_id, data } => {
                terminals.input(&terminal_id, outbox, data.0).await?;
            }
            command => return self.backend.command(identity, command).await,
        }
        Ok(CommandResult::Applied)
    }
}

impl<B: Backend> Server<B> {
    /// A server for `backend` and `terminals`, adding accounts through `logins`, whose events
    /// reach clients through `hub`; `auth` decides who may connect.
    pub fn new(
        tls: Tls,
        auth: Arc<Auth>,
        hub: Arc<Hub>,
        backend: B,
        terminals: Terminals,
        logins: Logins,
        host: Host,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                tls,
                auth,
                hub,
                backend,
                terminals,
                logins,
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
