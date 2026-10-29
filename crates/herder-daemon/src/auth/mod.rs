//! Who may connect, and as whom: named users with owner or member roles, their paired devices,
//! and the one-time codes that pair a new device.
//!
//! A device is a key pair the client generates once (`herder_client_core::DeviceKey`). It
//! presents a self-signed certificate for that key as its TLS client certificate, and TLS 1.3
//! makes it sign the handshake transcript, which includes the daemon's fresh random: every
//! connection proves the client holds the key. The daemon knows a device by the SHA-256 fingerprint of that
//! certificate.
//!
//! An unknown device pairs by sending a code minted by `herder pair` as the `pairing_code` of
//! its hello. A code names a user and a role, works once and expires after [`PAIRING_TTL`];
//! codes live in memory only, so a restart voids them.
//!
//! A device is a client or a host ([`DeviceRole`]), as its code says. A client connects as a
//! client and reads everything, and may replicate to a vault too. A host, paired by
//! `herder pair --host` on a vault, may only replicate: every client connection it opens is
//! refused, so its key, if stolen, reads nothing. An owner may let a host read the vault too, so
//! it can take handoffs ([`Auth::set_device_role`]), and take that back.
//!
//! A person ([`Person`]) is one identity across every daemon that knows them, named by their
//! passkeys ([`passkeys`]). A device the daemon does not know signs in as a person with a fresh
//! assertion, claims a daemon with no owner with the code `herder install` printed, accepts an
//! invite, or waits for an app of the person to approve its login. A person's devices are
//! listed under them, and every connection from one acts as the person: its user id is the
//! person's id. Per-daemon users stay until every install has moved to people.
//!
//! A claim ([`Auth::claim`]) moves an owner's user to a person, keeping a record of the ids they
//! had, so history attributed to those ids reads as the person's.
//!
//! Users, people and devices persist in `<data_dir>/auth.json`, written atomically on every
//! change.

pub mod claim;
pub mod control;
pub mod passkeys;

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use herder_protocol::{
    CommandBody, DeviceId, ErrorCode, ErrorInfo, NewPerson, PasskeyAssertion, PasskeyChallenge,
    PasskeyCreation, PersonCard, PersonId, PersonInfo, PersonPasskey, Role, SignIn, Timestamp,
    UserDevice, UserId, UserInfo,
};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use webauthn_rs::prelude::{AuthenticationResult, Passkey};

use passkeys::Passkeys;

use crate::data_dir::write_private;
use crate::ws::Identity;

/// How long a pairing code stays valid.
pub const PAIRING_TTL: Duration = Duration::from_secs(10 * 60);

/// Crockford's base32 alphabet: no I, L, O or U to misread. 256 is a multiple of its length, so
/// a random byte maps onto it without bias.
const CODE_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Characters in a code: 50 bits, against at most ten minutes of online guessing.
const CODE_LEN: usize = 10;

/// How long an invite stays valid: long enough to reach a teammate.
pub const INVITE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Characters in an invite: 100 bits, as it lives a day.
const INVITE_LEN: usize = 20;

/// Characters in a claim or login code. A person compares it by eye, and it only names a
/// window a passkey must still sign into.
const SHORT_CODE_LEN: usize = 6;

const FILE: &str = "auth.json";

/// Version of `auth.json`: 1 gave devices roles ([`Auth::migrate_device_roles`]).
const VERSION: u32 = 1;

/// What a paired device may connect as.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceRole {
    /// A client, reading everything its user may; it may replicate to a vault as a host too.
    #[default]
    Client,
    /// A host replicating its own sessions to the vault, and nothing else.
    Host,
}

impl DeviceRole {
    /// Whether a device with this role may open a connection as `peer`.
    fn allows(self, peer: DeviceRole) -> bool {
        self == DeviceRole::Client || peer == DeviceRole::Host
    }
}

/// A named user of this daemon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    /// The user.
    pub user_id: UserId,
    /// Name given to `herder pair --user`, unique on this daemon.
    pub name: String,
    /// What the user may do.
    pub role: Role,
    /// When the user's first device paired.
    pub created_at: Timestamp,
    /// A machine paired as a user before hosts were their own, made a host by a claim: it is
    /// listed with the vault's hosts, not its users.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub host: bool,
}

/// A paired client device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// The device.
    pub device_id: DeviceId,
    /// The user it acts as.
    pub user_id: UserId,
    /// The client it paired with, from its hello, e.g. `herder-tui/0.1.0`.
    pub client: String,
    /// SHA-256 of its certificate's DER encoding, as lowercase hex.
    pub fingerprint: String,
    /// When it paired.
    pub paired_at: Timestamp,
    /// What it may connect as; devices paired before roles are clients until migrated.
    #[serde(default)]
    pub role: DeviceRole,
    /// When it last connected; devices that have not since this was kept have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<Timestamp>,
}

/// A freshly minted pairing code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pairing {
    /// The code, as shown to people: two groups of five characters.
    pub code: String,
    /// User the device will act as; created when the device pairs, if new.
    pub user: String,
    /// The user's role.
    pub role: Role,
    /// What the device will connect as.
    pub device_role: DeviceRole,
    /// When the code stops working.
    pub expires_at: Timestamp,
}

/// A person on this daemon, known by their passkeys.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Person {
    /// The person, across every daemon that knows them.
    pub person_id: PersonId,
    /// How others see them.
    pub name: String,
    /// What they may do here.
    pub role: Role,
    /// Their passkeys' public keys and signature counters.
    pub passkeys: Vec<Passkey>,
    /// When they joined this daemon.
    pub created_at: Timestamp,
}

impl Person {
    /// The user id their connections act as: their own id.
    pub fn user_id(&self) -> UserId {
        UserId::new(self.person_id.as_str())
    }
}

/// A person a passkey proved, not yet on this daemon: [`Auth::verify_new_person`].
pub struct ProvenPerson {
    /// Who they are.
    pub person_id: PersonId,
    /// The name they gave.
    pub name: String,
    /// The passkey they hold.
    pub passkey: Passkey,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Users {
    /// [`VERSION`] once migrated; files from before versions have none.
    #[serde(default)]
    version: u32,
    users: Vec<User>,
    /// Devices of users and people alike; a person's are under their [`Person::user_id`].
    devices: Vec<Device>,
    #[serde(default)]
    people: Vec<Person>,
    /// User ids claims moved to people, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    moved: Vec<claim::Moved>,
}

impl Users {
    fn user(&self, name: &str) -> Option<&User> {
        self.users.iter().find(|user| user.name == name)
    }

    fn person(&self, person_id: &PersonId) -> Option<&Person> {
        self.people
            .iter()
            .find(|person| &person.person_id == person_id)
    }

    /// The user `user_id` is now: the person a claim moved it to, or itself.
    fn resolve<'a>(&'a self, user_id: &'a UserId) -> &'a UserId {
        self.moved
            .iter()
            .find(|moved| &moved.user_id == user_id)
            .map_or(user_id, |moved| &moved.person)
    }

    /// The role of `user_id`, a user's or a person's, and the person it is; an id a claim
    /// moved is its person's.
    fn member(&self, user_id: &UserId) -> Option<(Role, Option<PersonId>)> {
        let user_id = self.resolve(user_id);
        if let Some(user) = self.users.iter().find(|user| &user.user_id == user_id) {
            return Some((user.role, None));
        }
        self.people
            .iter()
            .find(|person| person.person_id.as_str() == user_id.as_str())
            .map(|person| (person.role, Some(person.person_id.clone())))
    }

    /// The name of `user_id`, a user's or a person's; an id a claim moved is its person's.
    fn name(&self, user_id: &UserId) -> Option<&str> {
        let user_id = self.resolve(user_id);
        self.users
            .iter()
            .find(|user| &user.user_id == user_id)
            .map(|user| user.name.as_str())
            .or_else(|| {
                self.people
                    .iter()
                    .find(|person| person.person_id.as_str() == user_id.as_str())
                    .map(|person| person.name.as_str())
            })
    }

    fn has_owner(&self) -> bool {
        self.users.iter().any(|user| user.role == Role::Owner)
            || self.people.iter().any(|person| person.role == Role::Owner)
    }

    /// The passkeys of `person_id`; none for a person not on this daemon.
    fn keys(&self, person_id: &PersonId) -> Vec<Passkey> {
        self.person(person_id)
            .map(|person| person.passkeys.clone())
            .unwrap_or_default()
    }

    fn device(&self, fingerprint: &str) -> Option<&Device> {
        self.devices
            .iter()
            .find(|device| device.fingerprint == fingerprint)
    }

    /// The devices acting as `user_id`.
    fn devices_of(&self, user_id: &UserId) -> Vec<DeviceId> {
        self.devices
            .iter()
            .filter(|device| &device.user_id == user_id)
            .map(|device| device.device_id.clone())
            .collect()
    }
}

/// A window that ends at `expires`, opened by a code.
struct Window {
    code: String,
    expires: Instant,
}

/// A device waiting for an app of a person to approve its login.
struct PendingLogin {
    fingerprint: String,
    client: String,
    expires: Instant,
    approved: oneshot::Sender<()>,
}

/// A login request, answering `start_login`.
pub struct LoginRequest {
    /// The code the device shows.
    pub code: String,
    /// When it expires.
    pub expires_at: Timestamp,
    /// Resolves once an app approves it; the device is then paired.
    pub approved: oneshot::Receiver<()>,
}

struct Pending {
    user: String,
    role: Role,
    device_role: DeviceRole,
    expires: Instant,
}

struct State {
    users: Users,
    /// Unexpired codes by their normalized form.
    codes: HashMap<String, Pending>,
    /// Cancels the open connections of each device, with what each was opened as, so revoking
    /// one disconnects it.
    connections: HashMap<DeviceId, Vec<(DeviceRole, CancellationToken)>>,
    /// The window in which a daemon with no owner takes its first, from `herder install`.
    claim: Option<Window>,
    /// Unexpired invites by their normalized token, with the role each gives.
    invites: HashMap<String, (Role, Instant)>,
    /// Devices waiting for a login approval, by their normalized code.
    logins: HashMap<String, PendingLogin>,
}

/// The daemon's users, people, devices and pending codes.
pub struct Auth {
    dir: PathBuf,
    state: Mutex<State>,
    passkeys: Passkeys,
}

impl Auth {
    /// Loads the users and devices from `<dir>/auth.json`; a missing file means none yet.
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join(FILE);
        let users = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("{} is not a valid users file", path.display()))?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => Users {
                version: VERSION,
                ..Users::default()
            },
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self {
            dir: dir.to_owned(),
            state: Mutex::new(State {
                users,
                codes: HashMap::new(),
                connections: HashMap::new(),
                claim: None,
                invites: HashMap::new(),
                logins: HashMap::new(),
            }),
            passkeys: Passkeys::new()?,
        })
    }

    /// Mints a one-time code that pairs a client device as `user`, valid for `ttl`.
    ///
    /// An existing user keeps their role, so `role` must be absent or equal to it. A new user
    /// defaults to member, except that the first user of a daemon is always its owner: users
    /// of hosts alone do not count.
    pub fn mint(&self, user: &str, role: Option<Role>, ttl: Duration) -> Result<Pairing> {
        let user = user_name(user)?;
        let mut state = self.lock();
        let ownerless = !state.users.users.iter().any(|u| u.role == Role::Owner);
        let role = match (state.users.user(user), role) {
            (Some(existing), None) => existing.role,
            (Some(existing), Some(role)) if role == existing.role => role,
            (Some(existing), Some(_)) => {
                bail!("{user} is already {}", describe(existing.role))
            }
            (None, Some(Role::Member)) if ownerless => {
                bail!("the first user of a daemon is its owner; pair the owner first")
            }
            (None, role) if ownerless => role.unwrap_or(Role::Owner),
            (None, role) => role.unwrap_or(Role::Member),
        };
        Self::insert_code(&mut state, user, role, DeviceRole::Client, ttl)
    }

    /// Mints a one-time code that pairs a client device as the paired user `user_id`, with
    /// their role, valid for `ttl`: what `pair_device` shares from a device already paired.
    pub fn mint_for(&self, user_id: &UserId, ttl: Duration) -> Result<Pairing> {
        let mut state = self.lock();
        let Some(user) = state.users.users.iter().find(|u| &u.user_id == user_id) else {
            bail!("no user {user_id} on this daemon");
        };
        let (name, role) = (user.name.clone(), user.role);
        Self::insert_code(&mut state, &name, role, DeviceRole::Client, ttl)
    }

    /// Mints a one-time code that pairs the vault host named `host` as a host-only device,
    /// valid for `ttl`. The device acts as the user `host`, a member when new.
    pub fn mint_host(&self, host: &str, ttl: Duration) -> Result<Pairing> {
        let host = user_name(host)?;
        let mut state = self.lock();
        let role = state
            .users
            .user(host)
            .map_or(Role::Member, |user| user.role);
        Self::insert_code(&mut state, host, role, DeviceRole::Host, ttl)
    }

    fn insert_code(
        state: &mut State,
        user: &str,
        role: Role,
        device_role: DeviceRole,
        ttl: Duration,
    ) -> Result<Pairing> {
        let code = new_code()?;
        let now = Instant::now();
        state.codes.retain(|_, pending| pending.expires > now);
        state.codes.insert(
            normalize(&code),
            Pending {
                user: user.to_owned(),
                role,
                device_role,
                expires: now + ttl,
            },
        );
        let expires_at = Timestamp::now()
            .checked_add(ttl)
            .context("the pairing code would expire too far in the future")?;
        Ok(Pairing {
            code,
            user: user.to_owned(),
            role,
            device_role,
            expires_at,
        })
    }

    /// Decides who a connection from the device with certificate `fingerprint`, opened as
    /// `peer`, acts as.
    ///
    /// A paired device is its user. An unpaired one pairs with `code`, which is used up; without
    /// a valid code it is refused. A host device is refused as a client. `connection` is
    /// cancelled if the device is revoked.
    pub fn authenticate(
        &self,
        fingerprint: &str,
        code: Option<&str>,
        client: &str,
        peer: DeviceRole,
        connection: &CancellationToken,
    ) -> Result<Identity, ErrorInfo> {
        let mut state = self.lock();
        let identity = match identity(&state.users, fingerprint) {
            Some((identity, role)) if role.allows(peer) => {
                self.seen(&mut state, &identity.device_id);
                identity
            }
            Some((identity, _)) => {
                let host = state
                    .users
                    .users
                    .iter()
                    .find(|user| user.user_id == identity.user_id)
                    .map_or("this host", |user| user.name.as_str());
                return Err(host_only(host));
            }
            None => {
                let Some(code) = code else {
                    return Err(forbidden(
                        "this device is not paired with this daemon; run `herder pair` on the \
                         daemon's machine and pair with the code it prints",
                    ));
                };
                self.pair(&mut state, fingerprint, code, client, peer)?
            }
        };
        connected(&mut state, &identity.device_id, peer, connection);
        Ok(identity)
    }

    fn pair(
        &self,
        state: &mut State,
        fingerprint: &str,
        code: &str,
        client: &str,
        peer: DeviceRole,
    ) -> Result<Identity, ErrorInfo> {
        let key = normalize(code);
        let refused = || forbidden("the pairing code is invalid, expired or already used");
        let pending = state.codes.get(&key).ok_or_else(refused)?;
        if pending.expires <= Instant::now() {
            state.codes.remove(&key);
            return Err(refused());
        }
        // The code stays, for the host's config.
        if !pending.device_role.allows(peer) {
            return Err(forbidden(
                "this code pairs a host, which may only back its sessions up to the vault; \
                 pair a client with `herder pair` on the vault",
            ));
        }
        let now = Timestamp::now();
        let mut users = state.users.clone();
        let user = match users.user(&pending.user) {
            Some(user) if user.role != pending.role => {
                let message = format!("{} is already {}", user.name, describe(user.role));
                return Err(forbidden(&message));
            }
            Some(user) => user.clone(),
            None => {
                let user = User {
                    user_id: UserId::new(ulid::Ulid::generate().to_string()),
                    name: pending.user.clone(),
                    role: pending.role,
                    created_at: now,
                    host: false,
                };
                users.users.push(user.clone());
                user
            }
        };
        let device = Device {
            device_id: DeviceId::new(ulid::Ulid::generate().to_string()),
            user_id: user.user_id.clone(),
            client: client.to_owned(),
            fingerprint: fingerprint.to_owned(),
            paired_at: now,
            role: pending.device_role,
            last_seen: Some(now),
        };
        users.devices.push(device.clone());
        self.save(&users).map_err(|err| {
            tracing::error!("cannot save a paired device: {err:#}");
            ErrorInfo {
                code: ErrorCode::Internal,
                message: "cannot save the paired device".to_owned(),
            }
        })?;
        state.users = users;
        state.codes.remove(&key);
        info!(
            user = %user.name,
            device_id = %device.device_id,
            role = ?device.role,
            "device paired"
        );
        Ok(Identity {
            user_id: user.user_id,
            device_id: device.device_id,
            role: user.role,
            person_id: None,
        })
    }

    /// Notes that `device_id` connected now. Failing to save only loses that note.
    fn seen(&self, state: &mut State, device_id: &DeviceId) {
        let mut users = state.users.clone();
        let Some(device) = users
            .devices
            .iter_mut()
            .find(|device| &device.device_id == device_id)
        else {
            return;
        };
        device.last_seen = Some(Timestamp::now());
        match self.save(&users) {
            Ok(()) => state.users = users,
            Err(err) => warn!(%device_id, "cannot note when a device connected: {err:#}"),
        }
    }

    /// The daemon's first owner, which the local control socket acts as: only the system user
    /// running the daemon reaches it. `None` until an owner pairs.
    pub fn owner(&self) -> Option<UserId> {
        let state = self.lock();
        let users = state
            .users
            .users
            .iter()
            .filter(|user| user.role == Role::Owner)
            .map(|user| (user.created_at, user.user_id.clone()));
        let people = state
            .users
            .people
            .iter()
            .filter(|person| person.role == Role::Owner)
            .map(|person| (person.created_at, person.user_id()));
        users
            .chain(people)
            .min_by_key(|(created_at, _)| *created_at)
            .map(|(_, user_id)| user_id)
    }

    /// The role of `user_id`, a user or a person; `None` once no such user or person is here.
    pub fn role(&self, user_id: &UserId) -> Option<Role> {
        self.lock().users.member(user_id).map(|(role, _)| role)
    }

    /// The name of `user_id`, a user or a person; `None` once no such user or person is here.
    pub fn name(&self, user_id: &UserId) -> Option<String> {
        self.lock().users.name(user_id).map(str::to_owned)
    }

    /// The person `user_id` is, or a claim moved it to; `None` for a per-daemon user.
    pub fn person_of(&self, user_id: &UserId) -> Option<PersonId> {
        self.lock()
            .users
            .member(user_id)
            .and_then(|(_, person)| person)
    }

    /// The user `user_id` is now: the person a claim moved it to, or itself.
    pub fn resolve(&self, user_id: &UserId) -> UserId {
        self.lock().users.resolve(user_id).clone()
    }

    /// Every paired device with its user, oldest first.
    pub fn devices(&self) -> Vec<(Device, User)> {
        let state = self.lock();
        state
            .users
            .devices
            .iter()
            .filter_map(|device| {
                let user = state
                    .users
                    .users
                    .iter()
                    .find(|user| user.user_id == device.user_id)?;
                Some((device.clone(), user.clone()))
            })
            .collect()
    }

    /// Every user but hosts, oldest first, each with their devices, oldest first: what
    /// `list_users` and `herder pair --list` show.
    pub fn users(&self) -> Vec<UserInfo> {
        let mut state = self.lock();
        state
            .connections
            .retain(|_, open| open.iter().any(|(_, token)| !token.is_cancelled()));
        let mut users: Vec<&User> = state.users.users.iter().filter(|u| !u.host).collect();
        users.sort_by_key(|user| user.created_at);
        users
            .into_iter()
            .map(|user| UserInfo {
                user_id: user.user_id.clone(),
                name: user.name.clone(),
                role: user.role,
                devices: devices_of(&state, &user.user_id),
            })
            .collect()
    }

    /// Makes `user_id` a `role`, and closes their devices' connections, which carry the old
    /// role, so they connect again with the new one. Refuses to leave the daemon with no
    /// owner.
    pub fn set_role(&self, user_id: &UserId, role: Role) -> Result<(), ErrorInfo> {
        let mut state = self.lock();
        let mut users = state.users.clone();
        let Some(user) = users.users.iter_mut().find(|user| &user.user_id == user_id) else {
            return Err(ErrorInfo {
                code: ErrorCode::NotFound,
                message: format!("no user {user_id} on this daemon"),
            });
        };
        if user.role == role {
            return Ok(());
        }
        user.role = role;
        if !users.has_owner() {
            return Err(ErrorInfo {
                code: ErrorCode::Conflict,
                message: "the last owner cannot be made a member; make someone else an owner \
                          first"
                    .to_owned(),
            });
        }
        self.save(&users).map_err(|err| {
            tracing::error!("cannot save a user's role: {err:#}");
            ErrorInfo {
                code: ErrorCode::Internal,
                message: "cannot save the user's role".to_owned(),
            }
        })?;
        state.users = users;
        disconnect(&mut state, user_id);
        info!(%user_id, ?role, "user role changed");
        Ok(())
    }

    /// Makes the devices in `hosts`, which replicated to this vault as hosts, host-only, once:
    /// devices paired before roles could connect as hosts and clients alike, and one that
    /// replicated is taken to be a host's. Returns the devices demoted.
    ///
    /// Run on the vault before it accepts connections. Later runs change nothing, so a client
    /// paired since that also replicates stays a client.
    pub fn migrate_device_roles(&self, hosts: &[DeviceId]) -> Result<Vec<DeviceId>> {
        let mut state = self.lock();
        if state.users.version >= VERSION {
            return Ok(Vec::new());
        }
        let mut users = state.users.clone();
        let mut demoted = Vec::new();
        for device in &mut users.devices {
            if hosts.contains(&device.device_id) && device.role != DeviceRole::Host {
                device.role = DeviceRole::Host;
                demoted.push(device.device_id.clone());
            }
        }
        users.version = VERSION;
        self.save(&users)?;
        state.users = users;
        for device_id in &demoted {
            info!(%device_id, "device made host-only: it replicated as a host");
        }
        Ok(demoted)
    }

    /// What the paired device `device_id` may connect as; `None` if it is not paired.
    pub fn device_role(&self, device_id: &DeviceId) -> Option<DeviceRole> {
        self.lock()
            .users
            .devices
            .iter()
            .find(|device| &device.device_id == device_id)
            .map(|device| device.role)
    }

    /// Lets the paired device `device_id` connect as `role` from now on, keeping it paired;
    /// `false` if it is not paired. Made a host, its client connections close and its host
    /// connections stay.
    pub fn set_device_role(&self, device_id: &DeviceId, role: DeviceRole) -> Result<bool> {
        let mut state = self.lock();
        let mut users = state.users.clone();
        let Some(device) = users
            .devices
            .iter_mut()
            .find(|device| &device.device_id == device_id)
        else {
            return Ok(false);
        };
        if device.role == role {
            return Ok(true);
        }
        device.role = role;
        self.save(&users)?;
        state.users = users;
        if let Some(open) = state.connections.get_mut(device_id) {
            open.retain(|(peer, token)| {
                if !role.allows(*peer) {
                    token.cancel();
                }
                !token.is_cancelled()
            });
        }
        info!(%device_id, ?role, "device role changed");
        Ok(true)
    }

    /// Unpairs a device and closes its open connections; `false` if it was not paired.
    pub fn revoke(&self, device_id: &DeviceId) -> Result<bool> {
        let mut state = self.lock();
        let mut users = state.users.clone();
        let before = users.devices.len();
        users
            .devices
            .retain(|device| &device.device_id != device_id);
        if users.devices.len() == before {
            return Ok(false);
        }
        self.save(&users)?;
        state.users = users;
        for (_, token) in state.connections.remove(device_id).unwrap_or_default() {
            token.cancel();
        }
        info!(%device_id, "device revoked");
        Ok(true)
    }

    /// Whether the device with certificate `fingerprint` is paired.
    pub fn is_paired(&self, fingerprint: &str) -> bool {
        self.lock().users.device(fingerprint).is_some()
    }

    /// Opens the window in which this daemon takes its first owner, for [`PAIRING_TTL`], with
    /// a fresh code; what `herder install` shows. Refused once the daemon has an owner.
    pub fn open_claim(&self) -> Result<(String, Timestamp)> {
        let mut state = self.lock();
        if state.users.has_owner() {
            bail!("this daemon has an owner already");
        }
        let code = random_code(SHORT_CODE_LEN, 3)?;
        state.claim = Some(Window {
            code: normalize(&code),
            expires: Instant::now() + PAIRING_TTL,
        });
        let expires_at = Timestamp::now()
            .checked_add(PAIRING_TTL)
            .context("the claim would expire too far in the future")?;
        info!("claim window opened");
        Ok((code, expires_at))
    }

    /// A fresh challenge for a passkey to sign: for a sign-in, or for a command that needs a
    /// fresh assertion.
    pub fn challenge(&self) -> Result<PasskeyChallenge, ErrorInfo> {
        self.passkeys.challenge()
    }

    /// A fresh challenge to create a passkey for a new person from.
    pub fn creation(&self) -> Result<PasskeyCreation, ErrorInfo> {
        self.passkeys.creation()
    }

    /// What a device signing in signs, and what a new passkey is created from while the
    /// daemon takes a claim or an invite.
    pub fn sign_in_challenge(
        &self,
    ) -> Result<(PasskeyChallenge, Option<PasskeyCreation>), ErrorInfo> {
        let creating = {
            let mut state = self.lock();
            let now = Instant::now();
            state.invites.retain(|_, (_, expires)| *expires > now);
            let claiming = state
                .claim
                .as_ref()
                .is_some_and(|claim| claim.expires > now && !state.users.has_owner());
            claiming || !state.invites.is_empty()
        };
        let creation = match creating {
            true => Some(self.passkeys.creation()?),
            false => None,
        };
        Ok((self.passkeys.challenge()?, creation))
    }

    /// Verifies the passkey `person` holds, by its registration or by an assertion over a
    /// fresh challenge, without adding them to this daemon.
    pub fn verify_new_person(&self, person: &NewPerson) -> Result<ProvenPerson, ErrorInfo> {
        let name = user_name(&person.name)
            .map_err(|err| bad_request(&err.to_string()))?
            .to_owned();
        let (person_id, passkey) = match &person.passkey {
            PersonPasskey::Register { registration } => self.passkeys.register(registration)?,
            PersonPasskey::Existing {
                person_id,
                key,
                assertion,
            } => {
                let mut passkey = passkeys::passkey(key)?;
                let asserted = self.passkeys.verify(assertion, |id| {
                    if id == person_id {
                        vec![passkey.clone()]
                    } else {
                        Vec::new()
                    }
                })?;
                passkey.update_credential(&asserted.result);
                (asserted.person_id, passkey)
            }
        };
        Ok(ProvenPerson {
            person_id,
            name,
            passkey,
        })
    }

    /// Verifies that `assertion`, over a fresh challenge, is by a passkey of `person_id`.
    pub fn verify_assertion(
        &self,
        person_id: &PersonId,
        assertion: &PasskeyAssertion,
    ) -> Result<(), ErrorInfo> {
        let mut state = self.lock();
        let asserted = self.passkeys.verify(assertion, |id| {
            if id == person_id {
                state.users.keys(id)
            } else {
                Vec::new()
            }
        })?;
        self.note_use(&mut state, &asserted.person_id, &asserted.result);
        Ok(())
    }

    /// Signs in the device with certificate `fingerprint` as a person, by `method`, and lists
    /// it under them. `connection` is cancelled if the device or person is revoked.
    pub fn sign_in(
        &self,
        fingerprint: &str,
        client: &str,
        method: SignIn,
        connection: &CancellationToken,
    ) -> Result<Identity, ErrorInfo> {
        let mut state = self.lock();
        if state.users.device(fingerprint).is_some() {
            return Err(bad_request("this device is paired already"));
        }
        let mut users = state.users.clone();
        let now = Instant::now();
        let person_id = match method {
            SignIn::Passkey { assertion } => {
                let asserted = self.passkeys.verify(&assertion, |id| users.keys(id))?;
                note(&mut users, &asserted.person_id, &asserted.result);
                asserted.person_id
            }
            SignIn::ClaimOwnership { code, person } => {
                let open = state
                    .claim
                    .as_ref()
                    .is_some_and(|claim| claim.expires > now && claim.code == normalize(&code));
                if !open || users.has_owner() {
                    return Err(forbidden(
                        "the code is wrong or expired, or this daemon has an owner already; \
                         run `herder install` on it again for a new code",
                    ));
                }
                let proven = self.verify_new_person(&person)?;
                let person_id = proven.person_id.clone();
                add_person(&mut users, proven, Role::Owner);
                person_id
            }
            SignIn::AcceptInvite { invite, person } => {
                let key = normalize(&invite);
                let role = match state.invites.get(&key) {
                    Some((role, expires)) if *expires > now => *role,
                    _ => return Err(forbidden("the invite is invalid, expired or already used")),
                };
                let proven = self.verify_new_person(&person)?;
                if users.person(&proven.person_id).is_some() {
                    return Err(conflict(
                        "you are on this daemon already; sign in with your passkey",
                    ));
                }
                let person_id = proven.person_id.clone();
                add_person(&mut users, proven, role);
                state.invites.remove(&key);
                person_id
            }
            SignIn::StartLogin => {
                return Err(bad_request("a login waits for an approval instead"));
            }
        };
        let Some(person) = users.person(&person_id).cloned() else {
            return Err(forbidden("this passkey's person is not on this daemon"));
        };
        let device = new_device(&person.user_id(), fingerprint, client);
        users.devices.push(device.clone());
        self.save(&users).map_err(saving)?;
        if person.role == Role::Owner {
            state.claim = None;
        }
        state.users = users;
        info!(
            person_id = %person.person_id,
            device_id = %device.device_id,
            "device signed in"
        );
        connected(
            &mut state,
            &device.device_id,
            DeviceRole::Client,
            connection,
        );
        Ok(Identity {
            user_id: person.user_id(),
            device_id: device.device_id,
            role: person.role,
            person_id: Some(person.person_id),
        })
    }

    /// Asks for a person's app to approve the device with certificate `fingerprint`, for up
    /// to [`PAIRING_TTL`].
    pub fn start_login(&self, fingerprint: &str, client: &str) -> Result<LoginRequest, ErrorInfo> {
        let code = random_code(SHORT_CODE_LEN, 3).map_err(|err| internal(&err))?;
        let expires_at = Timestamp::now()
            .checked_add(PAIRING_TTL)
            .map_err(|err| internal(&err))?;
        let (approve, approved) = oneshot::channel();
        let mut state = self.lock();
        let now = Instant::now();
        state
            .logins
            .retain(|_, login| login.expires > now && !login.approved.is_closed());
        state.logins.insert(
            normalize(&code),
            PendingLogin {
                fingerprint: fingerprint.to_owned(),
                client: client.to_owned(),
                expires: now + PAIRING_TTL,
                approved: approve,
            },
        );
        Ok(LoginRequest {
            code,
            expires_at,
            approved,
        })
    }

    /// Lists the device waiting with `code` under `identity`'s person, and lets it in.
    pub fn approve_login(
        &self,
        identity: &Identity,
        code: &str,
    ) -> Result<(DeviceId, String), ErrorInfo> {
        let Some(person_id) = &identity.person_id else {
            return Err(forbidden("only a person signs a device in"));
        };
        let mut state = self.lock();
        let key = normalize(code);
        let waiting = state
            .logins
            .get(&key)
            .is_some_and(|login| login.expires > Instant::now() && !login.approved.is_closed());
        let Some(login) = state.logins.remove(&key).filter(|_| waiting) else {
            return Err(not_found("no device is waiting to sign in with that code"));
        };
        if state.users.device(&login.fingerprint).is_some() {
            return Err(conflict("that device is paired already"));
        }
        let mut users = state.users.clone();
        if users.person(person_id).is_none() {
            return Err(forbidden("you are no longer on this daemon"));
        }
        let device = new_device(&identity.user_id, &login.fingerprint, &login.client);
        users.devices.push(device.clone());
        self.save(&users).map_err(saving)?;
        state.users = users;
        info!(%person_id, device_id = %device.device_id, "login approved");
        // The device stops waiting if it is gone; it is paired either way.
        let _ = login.approved.send(());
        Ok((device.device_id, device.client))
    }

    /// Mints an invite that lets someone join as a person with `role`, valid for
    /// [`INVITE_TTL`].
    pub fn invite(&self, role: Role) -> Result<(String, Timestamp), ErrorInfo> {
        let token = random_code(INVITE_LEN, 5).map_err(|err| internal(&err))?;
        let expires_at = Timestamp::now()
            .checked_add(INVITE_TTL)
            .map_err(|err| internal(&err))?;
        let mut state = self.lock();
        let now = Instant::now();
        state.invites.retain(|_, (_, expires)| *expires > now);
        state
            .invites
            .insert(normalize(&token), (role, now + INVITE_TTL));
        info!(?role, "invite minted");
        Ok((token, expires_at))
    }

    /// Gives `person_id` `role`, adding them from `card` when they are not here yet, and
    /// closes their connections, which carry the old role. Refuses to leave the daemon with
    /// no owner.
    pub fn set_person_role(
        &self,
        person_id: &PersonId,
        role: Role,
        card: Option<PersonCard>,
    ) -> Result<(), ErrorInfo> {
        let mut state = self.lock();
        let mut users = state.users.clone();
        match users
            .people
            .iter_mut()
            .find(|person| &person.person_id == person_id)
        {
            Some(person) if person.role == role => return Ok(()),
            Some(person) => person.role = role,
            None => {
                let Some(card) = card else {
                    return Err(not_found(&format!("no person {person_id} on this daemon")));
                };
                let name = user_name(&card.name)
                    .map_err(|err| bad_request(&err.to_string()))?
                    .to_owned();
                let passkeys = card
                    .passkeys
                    .iter()
                    .map(passkeys::passkey)
                    .collect::<Result<Vec<_>, _>>()?;
                if passkeys.is_empty() {
                    return Err(bad_request("a person needs at least one passkey"));
                }
                users.people.push(Person {
                    person_id: person_id.clone(),
                    name,
                    role,
                    passkeys,
                    created_at: Timestamp::now(),
                });
            }
        }
        if !users.has_owner() {
            return Err(last_owner());
        }
        self.save(&users).map_err(saving)?;
        state.users = users;
        disconnect(&mut state, &UserId::new(person_id.as_str()));
        info!(%person_id, ?role, "person role changed");
        Ok(())
    }

    /// Removes `person_id` with their devices, closing their connections. Refuses to leave
    /// the daemon with no owner.
    pub fn revoke_person(&self, person_id: &PersonId) -> Result<(), ErrorInfo> {
        let mut state = self.lock();
        let mut users = state.users.clone();
        let before = users.people.len();
        users.people.retain(|person| &person.person_id != person_id);
        if users.people.len() == before {
            return Err(not_found(&format!("no person {person_id} on this daemon")));
        }
        if !users.has_owner() {
            return Err(last_owner());
        }
        let user_id = UserId::new(person_id.as_str());
        users.devices.retain(|device| device.user_id != user_id);
        self.save(&users).map_err(saving)?;
        state.users = users;
        disconnect(&mut state, &user_id);
        info!(%person_id, "person revoked");
        Ok(())
    }

    /// Every person, oldest first, each with their passkeys and devices: what `list_people`
    /// shows.
    pub fn people(&self) -> Vec<PersonInfo> {
        let mut state = self.lock();
        state
            .connections
            .retain(|_, open| open.iter().any(|(_, token)| !token.is_cancelled()));
        let mut people: Vec<&Person> = state.users.people.iter().collect();
        people.sort_by_key(|person| person.created_at);
        people
            .into_iter()
            .map(|person| PersonInfo {
                person_id: person.person_id.clone(),
                name: person.name.clone(),
                role: person.role,
                passkeys: person.passkeys.iter().map(passkeys::key).collect(),
                devices: devices_of(&state, &person.user_id()),
            })
            .collect()
    }

    /// Notes an assertion on `person_id`'s passkeys. Failing to save only loses the counter.
    fn note_use(&self, state: &mut State, person_id: &PersonId, result: &AuthenticationResult) {
        let mut users = state.users.clone();
        if !note(&mut users, person_id, result) {
            return;
        }
        match self.save(&users) {
            Ok(()) => state.users = users,
            Err(err) => warn!(%person_id, "cannot note a passkey's use: {err:#}"),
        }
    }

    fn save(&self, users: &Users) -> Result<()> {
        let json = serde_json::to_vec_pretty(users).context("encoding the users file")?;
        write_private(&self.dir, FILE, &json)?;
        File::open(&self.dir)
            .and_then(|dir| dir.sync_all())
            .with_context(|| format!("syncing {}", self.dir.display()))
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // Every update swaps in a fully built value, so a poisoned state is still consistent.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Refuses commands `role` does not allow: terminals, and so adding accounts or
/// logging them in again and signing the host in to AWS, bringing down containers, browsing folders, changing projects,
/// reading or changing the daemon's settings, restarting it, backing up to a vault, forking
/// sessions onto the host, changing the skill library or which of its skills are enabled,
/// changing users' and people's roles, inviting people, revoking people and devices, claiming
/// users as a person, pairing a device as anyone but the caller, managing
/// secrets and storing an agent's are for owners only.
pub fn authorize(role: Role, command: &CommandBody) -> Result<(), ErrorInfo> {
    let terminal = matches!(
        command,
        CommandBody::OpenTerminal { .. }
            | CommandBody::AddAccount { .. }
            | CommandBody::InstallProvider { .. }
            | CommandBody::LogInAccount { .. }
            | CommandBody::ListAwsProfiles
            | CommandBody::AwsSsoLogin { .. }
            | CommandBody::AttachTerminal { .. }
            | CommandBody::DetachTerminal { .. }
            | CommandBody::CloseTerminal { .. }
            | CommandBody::ResizeTerminal { .. }
            | CommandBody::TerminalInput { .. }
    );
    if terminal && role != Role::Owner {
        return Err(forbidden("terminals are for the daemon's owners only"));
    }
    if matches!(command, CommandBody::ComposeDown { .. }) && role != Role::Owner {
        return Err(forbidden(
            "bringing down containers is for the daemon's owners only",
        ));
    }
    if matches!(command, CommandBody::SetAccountSettings { .. }) && role != Role::Owner {
        return Err(forbidden(
            "changing accounts is for the daemon's owners only",
        ));
    }
    let settings = matches!(
        command,
        CommandBody::GetSettings
            | CommandBody::SetSettings { .. }
            | CommandBody::SetResourceLimits { .. }
            | CommandBody::RestartDaemon
            | CommandBody::UpdateDaemon { .. }
    );
    if settings && role != Role::Owner {
        return Err(forbidden("the daemon's settings are for its owners only"));
    }
    let host = matches!(
        command,
        CommandBody::ListDirectory { .. }
            | CommandBody::AddProject { .. }
            | CommandBody::CloneProject { .. }
            | CommandBody::SetProjectSettings { .. }
            | CommandBody::RemoveProject { .. }
            | CommandBody::SetProjectIcon { .. }
    );
    if host && role != Role::Owner {
        return Err(forbidden(
            "browsing folders and changing projects are for the daemon's owners only",
        ));
    }
    let backup = matches!(
        command,
        CommandBody::GetVaultLink
            | CommandBody::LinkVault { .. }
            | CommandBody::SetVaultLink { .. }
            | CommandBody::UnlinkVault
            | CommandBody::InstallVault
            | CommandBody::LinkLocalVault
            | CommandBody::PairVaultHost { .. }
            | CommandBody::RevokeVaultHost { .. }
            | CommandBody::SetVaultHostAccess { .. }
    );
    if backup && role != Role::Owner {
        return Err(forbidden(
            "backing up to a vault is for the daemon's owners only",
        ));
    }
    if matches!(
        command,
        CommandBody::ForkSession { .. } | CommandBody::UploadHistory { .. }
    ) && role != Role::Owner
    {
        return Err(forbidden(
            "forking sessions onto this host is for the daemon's owners only",
        ));
    }
    let skills = matches!(
        command,
        CommandBody::SetSkillsRepo { .. }
            | CommandBody::PutSkill { .. }
            | CommandBody::DeleteSkill { .. }
            | CommandBody::ImportSkill { .. }
            | CommandBody::PullSkills
            | CommandBody::SetSkillEnabled { .. }
    );
    if skills && role != Role::Owner {
        return Err(forbidden(
            "changing the skill library is for the daemon's owners only",
        ));
    }
    let people = matches!(
        command,
        CommandBody::SetUserRole { .. }
            | CommandBody::RevokeDevice { .. }
            | CommandBody::Invite { .. }
            | CommandBody::SetPersonRole { .. }
            | CommandBody::RevokePerson { .. }
            | CommandBody::ClaimUsers { .. }
            | CommandBody::DecideLeftover { .. }
            | CommandBody::PairDevice { user: Some(_), .. }
            | CommandBody::PairDevice { role: Some(_), .. }
    );
    if people && role != Role::Owner {
        return Err(forbidden(
            "changing who is on the daemon is for its owners only",
        ));
    }
    let bots = matches!(
        command,
        CommandBody::ListBots
            | CommandBody::PutBot { .. }
            | CommandBody::RemoveBot { .. }
            | CommandBody::GetBotStatus
            | CommandBody::GetBotMemory { .. }
            | CommandBody::SetBotFact { .. }
            | CommandBody::DeleteBotFact { .. }
            | CommandBody::ClearBotMemory { .. }
    );
    if bots && role != Role::Owner {
        return Err(forbidden("the daemon's bots are for its owners only"));
    }
    let secrets = matches!(
        command,
        CommandBody::CreateSecretSet { .. }
            | CommandBody::DeleteSecretSet { .. }
            | CommandBody::PutSecret { .. }
            | CommandBody::DeleteSecret { .. }
            | CommandBody::GrantSecretSet { .. }
            | CommandBody::RevokeSecretSet { .. }
            | CommandBody::ListSecrets
            | CommandBody::ListSecretAudit { .. }
    );
    if secrets && role != Role::Owner {
        return Err(forbidden(
            "managing secrets is for the vault's owners only; ask one of them to do it",
        ));
    }
    let store = matches!(
        command,
        CommandBody::GetSecretToStore { .. } | CommandBody::ConfirmSecretStored { .. }
    );
    if store && role != Role::Owner {
        return Err(forbidden(
            "storing an agent's secret is for the owners of this machine and its vault; ask \
             one of them to allow it",
        ));
    }
    Ok(())
}

/// A device's identity, with what it may connect as.
fn identity(users: &Users, fingerprint: &str) -> Option<(Identity, DeviceRole)> {
    let device = users.device(fingerprint)?;
    let (role, person_id) = users.member(&device.user_id)?;
    let identity = Identity {
        user_id: device.user_id.clone(),
        device_id: device.device_id.clone(),
        role,
        person_id,
    };
    Some((identity, device.role))
}

/// Notes that `connection`, from `device_id` as `peer`, is open, so revoking closes it.
fn connected(
    state: &mut State,
    device_id: &DeviceId,
    peer: DeviceRole,
    connection: &CancellationToken,
) {
    let open = state.connections.entry(device_id.clone()).or_default();
    open.retain(|(_, token)| !token.is_cancelled());
    open.push((peer, connection.clone()));
}

/// Closes every connection of the devices acting as `user_id`.
fn disconnect(state: &mut State, user_id: &UserId) {
    for device_id in state.users.devices_of(user_id) {
        for (_, token) in state.connections.remove(&device_id).unwrap_or_default() {
            token.cancel();
        }
    }
}

/// The devices acting as `user_id`, oldest first, as lists show them.
fn devices_of(state: &State, user_id: &UserId) -> Vec<UserDevice> {
    state
        .users
        .devices
        .iter()
        .filter(|device| &device.user_id == user_id)
        .map(|device| UserDevice {
            device_id: device.device_id.clone(),
            client: device.client.clone(),
            paired_at: device.paired_at,
            last_seen: device.last_seen,
            online: state
                .connections
                .get(&device.device_id)
                .is_some_and(|open| open.iter().any(|(_, token)| !token.is_cancelled())),
            host_only: device.role == DeviceRole::Host,
        })
        .collect()
}

/// A client device acting as `user_id`, paired now.
fn new_device(user_id: &UserId, fingerprint: &str, client: &str) -> Device {
    let now = Timestamp::now();
    Device {
        device_id: DeviceId::new(ulid::Ulid::generate().to_string()),
        user_id: user_id.clone(),
        client: client.to_owned(),
        fingerprint: fingerprint.to_owned(),
        paired_at: now,
        role: DeviceRole::Client,
        last_seen: Some(now),
    }
}

/// Adds `proven` as `role`; a person already here takes `role` and their passkey too.
fn add_person(users: &mut Users, proven: ProvenPerson, role: Role) {
    if let Some(person) = users
        .people
        .iter_mut()
        .find(|person| person.person_id == proven.person_id)
    {
        person.role = role;
        if !person
            .passkeys
            .iter()
            .any(|passkey| passkey.cred_id() == proven.passkey.cred_id())
        {
            person.passkeys.push(proven.passkey);
        }
        return;
    }
    users.people.push(Person {
        person_id: proven.person_id,
        name: proven.name,
        role,
        passkeys: vec![proven.passkey],
        created_at: Timestamp::now(),
    });
}

/// Notes an assertion on `person_id`'s passkeys; whether any changed.
fn note(users: &mut Users, person_id: &PersonId, result: &AuthenticationResult) -> bool {
    let Some(person) = users
        .people
        .iter_mut()
        .find(|person| &person.person_id == person_id)
    else {
        return false;
    };
    let mut changed = false;
    for passkey in &mut person.passkeys {
        changed |= passkey.update_credential(result) == Some(true);
    }
    changed
}

fn saving(err: anyhow::Error) -> ErrorInfo {
    tracing::error!("cannot save the people file: {err:#}");
    internal(&"cannot save who is on this daemon")
}

fn internal(err: &dyn std::fmt::Display) -> ErrorInfo {
    tracing::error!("{err}");
    ErrorInfo {
        code: ErrorCode::Internal,
        message: err.to_string(),
    }
}

fn last_owner() -> ErrorInfo {
    conflict("the last owner cannot be removed or made a member; make someone else an owner first")
}

fn bad_request(message: &str) -> ErrorInfo {
    ErrorInfo {
        code: ErrorCode::BadRequest,
        message: message.to_owned(),
    }
}

fn not_found(message: &str) -> ErrorInfo {
    ErrorInfo {
        code: ErrorCode::NotFound,
        message: message.to_owned(),
    }
}

fn conflict(message: &str) -> ErrorInfo {
    ErrorInfo {
        code: ErrorCode::Conflict,
        message: message.to_owned(),
    }
}

/// A user name as given, trimmed.
fn user_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        bail!("a user name is 1 to 64 printable characters");
    }
    Ok(name)
}

/// The refusal of a client connection from a device of the host-only `host`, worded for the
/// person handing a session off to it, to whom the host relays it.
fn host_only(host: &str) -> ErrorInfo {
    forbidden(&format!(
        "{host} only backs its sessions up to the vault, so it cannot read other machines' \
         sessions; turn on \"Takes handoffs\" for it on the vault's page in the app"
    ))
}

fn forbidden(message: &str) -> ErrorInfo {
    ErrorInfo {
        code: ErrorCode::Forbidden,
        message: message.to_owned(),
    }
}

fn describe(role: Role) -> &'static str {
    match role {
        Role::Owner => "an owner",
        Role::Member => "a member",
    }
}

/// A random code, formatted `XXXXX-XXXXX`.
fn new_code() -> Result<String> {
    random_code(CODE_LEN, 5)
}

/// A random code of `len` characters, in groups of `group` joined by dashes.
fn random_code(len: usize, group: usize) -> Result<String> {
    let mut bytes = vec![0u8; len];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("the system random number generator failed"))?;
    let chars: Vec<char> = bytes
        .iter()
        .map(|byte| char::from(CODE_ALPHABET[usize::from(*byte) % CODE_ALPHABET.len()]))
        .collect();
    Ok(chars
        .chunks(group)
        .map(|chunk| chunk.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-"))
}

/// A code as typed, without separators or case.
fn normalize(code: &str) -> String {
    code.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> (tempfile::TempDir, Auth) {
        let tmp = tempfile::tempdir().unwrap();
        let auth = Auth::open(tmp.path()).unwrap();
        (tmp, auth)
    }

    fn pair(auth: &Auth, fingerprint: &str, code: &str) -> Result<Identity, ErrorInfo> {
        auth.authenticate(
            fingerprint,
            Some(code),
            "test",
            DeviceRole::Client,
            &CancellationToken::new(),
        )
    }

    #[test]
    fn codes_are_two_groups_of_crockford_base32() {
        let code = new_code().unwrap();
        assert_eq!(code.len(), CODE_LEN + 1);
        assert_eq!(&code[5..6], "-");
        assert!(normalize(&code).bytes().all(|b| CODE_ALPHABET.contains(&b)));
        assert_ne!(code, new_code().unwrap());
        assert_eq!(normalize(" abcde-fghjk "), "ABCDEFGHJK");
    }

    #[test]
    fn the_first_user_is_the_owner() {
        let (_tmp, auth) = open();
        let err = auth
            .mint("bob", Some(Role::Member), PAIRING_TTL)
            .unwrap_err();
        assert!(err.to_string().contains("first user"), "{err}");
        assert_eq!(auth.owner(), None);
        let pairing = auth.mint("alice", None, PAIRING_TTL).unwrap();
        assert_eq!(pairing.role, Role::Owner);
        let alice = pair(&auth, "fp-a", &pairing.code).unwrap();
        assert_eq!(alice.role, Role::Owner);
        assert_eq!(auth.owner().as_ref(), Some(&alice.user_id));

        // Later users default to member; an existing user keeps their role.
        let bob = auth.mint("bob", None, PAIRING_TTL).unwrap();
        assert_eq!(bob.role, Role::Member);
        pair(&auth, "fp-b", &bob.code).unwrap();
        assert_eq!(auth.owner().as_ref(), Some(&alice.user_id));
        assert_eq!(
            auth.mint("alice", None, PAIRING_TTL).unwrap().role,
            Role::Owner
        );
        let err = auth
            .mint("alice", Some(Role::Member), PAIRING_TTL)
            .unwrap_err();
        assert!(err.to_string().contains("already an owner"), "{err}");
        assert!(auth.mint(" ", None, PAIRING_TTL).is_err());
    }

    #[test]
    fn a_second_device_joins_its_existing_user() {
        let (_tmp, auth) = open();
        let first = pair(
            &auth,
            "fp-a",
            &auth.mint("alice", None, PAIRING_TTL).unwrap().code,
        );
        let second = pair(
            &auth,
            "fp-b",
            &auth.mint("alice", None, PAIRING_TTL).unwrap().code,
        );
        let (first, second) = (first.unwrap(), second.unwrap());
        assert_eq!(first.user_id, second.user_id);
        assert_ne!(first.device_id, second.device_id);
    }

    #[test]
    fn users_and_devices_survive_a_restart_but_codes_do_not() {
        let (tmp, auth) = open();
        let code = auth.mint("alice", None, PAIRING_TTL).unwrap().code;
        let alice = pair(&auth, "fp-a", &code).unwrap();
        let pending = auth.mint("alice", None, PAIRING_TTL).unwrap().code;
        drop(auth);

        let auth = Auth::open(tmp.path()).unwrap();
        let token = CancellationToken::new();
        assert_eq!(
            auth.authenticate("fp-a", None, "test", DeviceRole::Client, &token),
            Ok(alice)
        );
        assert!(pair(&auth, "fp-b", &pending).is_err());
        assert_eq!(auth.devices().len(), 1);
    }

    #[test]
    fn revoking_a_device_refuses_it_and_closes_its_connections() {
        let (_tmp, auth) = open();
        let code = auth.mint("alice", None, PAIRING_TTL).unwrap().code;
        let connection = CancellationToken::new();
        let alice = auth
            .authenticate("fp-a", Some(&code), "test", DeviceRole::Client, &connection)
            .unwrap();
        assert!(auth.revoke(&alice.device_id).unwrap());
        assert!(connection.is_cancelled());
        assert!(!auth.revoke(&alice.device_id).unwrap());
        let refused = auth.authenticate(
            "fp-a",
            None,
            "test",
            DeviceRole::Client,
            &CancellationToken::new(),
        );
        assert_eq!(refused.unwrap_err().code, ErrorCode::Forbidden);
    }

    fn connect(auth: &Auth, fingerprint: &str, peer: DeviceRole) -> Result<Identity, ErrorInfo> {
        auth.authenticate(fingerprint, None, "test", peer, &CancellationToken::new())
    }

    #[test]
    fn a_host_device_replicates_but_never_connects_as_a_client() {
        let (_tmp, auth) = open();
        pair(
            &auth,
            "fp-a",
            &auth.mint("alice", None, PAIRING_TTL).unwrap().code,
        )
        .unwrap();
        let pairing = auth.mint_host("devbox", PAIRING_TTL).unwrap();
        assert_eq!(
            (pairing.user.as_str(), pairing.role, pairing.device_role),
            ("devbox", Role::Member, DeviceRole::Host)
        );

        // A host code does not pair a client, and is not used up by trying.
        let refused = pair(&auth, "fp-h", &pairing.code).unwrap_err();
        assert_eq!(refused.code, ErrorCode::Forbidden);
        assert!(refused.message.contains("pairs a host"), "{refused:?}");
        let host = auth
            .authenticate(
                "fp-h",
                Some(&pairing.code),
                "test",
                DeviceRole::Host,
                &CancellationToken::new(),
            )
            .unwrap();
        assert_eq!(connect(&auth, "fp-h", DeviceRole::Host), Ok(host));
        let stolen = connect(&auth, "fp-h", DeviceRole::Client).unwrap_err();
        assert_eq!(stolen.code, ErrorCode::Forbidden);
        assert!(
            stolen.message.starts_with("devbox only backs"),
            "{stolen:?}"
        );

        // A client device connects as either.
        assert!(connect(&auth, "fp-a", DeviceRole::Client).is_ok());
        assert!(connect(&auth, "fp-a", DeviceRole::Host).is_ok());
        let roles: Vec<_> = auth.devices().iter().map(|(d, _)| d.role).collect();
        assert_eq!(roles, [DeviceRole::Client, DeviceRole::Host]);
    }

    #[test]
    fn a_host_device_reads_once_let_and_stops_when_no_longer_let() {
        let (tmp, auth) = open();
        let code = auth.mint_host("devbox", PAIRING_TTL).unwrap().code;
        let replication = CancellationToken::new();
        let host = auth
            .authenticate("fp-h", Some(&code), "test", DeviceRole::Host, &replication)
            .unwrap();
        assert_eq!(auth.device_role(&host.device_id), Some(DeviceRole::Host));

        assert!(
            auth.set_device_role(&host.device_id, DeviceRole::Client)
                .unwrap()
        );
        let reading = CancellationToken::new();
        let client = auth
            .authenticate("fp-h", None, "test", DeviceRole::Client, &reading)
            .unwrap();
        assert_eq!(client, host);
        // Kept across a restart.
        let auth = Auth::open(tmp.path()).unwrap();
        assert_eq!(auth.device_role(&host.device_id), Some(DeviceRole::Client));
        let reading = CancellationToken::new();
        let replication = CancellationToken::new();
        auth.authenticate("fp-h", None, "test", DeviceRole::Client, &reading)
            .unwrap();
        auth.authenticate("fp-h", None, "test", DeviceRole::Host, &replication)
            .unwrap();

        // Taking it back closes the client connection, not the replication.
        assert!(
            auth.set_device_role(&host.device_id, DeviceRole::Host)
                .unwrap()
        );
        assert!(reading.is_cancelled());
        assert!(!replication.is_cancelled());
        assert!(connect(&auth, "fp-h", DeviceRole::Client).is_err());
        assert!(connect(&auth, "fp-h", DeviceRole::Host).is_ok());

        let unknown = DeviceId::new("nobody");
        assert!(!auth.set_device_role(&unknown, DeviceRole::Client).unwrap());
        assert_eq!(auth.device_role(&unknown), None);
    }

    #[test]
    fn a_shared_code_pairs_as_the_sharer_with_their_role() {
        let (_tmp, auth) = open();
        let alice = pair(
            &auth,
            "fp-a",
            &auth.mint("alice", None, PAIRING_TTL).unwrap().code,
        )
        .unwrap();
        let bob = pair(
            &auth,
            "fp-b",
            &auth.mint("bob", None, PAIRING_TTL).unwrap().code,
        )
        .unwrap();
        for (sharer, role, phone) in [
            (&alice, Role::Owner, "fp-a2"),
            (&bob, Role::Member, "fp-b2"),
        ] {
            let pairing = auth.mint_for(&sharer.user_id, PAIRING_TTL).unwrap();
            assert_eq!(
                (pairing.role, pairing.device_role),
                (role, DeviceRole::Client)
            );
            let paired = pair(&auth, phone, &pairing.code).unwrap();
            assert_eq!((&paired.user_id, paired.role), (&sharer.user_id, role));
            assert_ne!(paired.device_id, sharer.device_id);
            // Once only.
            assert!(pair(&auth, "fp-x", &pairing.code).is_err());
        }
        // Each device keeps its own credential: revoking the new one leaves the sharer's.
        let phone = connect(&auth, "fp-b2", DeviceRole::Client).unwrap();
        assert!(auth.revoke(&phone.device_id).unwrap());
        assert!(connect(&auth, "fp-b", DeviceRole::Client).is_ok());

        let expired = auth.mint_for(&alice.user_id, Duration::ZERO).unwrap();
        assert_eq!(
            pair(&auth, "fp-y", &expired.code).unwrap_err().code,
            ErrorCode::Forbidden
        );
        assert!(auth.mint_for(&UserId::new("nobody"), PAIRING_TTL).is_err());
    }

    #[test]
    fn users_of_hosts_alone_do_not_make_the_first_owner() {
        let (_tmp, auth) = open();
        let host = auth.mint_host("devbox", PAIRING_TTL).unwrap().code;
        auth.authenticate(
            "fp-h",
            Some(&host),
            "test",
            DeviceRole::Host,
            &CancellationToken::new(),
        )
        .unwrap();
        assert!(auth.mint("bob", Some(Role::Member), PAIRING_TTL).is_err());
        assert_eq!(
            auth.mint("alice", None, PAIRING_TTL).unwrap().role,
            Role::Owner
        );
    }

    #[test]
    fn migration_makes_devices_that_replicated_host_only_once() {
        let (tmp, auth) = open();
        let laptop = pair(
            &auth,
            "fp-a",
            &auth.mint("alice", None, PAIRING_TTL).unwrap().code,
        );
        let devbox = pair(
            &auth,
            "fp-h",
            &auth.mint("devbox", None, PAIRING_TTL).unwrap().code,
        );
        let (laptop, devbox) = (laptop.unwrap(), devbox.unwrap());
        drop(auth);
        // A file from before roles: no version, no device roles.
        let path = tmp.path().join(FILE);
        let mut file: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        file.as_object_mut().unwrap().remove("version");
        for device in file["devices"].as_array_mut().unwrap() {
            device.as_object_mut().unwrap().remove("role");
        }
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();

        let auth = Auth::open(tmp.path()).unwrap();
        assert!(connect(&auth, "fp-h", DeviceRole::Client).is_ok());
        let hosts = [devbox.device_id.clone()];
        assert_eq!(auth.migrate_device_roles(&hosts).unwrap(), hosts);
        assert!(connect(&auth, "fp-h", DeviceRole::Client).is_err());
        assert!(connect(&auth, "fp-h", DeviceRole::Host).is_ok());
        assert!(connect(&auth, "fp-a", DeviceRole::Client).is_ok());
        drop(auth);

        // Once only: a client that replicates later stays a client, across restarts.
        let auth = Auth::open(tmp.path()).unwrap();
        assert!(connect(&auth, "fp-h", DeviceRole::Client).is_err());
        let both = [laptop.device_id, devbox.device_id];
        assert!(auth.migrate_device_roles(&both).unwrap().is_empty());
        assert!(connect(&auth, "fp-a", DeviceRole::Client).is_ok());
    }

    #[test]
    fn a_new_daemon_has_nothing_to_migrate() {
        let (_tmp, auth) = open();
        let code = auth.mint("alice", None, PAIRING_TTL).unwrap().code;
        let alice = pair(&auth, "fp-a", &code).unwrap();
        assert!(
            auth.migrate_device_roles(&[alice.device_id])
                .unwrap()
                .is_empty()
        );
        assert!(connect(&auth, "fp-a", DeviceRole::Client).is_ok());
    }
    #[test]
    fn only_owners_may_configure_accounts() {
        let (_tmp, auth) = open();
        let code = auth.mint("alice", None, PAIRING_TTL).unwrap().code;
        let mut alice = pair(&auth, "fp-a", &code).unwrap();
        let command = CommandBody::SetAccountSettings {
            account_id: herder_protocol::AccountId::new("work"),
            label: "Work".into(),
            config_dir: None,
            fallback: None,
        };
        assert!(authorize(alice.role, &command).is_ok());
        alice.role = Role::Member;
        assert_eq!(
            authorize(alice.role, &command).unwrap_err().code,
            ErrorCode::Forbidden
        );
    }

    #[test]
    fn only_owners_may_install_a_provider() {
        let (_tmp, auth) = open();
        let code = auth.mint("alice", None, PAIRING_TTL).unwrap().code;
        let mut alice = pair(&auth, "fp-a", &code).unwrap();
        let command = CommandBody::InstallProvider {
            provider: herder_protocol::Provider::Cursor,
            cols: 80,
            rows: 24,
        };
        assert!(authorize(alice.role, &command).is_ok());
        alice.role = Role::Member;
        assert_eq!(
            authorize(alice.role, &command).unwrap_err().code,
            ErrorCode::Forbidden
        );
    }

    #[test]
    fn only_owners_may_close_a_terminal() {
        let (_tmp, auth) = open();
        let code = auth.mint("alice", None, PAIRING_TTL).unwrap().code;
        let mut alice = pair(&auth, "fp-a", &code).unwrap();
        let command = CommandBody::CloseTerminal {
            terminal_id: herder_protocol::TerminalId::new("t1"),
        };
        assert!(authorize(alice.role, &command).is_ok());
        alice.role = Role::Member;
        assert_eq!(
            authorize(alice.role, &command).unwrap_err().code,
            ErrorCode::Forbidden
        );
    }

    #[test]
    fn only_owners_may_log_an_account_in_again() {
        let (_tmp, auth) = open();
        let code = auth.mint("alice", None, PAIRING_TTL).unwrap().code;
        let mut alice = pair(&auth, "fp-a", &code).unwrap();
        let command = CommandBody::LogInAccount {
            account_id: herder_protocol::AccountId::new("work"),
            cols: 80,
            rows: 24,
        };
        assert!(authorize(alice.role, &command).is_ok());
        alice.role = Role::Member;
        assert_eq!(
            authorize(alice.role, &command).unwrap_err().code,
            ErrorCode::Forbidden
        );
    }

    #[test]
    fn only_owners_may_read_or_change_the_settings_or_restart() {
        let (tmp, auth) = open();
        let code = auth.mint("alice", None, PAIRING_TTL).unwrap().code;
        let mut alice = pair(&auth, "fp-a", &code).unwrap();
        let settings = crate::Config::load_file(&tmp.path().join("none.toml"))
            .unwrap()
            .settings();
        let commands = [
            CommandBody::GetSettings,
            CommandBody::SetSettings {
                settings: Box::new(settings),
            },
            CommandBody::SetResourceLimits { max_turns: 4 },
            CommandBody::RestartDaemon,
            CommandBody::UpdateDaemon {
                when: herder_protocol::UpdateWhen::Idle,
            },
        ];
        for command in &commands {
            assert!(authorize(alice.role, command).is_ok());
        }
        alice.role = Role::Member;
        for command in &commands {
            assert_eq!(
                authorize(alice.role, command).unwrap_err().code,
                ErrorCode::Forbidden
            );
        }
    }

    #[test]
    fn only_owners_may_list_put_remove_or_watch_bots_or_change_their_memory() {
        let (_tmp, auth) = open();
        let code = auth.mint("alice", None, PAIRING_TTL).unwrap().code;
        let mut alice = pair(&auth, "fp-a", &code).unwrap();
        let bot_id = || herder_protocol::BotId::new("trash-can");
        let commands = [
            CommandBody::ListBots,
            CommandBody::PutBot {
                bot: Box::new(herder_protocol::Bot {
                    bot_id: bot_id(),
                    kind: herder_protocol::BotKind::Slack,
                    name: "trash-can".into(),
                    token: "BOT_TOKEN".parse().unwrap(),
                    app_token: Some("APP_TOKEN".parse().unwrap()),
                    allow: Vec::new(),
                    defaults: herder_protocol::BotDefaults::default(),
                    channel_bindings: Vec::new(),
                    instructions: None,
                }),
            },
            CommandBody::RemoveBot { bot_id: bot_id() },
            CommandBody::GetBotMemory { bot_id: bot_id() },
            CommandBody::SetBotFact {
                bot_id: bot_id(),
                fact_id: None,
                text: "Ada leads payments".into(),
            },
            CommandBody::DeleteBotFact {
                bot_id: bot_id(),
                fact_id: herder_protocol::FactId::new("01J9FACT"),
            },
            CommandBody::ClearBotMemory { bot_id: bot_id() },
            CommandBody::GetBotStatus,
        ];
        for command in &commands {
            assert!(authorize(alice.role, command).is_ok());
        }
        alice.role = Role::Member;
        for command in &commands {
            assert_eq!(
                authorize(alice.role, command).unwrap_err().code,
                ErrorCode::Forbidden
            );
        }
    }

    #[test]
    fn only_owners_may_manage_secrets() {
        let (_tmp, auth) = open();
        let code = auth.mint("alice", None, PAIRING_TTL).unwrap().code;
        let mut alice = pair(&auth, "fp-a", &code).unwrap();
        let (set, host_id) = (|| "prod".to_owned(), || herder_protocol::HostId::new("h1"));
        let commands = [
            CommandBody::CreateSecretSet { set: set() },
            CommandBody::DeleteSecretSet { set: set() },
            CommandBody::PutSecret {
                set: set(),
                name: "STRIPE_KEY".into(),
                description: "Stripe".into(),
                value: herder_protocol::SecretValue::new("sk").unwrap(),
            },
            CommandBody::DeleteSecret {
                set: set(),
                name: "STRIPE_KEY".into(),
            },
            CommandBody::GrantSecretSet {
                set: set(),
                host_id: host_id(),
                host_use: false,
            },
            CommandBody::RevokeSecretSet {
                set: set(),
                host_id: host_id(),
                host_use: false,
            },
            CommandBody::ListSecrets,
            CommandBody::ListSecretAudit { before: None },
        ];
        for command in &commands {
            assert!(authorize(alice.role, command).is_ok());
        }
        alice.role = Role::Member;
        for command in &commands {
            assert_eq!(
                authorize(alice.role, command).unwrap_err().code,
                ErrorCode::Forbidden
            );
        }
    }
}
