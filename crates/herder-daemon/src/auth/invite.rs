//! Invites: an owner mints one for a role, and whoever holds it joins this daemon once as a
//! person with that role, registering their own passkey (`accept_invite`).
//!
//! A token is 256 random bits; only its SHA-256 is kept, in `<data_dir>/invites.json`, so the
//! file alone lets nobody join. Invites survive a restart, as the person they were sent to may
//! take days to open them.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use herder_protocol::{PersonId, Role, Timestamp};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::data_dir::write_private;

/// How long an invite stays valid.
pub const INVITE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

const FILE: &str = "invites.json";

/// A freshly minted invite.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Minted {
    /// The token, base64url: shown once, never stored.
    pub token: String,
    /// The role the invitee gets.
    pub role: Role,
    /// When it stops working.
    pub expires_at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Invite {
    /// SHA-256 of the token, lowercase hex.
    hash: String,
    role: Role,
    /// The owner who minted it.
    by: PersonId,
    expires_at: Timestamp,
}

/// This daemon's unused invites.
pub struct Invites {
    dir: PathBuf,
    invites: Mutex<Vec<Invite>>,
}

impl Invites {
    /// Loads the invites from `<dir>/invites.json`; a missing file means none.
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join(FILE);
        let invites = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("{} is not a valid invites file", path.display()))?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self {
            dir: dir.to_owned(),
            invites: Mutex::new(invites),
        })
    }

    /// Mints an invite for `role`, by the owner `by`, valid for `ttl`.
    pub fn mint(&self, role: Role, by: &PersonId, ttl: Duration) -> Result<Minted> {
        let mut bytes = [0u8; 32];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| anyhow::anyhow!("the system random number generator failed"))?;
        let token = URL_SAFE_NO_PAD.encode(bytes);
        let expires_at = Timestamp::now()
            .checked_add(ttl)
            .context("the invite would expire too far in the future")?;
        let mut invites = self.lock();
        let mut kept = live(&invites);
        kept.push(Invite {
            hash: hash(&token),
            role,
            by: by.clone(),
            expires_at,
        });
        self.save(&kept)?;
        *invites = kept;
        Ok(Minted {
            token,
            role,
            expires_at,
        })
    }

    /// The role `token` invites to, using it up; `None` for a token that is unknown, expired
    /// or already used.
    pub fn redeem(&self, token: &str) -> Result<Option<Role>> {
        let hash = hash(token.trim());
        let mut invites = self.lock();
        let mut kept = live(&invites);
        let Some(at) = kept.iter().position(|invite| invite.hash == hash) else {
            return Ok(None);
        };
        let invite = kept.remove(at);
        self.save(&kept)?;
        *invites = kept;
        Ok(Some(invite.role))
    }

    /// Drops the unused invites `by` minted: an owner who is removed or made a member takes
    /// their invites with them.
    pub fn forget_by(&self, by: &PersonId) -> Result<()> {
        let mut invites = self.lock();
        let kept: Vec<_> = live(&invites)
            .into_iter()
            .filter(|invite| &invite.by != by)
            .collect();
        if kept.len() == invites.len() {
            return Ok(());
        }
        self.save(&kept)?;
        *invites = kept;
        Ok(())
    }

    fn save(&self, invites: &[Invite]) -> Result<()> {
        let json = serde_json::to_vec_pretty(invites).context("encoding the invites file")?;
        write_private(&self.dir, FILE, &json)?;
        File::open(&self.dir)
            .and_then(|dir| dir.sync_all())
            .with_context(|| format!("syncing {}", self.dir.display()))
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Invite>> {
        // Every update swaps in a fully built list, so a poisoned one is still consistent.
        self.invites.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The unexpired `invites`.
fn live(invites: &[Invite]) -> Vec<Invite> {
    let now = Timestamp::now();
    invites
        .iter()
        .filter(|invite| invite.expires_at > now)
        .cloned()
        .collect()
}

fn hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner() -> PersonId {
        PersonId::new("0190c6a2-0000-7000-8000-000000000001")
    }

    #[test]
    fn an_invite_works_once_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let minted = Invites::open(dir.path())
            .unwrap()
            .mint(Role::Member, &owner(), INVITE_TTL)
            .unwrap();
        let stored = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(!stored.contains(&minted.token), "only the hash is kept");

        let invites = Invites::open(dir.path()).unwrap();
        assert_eq!(invites.redeem("not-a-token").unwrap(), None);
        assert_eq!(invites.redeem(&minted.token).unwrap(), Some(Role::Member));
        assert_eq!(invites.redeem(&minted.token).unwrap(), None);
        let reopened = Invites::open(dir.path()).unwrap();
        assert_eq!(reopened.redeem(&minted.token).unwrap(), None);
    }

    #[test]
    fn an_expired_invite_does_not_work() {
        let dir = tempfile::tempdir().unwrap();
        let invites = Invites::open(dir.path()).unwrap();
        let minted = invites.mint(Role::Owner, &owner(), Duration::ZERO).unwrap();
        assert_eq!(invites.redeem(&minted.token).unwrap(), None);
    }

    #[test]
    fn an_owner_who_leaves_takes_their_invites() {
        let dir = tempfile::tempdir().unwrap();
        let invites = Invites::open(dir.path()).unwrap();
        let other = PersonId::new("0190c6a2-0000-7000-8000-000000000002");
        let theirs = invites.mint(Role::Owner, &owner(), INVITE_TTL).unwrap();
        let kept = invites.mint(Role::Member, &other, INVITE_TTL).unwrap();
        invites.forget_by(&owner()).unwrap();
        assert_eq!(invites.redeem(&theirs.token).unwrap(), None);
        assert_eq!(invites.redeem(&kept.token).unwrap(), Some(Role::Member));
    }
}
