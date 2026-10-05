//! The vault's database: every host's replicated journals and fleet index, and which device
//! replicates as which host, in one SQLite file.
//!
//! Journals are kept exactly as hosts stored them ([`JournalRecord`]), so event types newer
//! than this build survive. The API is synchronous; the vault calls it from `spawn_blocking`.
//!
//! Copies are kept per host. A session another host took over keeps its id, so a second
//! host replicating a session id is that host taking the session over: the copies other hosts
//! hold are marked recovered ([`VaultStore::claim`]). They stay, and keep whatever their host
//! sends when it returns, but the fleet shows only the current copy.
//!
//! Images prompts carried are kept per host the same way, beside the journals, by session and
//! attachment id ([`VaultStore::put_attachment`]), up to the cap the host's hello gave: the
//! host's oldest images make room for a new one.
//!
//! Archived sessions are dropped after the vault's retention period ([`VaultStore::prune`]);
//! a dropped session leaves a tombstone at the seq held, so its host, which still has it, is
//! not asked to send it again. A host and everything it replicated go only when an owner
//! forgets it ([`VaultStore::forget`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use herder_protocol::{
    AccountId, AttachmentData, AttachmentId, Batch, Bytes, Cursor, DeviceId, HostId,
    HostReplication, IMAGE_MEDIA_TYPES, Image, JournalRecord, MAX_BATCH_EVENTS, MAX_IMAGE_BYTES,
    RawEventBody, RejectReason, Seq, SessionHead, SessionId, SessionStatus, SessionSummary,
    Timestamp, UserId, VaultStatus,
};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};

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
    "
CREATE TABLE attachments (
    host_id       TEXT    NOT NULL,
    session_id    TEXT    NOT NULL,
    attachment_id TEXT    NOT NULL,
    media_type    TEXT    NOT NULL,
    sha256        BLOB    NOT NULL,
    data          BLOB    NOT NULL,
    PRIMARY KEY (host_id, session_id, attachment_id)
) STRICT;
",
    "
ALTER TABLE hosts ADD COLUMN attachments_cap INTEGER;
ALTER TABLE attachments ADD COLUMN size INTEGER NOT NULL DEFAULT 0;
UPDATE attachments SET size = length(data);
CREATE TABLE pruned (
    host_id    TEXT    NOT NULL,
    session_id TEXT    NOT NULL,
    seq        INTEGER NOT NULL,
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
    /// Most bytes of its images kept, as its latest hello gave it; `None` when it backs up
    /// no images.
    pub attachments_cap: Option<u64>,
    /// Its sessions held here.
    pub sessions: u32,
    /// Bytes of its images held here.
    pub attachment_bytes: u64,
}

/// What became of an image a host sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kept {
    /// Kept, after evicting this many of the host's oldest images to stay within its cap;
    /// also for a re-send of one held already.
    Stored {
        /// Images evicted.
        evicted: usize,
    },
    /// Not kept: the host backs up no images.
    Off,
    /// Not kept: the image alone is bigger than the host's cap.
    OverCap,
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

/// A batch or attachment that breaks the protocol's rules; the connection fails with a bad
/// request.
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
    /// The batch or attachment is malformed.
    #[error(transparent)]
    BadBatch(#[from] BadBatch),
}

/// Result of a vault store operation.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The vault's database.
#[derive(Debug)]
pub struct VaultStore {
    conn: Connection,
    path: PathBuf,
}

impl VaultStore {
    /// Opens or creates the database at `path`; the parent directory must exist.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_owned();
        let mut conn = Connection::open(&path)?;
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
        Ok(Self { conn, path })
    }

    /// The database file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Records that `device` replicates as `host`, keeping at most `attachments_cap` bytes
    /// of its images, unless another device that is still `paired` already does; returns
    /// whether it may. A lower cap evicts the host's oldest images at once; with none, the
    /// images held stay until the host is forgotten.
    pub fn bind(
        &mut self,
        device: &DeviceId,
        host: &HostId,
        host_name: &str,
        attachments_cap: Option<u64>,
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
            "INSERT INTO hosts (device_id, host_id, host_name, seen_at, attachments_cap)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (device_id) DO UPDATE SET host_name = ?3, seen_at = ?4,
               attachments_cap = ?5",
            params![
                device.as_str(),
                host.as_str(),
                host_name,
                Timestamp::now(),
                attachments_cap
            ],
        )?;
        if let Some(cap) = attachments_cap {
            evict(&tx, host, cap, 0)?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// Every host that ever replicated here, with its latest name and what it takes here,
    /// ordered by host id.
    pub fn hosts(&self) -> Result<Vec<HostRecord>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT host_id, host_name, seen_at, attachments_cap,
               (SELECT COUNT(*) FROM sessions s WHERE s.host_id = h.host_id),
               (SELECT COALESCE(SUM(size), 0) FROM attachments a WHERE a.host_id = h.host_id)
             FROM hosts h WHERE seen_at =
               (SELECT MAX(seen_at) FROM hosts WHERE host_id = h.host_id)
             GROUP BY host_id ORDER BY host_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(HostRecord {
                host_id: HostId::new(row.get::<_, String>(0)?),
                host_name: row.get(1)?,
                seen_at: row.get(2)?,
                attachments_cap: row.get(3)?,
                sessions: row.get(4)?,
                attachment_bytes: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every device that ever replicated here as a host.
    pub fn host_devices(&self) -> Result<Vec<DeviceId>> {
        let mut stmt = self.conn.prepare_cached("SELECT device_id FROM hosts")?;
        let rows = stmt.query_map([], |row| Ok(DeviceId::new(row.get::<_, String>(0)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every device that ever replicated here as `host`.
    pub fn devices_of(&self, host: &HostId) -> Result<Vec<DeviceId>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT device_id FROM hosts WHERE host_id = ?1")?;
        let rows = stmt.query_map([host.as_str()], |row| {
            Ok(DeviceId::new(row.get::<_, String>(0)?))
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
                    parent_host: summary.parent_host.clone(),
                    task: summary.task.clone(),
                    title: summary.title.clone(),
                    project_id: Some(summary.project_id.clone()),
                    account_id: account.as_ref()?.account_id.clone(),
                    children_need_you: need_you(&summary.session_id),
                    queue: Vec::new(),
                })
            })
            .collect())
    }

    /// What the vault holds, in total and of every host that ever replicated here, ordered by
    /// host id. Lags are left out: the store does not know them.
    pub fn status(&self) -> Result<VaultStatus> {
        let mut hosts: BTreeMap<HostId, HostReplication> = self
            .hosts()?
            .into_iter()
            .map(|host| {
                let replication = HostReplication {
                    host_id: host.host_id.clone(),
                    sessions: 0,
                    events: 0,
                    last_event_at: None,
                    lag_ms: None,
                };
                (host.host_id, replication)
            })
            .collect();
        // Each session's event count and newest event, by its last seq: `at` is text, whose
        // order is not the time's.
        let mut stmt = self.conn.prepare_cached(
            "SELECT l.host_id, l.n, e.at FROM
               (SELECT host_id, session_id, MAX(seq) AS seq, COUNT(*) AS n FROM events
                GROUP BY host_id, session_id) l
             JOIN events e
               ON e.host_id = l.host_id AND e.session_id = l.session_id AND e.seq = l.seq",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                HostId::new(row.get::<_, String>(0)?),
                row.get::<_, u64>(1)?,
                row.get::<_, Timestamp>(2)?,
            ))
        })?;
        for row in rows {
            let (host, events, at) = row?;
            if let Some(host) = hosts.get_mut(&host) {
                host.events += events;
                host.last_event_at = host.last_event_at.max(Some(at));
            }
        }
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT host_id, COUNT(*) FROM sessions s WHERE {CURRENT} GROUP BY host_id"
        ))?;
        let rows = stmt.query_map([], |row| {
            Ok((HostId::new(row.get::<_, String>(0)?), row.get::<_, u64>(1)?))
        })?;
        for row in rows {
            let (host, sessions) = row?;
            if let Some(host) = hosts.get_mut(&host) {
                host.sessions = sessions;
            }
        }
        let pages: u64 = self
            .conn
            .pragma_query_value(None, "page_count", |row| row.get(0))?;
        let page_size: u64 = self
            .conn
            .pragma_query_value(None, "page_size", |row| row.get(0))?;
        let hosts: Vec<HostReplication> = hosts.into_values().collect();
        Ok(VaultStatus {
            sessions: hosts.iter().map(|host| host.sessions).sum(),
            events: hosts.iter().map(|host| host.events).sum(),
            storage_bytes: pages * page_size,
            hosts,
        })
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

    /// For every session of `host` held here, the last seq held, ordered by session id; for
    /// one dropped as archived, the last seq it held then.
    pub fn cursors(&self, host: &HostId) -> Result<Vec<Cursor>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT session_id, MAX(seq) FROM events WHERE host_id = ?1 GROUP BY session_id
             UNION ALL SELECT session_id, seq FROM pruned WHERE host_id = ?1
             ORDER BY session_id",
        )?;
        let rows = stmt.query_map([host.as_str()], |row| {
            Ok(Cursor {
                session_id: SessionId::new(row.get::<_, String>(0)?),
                after_seq: row.get(1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Replaces what the fleet index holds for one session of `host`. A session dropped as
    /// archived stays dropped while it is archived; once it is not, it is taken back, and its
    /// next batch, finding nothing held, has the host send it again from the start.
    pub fn put_summary(&mut self, host: &HostId, summary: &SessionSummary) -> Result<()> {
        let key = [host.as_str(), summary.session_id.as_str()];
        if self.pruned(key)?.is_some() {
            if summary.status == SessionStatus::Archived {
                return Ok(());
            }
            self.conn
                .prepare_cached("DELETE FROM pruned WHERE host_id = ?1 AND session_id = ?2")?
                .execute(key)?;
        }
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

    /// The seq `host`'s session `key` was dropped at, if it was dropped as archived.
    fn pruned(&self, key: [&str; 2]) -> Result<Option<Seq>> {
        Ok(self
            .conn
            .prepare_cached("SELECT seq FROM pruned WHERE host_id = ?1 AND session_id = ?2")?
            .query_row(key, |row| row.get(0))
            .optional()?)
    }

    /// Keeps an image of `host`'s session durably, within `cap` bytes of the host's images:
    /// the oldest are evicted to make room. Without a cap, or bigger than it, the image is
    /// not kept. A re-send with the same bytes changes nothing; one with other bytes under
    /// the same id is malformed.
    pub fn put_attachment(
        &mut self,
        host: &HostId,
        image: &AttachmentData,
        cap: Option<u64>,
    ) -> Result<Kept> {
        let attachment = &image.attachment;
        let data = &image.data.0;
        if !IMAGE_MEDIA_TYPES.contains(&attachment.media_type.as_str()) {
            return Err(BadBatch("an attachment's media type is not an image type").into());
        }
        if attachment.size != data.len() as u64 || data.len() > MAX_IMAGE_BYTES {
            return Err(
                BadBatch("an attachment's size is not that of its bytes, or too big").into(),
            );
        }
        let Some(cap) = cap else {
            return Ok(Kept::Off);
        };
        if attachment.size > cap {
            return Ok(Kept::OverCap);
        }
        let sha256 = Sha256::digest(data).to_vec();
        let key = [
            host.as_str(),
            image.session_id.as_str(),
            attachment.attachment_id.as_str(),
        ];
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let held: Option<(String, Vec<u8>)> = tx
            .prepare_cached(
                "SELECT media_type, sha256 FROM attachments
                 WHERE host_id = ?1 AND session_id = ?2 AND attachment_id = ?3",
            )?
            .query_row(key, |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        match held {
            Some((media_type, held)) if media_type == attachment.media_type && held == sha256 => {
                return Ok(Kept::Stored { evicted: 0 });
            }
            Some(_) => {
                return Err(
                    BadBatch("an attachment differs from the one held under its id").into(),
                );
            }
            None => {}
        }
        let evicted = evict(&tx, host, cap, attachment.size)?;
        tx.prepare_cached(
            "INSERT INTO attachments
               (host_id, session_id, attachment_id, media_type, sha256, data, size)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?
        .execute(params![
            key[0],
            key[1],
            key[2],
            attachment.media_type,
            sha256,
            data,
            attachment.size
        ])?;
        tx.commit()?;
        Ok(Kept::Stored { evicted })
    }

    /// The image `attachment_id` of `host`'s `session`, if it is held here.
    pub fn attachment(
        &self,
        host: &HostId,
        session: &SessionId,
        attachment_id: &AttachmentId,
    ) -> Result<Option<Image>> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT media_type, data FROM attachments
                 WHERE host_id = ?1 AND session_id = ?2 AND attachment_id = ?3",
            )?
            .query_row(
                [host.as_str(), session.as_str(), attachment_id.as_str()],
                |row| {
                    Ok(Image {
                        media_type: row.get(0)?,
                        data: Bytes(row.get(1)?),
                    })
                },
            )
            .optional()?)
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
        let last = first + batch.events.len() as Seq - 1;
        let pruned: Option<Seq> = tx
            .prepare_cached("SELECT seq FROM pruned WHERE host_id = ?1 AND session_id = ?2")?
            .query_row([host_id, session_id], |row| row.get(0))
            .optional()?;
        if let Some(held) = pruned {
            // Dropped as archived: what an archived session still gets is not kept either.
            if first > held + 1 {
                return Ok(Outcome::Rejected {
                    held,
                    reason: RejectReason::Gap,
                });
            }
            let held = held.max(last);
            tx.prepare_cached("UPDATE pruned SET seq = ?3 WHERE host_id = ?1 AND session_id = ?2")?
                .execute(params![host_id, session_id, held])?;
            tx.commit()?;
            return Ok(Outcome::Acked(held));
        }
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
        Ok(Outcome::Acked(held.max(last)))
    }

    /// Drops every session of the `online` hosts that is archived and had no event since
    /// `before`, with its journal and images, leaving a tombstone at the seq it held. Hosts
    /// not online keep everything: a dead host's sessions go only when it is forgotten.
    /// Returns the sessions dropped.
    pub fn prune(
        &mut self,
        online: &[HostId],
        before: Timestamp,
    ) -> Result<Vec<(HostId, SessionId)>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut dropped = Vec::new();
        for host in online {
            let summaries = tx
                .prepare_cached("SELECT summary FROM sessions WHERE host_id = ?1")?
                .query_map([host.as_str()], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for summary in summaries {
                let summary: SessionSummary = serde_json::from_str(&summary)?;
                if summary.status != SessionStatus::Archived || summary.updated_at >= before {
                    continue;
                }
                let key = [host.as_str(), summary.session_id.as_str()];
                let held: Seq = tx
                    .prepare_cached(
                        "SELECT COALESCE(MAX(seq), 0) FROM events
                         WHERE host_id = ?1 AND session_id = ?2",
                    )?
                    .query_row(key, |row| row.get(0))?;
                for table in ["events", "attachments", "sessions", "recovered"] {
                    tx.prepare_cached(&format!(
                        "DELETE FROM {table} WHERE host_id = ?1 AND session_id = ?2"
                    ))?
                    .execute(key)?;
                }
                tx.prepare_cached(
                    "INSERT INTO pruned (host_id, session_id, seq) VALUES (?1, ?2, ?3)",
                )?
                .execute(params![key[0], key[1], held])?;
                dropped.push((host.clone(), summary.session_id));
            }
        }
        tx.commit()?;
        Ok(dropped)
    }

    /// Drops `host` and everything it replicated: its journals, images and tombstones, and
    /// the record that it recovered sessions from other hosts, whose copies show again.
    /// Returns how many of its sessions were held.
    pub fn forget(&mut self, host: &HostId) -> Result<u32> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sessions: u32 = tx
            .prepare_cached("SELECT COUNT(*) FROM sessions WHERE host_id = ?1")?
            .query_row([host.as_str()], |row| row.get(0))?;
        for table in [
            "events",
            "attachments",
            "sessions",
            "recovered",
            "pruned",
            "hosts",
        ] {
            tx.prepare_cached(&format!("DELETE FROM {table} WHERE host_id = ?1"))?
                .execute([host.as_str()])?;
        }
        tx.prepare_cached("DELETE FROM recovered WHERE to_host = ?1")?
            .execute([host.as_str()])?;
        tx.commit()?;
        Ok(sessions)
    }
}

/// Evicts `host`'s oldest images until `incoming` more bytes fit within `cap`; returns how
/// many went.
fn evict(tx: &rusqlite::Transaction<'_>, host: &HostId, cap: u64, incoming: u64) -> Result<usize> {
    let held = tx
        .prepare_cached("SELECT rowid, size FROM attachments WHERE host_id = ?1 ORDER BY rowid")?
        .query_map([host.as_str()], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, u64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut total: u64 = held.iter().map(|(_, size)| size).sum::<u64>() + incoming;
    let mut evicted = 0;
    for (rowid, size) in held {
        if total <= cap {
            break;
        }
        tx.prepare_cached("DELETE FROM attachments WHERE rowid = ?1")?
            .execute([rowid])?;
        total -= size;
        evicted += 1;
    }
    Ok(evicted)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cap no test image comes near.
    const CAP: Option<u64> = Some(1 << 30);

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

    #[test]
    fn attachments_are_kept_once_and_refused_when_they_differ() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = VaultStore::open(dir.path().join("vault.db")).unwrap();
        let (host, session) = (HostId::new("h1"), SessionId::new("s1"));
        let image = |media_type: &str, data: &[u8]| AttachmentData {
            session_id: session.clone(),
            attachment: herder_protocol::Attachment {
                attachment_id: AttachmentId::new("img1"),
                media_type: media_type.into(),
                size: data.len() as u64,
            },
            data: Bytes(data.to_vec()),
        };
        let png = image("image/png", b"\x89PNG\r\n\x1a\nA");
        store.put_attachment(&host, &png, CAP).unwrap();
        store.put_attachment(&host, &png, CAP).unwrap();
        let held = store
            .attachment(&host, &session, &AttachmentId::new("img1"))
            .unwrap();
        assert_eq!(
            held,
            Some(Image {
                media_type: "image/png".into(),
                data: png.data.clone()
            })
        );
        let other = AttachmentId::new("img2");
        assert_eq!(store.attachment(&host, &session, &other).unwrap(), None);
        assert_eq!(
            store
                .attachment(&HostId::new("h2"), &session, &AttachmentId::new("img1"))
                .unwrap(),
            None
        );
        let refused = |store: &mut VaultStore, image: AttachmentData| {
            matches!(
                store.put_attachment(&host, &image, CAP),
                Err(Error::BadBatch(_))
            )
        };
        assert!(refused(
            &mut store,
            image("image/png", b"\x89PNG\r\n\x1a\nB")
        ));
        assert!(refused(&mut store, image("image/svg+xml", b"<svg")));
        let mut wrong_size = png.clone();
        wrong_size.attachment.size += 1;
        assert!(refused(&mut store, wrong_size));
    }

    fn summary(id: &str, parent: Option<&str>, status: SessionStatus) -> SessionSummary {
        SessionSummary {
            session_id: SessionId::new(id),
            project_id: herder_protocol::ProjectId::new("github.com/org/repo"),
            repo: "/repo".into(),
            branch: Some(format!("herder/{id}")),
            status,
            prs: Vec::new(),
            parent: parent.map(SessionId::new),
            parent_host: None,
            task: None,
            title: None,
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
    fn the_status_counts_what_each_host_replicated() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = VaultStore::open(dir.path().join("vault.db")).unwrap();
        let (a, b, idle) = (HostId::new("a"), HostId::new("b"), HostId::new("idle"));
        let paired = [
            DeviceId::new("da"),
            DeviceId::new("db"),
            DeviceId::new("di"),
        ];
        for (device, host) in paired.iter().zip([&a, &b, &idle]) {
            assert!(
                store
                    .bind(device, host, host.as_str(), None, &paired)
                    .unwrap()
            );
        }
        let at = |at: &str| -> Timestamp { at.parse().unwrap() };
        let timed = |seq, when: &str| JournalRecord {
            at: at(when),
            ..record(seq, "x")
        };
        let journal = |id: &str, records| Batch {
            session_id: SessionId::new(id),
            events: records,
        };
        // A's newest event is the last of s1, though s2 is stored after it.
        let s1 = vec![
            timed(1, "2027-01-15T08:00:00Z"),
            timed(2, "2027-01-15T09:00:00.5Z"),
        ];
        store.append(&a, &journal("s1", s1)).unwrap();
        let s2 = vec![timed(1, "2027-01-15T08:30:00Z")];
        store.append(&a, &journal("s2", s2)).unwrap();
        for id in ["s1", "s2"] {
            store
                .put_summary(&a, &summary(id, None, SessionStatus::Idle))
                .unwrap();
        }
        // B recovers s2: it counts for B, while A's copy stays held.
        store.claim(&b, &SessionId::new("s2")).unwrap();
        store
            .put_summary(&b, &summary("s2", None, SessionStatus::Idle))
            .unwrap();
        let s2 = vec![
            timed(1, "2027-01-15T08:30:00Z"),
            timed(2, "2027-01-15T10:00:00Z"),
        ];
        store.append(&b, &journal("s2", s2)).unwrap();

        let status = store.status().unwrap();
        let replication = |host: &HostId, sessions, events, last: Option<&str>| HostReplication {
            host_id: host.clone(),
            sessions,
            events,
            last_event_at: last.map(at),
            lag_ms: None,
        };
        assert_eq!(
            status.hosts,
            [
                replication(&a, 1, 3, Some("2027-01-15T09:00:00.5Z")),
                replication(&b, 1, 2, Some("2027-01-15T10:00:00Z")),
                replication(&idle, 0, 0, None),
            ]
        );
        assert_eq!((status.sessions, status.events), (2, 5));
        assert!(status.storage_bytes > 0);
    }

    #[test]
    fn a_host_replicates_from_one_paired_device() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = VaultStore::open(dir.path().join("vault.db")).unwrap();
        let (h1, h2) = (HostId::new("h1"), HostId::new("h2"));
        let (d1, d2) = (DeviceId::new("d1"), DeviceId::new("d2"));
        let paired = [d1.clone(), d2.clone()];
        assert!(store.bind(&d1, &h1, "box", None, &paired).unwrap());
        assert!(store.bind(&d1, &h1, "box-renamed", None, &paired).unwrap());
        // d1 is h1; it cannot claim another host, nor d2 claim h1 while d1 is paired.
        assert!(!store.bind(&d1, &h2, "other", None, &paired).unwrap());
        assert!(!store.bind(&d2, &h1, "box", None, &paired).unwrap());
        // Once d1 is revoked, a new device may take over h1.
        assert!(
            store
                .bind(&d2, &h1, "box", None, std::slice::from_ref(&d2))
                .unwrap()
        );
        let seen: Timestamp = "2027-01-15T09:00:00Z".parse().unwrap();
        store.seen(&d2, seen).unwrap();
        assert_eq!(store.devices_of(&h1).unwrap(), [d1.clone(), d2.clone()]);
        assert!(store.devices_of(&h2).unwrap().is_empty());
        assert_eq!(store.host_devices().unwrap(), [d1, d2]);
        let hosts = store.hosts().unwrap();
        assert_eq!(hosts.len(), 1);
        assert_eq!(
            (hosts[0].host_name.as_str(), hosts[0].seen_at),
            ("box", seen)
        );
    }

    fn png(session: &str, id: &str, len: usize) -> AttachmentData {
        let mut data = b"\x89PNG\r\n\x1a\n".to_vec();
        data.resize(len, b'x');
        AttachmentData {
            session_id: SessionId::new(session),
            attachment: herder_protocol::Attachment {
                attachment_id: AttachmentId::new(id),
                media_type: "image/png".into(),
                size: len as u64,
            },
            data: Bytes(data),
        }
    }

    fn held(store: &VaultStore, host: &HostId, session: &str, id: &str) -> bool {
        store
            .attachment(host, &SessionId::new(session), &AttachmentId::new(id))
            .unwrap()
            .is_some()
    }

    #[test]
    fn images_are_kept_only_within_the_hosts_cap_oldest_evicted_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = VaultStore::open(dir.path().join("vault.db")).unwrap();
        let (host, device) = (HostId::new("h1"), DeviceId::new("d1"));
        let paired = std::slice::from_ref(&device);

        // Off: nothing is kept, and nothing fails.
        assert!(store.bind(&device, &host, "box", None, paired).unwrap());
        assert_eq!(
            store
                .put_attachment(&host, &png("s1", "a", 100), None)
                .unwrap(),
            Kept::Off
        );
        assert!(!held(&store, &host, "s1", "a"));

        assert!(
            store
                .bind(&device, &host, "box", Some(250), paired)
                .unwrap()
        );
        let cap = Some(250);
        for id in ["a", "b"] {
            assert_eq!(
                store
                    .put_attachment(&host, &png("s1", id, 100), cap)
                    .unwrap(),
                Kept::Stored { evicted: 0 }
            );
        }
        // Another host's images never make room for this one's.
        let other = HostId::new("h2");
        store
            .put_attachment(&other, &png("s9", "z", 200), cap)
            .unwrap();
        assert_eq!(
            store
                .put_attachment(&host, &png("s2", "c", 100), cap)
                .unwrap(),
            Kept::Stored { evicted: 1 }
        );
        assert!(!held(&store, &host, "s1", "a"));
        assert!(held(&store, &host, "s1", "b") && held(&store, &host, "s2", "c"));
        assert!(held(&store, &other, "s9", "z"));
        // Bigger than the cap alone: refused, and nothing evicted for it.
        assert_eq!(
            store
                .put_attachment(&host, &png("s2", "d", 300), cap)
                .unwrap(),
            Kept::OverCap
        );
        assert!(held(&store, &host, "s1", "b"));

        let usage = |store: &VaultStore| {
            let hosts = store.hosts().unwrap();
            let record = hosts.iter().find(|h| h.host_id == host).unwrap();
            (record.attachments_cap, record.attachment_bytes)
        };
        assert_eq!(usage(&store), (Some(250), 200));
        // A lower cap in the next hello evicts at once.
        assert!(
            store
                .bind(&device, &host, "box", Some(150), paired)
                .unwrap()
        );
        assert_eq!(usage(&store), (Some(150), 100));
        assert!(!held(&store, &host, "s1", "b") && held(&store, &host, "s2", "c"));
        // Turned off, what is held stays.
        assert!(store.bind(&device, &host, "box", None, paired).unwrap());
        assert_eq!(usage(&store), (None, 100));
    }

    fn created() -> serde_json::Value {
        serde_json::json!({
            "type": "session_created", "repo": "/repo", "worktree": "/wt", "branch": "b",
            "provider": "claude", "account_id": "main", "model": "m",
            "permission_mode": "ask"
        })
    }

    /// Replicates session `id` of `host` with `events` events, last changed `days` ago.
    fn replicate(
        store: &mut VaultStore,
        host: &HostId,
        id: &str,
        status: SessionStatus,
        days: i64,
    ) {
        let mut events = vec![event(1, created())];
        events.push(record(2, "x"));
        store
            .append(
                host,
                &Batch {
                    session_id: SessionId::new(id),
                    events,
                },
            )
            .unwrap();
        let updated_at = Timestamp::now() - jiff::SignedDuration::from_hours(24 * days);
        store
            .put_summary(
                host,
                &SessionSummary {
                    updated_at,
                    head_seq: 2,
                    ..summary(id, None, status)
                },
            )
            .unwrap();
        store
            .put_attachment(host, &png(id, "img", 10), CAP)
            .unwrap();
    }

    #[test]
    fn archived_sessions_of_online_hosts_are_pruned_and_not_taken_back_while_archived() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = VaultStore::open(dir.path().join("vault.db")).unwrap();
        let (live, dead) = (HostId::new("live"), HostId::new("dead"));
        replicate(&mut store, &live, "old", SessionStatus::Archived, 91);
        replicate(&mut store, &live, "recent", SessionStatus::Archived, 89);
        replicate(&mut store, &live, "idle", SessionStatus::Idle, 400);
        replicate(&mut store, &dead, "ancient", SessionStatus::Archived, 400);

        let before = Timestamp::now() - jiff::SignedDuration::from_hours(24 * 90);
        let dropped = store.prune(std::slice::from_ref(&live), before).unwrap();
        assert_eq!(dropped, [(live.clone(), SessionId::new("old"))]);
        let listed: Vec<String> = store
            .fleet()
            .unwrap()
            .iter()
            .map(|head| head.session_id.to_string())
            .collect();
        assert_eq!(listed, ["ancient", "idle", "recent"]);
        assert!(!held(&store, &live, "old", "img"));
        assert!(held(&store, &dead, "ancient", "img"));
        assert!(
            store
                .prune(std::slice::from_ref(&live), before)
                .unwrap()
                .is_empty()
        );

        // The host still has it: its cursor says it is held, so the host does not send it
        // again, and what an archived session still gets is acknowledged, not kept.
        let old = SessionId::new("old");
        let cursors = store.cursors(&live).unwrap();
        assert!(cursors.contains(&Cursor {
            session_id: old.clone(),
            after_seq: 2
        }));
        let archived = SessionSummary {
            head_seq: 3,
            ..summary("old", None, SessionStatus::Archived)
        };
        store.put_summary(&live, &archived).unwrap();
        let late = Batch {
            session_id: old.clone(),
            events: vec![record(3, "pr merged")],
        };
        assert_eq!(store.append(&live, &late).unwrap(), Outcome::Acked(3));
        assert!(store.records(&live, &old, 0, 10).unwrap().is_empty());
        assert_eq!(store.fleet().unwrap().len(), 3);

        // Unarchived, it is taken back: its next batch finds nothing held, and the host sends
        // it from the start.
        store
            .put_summary(&live, &summary("old", None, SessionStatus::Idle))
            .unwrap();
        let next = Batch {
            session_id: old.clone(),
            events: vec![record(4, "again")],
        };
        assert_eq!(
            store.append(&live, &next).unwrap(),
            Outcome::Rejected {
                held: 0,
                reason: RejectReason::Gap
            }
        );
        let whole = Batch {
            session_id: old.clone(),
            events: vec![event(1, created()), record(2, "x")],
        };
        assert_eq!(store.append(&live, &whole).unwrap(), Outcome::Acked(2));
        assert_eq!(store.fleet().unwrap().len(), 4);
    }

    #[test]
    fn forgetting_a_host_drops_everything_it_replicated() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = VaultStore::open(dir.path().join("vault.db")).unwrap();
        let (gone, kept) = (HostId::new("gone"), HostId::new("kept"));
        let (d1, d2) = (DeviceId::new("d1"), DeviceId::new("d2"));
        assert!(
            store
                .bind(&d1, &gone, "old-box", CAP, std::slice::from_ref(&d1))
                .unwrap()
        );
        assert!(
            store
                .bind(&d2, &kept, "box", CAP, std::slice::from_ref(&d2))
                .unwrap()
        );
        replicate(&mut store, &gone, "s1", SessionStatus::Idle, 1);
        replicate(&mut store, &gone, "s2", SessionStatus::Archived, 1);
        replicate(&mut store, &kept, "s3", SessionStatus::Idle, 1);
        // `gone` recovered s3 from `kept` once; forgetting it shows `kept`'s copy again.
        store.claim(&gone, &SessionId::new("s3")).unwrap();
        assert_eq!(store.fleet().unwrap().len(), 2);

        assert_eq!(store.forget(&gone).unwrap(), 2);
        assert!(store.devices_of(&gone).unwrap().is_empty());
        let hosts = store.hosts().unwrap();
        assert_eq!(hosts.len(), 1);
        assert_eq!(
            (hosts[0].sessions, hosts[0].attachment_bytes),
            (1, 10),
            "{hosts:?}"
        );
        assert!(store.cursors(&gone).unwrap().is_empty());
        assert!(!held(&store, &gone, "s1", "img"));
        let fleet = store.fleet().unwrap();
        assert_eq!(fleet.len(), 1);
        assert_eq!(fleet[0].host_id, Some(kept));
    }
}
