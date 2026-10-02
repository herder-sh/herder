//! The Codex adapter, replayed against `codex app-server` recordings in `fixtures/codex`.
//!
//! Every fixture but `limit_reached.jsonl` was recorded from the real CLI with
//! `fixtures/codex/record.py`; each test ends with a clean shutdown, which fails if the adapter
//! sent anything the recording did not.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use herder_adapters::codex;
use herder_adapters::fixture::{Fixture, Record};
use herder_adapters::transport::Transport;
use herder_adapters::{AdapterCommand, AdapterEvent, AdapterSession, StartRequest};
use herder_protocol::{
    ApprovalDecision, ApprovalId, ErrorClass, Item, ItemBody, ItemId, PermissionMode, Timestamp,
    TurnError, TurnId, UsageWindow,
};
use serde_json::{Value, json};
use tokio::time::timeout;

const TIMEOUT: Duration = Duration::from_secs(10);

/// The worktree the fixtures were recorded in; replay never touches it.
const CWD: &str = "/tmp/herder-codex-fixture";

const FIXTURES: [&str; 5] = ["turn", "approval", "interrupt", "limit_reached", "seed"];

fn path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/codex")
        .join(format!("{name}.jsonl"))
}

fn fixture(name: &str) -> Fixture {
    Fixture::load(path(name)).unwrap()
}

fn request(seed: Vec<Item>) -> StartRequest {
    StartRequest {
        config_dir: Some(PathBuf::from("/nonexistent/account")),
        env: BTreeMap::new(),
        cwd: PathBuf::from(CWD),
        model: None,
        permission_mode: PermissionMode::Ask,
        seed,
        mcp: None,
        launcher: Vec::new(),
    }
}

async fn start_with(fixture: Fixture, request: StartRequest) -> AdapterSession {
    timeout(TIMEOUT, codex::start(Transport::replay(fixture), request))
        .await
        .expect("start timed out")
        .unwrap()
}

/// Starts on `name` and checks the startup events every recording shares.
async fn start(name: &str) -> AdapterSession {
    let mut session = start_with(fixture(name), request(Vec::new())).await;
    let events = until(&mut session, |event| {
        matches!(event, AdapterEvent::ModelChanged { .. })
    })
    .await;
    assert_eq!(
        events,
        [
            AdapterEvent::UsageReported {
                windows: vec![weekly(25.0)]
            },
            AdapterEvent::ModelChanged {
                model: "gpt-6.1-sol".into()
            },
        ]
    );
    session
}

fn weekly(used_percent: f64) -> UsageWindow {
    UsageWindow {
        window: "weekly".into(),
        used_percent,
        resets_at: Some(Timestamp::from_second(1791052121).unwrap()),
    }
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

fn item(id: &str, body: ItemBody) -> Item {
    Item {
        id: ItemId::new(id),
        turn_id: turn(),
        body,
    }
}

fn assistant(text: &str) -> ItemBody {
    ItemBody::AssistantMessage { text: text.into() }
}

/// The text of every `ItemDelta` for `id`, joined.
fn streamed(events: &[AdapterEvent], id: &str) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::ItemDelta { item_id, text } if item_id.as_str() == id => {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn turn_streams_the_answer_on_the_switched_model_and_mode() {
    let mut session = start("turn").await;
    for command in [
        AdapterCommand::SetModel {
            model: "gpt-6-luna".into(),
        },
        AdapterCommand::SetPermissionMode {
            mode: PermissionMode::ReadOnly,
        },
        prompt("Reply with the word ok."),
    ] {
        session.commands.send(command).unwrap();
    }

    let events = until(&mut session, is_turn_end).await;
    assert_eq!(
        events,
        [
            AdapterEvent::ModelChanged {
                model: "gpt-6-luna".into()
            },
            AdapterEvent::PermissionModeChanged {
                mode: PermissionMode::ReadOnly
            },
            AdapterEvent::TurnStarted { turn_id: turn() },
            AdapterEvent::ItemStarted {
                item: item("item-1", assistant(""))
            },
            AdapterEvent::ItemDelta {
                item_id: ItemId::new("item-1"),
                text: "ok".into()
            },
            AdapterEvent::ItemCompleted {
                item: item("item-1", assistant("ok"))
            },
            AdapterEvent::UsageReported {
                windows: vec![weekly(25.0)]
            },
            AdapterEvent::TurnCompleted { turn_id: turn() },
        ]
    );
    shutdown(session).await;
}

const TOUCH: &str = "/usr/bin/bash -lc 'touch herder-ok.txt'";

async fn approval_requested(session: &mut AdapterSession) -> Vec<AdapterEvent> {
    session
        .commands
        .send(prompt("Run the shell command: touch herder-ok.txt"))
        .unwrap();
    until(session, |event| {
        matches!(event, AdapterEvent::ApprovalRequested { .. })
    })
    .await
}

#[tokio::test]
async fn approval_blocks_the_command_until_allowed() {
    let mut session = start("approval").await;
    let events = approval_requested(&mut session).await;
    let [
        AdapterEvent::TurnStarted { .. },
        AdapterEvent::ItemStarted { .. },
        ..,
        AdapterEvent::ItemCompleted { item: commentary },
        AdapterEvent::ItemCompleted { item: call },
        AdapterEvent::ApprovalRequested {
            approval_id,
            turn_id,
            tool_call_id,
            summary,
        },
    ] = events.as_slice()
    else {
        panic!("unexpected events {events:?}");
    };
    assert_eq!(commentary.id, ItemId::new("item-1"));
    assert_eq!(
        commentary.body,
        assistant(&streamed(&events, "item-1")),
        "the completed text is the streamed text"
    );
    assert_eq!(
        call,
        &item(
            "item-2",
            ItemBody::ToolCall {
                name: "shell".into(),
                input: json!({"command": TOUCH, "cwd": CWD}),
            }
        )
    );
    assert_eq!(approval_id, &ApprovalId::new("approval-1"));
    assert_eq!(turn_id, &turn());
    assert_eq!(tool_call_id, &call.id);
    assert_eq!(summary, &format!("Run {TOUCH}"));

    // Nothing more arrives while the approval is open.
    let waited = timeout(Duration::from_millis(50), session.events.recv()).await;
    assert!(waited.is_err(), "event before the answer: {waited:?}");

    session
        .commands
        .send(AdapterCommand::AnswerApproval {
            approval_id: approval_id.clone(),
            decision: ApprovalDecision::Allow,
        })
        .unwrap();
    let events = until(&mut session, is_turn_end).await;
    assert_eq!(
        events[..2],
        [
            AdapterEvent::ItemCompleted {
                item: item(
                    "item-3",
                    ItemBody::ToolResult {
                        call_id: ItemId::new("item-2"),
                        output: String::new(),
                        is_error: false,
                    }
                )
            },
            AdapterEvent::UsageReported {
                windows: vec![weekly(25.0)]
            },
        ]
    );
    assert_eq!(
        events[events.len() - 3..],
        [
            AdapterEvent::ItemCompleted {
                item: item(
                    "item-4",
                    assistant("Ran `touch herder-ok.txt` successfully.")
                )
            },
            AdapterEvent::UsageReported {
                windows: vec![weekly(25.0)]
            },
            AdapterEvent::TurnCompleted { turn_id: turn() },
        ]
    );
    shutdown(session).await;
}

#[tokio::test]
async fn denying_an_approval_declines_it() {
    // The recording, answered the other way; what Codex says next does not matter here.
    let text = std::fs::read_to_string(path("approval"))
        .unwrap()
        .replace(r#"\"decision\":\"accept\""#, r#"\"decision\":\"decline\""#);
    let fixture = Fixture::parse("approval-declined", &text).unwrap();
    let mut session = start_with(fixture, request(Vec::new())).await;
    let events = approval_requested(&mut session).await;
    let Some(AdapterEvent::ApprovalRequested { approval_id, .. }) = events.last() else {
        unreachable!();
    };
    session
        .commands
        .send(AdapterCommand::AnswerApproval {
            approval_id: approval_id.clone(),
            decision: ApprovalDecision::Deny,
        })
        .unwrap();
    until(&mut session, is_turn_end).await;
    shutdown(session).await;
}

#[tokio::test]
async fn interrupt_stops_the_turn_and_keeps_the_partial_answer() {
    let mut session = start("interrupt").await;
    session
        .commands
        .send(prompt("Count from 1 to 300, one number per line."))
        .unwrap();
    let mut events = until(&mut session, |event| {
        matches!(event, AdapterEvent::ItemDelta { .. })
    })
    .await;
    session.commands.send(AdapterCommand::Interrupt).unwrap();
    events.extend(until(&mut session, is_turn_end).await);

    let partial = streamed(&events, "item-1");
    assert!(partial.starts_with('1'), "{partial:?}");
    assert_eq!(
        events[events.len() - 2..],
        [
            AdapterEvent::ItemCompleted {
                item: item("item-1", assistant(&partial))
            },
            AdapterEvent::TurnInterrupted { turn_id: turn() },
        ]
    );
    shutdown(session).await;
}

#[tokio::test]
async fn usage_limit_fails_the_turn_with_limit_reached() {
    let mut session = start("limit_reached").await;
    session
        .commands
        .send(prompt("Reply with the word ok."))
        .unwrap();
    let events = until(&mut session, is_turn_end).await;
    assert_eq!(
        events,
        [
            AdapterEvent::TurnStarted { turn_id: turn() },
            AdapterEvent::UsageReported {
                windows: vec![
                    UsageWindow {
                        window: "five_hour".into(),
                        used_percent: 100.0,
                        resets_at: Some(Timestamp::from_second(1790960400).unwrap()),
                    },
                    weekly(41.0),
                ]
            },
            AdapterEvent::TurnFailed {
                turn_id: turn(),
                error: TurnError {
                    class: ErrorClass::LimitReached,
                    message: "You've hit your usage limit. Upgrade to Pro \
                              (https://chatgpt.com/explore/pro), visit \
                              https://chatgpt.com/codex/settings/usage to purchase more \
                              credits or try again at 5:00 PM."
                        .into(),
                },
            },
        ]
    );
    shutdown(session).await;
}

#[tokio::test]
async fn seed_transcript_is_context_for_the_first_turn() {
    let seed = vec![
        item(
            "seed-1",
            ItemBody::UserMessage {
                text: "My favourite colour is teal.".into(),
            },
        ),
        item("seed-2", assistant("Noted.")),
    ];
    let mut session = start_with(fixture("seed"), request(seed)).await;
    session
        .commands
        .send(prompt("What is my favourite colour? Reply with one word."))
        .unwrap();
    let events = until(&mut session, is_turn_end).await;
    assert_eq!(
        events[events.len() - 3..],
        [
            AdapterEvent::ItemCompleted {
                item: item("item-1", assistant("Teal"))
            },
            AdapterEvent::UsageReported {
                windows: vec![weekly(25.0)]
            },
            AdapterEvent::TurnCompleted { turn_id: turn() },
        ]
    );
    shutdown(session).await;
}

/// `turn.jsonl` up to and including line `last`, then `tail`.
fn turn_prefix(last: usize, tail: &str) -> Fixture {
    let text = std::fs::read_to_string(path("turn")).unwrap();
    let head: String = text
        .lines()
        .take(last)
        .map(|line| line.to_owned() + "\n")
        .collect();
    Fixture::parse("turn-prefix", &(head + tail)).unwrap()
}

#[tokio::test]
async fn missing_login_fails_start_with_auth() {
    // Line 6 sends account/read; the answer is what a logged-out CODEX_HOME gets.
    let logged_out = r#"{"id":1,"result":{"account":null,"requiresOpenaiAuth":true}}"#;
    let tail = Record::Out(logged_out.into()).to_line() + "\n{\"exit\":0}\n";
    let result = timeout(
        TIMEOUT,
        codex::start(
            Transport::replay(turn_prefix(6, &tail)),
            request(Vec::new()),
        ),
    )
    .await
    .unwrap();
    let error = result.unwrap_err();
    assert_eq!(error.class, ErrorClass::Auth, "{}", error.message);
}

#[tokio::test]
async fn crash_mid_turn_fails_the_turn_then_exits_with_the_error() {
    let mut session = start_with(turn_prefix(18, "{\"exit\":1}\n"), request(Vec::new())).await;
    session
        .commands
        .send(AdapterCommand::SetModel {
            model: "gpt-6-luna".into(),
        })
        .unwrap();
    session
        .commands
        .send(AdapterCommand::SetPermissionMode {
            mode: PermissionMode::ReadOnly,
        })
        .unwrap();
    session
        .commands
        .send(prompt("Reply with the word ok."))
        .unwrap();
    let crashed = TurnError {
        class: ErrorClass::Fatal,
        message: "codex app-server exited with code 1".into(),
    };
    let events = until(&mut session, |event| {
        matches!(event, AdapterEvent::Exited { .. })
    })
    .await;
    assert_eq!(
        events[events.len() - 3..],
        [
            AdapterEvent::TurnStarted { turn_id: turn() },
            AdapterEvent::TurnFailed {
                turn_id: turn(),
                error: crashed.clone()
            },
            AdapterEvent::Exited {
                error: Some(crashed)
            },
        ]
    );
}

// ---- Schema ----

fn schema(name: &str) -> jsonschema::Validator {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("schemas/codex")
        .join(format!("{name}.json"));
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

/// The schema of the result of each request the adapter sends.
fn response_schema(method: &str) -> &'static str {
    match method {
        "initialize" => "InitializeResponse",
        "account/read" => "GetAccountResponse",
        "account/rateLimits/read" => "GetAccountRateLimitsResponse",
        "thread/start" => "ThreadStartResponse",
        "thread/inject_items" => "ThreadInjectItemsResponse",
        "turn/start" => "TurnStartResponse",
        "turn/interrupt" => "TurnInterruptResponse",
        _ => panic!("no response schema for {method}"),
    }
}

/// The schema of herder's answer to each server request it answers.
fn answer_schema(method: &str) -> &'static str {
    match method {
        "item/commandExecution/requestApproval" => "CommandExecutionRequestApprovalResponse",
        "item/fileChange/requestApproval" => "FileChangeRequestApprovalResponse",
        _ => panic!("no answer schema for {method}"),
    }
}

/// Every line in every fixture, in both directions, is valid against the codex schema
/// snapshot the adapter was written from, so a hand-built fixture cannot invent a shape and a
/// regenerated snapshot flags what changed.
#[test]
fn fixtures_match_the_codex_schema() {
    let mut validators: HashMap<&str, jsonschema::Validator> = HashMap::new();
    let mut check = |schema_name: &'static str, value: &Value, at: &str| {
        let validator = validators
            .entry(schema_name)
            .or_insert_with(|| schema(schema_name));
        let errors: Vec<String> = validator
            .iter_errors(value)
            .map(|error| format!("{error} at {}", error.instance_path()))
            .collect();
        assert!(
            errors.is_empty(),
            "{at}: not a valid {schema_name}: {errors:#?}"
        );
    };
    for name in FIXTURES {
        let fixture = fixture(name);
        assert_eq!(
            fixture
                .header
                .as_ref()
                .and_then(|header| header.cli_version.as_deref()),
            Some(codex::CODEX_VERSION),
            "{name}"
        );
        // Request ids herder sent, and server requests it has yet to answer, by id.
        let mut sent: HashMap<String, String> = HashMap::new();
        let mut asked: HashMap<String, String> = HashMap::new();
        for (index, record) in fixture.records().enumerate() {
            let at = format!("{name} record {}", index + 1);
            let (Record::In(line) | Record::Out(line)) = record else {
                continue;
            };
            let message: Value = serde_json::from_str(line).unwrap();
            let id = message.get("id").map(Value::to_string);
            let method = message.get("method").and_then(Value::as_str);
            match (record, id, method) {
                (Record::In(_), Some(id), Some(method)) => {
                    check("ClientRequest", &message, &at);
                    sent.insert(id, method.to_owned());
                }
                (Record::In(_), None, Some(_)) => check("ClientNotification", &message, &at),
                (Record::In(_), Some(id), None) => {
                    let method = asked.remove(&id).expect("answer to an unknown request");
                    check(answer_schema(&method), &message["result"], &at);
                }
                (Record::Out(_), Some(id), Some(method)) => {
                    check("ServerRequest", &message, &at);
                    asked.insert(id, method.to_owned());
                }
                (Record::Out(_), None, Some(_)) => check("ServerNotification", &message, &at),
                (Record::Out(_), Some(id), None) => {
                    let method = sent.remove(&id).expect("response to an unknown request");
                    check(response_schema(&method), &message["result"], &at);
                }
                _ => panic!("{at}: not a JSON-RPC message: {line}"),
            }
        }
    }
}
