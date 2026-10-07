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
//! refused, so its key, if stolen, reads nothing.
//!
//! Users and devices persist in `<data_dir>/auth.json`, written atomically on every change.

pub mod control;

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use herder_protocol::{CommandBody, DeviceId, ErrorCode, ErrorInfo, Role, Timestamp, UserId};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::data_dir::write_private;
use crate::ws::Identity;

/// How long a pairing code stays valid.
pub const PAIRING_TTL: Duration = Duration::from_secs(10 * 60);

/// Crockford's base32 alphabet: no I, L, O or U to misread. 256 is a multiple of its length, so
/// a random byte maps onto it without bias.
const CODE_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Characters in a code: 50 bits, against at most ten minutes of online guessing.
const CODE_LEN: usize = 10;

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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Users {
    /// [`VERSION`] once migrated; files from before versions have none.
    #[serde(default)]
    version: u32,
    users: Vec<User>,
    devices: Vec<Device>,
}

impl Users {
    fn user(&self, name: &str) -> Option<&User> {
        self.users.iter().find(|user| user.name == name)
    }
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
    /// Cancels the open connections of each device, so revoking one disconnects it.
    connections: HashMap<DeviceId, Vec<CancellationToken>>,
}

/// The daemon's users, devices and pending pairing codes.
pub struct Auth {
    dir: PathBuf,
    state: Mutex<State>,
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
            }),
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
            Some((identity, role)) if role.allows(peer) => identity,
            Some(_) => return Err(host_only()),
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
        let open = state
            .connections
            .entry(identity.device_id.clone())
            .or_default();
        open.retain(|token| !token.is_cancelled());
        open.push(connection.clone());
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
            return Err(host_only());
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
                    user_id: UserId::new(ulid::Ulid::new().to_string()),
                    name: pending.user.clone(),
                    role: pending.role,
                    created_at: now,
                };
                users.users.push(user.clone());
                user
            }
        };
        let device = Device {
            device_id: DeviceId::new(ulid::Ulid::new().to_string()),
            user_id: user.user_id.clone(),
            client: client.to_owned(),
            fingerprint: fingerprint.to_owned(),
            paired_at: now,
            role: pending.device_role,
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
        })
    }

    /// The daemon's first owner, which the local control socket acts as: only the system user
    /// running the daemon reaches it. `None` until an owner pairs.
    pub fn owner(&self) -> Option<UserId> {
        self.lock()
            .users
            .users
            .iter()
            .filter(|user| user.role == Role::Owner)
            .min_by_key(|user| user.created_at)
            .map(|user| user.user_id.clone())
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
        for token in state.connections.remove(device_id).unwrap_or_default() {
            token.cancel();
        }
        info!(%device_id, "device revoked");
        Ok(true)
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

/// Refuses commands the identity's role does not allow: terminals, and so adding accounts or
/// logging them in again, bringing down containers, browsing folders, changing projects,
/// reading or changing the daemon's settings, restarting it, backing up to a vault, forking
/// sessions onto the host, and changing the skill library or which of its skills are enabled are for owners only.
pub fn authorize(identity: &Identity, command: &CommandBody) -> Result<(), ErrorInfo> {
    let terminal = matches!(
        command,
        CommandBody::OpenTerminal { .. }
            | CommandBody::AddAccount { .. }
            | CommandBody::LogInAccount { .. }
            | CommandBody::AttachTerminal { .. }
            | CommandBody::DetachTerminal { .. }
            | CommandBody::ResizeTerminal { .. }
            | CommandBody::TerminalInput { .. }
    );
    if terminal && identity.role != Role::Owner {
        return Err(forbidden("terminals are for the daemon's owners only"));
    }
    if matches!(command, CommandBody::ComposeDown { .. }) && identity.role != Role::Owner {
        return Err(forbidden(
            "bringing down containers is for the daemon's owners only",
        ));
    }
    if matches!(command, CommandBody::SetAccountSettings { .. }) && identity.role != Role::Owner {
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
    );
    if settings && identity.role != Role::Owner {
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
    if host && identity.role != Role::Owner {
        return Err(forbidden(
            "browsing folders and changing projects are for the daemon's owners only",
        ));
    }
    let backup = matches!(
        command,
        CommandBody::GetVaultLink
            | CommandBody::LinkVault { .. }
            | CommandBody::UnlinkVault
            | CommandBody::PairVaultHost { .. }
            | CommandBody::RevokeVaultHost { .. }
    );
    if backup && identity.role != Role::Owner {
        return Err(forbidden(
            "backing up to a vault is for the daemon's owners only",
        ));
    }
    if matches!(
        command,
        CommandBody::ForkSession { .. } | CommandBody::UploadHistory { .. }
    ) && identity.role != Role::Owner
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
    if skills && identity.role != Role::Owner {
        return Err(forbidden(
            "changing the skill library is for the daemon's owners only",
        ));
    }
    Ok(())
}

/// A device's identity, with what it may connect as.
fn identity(users: &Users, fingerprint: &str) -> Option<(Identity, DeviceRole)> {
    let device = users
        .devices
        .iter()
        .find(|device| device.fingerprint == fingerprint)?;
    let user = users
        .users
        .iter()
        .find(|user| user.user_id == device.user_id)?;
    let identity = Identity {
        user_id: user.user_id.clone(),
        device_id: device.device_id.clone(),
        role: user.role,
    };
    Some((identity, device.role))
}

/// A user name as given, trimmed.
fn user_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        bail!("a user name is 1 to 64 printable characters");
    }
    Ok(name)
}

fn host_only() -> ErrorInfo {
    forbidden(
        "this device is paired as a host: it may only replicate its sessions to the vault, \
         not read it; pair a client with `herder pair` on the vault",
    )
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
    let mut bytes = [0u8; CODE_LEN];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("the system random number generator failed"))?;
    let chars: String = bytes
        .iter()
        .map(|byte| char::from(CODE_ALPHABET[usize::from(*byte) % CODE_ALPHABET.len()]))
        .collect();
    let (first, second) = chars.split_at(CODE_LEN / 2);
    Ok(format!("{first}-{second}"))
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
        assert!(refused.message.contains("paired as a host"), "{refused:?}");
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

        // A client device connects as either.
        assert!(connect(&auth, "fp-a", DeviceRole::Client).is_ok());
        assert!(connect(&auth, "fp-a", DeviceRole::Host).is_ok());
        let roles: Vec<_> = auth.devices().iter().map(|(d, _)| d.role).collect();
        assert_eq!(roles, [DeviceRole::Client, DeviceRole::Host]);
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
        };
        assert!(authorize(&alice, &command).is_ok());
        alice.role = Role::Member;
        assert_eq!(
            authorize(&alice, &command).unwrap_err().code,
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
        assert!(authorize(&alice, &command).is_ok());
        alice.role = Role::Member;
        assert_eq!(
            authorize(&alice, &command).unwrap_err().code,
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
        ];
        for command in &commands {
            assert!(authorize(&alice, command).is_ok());
        }
        alice.role = Role::Member;
        for command in &commands {
            assert_eq!(
                authorize(&alice, command).unwrap_err().code,
                ErrorCode::Forbidden
            );
        }
    }
}
