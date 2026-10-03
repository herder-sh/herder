//! The vault's database: every host's replicated journals and fleet index, and which device
//! replicates as which host, in one SQLite file.
//!
//! Journals are kept exactly as hosts stored them ([`JournalRecord`]), so event types newer
//! than this build survive. The API is synchronous; the vault calls it from `spawn_blocking`.
//!
//! Copies are kept per host. A session recovered on another host keeps its id, so a second
//! host replicating a session id is that host taking the session over: the copies other hosts
//! hold are marked recovered ([`VaultStore::claim`]). They stay, and keep whatever their host
//! sends when it returns, but the fleet shows only the current copy.

use std::path::Path;

use herder_protocol::{
    AccountId, Batch, Cursor, DeviceId, HostId, JournalRecord, MAX_BATCH_EVENTS, RawEventBody,
    RejectReason, Seq, SessionHead, SessionId, SessionStatus, SessionSummary, Timestamp, UserId,
};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

/// Migration `i` takes the schema from version `i` to `i + 1`. Append only.
const MIGRATIONS: &[&str] = &[
    "
CREATE TABLE hosts (
    device_id TEXT NOT NULL PRIMARY KEY,
    host_id   TEXT NOT NULL,
    host_name TEXT NOT NULL,
    seen_at   TEXT NOT NULL
) STRICT;
CREATE INDEX hosts_host_id ON hosts (host_id);

CREATE TABLE events (
    host_id    TEXT    NOT NULL,
    session_id TEXT    NOT NULL,
    seq        INTEGER NOT NULL,
    at         TEXT    NOT NULL,
    by         TEXT,
    event_type TEXT    NOT NULL,
    body       TEXT    NOT NULL,
    PRIMARY KEY (host_id, session_id, seq)
) STRICT;

CREATE TABLE sessions (
    host_id    TEXT NOT NULL,
    session_id TEXT NOT NULL,
    summary    TEXT NOT NULL,
    PRIMARY KEY (host_id, session_id)
) STRICT;
",
    "
CREATE INDEX events_type ON events (host_id, session_id, event_type, seq);
",
    "
CREATE TABLE recovered (
    host_id    TEXT NOT NULL,
    session_id TEXT NOT NULL,
    to_host    TEXT NOT NULL,
    at         TEXT NOT NULL,
    PRIMARY KEY (host_id, session_id)
) STRICT;
",
];

/// Leaves out copies of sessions another host recovered; `s` is the `sessions` row.
const CURRENT: &str = "NOT EXISTS (SELECT 1 FROM recovered r
                       WHERE r.host_id = s.host_id AND r.session_id = s.session_id)";

/// Event types that set the account a session runs on; each body has an `account_id`.
const ACCOUNT_EVENTS: &str = "'session_created', 'account_switched', 'provider_switched'";

/// A host that replicated here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRecord {
    /// The host.
    pub host_id: HostId,
    /// Its display name, as its latest hello gave it.
    pub host_name: String,
    /// When it was last heard from.
    pub seen_at: Timestamp,
}

/// The account of a stored event body that sets one.
#[derive(serde::Deserialize)]
struct AccountOf {
    account_id: AccountId,
}

/// What became of a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Every event up to this seq is durable.
    Acked(Seq),
    /// The batch changed nothing; the vault holds events up to `held`.
    Rejected {
        /// Last seq the vault holds.
        held: Seq,
        /// Why.
        reason: RejectReason,
    },
}

/// A batch that breaks the protocol's rules; the connection fails with a bad request.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct BadBatch(&'static str);

/// Everything that can go wrong in the vault's database.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// SQLite failed, or a stored value could not be read back.
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A value could not be encoded.
    #[error("cannot encode: {0}")]
    Encode(#[from] serde_json::Error),
    /// The database was written by a newer herder.
    #[error("vault schema version {0} is newer than this build supports")]
    TooNew(u32),
    /// The batch is malformed.
    #[error(transparent)]
    BadBatch(#[from] BadBatch),
}

/// Result of a vault store operation.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The vault's database.
#[derive(Debug)]
pub struct VaultStore {
    conn: Connection,
}

impl VaultStore {
    /// Opens or creates the database at `path`; the parent directory must exist.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "wal")?;
        // An ack promises the events are durable, as the host's own journal does.
        conn.pragma_update(None, "synchronous", "FULL")?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let found: u32 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let version = MIGRATIONS.len() as u32;
        if found > version {
            return Err(Error::TooNew(found));
        }
        for sql in &MIGRATIONS[found as usize..] {
            tx.execute_batch(sql)?;
        }
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
        Ok(Self { conn })
    }

    /// Records that `device` replicates as `host`, unless another device that is still
    /// `paired` already does; returns whether it may.
    pub fn bind(
        &mut self,
        device: &DeviceId,
        host: &HostId,
        host_name: &str,
        paired: &[DeviceId],
    ) -> Result<bool> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let bound: Option<String> = tx
            .query_row(
                "SELECT host_id FROM hosts WHERE device_id = ?1",
                [device.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        match bound {
            Some(bound) if bound != host.as_str() => return Ok(false),
            Some(_) => {}
            None => {
                let mut others = tx.prepare("SELECT device_id FROM hosts WHERE host_id = ?1")?;
                let others = others
                    .query_map([host.as_str()], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                if others
                    .iter()
                    .any(|other| paired.iter().any(|p| p.as_str() == other))
                {
                    return Ok(false);
                }
            }
        }
        tx.execute(
            "INSERT INTO hosts (device_id, host_id, host_name, seen_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (device_id) DO UPDATE SET host_name = ?3, seen_at = ?4",
            params![device.as_str(), host.as_str(), host_name, Timestamp::now()],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Every host that ever replicated here, with its latest name, ordered by host id.
    pub fn hosts(&self) -> Result<Vec<HostRecord>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT host_id, host_name, seen_at FROM hosts h WHERE seen_at =
               (SELECT MAX(seen_at) FROM hosts WHERE host_id = h.host_id)
             GROUP BY host_id ORDER BY host_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(HostRecord {
                host_id: HostId::new(row.get::<_, String>(0)?),
                host_name: row.get(1)?,
                seen_at: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Records that the host replicating from `device` was last heard from `at`.
    pub fn seen(&mut self, device: &DeviceId, at: Timestamp) -> Result<()> {
        self.conn
            .prepare_cached("UPDATE hosts SET seen_at = ?2 WHERE device_id = ?1")?
            .execute(params![device.as_str(), at])?;
        Ok(())
    }

    /// Every replicated session of every host as clients list it, ordered by session id. `head_seq` is the last seq held here; a session is listed
    /// once its creation is held.
    pub fn fleet(&self) -> Result<Vec<SessionHead>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT s.summary, s.host_id,
               (SELECT MAX(seq) FROM events e
                WHERE e.host_id = s.host_id AND e.session_id = s.session_id),
               (SELECT body FROM events e
                WHERE e.host_id = s.host_id AND e.session_id = s.session_id
                  AND e.event_type IN ({ACCOUNT_EVENTS})
                ORDER BY seq DESC LIMIT 1)
             FROM sessions s WHERE {CURRENT} ORDER BY s.session_id, s.host_id"
        ))?;
        let rows = stmt.query_map([], |row| {
            let summary: String = row.get(0)?;
            let summary: SessionSummary = serde_json::from_str(&summary).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(0, Type::Text, err.into())
            })?;
            let account = row
                .get::<_, Option<String>>(3)?
                .map(|body| serde_json::from_str::<AccountOf>(&body))
                .transpose()
                .map_err(|err| {
                    rusqlite::Error::FromSqlConversionFailure(3, Type::Text, err.into())
                })?;
            let host = HostId::new(row.get::<_, String>(1)?);
            Ok((summary, host, row.get::<_, Option<Seq>>(2)?, account))
        })?;
        let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        let need_you = |id: &SessionId| {
            rows.iter()
                .filter(|(child, ..)| {
                    child.parent.as_ref() == Some(id) && child.status == SessionStatus::NeedsYou
                })
                .count() as u32
        };
        Ok(rows
            .iter()
            .filter_map(|(summary, host, head, account)| {
                Some(SessionHead {
                    session_id: summary.session_id.clone(),
                    host_id: Some(host.clone()),
                    head_seq: (*head)?,
                    status: summary.status,
                    parent: summary.parent.clone(),
                    task: summary.task.clone(),
                    project_id: Some(summary.project_id.clone()),
                    account_id: account.as_ref()?.account_id.clone(),
                    children_need_you: need_you(&summary.session_id),
                })
            })
            .collect())
    }

    /// The host that has `session` now, if it is held here.
    pub fn host_of(&self, session: &SessionId) -> Result<Option<HostId>> {
        Ok(self
            .conn
            .prepare_cached(&format!(
                "SELECT host_id FROM sessions s WHERE session_id = ?1 AND {CURRENT} LIMIT 1"
            ))?
            .query_row([session.as_str()], |row| row.get::<_, String>(0))
            .optional()?
            .map(HostId::new))
    }

    /// Records that `host` has `session` now: when other hosts hold copies not recovered yet,
    /// `host` recovered it from them, and they are marked so. Returns those hosts. Nothing
    /// changes when `host`'s own copy is a recovered one: a session never moves back.
    pub fn claim(&mut self, host: &HostId, session: &SessionId) -> Result<Vec<HostId>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (host_id, session_id) = (host.as_str(), session.as_str());
        let superseded = tx
            .prepare_cached("SELECT 1 FROM recovered WHERE host_id = ?1 AND session_id = ?2")?
            .exists([host_id, session_id])?;
        if superseded {
            return Ok(Vec::new());
        }
        let others = tx
            .prepare_cached(
                "SELECT host_id FROM sessions WHERE session_id = ?2 AND host_id != ?1
                 UNION SELECT DISTINCT host_id FROM events WHERE session_id = ?2 AND host_id != ?1
                 EXCEPT SELECT host_id FROM recovered WHERE session_id = ?2",
            )?
            .query_map([host_id, session_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let now = Timestamp::now();
        for other in &others {
            tx.prepare_cached(
                "INSERT INTO recovered (host_id, session_id, to_host, at) VALUES (?1, ?2, ?3, ?4)",
            )?
            .execute(params![other, session_id, host_id, now])?;
        }
        tx.commit()?;
        Ok(others.into_iter().map(HostId::new).collect())
    }

    /// The host that recovered `host`'s copy of `session`, if one did.
    pub fn recovered_to(&self, host: &HostId, session: &SessionId) -> Result<Option<HostId>> {
        Ok(self
            .conn
            .prepare_cached("SELECT to_host FROM recovered WHERE host_id = ?1 AND session_id = ?2")?
            .query_row([host.as_str(), session.as_str()], |row| {
                row.get::<_, String>(0)
            })
            .optional()?
            .map(HostId::new))
    }

    /// For every session of `host` held here, the last seq held, ordered by session id.
    pub fn cursors(&self, host: &HostId) -> Result<Vec<Cursor>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT session_id, MAX(seq) FROM events WHERE host_id = ?1
             GROUP BY session_id ORDER BY session_id",
        )?;
        let rows = stmt.query_map([host.as_str()], |row| {
            Ok(Cursor {
                session_id: SessionId::new(row.get::<_, String>(0)?),
                after_seq: row.get(1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Replaces what the fleet index holds for one session of `host`.
    pub fn put_summary(&mut self, host: &HostId, summary: &SessionSummary) -> Result<()> {
        self.conn
            .prepare_cached(
                "INSERT INTO sessions (host_id, session_id, summary) VALUES (?1, ?2, ?3)
                 ON CONFLICT (host_id, session_id) DO UPDATE SET summary = ?3",
            )?
            .execute(params![
                host.as_str(),
                summary.session_id.as_str(),
                serde_json::to_string(summary)?
            ])?;
        Ok(())
    }

    /// The fleet index of `host`, ordered by session id.
    pub fn summaries(&self, host: &HostId) -> Result<Vec<SessionSummary>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT summary FROM sessions WHERE host_id = ?1 ORDER BY session_id",
        )?;
        let rows = stmt.query_map([host.as_str()], |row| {
            let text: String = row.get(0)?;
            serde_json::from_str(&text)
                .map_err(|err| rusqlite::Error::FromSqlConversionFailure(0, Type::Text, err.into()))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Up to `limit` events of `host`'s `session` after `after_seq`, oldest first.
    pub fn records(
        &self,
        host: &HostId,
        session: &SessionId,
        after_seq: Seq,
        limit: usize,
    ) -> Result<Vec<JournalRecord>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT seq, at, by, body FROM events
             WHERE host_id = ?1 AND session_id = ?2 AND seq > ?3 ORDER BY seq LIMIT ?4",
        )?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let after = i64::try_from(after_seq).unwrap_or(i64::MAX);
        let rows = stmt.query_map(
            params![host.as_str(), session.as_str(), after, limit],
            |row| {
                let body: String = row.get(3)?;
                let body = serde_json::from_str(&body)
                    .and_then(RawEventBody::from_value)
                    .map_err(|err| {
                        rusqlite::Error::FromSqlConversionFailure(3, Type::Text, err.into())
                    })?;
                Ok(JournalRecord {
                    seq: row.get(0)?,
                    at: row.get(1)?,
                    by: row.get::<_, Option<String>>(2)?.map(UserId::new),
                    body,
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Stores a batch of `host`'s events in one transaction: skips the ones already held when
    /// they are equal, rejects it whole on a gap or a conflict.
    pub fn append(&mut self, host: &HostId, batch: &Batch) -> Result<Outcome> {
        let Some(first) = batch.events.first().map(|e| e.seq) else {
            return Err(BadBatch("a batch holds at least one event").into());
        };
        let consecutive = batch
            .events
            .iter()
            .map(|e| e.seq)
            .eq(first..first + batch.events.len() as Seq);
        if first == 0 || !consecutive {
            return Err(BadBatch("a batch's seqs are consecutive, from 1 on").into());
        }
        if batch.events.len() > MAX_BATCH_EVENTS {
            return Err(BadBatch("a batch holds at most 256 events").into());
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (host_id, session_id) = (host.as_str(), batch.session_id.as_str());
        let held: Seq = tx
            .prepare_cached("SELECT MAX(seq) FROM events WHERE host_id = ?1 AND session_id = ?2")?
            .query_row([host_id, session_id], |row| row.get::<_, Option<Seq>>(0))?
            .unwrap_or(0);
        if first > held + 1 {
            return Ok(Outcome::Rejected {
                held,
                reason: RejectReason::Gap,
            });
        }
        let (resent, new) = batch
            .events
            .split_at(((held + 1 - first) as usize).min(batch.events.len()));
        {
            let mut select = tx.prepare_cached(
                "SELECT at, by, body FROM events
                 WHERE host_id = ?1 AND session_id = ?2 AND seq = ?3",
            )?;
            for record in resent {
                let (at, by, body): (Timestamp, Option<String>, String) = select
                    .query_row(params![host_id, session_id, record.seq], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })?;
                let body: serde_json::Value = serde_json::from_str(&body)?;
                let same = at == record.at
                    && by.as_deref() == record.by.as_ref().map(UserId::as_str)
                    && body.as_object() == Some(record.body.as_json());
                if !same {
                    return Ok(Outcome::Rejected {
                        held,
                        reason: RejectReason::Conflict,
                    });
                }
            }
            let mut insert = tx.prepare_cached(
                "INSERT INTO events (host_id, session_id, seq, at, by, event_type, body)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for record in new {
                insert.execute(params![
                    host_id,
                    session_id,
                    record.seq,
                    record.at,
                    record.by.as_ref().map(UserId::as_str),
                    record.body.event_type(),
                    serde_json::to_string(record.body.as_json())?,
                ])?;
            }
        }
        tx.commit()?;
        Ok(Outcome::Acked(
            held.max(first + batch.events.len() as Seq - 1),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(seq: Seq, text: &str) -> JournalRecord {
        JournalRecord {
            seq,
            at: "2027-01-15T08:00:00Z".parse().unwrap(),
            by: None,
            body: RawEventBody::from_value(serde_json::json!({"type": "note", "text": text}))
                .unwrap(),
        }
    }

    fn batch(records: Vec<JournalRecord>) -> Batch {
        Batch {
            session_id: SessionId::new("s1"),
            events: records,
        }
    }

    #[test]
    fn batches_append_resend_and_reject() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = VaultStore::open(dir.path().join("vault.db")).unwrap();
        let host = HostId::new("h1");
        let first = batch(vec![record(1, "a"), record(2, "b")]);
        assert_eq!(store.append(&host, &first).unwrap(), Outcome::Acked(2));
        // A re-send of what is held, plus one new event.
        let overlap = batch(vec![record(2, "b"), record(3, "c")]);
        assert_eq!(store.append(&host, &overlap).unwrap(), Outcome::Acked(3));
        assert_eq!(store.append(&host, &first).unwrap(), Outcome::Acked(3));
        let gap = batch(vec![record(5, "e")]);
        assert_eq!(
            store.append(&host, &gap).unwrap(),
            Outcome::Rejected {
                held: 3,
                reason: RejectReason::Gap
            }
        );
        let conflict = batch(vec![record(3, "x"), record(4, "d")]);
        assert_eq!(
            store.append(&host, &conflict).unwrap(),
            Outcome::Rejected {
                held: 3,
                reason: RejectReason::Conflict
            }
        );
        assert!(matches!(
            store.append(&host, &batch(vec![record(4, "d"), record(6, "f")])),
            Err(Error::BadBatch(_))
        ));
        let held = store.records(&host, &SessionId::new("s1"), 0, 10).unwrap();
        assert_eq!(held, [record(1, "a"), record(2, "b"), record(3, "c")]);
        assert_eq!(
            store.cursors(&host).unwrap(),
            [Cursor {
                session_id: SessionId::new("s1"),
                after_seq: 3
            }]
        );
        assert!(store.cursors(&HostId::new("h2")).unwrap().is_empty());
    }

    fn summary(id: &str, parent: Option<&str>, status: SessionStatus) -> SessionSummary {
        SessionSummary {
            session_id: SessionId::new(id),
            project_id: herder_protocol::ProjectId::new("github.com/org/repo"),
            repo: "/repo".into(),
            branch: format!("herder/{id}"),
            status,
            prs: Vec::new(),
            parent: parent.map(SessionId::new),
            task: None,
            head_seq: 1,
            updated_at: "2027-01-15T08:00:00Z".parse().unwrap(),
        }
    }

    fn event(seq: Seq, body: serde_json::Value) -> JournalRecord {
        JournalRecord {
            body: RawEventBody::from_value(body).unwrap(),
            ..record(seq, "")
        }
    }

    #[test]
    fn the_fleet_lists_sessions_whose_creation_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = VaultStore::open(dir.path().join("vault.db")).unwrap();
        let host = HostId::new("h1");
        let created = serde_json::json!({
            "type": "session_created", "repo": "/repo", "worktree": "/wt", "branch": "b",
            "provider": "claude", "account_id": "main", "model": "m",
            "permission_mode": "ask"
        });
        let switched = serde_json::json!({"type": "account_switched", "account_id": "other"});
        let primary = Batch {
            session_id: SessionId::new("s1"),
            events: vec![
                event(1, created.clone()),
                event(2, switched),
                record(3, "x"),
            ],
        };
        store.append(&host, &primary).unwrap();
        let child = Batch {
            session_id: SessionId::new("s2"),
            events: vec![event(1, created)],
        };
        store.append(&host, &child).unwrap();
        store
            .put_summary(&host, &summary("s1", None, SessionStatus::Idle))
            .unwrap();
        store
            .put_summary(&host, &summary("s2", Some("s1"), SessionStatus::NeedsYou))
            .unwrap();
        // Summarised before any of its events arrived: not listed yet.
        store
            .put_summary(&host, &summary("s3", None, SessionStatus::Idle))
            .unwrap();

        let heads = store.fleet().unwrap();
        assert_eq!(heads.len(), 2);
        assert_eq!(heads[0].session_id.as_str(), "s1");
        assert_eq!(heads[0].host_id, Some(host.clone()));
        assert_eq!(heads[0].head_seq, 3);
        assert_eq!(heads[0].account_id.as_str(), "other");
        assert_eq!(heads[0].children_need_you, 1);
        assert_eq!(heads[1].account_id.as_str(), "main");
        assert_eq!(heads[1].parent.as_ref().unwrap().as_str(), "s1");
        assert_eq!(store.host_of(&SessionId::new("s2")).unwrap(), Some(host));
        assert_eq!(store.host_of(&SessionId::new("nope")).unwrap(), None);
    }

    #[test]
    fn a_session_another_host_replicates_is_recovered_and_never_moves_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = VaultStore::open(dir.path().join("vault.db")).unwrap();
        let (a, b, c) = (HostId::new("a"), HostId::new("b"), HostId::new("c"));
        let s1 = SessionId::new("s1");
        let created = serde_json::json!({
            "type": "session_created", "repo": "/repo", "worktree": "/wt", "branch": "b",
            "provider": "claude", "account_id": "main", "model": "m",
            "permission_mode": "ask"
        });
        let journal = |records: Vec<JournalRecord>| Batch {
            session_id: s1.clone(),
            events: records,
        };
        assert!(store.claim(&a, &s1).unwrap().is_empty());
        store
            .put_summary(&a, &summary("s1", None, SessionStatus::Running))
            .unwrap();
        store
            .append(&a, &journal(vec![event(1, created.clone())]))
            .unwrap();
        assert_eq!(store.host_of(&s1).unwrap(), Some(a.clone()));

        // B recovers it: A's copy is superseded once, and the fleet shows B's.
        assert_eq!(store.claim(&b, &s1).unwrap(), std::slice::from_ref(&a));
        assert!(store.claim(&b, &s1).unwrap().is_empty());
        store
            .put_summary(&b, &summary("s1", None, SessionStatus::Idle))
            .unwrap();
        store
            .append(&b, &journal(vec![event(1, created.clone())]))
            .unwrap();
        assert_eq!(store.recovered_to(&a, &s1).unwrap(), Some(b.clone()));
        assert_eq!(store.recovered_to(&b, &s1).unwrap(), None);
        assert_eq!(store.host_of(&s1).unwrap(), Some(b.clone()));
        let fleet = store.fleet().unwrap();
        assert_eq!(fleet.len(), 1);
        assert_eq!(fleet[0].host_id, Some(b.clone()));
        assert_eq!(fleet[0].status, SessionStatus::Idle);

        // A comes back and replicates what it had: kept, but the session stays B's.
        assert!(store.claim(&a, &s1).unwrap().is_empty());
        store.append(&a, &journal(vec![record(2, "late")])).unwrap();
        assert_eq!(store.records(&a, &s1, 0, 10).unwrap().len(), 2);
        assert_eq!(store.host_of(&s1).unwrap(), Some(b.clone()));

        // C recovers it from B.
        assert_eq!(store.claim(&c, &s1).unwrap(), std::slice::from_ref(&b));
        assert_eq!(store.recovered_to(&a, &s1).unwrap(), Some(b));
        store
            .put_summary(&c, &summary("s1", None, SessionStatus::Idle))
            .unwrap();
        store.append(&c, &journal(vec![event(1, created)])).unwrap();
        assert_eq!(store.host_of(&s1).unwrap(), Some(c));
        assert_eq!(store.fleet().unwrap().len(), 1);
    }

    #[test]
    fn a_host_replicates_from_one_paired_device() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = VaultStore::open(dir.path().join("vault.db")).unwrap();
        let (h1, h2) = (HostId::new("h1"), HostId::new("h2"));
        let (d1, d2) = (DeviceId::new("d1"), DeviceId::new("d2"));
        let paired = [d1.clone(), d2.clone()];
        assert!(store.bind(&d1, &h1, "box", &paired).unwrap());
        assert!(store.bind(&d1, &h1, "box-renamed", &paired).unwrap());
        // d1 is h1; it cannot claim another host, nor d2 claim h1 while d1 is paired.
        assert!(!store.bind(&d1, &h2, "other", &paired).unwrap());
        assert!(!store.bind(&d2, &h1, "box", &paired).unwrap());
        // Once d1 is revoked, a new device may take over h1.
        assert!(
            store
                .bind(&d2, &h1, "box", std::slice::from_ref(&d2))
                .unwrap()
        );
        let seen: Timestamp = "2027-01-15T09:00:00Z".parse().unwrap();
        store.seen(&d2, seen).unwrap();
        let hosts = store.hosts().unwrap();
        assert_eq!(hosts.len(), 1);
        assert_eq!(
            (hosts[0].host_name.as_str(), hosts[0].seen_at),
            ("box", seen)
        );
    }
}
