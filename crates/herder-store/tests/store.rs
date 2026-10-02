use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use herder_protocol::{
    AccountId, CiStatus, Event, EventBody, Item, ItemBody, ItemId, Mergeable, PermissionMode,
    PrState, Provider, PullRequest, ReviewStatus, SessionId, SessionStatus, Timestamp, TurnId,
    UserId,
};
use herder_store::{Error, NewEvent, Session, Store};
use proptest::prelude::*;
use rusqlite::Connection;

const CRASH_DB_ENV: &str = "HERDER_STORE_CRASH_DB";

fn at(second: i64) -> Timestamp {
    Timestamp::from_second(1_800_000_000 + second).unwrap()
}

fn created() -> EventBody {
    EventBody::SessionCreated {
        repo: "/src/herder".into(),
        worktree: "/src/herder-wt".into(),
        branch: "feature".into(),
        provider: Provider::Claude,
        account_id: AccountId::new("acct-1"),
        model: "opus".into(),
        permission_mode: PermissionMode::Ask,
    }
}

fn pr(number: u64, state: PrState) -> PullRequest {
    PullRequest {
        number,
        url: format!("https://github.com/herder-sh/herder/pull/{number}"),
        title: format!("PR {number}"),
        state,
        ci: CiStatus::Pending,
        review: ReviewStatus::Required,
        mergeable: Mergeable::Unknown,
    }
}

fn message(text: String) -> EventBody {
    EventBody::ItemAdded {
        item: Item {
            id: ItemId::new("item"),
            turn_id: TurnId::new("turn"),
            body: ItemBody::UserMessage { text },
        },
    }
}

fn new_event(session: &SessionId, second: i64, body: EventBody) -> NewEvent {
    NewEvent {
        session_id: session.clone(),
        at: at(second),
        by: Some(UserId::new("user-1")),
        body,
    }
}

/// The projections, recomputed from scratch by folding a session's journal.
fn fold(events: &[Event]) -> (Option<Session>, Vec<PullRequest>) {
    let mut session: Option<Session> = None;
    let mut prs = BTreeMap::new();
    for event in events {
        if let EventBody::SessionCreated {
            repo,
            worktree,
            branch,
            provider,
            account_id,
            model,
            permission_mode,
        } = &event.body
        {
            session = Some(Session {
                session_id: event.session_id.clone(),
                repo: repo.clone(),
                worktree: worktree.clone(),
                branch: branch.clone(),
                provider: provider.clone(),
                account_id: account_id.clone(),
                model: model.clone(),
                permission_mode: *permission_mode,
                status: SessionStatus::Idle,
                last_seq: 0,
                updated_at: event.at,
            });
        }
        let s = session.as_mut().expect("first event creates the session");
        s.last_seq = event.seq;
        s.updated_at = event.at;
        match &event.body {
            EventBody::SessionStatusChanged { status } => s.status = *status,
            EventBody::ModelSwitched { model } => s.model = model.clone(),
            EventBody::AccountSwitched { account_id } => s.account_id = account_id.clone(),
            EventBody::ProviderSwitched {
                provider,
                account_id,
                model,
            } => {
                s.provider = provider.clone();
                s.account_id = account_id.clone();
                s.model = model.clone();
            }
            EventBody::PermissionModeChanged { mode } => s.permission_mode = *mode,
            EventBody::PrLinked { pr } => {
                prs.insert(pr.number, pr.clone());
            }
            EventBody::PrUpdated { pr } => {
                if let Some(tracked) = prs.get_mut(&pr.number) {
                    *tracked = pr.clone();
                }
            }
            EventBody::PrUnlinked { number } => {
                prs.remove(number);
            }
            _ => {}
        }
    }
    (session, prs.into_values().collect())
}

/// Asserts that the session's projections equal a fold of its journal.
fn assert_projections_match_journal(store: &Store, session: &SessionId) {
    let journal = store.read_since(session, 0, usize::MAX).unwrap();
    let latest = store.latest_seq(session).unwrap();
    assert_eq!(journal.len() as u64, latest, "journal is gap-free from 1");
    let (expected_session, expected_prs) = fold(&journal);
    let projected = store.session(session).unwrap();
    if let Some(projected) = &projected {
        assert_eq!(
            projected.last_seq, latest,
            "projection seq equals journal seq"
        );
    }
    assert_eq!(projected, expected_session);
    assert_eq!(store.session_prs(session).unwrap(), expected_prs);
}

#[test]
fn append_assigns_seqs_and_updates_projections() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("herder.db")).unwrap();
    let s = SessionId::new("s1");

    let first = store.append(new_event(&s, 0, created())).unwrap();
    assert_eq!(first.seq, 1);
    let session = store.session(&s).unwrap().unwrap();
    assert_eq!(session.status, SessionStatus::Idle);
    assert_eq!(session.last_seq, 1);

    let bodies = [
        EventBody::SessionStatusChanged {
            status: SessionStatus::Running,
        },
        EventBody::ModelSwitched {
            model: "sonnet".into(),
        },
        EventBody::ProviderSwitched {
            provider: Provider::Other("newcli".into()),
            account_id: AccountId::new("acct-2"),
            model: "m2".into(),
        },
        EventBody::PermissionModeChanged {
            mode: PermissionMode::FullAccess,
        },
        EventBody::PrLinked {
            pr: pr(7, PrState::Draft),
        },
        EventBody::PrLinked {
            pr: pr(8, PrState::Open),
        },
        EventBody::PrUpdated {
            pr: pr(7, PrState::Open),
        },
        EventBody::PrUnlinked { number: 8 },
        // Arrives after the unlink: journaled, but does not track 8 again.
        EventBody::PrUpdated {
            pr: pr(8, PrState::Merged),
        },
        message("hello".into()),
    ];
    for (i, body) in bodies.into_iter().enumerate() {
        let event = store.append(new_event(&s, i as i64 + 1, body)).unwrap();
        assert_eq!(event.seq, i as u64 + 2);
    }

    let session = store.session(&s).unwrap().unwrap();
    assert_eq!(session.status, SessionStatus::Running);
    assert_eq!(session.provider, Provider::Other("newcli".into()));
    assert_eq!(session.account_id, AccountId::new("acct-2"));
    assert_eq!(session.model, "m2");
    assert_eq!(session.permission_mode, PermissionMode::FullAccess);
    assert_eq!(session.last_seq, 11);
    assert_eq!(session.updated_at, at(10));
    assert_eq!(store.session_prs(&s).unwrap(), vec![pr(7, PrState::Open)]);
    assert_eq!(store.sessions().unwrap(), vec![session]);
    assert_projections_match_journal(&store, &s);
}

#[test]
fn append_rejects_invalid_events_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("herder.db")).unwrap();
    let s = SessionId::new("s1");

    let err = store.append(new_event(&s, 0, message("x".into())));
    assert!(matches!(err, Err(Error::UnknownSession(id)) if id == s));
    assert_eq!(store.latest_seq(&s).unwrap(), 0);
    assert_eq!(store.session(&s).unwrap(), None);

    store.append(new_event(&s, 0, created())).unwrap();
    assert!(matches!(
        store.append(new_event(&s, 1, created())),
        Err(Error::SessionExists(_))
    ));
    assert!(matches!(
        store.append(new_event(&s, 1, EventBody::Unknown)),
        Err(Error::Encode(_))
    ));
    let unknown_status = EventBody::SessionStatusChanged {
        status: SessionStatus::Unknown,
    };
    assert!(matches!(
        store.append(new_event(&s, 1, unknown_status)),
        Err(Error::Encode(_))
    ));
    assert_eq!(store.latest_seq(&s).unwrap(), 1);
    assert_projections_match_journal(&store, &s);
}

#[test]
fn undecodable_bodies_read_as_unknown_keeping_their_seq() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herder.db");
    let mut store = Store::open(&path).unwrap();
    let s = SessionId::new("s1");
    store.append(new_event(&s, 0, created())).unwrap();

    // As a newer build would write them: an unknown type, and a known type in a changed shape.
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch(
        "INSERT INTO events (session_id, seq, at, by, event_type, body) VALUES
           ('s1', 2, '2027-01-15T08:00:00Z', NULL, 'from_the_future', '{\"type\":\"from_the_future\",\"x\":1}'),
           ('s1', 3, '2027-01-15T08:00:01Z', NULL, 'model_switched', '{\"type\":\"model_switched\",\"model\":42}');",
    )
    .unwrap();
    drop(raw);
    store
        .append(new_event(
            &s,
            5,
            EventBody::ModelSwitched { model: "m".into() },
        ))
        .unwrap();

    let events = store.read_since(&s, 0, 10).unwrap();
    let seqs: Vec<_> = events.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, [1, 2, 3, 4]);
    assert_eq!(events[1].body, EventBody::Unknown);
    assert_eq!(events[2].body, EventBody::Unknown);
    assert_eq!(
        events[3].body,
        EventBody::ModelSwitched { model: "m".into() }
    );
}

#[test]
fn stores_event_type_and_uses_wal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herder.db");
    let mut store = Store::open(&path).unwrap();
    let s = SessionId::new("s1");
    store.append(new_event(&s, 0, created())).unwrap();

    let raw = Connection::open(&path).unwrap();
    let event_type: String = raw
        .query_row("SELECT event_type FROM events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(event_type, "session_created");
    let mode: String = raw
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
}

#[test]
fn migrations_create_reopen_and_refuse_newer_schemas() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herder.db");
    let s = SessionId::new("s1");
    {
        let mut store = Store::open(&path).unwrap();
        store.append(new_event(&s, 0, created())).unwrap();
    }
    let version = |path: &Path| -> u32 {
        Connection::open(path)
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap()
    };
    assert_eq!(version(&path), 1);

    let store = Store::open(&path).unwrap();
    assert_eq!(store.latest_seq(&s).unwrap(), 1);
    assert_eq!(version(&path), 1);
    drop(store);

    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 2)
        .unwrap();
    assert!(matches!(
        Store::open(&path),
        Err(Error::TooNew {
            found: 2,
            supported: 1
        })
    ));
}

fn body_strategy() -> impl Strategy<Value = EventBody> {
    prop_oneof![
        any::<String>().prop_map(message),
        "[a-z0-9-]{1,12}".prop_map(|model| EventBody::ModelSwitched { model }),
        prop_oneof![
            Just(SessionStatus::Idle),
            Just(SessionStatus::Running),
            Just(SessionStatus::NeedsYou),
        ]
        .prop_map(|status| EventBody::SessionStatusChanged { status }),
        (1u64..4).prop_map(|n| EventBody::PrLinked {
            pr: pr(n, PrState::Open)
        }),
        (1u64..4).prop_map(|n| EventBody::PrUpdated {
            pr: pr(n, PrState::Merged)
        }),
        (1u64..4).prop_map(|number| EventBody::PrUnlinked { number }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Appending N events then reading from any cursor returns exactly the suffix.
    #[test]
    fn reading_from_any_cursor_returns_exactly_the_suffix(
        ops in prop::collection::vec((0usize..3, body_strategy(), any::<bool>()), 0..40),
        limit in 1usize..8,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("herder.db")).unwrap();
        let sessions: Vec<_> = (0..3).map(|i| SessionId::new(format!("s{i}"))).collect();
        let mut journals: Vec<Vec<Event>> = vec![Vec::new(); 3];
        for (i, s) in sessions.iter().enumerate() {
            journals[i].push(store.append(new_event(s, 0, created())).unwrap());
        }
        for (second, (i, body, by_user)) in ops.into_iter().enumerate() {
            let mut event = new_event(&sessions[i], second as i64 + 1, body);
            if !by_user {
                event.by = None;
            }
            let stored = store.append(event).unwrap();
            prop_assert_eq!(stored.seq, journals[i].len() as u64 + 1);
            journals[i].push(stored);
        }

        for (s, journal) in sessions.iter().zip(&journals) {
            let n = journal.len() as u64;
            prop_assert_eq!(store.latest_seq(s).unwrap(), n);
            for cursor in 0..=n + 1 {
                let suffix = &journal[(cursor as usize).min(journal.len())..];
                prop_assert_eq!(&store.read_since(s, cursor, usize::MAX).unwrap()[..], suffix);
            }

            // Forward paging from 0 and backward paging from the head both rebuild the journal.
            let (mut forward, mut cursor) = (Vec::new(), 0);
            loop {
                let page = store.read_since(s, cursor, limit).unwrap();
                prop_assert!(page.len() <= limit);
                let Some(last) = page.last() else { break };
                cursor = last.seq;
                forward.extend(page);
            }
            prop_assert_eq!(&forward, journal);
            let (mut backward, mut before) = (Vec::new(), None);
            loop {
                let page = store.read_page(s, before, limit).unwrap();
                prop_assert!(page.len() <= limit);
                let Some(first) = page.first() else { break };
                before = Some(first.seq);
                backward.splice(0..0, page);
            }
            prop_assert_eq!(&backward, journal);
            assert_projections_match_journal(&store, s);
        }
    }
}

/// Child half of `crash_mid_write_never_leaves_a_projection_ahead_of_the_journal`: appends
/// until killed. A no-op unless the parent set `HERDER_STORE_CRASH_DB`.
#[test]
fn crash_child() {
    let Some(path) = std::env::var_os(CRASH_DB_ENV) else {
        return;
    };
    let mut store = Store::open(path).unwrap();
    let sessions: Vec<_> = (0..4).map(|i| SessionId::new(format!("s{i}"))).collect();
    for s in &sessions {
        store.append(new_event(s, 0, created())).unwrap();
    }
    for i in 1.. {
        let s = &sessions[i as usize % sessions.len()];
        let body = match i % 5 {
            0 => EventBody::PrLinked {
                pr: pr(i as u64 % 3, PrState::Open),
            },
            1 => EventBody::PrUpdated {
                pr: pr(i as u64 % 3, PrState::Merged),
            },
            2 => EventBody::PrUnlinked {
                number: i as u64 % 3,
            },
            3 => EventBody::ModelSwitched {
                model: format!("m{i}"),
            },
            _ => message("x".repeat(i as usize % 4096)),
        };
        let event = store.append(new_event(s, i, body)).unwrap();
        println!("appended {} {}", s, event.seq);
    }
}

#[test]
fn crash_mid_write_never_leaves_a_projection_ahead_of_the_journal() {
    for round in 0..24u64 {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("herder.db");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["crash_child", "--exact", "--nocapture", "--test-threads=1"])
            .env(CRASH_DB_ENV, &path)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();

        // Drain the child's acknowledgements on a thread so it never blocks on a full pipe.
        let (started_tx, started) = mpsc::channel();
        let stdout = child.stdout.take().unwrap();
        let reader = thread::spawn(move || {
            let mut acknowledged: BTreeMap<String, u64> = BTreeMap::new();
            for line in BufReader::new(stdout).lines().map(Result::unwrap) {
                if let Some(rest) = line.strip_prefix("appended ") {
                    let (session, seq) = rest.split_once(' ').unwrap();
                    acknowledged.insert(session.to_owned(), seq.parse().unwrap());
                    let _ = started_tx.send(());
                }
            }
            acknowledged
        });

        // Kill the child while it is appending, at a different point each round.
        started.recv().unwrap();
        thread::sleep(Duration::from_micros(300 + round * 1_700));
        child.kill().unwrap();
        child.wait().unwrap();
        let acknowledged = reader.join().unwrap();

        let store = Store::open(&path).unwrap();
        for i in 0..4 {
            let s = SessionId::new(format!("s{i}"));
            assert_projections_match_journal(&store, &s);
            let acked = acknowledged.get(s.as_str()).copied().unwrap_or(0);
            assert!(
                store.latest_seq(&s).unwrap() >= acked,
                "a committed append survives the crash"
            );
        }
    }
}
