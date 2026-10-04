use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use herder_protocol::{
    AccountId, Attachment, AttachmentId, CiStatus, CommandId, CommandResult, Event, EventBody,
    HostId, Item, ItemBody, ItemId, JournalRecord, Mergeable, PermissionMode, PrState, PromptId,
    Provider, PullRequest, ReviewStatus, SessionId, SessionStatus, Timestamp, TitleSource, TurnId,
    UserId,
};
use herder_store::{
    COMMAND_RESULTS_KEPT, Error, NativeSession, NewEvent, QueuedPrompt, Session, Store,
};
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
        parent: None,
        parent_host: None,
        task: None,
        max_children: None,
        failover_pin: None,
    }
}

/// `session_created` of a child session of `parent`'s task.
fn child_created(parent: &SessionId, task: &str) -> EventBody {
    let EventBody::SessionCreated {
        repo,
        worktree,
        branch,
        provider,
        account_id,
        model,
        permission_mode,
        ..
    } = created()
    else {
        unreachable!()
    };
    EventBody::SessionCreated {
        repo,
        worktree: format!("{worktree}-{task}"),
        branch: format!("{branch}-{task}"),
        provider,
        account_id,
        model,
        permission_mode,
        parent: Some(parent.clone()),
        parent_host: None,
        task: Some(task.into()),
        max_children: None,
        failover_pin: None,
    }
}

fn pr(number: u64, state: PrState) -> PullRequest {
    PullRequest {
        number,
        url: format!("https://github.com/herder-sh/herder/pull/{number}"),
        title: format!("PR {number}"),
        head_branch: Some(format!("pr-{number}")),
        state,
        ci: CiStatus::Pending,
        review: ReviewStatus::Required,
        mergeable: Mergeable::Unknown,
    }
}

fn message(text: String) -> EventBody {
    EventBody::ItemAdded {
        item: Item {
            agent_message: None,
            parent_call_id: None,
            id: ItemId::new("item"),
            turn_id: TurnId::new("turn"),
            body: ItemBody::UserMessage {
                text,
                attachments: Vec::new(),
            },
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

fn titled(title: &str, source: TitleSource) -> EventBody {
    EventBody::TitleChanged {
        title: title.into(),
        source,
    }
}

fn checked_out(branch: &str) -> EventBody {
    EventBody::BranchCheckedOut {
        branch: branch.into(),
    }
}

/// The projections of one session.
#[derive(Debug, PartialEq)]
struct Projections {
    session: Option<Session>,
    prs: Vec<PullRequest>,
    branches: Vec<String>,
}

/// The projections, recomputed from scratch by folding a session's journal.
fn fold(events: &[Event]) -> Projections {
    let mut session: Option<Session> = None;
    let mut prs = BTreeMap::new();
    let mut branches: Vec<String> = Vec::new();
    for event in events {
        if let EventBody::SessionCreated {
            repo,
            worktree,
            branch,
            provider,
            account_id,
            model,
            permission_mode,
            parent,
            parent_host,
            task,
            ..
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
                parent: parent.clone(),
                parent_host: parent_host.clone(),
                task: task.clone(),
                title: None,
                title_source: None,
                status: SessionStatus::Idle,
                last_seq: 0,
                updated_at: event.at,
            });
            branches.push(branch.clone());
        }
        let s = session.as_mut().expect("first event creates the session");
        s.last_seq = event.seq;
        s.updated_at = event.at;
        match &event.body {
            EventBody::SessionStatusChanged { status, .. } => s.status = *status,
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
            EventBody::TitleChanged { title, source } => {
                s.title = Some(title.clone());
                s.title_source = Some(*source);
            }
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
            EventBody::BranchCheckedOut { branch } if !branches.contains(branch) => {
                branches.push(branch.clone());
            }
            _ => {}
        }
    }
    Projections {
        session,
        prs: prs.into_values().collect(),
        branches,
    }
}

/// Asserts that the session's projections equal a fold of its journal.
fn assert_projections_match_journal(store: &Store, session: &SessionId) {
    let journal = store.read_since(session, 0, usize::MAX).unwrap();
    let latest = store.latest_seq(session).unwrap();
    assert_eq!(journal.len() as u64, latest, "journal is gap-free from 1");
    let projected = Projections {
        session: store.session(session).unwrap(),
        prs: store.session_prs(session).unwrap(),
        branches: store.session_branches(session).unwrap(),
    };
    if let Some(projected) = &projected.session {
        assert_eq!(
            projected.last_seq, latest,
            "projection seq equals journal seq"
        );
    }
    assert_eq!(projected, fold(&journal));
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
            retry_at: None,
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
        checked_out("spike"),
        // Already owned, the created branch included: journaled, but listed once.
        checked_out("feature"),
        checked_out("spike"),
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
    assert_eq!(session.last_seq, 14);
    assert_eq!(session.updated_at, at(13));
    assert_eq!(store.session_prs(&s).unwrap(), vec![pr(7, PrState::Open)]);
    assert_eq!(store.session_branches(&s).unwrap(), ["feature", "spike"]);
    assert_eq!(store.sessions().unwrap(), vec![session]);
    assert_projections_match_journal(&store, &s);
}

#[test]
fn the_latest_title_and_its_source_are_projected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herder.db");
    let s = SessionId::new("s1");
    let mut store = Store::open(&path).unwrap();
    store.append(new_event(&s, 0, created())).unwrap();
    let session = store.session(&s).unwrap().unwrap();
    assert_eq!((session.title, session.title_source), (None, None));

    let titles = [
        ("Fix the auth tests", TitleSource::Auto),
        ("Flaky auth tests", TitleSource::User),
        ("Auth test fixes", TitleSource::Auto),
    ];
    for (second, (title, source)) in titles.into_iter().enumerate() {
        store
            .append(new_event(&s, second as i64 + 1, titled(title, source)))
            .unwrap();
        let session = store.session(&s).unwrap().unwrap();
        assert_eq!(
            (session.title.as_deref(), session.title_source),
            (Some(title), Some(source))
        );
    }
    assert_projections_match_journal(&store, &s);
    drop(store);

    let store = Store::open(&path).unwrap();
    let session = store.session(&s).unwrap().unwrap();
    assert_eq!(
        (session.title.as_deref(), session.title_source),
        (Some("Auth test fixes"), Some(TitleSource::Auto))
    );
}

#[test]
fn children_list_a_task_tree() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("herder.db")).unwrap();
    let primary = SessionId::new("primary");
    let other = SessionId::new("other");
    let (api, docs) = (SessionId::new("child-api"), SessionId::new("child-docs"));

    store.append(new_event(&primary, 0, created())).unwrap();
    store.append(new_event(&other, 0, created())).unwrap();
    for (second, (child, task)) in [(&docs, "docs"), (&api, "api")].into_iter().enumerate() {
        let second = second as i64 * 2 + 1;
        store
            .append(new_event(child, second, child_created(&primary, task)))
            .unwrap();
        store
            .append(new_event(
                &primary,
                second + 1,
                EventBody::ChildSpawned {
                    child_session_id: child.clone(),
                    host_id: None,
                    task: task.into(),
                },
            ))
            .unwrap();
    }
    store
        .append(new_event(
            &primary,
            5,
            EventBody::ChildReported {
                child_session_id: api.clone(),
                turn_id: TurnId::new("turn"),
                summary: "Done.".into(),
            },
        ))
        .unwrap();

    let children = store.children(&primary).unwrap();
    let tree: Vec<_> = children
        .iter()
        .map(|c| (c.session_id.as_str(), c.parent.as_ref(), c.task.as_deref()))
        .collect();
    assert_eq!(
        tree,
        [
            ("child-api", Some(&primary), Some("api")),
            ("child-docs", Some(&primary), Some("docs")),
        ]
    );
    assert_eq!(children[0], store.session(&api).unwrap().unwrap());
    assert!(store.children(&other).unwrap().is_empty());
    assert!(store.children(&api).unwrap().is_empty());

    let listed: Vec<_> = store
        .sessions()
        .unwrap()
        .into_iter()
        .map(|s| (s.session_id, s.parent))
        .collect();
    assert_eq!(
        listed,
        [
            (api.clone(), Some(primary.clone())),
            (docs.clone(), Some(primary.clone())),
            (other, None),
            (primary.clone(), None),
        ]
    );
    for s in [&primary, &api, &docs] {
        assert_projections_match_journal(&store, s);
    }
}

#[test]
fn a_child_needs_an_existing_parent() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("herder.db")).unwrap();
    let child = SessionId::new("child");
    let missing = SessionId::new("missing");

    let err = store.append(new_event(&child, 0, child_created(&missing, "t")));
    assert!(matches!(err, Err(Error::UnknownParent(id)) if id == missing));
    let err = store.append(new_event(&child, 0, child_created(&child, "t")));
    assert!(matches!(err, Err(Error::UnknownParent(id)) if id == child));
    assert_eq!(store.latest_seq(&child).unwrap(), 0);
    assert_eq!(store.session(&child).unwrap(), None);
}

#[test]
fn a_child_of_a_remote_parent_needs_no_local_parent() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("herder.db")).unwrap();
    let primary = SessionId::new("remote-primary");
    let child = SessionId::new("child");
    let host = HostId::new("mac");

    let mut body = child_created(&primary, "build");
    if let EventBody::SessionCreated { parent_host, .. } = &mut body {
        *parent_host = Some(host.clone());
    }
    store.append(new_event(&child, 0, body)).unwrap();

    let session = store.session(&child).unwrap().unwrap();
    assert_eq!(session.parent, Some(primary.clone()));
    assert_eq!(session.parent_host, Some(host));
    // The remote primary is not a session of this store.
    assert_eq!(store.session(&primary).unwrap(), None);
    assert_projections_match_journal(&store, &child);
}

#[test]
fn branches_are_per_session_and_outlive_a_status_change() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("herder.db")).unwrap();
    let (a, b) = (SessionId::new("a"), SessionId::new("b"));
    store.append(new_event(&a, 0, created())).unwrap();
    store.append(new_event(&b, 1, created())).unwrap();
    store
        .append(new_event(&a, 2, checked_out("fix/login")))
        .unwrap();
    let archived = EventBody::SessionStatusChanged {
        retry_at: None,
        status: SessionStatus::Archived,
    };
    store.append(new_event(&a, 3, archived)).unwrap();

    assert_eq!(
        store.session_branches(&a).unwrap(),
        ["feature", "fix/login"]
    );
    assert_eq!(store.session_branches(&b).unwrap(), ["feature"]);
    assert!(
        store
            .session_branches(&SessionId::new("missing"))
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        store.append(new_event(&SessionId::new("missing"), 4, checked_out("x"))),
        Err(Error::UnknownSession(_))
    ));
    for s in [&a, &b] {
        assert_projections_match_journal(&store, s);
    }
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
        retry_at: None,
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
fn records_keep_bodies_this_build_cannot_decode() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herder.db");
    let mut store = Store::open(&path).unwrap();
    let s = SessionId::new("s1");
    let first = store.append(new_event(&s, 0, created())).unwrap();
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch(
        "INSERT INTO events (session_id, seq, at, by, event_type, body) VALUES
           ('s1', 2, '2027-01-15T08:00:00Z', 'u1', 'from_the_future', '{\"type\":\"from_the_future\",\"x\":1}');",
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

    let records = store.read_records_since(&s, 0, 10).unwrap();
    let seqs: Vec<_> = records.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, [1, 2, 3]);
    assert_eq!(records[0], JournalRecord::from_event(&first).unwrap());
    assert_eq!(records[1].body.event_type(), "from_the_future");
    assert_eq!(records[1].body.as_json()["x"], 1);
    assert_eq!(records[1].by, Some(UserId::new("u1")));
    assert_eq!(
        records[2].body.decode(),
        EventBody::ModelSwitched { model: "m".into() }
    );
    let after = store.read_records_since(&s, 1, 1).unwrap();
    assert_eq!(after.iter().map(|r| r.seq).collect::<Vec<_>>(), [2]);
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
    assert_eq!(version(&path), 12);

    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.latest_seq(&s).unwrap(), 1);
    assert_eq!(version(&path), 12);
    store
        .append(new_event(
            &s,
            1,
            EventBody::PrLinked {
                pr: pr(7, PrState::Open),
            },
        ))
        .unwrap();
    drop(store);

    // Back to the v11 schema, as a build before remote parents left it; reopening migrates
    // it, and a session from before has no remote parent.
    Connection::open(&path)
        .unwrap()
        .execute_batch("ALTER TABLE sessions DROP COLUMN parent_host; PRAGMA user_version = 11;")
        .unwrap();
    let store = Store::open(&path).unwrap();
    assert_eq!(version(&path), 12);
    assert_eq!(store.session(&s).unwrap().unwrap().parent_host, None);
    drop(store);

    // Back to the v7 schema, as a build before session titles left it; reopening migrates
    // it, and the session is untitled until its first title.
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "ALTER TABLE sessions DROP COLUMN parent_host;
             ALTER TABLE sessions DROP COLUMN title; ALTER TABLE sessions DROP COLUMN title_source;
             ALTER TABLE queued_prompts DROP COLUMN prompt_id; ALTER TABLE queued_prompts DROP COLUMN agent_message; ALTER TABLE queued_prompts DROP COLUMN retry_at; DROP TABLE IF EXISTS agent_message_receipts; PRAGMA user_version = 7;",
        )
        .unwrap();
    let store = Store::open(&path).unwrap();
    assert_eq!(version(&path), 12);
    let session = store.session(&s).unwrap().unwrap();
    assert_eq!((session.title, session.title_source), (None, None));
    drop(store);

    // Back to the v4 schema, as a build before pull request branches left it; reopening
    // migrates it, and a pull request tracked before has no branch until it updates.
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "ALTER TABLE sessions DROP COLUMN parent_host;
             ALTER TABLE sessions DROP COLUMN title; ALTER TABLE sessions DROP COLUMN title_source;
             ALTER TABLE session_prs DROP COLUMN head_branch; DROP TABLE native_sessions;
             ALTER TABLE queued_prompts DROP COLUMN prompt_id; ALTER TABLE queued_prompts DROP COLUMN agent_message; ALTER TABLE queued_prompts DROP COLUMN retry_at;
             ALTER TABLE queued_prompts DROP COLUMN attachments; DROP TABLE IF EXISTS agent_message_receipts; PRAGMA user_version = 4;",
        )
        .unwrap();
    let mut store = Store::open(&path).unwrap();
    assert_eq!(version(&path), 12);
    let mut untracked = pr(7, PrState::Open);
    untracked.head_branch = None;
    assert_eq!(store.session_prs(&s).unwrap(), [untracked]);
    store
        .append(new_event(
            &s,
            2,
            EventBody::PrUpdated {
                pr: pr(7, PrState::Open),
            },
        ))
        .unwrap();
    assert_eq!(store.session_prs(&s).unwrap(), [pr(7, PrState::Open)]);
    store
        .append(new_event(&s, 3, EventBody::PrUnlinked { number: 7 }))
        .unwrap();
    drop(store);

    // Back to the v2 schema, as a build before session branches left it; reopening migrates
    // it and backfills each session's created branch.
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "ALTER TABLE sessions DROP COLUMN parent_host;
             ALTER TABLE sessions DROP COLUMN title; ALTER TABLE sessions DROP COLUMN title_source;
             ALTER TABLE session_prs DROP COLUMN head_branch;
             DROP TABLE session_branches; DROP TABLE command_results; DROP TABLE queued_prompts;
             DROP TABLE native_sessions; DROP TABLE IF EXISTS agent_message_receipts; PRAGMA user_version = 2;",
        )
        .unwrap();
    let mut store = Store::open(&path).unwrap();
    assert_eq!(version(&path), 12);
    assert_eq!(store.session_branches(&s).unwrap(), ["feature"]);
    store
        .append(new_event(&s, 1, checked_out("spike")))
        .unwrap();
    assert_projections_match_journal(&store, &s);
    drop(store);

    // Back to the v1 schema, as a build before task trees left it; reopening migrates it.
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "ALTER TABLE sessions DROP COLUMN parent_host;
             ALTER TABLE session_prs DROP COLUMN head_branch;
             DROP TABLE session_branches;
             DROP TABLE command_results;
             DROP TABLE queued_prompts;
             DROP TABLE native_sessions;
             DROP INDEX sessions_parent;
             ALTER TABLE sessions DROP COLUMN parent;
             ALTER TABLE sessions DROP COLUMN task;
             ALTER TABLE sessions DROP COLUMN title;
             ALTER TABLE sessions DROP COLUMN title_source;
             DROP TABLE IF EXISTS agent_message_receipts; PRAGMA user_version = 1;",
        )
        .unwrap();
    let mut store = Store::open(&path).unwrap();
    assert_eq!(version(&path), 12);
    assert_eq!(store.session_branches(&s).unwrap(), ["feature"]);
    let session = store.session(&s).unwrap().unwrap();
    assert_eq!((session.parent, session.task), (None, None));
    let child = SessionId::new("s2");
    store
        .append(new_event(&child, 1, child_created(&s, "t")))
        .unwrap();
    assert_eq!(store.children(&s).unwrap().len(), 1);
    store
        .append(new_event(&s, 2, titled("Auth", TitleSource::User)))
        .unwrap();
    let session = store.session(&s).unwrap().unwrap();
    assert_eq!(
        (session.title.as_deref(), session.title_source),
        (Some("Auth"), Some(TitleSource::User))
    );
    drop(store);

    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 13)
        .unwrap();
    assert!(matches!(
        Store::open(&path),
        Err(Error::TooNew {
            found: 13,
            supported: 12
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
        .prop_map(|status| EventBody::SessionStatusChanged {
            status,
            retry_at: None
        }),
        (1u64..4).prop_map(|n| EventBody::PrLinked {
            pr: pr(n, PrState::Open)
        }),
        (1u64..4).prop_map(|n| EventBody::PrUpdated {
            pr: pr(n, PrState::Merged)
        }),
        (1u64..4).prop_map(|number| EventBody::PrUnlinked { number }),
        prop_oneof![Just("feature"), Just("spike"), Just("fix/login")].prop_map(checked_out),
        (
            "[A-Za-z ]{1,20}",
            prop_oneof![Just(TitleSource::Auto), Just(TitleSource::User)]
        )
            .prop_map(|(title, source)| EventBody::TitleChanged { title, source }),
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
        // s0 is a primary session; s1 and s2 are children of its task.
        let sessions: Vec<_> = (0..3).map(|i| SessionId::new(format!("s{i}"))).collect();
        let mut journals: Vec<Vec<Event>> = vec![Vec::new(); 3];
        for (i, s) in sessions.iter().enumerate() {
            let body = match i {
                0 => created(),
                _ => child_created(&sessions[0], &format!("t{i}")),
            };
            journals[i].push(store.append(new_event(s, 0, body)).unwrap());
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
        let children: Vec<_> = store
            .children(&sessions[0])
            .unwrap()
            .into_iter()
            .map(|c| c.session_id)
            .collect();
        prop_assert_eq!(children, &sessions[1..]);
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
        let body = match i % 6 {
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
            4 => checked_out(&format!("b{}", i % 7)),
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

#[test]
fn command_results_survive_a_reopen_and_the_oldest_are_forgotten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herder.db");
    let alice = UserId::new("alice");
    let created = CommandResult::SessionCreated {
        session_id: SessionId::new("s1"),
    };
    {
        let mut store = Store::open(&path).unwrap();
        store
            .record_command_result(&alice, &CommandId::new("c1"), &created)
            .unwrap();
    }
    let mut store = Store::open(&path).unwrap();
    assert_eq!(
        store.command_result(&alice, &CommandId::new("c1")).unwrap(),
        Some(created)
    );
    // Ids are per user.
    let bob = UserId::new("bob");
    assert_eq!(
        store.command_result(&bob, &CommandId::new("c1")).unwrap(),
        None
    );

    for n in 0..COMMAND_RESULTS_KEPT {
        let id = CommandId::new(format!("n{n}"));
        store
            .record_command_result(&bob, &id, &CommandResult::Applied)
            .unwrap();
    }
    assert_eq!(
        store.command_result(&alice, &CommandId::new("c1")).unwrap(),
        None
    );
    assert_eq!(
        store.command_result(&bob, &CommandId::new("n0")).unwrap(),
        Some(CommandResult::Applied)
    );
}

#[test]
fn queued_prompts_survive_a_reopen_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herder.db");
    let (s1, s2) = (SessionId::new("s1"), SessionId::new("s2"));
    let prompt = |by: Option<&str>, text: &str, retry| QueuedPrompt {
        prompt_id: PromptId::new(text),
        agent_message: None,
        by: by.map(UserId::new),
        text: text.into(),
        attachments: Vec::new(),
        retry,
        retry_at: retry.then(|| "2026-10-03T21:20:00Z".parse().unwrap()),
    };
    let with_image = QueuedPrompt {
        agent_message: None,
        attachments: vec![Attachment {
            attachment_id: AttachmentId::new("a1"),
            media_type: "image/png".into(),
            size: 8,
        }],
        ..prompt(Some("bob"), "Like this.", false)
    };
    let queue = vec![
        prompt(Some("alice"), "First.", true),
        prompt(None, "From the primary.", false),
        with_image,
    ];
    {
        let mut store = Store::open(&path).unwrap();
        store.set_queued_prompts(&s1, &queue).unwrap();
        store.set_queued_prompts(&s2, &queue[..1]).unwrap();
    }
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.queued_prompts(&s1).unwrap(), queue);
    assert_eq!(
        store.sessions_with_queued_prompts().unwrap(),
        [s1.clone(), s2.clone()]
    );

    store.set_queued_prompts(&s1, &queue[1..]).unwrap();
    assert_eq!(store.queued_prompts(&s1).unwrap(), queue[1..]);
    store.set_queued_prompts(&s2, &[]).unwrap();
    assert_eq!(
        store.sessions_with_queued_prompts().unwrap(),
        std::slice::from_ref(&s1)
    );
    assert_eq!(
        store.queues().unwrap(),
        std::collections::HashMap::from([(s1, queue[1..].to_vec())])
    );
}

#[test]
fn native_sessions_survive_a_reopen_and_the_latest_wins() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herder.db");
    let (s1, s2) = (SessionId::new("s1"), SessionId::new("s2"));
    let native = |account: &str, id: &str| NativeSession {
        provider: Provider::Claude,
        account_id: AccountId::new(account),
        native_id: id.into(),
    };
    {
        let mut store = Store::open(&path).unwrap();
        assert_eq!(store.native_session(&s1).unwrap(), None);
        store.set_native_session(&s1, &native("work", "a")).unwrap();
        store.set_native_session(&s1, &native("home", "b")).unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(
        store.native_session(&s1).unwrap(),
        Some(native("home", "b"))
    );
    assert_eq!(store.native_session(&s2).unwrap(), None);
}

#[test]
fn v8_queue_migration_preserves_prompts_with_no_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herder.db");
    drop(Store::open(&path).unwrap());
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "ALTER TABLE sessions DROP COLUMN parent_host;
         ALTER TABLE queued_prompts DROP COLUMN prompt_id; ALTER TABLE queued_prompts DROP COLUMN agent_message; ALTER TABLE queued_prompts DROP COLUMN retry_at;
         INSERT INTO queued_prompts (session_id, position, by, text, attachments, retry)
         VALUES ('s1', 0, 'alice', 'continue', '[]', 1);
         DROP TABLE IF EXISTS agent_message_receipts; PRAGMA user_version = 8;",
        )
        .unwrap();
    drop(connection);
    let store = Store::open(&path).unwrap();
    let prompts = store.queued_prompts(&SessionId::new("s1")).unwrap();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].text, "continue");
    assert!(prompts[0].retry);
    assert_eq!(prompts[0].retry_at, None);
    assert_eq!(prompts[0].prompt_id.as_str().len(), 32, "{prompts:?}");
}

#[test]
fn nested_item_ancestry_survives_journal_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("nested.db");
    let session = SessionId::new("nested");
    let child = EventBody::ItemAdded {
        item: Item {
            id: ItemId::new("child-message"),
            turn_id: TurnId::new("turn"),
            agent_message: None,
            parent_call_id: Some(ItemId::new("agent-call")),
            body: ItemBody::AssistantMessage {
                text: "Child output".into(),
            },
        },
    };
    {
        let mut store = Store::open(&path).unwrap();
        store.append(new_event(&session, 0, created())).unwrap();
        store.append(new_event(&session, 1, child.clone())).unwrap();
    }
    let store = Store::open(&path).unwrap();
    let events = store.read_since(&session, 1, usize::MAX).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].body, child);
}

#[test]
fn agent_queue_to_journal_transition_is_atomic_and_preserves_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herder.db");
    let mut store = Store::open(&path).unwrap();
    let session = SessionId::new("receiver");
    store.append(new_event(&session, 0, created())).unwrap();
    let metadata = herder_protocol::AgentMessage {
        sender_session_id: SessionId::new("sender"),
        message_id: "message-1".into(),
        hop_count: 1,
        permission_ceiling: PermissionMode::Ask,
    };
    let queued = QueuedPrompt {
        prompt_id: PromptId::new("p1"),
        agent_message: Some(metadata.clone()),
        by: None,
        text: "Hello".into(),
        attachments: vec![],
        retry: false,
        retry_at: None,
    };
    let mut other = queued.clone();
    other.agent_message.as_mut().unwrap().sender_session_id = SessionId::new("other-sender");
    other.text = "Other payload".into();
    other.prompt_id = PromptId::new("p2");
    store
        .set_queued_prompts(&session, &[queued.clone(), other.clone()])
        .unwrap();
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(
        store.queued_prompts(&session).unwrap(),
        [queued, other.clone()]
    );
    let item = Item {
        agent_message: Some(metadata.clone()),
        parent_call_id: None,
        id: ItemId::new("message"),
        turn_id: TurnId::new("turn"),
        body: ItemBody::UserMessage {
            text: "Hello".into(),
            attachments: vec![],
        },
    };
    store
        .append(new_event(&session, 1, EventBody::ItemAdded { item }))
        .unwrap();
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.queued_prompts(&session).unwrap(), [other]);
    store.set_queued_prompts(&session, &[]).unwrap();
    assert_eq!(
        store
            .agent_message_text(&session, &SessionId::new("other-sender"), "message-1")
            .unwrap()
            .as_deref(),
        Some("Other payload")
    );
    assert_eq!(
        store
            .agent_message_text(&session, &SessionId::new("sender"), "message-1")
            .unwrap()
            .as_deref(),
        Some("Hello")
    );
    let events = store.read_since(&session, 1, 10).unwrap();
    let EventBody::ItemAdded { item } = &events[0].body else {
        panic!("missing prompt")
    };
    assert_eq!(item.agent_message, Some(metadata));
}
