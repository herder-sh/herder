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
//! A machine is a daemon this device signed in to, keyed by the daemon's [`HostId`]. The
//! profile, `<config_dir>/machines.json`, holds each machine's addresses, the pinned SHA-256
//! of its certificate, and the device key this device presents to it (one key per machine).
//! [`Client::sign_in`] and [`Client::log_in`] add a machine, [`Client::rename`]
//! changes the name it is shown by on this device, [`Client::set_addresses`] the addresses it
//! is reached at, [`Client::reconnect`] connects again now, and [`Client::forget`] removes it.
//!
//! Clients may share a config dir at once, as the Mac app and the TUI on one Mac do: each
//! change merges into the profile as the file holds it, under a lock, and each client re-reads
//! the file within a second of a change and starts, updates or stops machines to match.
//! The offline cache is shared too; whichever client saves last wins, as it is only a cache.
//!
//! [`Client::share`] makes the `herder://fleet` link of every machine, which another device of
//! the person signs in to each with its own passkey: it carries no code and grants nothing by
//! itself. The new device gets its own key on each, revocable on its own.
//!
//! # Connections
//!
//! Each machine has one supervisor task, the only thing that connects or retries. It races
//! the machine's addresses in their order of preference, each with a 300 ms head start over the
//! next, and keeps the first that answers. Pairing saves the link's addresses private network
//! ones first and Tailscale ones last; after that the user's order holds. A wake, as on a
//! network change, also moves a connection that is up to an address that comes before its own,
//! once one answers. After a failure or a lost connection it waits a capped,
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
//! pushed, at most every two seconds each, and a vault's [`Machine::vault`] its latest status;
//! they are live only while connected, so they are cleared when the connection is not up. So
//! are [`Machine::skills`] and [`Machine::session_skills`], the skill library and each
//! session's skills as the daemon last sent them.
//!
//! # Skills
//!
//! The client keeps every machine where this device's user is owner on one skill library: the
//! repository last set with `set_skills_repo` through [`Client::send`], else the one the first
//! such machine reports. When a connection comes up and the daemon's [`Machine::skills`] names
//! another repository, or none, as on a machine paired since, the client sends it
//! `set_skills_repo` once on that connection. A repository set from another device on a
//! connected machine becomes the library. After one machine accepts a write (`put_skill`,
//! `delete_skill`, `import_skill`), every other one is sent `pull_skills`, at once if connected,
//! else once it connects again. Each machine's [`Machine::skills`] says where it stands:
//! `head`, `last_pull` and `pull_error`. The library is kept in memory only: a new [`Client`]
//! learns it from its machines, which report it without credentials.
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
//! sessions with the newest events. [`Client::open`] does not wait for it: each machine's
//! supervisor reads its cache off the async runtime, then shows it in [`Client::machines`],
//! offline too, with a [`Client::changes`] notification, before it first connects; until then
//! the machine is `Connecting`. A subscription's first update waits for the cache and carries
//! every cached event; the subscription resumes after them. Live data always wins: the cache
//! is read only when a machine's supervisor starts, the daemon's lists replace the cached
//! ones, and its events extend the cached ones by seq. The cache is saved every 30 s while
//! something changed, when a connection ends, and on [`Client::suspend`]. Streaming items,
//! terminals and resource figures are live only and never cached.
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
//!
//! # Passkeys
//!
//! On a machine where this device acts as a person, opening or attaching a terminal and every
//! change to secrets need a fresh passkey assertion. A client opened with an
//! [`Authenticator`] ([`Client::open_with_passkey`]), as the apps are with Face ID, sends such
//! a command without one first: the daemon runs it when it needs none, such as a re-attach of
//! a terminal this device had, and otherwise refuses it with `assertion_required`, and the
//! client asks for a challenge, has the authenticator sign it and sends the command again.
//! Its machines list what the person's devices without a passkey ask to approve
//! ([`Machine::approvals`]; [`Client::approve_action`], [`Client::refuse_action`]). A client
//! without one, such as the TUI, has the daemon ask those devices instead: the command is
//! answered once one of them approves it, or refused once refused or expired.

#![warn(missing_docs)]

mod address;
pub mod auth;
mod cache;
mod catalog;
mod fork;
mod link;
mod offline;
mod profile;
mod sign_in;
mod skills;
mod supervisor;
mod terminal;

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use herder_protocol::{
    Account, AccountId, ApprovalRequest, AwsProfile, Command, CommandBody, CommandId,
    CommandResult, ErrorCode, ErrorInfo, Event, FailoverSettings, FleetHost, HostId, HostResources,
    Item, PasskeyAssertion, PasskeyChallenge, PersonId, Project, Provider, Role, SessionHead,
    SessionId, SessionSkill, SessionUsage, SkillsStatus, Terminal, TerminalId, Timestamp,
    UpdateStage, VaultStatus,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use auth::DeviceKey;
pub use catalog::{
    CatalogEntry, CatalogModel, ProviderHint, ProviderHintKind, catalog_entry, next_account_id,
    provider_catalog, provider_hints,
};
pub use link::{FleetLink, FleetMachine, LoginUri, OwnUri};
use profile::SavedMachine;
pub use sign_in::PendingSignIn;
use supervisor::{Subscription, Supervisor};
pub use terminal::{TerminalEvent, TerminalStream};

/// The version of this crate's public API, `API.md`. It goes up by one with every change
/// that can break a client: anything removed, renamed or changed in what is listed there.
/// Additions keep it.
pub const CLIENT_API_VERSION: u32 = 17;

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
    /// The link is not a valid `herder://` link of its kind.
    #[error("{message}")]
    InvalidLink {
        /// What is wrong with it.
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
    /// No address of the machine answered; see [`Client::reconnect`].
    #[error("no address answered: {message}")]
    Unreachable {
        /// Why each address did not answer.
        message: String,
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
    /// Addresses as `host:port`, in order of preference: connecting races them, each with a
    /// head start over the next. Set with [`Client::set_addresses`].
    pub addresses: Vec<String>,
    /// The address the current connection uses; `None` while not connected.
    pub address: Option<String>,
    /// SHA-256 of the pinned daemon certificate, lowercase hex.
    pub fingerprint: String,
    /// Where the connection stands.
    pub connection: ConnectionState,
    /// How the connection performs.
    pub quality: ConnectionQuality,
    /// This device's user's role, from the latest hello; `None` until the first connection.
    pub role: Option<Role>,
    /// The person this device acts for there, from the latest hello; `None` until the first
    /// connection.
    pub person_id: Option<PersonId>,
    /// The daemon's herder version, from the latest hello; `None` until the first connection,
    /// and from daemons too old to update from a client (before `UpdateDaemon`).
    pub herder_version: Option<String>,
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
    /// Each runnable provider's CLI on the machine, as last listed.
    pub providers: Vec<herder_protocol::ProviderStatus>,
    /// Open terminals, as last listed; owners only, so empty for members.
    pub terminals: Vec<Terminal>,
    /// The host's load and turn admission, as last sent; `None` while not connected.
    pub resources: Option<HostResources>,
    /// What each session with processes or containers uses, as last sent; empty while not
    /// connected.
    pub session_usage: HashMap<SessionId, SessionUsage>,
    /// What a vault holds and how far each host's replication got, as last sent; `None` for a
    /// daemon, and while not connected.
    pub vault: Option<VaultStatus>,
    /// The skill library as the daemon has it, as last sent; `None` until it sends it, and
    /// while not connected.
    pub skills: Option<SkillsStatus>,
    /// The skills each live session's agent may use, as last sent; empty while not connected.
    pub session_skills: HashMap<SessionId, Vec<SessionSkill>>,
    /// Where an owner's `update_daemon` stands, as last sent; `None` until one starts, and
    /// while not connected, so after the restart it ends with.
    pub update: Option<UpdateStage>,
    /// What this person's devices without a passkey, such as a terminal signed in with
    /// `herder login`, ask to do on the machine, oldest first: approve one with
    /// [`Client::approve_action`], or refuse it. Empty while not connected, and for a client
    /// opened without an [`Authenticator`], which the daemon sends none.
    pub approvals: Vec<ApprovalRequest>,
}

/// What [`Authenticator::assert`] returns.
pub type Assertion = Pin<Box<dyn Future<Output = Result<PasskeyAssertion, Error>> + Send>>;

/// Signs passkey challenges for the person this client acts as: the platform's passkey API,
/// asking the user for Face ID; see the crate docs, Passkeys.
pub trait Authenticator: Send + Sync {
    /// Signs `challenge`, which the machine `host_id` minted, asking the user. Fails, and the
    /// command with it, when the user cancels.
    fn assert(&self, host_id: HostId, challenge: PasskeyChallenge) -> Assertion;
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
    /// Signs passkey assertions; `None` for a client without a passkey, such as the TUI.
    authenticator: Option<Arc<dyn Authenticator>>,
    machines: Arc<Mutex<Vec<Arc<Supervisor>>>>,
    library: Arc<skills::Library>,
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
    /// saved machine. Returns without waiting for the offline cache; see the crate docs, Offline cache.
    ///
    /// `client` names this client in daemon logs, e.g. `herder-tui/0.1.0`. Call within a tokio
    /// runtime, which runs the supervisors. Other clients may share the config dir; see the
    /// crate docs, Machines.
    pub fn open(config_dir: String, client: String) -> Result<Self, Error> {
        Self::open_with(config_dir, client, None)
    }

    /// Opens the profile as [`Client::open`] does, for a client that signs passkey
    /// assertions with `authenticator`, as the apps do with Face ID. A command that needs a
    /// fresh assertion and that the daemon refuses with `assertion_required` is sent again
    /// with one; see the crate docs, Passkeys.
    pub fn open_with_passkey(
        config_dir: String,
        client: String,
        authenticator: Arc<dyn Authenticator>,
    ) -> Result<Self, Error> {
        Self::open_with(config_dir, client, Some(authenticator))
    }

    fn open_with(
        config_dir: String,
        client: String,
        authenticator: Option<Arc<dyn Authenticator>>,
    ) -> Result<Self, Error> {
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
                    authenticator.is_some(),
                    Arc::clone(&changed),
                    stop.child_token(),
                )
            })
            .collect::<Result<_, _>>()?;
        let machines = Arc::new(Mutex::new(machines));
        let library = Arc::default();
        tokio::spawn(skills::run(
            Arc::clone(&library),
            Arc::downgrade(&machines),
            changed.subscribe(),
            stop.child_token(),
        ));
        let inner = Arc::new(Inner {
            config_dir,
            client,
            authenticator,
            machines,
            library,
            changed,
            stop,
        });
        tokio::spawn(follow_profile(
            Arc::downgrade(&inner),
            inner.stop.child_token(),
        ));
        Ok(Self { inner })
    }

    /// Every paired machine, in pairing order.
    pub fn machines(&self) -> Vec<Machine> {
        self.lock().iter().map(|machine| machine.view()).collect()
    }

    /// Connects to the machine at `hosts` with certificate `fingerprint` under a new device
    /// key, to sign in as a person: the machine answers with what the person's passkey signs.
    /// Finish with [`PendingSignIn::finish`]. Fails with `Unreachable` when no address
    /// answers, and `Rejected` when the machine refuses to let devices sign in.
    pub async fn sign_in(
        &self,
        hosts: Vec<String>,
        fingerprint: String,
    ) -> Result<PendingSignIn, Error> {
        sign_in::start(self, &hosts, &fingerprint).await
    }

    /// Signs this device in to the machine at `hosts` with certificate `fingerprint` without
    /// a passkey, as `herder login` does: the machine gives a code, passed to `shown` with
    /// when it expires, and once an app of a person approves it (`approve_login`) this device
    /// is that person's and the machine is saved. Fails with `Rejected` when the request
    /// expires or the machine refuses it.
    pub async fn log_in(
        &self,
        hosts: Vec<String>,
        fingerprint: String,
        shown: impl FnOnce(&str, Timestamp) + Send,
    ) -> Result<Machine, Error> {
        sign_in::log_in(self, &hosts, &fingerprint, shown).await
    }

    /// Saves a freshly paired machine, replacing it if paired before, and starts its
    /// supervisor.
    pub(crate) fn add(&self, mut saved: SavedMachine) -> Result<Machine, (Vec<String>, String)> {
        let addresses = saved.addresses.clone();
        let host_id = saved.host_id.clone();
        self.update(|all| {
            match all.iter_mut().find(|m| m.host_id == saved.host_id) {
                // A machine paired again keeps the name it was given.
                Some(old) => {
                    saved.name.clone_from(&old.name);
                    *old = saved;
                }
                None => all.push(saved),
            }
            Ok(())
        })
        .and_then(|()| self.machine(&host_id))
        .map(|supervisor| supervisor.view())
        .map_err(|err| (addresses, err.to_string()))
    }

    /// The link that shares this device's machines with another device of the person's: each
    /// machine's addresses and fingerprint, which that device signs in to with its passkey
    /// ([`Client::sign_in`]). It carries no code, so it grants nothing by itself. `None` while
    /// no machine is saved.
    pub fn share(&self) -> Option<FleetLink> {
        let machines: Vec<FleetMachine> = self
            .lock()
            .iter()
            .map(|machine| FleetMachine {
                hosts: machine.saved.addresses.clone(),
                fingerprint: machine.saved.fingerprint.clone(),
            })
            .collect();
        (!machines.is_empty()).then_some(FleetLink { machines })
    }

    /// Shows a machine as `name` on this device from now on, and saves that.
    pub fn rename(&self, host_id: HostId, name: String) -> Result<(), Error> {
        self.machine(&host_id)?;
        self.update(|all| {
            saved_mut(all, &host_id)?.name = name;
            Ok(())
        })
    }

    /// Connects to a machine at `addresses`, in this order of preference, from now on, and
    /// saves that. Each is a host name or an IP address, with an optional port (7447 by
    /// default). A connection that is up stays up unless it uses an address no longer listed;
    /// one that uses an address the new order puts after one that answers moves there.
    pub fn set_addresses(&self, host_id: HostId, addresses: Vec<String>) -> Result<(), Error> {
        let mut normalized: Vec<String> = Vec::new();
        for text in &addresses {
            let address = address::normalize(text).map_err(|message| Error::Local { message })?;
            if !normalized.contains(&address) {
                normalized.push(address);
            }
        }
        if normalized.is_empty() {
            return Err(Error::Local {
                message: "a machine needs at least one address".to_owned(),
            });
        }
        self.machine(&host_id)?;
        self.update(|all| {
            saved_mut(all, &host_id)?.addresses = normalized;
            Ok(())
        })
    }

    /// Drops a machine's connection, if up, and connects again at once, racing its addresses
    /// in their order as always: the address the new connection uses, or
    /// [`Error::Unreachable`] when none answered (the supervisor then retries after its
    /// backoff, as after any failure). To connect through an address, put it first with
    /// [`Client::set_addresses`] and then reconnect.
    pub async fn reconnect(&self, host_id: HostId) -> Result<String, Error> {
        self.machine(&host_id)?.reconnect().await
    }

    /// Unpairs a machine on this device: removes it from the profile, with this device's key
    /// for it, deletes its offline cache, and stops its connection; its subscriptions end. The daemon still lists the
    /// device until its owner revokes it.
    pub fn forget(&self, host_id: HostId) -> Result<(), Error> {
        self.machine(&host_id)?;
        self.update(|all| {
            all.retain(|m| m.host_id != host_id);
            Ok(())
        })
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
    ///
    /// An accepted `set_skills_repo` makes its URL the skill library of every machine where
    /// this device's user is owner, and an accepted skill write (`put_skill`, `delete_skill`,
    /// `import_skill`) has every other such machine pull; see the crate docs, Skills.
    pub async fn send(
        &self,
        host_id: HostId,
        command: CommandBody,
    ) -> Result<CommandResult, Error> {
        let machine = self.machine(&host_id)?;
        let body = command.clone();
        if let CommandBody::SetSkillsRepo { url } = &body {
            let connection = machine.connections();
            self.inner
                .library
                .setting(&host_id, connection, url.clone());
        }
        let result = self
            .signed(&machine, command, |body| {
                let machine = Arc::clone(&machine);
                async move {
                    let command = Command {
                        id: new_command_id(),
                        body,
                    };
                    machine
                        .send(command)
                        .await?
                        .map_err(|info| Error::Rejected { info })
                }
            })
            .await?;
        match body {
            CommandBody::SetSkillsRepo { url } => {
                self.inner
                    .library
                    .repo_set(&host_id, machine.connections(), url);
            }
            CommandBody::PutSkill { .. }
            | CommandBody::DeleteSkill { .. }
            | CommandBody::ImportSkill { .. } => {
                let machines = self
                    .lock()
                    .iter()
                    .map(|m| m.saved.host_id.clone())
                    .collect::<Vec<_>>();
                self.inner.library.wrote(&host_id, machines);
            }
            _ => return Ok(result),
        }
        self.inner.changed.send_modify(|version| *version += 1);
        Ok(result)
    }

    /// Forks `session_id`, a session `source` lists, onto `destination`, on `account_id` or
    /// else the account the destination picks, and returns the daemon's `session_forked`;
    /// the caller must be an owner of `destination`. Where the destination finds the history:
    ///
    /// - a session of `destination` itself forks from its own journal;
    /// - else, while a machine that lists the session is connected (`source`, else the
    ///   machine the session runs on, else a vault of this client's that backs it up), this
    ///   client reads the session's journal and images there and relays them to `destination`
    ///   (`upload_history`);
    /// - else it fails with `not_found`: a host never reads other hosts' sessions, from them
    ///   or from its vault.
    pub async fn fork_session(
        &self,
        source: HostId,
        session_id: SessionId,
        destination: HostId,
        account_id: Option<AccountId>,
    ) -> Result<CommandResult, Error> {
        let destination = self.machine(&destination)?;
        let machines = self.lock().clone();
        fork::fork(machines, source, destination, session_id, account_id).await
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
        let machine = self.machine(&host_id)?;
        let body = CommandBody::OpenTerminal {
            session_id,
            cols,
            rows,
            assertion: None,
        };
        self.signed(&machine, body, |body| {
            let machine = Arc::clone(&machine);
            async move { machine.open_terminal(body).await }
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

    /// Installs or updates a provider's CLI on a machine, in a terminal of `cols` by `rows`;
    /// owners only. The owner watches the vendor's installer. Refused when this OS has no
    /// recipe or the caller is a member.
    pub async fn install_provider(
        &self,
        host_id: HostId,
        provider: Provider,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalStream, Error> {
        self.machine(&host_id)?
            .open_terminal(CommandBody::InstallProvider {
                provider,
                cols,
                rows,
            })
            .await
    }

    /// Closes a terminal of a machine, whether or not this client streams it: hangs up its
    /// program, which ends every stream of it with [`TerminalEvent::Closed`]; owners only.
    /// Closing a login before it has logged in adds no account, and removes the config dir the
    /// machine created for it.
    pub async fn close_terminal(
        &self,
        host_id: HostId,
        terminal_id: TerminalId,
    ) -> Result<(), Error> {
        self.send(host_id, CommandBody::CloseTerminal { terminal_id })
            .await
            .map(|_| ())
    }

    /// Logs an existing account of a machine in again: runs its provider's own login in the
    /// account's config dir, in a login terminal of `cols` by `rows`, and streams it, however
    /// long it takes to connect; owners only. Once the provider reports the account logged
    /// in, failover may choose it again.
    pub async fn log_in_account(
        &self,
        host_id: HostId,
        account_id: AccountId,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalStream, Error> {
        self.machine(&host_id)?
            .open_terminal(CommandBody::LogInAccount {
                account_id,
                cols,
                rows,
            })
            .await
    }

    /// The AWS profiles of a machine's `~/.aws/config`, in the file's order; owners only.
    /// Empty when the machine has no AWS config.
    pub async fn list_aws_profiles(&self, host_id: HostId) -> Result<Vec<AwsProfile>, Error> {
        match self.send(host_id, CommandBody::ListAwsProfiles).await? {
            CommandResult::AwsProfiles { profiles } => Ok(profiles),
            _ => Err(Error::Local {
                message: "the daemon sent an unexpected answer".to_owned(),
            }),
        }
    }

    /// Signs a machine in to AWS IAM Identity Center as `profile`: runs `aws sso login` with a
    /// device code in a terminal of `cols` by `rows` and streams it, however long it takes to
    /// connect; owners only. The terminal's purpose carries the device code to approve once
    /// the CLI prints it, and its exit code 0 says the sign-in succeeded. Refused when the
    /// machine has no AWS CLI or no such profile.
    pub async fn aws_sso_login(
        &self,
        host_id: HostId,
        profile: String,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalStream, Error> {
        self.machine(&host_id)?
            .open_terminal(CommandBody::AwsSsoLogin {
                profile,
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
        let machine = self.machine(&host_id)?;
        let body = CommandBody::AttachTerminal {
            terminal_id,
            assertion: None,
        };
        self.signed(&machine, body, |body| {
            let machine = Arc::clone(&machine);
            async move {
                let CommandBody::AttachTerminal {
                    terminal_id,
                    assertion,
                } = body
                else {
                    return Err(Error::Local {
                        message: "signing changed an attach into another command".to_owned(),
                    });
                };
                machine.attach_terminal(terminal_id, assertion).await
            }
        })
        .await
    }

    /// Approves `request_id`, one of a machine's [`Machine::approvals`], with an assertion the
    /// [`Authenticator`] signs: the device that asked does what it asked, once.
    pub async fn approve_action(&self, host_id: HostId, request_id: String) -> Result<(), Error> {
        let machine = self.machine(&host_id)?;
        let Some(authenticator) = self.inner.authenticator.clone() else {
            return Err(Error::Local {
                message: "this client has no passkey to approve with".to_owned(),
            });
        };
        let challenge = challenge(&machine).await?;
        let assertion = authenticator.assert(host_id, challenge).await?;
        let body = CommandBody::ApproveAction {
            request_id,
            assertion: Box::new(assertion),
        };
        once(&machine, body).await.map(drop)
    }

    /// Refuses `request_id`, one of a machine's [`Machine::approvals`].
    pub async fn refuse_action(&self, host_id: HostId, request_id: String) -> Result<(), Error> {
        let machine = self.machine(&host_id)?;
        once(&machine, CommandBody::RefuseAction { request_id })
            .await
            .map(drop)
    }

    /// Runs `send` with `body`, and once more with a fresh assertion the [`Authenticator`]
    /// signs when the daemon refuses it with `assertion_required`.
    async fn signed<T, Sent>(
        &self,
        machine: &Arc<Supervisor>,
        body: CommandBody,
        send: impl Fn(CommandBody) -> Sent,
    ) -> Result<T, Error>
    where
        Sent: Future<Output = Result<T, Error>>,
    {
        let first = send(body.clone()).await;
        let (Err(Error::Rejected { info }), Some(authenticator)) =
            (&first, &self.inner.authenticator)
        else {
            return first;
        };
        if info.code != ErrorCode::AssertionRequired {
            return first;
        }
        let challenge = challenge(machine).await?;
        let assertion = authenticator
            .assert(machine.saved.host_id.clone(), challenge)
            .await?;
        let mut body = body;
        body.sign(assertion);
        send(body).await
    }

    fn machine(&self, host_id: &HostId) -> Result<Arc<Supervisor>, Error> {
        find(&self.lock(), host_id)
    }

    /// Changes the profile as the file holds it now, then matches the machines to the result.
    fn update(
        &self,
        change: impl FnOnce(&mut Vec<SavedMachine>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let mut machines = self.lock();
        let (saved, ()) = profile::update(&self.inner.config_dir, change)?;
        self.inner.reconcile(&mut machines, saved);
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Arc<Supervisor>>> {
        self.inner.lock()
    }
}

/// How often a client checks whether another client sharing its config dir changed the profile.
const PROFILE_POLL: Duration = Duration::from_secs(1);

impl Inner {
    fn lock(&self) -> MutexGuard<'_, Vec<Arc<Supervisor>>> {
        // Every update leaves the list whole, so a poisoned list is consistent.
        self.machines.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Matches `machines` to `saved`, the profile as the file holds it, keeping its order:
    /// starts a supervisor for each machine new to this client or paired again with a new key,
    /// renames and readdresses the others in place, and forgets the ones no longer saved.
    fn reconcile(&self, machines: &mut Vec<Arc<Supervisor>>, saved: Vec<SavedMachine>) {
        let mut old = std::mem::take(machines);
        let mut changed = false;
        for saved in saved {
            let held = old.iter().position(|m| m.saved.host_id == saved.host_id);
            if let Some(index) = held
                && old[index].saved.device_key == saved.device_key
                && old[index].saved.fingerprint == saved.fingerprint
            {
                let machine = old.remove(index);
                let current = machine.saved();
                if current.name != saved.name {
                    machine.rename(saved.name);
                }
                if current.addresses != saved.addresses {
                    machine.set_addresses(saved.addresses);
                }
                changed |= index != 0;
                machines.push(machine);
                continue;
            }
            if let Some(index) = held {
                old.remove(index).stop();
            }
            changed = true;
            let name = saved.name.clone();
            match Supervisor::start(
                saved,
                self.config_dir.clone(),
                self.client.clone(),
                self.authenticator.is_some(),
                Arc::clone(&self.changed),
                self.stop.child_token(),
            ) {
                Ok(machine) => machines.push(machine),
                Err(err) => warn!(machine = %name, "skipping a saved machine: {err}"),
            }
        }
        for machine in old {
            changed = true;
            machine.forget();
        }
        if changed {
            self.changed.send_modify(|version| *version += 1);
        }
    }
}

/// Re-reads the profile whenever the file changes, as when another client sharing the config
/// dir paired, renamed, readdressed or forgot a machine, until the client stops.
async fn follow_profile(inner: Weak<Inner>, stop: CancellationToken) {
    let mut seen = None;
    let mut poll = tokio::time::interval(PROFILE_POLL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            _ = poll.tick() => {}
        }
        let Some(inner) = inner.upgrade() else { return };
        let mut machines = inner.lock();
        // Read under the list's lock so a change of this client's own is never half seen.
        let stamp = profile::stamp(&inner.config_dir);
        if stamp == seen {
            continue;
        }
        match profile::load(&inner.config_dir) {
            Ok(saved) => {
                inner.reconcile(&mut machines, saved);
                seen = stamp;
            }
            Err(err) => warn!("re-reading the profile: {err}"),
        }
    }
}

/// The saved machine `host_id` among `all`.
fn saved_mut<'a>(
    all: &'a mut [SavedMachine],
    host_id: &HostId,
) -> Result<&'a mut SavedMachine, Error> {
    all.iter_mut()
        .find(|m| m.host_id == *host_id)
        .ok_or_else(|| Error::UnknownMachine {
            host_id: host_id.clone(),
        })
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
    /// The next update: the first as soon as the machine's offline cache is read, then once
    /// something changed, coalescing whatever changed meanwhile. `None` once the client or the machine stops.
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
/// A machine at `hosts` with certificate `fingerprint` to pair or sign in with, under a new
/// device key; its host id and name come from its hello.
pub(crate) fn unsaved(
    hosts: &[String],
    fingerprint: &str,
) -> Result<(SavedMachine, DeviceKey), String> {
    let normalized = hosts
        .iter()
        .map(|host| address::normalize(host))
        .collect::<Result<Vec<_>, _>>()?;
    let device = DeviceKey::generate().map_err(|err| format!("{err:#}"))?;
    let saved = SavedMachine {
        host_id: HostId::new(""),
        name: String::new(),
        addresses: address::default_order(normalized),
        fingerprint: fingerprint.to_ascii_lowercase(),
        device_key: device.to_pem().to_owned(),
    };
    Ok((saved, device))
}

/// Sends `body` to `machine` once and returns the daemon's answer.
async fn once(machine: &Supervisor, body: CommandBody) -> Result<CommandResult, Error> {
    let command = Command {
        id: new_command_id(),
        body,
    };
    machine
        .send(command)
        .await?
        .map_err(|info| Error::Rejected { info })
}

/// A fresh passkey challenge from `machine`.
async fn challenge(machine: &Supervisor) -> Result<PasskeyChallenge, Error> {
    match once(machine, CommandBody::PasskeyChallenge).await? {
        CommandResult::PasskeyChallenge { challenge, .. } => Ok(challenge),
        other => Err(Error::Local {
            message: format!("the daemon answered a passkey challenge with {other:?}"),
        }),
    }
}

pub(crate) fn new_command_id() -> CommandId {
    CommandId::new(ulid::Ulid::generate().to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;
    use std::sync::mpsc;

    use herder_protocol::{
        AccountId, Event, EventBody, HostId, SessionHead, SessionStatus, Timestamp,
    };

    use super::*;
    use crate::offline::Cached;

    /// Opening a client never waits for the offline cache, however long it takes to read: here
    /// it is a FIFO, which blocks its reader until something writes it. Once written, the
    /// machine shows it and a subscription's first update carries it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn opening_does_not_wait_for_the_offline_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let host_id = HostId::new("host");
        let machine = SavedMachine {
            host_id: host_id.clone(),
            name: "laptop".to_owned(),
            // Nothing listens on port 1.
            addresses: vec!["127.0.0.1:1".to_owned()],
            fingerprint: "00".repeat(32),
            device_key: DeviceKey::generate().unwrap().to_pem().to_owned(),
        };
        profile::save(tmp.path(), &[machine]).unwrap();
        let cache = offline::path(tmp.path(), &host_id);
        fs::create_dir_all(cache.parent().unwrap()).unwrap();
        assert!(
            Command::new("mkfifo")
                .arg(&cache)
                .status()
                .unwrap()
                .success()
        );

        // On a thread of its own, so an open that waits fails the test rather than hangs it.
        let (opened, open) = mpsc::channel();
        let config = tmp.path().display().to_string();
        let runtime = tokio::runtime::Handle::current();
        std::thread::spawn(move || {
            let _entered = runtime.enter();
            let _ = opened.send(Client::open(config, "herder-test/0".into()));
        });
        let client = open
            .recv_timeout(Duration::from_secs(5))
            .expect("opening waited for the offline cache")
            .unwrap();
        let machine = &client.machines()[0];
        assert_eq!(machine.connection, ConnectionState::Connecting);
        assert!(machine.sessions.is_empty());

        let session_id = SessionId::new("s");
        let sub = client
            .subscribe_session(host_id, session_id.clone())
            .unwrap();
        let first = tokio::spawn(async move { sub.next().await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !first.is_finished(),
            "the first update came before the cache"
        );

        let cached = Cached {
            role: Some(Role::Owner),
            sessions: vec![SessionHead {
                session_id: session_id.clone(),
                host_id: None,
                head_seq: 1,
                status: SessionStatus::Idle,
                parent: None,
                parent_host: None,
                task: None,
                title: None,
                project_id: None,
                account_id: AccountId::new("a"),
                children_need_you: 0,
                queue: Vec::new(),
                threads: Vec::new(),
                chat: false,
                origin: None,
            }],
            logs: vec![vec![Event {
                session_id,
                seq: 1,
                at: Timestamp::UNIX_EPOCH,
                by: None,
                body: EventBody::SessionStatusChanged {
                    retry_at: None,
                    status: SessionStatus::Idle,
                },
            }]],
            ..Cached::default()
        };
        let json = serde_json::to_vec(&cached).unwrap();
        tokio::task::spawn_blocking(move || fs::write(cache, json))
            .await
            .unwrap()
            .unwrap();

        let first = tokio::time::timeout(Duration::from_secs(5), first)
            .await
            .expect("the cache never came")
            .unwrap()
            .unwrap();
        assert_eq!(first.events, cached.logs[0]);
        let machine = &client.machines()[0];
        assert_eq!(machine.sessions, cached.sessions);
        assert_eq!(machine.role, Some(Role::Owner));
    }
}
