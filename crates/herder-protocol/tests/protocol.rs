//! Contract tests: every message round-trips through serde, matches the committed JSON Schema,
//! and the committed schema matches the Rust types.

use std::collections::BTreeSet;
use std::path::PathBuf;

use herder_protocol::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn at() -> Timestamp {
    "2026-10-02T12:00:00Z".parse().unwrap()
}

fn event(seq: Seq, by: Option<&str>, body: EventBody) -> ServerMessage {
    ServerMessage::Event(Event {
        session_id: SessionId::new("01J9SESSION"),
        seq,
        at: at(),
        by: by.map(UserId::new),
        body,
    })
}

fn item(body: ItemBody) -> Item {
    Item {
        id: ItemId::new("01J9ITEM"),
        turn_id: TurnId::new("01J9TURN"),
        body,
    }
}

fn pr(state: PrState, ci: CiStatus, review: ReviewStatus, mergeable: Mergeable) -> PullRequest {
    PullRequest {
        number: 42,
        url: "https://github.com/herder-sh/herder/pull/42".into(),
        title: "Add protocol".into(),
        head_branch: Some("p0-2-protocol".into()),
        state,
        ci,
        review,
        mergeable,
    }
}

fn command(body: CommandBody) -> ClientMessage {
    ClientMessage::Command(Command {
        id: CommandId::new("01J9COMMAND"),
        body,
    })
}

/// One message per variant of every client-sent type, and every value of its enums.
fn client_fixtures() -> Vec<ClientMessage> {
    let session_id = || SessionId::new("01J9SESSION");
    let terminal_id = || TerminalId::new("01J9TERMINAL");
    let mut messages = vec![
        ClientMessage::Hello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client: "herder-tui/0.0.0".into(),
            resume: vec![Cursor {
                session_id: session_id(),
                after_seq: 7,
            }],
            pairing_code: None,
        }),
        ClientMessage::Hello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client: "herder-ios/0.0.0".into(),
            resume: Vec::new(),
            pairing_code: Some("ABCDE-FGHJK".into()),
        }),
        ClientMessage::Subscribe(Cursor {
            session_id: session_id(),
            after_seq: 0,
        }),
        ClientMessage::Unsubscribe {
            session_id: session_id(),
        },
        command(CommandBody::CreateSession {
            repo: Some("/home/dev/herder".into()),
            project_id: None,
            branch: Some("p0-2-protocol".into()),
            account_id: Some(AccountId::new("01J9ACCOUNT")),
            provider: Some(Provider::Claude),
            model: Some("opus".into()),
            permission_mode: Some(PermissionMode::Ask),
            max_children: Some(3),
            failover_pin: Some(true),
        }),
        command(CommandBody::CreateSession {
            repo: None,
            project_id: Some(ProjectId::new("github.com/herder-sh/herder")),
            branch: None,
            account_id: None,
            provider: None,
            model: None,
            permission_mode: None,
            max_children: None,
            failover_pin: None,
        }),
        ClientMessage::Sync {
            token: "01J9SYNC".into(),
        },
        command(CommandBody::SendPrompt {
            session_id: session_id(),
            text: "Fix the build".into(),
            images: Vec::new(),
        }),
        command(CommandBody::SendPrompt {
            session_id: session_id(),
            text: "Match this mockup".into(),
            images: vec![Image {
                media_type: "image/png".into(),
                data: Bytes(b"\x89PNG\r\n\x1a\n".to_vec()),
            }],
        }),
        command(CommandBody::GetAttachment {
            session_id: session_id(),
            attachment_id: AttachmentId::new("01J9ATTACHMENT"),
        }),
        command(CommandBody::UnarchiveSession {
            session_id: session_id(),
        }),
        command(CommandBody::ListDirectory {
            path: "~/Projects".into(),
        }),
        command(CommandBody::AddProject {
            path: "/home/dev/herder".into(),
        }),
        command(CommandBody::SetProjectSettings {
            project_id: ProjectId::new("github.com/herder-sh/herder"),
            default_permission_mode: Some(PermissionMode::AutoEdit),
            default_account: Some(AccountId::new("01J9ACCOUNT")),
            setup_command: Some("cargo fetch".into()),
        }),
        command(CommandBody::SetProjectSettings {
            project_id: ProjectId::new("github.com/herder-sh/herder"),
            default_permission_mode: None,
            default_account: None,
            setup_command: None,
        }),
        command(CommandBody::RemoveProject {
            project_id: ProjectId::new("github.com/herder-sh/herder"),
        }),
        command(CommandBody::ArchiveSession {
            session_id: session_id(),
            force: false,
        }),
        command(CommandBody::ArchiveSession {
            session_id: session_id(),
            force: true,
        }),
        command(CommandBody::Interrupt {
            session_id: session_id(),
        }),
        command(CommandBody::SetModel {
            session_id: session_id(),
            model: "sonnet".into(),
        }),
        command(CommandBody::SwitchAccount {
            session_id: session_id(),
            account_id: AccountId::new("01J9ACCOUNT2"),
        }),
        command(CommandBody::SwitchProvider {
            session_id: session_id(),
            account_id: AccountId::new("01J9ACCOUNT3"),
            model: Some("gpt-5-codex".into()),
        }),
        command(CommandBody::LinkPr {
            session_id: session_id(),
            number: 42,
        }),
        command(CommandBody::UnlinkPr {
            session_id: session_id(),
            number: 42,
        }),
        command(CommandBody::ComposeDown {
            session_id: session_id(),
            project: "app".into(),
        }),
        command(CommandBody::OpenTerminal {
            session_id: session_id(),
            cols: 120,
            rows: 40,
        }),
        command(CommandBody::AddAccount {
            account_id: AccountId::new("claude-work"),
            provider: Provider::Claude,
            label: Some("Work".into()),
            config_dir: Some("~/.claude-work".into()),
            cols: 120,
            rows: 40,
        }),
        command(CommandBody::AttachTerminal {
            terminal_id: terminal_id(),
        }),
        command(CommandBody::DetachTerminal {
            terminal_id: terminal_id(),
        }),
        command(CommandBody::ResizeTerminal {
            terminal_id: terminal_id(),
            cols: 80,
            rows: 24,
        }),
        command(CommandBody::TerminalInput {
            terminal_id: terminal_id(),
            data: Bytes(b"ls\r\x1b[A\xff".to_vec()),
        }),
    ];
    for mode in [
        PermissionMode::ReadOnly,
        PermissionMode::Ask,
        PermissionMode::AutoEdit,
        PermissionMode::FullAccess,
    ] {
        messages.push(command(CommandBody::SetPermissionMode {
            session_id: session_id(),
            mode,
        }));
    }
    for answer in [
        Answer::Text {
            text: "Use the staging database".into(),
        },
        Answer::Choice { index: 1 },
    ] {
        messages.push(command(CommandBody::AnswerQuestion {
            session_id: session_id(),
            question_id: QuestionId::new("01J9QUESTION"),
            answer,
        }));
    }
    for decision in [ApprovalDecision::Allow, ApprovalDecision::Deny] {
        messages.push(command(CommandBody::AnswerApproval {
            session_id: session_id(),
            approval_id: ApprovalId::new("01J9APPROVAL"),
            decision,
        }));
    }
    messages
}

/// One message per variant of every daemon-sent type, and every value of its enums.
fn server_fixtures() -> Vec<ServerMessage> {
    let session_id = || SessionId::new("01J9SESSION");
    let account_id = || AccountId::new("01J9ACCOUNT");
    let turn_id = || TurnId::new("01J9TURN");
    let terminal_id = || TerminalId::new("01J9TERMINAL");
    let owner = Some("01J9OWNER");
    let mut messages = vec![
        ServerMessage::Hello(ServerHello {
            protocol_version: PROTOCOL_VERSION,
            host_id: HostId::new("01J9HOST"),
            host_name: "build-box".into(),
            user_id: UserId::new("01J9OWNER"),
            device_id: DeviceId::new("01J9DEVICE"),
            role: Role::Owner,
        }),
        ServerMessage::Sessions {
            sessions: vec![
                SessionHead {
                    session_id: session_id(),
                    host_id: None,
                    head_seq: 12,
                    status: SessionStatus::Running,
                    parent: None,
                    task: None,
                    project_id: Some(ProjectId::new("github.com/herder-sh/herder")),
                    account_id: account_id(),
                    children_need_you: 1,
                },
                SessionHead {
                    session_id: SessionId::new("01J9CHILD"),
                    host_id: Some(HostId::new("01J9HOST")),
                    head_seq: 4,
                    status: SessionStatus::NeedsYou,
                    parent: Some(session_id()),
                    task: Some("Fix the flaky auth tests".into()),
                    project_id: None,
                    account_id: account_id(),
                    children_need_you: 0,
                },
            ],
        },
        ServerMessage::Synced {
            token: "01J9SYNC".into(),
        },
        ServerMessage::Snapshot {
            session_id: session_id(),
            item: item(ItemBody::AssistantMessage {
                text: "Looking at".into(),
            }),
        },
        ServerMessage::Delta {
            session_id: session_id(),
            item_id: ItemId::new("01J9ITEM"),
            text: " the build".into(),
        },
        ServerMessage::TerminalOutput {
            terminal_id: terminal_id(),
            data: Bytes(b"\x1b[32mok\x1b[0m\r\n\xc3".to_vec()),
        },
        ServerMessage::TerminalClosed {
            terminal_id: terminal_id(),
            exit_code: Some(1),
        },
        ServerMessage::TerminalClosed {
            terminal_id: terminal_id(),
            exit_code: None,
        },
        ServerMessage::CommandAccepted {
            command_id: CommandId::new("01J9COMMAND"),
            result: CommandResult::Applied,
        },
        ServerMessage::CommandAccepted {
            command_id: CommandId::new("01J9COMMAND"),
            result: CommandResult::SessionCreated {
                session_id: session_id(),
            },
        },
        ServerMessage::CommandAccepted {
            command_id: CommandId::new("01J9COMMAND"),
            result: CommandResult::TerminalOpened {
                terminal_id: terminal_id(),
            },
        },
        ServerMessage::CommandAccepted {
            command_id: CommandId::new("01J9COMMAND"),
            result: CommandResult::Attachment {
                media_type: "image/png".into(),
                data: Bytes(b"\x89PNG\r\n\x1a\n".to_vec()),
            },
        },
        ServerMessage::CommandAccepted {
            command_id: CommandId::new("01J9COMMAND"),
            result: CommandResult::Directory {
                path: "/home/dev/Projects".into(),
                entries: vec![
                    DirectoryEntry {
                        name: "herder".into(),
                        is_dir: true,
                        is_repo: true,
                    },
                    DirectoryEntry {
                        name: "notes.md".into(),
                        is_dir: false,
                        is_repo: false,
                    },
                ],
            },
        },
        ServerMessage::CommandAccepted {
            command_id: CommandId::new("01J9COMMAND"),
            result: CommandResult::ProjectAdded {
                project_id: ProjectId::new("github.com/herder-sh/herder"),
            },
        },
        event(
            1,
            owner,
            EventBody::SessionCreated {
                repo: "/home/dev/herder".into(),
                worktree: "/home/dev/herder-p0-2-protocol".into(),
                branch: "p0-2-protocol".into(),
                provider: Provider::Claude,
                account_id: account_id(),
                model: "opus".into(),
                permission_mode: PermissionMode::Ask,
                parent: None,
                task: None,
                max_children: Some(3),
                failover_pin: Some(false),
            },
        ),
        event(2, owner, EventBody::TurnStarted { turn_id: turn_id() }),
        event(
            3,
            owner,
            EventBody::ItemAdded {
                item: item(ItemBody::UserMessage {
                    text: "Fix the build".into(),
                    attachments: vec![Attachment {
                        attachment_id: AttachmentId::new("01J9ATTACHMENT"),
                        media_type: "image/png".into(),
                        size: 8,
                    }],
                }),
            },
        ),
        event(
            4,
            None,
            EventBody::ItemAdded {
                item: item(ItemBody::Reasoning {
                    text: "The linker fails".into(),
                }),
            },
        ),
        event(
            5,
            None,
            EventBody::ItemAdded {
                item: item(ItemBody::ToolCall {
                    name: "Bash".into(),
                    input: json!({ "command": "cargo build", "timeout": 120 }),
                }),
            },
        ),
        event(
            6,
            None,
            EventBody::ApprovalRequested {
                approval_id: ApprovalId::new("01J9APPROVAL"),
                turn_id: turn_id(),
                tool_call_id: ItemId::new("01J9ITEM"),
                summary: "Run cargo build".into(),
                routed_to: Route::User,
                reason: None,
            },
        ),
        event(
            8,
            None,
            EventBody::ItemAdded {
                item: item(ItemBody::ToolResult {
                    call_id: ItemId::new("01J9ITEM"),
                    output: "Finished".into(),
                    is_error: false,
                }),
            },
        ),
        event(
            9,
            None,
            EventBody::ItemAdded {
                item: item(ItemBody::AssistantMessage {
                    text: "Fixed.".into(),
                }),
            },
        ),
        event(10, None, EventBody::TurnCompleted { turn_id: turn_id() }),
        event(11, owner, EventBody::TurnInterrupted { turn_id: turn_id() }),
        event(
            12,
            owner,
            EventBody::ModelSwitched {
                model: "sonnet".into(),
            },
        ),
        event(
            13,
            None,
            EventBody::AccountSwitched {
                account_id: account_id(),
            },
        ),
        event(
            14,
            owner,
            EventBody::ProviderSwitched {
                provider: Provider::Codex,
                account_id: account_id(),
                model: "gpt-5-codex".into(),
            },
        ),
        event(
            15,
            None,
            EventBody::PrLinked {
                pr: pr(
                    PrState::Draft,
                    CiStatus::None,
                    ReviewStatus::None,
                    Mergeable::Unknown,
                ),
            },
        ),
        event(16, owner, EventBody::PrUnlinked { number: 42 }),
        event(
            17,
            None,
            EventBody::BranchCheckedOut {
                branch: "herder/spike".into(),
            },
        ),
        ServerMessage::Terminals {
            terminals: vec![
                Terminal {
                    terminal_id: terminal_id(),
                    purpose: TerminalPurpose::Shell {
                        session_id: session_id(),
                    },
                },
                Terminal {
                    terminal_id: TerminalId::new("01J9TERMINAL2"),
                    purpose: TerminalPurpose::Login {
                        account_id: AccountId::new("claude-work"),
                    },
                },
            ],
        },
    ];
    for (by, decision) in [
        (owner, ApprovalOutcome::Allow),
        (owner, ApprovalOutcome::Deny),
        (None, ApprovalOutcome::Expired),
    ] {
        messages.push(event(
            7,
            by,
            EventBody::ApprovalResolved {
                approval_id: ApprovalId::new("01J9APPROVAL"),
                decision,
                answered_by: Answerer::User,
            },
        ));
    }
    messages.extend(task_fixtures());
    messages.extend(resource_fixtures());
    messages.extend(project_fixtures());
    messages.extend(fleet_fixtures());
    for status in [
        SessionStatus::Idle,
        SessionStatus::Running,
        SessionStatus::WaitingForCapacity,
        SessionStatus::NeedsYou,
        SessionStatus::Error,
        SessionStatus::Archived,
        SessionStatus::Moved,
    ] {
        messages.push(event(17, None, EventBody::SessionStatusChanged { status }));
    }
    for class in [
        ErrorClass::LimitReached,
        ErrorClass::Auth,
        ErrorClass::Transient,
        ErrorClass::Fatal,
    ] {
        messages.push(event(
            18,
            None,
            EventBody::TurnFailed {
                turn_id: turn_id(),
                error: TurnError {
                    class,
                    message: "provider said no".into(),
                },
            },
        ));
    }
    for mode in [
        PermissionMode::ReadOnly,
        PermissionMode::Ask,
        PermissionMode::AutoEdit,
        PermissionMode::FullAccess,
    ] {
        messages.push(event(19, owner, EventBody::PermissionModeChanged { mode }));
    }
    let pr_states = [
        pr(
            PrState::Open,
            CiStatus::Pending,
            ReviewStatus::Required,
            Mergeable::Clean,
        ),
        pr(
            PrState::Merged,
            CiStatus::Passing,
            ReviewStatus::Approved,
            Mergeable::Clean,
        ),
        pr(
            PrState::Closed,
            CiStatus::Failing,
            ReviewStatus::ChangesRequested,
            Mergeable::Conflicting,
        ),
    ];
    for pr in pr_states {
        messages.push(event(20, None, EventBody::PrUpdated { pr }));
    }
    messages.push(ServerMessage::Accounts {
        accounts: [
            Provider::Claude,
            Provider::Codex,
            Provider::Cursor,
            Provider::Grok,
            Provider::Opencode,
            Provider::Gemini,
            Provider::Other("aider".into()),
        ]
        .into_iter()
        .map(|provider| Account {
            account_id: account_id(),
            label: format!("{} work", provider.as_str()),
            provider,
            usage: vec![
                UsageWindow {
                    window: "five_hour".into(),
                    used_percent: 37.5,
                    resets_at: Some(at()),
                },
                UsageWindow {
                    window: "weekly".into(),
                    used_percent: 2.0,
                    resets_at: None,
                },
            ],
        })
        .collect(),
        failover: FailoverSettings { pin: true },
    });
    for code in [
        ErrorCode::BadRequest,
        ErrorCode::Forbidden,
        ErrorCode::NotFound,
        ErrorCode::Conflict,
        ErrorCode::Unsupported,
        ErrorCode::ReadOnly,
        ErrorCode::Internal,
    ] {
        let error = ErrorInfo {
            code,
            message: "no".into(),
        };
        messages.push(ServerMessage::CommandRejected {
            command_id: CommandId::new("01J9COMMAND"),
            error: error.clone(),
        });
        messages.push(ServerMessage::Error { error });
    }
    messages.push(ServerMessage::Hello(ServerHello {
        protocol_version: PROTOCOL_VERSION,
        host_id: HostId::new("01J9HOST"),
        host_name: "build-box".into(),
        user_id: UserId::new("01J9MEMBER"),
        device_id: DeviceId::new("01J9DEVICE"),
        role: Role::Member,
    }));
    messages
}

/// Host lists of a vault: one empty, one with an online and an offline host.
fn fleet_fixtures() -> Vec<ServerMessage> {
    vec![
        ServerMessage::Hosts { hosts: Vec::new() },
        ServerMessage::Hosts {
            hosts: vec![
                FleetHost {
                    host_id: HostId::new("01J9HOST"),
                    host_name: "devbox".into(),
                    online: true,
                    last_seen: at(),
                },
                FleetHost {
                    host_id: HostId::new("01J9HOST2"),
                    host_name: "laptop".into(),
                    online: false,
                    last_seen: at(),
                },
            ],
        },
    ]
}

/// Project lists: one empty, one with a remote project with settings and a local one without.
fn project_fixtures() -> Vec<ServerMessage> {
    vec![
        ServerMessage::Projects {
            projects: Vec::new(),
        },
        ServerMessage::Projects {
            projects: vec![
                Project {
                    project_id: ProjectId::new("github.com/herder-sh/herder"),
                    name: "herder".into(),
                    paths: vec!["/home/dev/herder".into(), "/srv/herder".into()],
                    default_permission_mode: Some(PermissionMode::AutoEdit),
                    default_account: Some(AccountId::new("01J9ACCOUNT")),
                    setup_command: Some("cargo fetch".into()),
                },
                Project {
                    project_id: ProjectId::local(&HostId::new("01J9HOST"), "/home/dev/scratch"),
                    name: "scratch".into(),
                    paths: vec!["/home/dev/scratch".into()],
                    default_permission_mode: None,
                    default_account: None,
                    setup_command: None,
                },
            ],
        },
    ]
}

/// Host and session resource messages, with every constraint and container state.
fn resource_fixtures() -> Vec<ServerMessage> {
    let host = |pressure, constraint| {
        ServerMessage::HostResources(HostResources {
            cpu_cores: 16,
            cpu_percent: 62.5,
            load_1m: 9.25,
            memory_total_bytes: 64 << 30,
            memory_available_bytes: 12 << 30,
            pressure,
            running_turns: 4,
            max_turns: 4,
            waiting_turns: 2,
            constraint,
        })
    };
    let pressure = || Pressure {
        cpu_some: 31.5,
        memory_some: 4.0,
        memory_full: 0.5,
        io_some: 12.25,
    };
    let mut messages = vec![host(None, None)];
    for constraint in [
        Constraint::MaxTurns,
        Constraint::Memory,
        Constraint::Load,
        Constraint::Pressure,
    ] {
        messages.push(host(Some(pressure()), Some(constraint)));
    }
    let containers = [
        ContainerState::Created,
        ContainerState::Running,
        ContainerState::Paused,
        ContainerState::Restarting,
        ContainerState::Removing,
        ContainerState::Exited,
        ContainerState::Dead,
    ]
    .into_iter()
    .enumerate()
    .map(|(n, state)| Container {
        id: format!("4f3c2b1a0e9d{n}"),
        name: format!("herder-p2c-1-db-{n}"),
        compose_project: (n % 2 == 0).then(|| "herder-p2c-1".to_owned()),
        image: "postgres:17".into(),
        state,
    })
    .collect();
    messages.push(ServerMessage::SessionResources {
        session_id: SessionId::new("01J9SESSION"),
        usage: SessionUsage {
            cpu_percent: 18.75,
            memory_bytes: 3 << 30,
            processes: 42,
            containers,
        },
    });
    messages.push(ServerMessage::SessionResources {
        session_id: SessionId::new("01J9SESSION"),
        usage: SessionUsage {
            cpu_percent: 0.0,
            memory_bytes: 0,
            processes: 0,
            containers: Vec::new(),
        },
    });
    messages
}

/// Events of a task: a child session, its parent's journal, and routed approvals and questions.
fn task_fixtures() -> Vec<ServerMessage> {
    let primary = || SessionId::new("01J9PRIMARY");
    let child = || SessionId::new("01J9SESSION");
    let turn_id = || TurnId::new("01J9TURN");
    let approval_id = || ApprovalId::new("01J9APPROVAL");
    let question_id = || QuestionId::new("01J9QUESTION");
    let owner = Some("01J9OWNER");
    let in_primary = |seq, body| {
        ServerMessage::Event(Event {
            session_id: primary(),
            seq,
            at: at(),
            by: None,
            body,
        })
    };
    let mut messages = vec![
        event(
            1,
            owner,
            EventBody::SessionCreated {
                repo: "/home/dev/herder".into(),
                worktree: "/home/dev/herder-p0-6-store".into(),
                branch: "p0-6-store".into(),
                provider: Provider::Claude,
                account_id: AccountId::new("01J9ACCOUNT"),
                model: "opus".into(),
                permission_mode: PermissionMode::AutoEdit,
                parent: Some(primary()),
                task: Some("Store migration".into()),
                max_children: None,
                failover_pin: None,
            },
        ),
        in_primary(
            4,
            EventBody::ChildSpawned {
                child_session_id: child(),
                task: "Store migration".into(),
            },
        ),
        in_primary(
            9,
            EventBody::ChildReported {
                child_session_id: child(),
                turn_id: turn_id(),
                summary: "Added the parent column.".into(),
            },
        ),
        event(
            2,
            None,
            EventBody::ApprovalRequested {
                approval_id: approval_id(),
                turn_id: turn_id(),
                tool_call_id: ItemId::new("01J9ITEM"),
                summary: "Run cargo test".into(),
                routed_to: Route::Primary,
                reason: None,
            },
        ),
        event(
            3,
            None,
            EventBody::ApprovalResolved {
                approval_id: approval_id(),
                decision: ApprovalOutcome::Allow,
                answered_by: Answerer::Primary {
                    session_id: primary(),
                },
            },
        ),
        event(
            4,
            None,
            EventBody::QuestionAsked {
                question_id: question_id(),
                turn_id: turn_id(),
                text: "Which database should the migration target?".into(),
                choices: vec![],
                routed_to: Route::Primary,
                reason: None,
            },
        ),
        event(
            5,
            None,
            EventBody::QuestionAnswered {
                question_id: question_id(),
                answer: Answer::Text {
                    text: "SQLite only".into(),
                },
                answered_by: Answerer::Primary {
                    session_id: primary(),
                },
            },
        ),
        event(
            6,
            None,
            EventBody::QuestionAsked {
                question_id: question_id(),
                turn_id: turn_id(),
                text: "Force-push the branch?".into(),
                choices: vec!["Yes".into(), "No".into()],
                routed_to: Route::User,
                reason: Some(EscalationReason::ExceedsAuthority),
            },
        ),
        event(
            7,
            owner,
            EventBody::QuestionAnswered {
                question_id: question_id(),
                answer: Answer::Choice { index: 1 },
                answered_by: Answerer::User,
            },
        ),
    ];
    for (reason, note) in [
        (
            EscalationReason::MarkedByPrimary,
            Some("This drops the **production** table; your call."),
        ),
        (EscalationReason::ExceedsAuthority, None),
        (EscalationReason::Timeout, None),
    ] {
        let note = note.map(str::to_owned);
        messages.push(event(
            8,
            None,
            EventBody::QuestionEscalated {
                question_id: question_id(),
                reason,
                note: note.clone(),
            },
        ));
        messages.push(event(
            8,
            None,
            EventBody::ApprovalEscalated {
                approval_id: approval_id(),
                reason,
                note,
            },
        ));
    }
    messages
}

fn assert_round_trips<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: &T) {
    let text = serde_json::to_string(value).unwrap();
    let back: T = serde_json::from_str(&text).unwrap();
    assert_eq!(&back, value, "round trip changed {text}");
}

#[test]
fn every_client_message_round_trips() {
    client_fixtures().iter().for_each(assert_round_trips);
}

#[test]
fn every_server_message_round_trips() {
    server_fixtures().iter().for_each(assert_round_trips);
}

fn assert_valid<T: Serialize>(schema: &schemars::Schema, messages: &[T]) {
    let validator = jsonschema::validator_for(schema.as_value()).unwrap();
    for message in messages {
        let value = serde_json::to_value(message).unwrap();
        let errors: Vec<String> = validator
            .iter_errors(&value)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{value} violates schema: {errors:?}");
    }
}

#[test]
fn every_message_matches_its_schema() {
    assert_valid(&client_schema(), &client_fixtures());
    assert_valid(&server_schema(), &server_fixtures());
}

/// Every `type` tag and enum string the schema allows, so a missing fixture fails the build.
fn schema_names(value: &Value, names: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(tag)) = map.get("const") {
                names.insert(tag.clone());
            }
            if let Some(Value::Array(values)) = map.get("enum") {
                names.extend(values.iter().filter_map(Value::as_str).map(str::to_owned));
            }
            map.values().for_each(|v| schema_names(v, names));
        }
        Value::Array(values) => values.iter().for_each(|v| schema_names(v, names)),
        _ => {}
    }
}

fn fixture_strings(value: &Value, strings: &mut BTreeSet<String>) {
    match value {
        Value::String(s) => {
            strings.insert(s.clone());
        }
        Value::Object(map) => map.values().for_each(|v| fixture_strings(v, strings)),
        Value::Array(values) => values.iter().for_each(|v| fixture_strings(v, strings)),
        _ => {}
    }
}

fn assert_covered<T: Serialize>(schema: &schemars::Schema, messages: &[T]) {
    let mut names = BTreeSet::new();
    schema_names(schema.as_value(), &mut names);
    let mut strings = BTreeSet::new();
    for message in messages {
        fixture_strings(&serde_json::to_value(message).unwrap(), &mut strings);
    }
    let missing: Vec<_> = names.difference(&strings).collect();
    assert!(missing.is_empty(), "no round-trip fixture for {missing:?}");
}

#[test]
fn fixtures_cover_every_variant_and_enum_value() {
    assert_covered(&client_schema(), &client_fixtures());
    assert_covered(&server_schema(), &server_fixtures());
}

fn assert_schema_snapshot(file: &str, schema: &schemars::Schema) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("schema")
        .join(file);
    let generated = serde_json::to_string_pretty(schema).unwrap() + "\n";
    if std::env::var_os("HERDER_UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &generated).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == generated,
        "schema/{file} is out of date with the Rust types. This is a protocol contract change: \
         review it, then run `HERDER_UPDATE_SCHEMA=1 cargo test -p herder-protocol` and commit the diff."
    );
}

#[test]
fn client_schema_matches_snapshot() {
    assert_schema_snapshot("client_message.json", &client_schema());
}

#[test]
fn server_schema_matches_snapshot() {
    assert_schema_snapshot("server_message.json", &server_schema());
}

#[test]
fn wire_shape_is_tagged_and_nested() {
    let message = event(
        3,
        Some("01J9OWNER"),
        EventBody::ItemAdded {
            item: item(ItemBody::UserMessage {
                text: "hi".into(),
                attachments: Vec::new(),
            }),
        },
    );
    assert_eq!(
        serde_json::to_value(&message).unwrap(),
        json!({
            "type": "event",
            "session_id": "01J9SESSION",
            "seq": 3,
            "at": "2026-10-02T12:00:00Z",
            "by": "01J9OWNER",
            "body": {
                "type": "item_added",
                "item": {
                    "id": "01J9ITEM",
                    "turn_id": "01J9TURN",
                    "body": { "type": "user_message", "text": "hi" }
                }
            }
        })
    );
    let input = command(CommandBody::TerminalInput {
        terminal_id: TerminalId::new("01J9TERMINAL"),
        data: Bytes(b"ls\r".to_vec()),
    });
    assert_eq!(
        serde_json::to_value(&input).unwrap()["body"]["data"],
        "bHMN"
    );
}

#[test]
fn unknown_tags_decode_to_unknown() {
    let message: ServerMessage =
        serde_json::from_value(json!({ "type": "future", "x": 1 })).unwrap();
    assert_eq!(message, ServerMessage::Unknown);

    let message: ServerMessage = serde_json::from_value(json!({
        "type": "event",
        "session_id": "s",
        "seq": 1,
        "at": "2026-10-02T12:00:00Z",
        "body": { "type": "future_event", "x": 1 },
        "future_field": true
    }))
    .unwrap();
    let ServerMessage::Event(event) = message else {
        panic!("expected an event");
    };
    assert_eq!(event.body, EventBody::Unknown);

    let body: EventBody = serde_json::from_value(json!({
        "type": "item_added",
        "item": { "id": "i", "turn_id": "t", "body": { "type": "image" } }
    }))
    .unwrap();
    assert_eq!(
        body,
        EventBody::ItemAdded {
            item: Item {
                id: ItemId::new("i"),
                turn_id: TurnId::new("t"),
                body: ItemBody::Unknown,
            }
        }
    );

    let status: SessionStatus = serde_json::from_value(json!("paused")).unwrap();
    assert_eq!(status, SessionStatus::Unknown);
}

#[test]
fn events_without_task_fields_decode_as_top_level_and_user_routed() {
    let body: EventBody = serde_json::from_value(json!({
        "type": "session_created",
        "repo": "/r",
        "worktree": "/w",
        "branch": "b",
        "provider": "claude",
        "account_id": "a",
        "model": "opus",
        "permission_mode": "ask"
    }))
    .unwrap();
    let EventBody::SessionCreated {
        parent,
        task,
        max_children,
        failover_pin,
        ..
    } = body
    else {
        panic!("expected session_created");
    };
    assert_eq!((parent, task), (None, None));
    assert_eq!((max_children, failover_pin), (None, None));

    let pr: PullRequest = serde_json::from_value(json!({
        "number": 1,
        "url": "u",
        "title": "t",
        "state": "open",
        "ci": "none",
        "review": "none",
        "mergeable": "unknown"
    }))
    .unwrap();
    assert_eq!(pr.head_branch, None);

    let body: EventBody = serde_json::from_value(json!({
        "type": "approval_requested",
        "approval_id": "ap",
        "turn_id": "t",
        "tool_call_id": "i",
        "summary": "Run it"
    }))
    .unwrap();
    let EventBody::ApprovalRequested {
        routed_to, reason, ..
    } = body
    else {
        panic!("expected approval_requested");
    };
    assert_eq!((routed_to, reason), (Route::User, None));

    let body: EventBody = serde_json::from_value(json!({
        "type": "approval_resolved",
        "approval_id": "ap",
        "decision": "allow"
    }))
    .unwrap();
    let EventBody::ApprovalResolved { answered_by, .. } = body else {
        panic!("expected approval_resolved");
    };
    assert_eq!(answered_by, Answerer::User);
}

#[test]
fn optional_fields_may_be_absent() {
    let body: EventBody = serde_json::from_value(json!({
        "type": "approval_escalated",
        "approval_id": "ap",
        "reason": "timeout"
    }))
    .unwrap();
    let EventBody::ApprovalEscalated { note, .. } = body else {
        panic!("expected approval_escalated");
    };
    assert_eq!(note, None);

    let body: EventBody = serde_json::from_value(json!({
        "type": "question_escalated",
        "question_id": "q",
        "reason": "timeout"
    }))
    .unwrap();
    let EventBody::QuestionEscalated { note, .. } = body else {
        panic!("expected question_escalated");
    };
    assert_eq!(note, None);

    let message: ClientMessage = serde_json::from_value(json!({
        "type": "hello",
        "protocol_version": PROTOCOL_VERSION,
        "client": "c",
        "resume": []
    }))
    .unwrap();
    let ClientMessage::Hello(hello) = message else {
        panic!("expected a hello");
    };
    assert_eq!(hello.pairing_code, None);

    let message: ServerMessage = serde_json::from_value(json!({
        "type": "terminal_closed",
        "terminal_id": "t"
    }))
    .unwrap();
    assert_eq!(
        message,
        ServerMessage::TerminalClosed {
            terminal_id: TerminalId::new("t"),
            exit_code: None
        }
    );
}

#[test]
fn resource_optional_fields_may_be_absent() {
    let message: ServerMessage = serde_json::from_value(json!({
        "type": "host_resources",
        "cpu_cores": 4,
        "cpu_percent": 10.0,
        "load_1m": 0.5,
        "memory_total_bytes": 1024,
        "memory_available_bytes": 512,
        "running_turns": 0,
        "max_turns": 1,
        "waiting_turns": 0
    }))
    .unwrap();
    let ServerMessage::HostResources(host) = message else {
        panic!("expected host_resources");
    };
    assert_eq!((host.pressure, host.constraint), (None, None));

    let container: Container = serde_json::from_value(json!({
        "id": "c",
        "name": "n",
        "image": "i",
        "state": "running"
    }))
    .unwrap();
    assert_eq!(container.compose_project, None);
}

#[test]
fn project_optional_fields_may_be_absent() {
    let message: ServerMessage = serde_json::from_value(json!({
        "type": "sessions",
        "sessions": [{
            "session_id": "s",
            "head_seq": 3,
            "status": "idle",
            "account_id": "a",
            "children_need_you": 0
        }]
    }))
    .unwrap();
    let ServerMessage::Sessions { sessions } = message else {
        panic!("expected sessions");
    };
    let head = &sessions[0];
    assert_eq!(
        (&head.project_id, &head.parent, &head.task),
        (&None, &None, &None)
    );

    let command: CommandBody = serde_json::from_value(json!({
        "type": "create_session",
        "repo": "/r"
    }))
    .unwrap();
    assert_eq!(
        command,
        CommandBody::CreateSession {
            repo: Some("/r".into()),
            project_id: None,
            branch: None,
            account_id: None,
            provider: None,
            model: None,
            permission_mode: None,
            max_children: None,
            failover_pin: None,
        }
    );

    let project: Project = serde_json::from_value(json!({
        "project_id": "github.com/org/repo",
        "name": "repo",
        "paths": ["/r"]
    }))
    .unwrap();
    assert_eq!(
        (
            project.default_permission_mode,
            project.default_account,
            project.setup_command
        ),
        (None, None, None)
    );

    let command: CommandBody = serde_json::from_value(json!({
        "type": "set_project_settings",
        "project_id": "github.com/org/repo"
    }))
    .unwrap();
    assert_eq!(
        command,
        CommandBody::SetProjectSettings {
            project_id: ProjectId::new("github.com/org/repo"),
            default_permission_mode: None,
            default_account: None,
            setup_command: None,
        }
    );

    let remove = CommandBody::RemoveProject {
        project_id: ProjectId::new("github.com/org/repo"),
    };
    assert_eq!(
        serde_json::to_value(&remove).unwrap(),
        json!({ "type": "remove_project", "project_id": "github.com/org/repo" })
    );
}

#[test]
fn unknown_is_never_sent() {
    assert!(serde_json::to_string(&ServerMessage::Unknown).is_err());
    assert!(serde_json::to_string(&EventBody::Unknown).is_err());
    assert!(serde_json::to_string(&ItemBody::Unknown).is_err());
    assert!(serde_json::to_string(&SessionStatus::Unknown).is_err());
}

#[test]
fn unknown_provider_keeps_its_name() {
    let provider: Provider = serde_json::from_value(json!("aider")).unwrap();
    assert_eq!(provider, Provider::Other("aider".into()));
    assert_eq!(serde_json::to_value(&provider).unwrap(), "aider");
    let provider: Provider = serde_json::from_value(json!("claude")).unwrap();
    assert_eq!(provider, Provider::Claude);
}

#[test]
fn invalid_base64_is_rejected() {
    let result: Result<Bytes, _> = serde_json::from_value(json!("not base64!"));
    assert!(result.is_err());
}

#[test]
fn add_account_may_omit_label_and_config_dir() {
    let body: CommandBody = serde_json::from_value(json!({
        "type": "add_account",
        "account_id": "codex-2",
        "provider": "codex",
        "cols": 80,
        "rows": 24
    }))
    .unwrap();
    assert_eq!(
        body,
        CommandBody::AddAccount {
            account_id: AccountId::new("codex-2"),
            provider: Provider::Codex,
            label: None,
            config_dir: None,
            cols: 80,
            rows: 24,
        }
    );
}

#[test]
fn prompts_without_images_keep_their_wire_shape() {
    let prompt = json!({ "type": "send_prompt", "session_id": "s", "text": "hi" });
    let body: CommandBody = serde_json::from_value(prompt.clone()).unwrap();
    assert_eq!(
        body,
        CommandBody::SendPrompt {
            session_id: SessionId::new("s"),
            text: "hi".into(),
            images: Vec::new(),
        }
    );
    assert_eq!(serde_json::to_value(&body).unwrap(), prompt);

    let message = json!({ "type": "user_message", "text": "hi" });
    let body: ItemBody = serde_json::from_value(message.clone()).unwrap();
    assert_eq!(
        body,
        ItemBody::UserMessage {
            text: "hi".into(),
            attachments: Vec::new(),
        }
    );
    assert_eq!(serde_json::to_value(&body).unwrap(), message);

    let image = Image {
        media_type: "image/png".into(),
        data: Bytes(b"\x89PNG".to_vec()),
    };
    assert_eq!(
        serde_json::to_value(&image).unwrap(),
        json!({ "media_type": "image/png", "data": "iVBORw==" })
    );
}

#[test]
fn terminal_purpose_is_tagged() {
    let terminal = Terminal {
        terminal_id: TerminalId::new("t"),
        purpose: TerminalPurpose::Login {
            account_id: AccountId::new("a"),
        },
    };
    assert_eq!(
        serde_json::to_value(&terminal).unwrap(),
        json!({"terminal_id": "t", "purpose": {"type": "login", "account_id": "a"}})
    );
}

fn summary(status: SessionStatus, prs: Vec<PullRequest>) -> SessionSummary {
    SessionSummary {
        session_id: SessionId::new("01J9SESSION"),
        project_id: ProjectId::new("github.com/herder-sh/herder"),
        repo: "/home/dev/herder".into(),
        branch: "herder/1a2b3c4d".into(),
        status,
        prs,
        parent: None,
        task: None,
        head_seq: 17,
        updated_at: at(),
    }
}

/// One message per variant of every host-sent replication type, and every value of its enums.
fn host_fixtures() -> Vec<HostMessage> {
    let mut messages = vec![
        HostMessage::Hello(HostHello {
            replication_version: REPLICATION_VERSION,
            host_id: HostId::new("01J9HOST"),
            host_name: "devbox".into(),
            build: "herder/0.0.0".into(),
            pairing_code: Some("483-921".into()),
        }),
        HostMessage::Hello(HostHello {
            replication_version: REPLICATION_VERSION,
            host_id: HostId::new("01J9HOST"),
            host_name: "devbox".into(),
            build: "herder/0.0.0".into(),
            pairing_code: None,
        }),
        HostMessage::Session(SessionSummary {
            parent: Some(SessionId::new("01J9PRIMARY")),
            task: Some("write the tests".into()),
            ..summary(SessionStatus::Running, Vec::new())
        }),
    ];
    let statuses = [
        SessionStatus::Idle,
        SessionStatus::WaitingForCapacity,
        SessionStatus::NeedsYou,
        SessionStatus::Error,
        SessionStatus::Archived,
        SessionStatus::Moved,
    ];
    messages.extend(
        statuses
            .into_iter()
            .map(|status| HostMessage::Session(summary(status, Vec::new()))),
    );
    let prs = vec![
        pr(
            PrState::Draft,
            CiStatus::None,
            ReviewStatus::None,
            Mergeable::Unknown,
        ),
        pr(
            PrState::Open,
            CiStatus::Pending,
            ReviewStatus::Required,
            Mergeable::Clean,
        ),
        pr(
            PrState::Merged,
            CiStatus::Passing,
            ReviewStatus::Approved,
            Mergeable::Conflicting,
        ),
        pr(
            PrState::Closed,
            CiStatus::Failing,
            ReviewStatus::ChangesRequested,
            Mergeable::Clean,
        ),
    ];
    messages.push(HostMessage::Session(summary(SessionStatus::Idle, prs)));
    let record = |seq, by: Option<&str>, body: Value| JournalRecord {
        seq,
        at: at(),
        by: by.map(UserId::new),
        body: RawEventBody::from_value(body).unwrap(),
    };
    messages.push(HostMessage::Batch(Batch {
        session_id: SessionId::new("01J9SESSION"),
        events: vec![
            record(
                3,
                Some("01J9OWNER"),
                json!({ "type": "turn_started", "turn_id": "01J9TURN" }),
            ),
            record(
                4,
                None,
                json!({ "type": "event_from_the_future", "detail": { "n": 1 } }),
            ),
        ],
    }));
    messages
}

/// One message per variant of every vault-sent replication type, and every value of its enums.
fn vault_fixtures() -> Vec<VaultMessage> {
    let cursor = |after_seq| Cursor {
        session_id: SessionId::new("01J9SESSION"),
        after_seq,
    };
    let mut messages = vec![
        VaultMessage::Hello(VaultHello {
            replication_version: REPLICATION_VERSION,
            build: "herder/0.0.0".into(),
            acked: vec![cursor(12)],
        }),
        VaultMessage::Ack(cursor(17)),
        VaultMessage::Rejected {
            cursor: cursor(12),
            reason: RejectReason::Gap,
        },
        VaultMessage::Rejected {
            cursor: cursor(12),
            reason: RejectReason::Conflict,
        },
    ];
    let codes = [
        ReplicationErrorCode::BadRequest,
        ReplicationErrorCode::Forbidden,
        ReplicationErrorCode::Internal,
    ];
    messages.extend(codes.into_iter().map(|code| VaultMessage::Error {
        error: ReplicationError {
            code,
            message: "detail".into(),
        },
    }));
    messages
}

#[test]
fn every_replication_message_round_trips() {
    host_fixtures().iter().for_each(assert_round_trips);
    vault_fixtures().iter().for_each(assert_round_trips);
}

#[test]
fn every_replication_message_matches_its_schema() {
    assert_valid(&host_schema(), &host_fixtures());
    assert_valid(&vault_schema(), &vault_fixtures());
}

#[test]
fn replication_fixtures_cover_every_variant_and_enum_value() {
    assert_covered(&host_schema(), &host_fixtures());
    assert_covered(&vault_schema(), &vault_fixtures());
}

#[test]
fn host_schema_matches_snapshot() {
    assert_schema_snapshot("host_message.json", &host_schema());
}

#[test]
fn vault_schema_matches_snapshot() {
    assert_schema_snapshot("vault_message.json", &vault_schema());
}

#[test]
fn journal_records_carry_bodies_as_stored() {
    let event = Event {
        session_id: SessionId::new("01J9SESSION"),
        seq: 3,
        at: at(),
        by: Some(UserId::new("01J9OWNER")),
        body: EventBody::ItemAdded {
            item: item(ItemBody::UserMessage {
                text: "hi".into(),
                attachments: Vec::new(),
            }),
        },
    };
    let record = JournalRecord::from_event(&event).unwrap();
    assert_eq!(
        serde_json::to_value(&record).unwrap(),
        json!({
            "seq": 3,
            "at": "2026-10-02T12:00:00Z",
            "by": "01J9OWNER",
            "body": serde_json::to_value(&event.body).unwrap(),
        })
    );
    assert_eq!(record.body.event_type(), "item_added");
    assert_eq!(record.to_event(SessionId::new("01J9SESSION")), event);
    assert!(
        JournalRecord::from_event(&Event {
            body: EventBody::Unknown,
            ..event
        })
        .is_err()
    );
}

#[test]
fn unknown_event_types_pass_through_verbatim() {
    let body = json!({ "type": "event_from_the_future", "detail": { "n": 1 } });
    let raw: RawEventBody = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(raw.event_type(), "event_from_the_future");
    assert_eq!(raw.decode(), EventBody::Unknown);
    assert_eq!(serde_json::to_value(&raw).unwrap(), body);
    // A known type whose shape changed is kept as well, and decodes like the store reads it.
    let changed = json!({ "type": "turn_started", "turn": 7 });
    let raw = RawEventBody::from_value(changed.clone()).unwrap();
    assert_eq!(raw.decode(), EventBody::Unknown);
    assert_eq!(serde_json::to_value(&raw).unwrap(), changed);
}

#[test]
fn event_bodies_without_a_string_type_are_rejected() {
    for body in [
        json!({ "kind": "x" }),
        json!({ "type": 1 }),
        json!("turn_started"),
    ] {
        assert!(serde_json::from_value::<RawEventBody>(body).is_err());
    }
}

#[test]
fn unknown_replication_tags_decode_to_unknown() {
    let host: HostMessage = serde_json::from_value(json!({ "type": "future", "x": 1 })).unwrap();
    assert_eq!(host, HostMessage::Unknown);
    let vault: VaultMessage = serde_json::from_value(json!({ "type": "future", "x": 1 })).unwrap();
    assert_eq!(vault, VaultMessage::Unknown);
    assert!(serde_json::to_string(&HostMessage::Unknown).is_err());
    assert!(serde_json::to_string(&VaultMessage::Unknown).is_err());
}

#[test]
fn replication_optional_fields_may_be_absent() {
    let hello: HostMessage = serde_json::from_value(json!({
        "type": "hello",
        "replication_version": REPLICATION_VERSION,
        "host_id": "01J9HOST",
        "host_name": "devbox",
        "build": "herder/0.0.0"
    }))
    .unwrap();
    let HostMessage::Hello(hello) = hello else {
        panic!("not a hello: {hello:?}")
    };
    assert_eq!(hello.pairing_code, None);
    let session: HostMessage = serde_json::from_value(json!({
        "type": "session",
        "session_id": "01J9SESSION",
        "project_id": "github.com/herder-sh/herder",
        "repo": "/home/dev/herder",
        "branch": "herder/1a2b3c4d",
        "status": "idle",
        "prs": [],
        "head_seq": 1,
        "updated_at": "2026-10-02T12:00:00Z"
    }))
    .unwrap();
    let HostMessage::Session(session) = session else {
        panic!("not a session: {session:?}")
    };
    assert_eq!((session.parent, session.task), (None, None));
}

/// The vault never writes to a host's sessions: nothing it can send carries an event or a command.
#[test]
fn vault_messages_carry_no_events_or_commands() {
    let schema = vault_schema();
    let defs = schema.as_value()["$defs"].as_object().unwrap();
    for absent in [
        "Event",
        "EventBody",
        "RawEventBody",
        "JournalRecord",
        "Batch",
        "Command",
    ] {
        assert!(!defs.contains_key(absent), "vault schema has {absent}");
    }
}
