//! Shared client core for the TUI and the native apps: paired machines, one connection
//! supervisor per machine, a cache of what each daemon sent, and command dispatch.
//!
//! [`Client`] is the whole surface. It is shaped for foreign-language bindings (UniFFI, for
//! the Swift and Kotlin apps): every method takes and returns owned plain data (strings,
//! protocol types, the records and enums below), and streams are objects with an async
//! `next`, never a Rust `Stream`, generic or lifetime. [`auth`] is Rust-only plumbing for
//! whatever else connects to a daemon as a device.
//!
//! The surface is versioned by [`CLIENT_API_VERSION`] and frozen: `API.md` beside this crate
//! is the reviewed reference, and a test compares the surface with `public-api.txt`.
//!
//! # Machines
//!
//! A machine is a daemon this device paired with, keyed by the daemon's [`HostId`]. The
//! profile, `<config_dir>/machines.json`, holds each machine's addresses, the pinned SHA-256
//! of its certificate, and the device key this device presents to it (one key per machine).
//! [`Client::pair`] adds one from a `herder://pair` link, [`Client::rename`] changes the name
//! it is shown by on this device, and [`Client::forget`] removes it.
//!
//! # Connections
//!
//! Each machine has one supervisor task, the only thing that connects or retries. It tries
//! the machine's addresses in order; after a failure or a lost connection it waits a capped,
//! jittered exponential backoff (250 ms doubling to 30 s) and tries again, forever. A
//! connection silent for 45 s, despite pings, counts as lost. [`Machine::connection`] says
//! where it stands, and [`Client::synced`] waits until a connection is up and the daemon has
//! sent its lists and the replay of every subscription.
//!
//! A connection is pinged as soon as it is up and every 15 s after; [`Machine::quality`]
//! holds the round trips of the last 20 pongs, how many pongs did not come back before the
//! next ping, when the connection was established and how often it was re-established.
//!
//! # App lifecycle
//!
//! An app calls [`Client::suspend`] when it goes to the background and [`Client::wake`] when
//! it returns. Suspended, the client saves its offline cache and stops retrying lost
//! connections; connections that are up stay up for as long as the OS lets the process run.
//! Waking reconnects every disconnected machine at once and checks every connected one: a
//! connection silent for longer than 45 s, as after a long suspension in which the OS may
//! have killed the socket without either end noticing, is replaced right away; any other is
//! pinged and replaced if the pong does not come back within 5 s. Subscriptions resume from
//! the last seq held, so a suspension of any length shows no gap.
//!
//! # Resources
//!
//! [`Machine::resources`] and [`Machine::session_usage`] hold the latest figures the daemon
//! pushed, at most every two seconds each; they are live only while connected, so they are
//! cleared when the connection is not up.
//!
//! # Sessions
//!
//! The client caches, per session, every durable event it received and the items streaming
//! now (built from snapshots and deltas). While a session has subscribers the daemon streams
//! it; each new connection resumes it from the last seq held, and events are deduplicated by
//! seq, so a reconnect never shows a gap or a duplicate.
//!
//! # Offline cache
//!
//! What each machine last said is saved in `<config_dir>/cache/`, private to the user: the
//! role, the session, fleet host, project and account lists, and every event of the 20 listed
//! sessions with the newest events. A new [`Client`] starts from it, so [`Client::machines`]
//! and a subscription's first update show the last known state at once, offline too; the
//! subscription resumes after the cached events. Live data always wins: the cache is read only
//! when a machine's supervisor starts, the daemon's lists replace the cached ones, and its
//! events extend the cached ones by seq. The cache is saved every 30 s while something
//! changed, when a connection ends, and on [`Client::suspend`]. Streaming items, terminals and
//! resource figures are live only and never cached.
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

#![warn(missing_docs)]

pub mod auth;
mod cache;
mod offline;
mod pairing;
mod profile;
mod supervisor;
mod terminal;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use herder_protocol::{
    Account, AccountId, ClientHello, Command, CommandBody, CommandId, CommandResult, ErrorInfo,
    Event, FailoverSettings, FleetHost, HostId, HostResources, Item, PROTOCOL_VERSION, Project,
    Provider, Role, SessionHead, SessionId, SessionUsage, Terminal, TerminalId, Timestamp,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use auth::DeviceKey;
pub use pairing::PairingUri;
use profile::SavedMachine;
use supervisor::{Subscription, Supervisor};
pub use terminal::{TerminalEvent, TerminalStream};

/// The version of this crate's public API, `API.md`. It goes up by one with every change
/// that can break a client: anything removed, renamed or changed in what is listed there.
/// Additions keep it.
pub const CLIENT_API_VERSION: u32 = 5;

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
    #[error("{message}")]
    InvalidLink {
        /// What is wrong with it.
        message: String,
    },
    /// Pairing failed: no address answered, the certificate did not match, or the daemon
    /// refused the code.
    #[error("pairing failed: {message}")]
    Pairing {
        /// Why.
        message: String,
    },
    /// No paired machine has this host id.
    #[error("no paired machine {host_id}")]
    UnknownMachine {
        /// The host id asked for.
        host_id: HostId,
    },
    /// The daemon refused the command; nothing changed.
    #[error("{}", .info.message)]
    Rejected {
        /// The daemon's error; `info.code` says why.
        info: ErrorInfo,
    },
    /// Something on this device failed: the profile file, a device key, or no async runtime.
    #[error("{message}")]
    Local {
        /// What failed.
        message: String,
    },
    /// The client or the machine's supervisor stopped.
    #[error("the client is closed")]
    Closed,
}

/// A paired machine and what its daemon last said.
#[derive(Clone, Debug, PartialEq)]
pub struct Machine {
    /// The daemon's host id.
    pub host_id: HostId,
    /// The name it was given with [`Client::rename`], else the daemon's host name.
    pub name: String,
    /// Addresses tried in order, as `host:port`.
    pub addresses: Vec<String>,
    /// SHA-256 of the pinned daemon certificate, lowercase hex.
    pub fingerprint: String,
    /// Where the connection stands.
    pub connection: ConnectionState,
    /// How the connection performs.
    pub quality: ConnectionQuality,
    /// This device's user's role, from the latest hello; `None` until the first connection.
    pub role: Option<Role>,
    /// The daemon's sessions, as last listed. A vault's are every host's, each naming its
    /// host in `host_id`, and all read-only.
    pub sessions: Vec<SessionHead>,
    /// The hosts a vault lists sessions of, with their liveness, as last listed; empty for a
    /// daemon, whose sessions all run on its own host. A machine with hosts is a vault.
    pub hosts: Vec<FleetHost>,
    /// The projects with a clone on the daemon's host, as last listed; merge them across
    /// machines by `project_id`.
    pub projects: Vec<Project>,
    /// The daemon's accounts, as last listed.
    pub accounts: Vec<Account>,
    /// How the daemon's sessions fail over, as sent with the accounts.
    pub failover: FailoverSettings,
    /// Open terminals, as last listed; owners only, so empty for members.
    pub terminals: Vec<Terminal>,
    /// The host's load and turn admission, as last sent; `None` while not connected.
    pub resources: Option<HostResources>,
    /// What each session with processes or containers uses, as last sent; empty while not
    /// connected.
    pub session_usage: HashMap<SessionId, SessionUsage>,
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

/// How a machine's connection performs: round trips of the pings the client sends as soon as
/// a connection is up and every 15 s after, and how stable the connection is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectionQuality {
    /// When the current connection was established; `None` while not connected.
    pub connected_since: Option<Timestamp>,
    /// How many times a connection was established again after the first, since the client
    /// opened.
    pub reconnects: u32,
    /// Round trip of the latest pong, in milliseconds; `None` until one came back on the
    /// current connection.
    pub last_rtt_ms: Option<u32>,
    /// Mean round trip of the last 20 pongs on the current connection, in milliseconds.
    pub average_rtt_ms: Option<u32>,
    /// Shortest round trip of the last 20 pongs on the current connection, in milliseconds.
    pub min_rtt_ms: Option<u32>,
    /// Longest round trip of the last 20 pongs on the current connection, in milliseconds.
    pub max_rtt_ms: Option<u32>,
    /// Pings whose pong did not come back before the next ping was due, late or lost, since
    /// the client opened.
    pub missed_pongs: u32,
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
    /// Opens the profile in `config_dir`, a directory path, and starts connecting to every
    /// saved machine.
    ///
    /// `client` names this client in daemon logs, e.g. `herder-tui/0.1.0`. Call within a tokio
    /// runtime, which runs the supervisors. One client per config dir at a time.
    pub fn open(config_dir: String, client: String) -> Result<Self, Error> {
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(Error::Local {
                message: "the client needs a tokio runtime".to_owned(),
            });
        }
        let config_dir = PathBuf::from(config_dir);
        let saved = profile::load(&config_dir)?;
        let changed = Arc::new(watch::Sender::new(0));
        let stop = CancellationToken::new();
        let machines = saved
            .into_iter()
            .map(|saved| {
                Supervisor::start(
                    saved,
                    config_dir.clone(),
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
        let uri: PairingUri = link.parse()?;
        let device = DeviceKey::generate().map_err(|err| Error::Local {
            message: format!("{err:#}"),
        })?;
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
            .map_err(|message| Error::Pairing { message })?;
        // The supervisor opens its own connection; this one only proved the code.
        drop(ws);
        saved.host_id = hello.host_id;
        saved.name = hello.host_name;

        let mut machines = self.lock();
        let mut all: Vec<SavedMachine> = machines.iter().map(|m| m.saved()).collect();
        let index = all.iter().position(|m| m.host_id == saved.host_id);
        // A machine paired again keeps the name it was given.
        if let Some(index) = index {
            saved.name.clone_from(&all[index].name);
        }
        match index {
            Some(index) => all[index] = saved.clone(),
            None => all.push(saved.clone()),
        }
        profile::save(&self.inner.config_dir, &all)?;
        let supervisor = Supervisor::start(
            saved,
            self.inner.config_dir.clone(),
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

    /// Shows a machine as `name` on this device from now on, and saves that.
    pub fn rename(&self, host_id: HostId, name: String) -> Result<(), Error> {
        let machines = self.lock();
        let machine = find(&machines, &host_id)?;
        let mut all: Vec<SavedMachine> = machines.iter().map(|m| m.saved()).collect();
        for saved in &mut all {
            if saved.host_id == host_id {
                saved.name.clone_from(&name);
            }
        }
        profile::save(&self.inner.config_dir, &all)?;
        machine.rename(name);
        Ok(())
    }

    /// Unpairs a machine on this device: removes it from the profile, with this device's key
    /// for it, deletes its offline cache, and stops its connection; its subscriptions end. The daemon still lists the
    /// device until its owner revokes it.
    pub fn forget(&self, host_id: HostId) -> Result<(), Error> {
        let mut machines = self.lock();
        let machine = find(&machines, &host_id)?;
        let all: Vec<SavedMachine> = machines
            .iter()
            .filter(|m| m.saved.host_id != host_id)
            .map(|m| m.saved())
            .collect();
        profile::save(&self.inner.config_dir, &all)?;
        machines.retain(|m| m.saved.host_id != host_id);
        drop(machines);
        machine.forget();
        self.inner.changed.send_modify(|version| *version += 1);
        Ok(())
    }

    /// Waits until a machine is connected and its daemon sent everything it owes for what
    /// this client sent before the call: the session, account and terminal lists that follow
    /// each hello, and the replay of every session subscribed before. Projects arrive once the
    /// daemon has resolved them, which may be later.
    pub async fn synced(&self, host_id: HostId) -> Result<(), Error> {
        self.machine(&host_id)?.synced().await
    }

    /// The app went to the background: saves the offline cache, blocking on the file system,
    /// and stops retrying lost connections until [`Client::wake`]. Connections that are up are
    /// kept.
    pub fn suspend(&self) {
        let machines = self.lock().clone();
        for machine in machines {
            machine.suspend();
            machine.save();
        }
    }

    /// The app is in the foreground: reconnects every disconnected machine now instead of
    /// after its backoff, and resumes retrying after [`Client::suspend`]. Every connected
    /// machine is checked: its connection is replaced at once if it was silent for longer than
    /// 45 s, else pinged and replaced if the pong does not come back within 5 s.
    pub fn wake(&self) {
        for machine in self.lock().iter() {
            machine.wake();
        }
    }

    /// Notifications that [`Client::machines`] changed: a machine was paired, a connection
    /// changed state, or a daemon sent a new list or new resource figures.
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
        host_id: HostId,
        session_id: SessionId,
    ) -> Result<SessionSubscription, Error> {
        let machine = self.machine(&host_id)?;
        Ok(SessionSubscription(Subscription::new(machine, session_id)))
    }

    /// Sends a command to a machine and waits for the daemon's answer, however long it takes
    /// to connect. Dropping the future gives up; a command already sent may still apply.
    pub async fn send(
        &self,
        host_id: HostId,
        command: CommandBody,
    ) -> Result<CommandResult, Error> {
        let machine = self.machine(&host_id)?;
        let command = Command {
            id: new_command_id(),
            body: command,
        };
        machine
            .send(command)
            .await?
            .map_err(|info| Error::Rejected { info })
    }

    /// Opens a shell of `cols` by `rows` in a session's worktree and streams it, however long
    /// it takes to connect; owners only.
    pub async fn open_terminal(
        &self,
        host_id: HostId,
        session_id: SessionId,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalStream, Error> {
        self.machine(&host_id)?
            .open_terminal(CommandBody::OpenTerminal {
                session_id,
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
        host_id: HostId,
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
        self.machine(&host_id)?
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
        host_id: HostId,
        terminal_id: TerminalId,
    ) -> Result<TerminalStream, Error> {
        self.machine(&host_id)?.attach_terminal(terminal_id).await
    }

    fn machine(&self, host_id: &HostId) -> Result<Arc<Supervisor>, Error> {
        find(&self.lock(), host_id)
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Arc<Supervisor>>> {
        // Every update is a single push, replace or removal, so a poisoned list is consistent.
        self.inner
            .machines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// The supervisor of `host_id` among `machines`.
fn find(machines: &[Arc<Supervisor>], host_id: &HostId) -> Result<Arc<Supervisor>, Error> {
    machines
        .iter()
        .find(|machine| machine.saved.host_id == *host_id)
        .cloned()
        .ok_or_else(|| Error::UnknownMachine {
            host_id: host_id.clone(),
        })
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
    /// coalescing every change meanwhile: `true` then, `false` once the client stops.
    pub async fn next(&self) -> bool {
        let mut changed = self.changed.lock().await;
        tokio::select! {
            () = self.stop.cancelled() => false,
            changed = changed.changed() => changed.is_ok(),
        }
    }
}

/// A fresh command id.
pub(crate) fn new_command_id() -> CommandId {
    CommandId::new(ulid::Ulid::new().to_string())
}
