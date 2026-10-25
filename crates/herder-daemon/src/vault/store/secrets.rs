//! The vault's secrets in its database: sets, their encrypted values, grants to hosts, for
//! their sessions or their own use, and the audit of every value handed out. Values come in and go out sealed ([`Sealed`]); encrypting
//! them is [`crate::vault::secrets`]'s.

use std::collections::BTreeMap;

use herder_protocol::{
    HostId, MAX_SECRET_AUDIT_ENTRIES, SecretAccess, SecretError, SecretErrorCode, SecretInfo,
    SecretRef, SecretSet, SessionId, Timestamp,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{Result, VaultStore};

/// A value as the database keeps it: encrypted, with the nonce it was encrypted under.
#[derive(Clone, PartialEq, Eq)]
pub struct Sealed {
    /// The nonce.
    pub nonce: Vec<u8>,
    /// The value encrypted, with its tag.
    pub ciphertext: Vec<u8>,
}

/// What deleting a set did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetDeleted {
    /// It is gone, with its grants.
    Deleted,
    /// No such set.
    NotFound,
    /// It still holds secrets, so it stays.
    NotEmpty,
}

/// What granting a set did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grant {
    /// The host may fetch the set's secrets, now or already.
    Granted,
    /// No such set.
    NoSet,
    /// No such host has replicated here.
    NoHost,
}

/// What a host fetches secrets for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretUse {
    /// One of its sessions, from the sets granted to the host.
    Session(SessionId),
    /// Its own use, from the sets granted to it for that, under a purpose such as
    /// `bot:trip1-team`.
    Host(String),
}

impl SecretUse {
    /// Whether it draws on the grants for host use.
    pub fn host_use(&self) -> bool {
        matches!(self, Self::Host(_))
    }
}

/// A secret a host's fetch resolved to: as named, and where it is.
#[derive(Clone)]
pub struct Resolved {
    /// As the fetch named it.
    pub secret: SecretRef,
    /// Its set.
    pub set: String,
    /// Its value, sealed.
    pub sealed: Sealed,
}

impl VaultStore {
    /// Creates the empty set `set`; false when it exists.
    pub fn create_secret_set(&mut self, set: &str, at: Timestamp) -> Result<bool> {
        let created = self
            .conn
            .prepare_cached("INSERT OR IGNORE INTO secret_sets (name, created_at) VALUES (?1, ?2)")?
            .execute(params![set, at])?;
        Ok(created == 1)
    }

    /// Deletes the set `set` and its grants, unless it holds secrets.
    pub fn delete_secret_set(&mut self, set: &str) -> Result<SetDeleted> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !set_exists(&tx, set)? {
            return Ok(SetDeleted::NotFound);
        }
        let held: u32 = tx
            .prepare_cached("SELECT COUNT(*) FROM secrets WHERE set_name = ?1")?
            .query_row([set], |row| row.get(0))?;
        if held > 0 {
            return Ok(SetDeleted::NotEmpty);
        }
        tx.prepare_cached("DELETE FROM secret_grants WHERE set_name = ?1")?
            .execute([set])?;
        tx.prepare_cached("DELETE FROM secret_sets WHERE name = ?1")?
            .execute([set])?;
        tx.commit()?;
        Ok(SetDeleted::Deleted)
    }

    /// Creates the secret `name` in `set`, or replaces its description and value; false when
    /// there is no such set.
    pub fn put_secret(
        &mut self,
        set: &str,
        name: &str,
        description: &str,
        sealed: &Sealed,
        at: Timestamp,
    ) -> Result<bool> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !set_exists(&tx, set)? {
            return Ok(false);
        }
        tx.prepare_cached(
            "INSERT INTO secrets (set_name, name, description, nonce, ciphertext, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (set_name, name) DO UPDATE SET description = excluded.description,
                 nonce = excluded.nonce, ciphertext = excluded.ciphertext,
                 updated_at = excluded.updated_at",
        )?
        .execute(params![
            set,
            name,
            description,
            sealed.nonce,
            sealed.ciphertext,
            at
        ])?;
        tx.commit()?;
        Ok(true)
    }

    /// Deletes the secret `name` of `set`; false when there is none.
    pub fn delete_secret(&mut self, set: &str, name: &str) -> Result<bool> {
        let deleted = self
            .conn
            .prepare_cached("DELETE FROM secrets WHERE set_name = ?1 AND name = ?2")?
            .execute([set, name])?;
        Ok(deleted == 1)
    }

    /// Lets `host` fetch the secrets of `set` for its sessions, or with `host_use` for its own
    /// use.
    pub fn grant_secret_set(&mut self, set: &str, host: &HostId, host_use: bool) -> Result<Grant> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !set_exists(&tx, set)? {
            return Ok(Grant::NoSet);
        }
        let known = tx
            .prepare_cached("SELECT 1 FROM hosts WHERE host_id = ?1 LIMIT 1")?
            .query_row([host.as_str()], |_| Ok(()))
            .optional()?
            .is_some();
        if !known {
            return Ok(Grant::NoHost);
        }
        tx.prepare_cached(
            "INSERT OR IGNORE INTO secret_grants (set_name, host_id, host_use) VALUES (?1, ?2, ?3)",
        )?
        .execute(params![set, host.as_str(), host_use])?;
        tx.commit()?;
        Ok(Grant::Granted)
    }

    /// Stops `host` fetching the secrets of `set` for its sessions, or with `host_use` for its
    /// own use; false when it was not granted that way.
    pub fn revoke_secret_set(&mut self, set: &str, host: &HostId, host_use: bool) -> Result<bool> {
        let revoked = self
            .conn
            .prepare_cached(
                "DELETE FROM secret_grants WHERE set_name = ?1 AND host_id = ?2 AND host_use = ?3",
            )?
            .execute(params![set, host.as_str(), host_use])?;
        Ok(revoked == 1)
    }

    /// Every set with the hosts it is granted to, each way, ordered by name.
    pub fn secret_sets(&self) -> Result<Vec<SecretSet>> {
        let mut sets: BTreeMap<String, SecretSet> = self
            .conn
            .prepare_cached("SELECT name FROM secret_sets")?
            .query_map([], |row| row.get(0))?
            .map(|name: rusqlite::Result<String>| {
                name.map(|name| {
                    let set = SecretSet {
                        name: name.clone(),
                        hosts: Vec::new(),
                        host_use: Vec::new(),
                    };
                    (name, set)
                })
            })
            .collect::<rusqlite::Result<_>>()?;
        let grants = self
            .conn
            .prepare_cached(
                "SELECT set_name, host_id, host_use FROM secret_grants ORDER BY host_id",
            )?
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    HostId::new(row.get::<_, String>(1)?),
                    row.get::<_, bool>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (set, host, host_use) in grants {
            if let Some(set) = sets.get_mut(&set) {
                if host_use {
                    set.host_use.push(host);
                } else {
                    set.hosts.push(host);
                }
            }
        }
        Ok(sets.into_values().collect())
    }

    /// Every secret without its value, ordered by set, then name.
    pub fn secrets(&self) -> Result<Vec<SecretInfo>> {
        self.secret_infos(
            "SELECT set_name, name, description, updated_at FROM secrets
             ORDER BY set_name, name",
            [],
        )
    }

    /// The secrets in the sets granted to `host` for its sessions, without values, ordered by
    /// set, then name.
    pub fn granted_secrets(&self, host: &HostId) -> Result<Vec<SecretInfo>> {
        self.secret_infos(
            "SELECT s.set_name, s.name, s.description, s.updated_at FROM secrets s
             JOIN secret_grants g
                 ON g.set_name = s.set_name AND g.host_id = ?1 AND g.host_use = 0
             ORDER BY s.set_name, s.name",
            [host.as_str()],
        )
    }

    fn secret_infos(&self, sql: &str, params: impl rusqlite::Params) -> Result<Vec<SecretInfo>> {
        Ok(self
            .conn
            .prepare_cached(sql)?
            .query_map(params, |row| {
                Ok(SecretInfo {
                    set: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    updated_at: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Resolves every secret `host` asks for, for `host_use` or its sessions, to its set and
    /// sealed value: `set/NAME` in a set granted to the host that way, and `NAME` in the only
    /// such set that has it. The first one that does not resolve fails them all.
    pub fn resolve_secrets(
        &self,
        host: &HostId,
        host_use: bool,
        secrets: &[SecretRef],
    ) -> Result<Result<Vec<Resolved>, SecretError>> {
        let mut resolved = Vec::with_capacity(secrets.len());
        for secret in secrets {
            let found: Vec<(String, Sealed)> = self
                .conn
                .prepare_cached(
                    "SELECT s.set_name, s.nonce, s.ciphertext FROM secrets s
                     JOIN secret_grants g
                         ON g.set_name = s.set_name AND g.host_id = ?1 AND g.host_use = ?4
                     WHERE s.name = ?2 AND (?3 IS NULL OR s.set_name = ?3)
                     ORDER BY s.set_name",
                )?
                .query_map(
                    params![host.as_str(), secret.name, secret.set, host_use],
                    |row| {
                        Ok((
                            row.get(0)?,
                            Sealed {
                                nonce: row.get(1)?,
                                ciphertext: row.get(2)?,
                            },
                        ))
                    },
                )?
                .collect::<rusqlite::Result<_>>()?;
            let mut found = found.into_iter();
            match (found.next(), found.next()) {
                (Some((set, sealed)), None) => resolved.push(Resolved {
                    secret: secret.clone(),
                    set,
                    sealed,
                }),
                (Some(first), Some(second)) => {
                    let candidates = [first, second]
                        .into_iter()
                        .chain(found)
                        .map(|(set, _)| SecretRef {
                            set: Some(set),
                            name: secret.name.clone(),
                        })
                        .collect();
                    let message = format!(
                        "more than one set granted to this host{} has {secret}; name it as set/{}",
                        for_host_use(host_use),
                        secret.name
                    );
                    return Ok(Err(refusal(
                        SecretErrorCode::Ambiguous,
                        message,
                        secret,
                        candidates,
                    )));
                }
                (None, _) => return Ok(Err(self.unresolved(host, host_use, secret)?)),
            }
        }
        Ok(Ok(resolved))
    }

    /// Why `secret` does not resolve for `host`, for `host_use` or its sessions.
    fn unresolved(&self, host: &HostId, host_use: bool, secret: &SecretRef) -> Result<SecretError> {
        let way = for_host_use(host_use);
        let Some(set) = &secret.set else {
            let message = format!(
                "no set granted to this host{way} has {secret}; ask the vault's owner to grant \
                 its set{way}"
            );
            return Ok(refusal(
                SecretErrorCode::Unknown,
                message,
                secret,
                Vec::new(),
            ));
        };
        let granted = self
            .conn
            .prepare_cached(
                "SELECT 1 FROM secret_grants WHERE set_name = ?1 AND host_id = ?2 AND host_use = ?3",
            )?
            .query_row(params![set, host.as_str(), host_use], |_| Ok(()))
            .optional()?
            .is_some();
        if !granted && set_exists(&self.conn, set)? {
            let message = format!(
                "the set {set} is not granted to this host{way}; ask the vault's owner to grant \
                 it{way}"
            );
            return Ok(refusal(
                SecretErrorCode::NotGranted,
                message,
                secret,
                Vec::new(),
            ));
        }
        let message = if granted {
            format!("the set {set} has no secret {}", secret.name)
        } else {
            format!("there is no secret set {set}")
        };
        Ok(refusal(
            SecretErrorCode::Unknown,
            message,
            secret,
            Vec::new(),
        ))
    }

    /// Records that `host` was handed the value of each of `secrets`, by set and name, for
    /// `used`.
    pub fn audit_secrets(
        &mut self,
        host: &HostId,
        used: &SecretUse,
        secrets: &[(String, String)],
        at: Timestamp,
    ) -> Result<()> {
        let (session, purpose) = match used {
            SecretUse::Session(session) => (Some(session.as_str()), None),
            SecretUse::Host(purpose) => (None, Some(purpose.as_str())),
        };
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (set, name) in secrets {
            tx.prepare_cached(
                "INSERT INTO secret_audit (at, host_id, session_id, purpose, set_name, name)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?
            .execute(params![at, host.as_str(), session, purpose, set, name])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The latest values handed out, newest first, at most [`MAX_SECRET_AUDIT_ENTRIES`]; only
    /// those older than the entry `before` when given.
    pub fn secret_audit(&self, before: Option<u64>) -> Result<Vec<SecretAccess>> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT id, at, host_id, session_id, purpose, set_name, name FROM secret_audit
                 WHERE ?1 IS NULL OR id < ?1 ORDER BY id DESC LIMIT ?2",
            )?
            .query_map(params![before, MAX_SECRET_AUDIT_ENTRIES], |row| {
                Ok(SecretAccess {
                    id: row.get(0)?,
                    at: row.get(1)?,
                    host_id: HostId::new(row.get::<_, String>(2)?),
                    session_id: row.get::<_, Option<String>>(3)?.map(SessionId::new),
                    purpose: row.get(4)?,
                    set: row.get(5)?,
                    name: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
}

fn set_exists(conn: &rusqlite::Connection, set: &str) -> Result<bool> {
    Ok(conn
        .prepare_cached("SELECT 1 FROM secret_sets WHERE name = ?1")?
        .query_row([set], |_| Ok(()))
        .optional()?
        .is_some())
}

/// How a refusal names the grants a fetch draws on.
fn for_host_use(host_use: bool) -> &'static str {
    if host_use { " for host use" } else { "" }
}

fn refusal(
    code: SecretErrorCode,
    message: String,
    secret: &SecretRef,
    candidates: Vec<SecretRef>,
) -> SecretError {
    SecretError {
        code,
        message,
        secret: Some(secret.clone()),
        candidates,
    }
}
