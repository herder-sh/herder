//! The database schema and its ordered migrations, tracked in SQLite's `user_version`.

use rusqlite::{Connection, TransactionBehavior};

use crate::{Error, Result};

/// Migration `i` takes the schema from version `i` to `i + 1`. Append only; never edit a shipped entry.
const MIGRATIONS: &[&str] = &[V1, V2, V3, V4, V5, V6];

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

/// Every branch a session has owned, with the seq of the event that first named it: the
/// branch of `session_created`, then each `branch_checked_out`. Backfilled from `sessions`,
/// whose rows are always created at seq 1.
const V3: &str = "
CREATE TABLE session_branches (
    session_id     TEXT    NOT NULL,
    branch         TEXT    NOT NULL,
    first_seen_seq INTEGER NOT NULL,
    PRIMARY KEY (session_id, branch)
) STRICT;
INSERT INTO session_branches (session_id, branch, first_seen_seq)
    SELECT session_id, branch, 1 FROM sessions;
";

/// Daemon state that must outlive a restart but is not part of any journal: the result of
/// each accepted command, by user and command id, so a resend is not applied twice; and each
/// session's prompts still waiting for their turn, in order. Neither is a projection.
const V4: &str = "
CREATE TABLE command_results (
    n          INTEGER NOT NULL PRIMARY KEY,
    user_id    TEXT    NOT NULL,
    command_id TEXT    NOT NULL,
    result     TEXT    NOT NULL,
    UNIQUE (user_id, command_id)
) STRICT;

CREATE TABLE queued_prompts (
    session_id TEXT    NOT NULL,
    position   INTEGER NOT NULL,
    by         TEXT,
    text       TEXT    NOT NULL,
    retry      INTEGER NOT NULL,
    PRIMARY KEY (session_id, position)
) STRICT;
";

/// The branch a tracked pull request merges from; unknown for those tracked before.
const V5: &str = "
ALTER TABLE session_prs ADD COLUMN head_branch TEXT;
";

/// Daemon state, not a projection: the vendor CLI's own id for each session's latest CLI
/// session, with the provider and account it ran on, so a later start can resume it natively.
const V6: &str = "
CREATE TABLE native_sessions (
    session_id TEXT NOT NULL PRIMARY KEY,
    provider   TEXT NOT NULL,
    account_id TEXT NOT NULL,
    native_id  TEXT NOT NULL
) STRICT;
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
