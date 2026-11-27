//! How a [`Timestamp`] is kept in SQLite.

use herder_protocol::Timestamp;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, Type, ValueRef};

/// A [`Timestamp`] stored as RFC 3339 UTC text, such as `2026-11-27T10:00:00.5Z`.
///
/// rusqlite converts only jiff 0.2 types, so this does by hand what its `jiff` feature did:
/// the same text on write, and on read that text or whole Unix seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqlTime(pub Timestamp);

impl ToSql for SqlTime {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.0.to_string()))
    }
}

impl FromSql for SqlTime {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        if value.data_type() == Type::Integer {
            return value
                .as_i64()
                .and_then(|second| Timestamp::from_second(second).map_err(FromSqlError::other))
                .map(Self);
        }
        value
            .as_str()?
            .parse()
            .map(Self)
            .map_err(FromSqlError::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn round_trips_as_the_text_rusqlite_wrote() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE t (at)").unwrap();
        let at: Timestamp = "2026-11-27T10:00:00.5Z".parse().unwrap();
        db.execute("INSERT INTO t VALUES (?1)", [SqlTime(at)])
            .unwrap();
        let text: String = db
            .query_row("SELECT at FROM t", [], |row| row.get(0))
            .unwrap();
        assert_eq!(text, "2026-11-27T10:00:00.5Z");
        let back: SqlTime = db
            .query_row("SELECT at FROM t", [], |row| row.get(0))
            .unwrap();
        assert_eq!(back, SqlTime(at));
    }

    #[test]
    fn reads_unix_seconds() {
        let db = Connection::open_in_memory().unwrap();
        let back: SqlTime = db
            .query_row("SELECT 1764237600", [], |row| row.get(0))
            .unwrap();
        assert_eq!(back.0, Timestamp::from_second(1_764_237_600).unwrap());
    }
}
