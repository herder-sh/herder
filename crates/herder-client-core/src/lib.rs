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
//! [`Client::pair`] adds every machine a `herder://pair` link names, [`Client::rename`]
//! changes the name it is shown by on this device, [`Client::set_addresses`] the addresses it
//! is reached at, [`Client::reconnect`] connects again now, and [`Client::forget`] removes it.
//!
//! [`Client::share`] makes one link that pairs another device with every connected machine:
//! each daemon mints a one-time code for this device's user and role there, so the new device
//! gets its own key and is revocable on its own, and never more than this device may do.
//! It is a one-time share: machines paired later are not passed on.
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

mod address;
pub mod auth;
mod cache;
mod catalog;
mod fork;
mod offline;
mod pairing;
mod profile;
mod skills;
mod supervisor;
mod terminal;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures_util::future::join_all;
use herder_protocol::{
    Account, AccountId, ClientHello, Command, CommandBody, CommandId, CommandResult, ErrorInfo,
    Event, FailoverSettings, FleetHost, HostId, HostResources, Item, PROTOCOL_VERSION, Project,
    Provider, Role, SessionHead, SessionId, SessionSkill, SessionUsage, SkillsStatus, Terminal,
    TerminalId, Timestamp, VaultStatus,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use auth::DeviceKey;
pub use catalog::{
    CatalogEntry, CatalogModel, ProviderHint, ProviderHintKind, catalog_entry, next_account_id,
    provider_catalog, provider_hints,
};
pub use pairing::{PairingLink, PairingUri};
use profile::SavedMachine;
use supervisor::{Subscription, Supervisor};
pub use terminal::{TerminalEvent, TerminalStream};

/// The version of this crate's public API, `API.md`. It goes up by one with every change
/// that can break a client: anything removed, renamed or changed in what is listed there.
/// Additions keep it.
pub const CLIENT_API_VERSION: u32 = 11;

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
}

/// What pairing with one machine of a link came to; see [`Client::pair`].
// UniFFI passes records by value and cannot carry a `Box`; a link names a handful of machines,
// so the size of a result does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum PairResult {
    /// Paired, saved and connecting.
    Paired {
        /// The machine.
        machine: Machine,
    },
    /// Not paired; nothing was saved for it.
    Failed {
        /// The addresses the link names for it.
        addresses: Vec<String>,
        /// Why: no address answered, the certificate did not match, or the daemon refused
        /// the code.
        error: String,
    },
}

/// A link that pairs another device with this device's machines; see [`Client::share`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedLink {
    /// The link, one machine per code; format it with `to_string` for the QR code.
    pub link: PairingLink,
    /// The machines it pairs with, in the link's order.
    pub shared: Vec<HostId>,
    /// The machines left out, with why.
    pub skipped: Vec<SkippedMachine>,
    /// When the first of its codes stops working.
    pub expires_at: Timestamp,
}

/// A machine [`Client::share`] left out of the link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedMachine {
    /// The machine.
    pub host_id: HostId,
    /// Why: not connected, or its daemon refused or did not answer in time.
    pub error: String,
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
        let machines = Arc::new(Mutex::new(machines));
        let library = Arc::default();
        tokio::spawn(skills::run(
            Arc::clone(&library),
            Arc::downgrade(&machines),
            changed.subscribe(),
            stop.child_token(),
        ));
        Ok(Self {
            inner: Arc::new(Inner {
                config_dir,
                client,
                machines,
                library,
                changed,
                stop,
            }),
        })
    }

    /// Every paired machine, in pairing order.
    pub fn machines(&self) -> Vec<Machine> {
        self.lock().iter().map(|machine| machine.view()).collect()
    }

    /// Pairs with every machine a `herder://pair` link names, saves each that paired, and
    /// starts its supervisor; one result per machine, in the link's order. Fails only for a
    /// link that is not one.
    ///
    /// Pairing a machine already paired replaces it, with a new device key.
    pub async fn pair(&self, link: String) -> Result<Vec<PairResult>, Error> {
        let link: PairingLink = link.parse()?;
        let proved = join_all(link.machines.into_iter().map(|uri| self.prove(uri))).await;
        Ok(proved
            .into_iter()
            .map(|proved| match proved.and_then(|saved| self.add(saved)) {
                Ok(machine) => PairResult::Paired { machine },
                Err((addresses, error)) => PairResult::Failed { addresses, error },
            })
            .collect())
    }

    /// Pairs a new device key with the machine `uri` names: the machine as it will be saved,
    /// or its addresses and why it did not pair.
    async fn prove(&self, uri: PairingUri) -> Result<SavedMachine, (Vec<String>, String)> {
        let addresses = uri.hosts.clone();
        let normalized = uri
            .hosts
            .iter()
            .map(|host| address::normalize(host))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|message| (addresses.clone(), message))?;
        let device =
            DeviceKey::generate().map_err(|err| (addresses.clone(), format!("{err:#}")))?;
        let mut saved = SavedMachine {
            host_id: HostId::new(""),
            name: String::new(),
            addresses: address::default_order(normalized),
            fingerprint: uri.fingerprint.to_ascii_lowercase(),
            device_key: device.to_pem().to_owned(),
        };
        let hello = ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client: self.inner.client.clone(),
            resume: Vec::new(),
            pairing_code: Some(uri.code),
        };
        let (ws, hello, _) =
            supervisor::connect(&saved.addresses, &saved.fingerprint, &device, hello)
                .await
                .map_err(|message| (addresses, message))?;
        // The supervisor opens its own connection; this one only proved the code.
        drop(ws);
        saved.host_id = hello.host_id;
        saved.name = hello.host_name;
        Ok(saved)
    }

    /// Saves a freshly paired machine, replacing it if paired before, and starts its
    /// supervisor.
    fn add(&self, mut saved: SavedMachine) -> Result<Machine, (Vec<String>, String)> {
        let addresses = saved.addresses.clone();
        let failed = |err: Error| (addresses.clone(), err.to_string());
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
        profile::save(&self.inner.config_dir, &all).map_err(failed)?;
        let supervisor = Supervisor::start(
            saved,
            self.inner.config_dir.clone(),
            self.inner.client.clone(),
            Arc::clone(&self.inner.changed),
            self.inner.stop.child_token(),
        )
        .map_err(failed)?;
        match index {
            Some(index) => std::mem::replace(&mut machines[index], Arc::clone(&supervisor)).stop(),
            None => machines.push(Arc::clone(&supervisor)),
        }
        drop(machines);
        self.inner.changed.send_modify(|version| *version += 1);
        Ok(supervisor.view())
    }

    /// Makes a link that pairs another device with every connected machine, as this
    /// device's user with its role on each. Each machine is asked for a one-time code
    /// (`pair_device`); one not connected, or that refuses or does not answer within 10 s, is
    /// skipped. Fails with [`Error::Pairing`] when no machine gave a code.
    pub async fn share(&self) -> Result<SharedLink, Error> {
        let machines = self.lock().clone();
        let asked = machines.iter().map(|machine| async move {
            let host_id = machine.saved.host_id.clone();
            if machine.view().connection != ConnectionState::Connected {
                return (host_id, Err("not connected".to_owned()));
            }
            let command = Command {
                id: new_command_id(),
                body: CommandBody::PairDevice,
            };
            let answer = match tokio::time::timeout(SHARE_TIMEOUT, machine.send(command)).await {
                Err(_) => Err("did not answer in time".to_owned()),
                Ok(Err(err)) => Err(err.to_string()),
                Ok(Ok(Err(info))) => Err(info.message),
                Ok(Ok(Ok(CommandResult::DevicePairing {
                    code,
                    fingerprint,
                    addresses,
                    expires_at,
                }))) => Ok((
                    PairingUri {
                        hosts: addresses,
                        fingerprint,
                        code,
                    },
                    expires_at,
                )),
                Ok(Ok(Ok(_))) => Err("the daemon sent an unexpected answer".to_owned()),
            };
            (host_id, answer)
        });
        let (mut uris, mut shared, mut skipped) = (Vec::new(), Vec::new(), Vec::new());
        let mut expires_at: Option<Timestamp> = None;
        for (host_id, answer) in join_all(asked).await {
            match answer {
                Ok((uri, expires)) => {
                    uris.push(uri);
                    shared.push(host_id);
                    expires_at = Some(expires_at.map_or(expires, |at| at.min(expires)));
                }
                Err(error) => skipped.push(SkippedMachine { host_id, error }),
            }
        }
        let Some(expires_at) = expires_at else {
            let reasons: Vec<String> = skipped
                .iter()
                .map(|s| format!("{}: {}", s.host_id, s.error))
                .collect();
            return Err(Error::Pairing {
                message: if reasons.is_empty() {
                    "no machine is paired to share".to_owned()
                } else {
                    format!("no machine gave a code ({})", reasons.join("; "))
                },
            });
        };
        Ok(SharedLink {
            link: PairingLink { machines: uris },
            shared,
            skipped,
            expires_at,
        })
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
        let machines = self.lock();
        let machine = find(&machines, &host_id)?;
        let mut all: Vec<SavedMachine> = machines.iter().map(|m| m.saved()).collect();
        for saved in &mut all {
            if saved.host_id == host_id {
                saved.addresses.clone_from(&normalized);
            }
        }
        profile::save(&self.inner.config_dir, &all)?;
        machine.set_addresses(normalized);
        Ok(())
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
        let command = Command {
            id: new_command_id(),
            body: command,
        };
        let body = command.body.clone();
        if let CommandBody::SetSkillsRepo { url } = &body {
            let connection = machine.connections();
            self.inner
                .library
                .setting(&host_id, connection, url.clone());
        }
        let result = machine
            .send(command)
            .await?
            .map_err(|info| Error::Rejected { info })?;
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
    /// - else, while the machine the session runs on is connected, this client reads the
    ///   session's journal and images there and relays them to `destination`
    ///   (`upload_history`), even when `destination` has a vault;
    /// - else `destination` reads it from its vault, failing with `not_found` when it has none.
    ///
    /// A machine that is a vault relays the sessions it lists, for the hosts they run on.
    pub async fn fork_session(
        &self,
        source: HostId,
        session_id: SessionId,
        destination: HostId,
        account_id: Option<AccountId>,
    ) -> Result<CommandResult, Error> {
        let destination = self.machine(&destination)?;
        let from = self.machine(&source).ok();
        fork::fork(from, source, destination, session_id, account_id).await
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

/// How long [`Client::share`] waits for each machine's code.
const SHARE_TIMEOUT: Duration = Duration::from_secs(10);

/// A fresh command id.
pub(crate) fn new_command_id() -> CommandId {
    CommandId::new(ulid::Ulid::new().to_string())
}
