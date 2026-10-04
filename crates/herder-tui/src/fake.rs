//! Fake client-core state for tests: machines, sessions and their events as plain values.

use herder_client_core::{ConnectionState, Machine, SessionUpdate};
use herder_protocol::{
    AccountId, CiStatus, Event, EventBody, FleetHost, HostId, Item, ItemBody, ItemId, Mergeable,
    PermissionMode, PrState, Provider, PullRequest, ReviewStatus, Role, SessionHead, SessionId,
    SessionStatus, Timestamp, TurnId,
};

use crate::app::{App, Msg};
use crate::session::SessionKey;

/// A session as its daemon lists it: idle on `claude-main`, top-level.
pub fn head(id: &str, project: Option<&str>) -> SessionHead {
    SessionHead {
        session_id: SessionId::new(id),
        host_id: None,
        head_seq: 0,
        status: SessionStatus::Idle,
        parent: None,
        task: None,
        title: None,
        project_id: project.map(herder_protocol::ProjectId::new),
        account_id: AccountId::new("claude-main"),
        children_need_you: 0,
        queue: Vec::new(),
    }
}

pub fn machine(host: &str, name: &str, sessions: &[&str]) -> Machine {
    Machine {
        host_id: HostId::new(host),
        name: name.to_owned(),
        addresses: vec!["127.0.0.1:7447".to_owned()],
        address: None,
        fingerprint: "ab".repeat(32),
        connection: ConnectionState::Connected,
        quality: Default::default(),
        role: Some(Role::Owner),
        sessions: sessions.iter().map(|id| head(id, None)).collect(),
        hosts: Vec::new(),
        projects: Vec::new(),
        accounts: Vec::new(),
        failover: Default::default(),
        terminals: Vec::new(),
        resources: None,
        session_usage: Default::default(),
        vault: None,
    }
}

pub fn key(host: &str, session: &str) -> SessionKey {
    SessionKey {
        host_id: HostId::new(host),
        session_id: SessionId::new(session),
    }
}

pub fn created(branch: &str, parent: Option<&str>, task: Option<&str>) -> EventBody {
    created_in("/home/ann/src/app", branch, parent, task)
}

pub fn created_in(repo: &str, branch: &str, parent: Option<&str>, task: Option<&str>) -> EventBody {
    EventBody::SessionCreated {
        repo: repo.to_owned(),
        worktree: format!("/home/ann/.herder/worktrees/{branch}"),
        branch: branch.to_owned(),
        provider: Provider::Claude,
        account_id: AccountId::new("claude-main"),
        model: "claude-opus".to_owned(),
        permission_mode: PermissionMode::Ask,
        parent: parent.map(SessionId::new),
        task: task.map(str::to_owned),
        max_children: None,
        failover_pin: None,
    }
}

pub fn status(status: SessionStatus) -> EventBody {
    EventBody::SessionStatusChanged {
        status,
        retry_at: None,
    }
}

pub fn item(id: &str, body: ItemBody) -> Item {
    Item {
        agent_message: None,
        parent_call_id: None,
        id: ItemId::new(id),
        turn_id: TurnId::new("turn-1"),
        body,
    }
}

pub fn added(id: &str, body: ItemBody) -> EventBody {
    EventBody::ItemAdded {
        item: item(id, body),
    }
}

pub fn assistant(text: &str) -> ItemBody {
    ItemBody::AssistantMessage {
        text: text.to_owned(),
    }
}

/// An update carrying `bodies` as events numbered from `first`.
pub fn update(
    session: &str,
    first: u64,
    bodies: Vec<EventBody>,
    streaming: Vec<Item>,
) -> SessionUpdate {
    SessionUpdate {
        events: bodies
            .into_iter()
            .zip(first..)
            .map(|(body, seq)| Event {
                session_id: SessionId::new(session),
                seq,
                at: Timestamp::UNIX_EPOCH,
                by: None,
                body,
            })
            .collect(),
        streaming,
    }
}

/// Feeds `update` for `session` on `host` into the app.
pub fn feed(app: &mut App, host: &str, session: &str, update: SessionUpdate) {
    app.update(Msg::Session {
        key: key(host, session),
        update,
    });
}

/// One machine with a task tree and a lone session, every session loaded:
/// `s1` (idle), `s2` (needs you, a primary) with children `s3` (running) and `s4` (error).
/// The list groups by machine; [`projects`] has the project grouping.
pub fn tree() -> App {
    let mut app = App {
        grouping: crate::projects::Grouping::Machines,
        // Times the screen shows are counted from a clock that stands still.
        clock: Some(Timestamp::UNIX_EPOCH),
        ..App::default()
    };
    app.update(Msg::Machines(vec![machine(
        "h1",
        "box",
        &["s1", "s2", "s3", "s4"],
    )]));
    let sessions = [
        (
            "s1",
            created("herder/fix-login", None, None),
            SessionStatus::Idle,
        ),
        (
            "s2",
            created("herder/api", None, None),
            SessionStatus::NeedsYou,
        ),
        (
            "s3",
            created("herder/api-tests", Some("s2"), Some("write the tests")),
            SessionStatus::Running,
        ),
        (
            "s4",
            created("herder/api-docs", Some("s2"), Some("document it")),
            SessionStatus::Error,
        ),
    ];
    for (id, created, state) in sessions {
        feed(
            &mut app,
            "h1",
            id,
            update(id, 1, vec![created, status(state)], Vec::new()),
        );
    }
    app
}

/// A vault listing two hosts' sessions, every session loaded: `devbox`, online, runs `s1`
/// and `s3`; `laptop`, offline and last heard from 2h 5m ago, runs `s2`. The list groups by
/// machine.
pub fn vault() -> App {
    let mut app = App {
        grouping: crate::projects::Grouping::Machines,
        // Times the screen shows are counted from a clock that stands still.
        clock: Some(Timestamp::UNIX_EPOCH),
        ..App::default()
    };
    let mut vault = machine("v", "vault", &[]);
    let seen = Timestamp::now() - std::time::Duration::from_secs(2 * 3600 + 5 * 60 + 30);
    vault.hosts = vec![
        FleetHost {
            host_id: HostId::new("devbox"),
            host_name: "devbox".into(),
            online: true,
            last_seen: Timestamp::now(),
            usage: None,
        },
        FleetHost {
            host_id: HostId::new("laptop"),
            host_name: "laptop".into(),
            online: false,
            last_seen: seen,
            usage: None,
        },
    ];
    vault.sessions = [("s1", "devbox"), ("s2", "laptop"), ("s3", "devbox")]
        .into_iter()
        .map(|(id, host)| SessionHead {
            host_id: Some(HostId::new(host)),
            ..head(id, Some("github.com/org/app"))
        })
        .collect();
    app.update(Msg::Machines(vec![vault]));
    for (id, branch) in [
        ("s1", "herder/login"),
        ("s2", "herder/docs"),
        ("s3", "herder/api"),
    ] {
        let bodies = vec![created(branch, None, None), status(SessionStatus::Idle)];
        feed(&mut app, "v", id, update(id, 1, bodies, Vec::new()));
    }
    app
}

/// `s2` of [`vault`] after `devbox` took it over and `laptop` came back: the vault lists it
/// on `devbox`, and `laptop`, paired too, lists its own copy `moved`. Both hosts are online.
pub fn moved() -> App {
    let mut app = vault();
    let mut vault = app.machines[0].clone();
    for host in &mut vault.hosts {
        host.online = true;
    }
    vault.sessions = vec![SessionHead {
        host_id: Some(HostId::new("devbox")),
        ..head("s2", Some("github.com/org/app"))
    }];
    let mut laptop = machine("laptop", "laptop", &["s2"]);
    laptop.sessions[0].status = SessionStatus::Moved;
    app.update(Msg::Machines(vec![vault, laptop]));
    let moved = vec![
        created("herder/docs", None, None),
        status(SessionStatus::Moved),
    ];
    feed(&mut app, "laptop", "s2", update("s2", 1, moved, Vec::new()));
    app
}

pub fn started(turn: &str) -> EventBody {
    EventBody::TurnStarted {
        turn_id: TurnId::new(turn),
    }
}

pub fn approval(id: &str, summary: &str) -> EventBody {
    EventBody::ApprovalRequested {
        approval_id: herder_protocol::ApprovalId::new(id),
        turn_id: TurnId::new("turn-1"),
        tool_call_id: ItemId::new("call-1"),
        summary: summary.to_owned(),
        routed_to: herder_protocol::Route::User,
        reason: None,
    }
}

pub fn question(id: &str, text: &str, choices: &[&str]) -> EventBody {
    EventBody::QuestionAsked {
        question_id: herder_protocol::QuestionId::new(id),
        turn_id: TurnId::new("turn-1"),
        text: text.to_owned(),
        choices: choices.iter().map(|c| (*c).to_owned()).collect(),
        routed_to: herder_protocol::Route::User,
        reason: None,
    }
}

/// An account of `host`'s machine, for the new-session dialog.
pub fn account(id: &str, label: &str) -> herder_protocol::Account {
    herder_protocol::Account {
        config_dir: None,
        account_id: AccountId::new(id),
        provider: Provider::Claude,
        label: label.to_owned(),
        usage: Vec::new(),
    }
}

/// Presses each character of `text`.
pub fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        let key = ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Char(c),
            ratatui::crossterm::event::KeyModifiers::NONE,
        );
        app.update(Msg::Key(key));
    }
}

/// Pull request `number` of `acme/app`, with no checks, reviews or mergeability yet.
pub fn pr(number: u64, title: &str, state: PrState) -> PullRequest {
    PullRequest {
        number,
        url: format!("https://github.com/acme/app/pull/{number}"),
        title: title.to_owned(),
        head_branch: Some(format!("fix-{number}")),
        state,
        ci: CiStatus::None,
        review: ReviewStatus::None,
        mergeable: Mergeable::Unknown,
    }
}

/// [`tree`] with PRs: `s2` has #7 (open, passing, approved, clean) and #9 (a draft, checks
/// failing, conflicting); its child `s3` has #8 (merged).
pub fn with_prs() -> App {
    let mut app = tree();
    let mut seven = pr(7, "Add a health endpoint", PrState::Open);
    seven.ci = CiStatus::Passing;
    seven.review = ReviewStatus::Approved;
    seven.mergeable = Mergeable::Clean;
    let mut nine = pr(9, "Document the health endpoint", PrState::Draft);
    nine.ci = CiStatus::Failing;
    nine.review = ReviewStatus::Required;
    nine.mergeable = Mergeable::Conflicting;
    let mut eight = pr(8, "Test the health endpoint", PrState::Merged);
    eight.ci = CiStatus::Passing;
    for (session, pr) in [("s2", seven), ("s2", nine), ("s3", eight)] {
        feed(
            &mut app,
            "h1",
            session,
            update(session, 3, vec![EventBody::PrLinked { pr }], Vec::new()),
        );
    }
    app
}

/// `update` with every event recorded `secs` seconds after the epoch.
pub fn at(mut update: SessionUpdate, secs: i64) -> SessionUpdate {
    for event in &mut update.events {
        event.at = Timestamp::from_second(secs).unwrap();
    }
    update
}

/// An approval or question event put to the primary session first, as a child's are.
pub fn to_primary(mut body: EventBody) -> EventBody {
    match &mut body {
        EventBody::ApprovalRequested { routed_to, .. }
        | EventBody::QuestionAsked { routed_to, .. } => {
            *routed_to = herder_protocol::Route::Primary
        }
        _ => {}
    }
    body
}

/// [`tree`] with a child's question escalated to the user: `s3` asks "Which port?" of its
/// primary at 100 s, and `s2` escalates it with a note at 200 s. `s1` asks the user to approve
/// `cargo publish` at 150 s.
pub fn escalated() -> App {
    let mut app = tree();
    let asked = to_primary(question(
        "q1",
        "Which port should the server use?",
        &["8080", "3000"],
    ));
    feed(
        &mut app,
        "h1",
        "s3",
        at(update("s3", 3, vec![started("turn-1"), asked], vec![]), 100),
    );
    feed(
        &mut app,
        "h1",
        "s1",
        at(
            update(
                "s1",
                3,
                vec![started("turn-1"), approval("a1", "Bash: cargo publish")],
                vec![],
            ),
            150,
        ),
    );
    let escalation = EventBody::QuestionEscalated {
        question_id: herder_protocol::QuestionId::new("q1"),
        reason: herder_protocol::EscalationReason::MarkedByPrimary,
        note: Some("Production uses 8080 behind the proxy; your call.".to_owned()),
    };
    feed(
        &mut app,
        "h1",
        "s3",
        at(
            update(
                "s3",
                5,
                vec![escalation, status(SessionStatus::NeedsYou)],
                vec![],
            ),
            200,
        ),
    );
    app
}

/// Two machines with clones of `github.com/acme/app`, every session loaded, grouped by
/// project. `box` (h1) runs `s1` (idle) and `s3` (needs you) on its clone at
/// `/home/ann/src/app`, and `s2` (running) on `github.com/acme/docs`. `laptop` (h2) runs `s4`
/// (running) with its child `s6` on its clone at `/work/app`, and `s5` (idle) in
/// `/work/scratch`, which has no remote and which its daemon has not resolved yet. Each
/// machine has one account. `s3` has PR #7 and `s4` #12, both of `acme/app`; `s2` has #3.
pub fn projects() -> App {
    let app_id = Some("github.com/acme/app");
    let docs_id = Some("github.com/acme/docs");
    let sessions = [
        (
            "h1",
            "s1",
            app_id,
            "/home/ann/src/app",
            "herder/fix-login",
            None,
            SessionStatus::Idle,
        ),
        (
            "h1",
            "s2",
            docs_id,
            "/home/ann/src/docs",
            "herder/guide",
            None,
            SessionStatus::Running,
        ),
        (
            "h1",
            "s3",
            app_id,
            "/home/ann/src/app",
            "herder/api",
            None,
            SessionStatus::NeedsYou,
        ),
        (
            "h2",
            "s4",
            app_id,
            "/work/app",
            "herder/cache",
            None,
            SessionStatus::Running,
        ),
        (
            "h2",
            "s5",
            None,
            "/work/scratch",
            "herder/try",
            None,
            SessionStatus::Idle,
        ),
        (
            "h2",
            "s6",
            app_id,
            "/work/app",
            "herder/cache-tests",
            Some("s4"),
            SessionStatus::Running,
        ),
    ];
    let mut machines = vec![machine("h1", "box", &[]), machine("h2", "laptop", &[])];
    for machine in &mut machines {
        machine.accounts = vec![account("claude-main", "Main")];
        for (host, id, project, ..) in &sessions {
            if machine.host_id.as_str() == *host {
                machine.sessions.push(head(id, *project));
            }
        }
    }
    let mut app = App {
        clock: Some(Timestamp::UNIX_EPOCH),
        ..App::default()
    };
    app.update(Msg::Machines(machines));
    for (host, id, _, repo, branch, parent, state) in sessions {
        let task = parent.map(|_| "test the cache");
        let created = created_in(repo, branch, parent, task);
        feed(
            &mut app,
            host,
            id,
            update(id, 1, vec![created, status(state)], Vec::new()),
        );
    }
    let mut seven = pr(7, "Add a health endpoint", PrState::Open);
    seven.ci = CiStatus::Passing;
    let mut twelve = pr(12, "Cache the health checks", PrState::Draft);
    twelve.ci = CiStatus::Pending;
    let mut three = pr(3, "Write the health guide", PrState::Open);
    three.url = "https://github.com/acme/docs/pull/3".to_owned();
    for (host, session, pr) in [
        ("h1", "s3", seven),
        ("h2", "s4", twelve),
        ("h1", "s2", three),
    ] {
        feed(
            &mut app,
            host,
            session,
            update(session, 3, vec![EventBody::PrLinked { pr }], Vec::new()),
        );
    }
    app
}

/// Host figures of a busy machine: `running_turns` of 4 turns, with the cap binding at 4.
pub fn host_resources(running_turns: u32) -> herder_protocol::HostResources {
    herder_protocol::HostResources {
        cpu_cores: 8,
        cpu_percent: 42.0,
        load_1m: 3.25,
        memory_total_bytes: 16 << 30,
        memory_available_bytes: 6 << 30,
        pressure: Some(herder_protocol::Pressure {
            cpu_some: 3.0,
            memory_some: 12.0,
            memory_full: 1.0,
            io_some: 0.5,
        }),
        running_turns,
        max_turns: 4,
        waiting_turns: u32::from(running_turns == 4),
        constraint: (running_turns == 4).then_some(herder_protocol::Constraint::MaxTurns),
    }
}

/// A session running 3 processes and a Compose project `app` of two containers.
pub fn session_usage() -> herder_protocol::SessionUsage {
    let container = |name: &str, image: &str, state| herder_protocol::Container {
        id: format!("id-{name}"),
        name: name.to_owned(),
        compose_project: Some("app".to_owned()),
        image: image.to_owned(),
        state,
    };
    herder_protocol::SessionUsage {
        cpu_percent: 12.0,
        memory_bytes: 768 << 20,
        processes: 3,
        containers: vec![
            container(
                "app-db-1",
                "postgres:16",
                herder_protocol::ContainerState::Running,
            ),
            container(
                "app-web-1",
                "node:22",
                herder_protocol::ContainerState::Exited,
            ),
        ],
    }
}

/// Gives machine `h1` of `app` `host` figures, and `s2` the [`session_usage`] if `busy`.
pub fn with_resources(app: &mut App, host: herder_protocol::HostResources, busy: bool) {
    let mut machines = app.machines.clone();
    machines[0].resources = Some(host);
    machines[0].session_usage.clear();
    if busy {
        machines[0]
            .session_usage
            .insert(SessionId::new("s2"), session_usage());
    }
    app.update(Msg::Machines(machines));
}

/// A tool call item `id` of `turn`.
fn call(id: &str, turn: &str, name: &str, input: serde_json::Value) -> EventBody {
    EventBody::ItemAdded {
        item: Item {
            agent_message: None,
            parent_call_id: None,
            id: ItemId::new(id),
            turn_id: TurnId::new(turn),
            body: ItemBody::ToolCall {
                name: name.to_owned(),
                input,
            },
        },
    }
}

/// The result of the tool call `call`.
fn result(call: &str, turn: &str, output: &str) -> EventBody {
    EventBody::ItemAdded {
        item: Item {
            agent_message: None,
            parent_call_id: None,
            id: ItemId::new(format!("{call}-result")),
            turn_id: TurnId::new(turn),
            body: ItemBody::ToolResult {
                call_id: ItemId::new(call),
                output: output.to_owned(),
                is_error: false,
            },
        },
    }
}

/// An item `id` of `turn`.
fn turn_item(id: &str, turn: &str, body: ItemBody) -> EventBody {
    EventBody::ItemAdded {
        item: Item {
            agent_message: None,
            parent_call_id: None,
            id: ItemId::new(id),
            turn_id: TurnId::new(turn),
            body,
        },
    }
}

/// The worktree [`created`] gives `branch`.
fn worktree(branch: &str, path: &str) -> String {
    format!("/home/ann/.herder/worktrees/{branch}/{path}")
}

/// [`tree`] with `s2` open on the conversation of docs/tui-design.md §2.1: a finished turn
/// that read, searched, tested, edited and spawned `s3` for the tests, then a second turn
/// running for 1m 12s with its answer streaming. `claude-main` has used 38% of its five-hour
/// window. The clock stands at 272 s.
pub fn chat() -> App {
    use serde_json::json;
    let mut app = tree();
    let mut machines = app.machines.clone();
    machines[0].accounts = vec![herder_protocol::Account {
        usage: vec![
            herder_protocol::UsageWindow {
                window: "five_hour".to_owned(),
                used_percent: 38.0,
                resets_at: None,
            },
            herder_protocol::UsageWindow {
                window: "seven_day".to_owned(),
                used_percent: 12.0,
                resets_at: None,
            },
        ],
        ..account("claude-main", "claude-main")
    }];
    app.update(Msg::Machines(machines));
    let t1 = "turn-1";
    let steps: Vec<(i64, Vec<EventBody>)> = vec![
        (
            100,
            vec![
                EventBody::TurnStarted {
                    turn_id: TurnId::new(t1),
                },
                turn_item(
                    "u1",
                    t1,
                    ItemBody::UserMessage {
                        text: "Add a health endpoint and test it.".into(),
                        attachments: Vec::new(),
                    },
                ),
            ],
        ),
        (
            104,
            vec![turn_item(
                "r1",
                t1,
                ItemBody::Reasoning {
                    text: "Where the router lives: src/api.rs builds it with Router::new, \
                           so the route goes there.\nThen a test next to the others."
                        .into(),
                },
            )],
        ),
        (
            105,
            vec![
                call(
                    "c1",
                    t1,
                    "Read",
                    json!({"file_path": worktree("herder/api", "src/api.rs")}),
                ),
                result(
                    "c1",
                    t1,
                    "pub fn router() -> Router {\n    Router::new()\n}",
                ),
                call(
                    "c2",
                    t1,
                    "Grep",
                    json!({"pattern": "Router::new", "path": worktree("herder/api", "src")}),
                ),
                result(
                    "c2",
                    t1,
                    "Found 3 files\nsrc/api.rs\nsrc/main.rs\nsrc/test.rs",
                ),
            ],
        ),
        (
            110,
            vec![
                call(
                    "c3",
                    t1,
                    "Bash",
                    json!({"command": "cargo test --workspace"}),
                ),
                result(
                    "c3",
                    t1,
                    "running 12 tests\ntest health::ok ... ok\ntest health::version ... ok\n\
                     test api::router ... ok\ntest api::routes ... ok\ntest api::cors ... ok\n\
                     test db::pool ... ok\ntest db::migrate ... ok\ntest auth::login ... ok\n\
                     test auth::logout ... ok\ntest auth::refresh ... ok\ntest auth::expired ... ok\n\
                     \ntest result: ok. 12 passed; 0 failed",
                ),
            ],
        ),
        (
            115,
            vec![
                call(
                    "c4",
                    t1,
                    "Edit",
                    json!({
                        "file_path": worktree("herder/api", "src/api.rs"),
                        "old_string": "pub fn router() -> Router {\n    Router::new()\n        .route(\"/\", get(index))\n}\n",
                        "new_string": "pub fn router() -> Router {\n    Router::new()\n        .route(\"/\", get(index))\n        .route(\"/health\", get(health))\n}\n\n/// 200 with the build version, for load balancers.\nasync fn health() -> &'static str {\n    env!(\"CARGO_PKG_VERSION\")\n}\n",
                    }),
                ),
                result("c4", t1, "The file src/api.rs has been updated."),
                call(
                    "c5",
                    t1,
                    "TodoWrite",
                    json!({"todos": [
                        {"content": "Add GET /health", "status": "completed"},
                        {"content": "Test it", "status": "completed"},
                        {"content": "Write the docs page", "status": "in_progress"},
                        {"content": "Remove the old target", "status": "pending"},
                    ]}),
                ),
                result("c5", t1, "ok"),
            ],
        ),
        (
            120,
            vec![EventBody::ChildSpawned {
                child_session_id: SessionId::new("s3"),
                task: "write tests".into(),
            }],
        ),
        (
            172,
            vec![
                turn_item(
                    "a1",
                    t1,
                    assistant(
                        "I added `GET /health`; it returns **200** with the build version so \
                         load balancers can probe it.\n\n- one route in `src/api.rs`\n- one \
                         test, in a child session",
                    ),
                ),
                EventBody::TurnCompleted {
                    turn_id: TurnId::new(t1),
                },
            ],
        ),
        (
            200,
            vec![
                EventBody::TurnStarted {
                    turn_id: TurnId::new("turn-2"),
                },
                turn_item(
                    "u2",
                    "turn-2",
                    ItemBody::UserMessage {
                        text: "Good. Now remove the old target directory.".into(),
                        attachments: Vec::new(),
                    },
                ),
            ],
        ),
    ];
    for (seq, (secs, bodies)) in (3..).step_by(10).zip(steps) {
        feed(
            &mut app,
            "h1",
            "s2",
            at(update("s2", seq, bodies, Vec::new()), secs),
        );
    }
    let streaming = vec![Item {
        agent_message: None,
        parent_call_id: None,
        id: ItemId::new("a2"),
        turn_id: TurnId::new("turn-2"),
        body: assistant("Removing the old target next: first I check that nothing"),
    }];
    feed(
        &mut app,
        "h1",
        "s2",
        update("s2", 200, Vec::new(), streaming),
    );
    // The child runs its tests.
    feed(
        &mut app,
        "h1",
        "s3",
        at(
            update(
                "s3",
                3,
                vec![
                    EventBody::TurnStarted {
                        turn_id: TurnId::new("turn-c"),
                    },
                    call(
                        "k1",
                        "turn-c",
                        "Bash",
                        json!({"command": "cargo test -p api health"}),
                    ),
                ],
                Vec::new(),
            ),
            130,
        ),
    );
    app.clock = Some(Timestamp::from_second(272).unwrap());
    app.choose_row(crate::app::Row::Session {
        key: key("h1", "s2"),
        depth: 0,
    });
    app.act(crate::action::Action::Open);
    app.focus = crate::app::Focus::Composer;
    app
}

/// [`chat`] with the second turn asking to run `rm -rf target/`, 12 s ago.
pub fn chat_approval() -> App {
    let mut app = chat();
    let bodies = vec![
        call(
            "c6",
            "turn-2",
            "Bash",
            serde_json::json!({"command": "rm -rf target/"}),
        ),
        EventBody::ApprovalRequested {
            approval_id: herder_protocol::ApprovalId::new("a1"),
            turn_id: TurnId::new("turn-2"),
            tool_call_id: ItemId::new("c6"),
            summary: "$ rm -rf target/".to_owned(),
            routed_to: herder_protocol::Route::User,
            reason: None,
        },
    ];
    feed(
        &mut app,
        "h1",
        "s2",
        at(update("s2", 300, bodies, Vec::new()), 260),
    );
    app
}

/// [`chat`] with the second turn asking which heading level the docs page uses.
pub fn chat_question() -> App {
    let mut app = chat();
    let mut asked = question(
        "q1",
        "Which heading level for the API page?",
        &["h2 under Reference", "h1, its own page"],
    );
    if let EventBody::QuestionAsked { turn_id, .. } = &mut asked {
        *turn_id = TurnId::new("turn-2");
    }
    feed(
        &mut app,
        "h1",
        "s2",
        at(update("s2", 300, vec![asked], Vec::new()), 250),
    );
    app
}

/// A tool result that failed.
fn failed(call: &str, turn: &str, output: &str) -> EventBody {
    let mut body = result(call, turn, output);
    if let EventBody::ItemAdded { item } = &mut body
        && let ItemBody::ToolResult { is_error, .. } = &mut item.body
    {
        *is_error = true;
    }
    body
}

/// A session of [`live`]: id, branch, parent, task, status and first prompt.
type LiveSession = (
    &'static str,
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    SessionStatus,
    &'static str,
);

/// What a real daemon lists, as the owner's screen showed it: sessions with ULID ids on the
/// branches herder makes up from them (`herder/<slug>`), named by their first prompts; one on
/// a branch someone named; a task child; three archived sessions; a long repo path. The open
/// session runs in `full_access`, mid-turn, with timed tool calls, one of them failed, and
/// the last command refused. `claude-main` has used 31% of its session window and 22% of
/// its week; the host is loaded. Grouped by project; the clock stands at 400 s.
pub fn live() -> App {
    use serde_json::json;
    const REPO: &str = "/home/ann/Projects/herder-sh/herder";
    const PROJECT: &str = "github.com/herder-sh/herder";
    let sessions: [LiveSession; 7] = [
        (
            "01JB7Q2M3N4P5R6S7EQ3Z0KAE",
            "herder/eq3z0kae",
            None,
            None,
            SessionStatus::Running,
            "Fix the flaky reconnect test in herder-client-core and explain the root cause",
        ),
        (
            "01JB7Q2M3N4P5R6S77XBBHPH3",
            "herder/7xbbhph3",
            None,
            None,
            SessionStatus::Idle,
            "Restyle the secondary views to the spec",
        ),
        (
            "01JB7Q2M3N4P5R6S7K2WD9TQA",
            "herder/p2d-6-secondary-views",
            None,
            None,
            SessionStatus::NeedsYou,
            "Ship P2d.6",
        ),
        (
            "01JB7Q2M3N4P5R6S7M4FJ8VZC",
            "herder/m4fj8vzc",
            Some("01JB7Q2M3N4P5R6S7EQ3Z0KAE"),
            Some("write the regression test"),
            SessionStatus::Running,
            "Write a regression test for the reconnect race",
        ),
        (
            "01JB7Q2M3N4P5R6S7A1B2C3D4",
            "herder/a1b2c3d4",
            None,
            None,
            SessionStatus::Archived,
            "Bump ratatui",
        ),
        (
            "01JB7Q2M3N4P5R6S7E5F6G7H8",
            "herder/e5f6g7h8",
            None,
            None,
            SessionStatus::Archived,
            "Try a sqlite WAL checkpoint",
        ),
        (
            "01JB7Q2M3N4P5R6S7J9K0M1N2",
            "herder/j9k0m1n2",
            None,
            None,
            SessionStatus::Archived,
            "Draft the pairing docs",
        ),
    ];
    let mut app = App {
        clock: Some(Timestamp::from_second(400).unwrap()),
        ..App::default()
    };
    let mut machine = machine("h1", "box", &[]);
    machine.sessions = sessions
        .iter()
        .map(|(id, _, parent, task, status, _)| SessionHead {
            parent: parent.map(SessionId::new),
            task: task.map(str::to_owned),
            status: *status,
            ..head(id, Some(PROJECT))
        })
        .collect();
    machine.accounts = vec![herder_protocol::Account {
        usage: vec![
            herder_protocol::UsageWindow {
                window: "five_hour".to_owned(),
                used_percent: 31.0,
                resets_at: Some(Timestamp::from_second(400 + 3 * 3600 + 20 * 60).unwrap()),
            },
            herder_protocol::UsageWindow {
                window: "seven_day".to_owned(),
                used_percent: 22.0,
                resets_at: Some(Timestamp::from_second(400 + 4 * 86400).unwrap()),
            },
        ],
        ..account("claude-main", "claude-main")
    }];
    app.update(Msg::Machines(vec![machine]));
    for (id, branch, parent, task, state, prompt) in sessions {
        let mut created = created_in(REPO, branch, parent, task);
        if let EventBody::SessionCreated {
            permission_mode,
            worktree,
            ..
        } = &mut created
        {
            *permission_mode = PermissionMode::FullAccess;
            *worktree = format!(
                "/home/ann/.local/share/herder/worktrees/herder-{}",
                &id[17..]
            );
        }
        let prompt = turn_item(
            &format!("{id}-u"),
            "turn-0",
            ItemBody::UserMessage {
                text: prompt.to_owned(),
                attachments: Vec::new(),
            },
        );
        feed(
            &mut app,
            "h1",
            id,
            at(
                update(id, 1, vec![created, prompt, status(state)], Vec::new()),
                100,
            ),
        );
    }
    let open = "01JB7Q2M3N4P5R6S7EQ3Z0KAE";
    let t = "turn-1";
    let worktree = "/home/ann/.local/share/herder/worktrees/herder-EQ3Z0KAE";
    let test_output: String = (1..=38)
        .map(|n| format!("test client::reconnect::case_{n:02} ... ok\n"))
        .chain(["\ntest result: ok. 38 passed; 0 failed".to_owned()])
        .collect();
    let steps: Vec<(i64, Vec<EventBody>)> = vec![
        (
            200,
            vec![
                EventBody::TurnStarted {
                    turn_id: TurnId::new(t),
                },
                call(
                    "c1",
                    t,
                    "Read",
                    json!({"file_path": format!("{worktree}/crates/herder-client-core/src/connection.rs")}),
                ),
            ],
        ),
        (201, vec![result("c1", t, "pub struct Connection {")]),
        (
            203,
            vec![call(
                "c2",
                t,
                "Grep",
                json!({"pattern": "reconnect", "path": format!("{worktree}/crates")}),
            )],
        ),
        (
            204,
            vec![result(
                "c2",
                t,
                "Found 4 files\nconnection.rs\nclient.rs\nlib.rs\ntests.rs",
            )],
        ),
        (
            206,
            vec![call(
                "c3",
                t,
                "Bash",
                json!({"command": "cargo test -p herder-client-core reconnect"}),
            )],
        ),
        (220, vec![result("c3", t, &test_output)]),
        (
            222,
            vec![call(
                "c4",
                t,
                "Bash",
                json!({"command": "cargo clippy -p herder-client-core -- -D warnings"}),
            )],
        ),
        (
            225,
            vec![failed(
                "c4",
                t,
                "error: unused variable: `backoff`\n  --> crates/herder-client-core/src/connection.rs:212:13\n\nerror: could not compile `herder-client-core`",
            )],
        ),
        (
            230,
            vec![call(
                "c5",
                t,
                "Edit",
                json!({
                    "file_path": format!("{worktree}/crates/herder-client-core/src/connection.rs"),
                    "old_string": "let backoff = self.backoff.next();\nself.retry();\n",
                    "new_string": "let backoff = self.backoff.next();\nself.retry_after(backoff);\n",
                }),
            )],
        ),
        (231, vec![result("c5", t, "The file has been updated.")]),
        (
            240,
            vec![turn_item(
                "a1",
                t,
                assistant(
                    "The test raced the reconnect timer: `retry()` ignored the backoff, so a \
                     second attempt could land before the first one closed.",
                ),
            )],
        ),
    ];
    for (seq, (secs, bodies)) in (10..).step_by(10).zip(steps) {
        feed(
            &mut app,
            "h1",
            open,
            at(update(open, seq, bodies, Vec::new()), secs),
        );
    }
    let host = HostId::new("h1");
    let mut resources = host_resources(2);
    resources.cpu_percent = 44.0;
    resources.memory_available_bytes = resources.memory_total_bytes * 55 / 100;
    if let Some(machine) = app.machines.iter_mut().find(|m| m.host_id == host) {
        machine.resources = Some(resources);
    }
    app.choose_row(crate::app::Row::Session {
        key: key("h1", open),
        depth: 0,
    });
    app.act(crate::action::Action::Open);
    app.focus = crate::app::Focus::Composer;
    app.compose.errors.insert(
        key("h1", open),
        "model opus-9 is not available on claude-main".to_owned(),
    );
    app
}

/// The fleet view of [`backups`] with the vault selected: it backs up `devbox` (images on,
/// a third of its cap), `laptop` (images off) and `old-box`, gone, and its disk is `used`
/// GiB of 480.
pub fn vault_storage(used: f64) -> App {
    use herder_protocol::{HostUsage, VaultVolume};
    let mut app = backups();
    let mut machines = app.machines.clone();
    let seen = Timestamp::now() - std::time::Duration::from_secs(40 * 24 * 3600);
    let host = |name: &str, online: bool, sessions: u32, bytes: u64, cap: Option<u64>| FleetHost {
        host_id: HostId::new(name),
        host_name: name.into(),
        online,
        last_seen: if online { Timestamp::now() } else { seen },
        usage: Some(HostUsage {
            sessions,
            attachment_bytes: bytes,
            attachments_cap: cap,
        }),
    };
    const MIB: u64 = 1 << 20;
    if let Some(vault) = machines.iter_mut().find(|m| m.host_id.as_str() == "v") {
        vault.hosts = vec![
            host("devbox", true, 4, 340 * MIB, Some(1024 * MIB)),
            host("laptop", true, 1, 0, None),
            host("old-box", false, 12, 980 * MIB, Some(1024 * MIB)),
        ];
        vault.sessions = (1..=17)
            .map(|n| SessionHead {
                host_id: Some(HostId::new(match n {
                    ..=4 => "devbox",
                    5 => "laptop",
                    _ => "old-box",
                })),
                ..head(&format!("v{n}"), Some("github.com/org/app"))
            })
            .collect();
    }
    app.update(Msg::Machines(machines));
    // The vault answers the fleet view's question with how full its disk is.
    let volume = VaultVolume {
        total_bytes: 480 << 30,
        // Rounded up, so the tenths shown are those given.
        used_bytes: ((used * 10.0).round() as u64 * (1 << 30)).div_ceil(10),
    };
    app.update(Msg::Sent {
        origin: crate::compose::Origin::Backup(crate::backup::Sent::Ask(HostId::new("v"))),
        result: Ok(herder_protocol::CommandResult::VaultLink {
            is_vault: true,
            vault: None,
            volume: Some(volume),
        }),
    });
    for _ in 0..2 {
        app.update(Msg::Key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Char('j'),
            ratatui::crossterm::event::KeyModifiers::NONE,
        )));
    }
    app
}

/// The fleet view of three machines this client owns, each saying where it backs up: `devbox`
/// nowhere, `laptop` to `vault`, and `vault` is the vault. `devbox` is selected.
pub fn backups() -> App {
    let mut app = App {
        // Times the screen shows are counted from a clock that stands still.
        clock: Some(Timestamp::UNIX_EPOCH),
        ..App::default()
    };
    let at = |host: &str, name: &str, address: &str, digits: &str| Machine {
        addresses: vec![address.to_owned()],
        fingerprint: digits.repeat(16),
        ..machine(host, name, &[])
    };
    let vault = at("v", "vault", "vault.lan:7447", "3f9a");
    let machines = vec![
        at("devbox", "devbox", "devbox.lan:7447", "9c2e"),
        at("laptop", "laptop", "10.0.0.12:7447", "01de"),
        vault.clone(),
    ];
    app.update(Msg::Machines(machines));
    let linked = herder_protocol::LinkedVault {
        address: vault.addresses[0].clone(),
        fingerprint: vault.fingerprint,
    };
    let effects = app.update(Msg::Key(ratatui::crossterm::event::KeyEvent::new(
        ratatui::crossterm::event::KeyCode::Char('m'),
        ratatui::crossterm::event::KeyModifiers::NONE,
    )));
    for effect in effects {
        let crate::app::Effect::Send {
            host_id, origin, ..
        } = effect
        else {
            continue;
        };
        let (is_vault, vault) = match host_id.as_str() {
            "v" => (true, None),
            "laptop" => (false, Some(linked.clone())),
            _ => (false, None),
        };
        let result = Ok(herder_protocol::CommandResult::VaultLink {
            is_vault,
            vault,
            volume: None,
        });
        app.update(Msg::Sent { origin, result });
    }
    app
}
