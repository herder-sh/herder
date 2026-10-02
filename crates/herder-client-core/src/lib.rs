//! Shared client core for the TUI and the native apps: paired machines, one connection
//! supervisor per machine, a cache of what each daemon sent, and command dispatch.
//!
//! [`Client`] is the whole surface. It is built to get foreign-language bindings later, so
//! every method takes and returns owned plain data (strings, protocol types, the records
//! below) and streams are objects with an async `next`, never a Rust `Stream` or callback.
//!
//! # Machines
//!
//! A machine is a daemon this device paired with, keyed by the daemon's [`HostId`]. The
//! profile, `<config_dir>/machines.json`, holds each machine's addresses, the pinned SHA-256
//! of its certificate, and the device key this device presents to it (one key per machine).
//! [`Client::pair`] adds one from a `herder://pair` link.
//!
//! # Connections
//!
//! Each machine has one supervisor task, the only thing that connects or retries. It tries
//! the machine's addresses in order; after a failure or a lost connection it waits a capped,
//! jittered exponential backoff (250 ms doubling to 30 s) and tries again, forever.
//! [`Client::wake`] cuts the wait short, for when an app returns to the foreground. A
//! connection silent for 45 s, despite pings, counts as lost. [`Machine::connection`] says
//! where it stands.
//!
//! # Sessions
//!
//! The client caches, per session, every durable event it received and the items streaming
//! now (built from snapshots and deltas). While a session has subscribers the daemon streams
//! it; each new connection resumes it from the last seq held, and events are deduplicated by
//! seq, so a reconnect never shows a gap or a duplicate. The cache is in memory: a new
//! [`Client`] starts empty and replays each session from its start.
//!
//! # Commands
//!
//! [`Client::send`] mints the command id. A command waits for a connection; if the connection
//! drops before the daemon answers, it is resent with the same id on the next one, and the
//! daemon applies an id once. The daemon remembers ids in memory only, so a command whose
//! answer was lost to a daemon restart can apply twice.
//!
//! # Terminals
//!
//! [`Client::open_terminal`] and [`Client::attach_terminal`] return a [`TerminalStream`]: the
//! terminal's output, starting with the daemon's scrollback, then its exit. The daemon attaches
//! terminals per connection, so each new connection re-attaches every stream and replays the
//! scrollback after a [`TerminalEvent::Reattached`]. Input and resizes are best effort: sent
//! once if connected, dropped otherwise, except that the latest size is re-sent on re-attach.
//! Dropping the stream detaches. Terminals are owner-only; for a member both calls fail with
//! [`Error::Rejected`] carrying `forbidden`.

pub mod auth;
mod cache;
mod profile;
mod supervisor;
mod terminal;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use herder_protocol::{
    Account, AccountId, ClientHello, Command, CommandBody, CommandId, CommandResult, ErrorInfo,
    Event, HostId, Item, PROTOCOL_VERSION, Provider, Role, SessionHead, SessionId, Terminal,
    TerminalId,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use auth::{DeviceKey, PairingUri};
use profile::SavedMachine;
use supervisor::{Subscription, Supervisor};
pub use terminal::{TerminalEvent, TerminalStream};

/// An account to add with [`Client::add_account`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewAccount {
    /// Id of the new account; unique on its machine.
    pub account_id: AccountId,
    /// Provider to log in to.
    pub provider: Provider,
    /// Display label; the id when absent.
    pub label: Option<String>,
    /// Config dir on the machine, absolute or starting with `~/`; the daemon picks one in the
    /// home directory when absent.
    pub config_dir: Option<String>,
}

/// Why a client call failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The pairing link is not a valid `herder://pair` link.
    #[error("{0}")]
    InvalidLink(String),
    /// Pairing failed: no address answered, the certificate did not match, or the daemon
    /// refused the code.
    #[error("pairing failed: {0}")]
    Pairing(String),
    /// No paired machine has this host id.
    #[error("no paired machine {0}")]
    UnknownMachine(HostId),
    /// The daemon refused the command; nothing changed.
    #[error("{}", .0.message)]
    Rejected(ErrorInfo),
    /// Something on this device failed: the profile file, a device key, or no async runtime.
    #[error("{0}")]
    Local(String),
    /// The client or the machine's supervisor stopped.
    #[error("the client is closed")]
    Closed,
}

/// A paired machine and what its daemon last said.
#[derive(Clone, Debug, PartialEq)]
pub struct Machine {
    /// The daemon's host id.
    pub host_id: HostId,
    /// The daemon's host name.
    pub name: String,
    /// Addresses tried in order, as `host:port`.
    pub addresses: Vec<String>,
    /// SHA-256 of the pinned daemon certificate, lowercase hex.
    pub fingerprint: String,
    /// Where the connection stands.
    pub connection: ConnectionState,
    /// This device's user's role, from the latest hello; `None` until the first connection.
    pub role: Option<Role>,
    /// The daemon's sessions, as last listed.
    pub sessions: Vec<SessionHead>,
    /// The daemon's accounts, as last listed.
    pub accounts: Vec<Account>,
    /// Open terminals, as last listed; owners only, so empty for members.
    pub terminals: Vec<Terminal>,
}

/// Where a machine's connection stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    /// Trying to connect.
    Connecting,
    /// Connected; subscriptions are streaming.
    Connected,
    /// Not connected; retrying after a backoff or a [`Client::wake`].
    Disconnected {
        /// Why the last attempt failed or the connection ended.
        error: String,
    },
}

/// What changed in a session since the subscription's previous update.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionUpdate {
    /// New durable events, in seq order, never one delivered before; the first update of a
    /// subscription carries every cached event.
    pub events: Vec<Event>,
    /// Every item streaming now, with its text so far, in the order they began. Replaces the
    /// previous update's list: an item leaves it when its `item_added` event or its turn's end
    /// arrives.
    pub streaming: Vec<Item>,
}

/// The client: paired machines and their connections. Cheap to clone; everything stops once
/// the last clone is dropped.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    config_dir: PathBuf,
    client: String,
    machines: Mutex<Vec<Arc<Supervisor>>>,
    changed: Arc<watch::Sender<u64>>,
    stop: CancellationToken,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl Client {
    /// Opens the profile in `config_dir` and starts connecting to every saved machine.
    ///
    /// `client` names this client in daemon logs, e.g. `herder-tui/0.1.0`. Call within a tokio
    /// runtime, which runs the supervisors. One client per config dir at a time.
    pub fn open(config_dir: PathBuf, client: String) -> Result<Self, Error> {
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(Error::Local("the client needs a tokio runtime".to_owned()));
        }
        let saved = profile::load(&config_dir)?;
        let changed = Arc::new(watch::Sender::new(0));
        let stop = CancellationToken::new();
        let machines = saved
            .into_iter()
            .map(|saved| {
                Supervisor::start(
                    saved,
                    client.clone(),
                    Arc::clone(&changed),
                    stop.child_token(),
                )
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            inner: Arc::new(Inner {
                config_dir,
                client,
                machines: Mutex::new(machines),
                changed,
                stop,
            }),
        })
    }

    /// Every paired machine, in pairing order.
    pub fn machines(&self) -> Vec<Machine> {
        self.lock().iter().map(|machine| machine.view()).collect()
    }

    /// Pairs with the daemon a `herder://pair` link names, saves it, and starts its supervisor.
    ///
    /// Pairing a machine already paired replaces it, with a new device key.
    pub async fn pair(&self, link: String) -> Result<Machine, Error> {
        let uri: PairingUri = link
            .parse()
            .map_err(|err: anyhow::Error| Error::InvalidLink(format!("{err:#}")))?;
        let device = DeviceKey::generate().map_err(|err| Error::Local(format!("{err:#}")))?;
        let mut saved = SavedMachine {
            host_id: HostId::new(""),
            name: String::new(),
            addresses: uri.hosts,
            fingerprint: uri.fingerprint.to_ascii_lowercase(),
            device_key: device.to_pem().to_owned(),
        };
        let hello = ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client: self.inner.client.clone(),
            resume: Vec::new(),
            pairing_code: Some(uri.code),
        };
        let (ws, hello) = supervisor::connect(&saved, &device, hello)
            .await
            .map_err(Error::Pairing)?;
        // The supervisor opens its own connection; this one only proved the code.
        drop(ws);
        saved.host_id = hello.host_id;
        saved.name = hello.host_name;

        let mut machines = self.lock();
        let mut all: Vec<SavedMachine> = machines.iter().map(|m| m.saved.clone()).collect();
        let index = all.iter().position(|m| m.host_id == saved.host_id);
        match index {
            Some(index) => all[index] = saved.clone(),
            None => all.push(saved.clone()),
        }
        profile::save(&self.inner.config_dir, &all)?;
        let supervisor = Supervisor::start(
            saved,
            self.inner.client.clone(),
            Arc::clone(&self.inner.changed),
            self.inner.stop.child_token(),
        )?;
        match index {
            Some(index) => std::mem::replace(&mut machines[index], Arc::clone(&supervisor)).stop(),
            None => machines.push(Arc::clone(&supervisor)),
        }
        drop(machines);
        self.inner.changed.send_modify(|version| *version += 1);
        Ok(supervisor.view())
    }

    /// Reconnects every disconnected machine now instead of after its backoff.
    pub fn wake(&self) {
        for machine in self.lock().iter() {
            machine.wake();
        }
    }

    /// Notifications that [`Client::machines`] changed: a machine was paired, a connection
    /// changed state, or a daemon sent a new list.
    pub fn changes(&self) -> Changes {
        Changes {
            changed: tokio::sync::Mutex::new(self.inner.changed.subscribe()),
            stop: self.inner.stop.clone(),
        }
    }

    /// Streams a session of a machine: the cached state first, then every change, across
    /// reconnects. The daemon streams the session while any subscription to it is alive.
    pub fn subscribe_session(
        &self,
        host_id: &HostId,
        session_id: &SessionId,
    ) -> Result<SessionSubscription, Error> {
        let machine = self.machine(host_id)?;
        Ok(SessionSubscription(Subscription::new(
            machine,
            session_id.clone(),
        )))
    }

    /// Sends a command to a machine and waits for the daemon's answer, however long it takes
    /// to connect. Dropping the future gives up; a command already sent may still apply.
    pub async fn send(
        &self,
        host_id: &HostId,
        command: CommandBody,
    ) -> Result<CommandResult, Error> {
        let machine = self.machine(host_id)?;
        let command = Command {
            id: new_command_id(),
            body: command,
        };
        machine.send(command).await?.map_err(Error::Rejected)
    }

    /// Opens a shell of `cols` by `rows` in a session's worktree and streams it, however long
    /// it takes to connect; owners only.
    pub async fn open_terminal(
        &self,
        host_id: &HostId,
        session_id: &SessionId,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalStream, Error> {
        self.machine(host_id)?
            .open_terminal(CommandBody::OpenTerminal {
                session_id: session_id.clone(),
                cols,
                rows,
            })
            .await
    }

    /// Adds an account to a machine: runs its provider's own login in a login terminal of
    /// `cols` by `rows` and streams it, however long it takes to connect; owners only. The
    /// account joins the machine's account list once the login exits successfully.
    pub async fn add_account(
        &self,
        host_id: &HostId,
        account: NewAccount,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalStream, Error> {
        let NewAccount {
            account_id,
            provider,
            label,
            config_dir,
        } = account;
        self.machine(host_id)?
            .open_terminal(CommandBody::AddAccount {
                account_id,
                provider,
                label,
                config_dir,
                cols,
                rows,
            })
            .await
    }

    /// Attaches to an open terminal and streams it, however long it takes to connect; owners
    /// only. One stream per terminal per client: a second attach fails until the first stream
    /// is dropped.
    pub async fn attach_terminal(
        &self,
        host_id: &HostId,
        terminal_id: &TerminalId,
    ) -> Result<TerminalStream, Error> {
        self.machine(host_id)?
            .attach_terminal(terminal_id.clone())
            .await
    }

    fn machine(&self, host_id: &HostId) -> Result<Arc<Supervisor>, Error> {
        self.lock()
            .iter()
            .find(|machine| machine.saved.host_id == *host_id)
            .cloned()
            .ok_or_else(|| Error::UnknownMachine(host_id.clone()))
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Arc<Supervisor>>> {
        // Every update is a single push or replace, so a poisoned list is consistent.
        self.inner
            .machines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// A stream of [`SessionUpdate`]s for one session; dropping it unsubscribes.
pub struct SessionSubscription(Subscription);

impl SessionSubscription {
    /// The next update: immediately for the first, then once something changed, coalescing
    /// whatever changed meanwhile. `None` once the client or the machine stops.
    pub async fn next(&self) -> Option<SessionUpdate> {
        self.0.next().await
    }
}

/// Change notifications for [`Client::machines`]; see [`Client::changes`].
pub struct Changes {
    changed: tokio::sync::Mutex<watch::Receiver<u64>>,
    stop: CancellationToken,
}

impl Changes {
    /// Waits until the machines changed since the previous call (or since [`Client::changes`]),
    /// coalescing every change meanwhile; `None` once the client stops.
    pub async fn next(&self) -> Option<()> {
        let mut changed = self.changed.lock().await;
        tokio::select! {
            () = self.stop.cancelled() => None,
            changed = changed.changed() => changed.ok(),
        }
    }
}

/// A fresh command id.
pub(crate) fn new_command_id() -> CommandId {
    CommandId::new(ulid::Ulid::new().to_string())
}
