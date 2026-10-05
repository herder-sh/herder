//! The Codex adapter, replayed against `codex app-server` recordings in `fixtures/codex`.
//!
//! Every fixture but `limit_reached.jsonl` and `image.jsonl` was recorded from the real CLI with
//! `fixtures/codex/record.py`; each test ends with a clean shutdown, which fails if the adapter
//! sent anything the recording did not.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use herder_adapters::codex;
use herder_adapters::fixture::{Fixture, Record};
use herder_adapters::transport::Transport;
use herder_adapters::{Adapter, AdapterCommand, AdapterEvent, AdapterSession, StartRequest};
use herder_protocol::{
    ApprovalDecision, ApprovalId, Bytes, ErrorClass, Image, Item, ItemBody, ItemId, PermissionMode,
    Timestamp, TurnError, TurnId, TurnUsage, UsageWindow,
};
use serde_json::{Value, json};
use tokio::time::timeout;

const TIMEOUT: Duration = Duration::from_secs(10);

/// The worktree the fixtures were recorded in; replay never touches it.
const CWD: &str = "/tmp/herder-codex-fixture";

const FIXTURES: [&str; 7] = [
    "turn",
    "image",
    "approval",
    "interrupt",
    "limit_reached",
    "seed",
    "resume",
];

/// The thread `turn.jsonl` opened, which `resume.jsonl` reopens.
const TURN_THREAD: &str = "01a0fc7b-3cda-7031-a6b4-7a11c1f09469";

/// The thread each recording opened.
fn thread(name: &str) -> &'static str {
    match name {
        "turn" | "image" | "limit_reached" | "resume" => TURN_THREAD,
        "approval" => "01a0fc7b-51c2-7923-9f3f-644d234f31d0",
        "interrupt" => "01a0fc7b-74b0-7e30-a500-81aa38638e40",
        "seed" => "01a0fc7b-85e5-7f82-a515-5e0075532352",
        _ => unreachable!("no fixture {name}"),
    }
}

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
        resume: None,
        mcp: None,
        launcher: Vec::new(),
        skills: None,
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
    start_on(name, request(Vec::new())).await
}

/// Starts on `name` for `request` and checks the startup events every recording shares.
async fn start_on(name: &str, request: StartRequest) -> AdapterSession {
    let mut session = start_with(fixture(name), request).await;
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
            AdapterEvent::SessionIdentified {
                native_id: thread(name).into()
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
        agent_sender: None,
        turn_id: turn(),
        text: text.into(),
        images: Vec::new(),
    }
}

fn item(id: &str, body: ItemBody) -> Item {
    Item {
        agent_message: None,
        parent_call_id: None,
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
            AdapterEvent::TurnCompleted {
                turn_id: turn(),
                usage: tokens(7391, 5, 12032)
            },
        ]
    );
    shutdown(session).await;
}

#[tokio::test]
async fn a_prompts_images_go_ahead_of_its_text_as_data_urls() {
    assert!(codex::CodexAdapter::default().accepts_images());
    let mut session = start("image").await;
    let image = Image {
        media_type: "image/png".into(),
        data: Bytes(b"\x89PNG\r\n\x1a\npng".to_vec()),
    };
    for command in [
        AdapterCommand::SetModel {
            model: "gpt-6-luna".into(),
        },
        AdapterCommand::SetPermissionMode {
            mode: PermissionMode::ReadOnly,
        },
        AdapterCommand::SendPrompt {
            agent_sender: None,
            turn_id: turn(),
            text: "Reply with the word ok.".into(),
            images: vec![image],
        },
    ] {
        session.commands.send(command).unwrap();
    }
    // The recording only matches a `turn/start` that carries the image.
    let events = until(&mut session, is_turn_end).await;
    assert_eq!(
        events.last(),
        Some(&AdapterEvent::TurnCompleted {
            turn_id: turn(),
            usage: tokens(7391, 5, 12032)
        })
    );
    shutdown(session).await;
}

#[tokio::test]
async fn resume_reopens_the_thread_and_goes_on_in_it() {
    let request = StartRequest {
        resume: Some(TURN_THREAD.into()),
        ..request(Vec::new())
    };
    // The replay checks that `thread/resume` names the thread, and every later request.
    let mut session = start_on("resume", request).await;
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
    assert_eq!(streamed(&events, "item-1"), "ok");
    assert_eq!(
        events.last(),
        Some(&AdapterEvent::TurnCompleted {
            turn_id: turn(),
            usage: tokens(7391, 5, 12032)
        })
    );
    shutdown(session).await;
}

#[tokio::test]
async fn a_thread_codex_cannot_resume_fails_start() {
    // Line 10 sends thread/resume; the answer is what a CODEX_HOME without the rollout gets.
    let text = std::fs::read_to_string(path("resume")).unwrap();
    let head: String = text
        .lines()
        .filter(|line| !line.starts_with('#'))
        .take(10)
        .map(|line| line.to_owned() + "\n")
        .collect();
    let missing =
        r#"{"id":3,"error":{"code":-32600,"message":"no rollout found for thread id 01a0fc7b"}}"#;
    let tail = Record::Out(missing.into()).to_line() + "\n{\"exit\":0}\n";
    let request = StartRequest {
        resume: Some(TURN_THREAD.into()),
        ..request(Vec::new())
    };
    let fixture = Fixture::parse("resume-missing", &(head + &tail)).unwrap();
    let result = timeout(TIMEOUT, codex::start(Transport::replay(fixture), request))
        .await
        .unwrap();
    let error = result.unwrap_err();
    assert_eq!(error.class, ErrorClass::Fatal);
    assert!(error.message.contains("thread/resume"), "{}", error.message);
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
            AdapterEvent::TurnCompleted {
                turn_id: turn(),
                usage: tokens(3861, 79, 28032)
            },
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
                attachments: Vec::new(),
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
            AdapterEvent::TurnCompleted {
                turn_id: turn(),
                usage: tokens(3618, 6, 12288)
            },
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
        "thread/resume" => "ThreadResumeResponse",
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

#[tokio::test]
async fn read_usage_reads_the_limits_without_opening_a_thread() {
    // The recorded handshake, then the rate limits as the second request; stdin is closed
    // right after, with no thread/start.
    let fixture = turn_prefix(
        5,
        r#"{"dir":"in","line":"{\"id\":1,\"method\":\"account/rateLimits/read\",\"params\":null}"}
{"dir":"out","line":"{\"id\":1,\"result\":{\"rateLimits\":{\"limitId\":\"codex\",\"primary\":{\"usedPercent\":25,\"windowDurationMins\":10080,\"resetsAt\":1791052121},\"secondary\":null},\"rateLimitsByLimitId\":null}}"}
{"dir":"in","eof":true}
{"exit":0}
"#,
    );
    let windows = timeout(
        TIMEOUT,
        codex::read_usage(Transport::replay(fixture), &request(Vec::new())),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(windows, [weekly(25.0)]);
}

/// A turn's tokens, the `last` of its `thread/tokenUsage/updated`s added up with the cached
/// input split out. `gpt-6-luna` is not in the price table, so there is no cost.
fn tokens(input: u64, output: u64, cache_read: u64) -> Option<TurnUsage> {
    Some(TurnUsage {
        input,
        output,
        cache_read,
        ..TurnUsage::default()
    })
}

#[tokio::test]
async fn skill_mentions_reach_codex_as_typed() {
    // Codex expands `$name` itself, so each recording matches only with the text unchanged.
    let recorded = std::fs::read_to_string(path("turn")).unwrap();
    for text in [
        "Use $review on the diff",
        "Run $review, then $git:commit.",
        "echo $HOME costs $5",
    ] {
        let edited = recorded.replace("Reply with the word ok.", text);
        let fixture = Fixture::parse("skill-mentions", &edited).unwrap();
        let mut session = start_with(fixture, request(Vec::new())).await;
        for command in [
            AdapterCommand::SetModel {
                model: "gpt-6-luna".into(),
            },
            AdapterCommand::SetPermissionMode {
                mode: PermissionMode::ReadOnly,
            },
            prompt(text),
        ] {
            session.commands.send(command).unwrap();
        }
        let events = until(&mut session, is_turn_end).await;
        assert!(
            matches!(events.last(), Some(AdapterEvent::TurnCompleted { .. })),
            "{text}: {events:?}"
        );
        shutdown(session).await;
    }
}
