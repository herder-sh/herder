//! What each hub login was used for, added up from the journals hosts replicate.
//!
//! A host counts its own usage in a projection it fills as it journals each turn
//! (`herder_store::Store::usage_totals`, `failover_totals`); the hub has only the journals, so
//! it reads the same figures back out of them by the same rules: a turn counts once, the
//! first time it completed with usage, against the account, provider and model its session
//! was on then, and a fork's copy of another session's history does not count. Only the
//! accounts on hub logins, `vault-<pin>`, are added up.

use herder_protocol::{
    AccountId, FailoverTotal, HostId, HubFailoverTotal, HubUsageTotal, Provider, Timestamp,
    UsageTotal,
};
use herder_store::SqlTime;
use rusqlite::params;

use super::{HubStore, Result};
use crate::session::HUB_LOGIN_PREFIX;

/// The last `session_forked` of each copy: only what comes after it ran on its host.
const FORKED: &str = "forked AS (
    SELECT host_id, session_id, MAX(seq) AS seq FROM events
    WHERE event_type = 'session_forked' GROUP BY host_id, session_id
)";

impl HubStore {
    /// What each hub login's account was used for on each host from `since` on, per model
    /// and UTC day, ordered by pin, host, day, then model; and each one's limit hits and
    /// failovers per host, ordered by pin, then host.
    pub fn hub_usage(
        &self,
        since: Timestamp,
    ) -> Result<(Vec<HubUsageTotal>, Vec<HubFailoverTotal>)> {
        Ok((self.usage_totals(since)?, self.failover_totals(since)?))
    }

    fn usage_totals(&self, since: Timestamp) -> Result<Vec<HubUsageTotal>> {
        // `at` is RFC 3339 text in UTC whose fraction jiff leaves out when zero: text order is
        // time order only to the second, so the index narrows to a second early and
        // `unixepoch` keeps the exact period; its first ten characters are the UTC day.
        let mut stmt = self.conn.prepare_cached(&format!(
            "WITH {FORKED},
             firsts AS (
                SELECT e.host_id, e.session_id, MIN(e.seq) AS seq FROM events e
                LEFT JOIN forked f ON f.host_id = e.host_id AND f.session_id = e.session_id
                WHERE e.event_type = 'turn_completed' AND e.seq > COALESCE(f.seq, 0)
                    AND json_type(e.body, '$.usage') = 'object'
                GROUP BY e.host_id, e.session_id, json_extract(e.body, '$.turn_id')
             ),
             turns AS (
                SELECT e.host_id, substr(e.at, 1, 10) AS day,
                    json_extract(e.body, '$.usage') AS usage,
                    (SELECT json_extract(p.body, '$.account_id') FROM events p
                     WHERE p.host_id = e.host_id AND p.session_id = e.session_id
                        AND p.seq < e.seq AND p.event_type IN
                            ('session_created', 'account_switched', 'provider_switched')
                     ORDER BY p.seq DESC LIMIT 1) AS account_id,
                    (SELECT json_extract(p.body, '$.provider') FROM events p
                     WHERE p.host_id = e.host_id AND p.session_id = e.session_id
                        AND p.seq < e.seq
                        AND p.event_type IN ('session_created', 'provider_switched')
                     ORDER BY p.seq DESC LIMIT 1) AS provider,
                    (SELECT json_extract(p.body, '$.model') FROM events p
                     WHERE p.host_id = e.host_id AND p.session_id = e.session_id
                        AND p.seq < e.seq AND p.event_type IN
                            ('session_created', 'model_switched', 'provider_switched')
                     ORDER BY p.seq DESC LIMIT 1) AS model
                FROM firsts t JOIN events e
                    ON e.host_id = t.host_id AND e.session_id = t.session_id AND e.seq = t.seq
                WHERE e.at >= ?1 AND unixepoch(e.at, 'subsec') * 1000 >= ?2
             )
             SELECT account_id, host_id, day, provider, model, COUNT(*),
                SUM(json_extract(usage, '$.input')), SUM(json_extract(usage, '$.output')),
                SUM(json_extract(usage, '$.cache_read')),
                SUM(json_extract(usage, '$.cache_write')),
                TOTAL(json_extract(usage, '$.cost_usd')),
                MAX(json_extract(usage, '$.cost_estimated')
                    OR json_extract(usage, '$.cost_usd') IS NULL)
             FROM turns WHERE substr(account_id, 1, ?3) = ?4
             GROUP BY account_id, host_id, day, provider, model
             ORDER BY account_id, host_id, day, model, provider"
        ))?;
        let rows = stmt.query_map(
            params![
                SqlTime(early(since)),
                since.as_millisecond(),
                HUB_LOGIN_PREFIX.len(),
                HUB_LOGIN_PREFIX
            ],
            |row| {
                let account_id: String = row.get(0)?;
                Ok(HubUsageTotal {
                    pin: pin(&account_id),
                    host_id: HostId::new(row.get::<_, String>(1)?),
                    day: row.get(2)?,
                    usage: UsageTotal {
                        account_id: AccountId::new(account_id),
                        provider: Provider::from(row.get::<_, String>(3)?),
                        model: row.get(4)?,
                        turns: row.get(5)?,
                        input: row.get(6)?,
                        output: row.get(7)?,
                        cache_read: row.get(8)?,
                        cache_write: row.get(9)?,
                        cost_usd: row.get(10)?,
                        cost_estimated: row.get(11)?,
                    },
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// As `herder_store::Store::failover_totals` counts them, per host.
    fn failover_totals(&self, since: Timestamp) -> Result<Vec<HubFailoverTotal>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "WITH {FORKED},
             counted AS (
                SELECT e.host_id, e.event_type,
                    json_extract(e.body, '$.account_id') AS onto,
                    (SELECT json_extract(p.body, '$.account_id') FROM events p
                     WHERE p.host_id = e.host_id AND p.session_id = e.session_id
                        AND p.seq < e.seq AND p.event_type IN
                            ('session_created', 'account_switched', 'provider_switched')
                     ORDER BY p.seq DESC LIMIT 1) AS off
                FROM events e
                LEFT JOIN forked f ON f.host_id = e.host_id AND f.session_id = e.session_id
                WHERE e.event_type IN ('turn_failed', 'account_switched')
                    AND e.at >= ?1 AND unixepoch(e.at, 'subsec') * 1000 >= ?2
                    AND e.seq > COALESCE(f.seq, 0)
                    AND (e.event_type = 'account_switched' AND e.by IS NULL
                        OR json_extract(e.body, '$.error.class') = 'limit_reached')
             )
             SELECT account_id, host_id, SUM(hits), SUM(outs), SUM(ins) FROM (
                SELECT host_id, off AS account_id, event_type = 'turn_failed' AS hits,
                    event_type = 'account_switched' AS outs, 0 AS ins
                FROM counted
                UNION ALL
                SELECT host_id, onto, 0, 0, 1 FROM counted
                WHERE event_type = 'account_switched'
             )
             WHERE substr(account_id, 1, ?3) = ?4
             GROUP BY account_id, host_id ORDER BY account_id, host_id"
        ))?;
        let rows = stmt.query_map(
            params![
                SqlTime(early(since)),
                since.as_millisecond(),
                HUB_LOGIN_PREFIX.len(),
                HUB_LOGIN_PREFIX
            ],
            |row| {
                let account_id: String = row.get(0)?;
                Ok(HubFailoverTotal {
                    pin: pin(&account_id),
                    host_id: HostId::new(row.get::<_, String>(1)?),
                    failovers: FailoverTotal {
                        account_id: AccountId::new(account_id),
                        limit_hits: row.get(2)?,
                        failovers_out: row.get(3)?,
                        failovers_in: row.get(4)?,
                    },
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// A second before `since`, where the text order of `at` starts to matter.
fn early(since: Timestamp) -> Timestamp {
    Timestamp::from_second(since.as_second().saturating_sub(1)).unwrap_or(Timestamp::MIN)
}

/// The pin of the hub login whose account is `account_id`.
fn pin(account_id: &str) -> String {
    account_id
        .strip_prefix(HUB_LOGIN_PREFIX)
        .unwrap_or(account_id)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use herder_protocol::{
        Batch, ErrorClass, EventBody, MAX_BATCH_EVENTS, PermissionMode, SessionId, TurnError,
        TurnId, TurnUsage, UserId,
    };
    use herder_store::{NewEvent, Store};

    use super::*;

    /// A host's own store, journaled as its daemon journals: a turn's usage is recorded as it
    /// completes, and an imported history records none.
    struct Host {
        id: HostId,
        store: Store,
    }

    impl Host {
        fn open(dir: &std::path::Path, id: &str) -> Self {
            Self {
                id: HostId::new(id),
                store: Store::open(dir.join(format!("{id}.db"))).unwrap(),
            }
        }

        fn record(&mut self, session: &str, at: &str, by: Option<&str>, body: EventBody) {
            let stored = self
                .store
                .append(NewEvent {
                    session_id: SessionId::new(session),
                    at: at.parse().unwrap(),
                    by: by.map(UserId::new),
                    body,
                })
                .unwrap();
            if let EventBody::TurnCompleted {
                turn_id,
                usage: Some(usage),
            } = &stored.body
            {
                self.store
                    .record_turn_usage(&stored.session_id, turn_id, usage, stored.at)
                    .unwrap();
            }
        }

        /// Appends events as a fork imports another session's history: without usage.
        fn import(&mut self, session: &str, at: &str, body: EventBody) {
            self.store
                .append(NewEvent {
                    session_id: SessionId::new(session),
                    at: at.parse().unwrap(),
                    by: None,
                    body,
                })
                .unwrap();
        }

        /// Sends every session's journal to the hub, as its replicator does.
        fn replicate(&self, hub: &mut HubStore) {
            for session in self.store.sessions().unwrap() {
                let records = self
                    .store
                    .read_records_since(&session.session_id, 0, usize::MAX)
                    .unwrap();
                for events in records.chunks(MAX_BATCH_EVENTS) {
                    let batch = Batch {
                        session_id: session.session_id.clone(),
                        events: events.to_vec(),
                    };
                    hub.append(&self.id, &batch).unwrap();
                }
            }
        }
    }

    fn created(account: &str, provider: Provider, model: &str) -> EventBody {
        EventBody::SessionCreated {
            repo: "/home/dev/app".into(),
            worktree: "/home/dev/worktrees/app".into(),
            branch: None,
            provider,
            account_id: AccountId::new(account),
            model: model.into(),
            permission_mode: PermissionMode::Ask,
            parent: None,
            parent_host: None,
            task: None,
            failover_pin: None,
            chat: false,
            origin: None,
        }
    }

    fn done(turn: &str, tokens: u64, cost_usd: Option<f64>) -> EventBody {
        EventBody::TurnCompleted {
            turn_id: TurnId::new(turn),
            usage: Some(TurnUsage {
                input: tokens,
                output: tokens * 2,
                cache_read: tokens * 10,
                cache_write: tokens * 3,
                cost_usd,
                cost_estimated: false,
            }),
        }
    }

    fn limit_hit(turn: &str) -> EventBody {
        EventBody::TurnFailed {
            turn_id: TurnId::new(turn),
            error: TurnError {
                class: ErrorClass::LimitReached,
                message: "5-hour limit reached".into(),
            },
        }
    }

    fn switched(account: &str) -> EventBody {
        EventBody::AccountSwitched {
            account_id: AccountId::new(account),
        }
    }

    /// Three hosts with sessions on two Claude logins and a Codex one, a local account, a
    /// fork, failovers, a model switch, turns on two days and one before the period.
    fn fleet(dir: &std::path::Path) -> Vec<Host> {
        let (ada, carol) = ("vault-claude-ada", "vault-claude-carol");
        let mut one = Host::open(dir, "h1");
        one.record(
            "a",
            "2027-01-10T09:00:00Z",
            Some("alice"),
            created(ada, Provider::Claude, "opus"),
        );
        // Before the period.
        one.record(
            "a",
            "2027-01-10T09:01:00Z",
            None,
            done("t0", 1000, Some(9.0)),
        );
        one.record(
            "a",
            "2027-01-14T23:59:59.5Z",
            None,
            done("t1", 10, Some(0.5)),
        );
        // A turn completed again keeps its first figures; one without usage counts nothing.
        one.record("a", "2027-01-15T00:00:01Z", None, done("t1", 99, Some(4.0)));
        one.record(
            "a",
            "2027-01-15T00:02:00Z",
            None,
            EventBody::TurnCompleted {
                turn_id: TurnId::new("t2"),
                usage: None,
            },
        );
        one.record(
            "a",
            "2027-01-15T00:03:00Z",
            Some("alice"),
            EventBody::ModelSwitched {
                model: "sonnet".into(),
            },
        );
        one.record("a", "2027-01-15T00:04:00Z", None, done("t3", 20, None));
        one.record("a", "2027-01-15T00:05:00Z", None, limit_hit("t4"));
        one.record("a", "2027-01-15T00:05:01Z", None, switched(carol));
        one.record(
            "a",
            "2027-01-15T00:06:00Z",
            None,
            done("t4", 30, Some(0.25)),
        );
        // A user's switch is no failover.
        one.record("a", "2027-01-15T00:07:00Z", Some("alice"), switched(ada));
        one.record(
            "a",
            "2027-01-15T00:08:00Z",
            None,
            done("t5", 40, Some(0.25)),
        );
        one.record(
            "local",
            "2027-01-15T01:00:00Z",
            Some("alice"),
            created("main", Provider::Claude, "opus"),
        );
        one.record(
            "local",
            "2027-01-15T01:01:00Z",
            None,
            done("t1", 500, Some(2.0)),
        );
        one.record(
            "x",
            "2027-01-15T02:00:00Z",
            Some("alice"),
            created("vault-codex-bo", Provider::Codex, "gpt-5"),
        );
        one.record(
            "x",
            "2027-01-15T02:01:00Z",
            None,
            done("t1", 7, Some(0.125)),
        );

        // h2 forks session `a`: its copy of a's history counts on h1 only.
        let mut two = Host::open(dir, "h2");
        let history = one
            .store
            .read_records_since(&SessionId::new("a"), 0, usize::MAX)
            .unwrap();
        for record in history {
            two.import("f", &record.at.to_string(), record.body.decode());
        }
        two.record(
            "f",
            "2027-01-15T03:00:00Z",
            Some("bob"),
            EventBody::SessionForked {
                from_session: SessionId::new("a"),
                from_host: HostId::new("h1"),
            },
        );
        two.record("f", "2027-01-15T03:01:00Z", None, done("t9", 50, Some(0.5)));
        two.record("f", "2027-01-15T03:02:00Z", None, limit_hit("t10"));
        two.record("f", "2027-01-15T03:02:01Z", None, switched(carol));
        two.record(
            "f",
            "2027-01-15T03:03:00Z",
            None,
            done("t10", 60, Some(0.5)),
        );
        two.record(
            "b",
            "2027-01-16T08:00:00Z",
            Some("bob"),
            created(carol, Provider::Claude, "opus"),
        );
        two.record("b", "2027-01-16T08:01:00Z", None, done("t1", 70, Some(1.0)));
        two.record(
            "b",
            "2027-01-16T08:02:00Z",
            None,
            EventBody::ProviderSwitched {
                provider: Provider::Codex,
                account_id: AccountId::new("vault-codex-bo"),
                model: "gpt-5".into(),
            },
        );
        two.record("b", "2027-01-16T08:03:00Z", None, done("t2", 80, Some(1.0)));

        let mut three = Host::open(dir, "h3");
        three.record(
            "c",
            "2027-01-16T10:00:00Z",
            Some("carl"),
            created(ada, Provider::Claude, "opus"),
        );
        three.record("c", "2027-01-16T10:01:00Z", None, done("t1", 90, Some(0.5)));
        vec![one, two, three]
    }

    /// A row as the test reads it: pin, host, day, model, turns, input, cost, estimated.
    type Row<'a> = (&'a str, &'a str, &'a str, &'a str, u64, u64, f64, bool);

    const SINCE: &str = "2027-01-14T00:00:00Z";

    #[test]
    fn each_hosts_rows_add_up_to_its_own_totals_on_hub_logins() {
        let dir = tempfile::tempdir().unwrap();
        let hosts = fleet(dir.path());
        let mut hub = HubStore::open(dir.path().join("hub.db")).unwrap();
        for host in &hosts {
            host.replicate(&mut hub);
        }
        let since: Timestamp = SINCE.parse().unwrap();
        let (totals, failovers) = hub.hub_usage(since).unwrap();

        for host in &hosts {
            // The hub's rows of this host, over its days, keyed as the host keys its own.
            let mut added: BTreeMap<(String, String, String), UsageTotal> = BTreeMap::new();
            for row in totals.iter().filter(|row| row.host_id == host.id) {
                assert_eq!(row.pin, &row.usage.account_id.as_str()["vault-".len()..]);
                let u = &row.usage;
                let key = (
                    u.account_id.to_string(),
                    u.provider.as_str().to_owned(),
                    u.model.clone(),
                );
                let total = added.entry(key).or_insert_with(|| UsageTotal {
                    turns: 0,
                    input: 0,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                    cost_usd: 0.0,
                    cost_estimated: false,
                    ..u.clone()
                });
                total.turns += u.turns;
                total.input += u.input;
                total.output += u.output;
                total.cache_read += u.cache_read;
                total.cache_write += u.cache_write;
                total.cost_usd += u.cost_usd;
                total.cost_estimated |= u.cost_estimated;
            }
            let own: Vec<UsageTotal> = host
                .store
                .usage_totals(since)
                .unwrap()
                .into_iter()
                .filter(|total| total.account_id.as_str().starts_with("vault-"))
                .collect();
            assert!(!own.is_empty(), "{} has no usage on hub logins", host.id);
            let mut added: Vec<UsageTotal> = added.into_values().collect();
            added.sort_by(|a, b| (&a.account_id, &a.model).cmp(&(&b.account_id, &b.model)));
            assert_eq!(added, own, "{}", host.id);

            let hub_failovers: Vec<FailoverTotal> = failovers
                .iter()
                .filter(|row| row.host_id == host.id)
                .map(|row| row.failovers.clone())
                .collect();
            let own_failovers: Vec<FailoverTotal> = host
                .store
                .failover_totals(since)
                .unwrap()
                .into_iter()
                .filter(|total| total.account_id.as_str().starts_with("vault-"))
                .collect();
            assert_eq!(hub_failovers, own_failovers, "{}", host.id);
        }
    }

    #[test]
    fn rows_go_per_login_host_utc_day_and_model() {
        let dir = tempfile::tempdir().unwrap();
        let hosts = fleet(dir.path());
        let mut hub = HubStore::open(dir.path().join("hub.db")).unwrap();
        for host in &hosts {
            host.replicate(&mut hub);
        }
        let (totals, failovers) = hub.hub_usage(SINCE.parse().unwrap()).unwrap();
        let rows: Vec<Row<'_>> = totals
            .iter()
            .map(|row| {
                let u = &row.usage;
                (
                    row.pin.as_str(),
                    row.host_id.as_str(),
                    row.day.as_str(),
                    u.model.as_str(),
                    u.turns,
                    u.input,
                    u.cost_usd,
                    u.cost_estimated,
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("claude-ada", "h1", "2027-01-14", "opus", 1, 10, 0.5, false),
                (
                    "claude-ada",
                    "h1",
                    "2027-01-15",
                    "sonnet",
                    2,
                    60,
                    0.25,
                    true
                ),
                (
                    "claude-ada",
                    "h2",
                    "2027-01-15",
                    "sonnet",
                    1,
                    50,
                    0.5,
                    false
                ),
                ("claude-ada", "h3", "2027-01-16", "opus", 1, 90, 0.5, false),
                (
                    "claude-carol",
                    "h1",
                    "2027-01-15",
                    "sonnet",
                    1,
                    30,
                    0.25,
                    false
                ),
                (
                    "claude-carol",
                    "h2",
                    "2027-01-15",
                    "sonnet",
                    1,
                    60,
                    0.5,
                    false
                ),
                (
                    "claude-carol",
                    "h2",
                    "2027-01-16",
                    "opus",
                    1,
                    70,
                    1.0,
                    false
                ),
                ("codex-bo", "h1", "2027-01-15", "gpt-5", 1, 7, 0.125, false),
                ("codex-bo", "h2", "2027-01-16", "gpt-5", 1, 80, 1.0, false),
            ]
        );
        let counts: Vec<(&str, &str, u64, u64, u64)> = failovers
            .iter()
            .map(|row| {
                let f = &row.failovers;
                (
                    row.pin.as_str(),
                    row.host_id.as_str(),
                    f.limit_hits,
                    f.failovers_out,
                    f.failovers_in,
                )
            })
            .collect();
        assert_eq!(
            counts,
            [
                ("claude-ada", "h1", 1, 1, 0),
                ("claude-ada", "h2", 1, 1, 0),
                ("claude-carol", "h1", 0, 0, 1),
                ("claude-carol", "h2", 0, 0, 1),
            ]
        );
    }
}
