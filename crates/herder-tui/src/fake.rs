//! Fake client-core state for tests: machines, sessions and their events as plain values.

use herder_client_core::{ConnectionState, Machine, SessionUpdate};
use herder_protocol::{
    AccountId, Event, EventBody, HostId, Item, ItemBody, ItemId, PermissionMode, Provider, Role,
    SessionHead, SessionId, SessionStatus, Timestamp, TurnId,
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
