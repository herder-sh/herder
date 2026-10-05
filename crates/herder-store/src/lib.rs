//! SQLite append-only event journal and the projections derived from it.
//!
//! One [`Store`] owns the daemon's database file. Every durable [`Event`] is appended to the
//! journal, and the read models it affects (the [`Session`] row with its title, the session's tracked pull
//! requests and the branches it owns) are updated in the same transaction, so a projection is never ahead of, or behind,
//! the journal.
//!
//! The API is synchronous; the daemon calls it from a dedicated thread or `spawn_blocking`.
//! Appends take `&mut self`, so there is exactly one writer per [`Store`].
//!
//! Beside the journal, the store keeps daemon state that must survive a restart and is not a
//! projection: the results of accepted commands ([`Store::command_result`]), each session's
//! queued prompts ([`Store::queued_prompts`]) and the vendor CLI session behind each session
//! ([`Store::native_session`]) and the usage of each completed turn ([`Store::record_turn_usage`]).
//!
//! Reads are forward compatible: a stored body this build cannot decode (an event type from a
//! newer build, or a known type whose shape changed) is returned as [`EventBody::Unknown`] with
//! its real `seq`, so cursors stay gap-free and callers skip it. `Unknown` never serializes, so
//! such events are not forwarded to clients.

mod project;
mod schema;

use std::collections::HashMap;
use std::path::Path;

use herder_protocol::{
    AccountId, Attachment, CommandId, CommandResult, Event, EventBody, HostId, JournalRecord,
    PermissionMode, PromptId, Provider, PullRequest, RawEventBody, Seq, SessionId, SessionStatus,
    Timestamp, TitleSource, TurnId, TurnUsage, UsageTotal, UserId,
};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Everything that can go wrong in the store.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// SQLite failed, or a stored value could not be read back.
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// The file system does not support SQLite's WAL journal mode.
    #[error("cannot enable WAL journal mode; got {0:?}")]
    NoWal(String),
    /// The database was written by a newer herder; this build refuses to touch it.
    #[error("database schema version {found} is newer than this build supports ({supported})")]
    TooNew {
        /// Schema version found in the file.
        found: u32,
        /// Newest schema version this build knows.
        supported: u32,
    },
    /// An event other than `session_created` was appended to a session with no events.
    #[error("session {0} does not exist")]
    UnknownSession(SessionId),
    /// A `session_created` event was appended to a session that already has events.
    #[error("session {0} already exists")]
    SessionExists(SessionId),
    /// A `session_created` event names a parent session of this host that does not exist.
    #[error("parent session {0} does not exist")]
    UnknownParent(SessionId),
    /// The event cannot be stored, such as one with an `Unknown` body or status.
    #[error("cannot encode event: {0}")]
    Encode(#[from] serde_json::Error),
}

/// Result of a store operation.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// An event to append; the store assigns its `seq`.
#[derive(Clone, Debug, PartialEq)]
pub struct NewEvent {
    /// Session whose journal receives the event.
    pub session_id: SessionId,
    /// When the daemon recorded the event.
    pub at: Timestamp,
    /// User whose command caused the event; `None` when the agent or the daemon caused it.
    pub by: Option<UserId>,
    /// What happened.
    pub body: EventBody,
}

/// A session's current state, projected from its journal.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    /// The session.
    pub session_id: SessionId,
    /// Absolute path of the repository, or folder, on the host.
    pub repo: String,
    /// Absolute path of the session's worktree on the host; `repo` itself when it has no
    /// branch.
    pub worktree: String,
    /// Branch the session was created on; `None` for a session that works in `repo` itself.
    pub branch: Option<String>,
    /// Current provider.
    pub provider: Provider,
    /// Current account.
    pub account_id: AccountId,
    /// Current model, in the provider's own naming.
    pub model: String,
    /// Current permission mode.
    pub permission_mode: PermissionMode,
    /// Primary session of the task this session is a child of; `None` for a top-level session.
    pub parent: Option<SessionId>,
    /// Host of the parent when it lives on another machine; `None` when the parent, if any,
    /// is a session of this host.
    pub parent_host: Option<HostId>,
    /// Short label of the session's task, shown in the task tree.
    pub task: Option<String>,
    /// Current title, from the latest `title_changed`; `None` until the first.
    pub title: Option<String>,
    /// Who chose the current title; `None` while there is none.
    pub title_source: Option<TitleSource>,
    /// Current status; `Idle` until the first status change.
    pub status: SessionStatus,
    /// Seq of the latest event, equal to [`Store::latest_seq`].
    pub last_seq: Seq,
    /// `at` of the latest event.
    pub updated_at: Timestamp,
}

/// A prompt waiting for its session's next turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedPrompt {
    /// The prompt's id, as clients see it in the session's queue.
    pub prompt_id: PromptId,
    /// Agent sender and delivery key; absent for human prompts.
    pub agent_message: Option<herder_protocol::AgentMessage>,
    /// User who sent it; `None` when the primary session's agent did.
    pub by: Option<UserId>,
    /// Prompt text.
    pub text: String,
    /// Images it carries, kept by the daemon apart from the store.
    pub attachments: Vec<Attachment>,
    /// Whether another immediate failover retry is disabled for this prompt.
    pub retry: bool,
    /// Earliest time to retry after a usage limit; ordinary prompts have no deadline.
    pub retry_at: Option<Timestamp>,
}

/// The vendor CLI's own session behind a herder session, as its adapter reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeSession {
    /// Provider whose CLI ran it.
    pub provider: Provider,
    /// Account it ran on, whose config dir holds its transcript.
    pub account_id: AccountId,
    /// The CLI's id for it.
    pub native_id: String,
}

/// Accepted command results kept; the oldest are forgotten first. A client resends a command
/// only until it reconnects, so only recent ids matter.
pub const COMMAND_RESULTS_KEPT: i64 = 4096;

/// The daemon's event journal and projections, in one SQLite file.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens or creates the database at `path` and migrates it to this build's schema.
    ///
    /// Fails with [`Error::TooNew`] when a newer build wrote the file. The parent directory
    /// must exist.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut conn = Connection::open(path)?;
        let mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "wal", |row| row.get(0))?;
        if mode != "wal" {
            return Err(Error::NoWal(mode));
        }
        // FULL, not NORMAL: in WAL mode NORMAL may lose the latest commits on power loss. A
        // client may already hold those events, and the store would then reissue their seqs
        // for different events, which that client's cursor silently skips.
        conn.pragma_update(None, "synchronous", "FULL")?;
        schema::migrate(&mut conn)?;
        Ok(Self { conn })
    }

    /// Appends an event to its session's journal at the next seq and updates the projections,
    /// in one transaction; returns the event as stored.
    ///
    /// A session's first event must be `session_created`, and only its first; the parent it
    /// names, if any, must already exist, unless `parent_host` places it on another machine.
    pub fn append(&mut self, event: NewEvent) -> Result<Event> {
        let body = serde_json::to_value(&event.body)?;
        let event_type = match body.get("type") {
            Some(serde_json::Value::String(tag)) => tag.clone(),
            _ => {
                let err = serde::ser::Error::custom("event body has no type tag");
                return Err(Error::Encode(err));
            }
        };

        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let latest = latest_seq(&tx, &event.session_id)?;
        let creates = matches!(event.body, EventBody::SessionCreated { .. });
        if creates && latest != 0 {
            return Err(Error::SessionExists(event.session_id));
        }
        if !creates && latest == 0 {
            return Err(Error::UnknownSession(event.session_id));
        }
        if let EventBody::SessionCreated {
            parent: Some(parent),
            parent_host: None,
            ..
        } = &event.body
            && latest_seq(&tx, parent)? == 0
        {
            return Err(Error::UnknownParent(parent.clone()));
        }
        let stored = Event {
            session_id: event.session_id,
            seq: latest + 1,
            at: event.at,
            by: event.by,
            body: event.body,
        };
        tx.prepare_cached(
            "INSERT INTO events (session_id, seq, at, by, event_type, body)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?
        .execute(params![
            stored.session_id.as_str(),
            stored.seq,
            stored.at,
            stored.by.as_ref().map(UserId::as_str),
            event_type,
            body.to_string(),
        ])?;
        // Moving an agent prompt from queue to journal is atomic: replay/dedup cannot lose
        // it in the crash window between queue removal and writing its transcript item.
        if let EventBody::ItemAdded { item } = &stored.body
            && matches!(item.body, herder_protocol::ItemBody::UserMessage { .. })
            && let Some(message) = &item.agent_message
        {
            tx.execute(
                "DELETE FROM queued_prompts WHERE session_id = ?1
                AND json_extract(agent_message, '$.sender_session_id') = ?2
                AND json_extract(agent_message, '$.message_id') = ?3",
                params![
                    stored.session_id.as_str(),
                    message.sender_session_id.as_str(),
                    message.message_id
                ],
            )?;
        }
        project::apply(&tx, &stored)?;
        tx.commit()?;
        Ok(stored)
    }

    /// Up to `limit` events of `session` with seq greater than `after_seq`, oldest first.
    ///
    /// `after_seq` is a client cursor: 0 reads from the start.
    pub fn read_since(
        &self,
        session: &SessionId,
        after_seq: Seq,
        limit: usize,
    ) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT session_id, seq, at, by, body FROM events
             WHERE session_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![session.as_str(), clamp(after_seq), clamp(limit)],
            read_event,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Up to `limit` events of `session` with seq greater than `after_seq`, oldest first, exactly
    /// as stored: the body is the raw JSON, whether or not this build knows its type. For
    /// replication, which must pass on event types newer than this build.
    pub fn read_records_since(
        &self,
        session: &SessionId,
        after_seq: Seq,
        limit: usize,
    ) -> Result<Vec<JournalRecord>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT seq, at, by, body FROM events
             WHERE session_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![session.as_str(), clamp(after_seq), clamp(limit)],
            |row| {
                let body: String = row.get(3)?;
                let body = serde_json::from_str(&body)
                    .and_then(RawEventBody::from_value)
                    .map_err(|err| {
                        rusqlite::Error::FromSqlConversionFailure(3, Type::Text, Box::new(err))
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

    /// Up to `limit` events of `session` with seq less than `before_seq`, oldest first: the page
    /// that ends just before `before_seq`, or the latest page when `before_seq` is `None`.
    pub fn read_page(
        &self,
        session: &SessionId,
        before_seq: Option<Seq>,
        limit: usize,
    ) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT session_id, seq, at, by, body FROM events
             WHERE session_id = ?1 AND seq < ?2 ORDER BY seq DESC LIMIT ?3",
        )?;
        let before = before_seq.map_or(i64::MAX, clamp);
        let rows = stmt.query_map(params![session.as_str(), before, clamp(limit)], read_event)?;
        let mut page = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        page.reverse();
        Ok(page)
    }

    /// Seq of the latest event of `session`; 0 when it has none.
    pub fn latest_seq(&self, session: &SessionId) -> Result<Seq> {
        latest_seq(&self.conn, session)
    }

    /// The session's projected state, or `None` when it does not exist.
    pub fn session(&self, session: &SessionId) -> Result<Option<Session>> {
        let found = self
            .conn
            .prepare_cached(&format!("{SESSION_SELECT} WHERE session_id = ?1"))?
            .query_row([session.as_str()], read_session)
            .optional()?;
        Ok(found)
    }

    /// Every session's projected state, ordered by session id.
    pub fn sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self
            .conn
            .prepare_cached(&format!("{SESSION_SELECT} ORDER BY session_id"))?;
        let rows = stmt.query_map([], read_session)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every child session of `parent`'s task, ordered by session id.
    pub fn children(&self, parent: &SessionId) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "{SESSION_SELECT} WHERE parent = ?1 ORDER BY session_id"
        ))?;
        let rows = stmt.query_map([parent.as_str()], read_session)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Pull requests tracked for the session, ordered by number.
    pub fn session_prs(&self, session: &SessionId) -> Result<Vec<PullRequest>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT number, url, title, state, ci, review, mergeable, head_branch FROM session_prs
             WHERE session_id = ?1 ORDER BY number",
        )?;
        let rows = stmt.query_map([session.as_str()], |row| {
            Ok(PullRequest {
                number: row.get(0)?,
                url: row.get(1)?,
                title: row.get(2)?,
                head_branch: row.get(7)?,
                state: get_tag(row, 3)?,
                ci: get_tag(row, 4)?,
                review: get_tag(row, 5)?,
                mergeable: get_tag(row, 6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every branch the session has owned, in the order first seen: the branch it was created
    /// on, then each one a `branch_checked_out` named. Kept after the worktree is removed.
    pub fn session_branches(&self, session: &SessionId) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT branch FROM session_branches WHERE session_id = ?1 ORDER BY first_seen_seq",
        )?;
        let rows = stmt.query_map([session.as_str()], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

impl Store {
    /// The result `user`'s command `command_id` was accepted with, if it was and is still kept.
    /// A result this build cannot decode reads as none.
    pub fn command_result(
        &self,
        user: &UserId,
        command_id: &CommandId,
    ) -> Result<Option<CommandResult>> {
        let result: Option<String> = self
            .conn
            .prepare_cached(
                "SELECT result FROM command_results WHERE user_id = ?1 AND command_id = ?2",
            )?
            .query_row([user.as_str(), command_id.as_str()], |row| row.get(0))
            .optional()?;
        Ok(result.and_then(|result| serde_json::from_str(&result).ok()))
    }

    /// Records that `user`'s command `command_id` was accepted with `result`, forgetting the
    /// oldest results past [`COMMAND_RESULTS_KEPT`].
    pub fn record_command_result(
        &mut self,
        user: &UserId,
        command_id: &CommandId,
        result: &CommandResult,
    ) -> Result<()> {
        let result = serde_json::to_string(result)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.prepare_cached(
            "INSERT OR REPLACE INTO command_results (user_id, command_id, result)
             VALUES (?1, ?2, ?3)",
        )?
        .execute(params![user.as_str(), command_id.as_str(), result])?;
        tx.prepare_cached(
            "DELETE FROM command_results WHERE n <= (SELECT MAX(n) FROM command_results) - ?1",
        )?
        .execute([COMMAND_RESULTS_KEPT])?;
        tx.commit()?;
        Ok(())
    }

    /// Text accepted under an agent delivery key, even after its queue entry was discarded.
    pub fn agent_message_text(
        &self,
        recipient: &SessionId,
        sender: &SessionId,
        message_id: &str,
    ) -> Result<Option<String>> {
        Ok(self.conn.query_row(
            "SELECT text FROM agent_message_receipts WHERE recipient = ?1 AND sender = ?2 AND message_id = ?3",
            params![recipient.as_str(), sender.as_str(), message_id], |row| row.get(0),
        ).optional()?)
    }

    /// The prompts queued in `session`, oldest first.
    pub fn queued_prompts(&self, session: &SessionId) -> Result<Vec<QueuedPrompt>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "{QUEUED_SELECT} WHERE session_id = ?1 ORDER BY position"
        ))?;
        let rows = stmt.query_map([session.as_str()], queued_prompt)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The prompts queued in every session that has any, each oldest first.
    pub fn queues(&self) -> Result<HashMap<SessionId, Vec<QueuedPrompt>>> {
        let mut stmt = self
            .conn
            .prepare_cached(&format!("{QUEUED_SELECT} ORDER BY session_id, position"))?;
        let rows = stmt.query_map([], |row| {
            Ok((
                SessionId::new(row.get::<_, String>(7)?),
                queued_prompt(row)?,
            ))
        })?;
        let mut queues: HashMap<SessionId, Vec<QueuedPrompt>> = HashMap::new();
        for row in rows {
            let (session, prompt) = row?;
            queues.entry(session).or_default().push(prompt);
        }
        Ok(queues)
    }

    /// Replaces the prompts queued in `session` with `prompts`, oldest first.
    pub fn set_queued_prompts(
        &mut self,
        session: &SessionId,
        prompts: &[QueuedPrompt],
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.prepare_cached("DELETE FROM queued_prompts WHERE session_id = ?1")?
            .execute([session.as_str()])?;
        let mut insert = tx.prepare_cached(
            "INSERT INTO queued_prompts (session_id, position, by, text, attachments, retry, retry_at, agent_message, prompt_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        for (position, prompt) in prompts.iter().enumerate() {
            if let Some(message) = &prompt.agent_message {
                tx.execute("INSERT OR IGNORE INTO agent_message_receipts (recipient, sender, message_id, text)
                    VALUES (?1, ?2, ?3, ?4)", params![session.as_str(), message.sender_session_id.as_str(), message.message_id, prompt.text])?;
            }
            insert.execute(params![
                session.as_str(),
                clamp(position),
                prompt.by.as_ref().map(UserId::as_str),
                prompt.text,
                serde_json::to_string(&prompt.attachments)?,
                prompt.retry,
                prompt.retry_at,
                prompt
                    .agent_message
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                prompt.prompt_id.as_str(),
            ])?;
        }
        drop(insert);
        tx.commit()?;
        Ok(())
    }

    /// The CLI session last reported for `session`, if any.
    pub fn native_session(&self, session: &SessionId) -> Result<Option<NativeSession>> {
        let found = self
            .conn
            .prepare_cached(
                "SELECT provider, account_id, native_id FROM native_sessions
                 WHERE session_id = ?1",
            )?
            .query_row([session.as_str()], |row| {
                Ok(NativeSession {
                    provider: Provider::from(row.get::<_, String>(0)?),
                    account_id: AccountId::new(row.get::<_, String>(1)?),
                    native_id: row.get(2)?,
                })
            })
            .optional()?;
        Ok(found)
    }

    /// Records `native` as the CLI session behind `session`, replacing any earlier one.
    pub fn set_native_session(
        &mut self,
        session: &SessionId,
        native: &NativeSession,
    ) -> Result<()> {
        self.conn
            .prepare_cached(
                "INSERT OR REPLACE INTO native_sessions (session_id, provider, account_id, native_id)
                 VALUES (?1, ?2, ?3, ?4)",
            )?
            .execute(params![
                session.as_str(),
                native.provider.as_str(),
                native.account_id.as_str(),
                native.native_id,
            ])?;
        Ok(())
    }

    /// Records the usage of `session`'s turn `turn_id`, completed at `at`, against the
    /// session's current account, provider and model. A turn already recorded keeps its first
    /// figures; an unknown session records nothing.
    pub fn record_turn_usage(
        &mut self,
        session: &SessionId,
        turn_id: &TurnId,
        usage: &TurnUsage,
        at: Timestamp,
    ) -> Result<()> {
        self.conn
            .prepare_cached(
                "INSERT OR IGNORE INTO turn_usage (session_id, turn_id, account_id, provider,
                    model, input, output, cache_read, cache_write, cost_usd, cost_estimated, at_ms)
                 SELECT session_id, ?2, account_id, provider, model, ?3, ?4, ?5, ?6, ?7, ?8, ?9
                 FROM sessions WHERE session_id = ?1",
            )?
            .execute(params![
                session.as_str(),
                turn_id.as_str(),
                clamp(usage.input),
                clamp(usage.output),
                clamp(usage.cache_read),
                clamp(usage.cache_write),
                usage.cost_usd,
                usage.cost_estimated,
                at.as_millisecond(),
            ])?;
        Ok(())
    }

    /// The usage of the turns completed at or after `since`, one total per account and model,
    /// ordered by account, then model.
    pub fn usage_totals(&self, since: Timestamp) -> Result<Vec<UsageTotal>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT account_id, provider, model, COUNT(*), SUM(input), SUM(output),
                SUM(cache_read), SUM(cache_write), TOTAL(cost_usd),
                MAX(cost_estimated OR cost_usd IS NULL)
             FROM turn_usage WHERE at_ms >= ?1
             GROUP BY account_id, provider, model ORDER BY account_id, model, provider",
        )?;
        let rows = stmt.query_map([since.as_millisecond()], |row| {
            Ok(UsageTotal {
                account_id: AccountId::new(row.get::<_, String>(0)?),
                provider: Provider::from(row.get::<_, String>(1)?),
                model: row.get(2)?,
                turns: row.get(3)?,
                input: row.get(4)?,
                output: row.get(5)?,
                cache_read: row.get(6)?,
                cache_write: row.get(7)?,
                cost_usd: row.get(8)?,
                cost_estimated: row.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every session with a prompt queued, ordered by session id.
    pub fn sessions_with_queued_prompts(&self) -> Result<Vec<SessionId>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT DISTINCT session_id FROM queued_prompts ORDER BY session_id")?;
        let rows = stmt.query_map([], |row| Ok(SessionId::new(row.get::<_, String>(0)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

const QUEUED_SELECT: &str = "SELECT by, text, attachments, retry, retry_at, agent_message,
    prompt_id, session_id FROM queued_prompts";

/// A queued prompt from a row of [`QUEUED_SELECT`].
fn queued_prompt(row: &Row<'_>) -> rusqlite::Result<QueuedPrompt> {
    let attachments: String = row.get(2)?;
    let agent_json: Option<String> = row.get(5)?;
    Ok(QueuedPrompt {
        prompt_id: PromptId::new(row.get::<_, String>(6)?),
        agent_message: agent_json
            .map(|json| serde_json::from_str(&json))
            .transpose()
            .map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(5, Type::Text, Box::new(err))
            })?,
        by: row.get::<_, Option<String>>(0)?.map(UserId::new),
        text: row.get(1)?,
        attachments: serde_json::from_str(&attachments).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(2, Type::Text, Box::new(err))
        })?,
        retry: row.get(3)?,
        retry_at: row.get(4)?,
    })
}

const SESSION_SELECT: &str = "SELECT session_id, repo, worktree, branch, provider, account_id,
    model, permission_mode, parent, task, status, last_seq, updated_at, title, title_source,
    parent_host FROM sessions";

fn latest_seq(conn: &Connection, session: &SessionId) -> Result<Seq> {
    let max: Option<Seq> = conn
        .prepare_cached("SELECT MAX(seq) FROM events WHERE session_id = ?1")?
        .query_row([session.as_str()], |row| row.get(0))?;
    Ok(max.unwrap_or(0))
}

fn read_event(row: &Row<'_>) -> rusqlite::Result<Event> {
    let body: String = row.get(4)?;
    Ok(Event {
        session_id: SessionId::new(row.get::<_, String>(0)?),
        seq: row.get(1)?,
        at: row.get(2)?,
        by: row.get::<_, Option<String>>(3)?.map(UserId::new),
        body: serde_json::from_str(&body).unwrap_or(EventBody::Unknown),
    })
}

fn read_session(row: &Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        session_id: SessionId::new(row.get::<_, String>(0)?),
        repo: row.get(1)?,
        worktree: row.get(2)?,
        branch: row.get(3)?,
        provider: Provider::from(row.get::<_, String>(4)?),
        account_id: AccountId::new(row.get::<_, String>(5)?),
        model: row.get(6)?,
        permission_mode: get_tag(row, 7)?,
        parent: row.get::<_, Option<String>>(8)?.map(SessionId::new),
        task: row.get(9)?,
        status: get_tag(row, 10)?,
        last_seq: row.get(11)?,
        updated_at: row.get(12)?,
        title: row.get(13)?,
        title_source: get_tag(row, 14)?,
        parent_host: row.get::<_, Option<String>>(15)?.map(HostId::new),
    })
}

/// The serde name of a unit enum value, as stored in projection columns.
fn tag(value: &impl Serialize) -> Result<String> {
    match serde_json::to_value(value)? {
        serde_json::Value::String(tag) => Ok(tag),
        other => Err(Error::Encode(serde::ser::Error::custom(format!(
            "{other} is not a unit enum value"
        )))),
    }
}

/// Reads a column written by [`tag`] back into its enum, or a nullable one into an `Option`.
fn get_tag<T: DeserializeOwned>(row: &Row<'_>, idx: usize) -> rusqlite::Result<T> {
    let tag = match row.get::<_, Option<String>>(idx)? {
        Some(tag) => serde_json::Value::String(tag),
        None => serde_json::Value::Null,
    };
    serde_json::from_value(tag)
        .map_err(|err| rusqlite::Error::FromSqlConversionFailure(idx, Type::Text, Box::new(err)))
}

/// A cursor or limit as an SQLite integer; values past `i64::MAX` mean "no bound".
fn clamp(value: impl TryInto<i64>) -> i64 {
    value.try_into().unwrap_or(i64::MAX)
}
