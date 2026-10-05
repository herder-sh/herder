//! The Claude adapter, replayed against `claude` recordings in `fixtures/claude`.
//!
//! Every fixture but `limit_reached.jsonl` was recorded from the real CLI with
//! `fixtures/claude/record.py`; each test ends with a clean shutdown, which fails if the adapter
//! sent anything the recording did not. The inline fixtures at the end cover what no recording
//! shows: subagent approvals, requests herder does not handle, several questions in one call,
//! free-text and multi-select answers, a dangerous removal in `full_access`, a CLI that
//! dies, and turns the CLI starts on its own for a background task's result.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use herder_adapters::claude;
use herder_adapters::fixture::Fixture;
use herder_adapters::transport::Transport;
use herder_adapters::{AdapterCommand, AdapterEvent, AdapterSession, Capabilities, StartRequest};
use herder_protocol::{
    Answer, ApprovalDecision, ApprovalId, Bytes, ErrorClass, Image, Item, ItemBody, ItemId,
    PermissionMode, QuestionId, Timestamp, TurnError, TurnId, TurnUsage, UsageWindow,
};
use serde_json::json;
use tokio::time::timeout;

const TIMEOUT: Duration = Duration::from_secs(10);

/// The model the recordings ran on.
const HAIKU: &str = "claude-haiku-4-5-20251001";

const FIXTURES: [&str; 9] = [
    "turn",
    "switch",
    "approval",
    "question",
    "question_interrupt",
    "interrupt",
    "seed",
    "limit_reached",
    "full_access",
];

fn fixture(name: &str) -> Fixture {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/claude")
        .join(format!("{name}.jsonl"));
    Fixture::load(path).unwrap()
}

fn request(seed: Vec<Item>) -> StartRequest {
    StartRequest {
        config_dir: Some(PathBuf::from("/nonexistent/account")),
        env: BTreeMap::new(),
        cwd: PathBuf::from("/tmp/herder-claude-fixture"),
        model: None,
        permission_mode: PermissionMode::Ask,
        seed,
        resume: None,
        mcp: None,
        launcher: Vec::new(),
        skills: None,
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
            reports_usage: true,
            native_resume: true,
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
        agent_sender: None,
        turn_id: turn(),
        text: text.into(),
        images: Vec::new(),
    }
}

fn id(n: u32) -> ItemId {
    ItemId::new(format!("item-{n}"))
}

fn item(n: u32, body: ItemBody) -> Item {
    Item {
        agent_message: None,
        follow_up: None,
        parent_call_id: None,
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

/// The `rate_limit_event` of a recorded turn: its five-hour and seven-day use, in percent.
fn usage(five_hour: f64, seven_day: f64) -> AdapterEvent {
    let window = |name: &str, used_percent, resets_at| UsageWindow {
        window: name.into(),
        used_percent,
        resets_at: Some(Timestamp::from_second(resets_at).unwrap()),
    };
    AdapterEvent::UsageReported {
        windows: vec![
            window("five_hour", five_hour, 1790953200),
            window("seven_day", seven_day, 1791529200),
        ],
    }
}

fn identified(native_id: &str) -> AdapterEvent {
    AdapterEvent::SessionIdentified {
        native_id: native_id.into(),
    }
}

fn started() -> AdapterEvent {
    AdapterEvent::TurnStarted { turn_id: turn() }
}

fn completed() -> AdapterEvent {
    AdapterEvent::TurnCompleted {
        turn_id: turn(),
        usage: None,
    }
}

fn completed_with(usage: TurnUsage) -> AdapterEvent {
    AdapterEvent::TurnCompleted {
        turn_id: turn(),
        usage: Some(usage),
    }
}

/// A turn's usage as its `result` line reports it, cost included.
fn reported(input: u64, output: u64, cache_read: u64, cache_write: u64, cost: f64) -> TurnUsage {
    TurnUsage {
        input,
        output,
        cache_read,
        cache_write,
        cost_usd: Some(cost),
        cost_estimated: false,
    }
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
    let mut expected = vec![
        started(),
        identified("499f57af-e4af-4c6c-b348-d47c9b704e70"),
        model(HAIKU),
        usage(47.0, 5.0),
    ];
    expected.extend(streamed(1, &["ok"]));
    expected.push(completed_with(reported(10, 61, 16304, 3114, 0.0093474)));
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
    let mut expected = vec![
        started(),
        identified("4c3c016d-005d-4d44-bff0-a232db1ca4d9"),
        model("claude-sonnet-5-5"),
        usage(47.0, 5.0),
    ];
    expected.extend(streamed(1, &["ok"]));
    expected.push(completed_with(reported(2, 4, 13420, 3565, 0.018162)));
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
            identified("4add687d-6595-4709-9f3f-07e191fc63b8"),
            model(HAIKU),
            usage(47.0, 5.0),
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
    expected.push(completed_with(reported(
        18,
        177,
        35742,
        3336,
        0.012368200000000001,
    )));
    assert_eq!(events, expected);
    shutdown(session).await;
}

fn full_access() -> StartRequest {
    StartRequest {
        permission_mode: PermissionMode::FullAccess,
        ..request(Vec::new())
    }
}

#[tokio::test]
async fn full_access_allows_what_a_safety_check_still_asks() {
    let mut session = start_with(fixture("full_access"), full_access()).await;
    session
        .commands
        .send(prompt(
            "Run these two shell commands with the Bash tool, verbatim, as two separate calls \
             in order, then reply with the word done. First: `cd sub` Second: `cd \
             /tmp/herder-claude-fixture && rm -f sub/*; ls sub`",
        ))
        .unwrap();
    // The recording answers the dangerous rm check's `can_use_tool` with an allow, which the
    // replay expects from the adapter before Claude goes on.
    let events = until(&mut session, is_turn_end).await;
    let approvals: Vec<_> = events
        .iter()
        .filter(|event| matches!(event, AdapterEvent::ApprovalRequested { .. }))
        .collect();
    assert_eq!(approvals, Vec::<&AdapterEvent>::new());
    let tools: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::ItemCompleted { item } => match &item.body {
                ItemBody::ToolCall { input, .. } => Some(input["command"].clone()),
                ItemBody::ToolResult { output, .. } => Some(json!(output)),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        tools,
        [
            json!("cd sub"),
            json!("(Bash completed with no output)"),
            json!("cd /tmp/herder-claude-fixture && rm -f sub/*; ls sub"),
            json!("(Bash completed with no output)"),
        ]
    );
    assert_eq!(
        events.last(),
        Some(&completed_with(reported(
            26,
            492,
            59111,
            630,
            0.009657100000000002
        )))
    );
    shutdown(session).await;
}

const QUESTION_PROMPT: &str = "Use the AskUserQuestion tool to ask me whether to print A or B, \
                               then reply with exactly the letter I chose.";

/// The recorded question: the `AskUserQuestion` call, then its one question.
fn letter_question() -> [AdapterEvent; 2] {
    [
        AdapterEvent::ItemCompleted {
            item: item(
                1,
                ItemBody::ToolCall {
                    name: "AskUserQuestion".into(),
                    input: json!({"questions": [{
                        "question": "Which letter would you like me to print?",
                        "header": "Choice",
                        "options": [
                            {"label": "A", "description": "Print the letter A"},
                            {"label": "B", "description": "Print the letter B"}
                        ],
                        "multiSelect": false
                    }]}),
                },
            ),
        },
        AdapterEvent::QuestionAsked {
            question_id: QuestionId::new("question-1"),
            turn_id: turn(),
            text: "Which letter would you like me to print?\n\n- **A**: Print the letter A\n- \
                   **B**: Print the letter B"
                .into(),
            choices: vec!["A".into(), "B".into()],
        },
    ]
}

#[tokio::test]
async fn a_question_waits_for_its_answer_and_claude_goes_on_with_it() {
    let mut session = start("question").await;
    session.commands.send(prompt(QUESTION_PROMPT)).unwrap();
    let events = until(&mut session, |event| {
        matches!(event, AdapterEvent::QuestionAsked { .. })
    })
    .await;
    let mut expected = vec![
        started(),
        identified("8abc09ed-1328-46b7-980c-fa8e81269db3"),
        model(HAIKU),
        usage(62.0, 7.0),
    ];
    expected.extend(letter_question());
    assert_eq!(events, expected);
    // The replay checks the answer line: the call's input plus `answers`, keyed by question.
    session
        .commands
        .send(AdapterCommand::AnswerQuestion {
            question_id: QuestionId::new("question-1"),
            answer: Answer::Choice { index: 1 },
        })
        .unwrap();
    let events = until(&mut session, is_turn_end).await;
    let mut expected = vec![AdapterEvent::ItemCompleted {
        item: item(
            2,
            ItemBody::ToolResult {
                call_id: id(1),
                output: "Your questions have been answered: \"Which letter would you like me to \
                         print?\"=\"B\". You can now continue with these answers in mind."
                    .into(),
                is_error: false,
            },
        ),
    }];
    expected.extend(streamed(3, &["B"]));
    expected.push(completed_with(reported(18, 198, 35742, 3382, 0.0125502)));
    assert_eq!(events, expected);
    shutdown(session).await;
}

#[tokio::test]
async fn an_interrupt_withdraws_a_pending_question() {
    let mut session = start("question_interrupt").await;
    session.commands.send(prompt(QUESTION_PROMPT)).unwrap();
    let events = until(&mut session, |event| {
        matches!(event, AdapterEvent::QuestionAsked { .. })
    })
    .await;
    assert_eq!(
        events[1],
        identified("59620c3b-5802-4ea2-880d-9a4ffe71a3d7")
    );
    assert_eq!(events[4..], letter_question());
    session.commands.send(AdapterCommand::Interrupt).unwrap();
    let events = until(&mut session, is_turn_end).await;
    assert_eq!(
        events,
        [
            AdapterEvent::ItemCompleted {
                item: item(
                    2,
                    ItemBody::ToolResult {
                        call_id: id(1),
                        output: "The user doesn't want to proceed with this tool use. The tool \
                                 use was rejected (eg. if it was a file edit, the new_string was \
                                 NOT written to the file). STOP what you are doing and wait for \
                                 the user to tell you how to proceed."
                            .into(),
                        is_error: true,
                    },
                ),
            },
            AdapterEvent::TurnInterrupted { turn_id: turn() },
        ]
    );
    // Withdrawn: a late answer sends nothing, which the clean shutdown proves.
    session
        .commands
        .send(AdapterCommand::AnswerQuestion {
            question_id: QuestionId::new("question-1"),
            answer: Answer::Choice { index: 0 },
        })
        .unwrap();
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
            identified("57d42dcd-0361-4083-9ade-63cc697be95a"),
            model(HAIKU),
            usage(47.0, 5.0),
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
            agent_message: None,
            follow_up: None,
            parent_call_id: None,
            id: ItemId::new("old-1"),
            turn_id: TurnId::new("old"),
            body: ItemBody::UserMessage {
                text: "My favourite colour is teal.".into(),
                attachments: Vec::new(),
            },
        },
        Item {
            agent_message: None,
            follow_up: None,
            parent_call_id: None,
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
    // The session id and model were reported while the seed went in.
    let mut expected = vec![
        identified("0bea1056-4c0a-442f-995b-83d901d2624f"),
        model(HAIKU),
        started(),
        usage(47.0, 5.0),
    ];
    expected.extend(streamed(1, &["Teal"]));
    expected.push(completed_with(reported(10, 66, 16304, 3161, 0.0094714)));
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
            identified("499f57af-e4af-4c6c-b348-d47c9b704e70"),
            model(HAIKU),
            usage(40.0, 100.0),
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
async fn a_prompt_with_images_sends_them_as_base64_blocks_before_its_text() {
    let fixture = Fixture::parse(
        "inline",
        r#"{"dir":"in","line":"{\"type\":\"control_request\",\"request_id\":\"herder-1\",\"request\":{\"subtype\":\"initialize\"}}"}
{"dir":"out","line":"{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"herder-1\",\"response\":{}}}"}
{"dir":"in","line":"{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"image\",\"source\":{\"type\":\"base64\",\"media_type\":\"image/png\",\"data\":\"iVBORw0KGgo=\"}},{\"type\":\"image\",\"source\":{\"type\":\"base64\",\"media_type\":\"image/jpeg\",\"data\":\"/9j/\"}},{\"type\":\"text\",\"text\":\"Match these.\"}]},\"parent_tool_use_id\":null,\"session_id\":\"\",\"origin\":{\"kind\":\"human\"}}"}
{"dir":"out","line":"{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"\"}"}
{"dir":"in","eof":true}
{"exit":0}
"#,
    )
    .unwrap();
    let mut session = start_with(fixture, request(Vec::new())).await;
    let image = |media_type: &str, data: &[u8]| Image {
        media_type: media_type.into(),
        data: Bytes(data.to_vec()),
    };
    session
        .commands
        .send(AdapterCommand::SendPrompt {
            agent_sender: None,
            turn_id: turn(),
            text: "Match these.".into(),
            images: vec![
                image("image/png", b"\x89PNG\r\n\x1a\n"),
                image("image/jpeg", b"\xff\xd8\xff"),
            ],
        })
        .unwrap();
    assert_eq!(
        until(&mut session, is_turn_end).await,
        [started(), completed()]
    );
    shutdown(session).await;
}

#[test]
fn claude_takes_images() {
    use herder_adapters::Adapter;
    use herder_adapters::claude::ClaudeAdapter;

    assert!(ClaudeAdapter::default().accepts_images());
}

#[tokio::test]
async fn full_access_still_asks_before_removing_a_critical_path() {
    let fixture = inline(
        r#"{"dir":"out","line":"{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"toolu_rm\",\"name\":\"Bash\",\"input\":{\"command\":\"rm -rf $DIR/\"}}]},\"parent_tool_use_id\":null}"}
{"dir":"out","line":"{\"type\":\"control_request\",\"request_id\":\"r1\",\"request\":{\"subtype\":\"can_use_tool\",\"tool_name\":\"Bash\",\"input\":{\"command\":\"rm -rf $DIR/\"},\"decision_reason\":\"Dangerous rm operation on critical path: /\",\"decision_reason_type\":\"safetyCheck\",\"classifier_approvable\":false,\"tool_use_id\":\"toolu_rm\"}}"}
{"dir":"in","line":"{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"r1\",\"response\":{\"behavior\":\"deny\",\"message\":\"The user denied this tool call.\"}}}"}
{"dir":"out","line":"{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"\"}"}
{"dir":"in","eof":true}
{"exit":0}
"#,
    );
    let mut session = start_with(fixture, full_access()).await;
    session.commands.send(prompt("go")).unwrap();
    let events = until(&mut session, |event| {
        matches!(event, AdapterEvent::ApprovalRequested { .. })
    })
    .await;
    assert_eq!(
        events.last(),
        Some(&AdapterEvent::ApprovalRequested {
            approval_id: ApprovalId::new("approval-1"),
            turn_id: turn(),
            tool_call_id: id(1),
            summary: "Bash: rm -rf $DIR/".into(),
        })
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
async fn several_questions_in_one_call_are_answered_together() {
    let fixture = inline(
        r#"{"dir":"out","line":"{\"type\":\"control_request\",\"request_id\":\"r1\",\"request\":{\"subtype\":\"can_use_tool\",\"tool_name\":\"AskUserQuestion\",\"input\":{\"questions\":[{\"question\":\"Which database?\",\"header\":\"Database\",\"options\":[{\"label\":\"Postgres\",\"description\":\"Relational\"},{\"label\":\"SQLite\"}],\"multiSelect\":false},{\"question\":\"Which extras?\",\"header\":\"Extras\",\"options\":[{\"label\":\"Docs\"},{\"label\":\"Tests\"}],\"multiSelect\":true}]},\"tool_use_id\":\"toolu_1\"}}"}
{"dir":"in","line":"{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"r1\",\"response\":{\"behavior\":\"allow\",\"updatedInput\":{\"answers\":{\"Which database?\":\"Postgres\",\"Which extras?\":\"Docs, Tests\"},\"questions\":[{\"header\":\"Database\",\"multiSelect\":false,\"options\":[{\"description\":\"Relational\",\"label\":\"Postgres\"},{\"label\":\"SQLite\"}],\"question\":\"Which database?\"},{\"header\":\"Extras\",\"multiSelect\":true,\"options\":[{\"label\":\"Docs\"},{\"label\":\"Tests\"}],\"question\":\"Which extras?\"}]}}}}"}
{"dir":"out","line":"{\"type\":\"control_request\",\"request_id\":\"r2\",\"request\":{\"subtype\":\"can_use_tool\",\"tool_name\":\"AskUserQuestion\",\"input\":{\"questions\":[]},\"tool_use_id\":\"toolu_2\"}}"}
{"dir":"in","line":"{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"r2\",\"response\":{\"behavior\":\"deny\",\"message\":\"herder could not read these questions. Ask the user in your reply instead, then end your turn.\"}}}"}
{"dir":"out","line":"{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"\"}"}
{"dir":"in","eof":true}
{"exit":0}
"#,
    );
    let mut session = start_with(fixture, request(Vec::new())).await;
    session.commands.send(prompt("go")).unwrap();
    let asked = |n: u32, text: &str, choices: [&str; 2]| AdapterEvent::QuestionAsked {
        question_id: QuestionId::new(format!("question-{n}")),
        turn_id: turn(),
        text: text.into(),
        choices: choices.map(str::to_owned).to_vec(),
    };
    let events = until(&mut session, |event| {
        matches!(event, AdapterEvent::QuestionAsked { question_id, .. } if question_id.as_str() == "question-2")
    })
    .await;
    assert_eq!(
        events,
        [
            started(),
            asked(
                1,
                "Which database?\n\n- **Postgres**: Relational",
                ["Postgres", "SQLite"]
            ),
            asked(
                2,
                "Which extras?\n\nMore than one may apply: to pick several, answer with their \
                 names separated by commas.",
                ["Docs", "Tests"]
            ),
        ]
    );
    let answer = |n: u32, answer: Answer| AdapterCommand::AnswerQuestion {
        question_id: QuestionId::new(format!("question-{n}")),
        answer,
    };
    // A choice the question does not have is ignored; the call is answered only once every
    // question is, which the replay checks line by line.
    for command in [
        answer(2, Answer::Choice { index: 5 }),
        answer(
            2,
            Answer::Text {
                text: "Docs, Tests".into(),
            },
        ),
        answer(1, Answer::Choice { index: 0 }),
    ] {
        session.commands.send(command).unwrap();
    }
    // The second call has no question to show, so it is refused at once.
    assert_eq!(until(&mut session, is_turn_end).await, [completed()]);
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

#[tokio::test]
async fn read_usage_answers_with_the_plan_windows_and_runs_no_turn() {
    // The answer is trimmed from Claude Code 2.1.286's own; nothing but the two requests is
    // sent, then stdin is closed.
    let fixture = Fixture::parse(
        "inline",
        r#"{"dir":"in","line":"{\"type\":\"control_request\",\"request_id\":\"herder-1\",\"request\":{\"subtype\":\"initialize\"}}"}
{"dir":"out","line":"{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"herder-1\",\"response\":{}}}"}
{"dir":"in","line":"{\"type\":\"control_request\",\"request_id\":\"herder-2\",\"request\":{\"subtype\":\"get_usage\",\"skip_behaviors\":true}}"}
{"dir":"out","line":"{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"herder-2\",\"response\":{\"session\":{\"total_cost_usd\":0},\"rate_limits_available\":true,\"rate_limits\":{\"five_hour\":{\"utilization\":9,\"resets_at\":\"2026-10-02T20:19:59.921522+00:00\"},\"seven_day\":{\"utilization\":2,\"resets_at\":\"2026-10-08T15:59:59.921548+00:00\"},\"seven_day_opus\":null,\"extra_usage\":{\"is_enabled\":true,\"utilization\":null},\"model_scoped\":[{\"display_name\":\"Fable\",\"utilization\":0,\"resets_at\":\"2026-10-08T16:00:00+00:00\"}]},\"behaviors\":null}}}"}
{"dir":"in","eof":true}
{"exit":0}
"#,
    )
    .unwrap();
    let windows = timeout(TIMEOUT, claude::read_usage(Transport::replay(fixture)))
        .await
        .unwrap()
        .unwrap();
    let window = |name: &str, used_percent, resets_at: &str| UsageWindow {
        window: name.into(),
        used_percent,
        resets_at: Some(resets_at.parse().unwrap()),
    };
    assert_eq!(
        windows,
        [
            window("five_hour", 9.0, "2026-10-02T20:19:59.921522Z"),
            window("seven_day", 2.0, "2026-10-08T15:59:59.921548Z"),
            window("seven_day_fable", 0.0, "2026-10-08T16:00:00Z"),
        ]
    );
}

#[tokio::test]
async fn read_usage_of_an_account_without_plan_limits_is_empty() {
    let fixture = Fixture::parse(
        "inline",
        r#"{"dir":"in","line":"{\"type\":\"control_request\",\"request_id\":\"herder-1\",\"request\":{\"subtype\":\"initialize\"}}"}
{"dir":"out","line":"{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"herder-1\",\"response\":{}}}"}
{"dir":"in","line":"{\"type\":\"control_request\",\"request_id\":\"herder-2\",\"request\":{\"subtype\":\"get_usage\",\"skip_behaviors\":true}}"}
{"dir":"out","line":"{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"herder-2\",\"response\":{\"rate_limits_available\":false,\"rate_limits\":null}}}"}
{"dir":"in","eof":true}
{"exit":0}
"#,
    )
    .unwrap();
    let windows = timeout(TIMEOUT, claude::read_usage(Transport::replay(fixture)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(windows, []);
}

#[tokio::test]
async fn nested_transcripts_keep_ancestry_without_interrupting_parent_streams() {
    // Synthetic stream-json fixture: two siblings, a grandchild and interleaved parent text.
    let lines = [
        json!({"type":"assistant","message":{"content":[
            {"type":"tool_use","id":"agent-a","name":"Agent","input":{"prompt":"Investigate"}},
            {"type":"tool_use","id":"agent-b","name":"Task","input":{"prompt":"Review"}}
        ]}}),
        json!({"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"text"}}}),
        json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Parent "}}}),
        json!({"type":"assistant","parent_tool_use_id":"agent-a","message":{"content":[
            {"type":"text","text":"Child A"},
            {"type":"tool_use","id":"read-a","name":"Read","input":{"file_path":"a.rs"}},
            {"type":"tool_use","id":"agent-c","name":"Agent","input":{"prompt":"Explore"}}
        ]}}),
        json!({"type":"assistant","parent_tool_use_id":"agent-b","message":{"content":[{"type":"text","text":"Child B"}]}}),
        json!({"type":"assistant","parent_tool_use_id":"agent-c","message":{"content":[{"type":"text","text":"Grandchild"}]}}),
        json!({"type":"user","parent_tool_use_id":"agent-a","message":{"content":[{"type":"tool_result","tool_use_id":"read-a","content":"permission denied","is_error":true}]}}),
        json!({"type":"user","parent_tool_use_id":"agent-a","message":{"content":[{"type":"tool_result","tool_use_id":"agent-c","content":"Exploration complete"}]}}),
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"agent-a","content":"A complete"},{"type":"tool_result","tool_use_id":"agent-b","content":"B complete"}]}}),
        json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"reply"}}}),
        json!({"type":"assistant","message":{"content":[{"type":"text","text":"Parent reply"}]}}),
        json!({"type":"result","subtype":"success","is_error":false,"result":"Parent reply"}),
    ];
    let mut replay = lines
        .into_iter()
        .map(|line| json!({"dir":"out", "line":line.to_string()}).to_string())
        .collect::<Vec<_>>()
        .join("\n");
    replay.push_str("\n{\"dir\":\"in\",\"eof\":true}\n{\"exit\":0}\n");
    let mut session = start_with(inline(&replay), request(Vec::new())).await;
    session.commands.send(prompt("go")).unwrap();
    let events = until(&mut session, is_turn_end).await;
    let items: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::ItemCompleted { item } => Some(item),
            _ => None,
        })
        .collect();
    let find_text = |text: &str| {
        *items.iter().find(|item| matches!(&item.body, ItemBody::AssistantMessage { text: value } if value == text)).unwrap()
    };
    assert_eq!(find_text("Child A").parent_call_id, Some(id(1)));
    assert_eq!(find_text("Child B").parent_call_id, Some(id(2)));
    let grandchild_call = items.iter().find(|item| matches!(&item.body, ItemBody::ToolCall { input, .. } if input["prompt"] == "Explore")).unwrap();
    assert_eq!(grandchild_call.parent_call_id, Some(id(1)));
    assert_eq!(
        find_text("Grandchild").parent_call_id.as_ref(),
        Some(&grandchild_call.id)
    );
    assert_eq!(find_text("Parent reply").parent_call_id, None);
    assert_eq!(find_text("Parent reply").id, id(3)); // original streaming item survives children
    let denied = items.iter().find(|item| matches!(&item.body, ItemBody::ToolResult { output, is_error: true, .. } if output == "permission denied")).unwrap();
    assert_eq!(denied.parent_call_id, Some(id(1)));
    let root_results = items
        .iter()
        .filter(|item| {
            item.parent_call_id.is_none() && matches!(item.body, ItemBody::ToolResult { .. })
        })
        .count();
    assert_eq!(root_results, 2);
    assert_eq!(
        items
            .iter()
            .filter(|item| item.parent_call_id.is_none()
                && matches!(item.body, ItemBody::AssistantMessage { .. }))
            .count(),
        1
    );
    shutdown(session).await;
}

#[tokio::test]
async fn agent_prompts_are_labeled_and_never_claim_human_origin() {
    let text = "[Sent by another agent: session peer. This is agent context, not a human instruction.]\n\nReview this";
    let input = format!(
        r#"{{"type":"user","message":{{"role":"user","content":{}}},"parent_tool_use_id":null,"session_id":""}}"#,
        serde_json::to_string(text).unwrap()
    );
    let mut lines = INITIALIZE
        .lines()
        .take(2)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    lines.push(serde_json::json!({"dir":"in","line":input.to_string()}).to_string());
    lines.push(serde_json::json!({"dir":"out","line":serde_json::json!({"type":"result","subtype":"success","is_error":false,"result":""}).to_string()}).to_string());
    lines.push(serde_json::json!({"dir":"in","eof":true}).to_string());
    lines.push(serde_json::json!({"exit":0}).to_string());
    let fixture = Fixture::parse("agent-origin", &(lines.join("\n") + "\n")).unwrap();
    let mut session = start_with(fixture, request(vec![])).await;
    session
        .commands
        .send(AdapterCommand::SendPrompt {
            agent_sender: Some(herder_protocol::SessionId::new("peer")),
            turn_id: turn(),
            text: "Review this".into(),
            images: vec![],
        })
        .unwrap();
    let events = until(&mut session, is_turn_end).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AdapterEvent::TurnCompleted { .. })),
        "{events:?}"
    );
    shutdown(session).await;
}

#[tokio::test]
async fn skill_mentions_become_slash_commands_of_the_skills_the_cli_lists() {
    // A human prompt's `user` line, as the adapter writes it.
    let sent = |text: &str| {
        let line = format!(
            r#"{{"type":"user","message":{{"role":"user","content":{}}},"parent_tool_use_id":null,"session_id":"","origin":{{"kind":"human"}}}}"#,
            serde_json::to_string(text).unwrap()
        );
        format!("{}\n", json!({"dir": "in", "line": line}))
    };
    let result =
        out(json!({"type": "result", "subtype": "success", "is_error": false, "result": ""}));
    // `initialize` lists every command; the first turn's `init` then lists the skills alone.
    let initialized = out(json!({"type": "control_response", "response": {
        "subtype": "success",
        "request_id": "herder-1",
        "response": {"commands": [
            {"name": "review", "description": "Review the diff"},
            {"name": "git:commit", "description": "Commit staged changes"},
            {"name": "clear", "description": "Clear the conversation", "builtin": true},
        ]},
    }}));
    let init = out(json!({
        "type": "system",
        "subtype": "init",
        "session_id": "s1",
        "skills": ["review", "git:commit"],
    }));
    let mut text = String::from(INITIALIZE.lines().next().unwrap());
    text.push('\n');
    text.push_str(&initialized);
    for line in [
        sent("Use /review on the diff"),
        init,
        result.clone(),
        sent("Run /review, then /git:commit."),
        result.clone(),
        sent("echo $HOME costs $5, then $clear"),
        result,
    ] {
        text.push_str(&line);
    }
    text.push_str("{\"dir\":\"in\",\"eof\":true}\n{\"exit\":0}\n");
    let fixture = Fixture::parse("skill-mentions", &text).unwrap();
    let mut session = start_with(fixture, request(Vec::new())).await;
    for prompt_text in [
        "Use $review on the diff",
        "Run $review, then $git:commit.",
        // `clear` was a command but is no skill.
        "echo $HOME costs $5, then $clear",
    ] {
        session.commands.send(prompt(prompt_text)).unwrap();
        let events = until(&mut session, is_turn_end).await;
        assert_eq!(events.last(), Some(&completed()), "{prompt_text}");
    }
    shutdown(session).await;
}

// ---- Turns the CLI starts on its own ----

/// The CLI's stdout line `line`, for an inline fixture.
fn out(line: serde_json::Value) -> String {
    format!("{}\n", json!({"dir": "out", "line": line.to_string()}))
}

/// A `<task-notification>` prompt the CLI wrote and replayed, for the task `tool_use_id`
/// started.
fn notification(tool_use_id: &str, status: &str, result: &str) -> String {
    let text = format!(
        "<task-notification>\n<task-id>a1</task-id>\n<tool-use-id>{tool_use_id}</tool-use-id>\n\
         <output-file>/tmp/tasks/a1.output</output-file>\n<status>{status}</status>\n\
         <summary>Agent \"Review\" finished</summary>\n<result>{result}</result>\n\
         <usage><subagent_tokens>11362</subagent_tokens></usage>\n</task-notification>"
    );
    out(json!({
        "type": "user",
        "message": {"role": "user", "content": text},
        "parent_tool_use_id": null,
        "session_id": "s1",
        "uuid": "u1",
        "isReplay": true,
        "origin": {"kind": "task-notification", "producer": "session-task"},
    }))
}

fn reply(text: &str) -> String {
    out(json!({
        "type": "assistant",
        "message": {"content": [{"type": "text", "text": text}]},
        "parent_tool_use_id": null,
    }))
}

fn success() -> String {
    out(json!({"type": "result", "subtype": "success", "is_error": false, "result": ""}))
}

const INITIALIZED: &str = r#"{"dir":"in","line":"{\"type\":\"control_request\",\"request_id\":\"herder-1\",\"request\":{\"subtype\":\"initialize\"}}"}
{"dir":"out","line":"{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"herder-1\",\"response\":{}}}"}
"#;

const STOPPED: &str = r#"{"dir":"in","eof":true}
{"exit":0}
"#;

/// herder's prompt `text` as it is sent.
fn sent(text: &str) -> String {
    format!(
        "{}\n",
        json!({"dir": "in", "line": format!(
            r#"{{"type":"user","message":{{"role":"user","content":"{text}"}},"parent_tool_use_id":null,"session_id":"","origin":{{"kind":"human"}}}}"#
        )})
    )
}

fn item_in(n: u32, turn_id: &TurnId, body: ItemBody) -> AdapterEvent {
    AdapterEvent::ItemCompleted {
        item: Item {
            turn_id: turn_id.clone(),
            ..item(n, body)
        },
    }
}

/// The id of the turn the CLI started, from its `TurnStarted`.
fn unprompted(event: &AdapterEvent) -> TurnId {
    let AdapterEvent::TurnStarted { turn_id } = event else {
        panic!("expected a turn to start, got {event:?}");
    };
    assert_ne!(*turn_id, turn());
    turn_id.clone()
}

#[tokio::test]
async fn a_background_agent_works_between_turns_and_its_result_starts_one() {
    let fixture = Fixture::parse(
        "inline",
        &[
            INITIALIZED.to_owned(),
            sent("Review it in the background."),
            out(json!({
                "type": "assistant",
                "message": {"content": [{
                    "type": "tool_use", "id": "toolu_agent", "name": "Agent",
                    "input": {"description": "Review", "prompt": "Review the diff.", "run_in_background": true},
                }]},
                "parent_tool_use_id": null,
            })),
            out(json!({
                "type": "system", "subtype": "task_started", "task_id": "a1",
                "tool_use_id": "toolu_agent", "description": "Review", "task_type": "local_agent",
                "is_backgrounded": true, "uuid": "u0", "session_id": "s1",
            })),
            out(json!({
                "type": "user",
                "message": {"role": "user", "content": [{
                    "type": "tool_result", "tool_use_id": "toolu_agent",
                    "content": [{"type": "text", "text": "Async agent launched successfully.\nagentId: a1"}],
                }]},
                "parent_tool_use_id": null,
            })),
            reply("Started a review."),
            success(),
            // The agent works on after its turn ended.
            out(json!({
                "type": "assistant",
                "message": {"content": [{
                    "type": "tool_use", "id": "toolu_grep", "name": "Grep", "input": {"pattern": "TODO"},
                }]},
                "parent_tool_use_id": "toolu_agent",
            })),
            out(json!({
                "type": "user",
                "message": {"role": "user", "content": [{
                    "type": "tool_result", "tool_use_id": "toolu_grep", "content": "src/lib.rs",
                }]},
                "parent_tool_use_id": "toolu_agent",
            })),
            // The agent ends while no turn runs: the CLI says so and starts a turn itself.
            out(json!({
                "type": "system", "subtype": "task_notification", "task_id": "a1",
                "tool_use_id": "toolu_agent", "status": "completed",
                "output_file": "/tmp/tasks/a1.output", "summary": "Agent \"Review\" finished",
                "uuid": "u1", "session_id": "s1",
            })),
            notification("toolu_agent", "completed", "Found 2 issues: a &lt; b &amp;&amp; c."),
            out(json!({"type": "system", "subtype": "init", "session_id": "s1"})),
            reply("The review found 2 issues."),
            success(),
            // herder's next prompt, echoed back by `--replay-user-messages`.
            sent("Fix them."),
            out(json!({
                "type": "user",
                "message": {"role": "user", "content": "Fix them."},
                "parent_tool_use_id": null,
                "isReplay": true,
                "origin": {"kind": "human"},
            })),
            reply("Fixed."),
            success(),
            STOPPED.to_owned(),
        ]
        .concat(),
    )
    .unwrap();
    let mut session = start_with(fixture, request(Vec::new())).await;
    session
        .commands
        .send(prompt("Review it in the background."))
        .unwrap();
    let launched = until(&mut session, is_turn_end).await;
    assert_eq!(launched[0], started());
    assert!(
        launched.contains(&AdapterEvent::BackgroundAgents { running: 1 }),
        "{launched:?}"
    );
    assert_eq!(launched.last(), Some(&completed()));

    // The agent's own lines join its call, in the turn that launched it.
    let nested = |n, body| AdapterEvent::ItemCompleted {
        item: Item {
            parent_call_id: Some(id(1)),
            ..item(n, body)
        },
    };
    assert_eq!(
        until(&mut session, |event| matches!(
            event,
            AdapterEvent::ItemCompleted { item } if item.id == id(5)
        ))
        .await,
        [
            nested(
                4,
                ItemBody::ToolCall {
                    name: "Grep".into(),
                    input: json!({"pattern": "TODO"}),
                }
            ),
            nested(
                5,
                ItemBody::ToolResult {
                    call_id: id(4),
                    output: "src/lib.rs".into(),
                    is_error: false,
                }
            ),
        ]
    );

    let own = until(&mut session, is_turn_end).await;
    let turn_id = unprompted(&own[0]);
    assert_eq!(
        own,
        [
            AdapterEvent::TurnStarted {
                turn_id: turn_id.clone()
            },
            // The agent's result answers the call that started it.
            item_in(
                6,
                &turn_id,
                ItemBody::ToolResult {
                    call_id: id(1),
                    output: "Found 2 issues: a < b && c.".into(),
                    is_error: false,
                }
            ),
            // Only now, with its reply's turn open, is the agent no longer counted.
            AdapterEvent::BackgroundAgents { running: 0 },
            item_in(7, &turn_id, message("The review found 2 issues.")),
            AdapterEvent::TurnCompleted {
                turn_id,
                usage: None
            },
        ]
    );

    session.commands.send(prompt("Fix them.")).unwrap();
    assert_eq!(
        until(&mut session, is_turn_end).await,
        [
            started(),
            AdapterEvent::ItemCompleted {
                item: item(8, message("Fixed."))
            },
            completed(),
        ]
    );
    shutdown(session).await;
}

#[tokio::test]
async fn a_background_command_is_counted_apart_from_agents_until_it_ends() {
    let fixture = Fixture::parse(
        "inline",
        &[
            INITIALIZED.to_owned(),
            sent("Build it in the background."),
            out(json!({
                "type": "assistant",
                "message": {"content": [{
                    "type": "tool_use", "id": "toolu_build", "name": "Bash",
                    "input": {"command": "cargo build", "run_in_background": true},
                }]},
                "parent_tool_use_id": null,
            })),
            out(json!({
                "type": "system", "subtype": "task_started", "task_id": "b1",
                "tool_use_id": "toolu_build", "description": "cargo build", "task_type": "local_bash",
                "uuid": "u0", "session_id": "s1",
            })),
            out(json!({
                "type": "user",
                "message": {"role": "user", "content": [{
                    "type": "tool_result", "tool_use_id": "toolu_build",
                    "content": "Command running in background with ID: b1",
                }]},
                "parent_tool_use_id": null,
            })),
            reply("Building."),
            success(),
            notification("toolu_build", "completed", "Finished"),
            out(json!({"type": "system", "subtype": "init", "session_id": "s1"})),
            reply("It built."),
            success(),
            STOPPED.to_owned(),
        ]
        .concat(),
    )
    .unwrap();
    let mut session = start_with(fixture, request(Vec::new())).await;
    session
        .commands
        .send(prompt("Build it in the background."))
        .unwrap();
    let launched = until(&mut session, is_turn_end).await;
    assert!(
        launched.contains(&AdapterEvent::BackgroundCommands { running: 1 }),
        "{launched:?}"
    );
    // A command is no agent: the session does not look busy for it.
    assert!(
        !launched
            .iter()
            .any(|event| matches!(event, AdapterEvent::BackgroundAgents { .. })),
        "{launched:?}"
    );
    assert_eq!(launched.last(), Some(&completed()));

    // Its end starts a turn of the CLI's own, in which it is no longer counted.
    let own = until(&mut session, is_turn_end).await;
    unprompted(&own[0]);
    assert!(
        own.contains(&AdapterEvent::BackgroundCommands { running: 0 }),
        "{own:?}"
    );
    assert!(
        matches!(own.last(), Some(AdapterEvent::TurnCompleted { .. })),
        "{own:?}"
    );
    shutdown(session).await;
}

#[tokio::test]
async fn a_prompt_sent_during_a_turn_the_cli_started_waits_for_its_end() {
    let fixture = Fixture::parse(
        "inline",
        &[
            INITIALIZED.to_owned(),
            // A notification for a call this process never saw adds no item.
            notification("toolu_unknown", "failed", "It broke."),
            out(json!({
                "type": "assistant",
                "message": {"content": [{
                    "type": "tool_use", "id": "toolu_ls", "name": "Bash", "input": {"command": "ls"},
                }]},
                "parent_tool_use_id": null,
            })),
            out(json!({
                "type": "control_request", "request_id": "r1",
                "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "ls"}, "tool_use_id": "toolu_ls"},
            })),
            r#"{"dir":"in","line":"{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"r1\",\"response\":{\"behavior\":\"allow\"}}}"}
"#.to_owned(),
            reply("Checked."),
            success(),
            sent("go"),
            reply("ok"),
            success(),
            STOPPED.to_owned(),
        ]
        .concat(),
    )
    .unwrap();
    let mut session = start_with(fixture, request(Vec::new())).await;
    let asked = until(&mut session, |event| {
        matches!(event, AdapterEvent::ApprovalRequested { .. })
    })
    .await;
    let turn_id = unprompted(&asked[0]);
    // The prompt comes while the CLI's own turn waits for an approval.
    session.commands.send(prompt("go")).unwrap();
    session
        .commands
        .send(AdapterCommand::AnswerApproval {
            approval_id: ApprovalId::new("approval-1"),
            decision: ApprovalDecision::Allow,
        })
        .unwrap();
    let mut events = until(&mut session, is_turn_end).await;
    events.extend(until(&mut session, is_turn_end).await);
    assert_eq!(
        events,
        [
            item_in(2, &turn_id, message("Checked.")),
            AdapterEvent::TurnCompleted {
                turn_id,
                usage: None
            },
            started(),
            AdapterEvent::ItemCompleted {
                item: item(3, message("ok"))
            },
            completed(),
        ]
    );
    shutdown(session).await;
}

#[tokio::test]
async fn an_interrupt_stops_a_turn_the_cli_started() {
    let fixture = Fixture::parse(
        "inline",
        &[
            INITIALIZED.to_owned(),
            notification("toolu_unknown", "completed", "Done."),
            reply("Looking."),
            r#"{"dir":"in","line":"{\"type\":\"control_request\",\"request_id\":\"herder-2\",\"request\":{\"subtype\":\"interrupt\"}}"}
"#.to_owned(),
            out(json!({"type": "result", "subtype": "error_during_execution", "is_error": true})),
            STOPPED.to_owned(),
        ]
        .concat(),
    )
    .unwrap();
    let mut session = start_with(fixture, request(Vec::new())).await;
    let events = until(&mut session, |event| {
        matches!(event, AdapterEvent::ItemCompleted { .. })
    })
    .await;
    let turn_id = unprompted(&events[0]);
    session.commands.send(AdapterCommand::Interrupt).unwrap();
    assert_eq!(
        until(&mut session, is_turn_end).await,
        [AdapterEvent::TurnInterrupted { turn_id }]
    );
    shutdown(session).await;
}

fn result(input: u64, output: u64, total_cost_usd: Option<f64>) -> String {
    let mut result = json!({
        "type": "result", "subtype": "success", "is_error": false, "result": "",
        "usage": {
            "input_tokens": input, "output_tokens": output,
            "cache_read_input_tokens": 1000, "cache_creation_input_tokens": 200,
        },
    });
    if let Some(total) = total_cost_usd {
        result["total_cost_usd"] = json!(total);
    }
    out(result)
}

/// The usage a turn ends with.
fn turn_usage(events: &[AdapterEvent]) -> TurnUsage {
    match events.last() {
        Some(AdapterEvent::TurnCompleted {
            usage: Some(usage), ..
        }) => usage.clone(),
        other => panic!("no usage: {other:?}"),
    }
}

#[tokio::test]
async fn each_turn_costs_what_the_process_spent_during_it_or_the_price_table_s_estimate() {
    let fixture = Fixture::parse(
        "inline",
        &[
            INITIALIZED.to_owned(),
            sent("one"),
            out(json!({"type": "system", "subtype": "init", "model": "claude-haiku-4-5-20251001"})),
            result(10, 20, Some(0.01)),
            sent("two"),
            result(30, 40, Some(0.03)),
            sent("three"),
            result(1_000_000, 100_000, None),
            STOPPED.to_owned(),
        ]
        .concat(),
    )
    .unwrap();
    let mut session = start_with(fixture, request(Vec::new())).await;

    session.commands.send(prompt("one")).unwrap();
    let first = turn_usage(&until(&mut session, is_turn_end).await);
    assert_eq!(
        first,
        TurnUsage {
            input: 10,
            output: 20,
            cache_read: 1000,
            cache_write: 200,
            cost_usd: Some(0.01),
            cost_estimated: false,
        }
    );

    // `total_cost_usd` adds up over the process, so the second turn cost the difference.
    session.commands.send(prompt("two")).unwrap();
    let second = turn_usage(&until(&mut session, is_turn_end).await);
    assert_eq!((second.input, second.output), (30, 40));
    assert!(
        (second.cost_usd.unwrap() - 0.02).abs() < 1e-12,
        "{second:?}"
    );
    assert!(!second.cost_estimated);

    // No cost from the CLI: the haiku price table, $1 in, $5 out, $0.10 cache read, $1.25
    // cache write per million tokens.
    session.commands.send(prompt("three")).unwrap();
    let third = turn_usage(&until(&mut session, is_turn_end).await);
    assert!(third.cost_estimated);
    let estimate = 1.0 + 0.5 + 0.0001 + 0.00025;
    assert!(
        (third.cost_usd.unwrap() - estimate).abs() < 1e-12,
        "{third:?}"
    );
    shutdown(session).await;
}
