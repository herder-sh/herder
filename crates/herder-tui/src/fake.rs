//! Fake client-core state for tests: machines, sessions and their events as plain values.

use herder_client_core::{ConnectionState, Machine, SessionUpdate};
use herder_protocol::{
    AccountId, CiStatus, Event, EventBody, HostId, Item, ItemBody, ItemId, Mergeable,
    PermissionMode, PrState, Provider, PullRequest, ReviewStatus, Role, SessionHead, SessionId,
    SessionStatus, Timestamp, TurnId,
};

use crate::app::{App, Msg};
use crate::session::SessionKey;

pub fn machine(host: &str, name: &str, sessions: &[&str]) -> Machine {
    Machine {
        host_id: HostId::new(host),
        name: name.to_owned(),
        addresses: vec!["127.0.0.1:7447".to_owned()],
        fingerprint: "ab".repeat(32),
        connection: ConnectionState::Connected,
        role: Some(Role::Owner),
        sessions: sessions
            .iter()
            .map(|id| SessionHead {
                session_id: SessionId::new(*id),
                head_seq: 0,
            })
            .collect(),
        accounts: Vec::new(),
        terminals: Vec::new(),
    }
}

pub fn key(host: &str, session: &str) -> SessionKey {
    SessionKey {
        host_id: HostId::new(host),
        session_id: SessionId::new(session),
    }
}

pub fn created(branch: &str, parent: Option<&str>, task: Option<&str>) -> EventBody {
    EventBody::SessionCreated {
        repo: "/home/ann/src/app".to_owned(),
        worktree: format!("/home/ann/.herder/worktrees/{branch}"),
        branch: branch.to_owned(),
        provider: Provider::Claude,
        account_id: AccountId::new("claude-main"),
        model: "claude-opus".to_owned(),
        permission_mode: PermissionMode::Ask,
        parent: parent.map(SessionId::new),
        task: task.map(str::to_owned),
    }
}

pub fn status(status: SessionStatus) -> EventBody {
    EventBody::SessionStatusChanged { status }
}

pub fn item(id: &str, body: ItemBody) -> Item {
    Item {
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
pub fn tree() -> App {
    let mut app = App::default();
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
