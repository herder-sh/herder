//! Runs the ACP adapter against fixtures recorded from real agents, through `Box<dyn Adapter>`.
//!
//! `fixtures/opencode/` was recorded from `opencode acp` 1.18.21 with `herder dev record`, except
//! the files whose first line says they are hand-built; `fixtures/grok/auth_required.jsonl` from
//! `grok agent stdio` 1.0.46, logged out. The other Grok fixtures are hand-built around that
//! recorded `initialize`, since no Grok login was available.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use herder_adapters::acp::{AcpAdapter, AgentProfile};
use herder_adapters::{
    Adapter, AdapterCommand, AdapterEvent, AdapterSession, Capabilities, StartRequest,
};
use herder_protocol::{
    ApprovalDecision, ApprovalId, Bytes, ErrorClass, Image, Item, ItemBody, ItemId, PermissionMode,
    TurnError, TurnId,
};
use serde_json::json;
use tokio::time::timeout;

const TIMEOUT: Duration = Duration::from_secs(10);

fn fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(path)
}

fn request(mode: PermissionMode) -> StartRequest {
    StartRequest {
        config_dir: Some(PathBuf::from("/nonexistent/account")),
        env: BTreeMap::new(),
        // Where the fixtures were recorded.
        cwd: PathBuf::from("/tmp/acpwork/repo"),
        model: None,
        permission_mode: mode,
        seed: Vec::new(),
        resume: None,
        mcp: None,
        launcher: Vec::new(),
    }
}

async fn try_start(
    profile: AgentProfile,
    path: &str,
    request: StartRequest,
) -> Result<AdapterSession, TurnError> {
    let adapter: Box<dyn Adapter> = Box::new(AcpAdapter::replaying(profile, fixture(path)));
    timeout(TIMEOUT, adapter.start(request))
        .await
        .expect("start timed out")
}

async fn start(path: &str, request: StartRequest) -> AdapterSession {
    try_start(AgentProfile::opencode(), path, request)
        .await
        .unwrap()
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

async fn shutdown(mut session: AdapterSession) {
    session.commands.send(AdapterCommand::Shutdown).unwrap();
    let events = until(&mut session, |event| {
        matches!(event, AdapterEvent::Exited { .. })
    })
    .await;
    assert_eq!(events.last(), Some(&AdapterEvent::Exited { error: None }));
}

fn turn() -> TurnId {
    TurnId::new("turn-1")
}

fn prompt(session: &AdapterSession, text: &str) {
    session
        .commands
        .send(AdapterCommand::SendPrompt {
            agent_sender: None,
            turn_id: turn(),
            text: text.into(),
            images: Vec::new(),
        })
        .unwrap();
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

fn message(id: &str, text: &str) -> Item {
    item(id, ItemBody::AssistantMessage { text: text.into() })
}

fn model(name: &str) -> AdapterEvent {
    AdapterEvent::ModelChanged { model: name.into() }
}

#[tokio::test]
async fn prompt_streams_the_reply() {
    let mut session = start("opencode/prompt.jsonl", request(PermissionMode::Ask)).await;
    assert_eq!(
        session.capabilities,
        Capabilities {
            native_model_switch: true,
            native_permission_mode_switch: true,
            reports_usage: false,
            native_resume: false,
        }
    );
    prompt(&session, "reply with the word ok");
    let events = until(&mut session, is_turn_end).await;
    assert_eq!(
        events,
        [
            model("opencode/big-pickle"),
            AdapterEvent::TurnStarted { turn_id: turn() },
            AdapterEvent::ItemStarted {
                item: message("item-1", "")
            },
            AdapterEvent::ItemDelta {
                item_id: ItemId::new("item-1"),
                text: "ok".into()
            },
            AdapterEvent::ItemCompleted {
                item: message("item-1", "ok")
            },
            AdapterEvent::TurnCompleted { turn_id: turn() },
        ]
    );
    shutdown(session).await;
}

/// The items of the recorded tool call up to its approval request.
fn tool_call() -> Item {
    item(
        "item-2",
        ItemBody::ToolCall {
            name: "bash".into(),
            input: json!({"command": "echo hi > out.txt", "cwd": "/tmp/acpwork/repo"}),
        },
    )
}

#[tokio::test]
async fn permission_request_is_asked_and_answered() {
    let mut session = start(
        "opencode/approval_allow.jsonl",
        request(PermissionMode::Ask),
    )
    .await;
    prompt(
        &session,
        "Run the shell command: echo hi > out.txt  then reply with the word done",
    );
    let events = until(&mut session, |event| {
        matches!(event, AdapterEvent::ApprovalRequested { .. })
    })
    .await;
    let completed: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::ItemCompleted { item } => Some(item.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        completed,
        [
            message("item-1", "I'll create the file first."),
            tool_call()
        ]
    );
    assert_eq!(
        events.last(),
        Some(&AdapterEvent::ApprovalRequested {
            approval_id: ApprovalId::new("approval-1"),
            turn_id: turn(),
            tool_call_id: ItemId::new("item-2"),
            summary: "echo hi > out.txt".into(),
        })
    );

    session
        .commands
        .send(AdapterCommand::AnswerApproval {
            approval_id: ApprovalId::new("approval-1"),
            decision: ApprovalDecision::Allow,
        })
        .unwrap();
    let events = until(&mut session, is_turn_end).await;
    let completed: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::ItemCompleted { item } => Some(item.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        completed,
        [
            item(
                "item-3",
                ItemBody::ToolResult {
                    call_id: ItemId::new("item-2"),
                    output: "(no output)".into(),
                    is_error: false,
                }
            ),
            message("item-4", "done"),
        ]
    );
    assert_eq!(
        events.last(),
        Some(&AdapterEvent::TurnCompleted { turn_id: turn() })
    );
    shutdown(session).await;
}

#[tokio::test]
async fn full_access_allows_without_asking() {
    let mut session = start(
        "opencode/approval_allow.jsonl",
        request(PermissionMode::FullAccess),
    )
    .await;
    prompt(
        &session,
        "Run the shell command: echo hi > out.txt  then reply with the word done",
    );
    let events = until(&mut session, is_turn_end).await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AdapterEvent::ApprovalRequested { .. }))
    );
    assert_eq!(
        events.last(),
        Some(&AdapterEvent::TurnCompleted { turn_id: turn() })
    );
    shutdown(session).await;
}

#[tokio::test]
async fn read_only_refuses_commands_without_asking() {
    let mut session = start(
        "opencode/approval_deny.jsonl",
        request(PermissionMode::ReadOnly),
    )
    .await;
    prompt(
        &session,
        "Run the shell command: echo hi > out.txt  then reply with the word done",
    );
    let events = until(&mut session, is_turn_end).await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AdapterEvent::ApprovalRequested { .. }))
    );
    let result = events.iter().find_map(|event| match event {
        AdapterEvent::ItemCompleted { item } => match &item.body {
            ItemBody::ToolResult {
                call_id, is_error, ..
            } => Some((call_id.clone(), *is_error)),
            _ => None,
        },
        _ => None,
    });
    assert_eq!(result, Some((ItemId::new("item-1"), true)));
    shutdown(session).await;
}

#[tokio::test]
async fn interrupt_cancels_the_turn() {
    let mut session = start("opencode/interrupt.jsonl", request(PermissionMode::Ask)).await;
    prompt(
        &session,
        "Write the numbers from 1 to 300, one per line, as words.",
    );
    let events = until(&mut session, |event| {
        matches!(event, AdapterEvent::ItemDelta { item_id, .. } if item_id.as_str() == "item-2")
    })
    .await;
    // The thought streamed first and was closed when the message began.
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::ItemCompleted { item: Item { parent_call_id: None,  body: ItemBody::Reasoning { text }, .. } }
            if text.starts_with("The user wants numbers")
    )));
    session.commands.send(AdapterCommand::Interrupt).unwrap();
    let events = until(&mut session, is_turn_end).await;
    assert!(matches!(
        &events[..],
        [
            AdapterEvent::ItemCompleted { item: Item { parent_call_id: None,  body: ItemBody::AssistantMessage { .. }, .. } },
            AdapterEvent::TurnInterrupted { turn_id },
        ] if *turn_id == turn()
    ));
    shutdown(session).await;
}

#[tokio::test]
async fn starting_model_is_set_through_the_config_option() {
    let mut start_request = request(PermissionMode::Ask);
    start_request.model = Some("opencode/nemotron-3.5-lightning-free".into());
    let mut session = start("opencode/set_model.jsonl", start_request).await;
    let events = until(&mut session, |event| {
        matches!(event, AdapterEvent::ModelChanged { .. })
    })
    .await;
    assert_eq!(events, [model("opencode/nemotron-3.5-lightning-free")]);
    shutdown(session).await;
}

#[tokio::test]
async fn set_model_switches_natively() {
    let mut session = start("opencode/set_model.jsonl", request(PermissionMode::Ask)).await;
    session
        .commands
        .send(AdapterCommand::SetModel {
            model: "opencode/nemotron-3.5-lightning-free".into(),
        })
        .unwrap();
    let events = until(&mut session, |event| {
        *event == model("opencode/nemotron-3.5-lightning-free")
    })
    .await;
    assert_eq!(events[0], model("opencode/big-pickle"));
    shutdown(session).await;
}

#[tokio::test]
async fn unknown_starting_model_fails_the_start() {
    let mut start_request = request(PermissionMode::Ask);
    start_request.model = Some("anthropic/claude-sonnet-4-5".into());
    let error = try_start(
        AgentProfile::opencode(),
        "opencode/unknown_model.jsonl",
        start_request,
    )
    .await
    .unwrap_err();
    assert_eq!(error.class, ErrorClass::Fatal);
    assert!(error.message.contains("model not found"), "{error:?}");
}

#[tokio::test]
async fn logged_out_grok_fails_the_start_with_auth() {
    let error = try_start(
        AgentProfile::grok(),
        "grok/auth_required.jsonl",
        request(PermissionMode::Ask),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error,
        TurnError {
            class: ErrorClass::Auth,
            message: "session/new: Authentication required".into(),
        }
    );
}

#[tokio::test]
async fn quota_error_fails_the_turn_with_limit_reached() {
    let mut session = start("opencode/limit_reached.jsonl", request(PermissionMode::Ask)).await;
    prompt(&session, "reply with the word ok");
    let events = until(&mut session, is_turn_end).await;
    assert!(matches!(
        events.last(),
        Some(AdapterEvent::TurnFailed { error, .. }) if error.class == ErrorClass::LimitReached
    ));
    shutdown(session).await;
}

#[tokio::test]
async fn agent_dying_mid_turn_fails_the_turn_then_exits() {
    let mut session = start("opencode/crash.jsonl", request(PermissionMode::Ask)).await;
    prompt(&session, "reply with the word ok");
    let events = until(&mut session, |event| {
        matches!(event, AdapterEvent::Exited { .. })
    })
    .await;
    let error = TurnError {
        class: ErrorClass::Transient,
        message: "the agent exited unexpectedly with code 1".into(),
    };
    assert_eq!(
        &events[events.len() - 3..],
        [
            AdapterEvent::ItemCompleted {
                item: message("item-1", "ok")
            },
            AdapterEvent::TurnFailed {
                turn_id: turn(),
                error: error.clone()
            },
            AdapterEvent::Exited { error: Some(error) },
        ]
    );
}

#[tokio::test]
async fn seed_goes_in_front_of_the_first_prompt() {
    let mut start_request = request(PermissionMode::Ask);
    start_request.seed = vec![
        Item {
            agent_message: None,
            parent_call_id: None,
            id: ItemId::new("old-1"),
            turn_id: TurnId::new("old"),
            body: ItemBody::UserMessage {
                text: "what is 2+2?".into(),
                attachments: Vec::new(),
            },
        },
        Item {
            agent_message: None,
            parent_call_id: None,
            id: ItemId::new("old-2"),
            turn_id: TurnId::new("old"),
            body: ItemBody::AssistantMessage { text: "4".into() },
        },
    ];
    let mut session = start("opencode/seeded.jsonl", start_request).await;
    prompt(&session, "reply with the word ok");
    let events = until(&mut session, is_turn_end).await;
    assert_eq!(
        events.last(),
        Some(&AdapterEvent::TurnCompleted { turn_id: turn() })
    );
    shutdown(session).await;
}

#[tokio::test]
async fn permission_mode_switch_is_acknowledged() {
    let mut session = start("opencode/prompt.jsonl", request(PermissionMode::Ask)).await;
    session
        .commands
        .send(AdapterCommand::SetPermissionMode {
            mode: PermissionMode::FullAccess,
        })
        .unwrap();
    let events = until(&mut session, |event| {
        matches!(event, AdapterEvent::PermissionModeChanged { .. })
    })
    .await;
    assert_eq!(
        events.last(),
        Some(&AdapterEvent::PermissionModeChanged {
            mode: PermissionMode::FullAccess
        })
    );
    shutdown(session).await;
}

async fn start_grok(path: &str, request: StartRequest) -> AdapterSession {
    try_start(AgentProfile::grok(), path, request)
        .await
        .unwrap()
}

#[tokio::test]
async fn grok_streams_the_reply_and_switches_models_natively() {
    let mut session = start_grok("grok/prompt.jsonl", request(PermissionMode::Ask)).await;
    assert!(session.capabilities.native_model_switch);
    prompt(&session, "reply with the word ok");
    let events = until(&mut session, is_turn_end).await;
    let completed: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::ItemCompleted { item } => Some(item.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(events[0], model("grok-4.6"));
    assert_eq!(
        completed,
        [
            item(
                "item-1",
                ItemBody::Reasoning {
                    text: "The user wants ok.".into()
                }
            ),
            message("item-2", "ok"),
        ]
    );
    assert_eq!(
        events.last(),
        Some(&AdapterEvent::TurnCompleted { turn_id: turn() })
    );
    shutdown(session).await;

    let mut session = start_grok("grok/set_model.jsonl", request(PermissionMode::Ask)).await;
    session
        .commands
        .send(AdapterCommand::SetModel {
            model: "grok-4.5".into(),
        })
        .unwrap();
    let events = until(&mut session, |event| *event == model("grok-4.5")).await;
    assert_eq!(events[0], model("grok-4.6"));
    shutdown(session).await;
}

#[tokio::test]
async fn grok_asks_before_a_command_and_runs_it_once_allowed() {
    let mut session = start_grok("grok/approval_allow.jsonl", request(PermissionMode::Ask)).await;
    prompt(
        &session,
        "Run the shell command: echo hi > out.txt  then reply with the word done",
    );
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
            summary: "echo hi > out.txt".into(),
        })
    );
    session
        .commands
        .send(AdapterCommand::AnswerApproval {
            approval_id: ApprovalId::new("approval-1"),
            decision: ApprovalDecision::Allow,
        })
        .unwrap();
    let events = until(&mut session, is_turn_end).await;
    let completed: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::ItemCompleted { item } => Some(item.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        completed,
        [
            item(
                "item-2",
                ItemBody::ToolResult {
                    call_id: ItemId::new("item-1"),
                    output: "exit code 0".into(),
                    is_error: false,
                }
            ),
            message("item-3", "done"),
        ]
    );
    assert_eq!(
        events.last(),
        Some(&AdapterEvent::TurnCompleted { turn_id: turn() })
    );
    shutdown(session).await;
}

fn png() -> Image {
    Image {
        media_type: "image/png".into(),
        data: Bytes(b"\x89PNG\r\n\x1a\npng".to_vec()),
    }
}

/// Starts `adapter` on its fixture and runs one prompt carrying [`png`] to its end.
async fn prompt_with_image(adapter: &AcpAdapter) -> Vec<AdapterEvent> {
    let mut session = timeout(TIMEOUT, adapter.start(request(PermissionMode::Ask)))
        .await
        .expect("start timed out")
        .unwrap();
    session
        .commands
        .send(AdapterCommand::SendPrompt {
            agent_sender: None,
            turn_id: turn(),
            text: "reply with the word ok".into(),
            images: vec![png()],
        })
        .unwrap();
    let events = until(&mut session, is_turn_end).await;
    shutdown(session).await;
    events
}

#[tokio::test]
async fn an_agent_that_advertises_images_gets_them_as_image_blocks() {
    let adapter = AcpAdapter::replaying(AgentProfile::opencode(), fixture("opencode/image.jsonl"));
    assert!(adapter.accepts_images());
    // The recording only matches a prompt with the image block ahead of the text.
    let events = prompt_with_image(&adapter).await;
    assert_eq!(
        events.last(),
        Some(&AdapterEvent::TurnCompleted { turn_id: turn() })
    );
    assert!(adapter.accepts_images());
}

#[tokio::test]
async fn an_agent_that_takes_no_images_gets_a_line_naming_each_and_is_believed() {
    // A profile that guesses wrong is corrected by what the agent says in `initialize`.
    let profile = AgentProfile {
        images: true,
        ..AgentProfile::grok()
    };
    let adapter = AcpAdapter::replaying(profile, fixture("grok/image.jsonl"));
    assert!(adapter.accepts_images());
    // The recording only matches a prompt whose text names the image.
    let events = prompt_with_image(&adapter).await;
    assert_eq!(
        events.last(),
        Some(&AdapterEvent::TurnCompleted { turn_id: turn() })
    );
    assert!(!adapter.accepts_images());
    assert!(!AcpAdapter::new(AgentProfile::grok()).accepts_images());
    assert!(!AcpAdapter::new(AgentProfile::cursor()).accepts_images());
}
