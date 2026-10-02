//! The database schema and its ordered migrations, tracked in SQLite's `user_version`.

use rusqlite::{Connection, TransactionBehavior};

use crate::{Error, Result};

/// Migration `i` takes the schema from version `i` to `i + 1`. Append only; never edit a shipped entry.
const MIGRATIONS: &[&str] = &[V1, V2];

/// Schema version this build writes.
pub(crate) const VERSION: u32 = MIGRATIONS.len() as u32;

const V1: &str = "
CREATE TABLE events (
    session_id TEXT    NOT NULL,
    seq        INTEGER NOT NULL,
    at         TEXT    NOT NULL,
    by         TEXT,
    event_type TEXT    NOT NULL,
    body       TEXT    NOT NULL,
    PRIMARY KEY (session_id, seq)
) STRICT;

CREATE TABLE sessions (
    session_id      TEXT    NOT NULL PRIMARY KEY,
    repo            TEXT    NOT NULL,
    worktree        TEXT    NOT NULL,
    branch          TEXT    NOT NULL,
    provider        TEXT    NOT NULL,
    account_id      TEXT    NOT NULL,
    model           TEXT    NOT NULL,
    permission_mode TEXT    NOT NULL,
    status          TEXT    NOT NULL,
    last_seq        INTEGER NOT NULL,
    updated_at      TEXT    NOT NULL
) STRICT;

CREATE TABLE session_prs (
    session_id TEXT    NOT NULL,
    number     INTEGER NOT NULL,
    url        TEXT    NOT NULL,
    title      TEXT    NOT NULL,
    state      TEXT    NOT NULL,
    ci         TEXT    NOT NULL,
    review     TEXT    NOT NULL,
    mergeable  TEXT    NOT NULL,
    PRIMARY KEY (session_id, number)
) STRICT;
";

/// Task trees: a child session's primary session and its task label.
const V2: &str = "
ALTER TABLE sessions ADD COLUMN parent TEXT;
ALTER TABLE sessions ADD COLUMN task TEXT;
CREATE INDEX sessions_parent ON sessions (parent);
";

/// Brings the schema up to [`VERSION`] in one transaction, refusing a database from a newer build.
pub(crate) fn migrate(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let found: u32 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if found > VERSION {
        return Err(Error::TooNew {
            found,
            supported: VERSION,
        });
    }
    for sql in &MIGRATIONS[found as usize..] {
        tx.execute_batch(sql)?;
    }
    tx.pragma_update(None, "user_version", VERSION)?;
    tx.commit()?;
    Ok(())
}
