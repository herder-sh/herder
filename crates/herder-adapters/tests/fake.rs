//! Runs the fake adapter end to end through `Box<dyn Adapter>`, as the daemon will.

use std::collections::BTreeMap;
use std::path::PathBuf;

use herder_adapters::fake::FakeAdapter;
use herder_adapters::{Adapter, AdapterCommand, AdapterEvent, AdapterSession, StartRequest};
use herder_protocol::{
    Answer, ApprovalDecision, ApprovalId, ErrorClass, Item, ItemBody, ItemId, PermissionMode,
    QuestionId, TurnError, TurnId, UsageWindow,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/fake")
        .join(name)
}

fn request() -> StartRequest {
    StartRequest {
        config_dir: Some(PathBuf::from("/nonexistent/account")),
        env: BTreeMap::new(),
        cwd: PathBuf::from("/nonexistent/worktree"),
        model: None,
        permission_mode: PermissionMode::Ask,
        seed: Vec::new(),
        resume: None,
        mcp: None,
        launcher: Vec::new(),
    }
}

async fn start(script: PathBuf) -> AdapterSession {
    let adapter: Box<dyn Adapter> = Box::new(FakeAdapter::new(script));
    adapter.start(request()).await.unwrap()
}

/// Reads events up to and including the first one `done` accepts.
async fn until(
    session: &mut AdapterSession,
    done: impl Fn(&AdapterEvent) -> bool,
) -> Vec<AdapterEvent> {
    let mut seen = Vec::new();
    while let Some(event) = session.events.recv().await {
        let stop = done(&event);
        seen.push(event);
        if stop {
            return seen;
        }
    }
    panic!("events closed before the expected event; got {seen:?}");
}

fn turn() -> TurnId {
    TurnId::new("turn-1")
}

fn prompt(text: &str) -> AdapterCommand {
    AdapterCommand::SendPrompt {
        turn_id: turn(),
        text: text.into(),
        images: Vec::new(),
    }
}

fn item(id: &str, body: ItemBody) -> Item {
    Item {
        parent_call_id: None,
        id: ItemId::new(id),
        turn_id: turn(),
        body,
    }
}

fn is_turn_end(event: &AdapterEvent) -> bool {
    matches!(
        event,
        AdapterEvent::TurnCompleted { .. }
            | AdapterEvent::TurnInterrupted { .. }
            | AdapterEvent::TurnFailed { .. }
    )
}

fn clean_exit() -> AdapterEvent {
    AdapterEvent::Exited { error: None }
}

/// Shuts the session down and returns everything it sent until the channel closed.
async fn shutdown(mut session: AdapterSession) -> Vec<AdapterEvent> {
    session.commands.send(AdapterCommand::Shutdown).unwrap();
    let mut rest = Vec::new();
    while let Some(event) = session.events.recv().await {
        rest.push(event);
    }
    rest
}

#[tokio::test]
async fn full_turn_streams_an_assistant_message() {
    let mut session = start(fixture("full_turn.jsonl")).await;
    session.commands.send(prompt("Say hello.")).unwrap();

    let events = until(&mut session, is_turn_end).await;
    let streamed = item("item-1", ItemBody::AssistantMessage { text: "".into() });
    let done = item(
        "item-1",
        ItemBody::AssistantMessage {
            text: "Hello, world.".into(),
        },
    );
    assert_eq!(
        events,
        [
            AdapterEvent::ModelChanged {
                model: "fake-model-1".into()
            },
            AdapterEvent::TurnStarted { turn_id: turn() },
            AdapterEvent::ItemStarted { item: streamed },
            AdapterEvent::ItemDelta {
                item_id: ItemId::new("item-1"),
                text: "Hello".into()
            },
            AdapterEvent::ItemDelta {
                item_id: ItemId::new("item-1"),
                text: ", world.".into()
            },
            AdapterEvent::ItemCompleted { item: done },
            AdapterEvent::UsageReported {
                windows: vec![UsageWindow {
                    window: "five_hour".into(),
                    used_percent: 12.5,
                    resets_at: None,
                }]
            },
            AdapterEvent::TurnCompleted { turn_id: turn() },
        ]
    );
    assert_eq!(shutdown(session).await, [clean_exit()]);
}

#[tokio::test]
async fn approval_blocks_the_turn_until_answered() {
    let mut session = start(fixture("approval.jsonl")).await;
    session.commands.send(prompt("Run the tests.")).unwrap();

    let events = until(&mut session, |event| {
        matches!(event, AdapterEvent::ApprovalRequested { .. })
    })
    .await;
    assert_eq!(
        events.last(),
        Some(&AdapterEvent::ApprovalRequested {
            approval_id: ApprovalId::new("approval-1"),
            turn_id: turn(),
            tool_call_id: ItemId::new("item-1"),
            summary: "Run cargo test".into(),
        })
    );

    // Nothing more arrives while the approval is open.
    let waited =
        tokio::time::timeout(std::time::Duration::from_millis(50), session.events.recv()).await;
    assert!(waited.is_err(), "event before the answer: {waited:?}");

    session
        .commands
        .send(AdapterCommand::AnswerApproval {
            approval_id: ApprovalId::new("approval-1"),
            decision: ApprovalDecision::Allow,
        })
        .unwrap();
    let events = until(&mut session, is_turn_end).await;
    assert_eq!(
        events,
        [
            AdapterEvent::ItemCompleted {
                item: item(
                    "item-2",
                    ItemBody::ToolResult {
                        call_id: ItemId::new("item-1"),
                        output: "test result: ok".into(),
                        is_error: false,
                    }
                )
            },
            AdapterEvent::ItemCompleted {
                item: item(
                    "item-3",
                    ItemBody::AssistantMessage {
                        text: "All tests pass.".into()
                    }
                )
            },
            AdapterEvent::TurnCompleted { turn_id: turn() },
        ]
    );
    assert_eq!(shutdown(session).await, [clean_exit()]);
}

/// Starts `question.jsonl` and reads up to its question.
async fn asked() -> AdapterSession {
    let mut session = start(fixture("question.jsonl")).await;
    session.commands.send(prompt("Add a migration.")).unwrap();
    let events = until(&mut session, |event| {
        matches!(event, AdapterEvent::QuestionAsked { .. })
    })
    .await;
    assert_eq!(
        events,
        [
            AdapterEvent::TurnStarted { turn_id: turn() },
            AdapterEvent::QuestionAsked {
                question_id: QuestionId::new("question-1"),
                turn_id: turn(),
                text: "Which database should the migration target?".into(),
                choices: vec!["SQLite".into(), "Postgres".into()],
            },
        ]
    );
    session
}

#[tokio::test]
async fn question_blocks_the_turn_until_answered() {
    let mut session = asked().await;

    // Nothing more arrives while the question is open.
    let waited =
        tokio::time::timeout(std::time::Duration::from_millis(50), session.events.recv()).await;
    assert!(waited.is_err(), "event before the answer: {waited:?}");

    session
        .commands
        .send(AdapterCommand::AnswerQuestion {
            question_id: QuestionId::new("question-1"),
            answer: Answer::Choice { index: 0 },
        })
        .unwrap();
    let events = until(&mut session, is_turn_end).await;
    assert_eq!(
        events,
        [
            AdapterEvent::ItemCompleted {
                item: item(
                    "item-1",
                    ItemBody::AssistantMessage {
                        text: "Added a SQLite migration.".into()
                    }
                )
            },
            AdapterEvent::TurnCompleted { turn_id: turn() },
        ]
    );
    assert_eq!(shutdown(session).await, [clean_exit()]);
}

#[tokio::test]
async fn a_different_answer_is_a_script_mismatch() {
    let mut session = asked().await;
    session
        .commands
        .send(AdapterCommand::AnswerQuestion {
            question_id: QuestionId::new("question-1"),
            answer: Answer::Text {
                text: "Postgres".into(),
            },
        })
        .unwrap();
    let events = until(&mut session, |_| true).await;
    let [AdapterEvent::Exited { error: Some(error) }] = events.as_slice() else {
        panic!("expected a fatal exit, got {events:?}");
    };
    assert_eq!(error.class, ErrorClass::Fatal);
    assert!(error.message.contains("line 5"), "{}", error.message);
}

#[tokio::test]
async fn limit_reached_fails_the_turn_with_its_class() {
    let mut session = start(fixture("limit_reached.jsonl")).await;
    session
        .commands
        .send(prompt("Refactor the parser."))
        .unwrap();

    let events = until(&mut session, is_turn_end).await;
    assert_eq!(
        events.last(),
        Some(&AdapterEvent::TurnFailed {
            turn_id: turn(),
            error: TurnError {
                class: ErrorClass::LimitReached,
                message: "5-hour limit reached, resets 17:00 UTC".into(),
            },
        })
    );
    assert_eq!(shutdown(session).await, [clean_exit()]);
}

#[tokio::test]
async fn unexpected_command_exits_fatal_naming_the_line() {
    let mut session = start(fixture("approval.jsonl")).await;
    session.commands.send(prompt("Something else.")).unwrap();

    let events = until(&mut session, |_| true).await;
    let [AdapterEvent::Exited { error: Some(error) }] = events.as_slice() else {
        panic!("expected a fatal exit, got {events:?}");
    };
    assert_eq!(error.class, ErrorClass::Fatal);
    assert!(error.message.contains("line 2"), "{}", error.message);
    assert!(session.events.recv().await.is_none());
}

#[tokio::test]
async fn dropping_the_commands_exits_cleanly() {
    let AdapterSession {
        commands,
        mut events,
        ..
    } = start(fixture("limit_reached.jsonl")).await;
    drop(commands);
    assert_eq!(events.recv().await, Some(clean_exit()));
    assert!(events.recv().await.is_none());
}

#[tokio::test]
async fn malformed_script_fails_to_start() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("bad.jsonl");
    std::fs::write(&script, "# comment\n\n{\"emit\": {\"type\": \"nope\"}}\n").unwrap();

    let adapter: Box<dyn Adapter> = Box::new(FakeAdapter::new(script));
    let error = adapter.start(request()).await.unwrap_err();
    assert_eq!(error.class, ErrorClass::Fatal);
    assert!(error.message.contains("bad.jsonl:3"), "{}", error.message);
}
