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
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use herder_protocol::{
    Account, AccountId, CommandBody, CommandId, CommandResult, DeviceId, ErrorCode, ErrorInfo,
    Event, HostId, Role, Seq, SessionHead, SessionId, TerminalPurpose, UserId,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

pub(crate) use conn::handshake;
pub use tls::{Tls, fingerprint};

/// A connection after the TLS and WebSocket handshakes.
pub(crate) type Ws =
    tokio_tungstenite::WebSocketStream<tokio_rustls::server::TlsStream<tokio::net::TcpStream>>;

use crate::auth::control::addresses;
use crate::auth::{self, Auth, PAIRING_TTL};
use crate::hub::Hub;
use crate::hub::Outbox;
use crate::listen;
use crate::login::{Login, Logins, NewAccount};
use crate::mcp::{Control, ControlFuture, Overview};
use crate::providers::Providers;
use crate::session::SessionManager;
use crate::settings::Settings;
use crate::terminal::{LoginHooks, OnExit, Terminals};
use crate::vault::Link;
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

    /// Pulls the skill library, as a client opened; changes go out through the hub.
    fn refresh_skills(&self) {}

    /// Up to `limit` events of a session after `after_seq`, oldest first; for replay.
    fn read_since(
        &self,
        session_id: &SessionId,
        after_seq: Seq,
        limit: usize,
    ) -> impl Future<Output = anyhow::Result<Vec<Event>>> + Send;

    /// Applies `user_id`'s command `command_id`; an error rejects it with nothing changed.
    /// Each command id reaches here once per daemon, unless it was rejected; an id accepted
    /// before a restart must be answered with its first result, not applied again.
    fn command(
        &self,
        user_id: &UserId,
        command_id: &CommandId,
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

    fn refresh_skills(&self) {
        SessionManager::refresh_skills(self);
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
        user_id: &UserId,
        command_id: &CommandId,
        command: CommandBody,
    ) -> Result<CommandResult, ErrorInfo> {
        self.handle_once(user_id.clone(), command_id.clone(), command)
            .await
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
    /// The host's link to its vault, which answers the vault link commands; a vault has none.
    link: OnceLock<Arc<Link>>,
    /// The addresses the server listens on, set once it does; `pair_device` advertises them.
    listen: OnceLock<Vec<SocketAddr>>,
    /// The daemon's settings, which answer the settings commands, once set.
    settings: OnceLock<Arc<Settings>>,
    /// The host's provider CLIs, which answer install and the providers list, once set.
    providers: OnceLock<Arc<Providers>>,
}

impl<B: Backend> Shared<B> {
    /// Applies command `command_id` from the connection with `outbox`: terminal commands and
    /// pairing a device here, as they act on the connection or its device, the rest in
    /// [`Shared::run`].
    async fn apply(
        &self,
        identity: &Identity,
        outbox: &Arc<Outbox>,
        command_id: &CommandId,
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
                let account = NewAccount {
                    account_id: &account_id,
                    provider: &provider,
                    label: label.as_deref(),
                    config_dir: config_dir.as_deref(),
                };
                let login = self.logins.start(&account, &logging_in(terminals))?;
                return open_login(terminals, account_id, login, cols, rows, outbox);
            }
            CommandBody::LogInAccount {
                account_id,
                cols,
                rows,
            } => {
                let login = self.logins.again(&account_id, &logging_in(terminals))?;
                return open_login(terminals, account_id, login, cols, rows, outbox);
            }
            CommandBody::InstallProvider {
                provider,
                cols,
                rows,
            } => {
                let Some(providers) = self.providers.get() else {
                    return Err(ErrorInfo {
                        code: ErrorCode::Unsupported,
                        message: "this daemon does not install providers".to_owned(),
                    });
                };
                let command = providers.command(&provider)?;
                let watching = providers.clone();
                let name = provider.as_str().to_owned();
                let on_exit: OnExit = Box::new(move |exit_code| {
                    watching.refresh();
                    match exit_code {
                        Some(0) => format!("{name} install finished"),
                        Some(code) => format!("{name} install exited {code}"),
                        None => format!("{name} install ended"),
                    }
                });
                return open_install(terminals, provider, command, cols, rows, outbox, on_exit);
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
            CommandBody::PairDevice => return self.pair_device(identity),
            command => return self.run(&identity.user_id, command_id, command).await,
        }
        Ok(CommandResult::Applied)
    }

    /// Applies `user_id`'s command `command_id` that needs no connection: every command but
    /// the terminal ones and pairing a device.
    async fn run(
        &self,
        user_id: &UserId,
        command_id: &CommandId,
        command: CommandBody,
    ) -> Result<CommandResult, ErrorInfo> {
        match command {
            CommandBody::SetAccountSettings {
                account_id,
                label,
                config_dir,
            } => {
                self.logins
                    .set_settings(&account_id, &label, config_dir.as_deref())
                    .await?;
                Ok(CommandResult::Applied)
            }
            command @ (CommandBody::GetVaultLink
            | CommandBody::LinkVault { .. }
            | CommandBody::UnlinkVault)
                if let Some(link) = self.link.get() =>
            {
                link.command(command).await
            }
            command @ (CommandBody::GetSettings
            | CommandBody::SetSettings { .. }
            | CommandBody::SetResourceLimits { .. }
            | CommandBody::RestartDaemon) => match self.settings.get() {
                Some(settings) => settings.command(command).await,
                None => Err(ErrorInfo {
                    code: ErrorCode::Unsupported,
                    message: "this daemon's settings are not changed from a client".to_owned(),
                }),
            },
            command => self.backend.command(user_id, command_id, command).await,
        }
    }

    /// A one-time code that pairs another device as `identity`'s user, with this daemon's
    /// own addresses rather than the one the caller reached it on.
    fn pair_device(&self, identity: &Identity) -> Result<CommandResult, ErrorInfo> {
        let Some(listen) = self.listen.get() else {
            return Err(ErrorInfo {
                code: ErrorCode::Internal,
                message: "the daemon is not listening yet".to_owned(),
            });
        };
        let pairing = self
            .auth
            .mint_for(&identity.user_id, PAIRING_TTL)
            .map_err(|err| ErrorInfo {
                code: ErrorCode::Internal,
                message: format!("{err:#}"),
            })?;
        Ok(CommandResult::DevicePairing {
            code: pairing.code,
            fingerprint: self.tls.fingerprint().to_owned(),
            addresses: addresses(listen),
            expires_at: pairing.expires_at,
        })
    }
}

/// The accounts with a login running in `terminals`.
fn logging_in(terminals: &Terminals) -> Vec<AccountId> {
    terminals
        .list()
        .into_iter()
        .filter_map(|terminal| match terminal.purpose {
            TerminalPurpose::Login { account_id } => Some(account_id),
            TerminalPurpose::Shell { .. } | TerminalPurpose::Install { .. } => None,
        })
        .collect()
}

/// Runs `login`, of `account_id`, in a login terminal of `cols` by `rows` attached to `outbox`.
fn open_login(
    terminals: &Terminals,
    account_id: AccountId,
    login: Login,
    cols: u16,
    rows: u16,
    outbox: &Arc<Outbox>,
) -> Result<CommandResult, ErrorInfo> {
    let pending = login.pending;
    let hooks = LoginHooks {
        done: Box::new(pending.check()),
        on_exit: Box::new(move |exit_code| pending.finish(exit_code)),
    };
    let terminal_id = terminals.open_login(account_id, login.command, cols, rows, outbox, hooks)?;
    Ok(CommandResult::TerminalOpened { terminal_id })
}

/// Runs `command`, the installer of `provider`, in a terminal of `cols` by `rows`.
fn open_install(
    terminals: &Terminals,
    provider: herder_protocol::Provider,
    command: portable_pty::CommandBuilder,
    cols: u16,
    rows: u16,
    outbox: &Arc<Outbox>,
    on_exit: OnExit,
) -> Result<CommandResult, ErrorInfo> {
    let terminal_id = terminals.open_install(provider, command, cols, rows, outbox, on_exit)?;
    Ok(CommandResult::TerminalOpened { terminal_id })
}

/// The server's [`Control`], for agents; weak, as the session manager that holds it is the
/// server's backend.
struct AgentControl<B>(Weak<Shared<B>>);

impl<B: Backend> AgentControl<B> {
    fn shared(&self) -> Result<Arc<Shared<B>>, ErrorInfo> {
        self.0.upgrade().ok_or_else(|| ErrorInfo {
            code: ErrorCode::Internal,
            message: "the daemon is shutting down".to_owned(),
        })
    }
}

impl<B: Backend> Control for AgentControl<B> {
    fn overview(&self) -> ControlFuture<Overview> {
        let shared = self.shared();
        Box::pin(async move {
            let shared = shared?;
            let sessions = shared.backend.sessions().await.map_err(|err| ErrorInfo {
                code: ErrorCode::Internal,
                message: format!("{err:#}"),
            })?;
            Ok(Overview {
                sessions,
                projects: shared.hub.projects(),
                accounts: shared.backend.accounts(),
            })
        })
    }

    fn command(&self, user_id: UserId, command: CommandBody) -> ControlFuture<CommandResult> {
        let shared = self.shared();
        Box::pin(async move {
            let shared = shared?;
            let connection = matches!(
                command,
                CommandBody::OpenTerminal { .. }
                    | CommandBody::AttachTerminal { .. }
                    | CommandBody::DetachTerminal { .. }
                    | CommandBody::ResizeTerminal { .. }
                    | CommandBody::TerminalInput { .. }
                    | CommandBody::AddAccount { .. }
                    | CommandBody::LogInAccount { .. }
                    | CommandBody::InstallProvider { .. }
            );
            let pairing = matches!(
                command,
                CommandBody::PairDevice | CommandBody::PairVaultHost { .. }
            );
            if connection || pairing {
                return Err(ErrorInfo {
                    code: ErrorCode::Forbidden,
                    message: "terminals, logins, installs and pairing codes are never run for \
                              an agent; ask the user to do this in a herder app"
                        .to_owned(),
                });
            }
            let Some(role) = shared.auth.role(&user_id) else {
                return Err(ErrorInfo {
                    code: ErrorCode::Forbidden,
                    message: "the user who started this session is no longer paired with this \
                              daemon"
                        .to_owned(),
                });
            };
            auth::authorize(role, &command)?;
            let command_id = CommandId::new(ulid::Ulid::new().to_string());
            shared.run(&user_id, &command_id, command).await
        })
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
                link: OnceLock::new(),
                listen: OnceLock::new(),
                settings: OnceLock::new(),
                providers: OnceLock::new(),
            }),
        }
    }

    /// The commands this server runs for clients, for agents to run through MCP.
    pub fn control(&self) -> Arc<dyn Control> {
        Arc::new(AgentControl(Arc::downgrade(&self.shared)))
    }

    /// Answers install and the providers list with `providers`; once per server.
    pub fn manage_providers(&self, providers: Providers) -> anyhow::Result<()> {
        self.shared
            .providers
            .set(Arc::new(providers))
            .map_err(|_| anyhow::anyhow!("the providers are managed already"))
    }

    /// Answers the settings commands with `settings`; once per server.
    pub fn manage_settings(&self, settings: Arc<Settings>) -> anyhow::Result<()> {
        self.shared
            .settings
            .set(settings)
            .map_err(|_| anyhow::anyhow!("the settings are managed already"))
    }

    /// Answers the vault link commands with `link`; once per server.
    pub fn link_vault(&self, link: Arc<Link>) -> anyhow::Result<()> {
        self.shared
            .link
            .set(link)
            .map_err(|_| anyhow::anyhow!("the vault is linked already"))
    }

    /// Takes `listen` for the addresses clients reach the server on; [`Server::run`] does it
    /// itself, the vault, which accepts connections for the server, before it serves one.
    pub(crate) fn listening_on(&self, listen: Vec<SocketAddr>) {
        let _ = self.shared.listen.set(listen);
    }

    /// Serves one client whose handshakes are done and whose first text frame, `first`, was
    /// already read: for the vault, whose port takes hosts and clients alike. The caller runs
    /// the hub's flusher.
    pub(crate) async fn serve(
        &self,
        ws: Ws,
        device: String,
        first: String,
        peer: SocketAddr,
        cancel: CancellationToken,
    ) {
        let shared = Arc::clone(&self.shared);
        conn::serve(ws, device, Some(first), peer, shared, cancel).await;
    }

    /// Accepts connections on every one of `listeners` until `shutdown`, which also closes
    /// every connection.
    pub async fn run(self, listeners: Vec<TcpListener>, shutdown: CancellationToken) {
        match listen::local_addrs(&listeners) {
            Ok(listen) => self.listening_on(listen),
            Err(err) => warn!("cannot tell the addresses the server listens on: {err}"),
        }
        let hub = Arc::clone(&self.shared.hub);
        let flusher = tokio::spawn({
            let shutdown = shutdown.clone();
            async move { hub.run_flusher(shutdown).await }
        });
        let mut acceptor = listen::Acceptor::default();
        loop {
            let accepted = tokio::select! {
                () = shutdown.cancelled() => break,
                accepted = acceptor.accept(&listeners) => accepted,
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
