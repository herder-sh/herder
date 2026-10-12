//! Contract tests: the committed `tools/list` snapshot matches the Rust types, every schema is
//! one MCP clients accept, and example calls and results parse and validate against them.

use std::collections::BTreeSet;
use std::path::PathBuf;

use herder_protocol::{Answer, ApprovalDecision, CommandBody, CommandResult, SessionStatus};
use herder_protocol::{ApprovalId, PermissionMode, Provider, QuestionId, SessionId, TurnId};
use herder_tasktools::*;
use serde_json::{Value, json};

#[test]
fn tools_list_matches_snapshot() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schema/tools.json");
    let generated = serde_json::to_string_pretty(&tools_list()).unwrap() + "\n";
    if std::env::var_os("HERDER_UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &generated).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == generated,
        "schema/tools.json is out of date with the Rust types. This is a contract change: \
         review it, then run `HERDER_UPDATE_SCHEMA=1 cargo test -p herder-tasktools` and commit the diff."
    );
}

#[test]
fn tools_list_is_the_mcp_shape() {
    let list = serde_json::to_value(tools_list()).unwrap();
    let tools = list["tools"].as_array().unwrap();
    let names: Vec<_> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "spawn",
            "send",
            "send_session",
            "status",
            "wait_for",
            "answer",
            "escalate",
            "publish",
            "overview",
            "command"
        ]
    );
    for tool in tools {
        let keys: BTreeSet<_> = tool.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys,
            BTreeSet::from(
                ["name", "description", "inputSchema", "outputSchema"].map(String::from)
            )
        );
        assert!(!tool["description"].as_str().unwrap().is_empty());
    }
}

#[test]
fn tool_names_round_trip() {
    for tool in Tool::ALL {
        assert_eq!(Tool::from_name(tool.name()), Some(tool));
    }
    assert_eq!(Tool::from_name("unknown"), None);
}

/// Some clients (the Anthropic API among them) reject an input schema whose root is not a
/// plain object schema, and agents read inlined schemas better than `$ref`s.
#[test]
fn schemas_are_self_contained_object_schemas() {
    for tool in Tool::ALL {
        for (kind, schema) in [
            ("input", tool.input_schema()),
            ("output", tool.output_schema()),
        ] {
            let value = schema.as_value();
            assert_eq!(value["type"], "object", "{} {kind}", tool.name());
            assert!(
                !value.to_string().contains("$ref") && value.get("$defs").is_none(),
                "{} {kind} schema is not inlined",
                tool.name()
            );
            jsonschema::validator_for(value).unwrap();
        }
        let input = tool.input_schema();
        for key in ["oneOf", "anyOf", "allOf"] {
            assert!(
                input.get(key).is_none(),
                "{} input has a top-level {key}",
                tool.name()
            );
        }
        assert_eq!(input.get("additionalProperties"), Some(&json!(false)));
    }
}

fn assert_valid(schema: &schemars::Schema, value: &Value) {
    let validator = jsonschema::validator_for(schema.as_value()).unwrap();
    let errors: Vec<_> = validator
        .iter_errors(value)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{value} does not match: {errors:?}");
}

fn call(tool: Tool, arguments: Value) -> Result<ToolCall, ToolError> {
    ToolCall::parse(tool, Some(arguments))
}

/// Every call shape: arguments valid against the input schema, parsed to the expected typed
/// call, and serialized back to the same arguments.
#[test]
fn calls_parse_validate_and_round_trip() {
    let child = || SessionId::new("01J9CHILD");
    let cases = [
        (
            json!({ "task": "Fix tests", "prompt": "Make the auth tests pass." }),
            ToolCall::Spawn(SpawnInput {
                task: "Fix tests".into(),
                prompt: "Make the auth tests pass.".into(),
                provider: None,
                model: None,
                permission_mode: None,
            }),
        ),
        (
            json!({
                "task": "Port to codex",
                "prompt": "Port it.",
                "provider": "codex",
                "model": "gpt-5-codex",
                "permission_mode": "auto_edit"
            }),
            ToolCall::Spawn(SpawnInput {
                task: "Port to codex".into(),
                prompt: "Port it.".into(),
                provider: Some(Provider::Codex),
                model: Some("gpt-5-codex".into()),
                permission_mode: Some(PermissionMode::AutoEdit),
            }),
        ),
        (
            json!({ "child": "01J9CHILD", "text": "Also cover logout." }),
            ToolCall::Send(SendInput {
                child: child(),
                text: "Also cover logout.".into(),
            }),
        ),
        (json!({}), ToolCall::Status(StatusInput { children: None })),
        (
            json!({ "children": ["01J9CHILD"] }),
            ToolCall::Status(StatusInput {
                children: Some(vec![child()]),
            }),
        ),
        (
            json!({ "timeout_secs": 600 }),
            ToolCall::WaitFor(WaitForInput {
                child: None,
                timeout_secs: 600,
            }),
        ),
        (
            json!({ "child": "01J9CHILD", "timeout_secs": 30 }),
            ToolCall::WaitFor(WaitForInput {
                child: Some(child()),
                timeout_secs: 30,
            }),
        ),
        (
            json!({ "child": "01J9CHILD", "question_id": "01J9Q", "text": "Use Postgres." }),
            ToolCall::Answer(AnswerInput::Question {
                child: child(),
                question_id: QuestionId::new("01J9Q"),
                answer: Answer::Text {
                    text: "Use Postgres.".into(),
                },
            }),
        ),
        (
            json!({ "child": "01J9CHILD", "question_id": "01J9Q", "choice": 1 }),
            ToolCall::Answer(AnswerInput::Question {
                child: child(),
                question_id: QuestionId::new("01J9Q"),
                answer: Answer::Choice { index: 1 },
            }),
        ),
        (
            json!({ "child": "01J9CHILD", "approval_id": "01J9A", "decision": "allow" }),
            ToolCall::Answer(AnswerInput::Approval {
                child: child(),
                approval_id: ApprovalId::new("01J9A"),
                decision: ApprovalDecision::Allow,
            }),
        ),
        (
            json!({ "child": "01J9CHILD", "approval_id": "01J9A", "decision": "deny" }),
            ToolCall::Answer(AnswerInput::Approval {
                child: child(),
                approval_id: ApprovalId::new("01J9A"),
                decision: ApprovalDecision::Deny,
            }),
        ),
        (
            json!({ "child": "01J9CHILD", "question_id": "01J9Q" }),
            ToolCall::Escalate(EscalateInput {
                child: child(),
                request: RequestRef::Question(QuestionId::new("01J9Q")),
                note: None,
            }),
        ),
        (
            json!({ "child": "01J9CHILD", "approval_id": "01J9A", "note": "It drops the staging table." }),
            ToolCall::Escalate(EscalateInput {
                child: child(),
                request: RequestRef::Approval(ApprovalId::new("01J9A")),
                note: Some("It drops the staging table.".into()),
            }),
        ),
        (
            json!({ "title": "Latency", "html": "<!doctype html><p>p50</p>" }),
            ToolCall::Publish(PublishInput {
                title: "Latency".into(),
                path: None,
                html: Some("<!doctype html><p>p50</p>".into()),
            }),
        ),
        (
            json!({ "title": "Settings screen", "path": "shots/settings.png" }),
            ToolCall::Publish(PublishInput {
                title: "Settings screen".into(),
                path: Some("shots/settings.png".into()),
                html: None,
            }),
        ),
        (json!({}), ToolCall::Overview(OverviewInput {})),
        (
            json!({ "command": { "type": "rename_session", "session_id": "01J9CHILD", "title": "Auth" } }),
            ToolCall::Command(CommandInput {
                command: CommandBody::RenameSession {
                    session_id: child(),
                    title: "Auth".into(),
                },
            }),
        ),
        (
            json!({ "command": { "type": "restart_daemon" } }),
            ToolCall::Command(CommandInput {
                command: CommandBody::RestartDaemon,
            }),
        ),
    ];
    for (arguments, expected) in cases {
        let tool = expected.tool();
        assert_valid(&tool.input_schema(), &arguments);
        assert_eq!(call(tool, arguments.clone()), Ok(expected.clone()));
        let back = match expected {
            ToolCall::Spawn(input) => serde_json::to_value(input),
            ToolCall::Send(input) => serde_json::to_value(input),
            ToolCall::SendSession(input) => serde_json::to_value(input),
            ToolCall::Status(input) => serde_json::to_value(input),
            ToolCall::WaitFor(input) => serde_json::to_value(input),
            ToolCall::Answer(input) => serde_json::to_value(input),
            ToolCall::Escalate(input) => serde_json::to_value(input),
            ToolCall::Publish(input) => serde_json::to_value(input),
            ToolCall::Overview(input) => serde_json::to_value(input),
            ToolCall::Command(input) => serde_json::to_value(input),
        };
        assert_eq!(back.unwrap(), arguments);
    }
}

#[test]
fn omitted_arguments_read_as_empty() {
    assert_eq!(
        ToolCall::parse(Tool::Status, None),
        Ok(ToolCall::Status(StatusInput { children: None }))
    );
    assert_eq!(
        ToolCall::parse(Tool::Spawn, None).unwrap_err().code,
        ErrorCode::InvalidArguments
    );
}

/// The flat answer and escalate schemas admit combinations that mean nothing; parsing refuses
/// them with an error the agent can act on.
#[test]
fn malformed_calls_are_invalid_arguments() {
    let cases = [
        (Tool::Spawn, json!({ "task": "No prompt" })),
        (
            Tool::Spawn,
            json!({ "task": "t", "prompt": "p", "depth": 2 }),
        ),
        (Tool::Send, json!({ "child": "01J9CHILD" })),
        (Tool::WaitFor, json!({ "child": "01J9CHILD" })),
        (Tool::Answer, json!({})),
        (Tool::Answer, json!({ "child": "01J9CHILD", "text": "yes" })),
        (
            Tool::Answer,
            json!({ "child": "01J9CHILD", "question_id": "01J9Q" }),
        ),
        (
            Tool::Answer,
            json!({ "child": "01J9CHILD", "question_id": "01J9Q", "text": "yes", "choice": 0 }),
        ),
        (
            Tool::Answer,
            json!({ "child": "01J9CHILD", "question_id": "01J9Q", "decision": "allow" }),
        ),
        (
            Tool::Answer,
            json!({ "child": "01J9CHILD", "approval_id": "01J9A" }),
        ),
        (
            Tool::Answer,
            json!({ "child": "01J9CHILD", "approval_id": "01J9A", "decision": "allow", "text": "ok" }),
        ),
        (
            Tool::Answer,
            json!({ "child": "01J9CHILD", "question_id": "01J9Q", "approval_id": "01J9A", "text": "yes" }),
        ),
        (
            Tool::Escalate,
            json!({ "child": "01J9CHILD", "note": "yours" }),
        ),
        (
            Tool::Escalate,
            json!({ "child": "01J9CHILD", "question_id": "01J9Q", "approval_id": "01J9A" }),
        ),
        (
            Tool::Answer,
            json!({ "question_id": "01J9Q", "text": "yes" }),
        ),
        (Tool::Escalate, json!({ "question_id": "01J9Q" })),
        (Tool::Publish, json!({ "html": "<p>untitled</p>" })),
        (
            Tool::Publish,
            json!({ "title": "t", "html": "<p></p>", "height": 400 }),
        ),
        (Tool::Overview, json!({ "all": true })),
        (Tool::Command, json!({})),
        (
            Tool::Command,
            json!({ "command": { "type": "format_disk" } }),
        ),
        (Tool::Command, json!({ "command": { "type": "interrupt" } })),
        (
            Tool::Command,
            json!({ "type": "interrupt", "session_id": "01J9CHILD" }),
        ),
    ];
    for (tool, arguments) in cases {
        let error = call(tool, arguments.clone()).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArguments, "{arguments}");
        assert!(!error.message.is_empty());
    }
}

/// Every output shape validates against its tool's output schema.
#[test]
fn outputs_match_their_schemas() {
    let child = || SessionId::new("01J9CHILD");
    let question = || Request::Question {
        question_id: QuestionId::new("01J9Q"),
        text: "Which database?".into(),
        choices: vec!["Postgres".into(), "SQLite".into()],
    };
    let approval = || Request::Approval {
        approval_id: ApprovalId::new("01J9A"),
        summary: "Run `rm -rf target`".into(),
    };
    let outputs = [
        (
            Tool::Spawn,
            serde_json::to_value(SpawnOutput {
                child: child(),
                branch: Some("herder/1a2b3c4d".into()),
            }),
        ),
        (
            Tool::Send,
            serde_json::to_value(SendOutput { queued: false }),
        ),
        (
            Tool::Send,
            serde_json::to_value(SendOutput { queued: true }),
        ),
        (
            Tool::Status,
            serde_json::to_value(StatusOutput {
                children: vec![
                    ChildStatus {
                        child: child(),
                        task: "Fix tests".into(),
                        branch: Some("herder/1a2b3c4d".into()),
                        status: SessionStatus::Running,
                        last_report: None,
                        open_questions: vec![question(), approval()],
                    },
                    ChildStatus {
                        child: SessionId::new("01J9CHILD2"),
                        task: "Write docs".into(),
                        branch: Some("herder/5e6f7a8b".into()),
                        status: SessionStatus::Idle,
                        last_report: Some("Docs written.".into()),
                        open_questions: vec![],
                    },
                ],
            }),
        ),
        (
            Tool::WaitFor,
            serde_json::to_value(WaitForOutput::Report {
                child: child(),
                turn_id: TurnId::new("01J9TURN"),
                summary: "Tests pass.".into(),
                status: SessionStatus::Idle,
            }),
        ),
        (
            Tool::WaitFor,
            serde_json::to_value(WaitForOutput::Request {
                child: child(),
                request: question(),
            }),
        ),
        (
            Tool::WaitFor,
            serde_json::to_value(WaitForOutput::Request {
                child: child(),
                request: approval(),
            }),
        ),
        (Tool::WaitFor, serde_json::to_value(WaitForOutput::Timeout)),
        (Tool::WaitFor, serde_json::to_value(WaitForOutput::Idle)),
        (Tool::Answer, serde_json::to_value(AnswerOutput {})),
        (Tool::Escalate, serde_json::to_value(EscalateOutput {})),
        (
            Tool::Publish,
            serde_json::to_value(PublishOutput {
                url: Some("https://krowk.com/a/art_1".into()),
                expires_at: Some("2026-10-11T17:57:01Z".parse().unwrap()),
            }),
        ),
        (
            Tool::Publish,
            serde_json::to_value(PublishOutput {
                url: None,
                expires_at: None,
            }),
        ),
        (
            Tool::Overview,
            serde_json::to_value(OverviewOutput {
                you: child(),
                sessions: Vec::new(),
                projects: Vec::new(),
                accounts: Vec::new(),
            }),
        ),
        (Tool::Command, serde_json::to_value(CommandResult::Applied)),
        (
            Tool::Command,
            serde_json::to_value(CommandResult::SessionCreated {
                session_id: child(),
            }),
        ),
    ];
    for (tool, output) in outputs {
        let output = output.unwrap();
        assert_valid(&tool.output_schema(), &output);
        let result = CallToolResult::success(&output).unwrap();
        assert!(!result.is_error);
        assert_eq!(result.structured_content.as_ref(), Some(&output));
    }
}

#[test]
fn wait_for_output_wire_shape() {
    assert_eq!(
        serde_json::to_value(WaitForOutput::Request {
            child: SessionId::new("01J9CHILD"),
            request: Request::Approval {
                approval_id: ApprovalId::new("01J9A"),
                summary: "Push to main".into(),
            },
        })
        .unwrap(),
        json!({
            "kind": "request",
            "child": "01J9CHILD",
            "request": { "kind": "approval", "approval_id": "01J9A", "summary": "Push to main" }
        })
    );
    assert_eq!(
        serde_json::to_value(WaitForOutput::Timeout).unwrap(),
        json!({ "kind": "timeout" })
    );
}

#[test]
fn call_results_are_the_mcp_shape() {
    assert_eq!(
        serde_json::to_value(
            CallToolResult::success(&SpawnOutput {
                child: SessionId::new("01J9CHILD"),
                branch: Some("herder/1a2b3c4d".into()),
            })
            .unwrap()
        )
        .unwrap(),
        json!({
            "content": [{ "type": "text", "text": r#"{"branch":"herder/1a2b3c4d","child":"01J9CHILD"}"# }],
            "structuredContent": { "child": "01J9CHILD", "branch": "herder/1a2b3c4d" },
            "isError": false
        })
    );
}

#[test]
fn tool_errors_carry_stable_codes() {
    let codes = [
        (ErrorCode::InvalidArguments, "invalid_arguments"),
        (ErrorCode::DepthExceeded, "depth_exceeded"),
        (ErrorCode::NotAllowed, "not_allowed"),
        (ErrorCode::NotYourChild, "not_your_child"),
        (ErrorCode::NotFound, "not_found"),
        (ErrorCode::AlreadyResolved, "already_resolved"),
        (ErrorCode::HostBusy, "host_busy"),
        (ErrorCode::Internal, "internal"),
    ];
    for (code, wire) in codes {
        assert_eq!(serde_json::to_value(code).unwrap(), wire);
    }
    let result = CallToolResult::from(ToolError::new(
        ErrorCode::DepthExceeded,
        "children cannot spawn children",
    ));
    assert_eq!(
        serde_json::to_value(result).unwrap(),
        json!({
            "content": [{
                "type": "text",
                "text": r#"{"code":"depth_exceeded","message":"children cannot spawn children"}"#
            }],
            "isError": true
        })
    );
}

#[test]
fn host_busy_carries_its_retry_hint() {
    let error = ToolError::host_busy(30, "this machine is at capacity");
    assert_eq!(error.code, ErrorCode::HostBusy);
    assert_eq!(error.retry_after_secs, Some(30));
    assert_eq!(
        serde_json::to_value(CallToolResult::from(error.clone())).unwrap(),
        json!({
            "content": [{
                "type": "text",
                "text": r#"{"code":"host_busy","message":"this machine is at capacity","retry_after_secs":30}"#
            }],
            "isError": true
        })
    );
    let back: ToolError = serde_json::from_value(
        json!({ "code": "host_busy", "message": "this machine is at capacity", "retry_after_secs": 30 }),
    )
    .unwrap();
    assert_eq!(back, error);
    let plain: ToolError =
        serde_json::from_value(json!({ "code": "internal", "message": "no" })).unwrap();
    assert_eq!(plain.retry_after_secs, None);
}

#[test]
fn send_session_accepts_only_destination_text_and_delivery_key() {
    let args = json!({"session_id":"peer","text":"Review this","message_id":"review-1"});
    assert!(matches!(
        ToolCall::parse(Tool::SendSession, Some(args.clone())),
        Ok(ToolCall::SendSession(_))
    ));
    for (field, value) in [
        ("sender_session_id", json!("forged")),
        ("hop_count", json!(0)),
        ("permission_ceiling", json!("full_access")),
    ] {
        let mut forged = args.clone();
        forged[field] = value;
        assert_eq!(
            ToolCall::parse(Tool::SendSession, Some(forged))
                .unwrap_err()
                .code,
            ErrorCode::InvalidArguments
        );
    }
}

/// `tools/list` names the commands only; a call with a command's arguments wrong is answered
/// with that command's schema, so the agent learns them on demand.
#[test]
fn a_command_without_its_arguments_answers_with_its_schema() {
    let error = call(
        Tool::Command,
        json!({ "command": { "type": "rename_session" } }),
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArguments);
    let (_, schema) = error
        .message
        .split_once("the JSON Schema of `rename_session` is ")
        .unwrap();
    let schema: Value = serde_json::from_str(schema).unwrap();
    assert_eq!(schema["required"], json!(["type", "session_id", "title"]));

    let unknown = call(
        Tool::Command,
        json!({ "command": { "type": "format_disk" } }),
    )
    .unwrap_err();
    assert!(
        !unknown.message.contains("JSON Schema"),
        "{}",
        unknown.message
    );
}
