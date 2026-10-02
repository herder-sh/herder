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
    /// When the code stops working.
    pub expires_at: Timestamp,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Users {
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
            Err(err) if err.kind() == io::ErrorKind::NotFound => Users::default(),
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

    /// Mints a one-time code that pairs a device as `user`, valid for `ttl`.
    ///
    /// An existing user keeps their role, so `role` must be absent or equal to it. A new user
    /// defaults to member, except that the first user of a daemon is always its owner.
    pub fn mint(&self, user: &str, role: Option<Role>, ttl: Duration) -> Result<Pairing> {
        let user = user.trim();
        if user.is_empty() || user.chars().count() > 64 || user.chars().any(char::is_control) {
            bail!("a user name is 1 to 64 printable characters");
        }
        let mut state = self.lock();
        let role = match (state.users.user(user), role) {
            (Some(existing), None) => existing.role,
            (Some(existing), Some(role)) if role == existing.role => role,
            (Some(existing), Some(_)) => {
                bail!("{user} is already {}", describe(existing.role))
            }
            (None, Some(Role::Member)) if state.users.users.is_empty() => {
                bail!("the first user of a daemon is its owner; pair the owner first")
            }
            (None, role) if state.users.users.is_empty() => role.unwrap_or(Role::Owner),
            (None, role) => role.unwrap_or(Role::Member),
        };
        let code = new_code()?;
        let now = Instant::now();
        state.codes.retain(|_, pending| pending.expires > now);
        state.codes.insert(
            normalize(&code),
            Pending {
                user: user.to_owned(),
                role,
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
            expires_at,
        })
    }

    /// Decides who a connection from the device with certificate `fingerprint` acts as.
    ///
    /// A paired device is its user. An unpaired one pairs with `code`, which is used up; without
    /// a valid code it is refused. `connection` is cancelled if the device is revoked.
    pub fn authenticate(
        &self,
        fingerprint: &str,
        code: Option<&str>,
        client: &str,
        connection: &CancellationToken,
    ) -> Result<Identity, ErrorInfo> {
        let mut state = self.lock();
        let identity = match identity(&state.users, fingerprint) {
            Some(identity) => identity,
            None => {
                let Some(code) = code else {
                    return Err(forbidden(
                        "this device is not paired with this daemon; run `herder pair` on the \
                         daemon's machine and pair with the code it prints",
                    ));
                };
                self.pair(&mut state, fingerprint, code, client)?
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
    ) -> Result<Identity, ErrorInfo> {
        let key = normalize(code);
        let refused = || forbidden("the pairing code is invalid, expired or already used");
        let pending = state.codes.get(&key).ok_or_else(refused)?;
        if pending.expires <= Instant::now() {
            state.codes.remove(&key);
            return Err(refused());
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
        info!(user = %user.name, device_id = %device.device_id, "device paired");
        Ok(Identity {
            user_id: user.user_id,
            device_id: device.device_id,
            role: user.role,
        })
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

/// Refuses commands the identity's role does not allow: terminals, and so adding accounts,
/// and bringing down containers are for owners only.
pub fn authorize(identity: &Identity, command: &CommandBody) -> Result<(), ErrorInfo> {
    let terminal = matches!(
        command,
        CommandBody::OpenTerminal { .. }
            | CommandBody::AddAccount { .. }
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
    Ok(())
}

fn identity(users: &Users, fingerprint: &str) -> Option<Identity> {
    let device = users
        .devices
        .iter()
        .find(|device| device.fingerprint == fingerprint)?;
    let user = users
        .users
        .iter()
        .find(|user| user.user_id == device.user_id)?;
    Some(Identity {
        user_id: user.user_id.clone(),
        device_id: device.device_id.clone(),
        role: user.role,
    })
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
        auth.authenticate(fingerprint, Some(code), "test", &CancellationToken::new())
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
        let pairing = auth.mint("alice", None, PAIRING_TTL).unwrap();
        assert_eq!(pairing.role, Role::Owner);
        let alice = pair(&auth, "fp-a", &pairing.code).unwrap();
        assert_eq!(alice.role, Role::Owner);

        // Later users default to member; an existing user keeps their role.
        assert_eq!(
            auth.mint("bob", None, PAIRING_TTL).unwrap().role,
            Role::Member
        );
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
        assert_eq!(auth.authenticate("fp-a", None, "test", &token), Ok(alice));
        assert!(pair(&auth, "fp-b", &pending).is_err());
        assert_eq!(auth.devices().len(), 1);
    }

    #[test]
    fn revoking_a_device_refuses_it_and_closes_its_connections() {
        let (_tmp, auth) = open();
        let code = auth.mint("alice", None, PAIRING_TTL).unwrap().code;
        let connection = CancellationToken::new();
        let alice = auth
            .authenticate("fp-a", Some(&code), "test", &connection)
            .unwrap();
        assert!(auth.revoke(&alice.device_id).unwrap());
        assert!(connection.is_cancelled());
        assert!(!auth.revoke(&alice.device_id).unwrap());
        let refused = auth.authenticate("fp-a", None, "test", &CancellationToken::new());
        assert_eq!(refused.unwrap_err().code, ErrorCode::Forbidden);
    }
}
