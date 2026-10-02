//! The Claude adapter, replayed against `claude` recordings in `fixtures/claude`.
//!
//! Every fixture but `limit_reached.jsonl` was recorded from the real CLI with
//! `fixtures/claude/record.py`; each test ends with a clean shutdown, which fails if the adapter
//! sent anything the recording did not. The inline fixtures at the end cover what no recording
//! shows: subagent approvals, requests herder does not handle, and a CLI that dies.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use herder_adapters::claude;
use herder_adapters::fixture::Fixture;
use herder_adapters::transport::Transport;
use herder_adapters::{AdapterCommand, AdapterEvent, AdapterSession, Capabilities, StartRequest};
use herder_protocol::{
    ApprovalDecision, ApprovalId, ErrorClass, Item, ItemBody, ItemId, PermissionMode, TurnError,
    TurnId,
};
use serde_json::json;
use tokio::time::timeout;

const TIMEOUT: Duration = Duration::from_secs(10);

/// The model the recordings ran on.
const HAIKU: &str = "claude-haiku-4-5-20251001";

const FIXTURES: [&str; 7] = [
    "turn",
    "switch",
    "approval",
    "question",
    "interrupt",
    "seed",
    "limit_reached",
];

fn fixture(name: &str) -> Fixture {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/claude")
        .join(format!("{name}.jsonl"));
    Fixture::load(path).unwrap()
}

fn request(seed: Vec<Item>) -> StartRequest {
    StartRequest {
        config_dir: PathBuf::from("/nonexistent/account"),
        env: BTreeMap::new(),
        cwd: PathBuf::from("/tmp/herder-claude-fixture"),
        model: None,
        permission_mode: PermissionMode::Ask,
        seed,
    }
}

async fn start_with(fixture: Fixture, request: StartRequest) -> AdapterSession {
    let session = timeout(TIMEOUT, claude::start(Transport::replay(fixture), request))
        .await
        .expect("start timed out")
        .unwrap();
    assert_eq!(
        session.capabilities,
        Capabilities {
            native_model_switch: true,
            native_permission_mode_switch: true,
            reports_usage: false,
        }
    );
    session
}

async fn start(name: &str) -> AdapterSession {
    start_with(fixture(name), request(Vec::new())).await
}

/// Reads events up to and including the first one `done` accepts.
async fn until(
    session: &mut AdapterSession,
    done: impl Fn(&AdapterEvent) -> bool,
) -> Vec<AdapterEvent> {
    let mut seen = Vec::new();
    loop {
        let event = timeout(TIMEOUT, session.events.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out; got {seen:?}"));
        let Some(event) = event else {
            panic!("events closed before the expected event; got {seen:?}");
        };
        let stop = done(&event);
        seen.push(event);
        if stop {
            return seen;
        }
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

/// Shuts down and asserts the replay matched to the end.
async fn shutdown(mut session: AdapterSession) {
    session.commands.send(AdapterCommand::Shutdown).unwrap();
    let mut rest = Vec::new();
    timeout(TIMEOUT, async {
        while let Some(event) = session.events.recv().await {
            rest.push(event);
        }
    })
    .await
    .expect("events did not close");
    assert_eq!(rest, [AdapterEvent::Exited { error: None }]);
}

fn turn() -> TurnId {
    TurnId::new("turn-1")
}

fn prompt(text: &str) -> AdapterCommand {
    AdapterCommand::SendPrompt {
        turn_id: turn(),
        text: text.into(),
    }
}

fn id(n: u32) -> ItemId {
    ItemId::new(format!("item-{n}"))
}

fn item(n: u32, body: ItemBody) -> Item {
    Item {
        id: id(n),
        turn_id: turn(),
        body,
    }
}

fn message(text: &str) -> ItemBody {
    ItemBody::AssistantMessage { text: text.into() }
}

/// A streamed assistant message: started, its deltas, completed.
fn streamed(n: u32, deltas: &[&str]) -> Vec<AdapterEvent> {
    let mut events = vec![AdapterEvent::ItemStarted {
        item: item(n, message("")),
    }];
    events.extend(deltas.iter().map(|text| AdapterEvent::ItemDelta {
        item_id: id(n),
        text: (*text).into(),
    }));
    events.push(AdapterEvent::ItemCompleted {
        item: item(n, message(&deltas.concat())),
    });
    events
}

fn model(model: &str) -> AdapterEvent {
    AdapterEvent::ModelChanged {
        model: model.into(),
    }
}

fn started() -> AdapterEvent {
    AdapterEvent::TurnStarted { turn_id: turn() }
}

fn completed() -> AdapterEvent {
    AdapterEvent::TurnCompleted { turn_id: turn() }
}

#[test]
fn every_fixture_parses() {
    for name in FIXTURES {
        let header = fixture(name).header.expect("fixture has a header");
        assert_eq!(header.provider, "claude", "{name}");
        assert_eq!(
            header.cli_version.as_deref(),
            Some(claude::CLAUDE_VERSION),
            "{name}"
        );
    }
}

#[tokio::test]
async fn a_turn_streams_its_reply() {
    let mut session = start("turn").await;
    session
        .commands
        .send(prompt("Reply with the word ok."))
        .unwrap();
    let events = until(&mut session, is_turn_end).await;
    // The thinking block came with no text, so it is not shown.
    let mut expected = vec![started(), model(HAIKU)];
    expected.extend(streamed(1, &["ok"]));
    expected.push(completed());
    assert_eq!(events, expected);
    shutdown(session).await;
}

#[tokio::test]
async fn model_and_permission_mode_switch_natively() {
    let mut session = start("switch").await;
    session
        .commands
        .send(AdapterCommand::SetModel {
            model: "sonnet".into(),
        })
        .unwrap();
    session
        .commands
        .send(AdapterCommand::SetPermissionMode {
            mode: PermissionMode::AutoEdit,
        })
        .unwrap();
    let events = until(&mut session, |event| {
        matches!(event, AdapterEvent::PermissionModeChanged { .. })
    })
    .await;
    assert_eq!(
        events,
        [
            model("sonnet"),
            AdapterEvent::PermissionModeChanged {
                mode: PermissionMode::AutoEdit
            },
        ]
    );
    session
        .commands
        .send(prompt("Reply with the word ok."))
        .unwrap();
    let events = until(&mut session, is_turn_end).await;
    // The CLI's own status line repeats the mode, which changes nothing; its init names the
    // model the alias resolved to.
    let mut expected = vec![started(), model("claude-sonnet-5-5")];
    expected.extend(streamed(1, &["ok"]));
    expected.push(completed());
    assert_eq!(events, expected);
    shutdown(session).await;
}

#[tokio::test]
async fn a_tool_call_waits_for_its_approval() {
    let mut session = start("approval").await;
    session
        .commands
        .send(prompt(
            "Run the shell command `touch herder-ok.txt` with the Bash tool, then reply with \
             the word done.",
        ))
        .unwrap();
    let events = until(&mut session, |event| {
        matches!(event, AdapterEvent::ApprovalRequested { .. })
    })
    .await;
    assert_eq!(
        events,
        [
            started(),
            model(HAIKU),
            AdapterEvent::ItemCompleted {
                item: item(
                    1,
                    ItemBody::ToolCall {
                        name: "Bash".into(),
                        input: json!({
                            "command": "touch herder-ok.txt",
                            "description": "Create an empty file named herder-ok.txt"
                        }),
                    }
                ),
            },
            AdapterEvent::ApprovalRequested {
                approval_id: ApprovalId::new("approval-1"),
                turn_id: turn(),
                tool_call_id: id(1),
                summary: "Bash: touch herder-ok.txt".into(),
            },
        ]
    );
    session
        .commands
        .send(AdapterCommand::AnswerApproval {
            approval_id: ApprovalId::new("approval-1"),
            decision: ApprovalDecision::Allow,
        })
        .unwrap();
    let events = until(&mut session, is_turn_end).await;
    let mut expected = vec![AdapterEvent::ItemCompleted {
        item: item(
            2,
            ItemBody::ToolResult {
                call_id: id(1),
                output: "(Bash completed with no output)".into(),
                is_error: false,
            },
        ),
    }];
    expected.extend(streamed(3, &["done"]));
    expected.push(completed());
    assert_eq!(events, expected);
    shutdown(session).await;
}

#[tokio::test]
async fn questions_are_refused_until_the_contract_has_them() {
    let mut session = start("question").await;
    session
        .commands
        .send(prompt(
            "Use the AskUserQuestion tool to ask me whether I prefer tea or coffee. If you \
             cannot, reply with the word skipped.",
        ))
        .unwrap();
    let events = until(&mut session, is_turn_end).await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AdapterEvent::ApprovalRequested { .. })),
        "{events:?}"
    );
    let AdapterEvent::ItemCompleted { item: call } = &events[2] else {
        panic!("expected the question's tool call, got {events:?}");
    };
    assert!(
        matches!(&call.body, ItemBody::ToolCall { name, .. } if name == "AskUserQuestion"),
        "{call:?}"
    );
    let mut expected = vec![AdapterEvent::ItemCompleted {
        item: item(
            2,
            ItemBody::ToolResult {
                call_id: id(1),
                output: "herder cannot show questions yet. Ask the user in your reply instead, \
                         then end your turn."
                    .into(),
                is_error: true,
            },
        ),
    }];
    expected.extend(streamed(3, &["sk", "ipped"]));
    expected.push(completed());
    assert_eq!(events[3..], expected);
    shutdown(session).await;
}

#[tokio::test]
async fn an_interrupt_ends_the_turn_with_what_streamed() {
    let mut session = start("interrupt").await;
    session
        .commands
        .send(prompt("Count from 1 to 300, one number per line."))
        .unwrap();
    let events = until(&mut session, |event| {
        matches!(event, AdapterEvent::ItemDelta { .. })
    })
    .await;
    assert_eq!(
        events,
        [
            started(),
            model(HAIKU),
            AdapterEvent::ItemStarted {
                item: item(1, message(""))
            },
            AdapterEvent::ItemDelta {
                item_id: id(1),
                text: "1".into()
            },
        ]
    );
    session.commands.send(AdapterCommand::Interrupt).unwrap();
    let events = until(&mut session, is_turn_end).await;
    assert_eq!(
        events,
        [
            AdapterEvent::ItemDelta {
                item_id: id(1),
                text: "\n2\n3\n4".into()
            },
            AdapterEvent::ItemCompleted {
                item: item(1, message("1\n2\n3\n4"))
            },
            AdapterEvent::TurnInterrupted { turn_id: turn() },
        ]
    );
    shutdown(session).await;
}

#[tokio::test]
async fn a_seed_becomes_context_before_the_first_prompt() {
    let seed = vec![
        Item {
            id: ItemId::new("old-1"),
            turn_id: TurnId::new("old"),
            body: ItemBody::UserMessage {
                text: "My favourite colour is teal.".into(),
            },
        },
        Item {
            id: ItemId::new("old-2"),
            turn_id: TurnId::new("old"),
            body: message("Noted."),
        },
    ];
    let mut session = start_with(fixture("seed"), request(seed)).await;
    session
        .commands
        .send(prompt("What is my favourite colour? Reply with one word."))
        .unwrap();
    let events = until(&mut session, is_turn_end).await;
    // The model was reported while the seed went in.
    let mut expected = vec![model(HAIKU), started()];
    expected.extend(streamed(1, &["Teal"]));
    expected.push(completed());
    assert_eq!(events, expected);
    shutdown(session).await;
}

#[tokio::test]
async fn a_spent_limit_fails_the_turn_with_limit_reached() {
    let mut session = start("limit_reached").await;
    session
        .commands
        .send(prompt("Reply with the word ok."))
        .unwrap();
    let events = until(&mut session, is_turn_end).await;
    assert_eq!(
        events,
        [
            started(),
            model(HAIKU),
            AdapterEvent::TurnFailed {
                turn_id: turn(),
                error: TurnError {
                    class: ErrorClass::LimitReached,
                    message: "You've hit your weekly limit · resets Oct 5 at 4pm (UTC)".into(),
                },
            },
        ]
    );
    shutdown(session).await;
}

// ---- Inline fixtures ----

const INITIALIZE: &str = r#"{"dir":"in","line":"{\"type\":\"control_request\",\"request_id\":\"herder-1\",\"request\":{\"subtype\":\"initialize\"}}"}
{"dir":"out","line":"{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"herder-1\",\"response\":{}}}"}
{"dir":"in","line":"{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"go\"},\"parent_tool_use_id\":null,\"session_id\":\"\",\"origin\":{\"kind\":\"human\"}}"}
"#;

fn inline(rest: &str) -> Fixture {
    Fixture::parse("inline", &format!("{INITIALIZE}{rest}")).unwrap()
}

#[tokio::test]
async fn a_subagent_approval_first_shows_its_tool_call() {
    let fixture = inline(
        r#"{"dir":"out","line":"{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"toolu_sub\",\"name\":\"Write\",\"input\":{}}]},\"parent_tool_use_id\":\"toolu_task\"}"}
{"dir":"out","line":"{\"type\":\"control_request\",\"request_id\":\"r1\",\"request\":{\"subtype\":\"can_use_tool\",\"tool_name\":\"Write\",\"input\":{\"file_path\":\"/w/a.txt\"},\"tool_use_id\":\"toolu_sub\",\"agent_id\":\"a1\"}}"}
{"dir":"in","line":"{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"r1\",\"response\":{\"behavior\":\"deny\",\"message\":\"The user denied this tool call.\"}}}"}
{"dir":"out","line":"{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"\"}"}
{"dir":"in","eof":true}
{"exit":0}
"#,
    );
    let mut session = start_with(fixture, request(Vec::new())).await;
    session.commands.send(prompt("go")).unwrap();
    let events = until(&mut session, |event| {
        matches!(event, AdapterEvent::ApprovalRequested { .. })
    })
    .await;
    assert_eq!(
        events,
        [
            started(),
            AdapterEvent::ItemCompleted {
                item: item(
                    1,
                    ItemBody::ToolCall {
                        name: "Write".into(),
                        input: json!({"file_path": "/w/a.txt"}),
                    }
                ),
            },
            AdapterEvent::ApprovalRequested {
                approval_id: ApprovalId::new("approval-1"),
                turn_id: turn(),
                tool_call_id: id(1),
                summary: "Write: /w/a.txt".into(),
            },
        ]
    );
    session
        .commands
        .send(AdapterCommand::AnswerApproval {
            approval_id: ApprovalId::new("approval-1"),
            decision: ApprovalDecision::Deny,
        })
        .unwrap();
    assert_eq!(until(&mut session, is_turn_end).await, [completed()]);
    shutdown(session).await;
}

#[tokio::test]
async fn unknown_requests_are_refused_and_cancelled_approvals_dropped() {
    let fixture = inline(
        r#"{"dir":"out","line":"{\"type\":\"control_request\",\"request_id\":\"r1\",\"request\":{\"subtype\":\"elicitation\",\"mcp_server_name\":\"x\"}}"}
{"dir":"in","line":"{\"type\":\"control_response\",\"response\":{\"subtype\":\"error\",\"request_id\":\"r1\",\"error\":\"herder does not handle this request\"}}"}
{"dir":"out","line":"{\"type\":\"control_request\",\"request_id\":\"r2\",\"request\":{\"subtype\":\"can_use_tool\",\"tool_name\":\"Bash\",\"input\":{\"command\":\"ls\"},\"tool_use_id\":\"toolu_1\"}}"}
{"dir":"out","line":"{\"type\":\"control_cancel_request\",\"request_id\":\"r2\"}"}
{"dir":"in","line":"{\"type\":\"control_request\",\"request_id\":\"herder-2\",\"request\":{\"subtype\":\"interrupt\"}}"}
{"dir":"out","line":"{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true}"}
{"dir":"in","eof":true}
{"exit":0}
"#,
    );
    let mut session = start_with(fixture, request(Vec::new())).await;
    session.commands.send(prompt("go")).unwrap();
    let events = until(&mut session, |event| {
        matches!(event, AdapterEvent::ApprovalRequested { .. })
    })
    .await;
    assert_eq!(events.len(), 3, "{events:?}");
    // Withdrawn by the CLI: answering it sends nothing, so the next line out is the interrupt
    // the replay expects.
    session
        .commands
        .send(AdapterCommand::AnswerApproval {
            approval_id: ApprovalId::new("approval-1"),
            decision: ApprovalDecision::Allow,
        })
        .unwrap();
    session.commands.send(AdapterCommand::Interrupt).unwrap();
    assert_eq!(
        until(&mut session, is_turn_end).await,
        [AdapterEvent::TurnInterrupted { turn_id: turn() }]
    );
    shutdown(session).await;
}

#[tokio::test]
async fn a_cli_that_dies_fails_the_open_turn_then_exits() {
    let fixture = inline(
        r#"{"dir":"out","line":"not json"}
{"exit":3}
"#,
    );
    let mut session = start_with(fixture, request(Vec::new())).await;
    session.commands.send(prompt("go")).unwrap();
    let mut events = Vec::new();
    timeout(TIMEOUT, async {
        while let Some(event) = session.events.recv().await {
            events.push(event);
        }
    })
    .await
    .expect("events did not close");
    let error = TurnError {
        class: ErrorClass::Fatal,
        message: "claude exited with code 3".into(),
    };
    assert_eq!(
        events,
        [
            started(),
            AdapterEvent::TurnFailed {
                turn_id: turn(),
                error: error.clone(),
            },
            AdapterEvent::Exited { error: Some(error) },
        ]
    );
}

#[tokio::test]
async fn a_cli_that_dies_before_initialize_answers_fails_start() {
    let fixture = Fixture::parse(
        "inline",
        r#"{"dir":"in","line":"{\"type\":\"control_request\",\"request_id\":\"herder-1\",\"request\":{\"subtype\":\"initialize\"}}"}
{"exit":1}
"#,
    )
    .unwrap();
    let error = timeout(
        TIMEOUT,
        claude::start(Transport::replay(fixture), request(Vec::new())),
    )
    .await
    .expect("start timed out")
    .unwrap_err();
    assert_eq!(
        error,
        TurnError {
            class: ErrorClass::Fatal,
            message: "claude stopped while starting: claude exited with code 1".into(),
        }
    );
}
