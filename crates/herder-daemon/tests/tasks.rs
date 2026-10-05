//! A primary session runs child sessions through the task tools, called over herder's MCP
//! server as the primary's CLI would.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use herder_adapters::{
    Adapter, AdapterCommand, AdapterEvent, AdapterSession, Capabilities, StartFuture, StartRequest,
};
use herder_daemon::mcp;
use herder_daemon::resources::{Admission, ReadHost, Reading, ResourcesConfig};
use herder_daemon::session::{
    AccountConfig, Accounts, Adapters, Escalation, EventSink, Notifier, SessionManager, Setup,
    TaskLimits, ulid_turn_ids,
};
use herder_daemon::worktree::Worktrees;
use herder_protocol::{
    Account, AccountId, Answer, Answerer, ApprovalDecision, ApprovalId, ApprovalOutcome,
    CommandBody, CommandResult, ErrorClass, ErrorCode, EscalationReason, Event, EventBody, Item,
    ItemBody, ItemId, PermissionMode, PromptId, Provider, QuestionId, Route, SessionHead,
    SessionId, SessionStatus, Timestamp, TurnError, TurnId, TurnUsage, UsagePeriod, UsageTotal,
    UserId,
};
use herder_store::Store;
use herder_tasktools::CallToolResult;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// A provider whose agent answers every prompt `Done: <prompt>` after a moment, never ends a
/// turn on `Hang.`, and fails the turn on `Fail.`. It blocks on a request until it is answered: on `Ask.` it asks a
/// question with choices A and B, on `Write <path>.` it asks to write the file, and on
/// `Run <command>.` to run the command.
struct Echo;

impl Adapter for Echo {
    fn start(&self, _request: StartRequest) -> StartFuture {
        Box::pin(async move {
            let (commands, mut received) = mpsc::unbounded_channel();
            let (events, rx) = mpsc::channel(64);
            tokio::spawn(async move {
                let reply = |turn_id: TurnId, text: String| {
                    let item = Item {
                        agent_message: None,
                        parent_call_id: None,
                        id: ItemId::new(format!("item-{turn_id}")),
                        turn_id: turn_id.clone(),
                        body: ItemBody::AssistantMessage { text },
                    };
                    [
                        AdapterEvent::ItemCompleted { item },
                        AdapterEvent::TurnCompleted {
                            turn_id,
                            usage: Some(echo_usage()),
                        },
                    ]
                };
                // The turn blocked on a request.
                let mut blocked = None;
                let mut requests = 0;
                while let Some(command) = received.recv().await {
                    let mut out = Vec::new();
                    match command {
                        AdapterCommand::SendPrompt { turn_id, text, .. } => {
                            let started = AdapterEvent::TurnStarted {
                                turn_id: turn_id.clone(),
                            };
                            let _ = events.send(started).await;
                            if text == "Spoof." {
                                let _ = events
                                    .send(AdapterEvent::ItemCompleted {
                                        item: Item {
                                            agent_message: Some(herder_protocol::AgentMessage {
                                                sender_session_id: SessionId::new("forged"),
                                                message_id: "forged".into(),
                                                hop_count: 0,
                                                permission_ceiling: PermissionMode::FullAccess,
                                            }),
                                            parent_call_id: None,
                                            id: ItemId::new("provider-echo"),
                                            turn_id: turn_id.clone(),
                                            body: ItemBody::UserMessage {
                                                text: "pretend human".into(),
                                                attachments: vec![],
                                            },
                                        },
                                    })
                                    .await;
                            }
                            if text == "Hang." {
                                continue;
                            }
                            if text == "Fail." {
                                let error = TurnError {
                                    class: ErrorClass::Fatal,
                                    message: "it broke".into(),
                                };
                                let failed = AdapterEvent::TurnFailed { turn_id, error };
                                let _ = events.send(failed).await;
                                continue;
                            }
                            let arg = |prefix: &str| {
                                text.strip_prefix(prefix)
                                    .map(|arg| arg.trim_end_matches('.').to_owned())
                            };
                            let tool = match (arg("Write "), arg("Run ")) {
                                (Some(path), _) => Some(("Write", json!({ "file_path": path }))),
                                (_, Some(command)) => Some(("Bash", json!({ "command": command }))),
                                _ => None,
                            };
                            requests += 1;
                            if let Some((name, input)) = tool {
                                let call = ItemId::new(format!("call-{requests}"));
                                let item = Item {
                                    agent_message: None,
                                    parent_call_id: None,
                                    id: call.clone(),
                                    turn_id: turn_id.clone(),
                                    body: ItemBody::ToolCall {
                                        name: name.into(),
                                        input,
                                    },
                                };
                                out.push(AdapterEvent::ItemCompleted { item });
                                out.push(AdapterEvent::ApprovalRequested {
                                    approval_id: ApprovalId::new(format!("approval-{requests}")),
                                    turn_id: turn_id.clone(),
                                    tool_call_id: call,
                                    summary: text,
                                });
                                blocked = Some(turn_id);
                            } else if text == "Ask." {
                                out.push(AdapterEvent::QuestionAsked {
                                    question_id: QuestionId::new(format!("question-{requests}")),
                                    turn_id: turn_id.clone(),
                                    text: "A or B?".into(),
                                    choices: vec!["A".into(), "B".into()],
                                });
                                blocked = Some(turn_id);
                            } else {
                                tokio::time::sleep(Duration::from_millis(100)).await;
                                out.extend(reply(turn_id, format!("Done: {text}")));
                            }
                        }
                        AdapterCommand::AnswerQuestion { answer, .. } => {
                            if let Some(turn_id) = blocked.take() {
                                let answer = serde_json::to_string(&answer).unwrap();
                                out.extend(reply(turn_id, format!("Answered: {answer}")));
                            }
                        }
                        AdapterCommand::AnswerApproval { decision, .. } => {
                            if let Some(turn_id) = blocked.take() {
                                out.extend(reply(turn_id, format!("Decided: {decision:?}")));
                            }
                        }
                        AdapterCommand::Shutdown => break,
                        _ => {}
                    }
                    for event in out {
                        let _ = events.send(event).await;
                    }
                }
                let _ = events.send(AdapterEvent::Exited { error: None }).await;
            });
            Ok(AdapterSession {
                capabilities: Capabilities {
                    native_model_switch: true,
                    native_permission_mode_switch: true,
                    reports_usage: false,
                    native_resume: false,
                },
                commands,
                events: rx,
            })
        })
    }
}

/// What every turn `Echo` completes reports using.
fn echo_usage() -> TurnUsage {
    TurnUsage {
        input: 100,
        output: 10,
        cache_read: 1_000,
        cache_write: 5,
        cost_usd: Some(0.25),
        cost_estimated: false,
    }
}

/// Every escalation the notifier was told about.
#[derive(Default)]
struct Recorder(Mutex<Vec<Escalation>>);

impl Notifier for Recorder {
    fn escalated(&self, escalation: &Escalation) {
        self.0.lock().unwrap().push(escalation.clone());
    }
}

impl Recorder {
    fn taken(&self) -> Vec<(SessionId, EscalationReason, Option<String>)> {
        std::mem::take(&mut *self.0.lock().unwrap())
            .into_iter()
            .map(|escalation| (escalation.child, escalation.reason, escalation.note))
            .collect()
    }
}

struct Quiet;

impl EventSink for Quiet {
    fn event(&self, _: &Event) {}
    fn snapshot(&self, _: &SessionId, _: &Item) {}
    fn delta(&self, _: &SessionId, _: &ItemId, _: &str) {}
    fn sessions_changed(&self, _: &[SessionHead]) {}
    fn accounts_changed(&self, _: &[Account]) {}
}

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

struct Daemon {
    manager: SessionManager,
    data_dir: PathBuf,
    repo: PathBuf,
    notifier: Arc<Recorder>,
    shutdown: CancellationToken,
}

impl Daemon {
    /// A daemon on `dir`, which a later daemon may open again.
    async fn open(dir: &Path) -> Self {
        Self::open_with(dir, TaskLimits::default()).await
    }

    /// A daemon on `dir` whose tasks have `limits`.
    async fn open_with(dir: &Path, limits: TaskLimits) -> Self {
        let mut adapters = Adapters::new();
        adapters.register(Provider::Other("echo".into()), Arc::new(Echo));
        let mut accounts = Accounts::new();
        accounts.insert(
            AccountId::new("account-1"),
            AccountConfig {
                provider: Provider::Other("echo".into()),
                label: "Account 1".into(),
                config_dir: None,
            },
        );
        let setup = Setup {
            store: Store::open(dir.join("herder.db")).unwrap(),
            adapters,
            accounts,
            sink: Arc::new(Quiet),
            turn_ids: ulid_turn_ids(),
            worktrees: Worktrees::new(dir.join("worktrees")),
            attachments: dir.join("attachments"),
        };
        let shutdown = CancellationToken::new();
        let manager = SessionManager::open(setup, shutdown.clone()).await.unwrap();
        let data_dir = dir.join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        manager
            .serve_mcp(
                mcp::Config {
                    data_dir: data_dir.clone(),
                    herder: PathBuf::from("/opt/herder"),
                },
                limits,
            )
            .unwrap();
        let notifier = Arc::new(Recorder::default());
        manager.notify_escalations(notifier.clone()).unwrap();
        let repo = dir.join("app");
        if !repo.exists() {
            std::fs::create_dir(&repo).unwrap();
            git(&repo, &["init", "--quiet", "--initial-branch=main"]);
            git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "init"]);
        }
        Self {
            manager,
            data_dir,
            repo,
            notifier,
            shutdown,
        }
    }

    /// A top-level session in `mode` whose CLI has started, so it holds an MCP token.
    async fn primary(&self, mode: PermissionMode) -> SessionId {
        self.primary_with(mode, None).await
    }

    /// A primary created with its own limit on children, as `primary`.
    async fn primary_with(&self, mode: PermissionMode, max_children: Option<u32>) -> SessionId {
        let session_id = self.create_with(mode, max_children).await;
        self.start(&session_id).await;
        session_id
    }

    async fn create_with(&self, mode: PermissionMode, max_children: Option<u32>) -> SessionId {
        let create = CommandBody::CreateSession {
            repo: Some(self.repo.to_str().unwrap().to_owned()),
            project_id: None,
            branch: None,
            account_id: Some(AccountId::new("account-1")),
            provider: None,
            model: Some("echo-1".into()),
            permission_mode: Some(mode),
            max_children,
            failover_pin: None,
        };
        let CommandResult::SessionCreated { session_id } =
            self.manager.handle(alice(), create).await.unwrap()
        else {
            panic!("expected a created session");
        };
        session_id
    }

    /// Runs a turn of `session_id`, which starts its CLI and grants it an MCP token.
    async fn start(&self, session_id: &SessionId) {
        let turns = |events: &[Event]| {
            events
                .iter()
                .filter(|event| matches!(event.body, EventBody::TurnCompleted { .. }))
                .count()
        };
        let before = turns(&self.journal(session_id).await);
        let prompt = CommandBody::SendPrompt {
            session_id: session_id.clone(),
            text: "Plan.".into(),
            images: Vec::new(),
        };
        self.manager.handle(alice(), prompt).await.unwrap();
        for _ in 0..250 {
            if turns(&self.journal(session_id).await) > before {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the turn did not complete");
    }

    async fn journal(&self, session_id: &SessionId) -> Vec<Event> {
        self.manager.read_since(session_id, 0, 1000).await.unwrap()
    }

    /// Waits for `session_id`'s journal to hold `count` events `matching`.
    async fn until_n(
        &self,
        session_id: &SessionId,
        count: usize,
        matching: fn(&EventBody) -> bool,
    ) {
        for _ in 0..250 {
            let journal = self.journal(session_id).await;
            if journal.iter().filter(|event| matching(&event.body)).count() >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("timed out; journal: {:#?}", self.journal(session_id).await);
    }

    async fn until(&self, session_id: &SessionId, matching: fn(&EventBody) -> bool) {
        self.until_n(session_id, 1, matching).await;
    }

    /// The worktree `session_id` was created with.
    async fn worktree(&self, session_id: &SessionId) -> PathBuf {
        match &self.journal(session_id).await[0].body {
            EventBody::SessionCreated { worktree, .. } => PathBuf::from(worktree),
            other => panic!("expected session_created, got {other:?}"),
        }
    }

    /// The latest status `session_id` journaled.
    async fn status(&self, session_id: &SessionId) -> SessionStatus {
        let journal = self.journal(session_id).await;
        journal
            .iter()
            .rev()
            .find_map(|event| match event.body {
                EventBody::SessionStatusChanged { status, .. } => Some(status),
                _ => None,
            })
            .unwrap_or(SessionStatus::Idle)
    }

    /// Whether `session_id` was ever `needs_you`.
    async fn ever_needed_you(&self, session_id: &SessionId) -> bool {
        let journal = self.journal(session_id).await;
        journal.iter().any(|event| needs_you(&event.body))
    }

    /// Alice answers through a client, as any user can.
    async fn user_answers(&self, command: CommandBody) {
        let result = self.manager.handle(alice(), command).await;
        assert_eq!(result, Ok(CommandResult::Applied));
    }

    /// An MCP client for `session`'s CLI, through the shim.
    fn connect(&self, session: &SessionId) -> Client {
        let (input, shim_input) = tokio::io::duplex(1 << 16);
        let (shim_output, output) = tokio::io::duplex(1 << 16);
        let (dir, session) = (self.data_dir.clone(), session.clone());
        let shim =
            tokio::spawn(async move { mcp::shim(&dir, &session, shim_input, shim_output).await });
        Client {
            input,
            output: BufReader::new(output),
            _shim: shim,
            next_id: 0,
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

struct Client {
    input: DuplexStream,
    output: BufReader<DuplexStream>,
    _shim: JoinHandle<anyhow::Result<()>>,
    next_id: u64,
}

impl Client {
    async fn call(&mut self, name: &str, arguments: Value) -> CallToolResult {
        self.next_id += 1;
        let id = self.next_id;
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        let mut line = request.to_string();
        line.push('\n');
        self.input.write_all(line.as_bytes()).await.unwrap();
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(10), self.output.read_line(&mut line))
            .await
            .expect("no answer in time")
            .unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], id, "{response}");
        serde_json::from_value(response["result"].clone()).unwrap()
    }

    /// A successful call's `structuredContent`.
    async fn ok(&mut self, name: &str, arguments: Value) -> Value {
        let result = self.call(name, arguments).await;
        assert!(!result.is_error, "{name}: {result:?}");
        result.structured_content.unwrap()
    }

    /// A failed call's error code.
    async fn fails(&mut self, name: &str, arguments: Value) -> String {
        let result = self.call(name, arguments).await;
        assert!(result.is_error, "{name}: {result:?}");
        let error: Value = serde_json::from_str(&result.content[0].text).unwrap();
        error["code"].as_str().unwrap().to_owned()
    }
}

fn alice() -> UserId {
    UserId::new("alice")
}

fn id(value: &Value) -> SessionId {
    SessionId::new(value.as_str().unwrap())
}

#[tokio::test]
async fn a_primary_spawns_two_children_and_gets_each_report_once() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).await;
    let primary = daemon.primary(PermissionMode::AutoEdit).await;
    let mut tools = daemon.connect(&primary);

    let a = tools
        .ok("spawn", json!({ "task": "Fix A", "prompt": "Do A." }))
        .await;
    let b = tools
        .ok("spawn", json!({ "task": "Fix B", "prompt": "Do B." }))
        .await;
    let (child_a, child_b) = (id(&a["child"]), id(&b["child"]));
    assert_ne!(a["branch"], b["branch"]);

    // Each child finished its turn with a clean worktree, so it is archived by the time its
    // report comes.
    let mut reports = BTreeSet::new();
    for _ in 0..2 {
        let event = tools.ok("wait_for", json!({ "timeout_secs": 10 })).await;
        assert_eq!(event["kind"], "report", "{event}");
        assert_eq!(event["status"], "archived", "{event}");
        reports.insert((
            event["child"].as_str().unwrap().to_owned(),
            event["summary"].as_str().unwrap().to_owned(),
        ));
    }
    assert_eq!(
        reports,
        BTreeSet::from([
            (child_a.to_string(), "Done: Do A.".to_owned()),
            (child_b.to_string(), "Done: Do B.".to_owned()),
        ])
    );
    // Each event once: nothing is left, and no child works.
    let event = tools.ok("wait_for", json!({ "timeout_secs": 10 })).await;
    assert_eq!(event, json!({ "kind": "idle" }));

    // The primary's journal records both spawns and both reports.
    let journal = daemon.journal(&primary).await;
    let spawned: Vec<_> = journal
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::ChildSpawned {
                child_session_id,
                task,
                ..
            } => Some((child_session_id.clone(), task.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        spawned,
        [
            (child_a.clone(), "Fix A".to_owned()),
            (child_b.clone(), "Fix B".to_owned())
        ]
    );
    let reported = journal
        .iter()
        .filter(|event| matches!(event.body, EventBody::ChildReported { .. }))
        .count();
    assert_eq!(reported, 2);

    // A child is a full session of its own, on the primary's settings, prompted by the agent.
    let child = daemon.journal(&child_a).await;
    let EventBody::SessionCreated {
        repo,
        worktree,
        branch,
        account_id,
        model,
        permission_mode,
        parent,
        task,
        ..
    } = &child[0].body
    else {
        panic!("expected session_created, got {:?}", child[0].body);
    };
    let branch = branch.as_ref().unwrap();
    assert_eq!(repo, daemon.repo.to_str().unwrap());
    // Archived: the worktree is gone, the branch kept.
    assert!(!Path::new(worktree).exists());
    let kept = format!("refs/heads/{branch}");
    git(&daemon.repo, &["show-ref", "--verify", "--quiet", &kept]);
    assert_eq!(branch, a["branch"].as_str().unwrap());
    assert_eq!(account_id, &AccountId::new("account-1"));
    assert_eq!(model, "echo-1");
    assert_eq!(*permission_mode, PermissionMode::AutoEdit);
    assert_eq!(parent.as_ref(), Some(&primary));
    assert_eq!(task.as_deref(), Some("Fix A"));
    assert_eq!(child[0].by, None);
    let prompt = child
        .iter()
        .find(|event| {
            matches!(&event.body, EventBody::ItemAdded { item }
                if matches!(&item.body, ItemBody::UserMessage { .. }))
        })
        .unwrap();
    assert_eq!(prompt.by, None);

    let status = tools.ok("status", json!({})).await;
    assert_eq!(
        status,
        json!({ "children": [
            {
                "child": child_a.as_str(),
                "task": "Fix A",
                "branch": a["branch"],
                "status": "archived",
                "last_report": "Done: Do A.",
                "open_questions": [],
            },
            {
                "child": child_b.as_str(),
                "task": "Fix B",
                "branch": b["branch"],
                "status": "archived",
                "last_report": "Done: Do B.",
                "open_questions": [],
            },
        ]})
    );
    let only_b = tools
        .ok("status", json!({ "children": [child_b.as_str()] }))
        .await;
    assert_eq!(only_b["children"].as_array().unwrap().len(), 1);
    assert_eq!(only_b["children"][0]["child"], child_b.as_str());
}

#[tokio::test]
async fn a_child_never_gets_a_permission_mode_above_its_primarys() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).await;
    let primary = daemon.primary(PermissionMode::Ask).await;
    let mut tools = daemon.connect(&primary);
    for mode in ["auto_edit", "full_access"] {
        let spawn = json!({ "task": "T", "prompt": "Do it.", "permission_mode": mode });
        assert_eq!(tools.fails("spawn", spawn).await, "not_allowed", "{mode}");
    }
    let other = json!({ "task": "T", "prompt": "Do it.", "provider": "codex" });
    assert_eq!(tools.fails("spawn", other).await, "not_allowed");
    // Nothing was created for the refused calls.
    assert_eq!(
        tools.ok("status", json!({})).await,
        json!({ "children": [] })
    );

    let lower = json!({ "task": "T", "prompt": "Do it.", "permission_mode": "read_only" });
    let child = id(&tools.ok("spawn", lower).await["child"]);
    let journal = daemon.journal(&child).await;
    assert!(matches!(
        journal[0].body,
        EventBody::SessionCreated {
            permission_mode: PermissionMode::ReadOnly,
            ..
        }
    ));
}

#[tokio::test]
async fn spawn_past_the_child_limit_is_refused_until_a_child_is_archived() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open_with(dir.path(), TaskLimits { max_children: 2 }).await;
    let primary = daemon.primary(PermissionMode::Ask).await;
    let mut tools = daemon.connect(&primary);
    // Children blocked on a question stay live.
    let ask = json!({ "task": "T", "prompt": "Ask." });
    let first = id(&tools.ok("spawn", ask.clone()).await["child"]);
    tools.ok("spawn", ask.clone()).await;
    for _ in 0..2 {
        let request = tools.ok("wait_for", json!({ "timeout_secs": 10 })).await;
        assert_eq!(request["kind"], "request");
    }

    let refused = tools.call("spawn", ask.clone()).await;
    assert!(refused.is_error, "{refused:?}");
    let error: Value = serde_json::from_str(&refused.content[0].text).unwrap();
    assert_eq!(error["code"], "limit_exceeded");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("its limit is 2"), "{message}");
    let status = tools.ok("status", json!({})).await;
    assert_eq!(status["children"].as_array().unwrap().len(), 2);

    // The limit is per task: another primary spawns freely, and one created with its own
    // limit keeps to that.
    let other = daemon.primary(PermissionMode::Ask).await;
    daemon.connect(&other).ok("spawn", ask.clone()).await;
    let own = daemon.primary_with(PermissionMode::Ask, Some(1)).await;
    let mut own_tools = daemon.connect(&own);
    own_tools.ok("spawn", ask.clone()).await;
    assert_eq!(
        own_tools.fails("spawn", ask.clone()).await,
        "limit_exceeded"
    );

    // Once answered, the first child finishes and is archived, which frees its slot by the
    // time its report comes.
    let answer = json!({ "child": first.as_str(), "question_id": "question-1", "choice": 0 });
    tools.ok("answer", answer).await;
    let wait = json!({ "child": first.as_str(), "timeout_secs": 10 });
    let report = tools.ok("wait_for", wait).await;
    assert_eq!(report["status"], "archived", "{report}");
    tools.ok("spawn", ask.clone()).await;
    assert_eq!(tools.fails("spawn", ask).await, "limit_exceeded");
}

/// A host with `GIB`s of memory available, which the test changes.
struct FakeHost(Arc<Mutex<u64>>);

const GIB: u64 = 1024 * 1024 * 1024;

impl ReadHost for FakeHost {
    fn read(&self) -> anyhow::Result<Reading> {
        Ok(Reading {
            memory_total: 16 * GIB,
            memory_available: *self.0.lock().unwrap() * GIB,
            load_1m: 0.5,
            cpu_percent: 10.0,
            pressure: None,
        })
    }
}

#[tokio::test]
async fn spawn_on_a_busy_host_is_refused_with_a_retry_hint() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).await;
    let available = Arc::new(Mutex::new(8));
    let config = ResourcesConfig::default();
    let admission = Admission::new(config.budget(8), Box::new(FakeHost(available.clone())));
    daemon.manager.admit_turns(Arc::new(admission)).unwrap();
    let primary = daemon.primary(PermissionMode::Ask).await;
    let mut tools = daemon.connect(&primary);
    let spawn = json!({ "task": "T", "prompt": "Do it." });

    *available.lock().unwrap() = 1;
    let refused = tools.call("spawn", spawn.clone()).await;
    assert!(refused.is_error, "{refused:?}");
    let error: Value = serde_json::from_str(&refused.content[0].text).unwrap();
    assert_eq!(error["code"], "host_busy");
    assert_eq!(error["retry_after_secs"], 30);
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("less than 2048 MiB"), "{message}");
    let status = tools.ok("status", json!({})).await;
    assert!(status["children"].as_array().unwrap().is_empty());

    *available.lock().unwrap() = 8;
    tools.ok("spawn", spawn).await;
    let report = tools.ok("wait_for", json!({ "timeout_secs": 10 })).await;
    assert_eq!(report["kind"], "report");
}

#[tokio::test]
async fn with_one_turn_allowed_a_primary_waiting_for_its_child_lets_the_child_run() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).await;
    let config = ResourcesConfig {
        max_turns: Some(1),
        ..ResourcesConfig::default()
    };
    let host = FakeHost(Arc::new(Mutex::new(8)));
    let admission = Arc::new(Admission::new(config.budget(8), Box::new(host)));
    daemon.manager.admit_turns(admission.clone()).unwrap();
    let primary = daemon.primary(PermissionMode::Ask).await;
    // The primary's turn runs on, holding the only slot, while it calls the task tools.
    let prompt = CommandBody::SendPrompt {
        session_id: primary.clone(),
        text: "Hang.".into(),
        images: Vec::new(),
    };
    daemon.manager.handle(alice(), prompt).await.unwrap();
    daemon
        .until_n(&primary, 2, |body| {
            matches!(body, EventBody::TurnStarted { .. })
        })
        .await;
    let mut tools = daemon.connect(&primary);
    let spawn = json!({ "task": "T", "prompt": "Do it." });

    // Only the turn limit binds, so the spawn succeeds and the child's turn waits.
    let child = id(&tools.ok("spawn", spawn.clone()).await["child"]);
    daemon
        .until(&child, |body| {
            matches!(
                body,
                EventBody::SessionStatusChanged {
                    retry_at: None,
                    status: SessionStatus::WaitingForCapacity
                }
            )
        })
        .await;
    assert_eq!(admission.resources().waiting_turns, 1);

    // Waiting for the child lends it the primary's slot; the primary takes it back after.
    let report = tools.ok("wait_for", json!({ "timeout_secs": 10 })).await;
    assert_eq!(report["kind"], "report", "{report}");
    assert_eq!(report["summary"], "Done: Do it.");
    let host = admission.resources();
    assert_eq!((host.running_turns, host.waiting_turns), (1, 0));

    // The primary goes on with its task the same way.
    tools.ok("spawn", spawn).await;
    let report = tools.ok("wait_for", json!({ "timeout_secs": 10 })).await;
    assert_eq!(report["summary"], "Done: Do it.", "{report}");
    assert_eq!(admission.resources().running_turns, 1);
}

#[tokio::test]
async fn concurrent_spawns_cannot_both_take_the_last_free_slot() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open_with(dir.path(), TaskLimits { max_children: 1 }).await;
    let primary = daemon.primary(PermissionMode::Ask).await;
    let (mut a, mut b) = (daemon.connect(&primary), daemon.connect(&primary));
    let spawn = json!({ "task": "T", "prompt": "Do it." });
    let (a, b) = tokio::join!(a.call("spawn", spawn.clone()), b.call("spawn", spawn));
    assert_ne!(a.is_error, b.is_error, "{a:?} {b:?}");
    let refused = if a.is_error { a } else { b };
    assert!(
        refused.content[0].text.contains("limit_exceeded"),
        "{refused:?}"
    );
}

#[tokio::test]
async fn a_child_cannot_spawn_and_reaches_only_its_own_children() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).await;
    let primary = daemon.primary(PermissionMode::Ask).await;
    let mut tools = daemon.connect(&primary);
    let child = id(&tools
        .ok("spawn", json!({ "task": "T", "prompt": "Ask." }))
        .await["child"]);
    let request = tools.ok("wait_for", json!({ "timeout_secs": 10 })).await;
    assert_eq!(request["kind"], "request");

    // The child's CLI has started, so it holds a token and can call the tools.
    let mut child_tools = daemon.connect(&child);
    let spawn = json!({ "task": "Deeper", "prompt": "Do more." });
    assert_eq!(child_tools.fails("spawn", spawn).await, "depth_exceeded");
    let send = json!({ "child": primary.as_str(), "text": "Hi." });
    assert_eq!(child_tools.fails("send", send).await, "not_your_child");
    assert_eq!(
        child_tools.ok("status", json!({})).await,
        json!({ "children": [] })
    );

    // Another task's child, and a session that does not exist, are out of reach too.
    let other = daemon.primary(PermissionMode::Ask).await;
    let mut other_tools = daemon.connect(&other);
    let send = json!({ "child": child.as_str(), "text": "Hi." });
    assert_eq!(other_tools.fails("send", send).await, "not_your_child");
    let wait = json!({ "child": child.as_str(), "timeout_secs": 1 });
    assert_eq!(other_tools.fails("wait_for", wait).await, "not_your_child");
    let status = json!({ "children": [child.as_str()] });
    assert_eq!(other_tools.fails("status", status).await, "not_your_child");
    let send = json!({ "child": "01NOSUCHSESSION", "text": "Hi." });
    assert_eq!(tools.fails("send", send).await, "not_found");
}

#[tokio::test]
async fn send_prompts_a_child_and_queues_behind_its_running_turn() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).await;
    let primary = daemon.primary(PermissionMode::Ask).await;
    let mut tools = daemon.connect(&primary);
    let child = id(&tools
        .ok("spawn", json!({ "task": "T", "prompt": "First." }))
        .await["child"]);

    // The first turn is still running, so the follow-up waits behind it.
    let send = json!({ "child": child.as_str(), "text": "Second." });
    assert_eq!(tools.ok("send", send).await, json!({ "queued": true }));
    let wait = json!({ "child": child.as_str(), "timeout_secs": 10 });
    let first = tools.ok("wait_for", wait.clone()).await;
    assert_eq!(first["summary"], "Done: First.");
    // The child went on with the queued prompt.
    assert_eq!(first["status"], "running");
    let second = tools.ok("wait_for", wait.clone()).await;
    assert_eq!(second["summary"], "Done: Second.");
    assert_eq!(second["status"], "archived");
    let worktree = daemon.worktree(&child).await;
    assert!(!worktree.exists());

    // A finished child is unarchived, back on its branch, and starts at once.
    let send = json!({ "child": child.as_str(), "text": "Third." });
    assert_eq!(tools.ok("send", send).await, json!({ "queued": false }));
    assert!(worktree.is_dir());
    let third = tools.ok("wait_for", wait).await;
    assert_eq!(third["summary"], "Done: Third.");
    assert_eq!(third["status"], "archived");
    let send = json!({ "child": child.as_str(), "text": "Hang." });
    assert_eq!(tools.ok("send", send).await, json!({ "queued": false }));
    let wait = json!({ "child": child.as_str(), "timeout_secs": 1 });
    assert_eq!(
        tools.ok("wait_for", wait).await,
        json!({ "kind": "timeout" })
    );
    daemon
        .until_n(&primary, 3, |body| {
            matches!(body, EventBody::ChildReported { .. })
        })
        .await;
}

#[tokio::test]
async fn children_that_have_not_finished_cleanly_and_primaries_stay_live() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).await;
    let primary = daemon.primary(PermissionMode::Ask).await;
    let mut tools = daemon.connect(&primary);

    // A child with an open question stays live.
    let dirty = id(&tools
        .ok("spawn", json!({ "task": "T", "prompt": "Ask." }))
        .await["child"]);
    let request = tools.ok("wait_for", json!({ "timeout_secs": 10 })).await;
    assert_eq!(request["kind"], "request");
    assert_eq!(daemon.status(&dirty).await, SessionStatus::Running);

    // Its turn ends with an untracked file in its worktree: it stays idle, and says why.
    let worktree = daemon.worktree(&dirty).await;
    std::fs::write(worktree.join("notes.txt"), "half done").unwrap();
    let answer = json!({ "child": dirty.as_str(), "question_id": "question-1", "choice": 0 });
    tools.ok("answer", answer).await;
    let report = tools.ok("wait_for", json!({ "timeout_secs": 10 })).await;
    assert_eq!(report["status"], "idle", "{report}");
    let summary = report["summary"].as_str().unwrap();
    assert!(summary.starts_with("Answered: "), "{summary}");
    assert!(
        summary.contains("uncommitted or untracked changes"),
        "{summary}"
    );
    assert!(worktree.join("notes.txt").is_file());
    assert_eq!(daemon.status(&dirty).await, SessionStatus::Idle);

    // A failed child stays live.
    let failed = id(&tools
        .ok("spawn", json!({ "task": "T", "prompt": "Fail." }))
        .await["child"]);
    let report = tools.ok("wait_for", json!({ "timeout_secs": 10 })).await;
    assert_eq!(report["child"], failed.as_str());
    assert_eq!(report["status"], "needs_you", "{report}");
    assert!(daemon.worktree(&failed).await.is_dir());

    // A primary's finished turns never archive it.
    daemon.start(&primary).await;
    daemon
        .until_n(&primary, 2, |body| {
            matches!(
                body,
                EventBody::SessionStatusChanged {
                    status: SessionStatus::Idle,
                    ..
                }
            )
        })
        .await;
    assert_eq!(daemon.status(&primary).await, SessionStatus::Idle);
    assert!(daemon.worktree(&primary).await.is_dir());
}

#[tokio::test]
async fn wait_for_filters_by_child_and_leaves_other_reports_waiting() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).await;
    let primary = daemon.primary(PermissionMode::Ask).await;
    let mut tools = daemon.connect(&primary);
    let a = id(&tools
        .ok("spawn", json!({ "task": "A", "prompt": "Do A." }))
        .await["child"]);
    let b = id(&tools
        .ok("spawn", json!({ "task": "B", "prompt": "Do B." }))
        .await["child"]);
    // Both have reported before anyone waits.
    daemon
        .until_n(&primary, 2, |body| {
            matches!(body, EventBody::ChildReported { .. })
        })
        .await;
    let wait_b = json!({ "child": b.as_str(), "timeout_secs": 5 });
    assert_eq!(
        tools.ok("wait_for", wait_b.clone()).await["child"],
        b.as_str()
    );
    assert_eq!(
        tools.ok("wait_for", wait_b).await,
        json!({ "kind": "idle" })
    );
    let any = tools.ok("wait_for", json!({ "timeout_secs": 5 })).await;
    assert_eq!(any["child"], a.as_str());
    let bad = json!({ "timeout_secs": 0 });
    assert_eq!(tools.fails("wait_for", bad).await, "invalid_arguments");
}

#[tokio::test]
async fn a_child_turn_cut_short_by_a_restart_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).await;
    let primary = daemon.primary(PermissionMode::Ask).await;
    let mut tools = daemon.connect(&primary);
    let child = id(&tools
        .ok("spawn", json!({ "task": "T", "prompt": "Hang." }))
        .await["child"]);
    daemon
        .until(&child, |body| matches!(body, EventBody::TurnStarted { .. }))
        .await;
    drop(tools);
    drop(daemon);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let daemon = Daemon::open(dir.path()).await;
    let journal = daemon.journal(&primary).await;
    let Some(EventBody::ChildReported {
        child_session_id,
        summary,
        ..
    }) = journal.last().map(|event| &event.body)
    else {
        panic!("expected child_reported, got {journal:#?}");
    };
    assert_eq!(child_session_id, &child);
    assert_eq!(
        summary,
        "The turn failed: the daemon stopped during this turn"
    );

    // The new daemon still hands the report to the primary's wait_for.
    daemon.start(&primary).await;
    let mut tools = daemon.connect(&primary);
    let report = tools.ok("wait_for", json!({ "timeout_secs": 5 })).await;
    assert_eq!(report["child"], child.as_str());
    assert_eq!(report["status"], "needs_you");
}

/// The turns with usage that `sessions` journaled.
async fn turns_with_usage(daemon: &Daemon, sessions: &[&SessionId]) -> u64 {
    let mut turns = 0;
    for session in sessions {
        let journal = daemon.journal(session).await;
        turns += journal
            .iter()
            .filter(|event| matches!(event.body, EventBody::TurnCompleted { usage: Some(_), .. }))
            .count() as u64;
    }
    turns
}

/// `by`'s usage summary for `period`, checking it starts where the period does.
async fn usage_summary(daemon: &Daemon, by: UserId, period: UsagePeriod) -> Vec<UsageTotal> {
    let before = period.start(Timestamp::now());
    let result = daemon
        .manager
        .handle(by, CommandBody::GetUsageSummary { period })
        .await
        .unwrap();
    let CommandResult::UsageSummary {
        period: answered,
        since,
        totals,
    } = result
    else {
        panic!("expected a usage summary, got {result:?}");
    };
    assert_eq!(answered, period);
    assert!(before <= since && since <= period.start(Timestamp::now()));
    totals
}

#[tokio::test]
async fn a_childs_turns_count_in_the_usage_summary_for_every_user_and_period_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).await;
    let primary = daemon.primary(PermissionMode::AutoEdit).await;
    let mut tools = daemon.connect(&primary);
    let child = id(&tools
        .ok("spawn", json!({ "task": "Fix A", "prompt": "Do A." }))
        .await["child"]);
    let report = tools.ok("wait_for", json!({ "timeout_secs": 10 })).await;
    assert_eq!(report["kind"], "report", "{report}");
    drop(tools);

    // The primary's turn and its child's, archived since, each counted once.
    let turns = turns_with_usage(&daemon, &[&primary, &child]).await;
    assert!(turns >= 2, "{turns} turns");
    let usage = echo_usage();
    let expected = [UsageTotal {
        account_id: AccountId::new("account-1"),
        provider: Provider::Other("echo".into()),
        model: "echo-1".into(),
        turns,
        input: usage.input * turns,
        output: usage.output * turns,
        cache_read: usage.cache_read * turns,
        cache_write: usage.cache_write * turns,
        cost_usd: 0.25 * turns as f64,
        cost_estimated: false,
    }];
    // Every user sees every session, so a member gets what an owner does.
    for by in [alice(), UserId::new("bob")] {
        for period in [
            UsagePeriod::Day,
            UsagePeriod::Week,
            UsagePeriod::ThirtyDays,
            UsagePeriod::Month,
        ] {
            assert_eq!(usage_summary(&daemon, by.clone(), period).await, expected);
        }
    }
    drop(daemon);
    tokio::time::sleep(Duration::from_millis(100)).await;

    // A restarted daemon answers from the stored history.
    let daemon = Daemon::open(dir.path()).await;
    assert_eq!(
        usage_summary(&daemon, alice(), UsagePeriod::ThirtyDays).await,
        expected
    );
}

/// The child's last journaled event matching `matching`.
/// Whether `body` sets the session `needs_you`.
fn needs_you(body: &EventBody) -> bool {
    *body
        == EventBody::SessionStatusChanged {
            retry_at: None,
            status: SessionStatus::NeedsYou,
        }
}

fn last<T>(journal: &[Event], matching: impl Fn(&Event) -> Option<T>) -> T {
    journal
        .iter()
        .rev()
        .find_map(matching)
        .unwrap_or_else(|| panic!("no such event in {journal:#?}"))
}

#[tokio::test]
async fn a_primary_answers_its_childrens_questions_and_approvals() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).await;
    let primary = daemon.primary(PermissionMode::AutoEdit).await;
    let mut tools = daemon.connect(&primary);
    let asks = id(&tools
        .ok("spawn", json!({ "task": "Ask", "prompt": "Ask." }))
        .await["child"]);
    let writes = id(&tools
        .ok(
            "spawn",
            json!({ "task": "Write", "prompt": "Write src/new.rs." }),
        )
        .await["child"]);

    // Both requests reach the primary, with the adapter's ids and the child that asked.
    let mut requests = Vec::new();
    for _ in 0..2 {
        let event = tools.ok("wait_for", json!({ "timeout_secs": 10 })).await;
        assert_eq!(event["kind"], "request", "{event}");
        requests.push(event);
    }
    requests.sort_by_key(|event| event["request"]["kind"].as_str().unwrap().to_owned());
    let (approval, question) = (&requests[0], &requests[1]);
    assert_eq!(approval["child"], writes.as_str());
    assert_eq!(
        approval["request"],
        json!({
            "kind": "approval",
            "approval_id": "approval-1",
            "summary": "Write src/new.rs.",
        })
    );
    assert_eq!(question["child"], asks.as_str());
    assert_eq!(
        question["request"],
        json!({
            "kind": "question",
            "question_id": "question-1",
            "text": "A or B?",
            "choices": ["A", "B"],
        })
    );
    // status lists them as open; the children work on, waiting on the primary.
    let status = tools.ok("status", json!({})).await;
    for child in status["children"].as_array().unwrap() {
        assert_eq!(child["status"], "running", "{child}");
        assert_eq!(
            child["open_questions"].as_array().unwrap().len(),
            1,
            "{child}"
        );
    }
    let journal = daemon.journal(&asks).await;
    let routed = last(&journal, |event| match &event.body {
        EventBody::QuestionAsked {
            routed_to, reason, ..
        } => Some((*routed_to, *reason)),
        _ => None,
    });
    assert_eq!(routed, (Route::Primary, None));

    // A wrong choice is the primary's to correct; then each answer unblocks its child.
    let wrong = json!({ "child": asks.as_str(), "question_id": "question-1", "choice": 2 });
    assert_eq!(tools.fails("answer", wrong).await, "invalid_arguments");
    let choose = json!({ "child": asks.as_str(), "question_id": "question-1", "choice": 1 });
    assert_eq!(tools.ok("answer", choose.clone()).await, json!({}));
    let allow =
        json!({ "child": writes.as_str(), "approval_id": "approval-1", "decision": "allow" });
    assert_eq!(tools.ok("answer", allow.clone()).await, json!({}));
    let mut reports = BTreeSet::new();
    for _ in 0..2 {
        let event = tools.ok("wait_for", json!({ "timeout_secs": 10 })).await;
        assert_eq!(event["kind"], "report", "{event}");
        reports.insert(event["summary"].as_str().unwrap().to_owned());
    }
    assert_eq!(
        reports,
        BTreeSet::from([
            r#"Answered: {"type":"choice","index":1}"#.to_owned(),
            "Decided: Allow".to_owned(),
        ])
    );

    // Journaled as the primary's answers, with no user.
    let primary_answered = Answerer::Primary {
        session_id: primary.clone(),
    };
    let journal = daemon.journal(&asks).await;
    let answered = last(&journal, |event| match &event.body {
        EventBody::QuestionAnswered {
            answer,
            answered_by,
            ..
        } => Some((event.by.clone(), answer.clone(), answered_by.clone())),
        _ => None,
    });
    assert_eq!(
        answered,
        (None, Answer::Choice { index: 1 }, primary_answered.clone())
    );
    let journal = daemon.journal(&writes).await;
    let resolved = last(&journal, |event| match &event.body {
        EventBody::ApprovalResolved {
            decision,
            answered_by,
            ..
        } => Some((event.by.clone(), *decision, answered_by.clone())),
        _ => None,
    });
    assert_eq!(resolved, (None, ApprovalOutcome::Allow, primary_answered));
    // Never the user's: no needs_you, no notification.
    assert!(!daemon.ever_needed_you(&asks).await);
    assert!(!daemon.ever_needed_you(&writes).await);
    assert!(daemon.notifier.taken().is_empty());

    // Answered once; unknown ids and other tasks' children are refused.
    assert_eq!(tools.fails("answer", choose).await, "already_resolved");
    assert_eq!(
        tools.fails("answer", allow.clone()).await,
        "already_resolved"
    );
    let unknown =
        json!({ "child": writes.as_str(), "approval_id": "approval-9", "decision": "deny" });
    assert_eq!(tools.fails("answer", unknown).await, "not_found");
    let bare = json!({ "approval_id": "approval-1", "decision": "deny" });
    assert_eq!(tools.fails("answer", bare).await, "invalid_arguments");
    let other = daemon.primary(PermissionMode::AutoEdit).await;
    let mut other_tools = daemon.connect(&other);
    assert_eq!(other_tools.fails("answer", allow).await, "not_your_child");
    let escalate = json!({ "child": asks.as_str(), "question_id": "question-1" });
    assert_eq!(
        other_tools.fails("escalate", escalate).await,
        "not_your_child"
    );
}

#[tokio::test]
async fn requests_the_primary_may_not_decide_or_escalates_go_to_the_user() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).await;
    let primary = daemon.primary(PermissionMode::FullAccess).await;
    let mut tools = daemon.connect(&primary);

    // A command needs full_access, and a write outside the worktree leaves it: both go
    // straight to the user, and the primary never sees them.
    for prompt in ["Run cargo test.", "Write ../outside.txt."] {
        let child = id(&tools
            .ok("spawn", json!({ "task": "T", "prompt": prompt }))
            .await["child"]);
        daemon
            .until(&child, |body| {
                matches!(body, EventBody::ApprovalRequested { .. })
            })
            .await;
        let journal = daemon.journal(&child).await;
        let routed = last(&journal, |event| match &event.body {
            EventBody::ApprovalRequested {
                routed_to, reason, ..
            } => Some((*routed_to, *reason)),
            _ => None,
        });
        assert_eq!(
            routed,
            (Route::User, Some(EscalationReason::ExceedsAuthority)),
            "{prompt}"
        );
        // Journaled after the request it settles on.
        daemon.until(&child, needs_you).await;
        assert_eq!(daemon.status(&child).await, SessionStatus::NeedsYou);
        // Lists show it on the child and rolled up on its primary.
        let heads = daemon.manager.sessions().await.unwrap();
        let head = |id: &SessionId| heads.iter().find(|head| head.session_id == *id).unwrap();
        assert_eq!(head(&primary).children_need_you, 1);
        let listed = head(&child);
        assert_eq!(
            (
                listed.status,
                listed.parent.as_ref(),
                listed.task.as_deref()
            ),
            (SessionStatus::NeedsYou, Some(&primary), Some("T"))
        );
        assert_eq!(
            daemon.notifier.taken(),
            [(child.clone(), EscalationReason::ExceedsAuthority, None)]
        );
        let wait = json!({ "child": child.as_str(), "timeout_secs": 1 });
        assert_eq!(
            tools.ok("wait_for", wait.clone()).await,
            json!({ "kind": "timeout" })
        );
        let status = json!({ "children": [child.as_str()] });
        assert_eq!(
            tools.ok("status", status).await["children"][0]["open_questions"],
            json!([])
        );
        let allow =
            json!({ "child": child.as_str(), "approval_id": "approval-1", "decision": "allow" });
        assert_eq!(tools.fails("answer", allow).await, "not_allowed");

        daemon
            .user_answers(CommandBody::AnswerApproval {
                session_id: child.clone(),
                approval_id: ApprovalId::new("approval-1"),
                decision: ApprovalDecision::Deny,
            })
            .await;
        let report = tools.ok("wait_for", wait).await;
        assert_eq!(report["summary"], "Decided: Deny");
    }

    // The primary hands a question on, with a note.
    let child = id(&tools
        .ok("spawn", json!({ "task": "Ask", "prompt": "Ask." }))
        .await["child"]);
    let request = tools.ok("wait_for", json!({ "timeout_secs": 10 })).await;
    let question_id = request["request"]["question_id"].clone();
    assert!(!daemon.ever_needed_you(&child).await);
    let escalate = json!({
        "child": child.as_str(),
        "question_id": question_id,
        "note": "This is a product call.",
    });
    assert_eq!(tools.ok("escalate", escalate.clone()).await, json!({}));
    let journal = daemon.journal(&child).await;
    let escalated = last(&journal, |event| match &event.body {
        EventBody::QuestionEscalated {
            question_id,
            reason,
            note,
        } => Some((question_id.clone(), *reason, note.clone())),
        _ => None,
    });
    let note = Some("This is a product call.".to_owned());
    assert_eq!(
        escalated,
        (
            QuestionId::new("question-1"),
            EscalationReason::MarkedByPrimary,
            note.clone()
        )
    );
    assert_eq!(daemon.status(&child).await, SessionStatus::NeedsYou);
    assert_eq!(
        daemon.notifier.taken(),
        [(child.clone(), EscalationReason::MarkedByPrimary, note)]
    );
    // It is the user's now.
    let choose = json!({ "child": child.as_str(), "question_id": question_id, "choice": 0 });
    assert_eq!(tools.fails("answer", choose).await, "not_allowed");
    assert_eq!(tools.fails("escalate", escalate).await, "not_allowed");
    daemon
        .user_answers(CommandBody::AnswerQuestion {
            session_id: child.clone(),
            question_id: QuestionId::new("question-1"),
            answer: Answer::Text { text: "B".into() },
        })
        .await;
    let report = tools.ok("wait_for", json!({ "timeout_secs": 10 })).await;
    assert_eq!(report["summary"], r#"Answered: {"type":"text","text":"B"}"#);
    assert_eq!(report["status"], "archived");
}

#[tokio::test]
async fn a_request_the_primary_leaves_unanswered_goes_to_the_user() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).await;
    let primary = daemon.primary(PermissionMode::AutoEdit).await;
    let mut tools = daemon.connect(&primary);
    let child = id(&tools
        .ok(
            "spawn",
            json!({ "task": "Write", "prompt": "Write notes.md." }),
        )
        .await["child"]);
    let request = tools.ok("wait_for", json!({ "timeout_secs": 10 })).await;
    assert_eq!(request["kind"], "request");

    // Just short of the timeout it still waits for the primary; past it, for the user.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(10 * 60 - 1)).await;
    tokio::time::resume();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!daemon.ever_needed_you(&child).await);
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(2)).await;
    tokio::time::resume();
    daemon
        .until(&child, |body| {
            matches!(
                body,
                EventBody::ApprovalEscalated {
                    reason: EscalationReason::Timeout,
                    note: None,
                    ..
                }
            )
        })
        .await;
    daemon.until(&child, needs_you).await;
    assert_eq!(daemon.status(&child).await, SessionStatus::NeedsYou);
    assert_eq!(
        daemon.notifier.taken(),
        [(child.clone(), EscalationReason::Timeout, None)]
    );
    let status = tools.ok("status", json!({})).await;
    assert_eq!(status["children"][0]["open_questions"], json!([]));
    let allow = json!({
        "child": child.as_str(),
        "approval_id": request["request"]["approval_id"],
        "decision": "allow",
    });
    assert_eq!(tools.fails("answer", allow).await, "not_allowed");

    daemon
        .user_answers(CommandBody::AnswerApproval {
            session_id: child.clone(),
            approval_id: ApprovalId::new("approval-1"),
            decision: ApprovalDecision::Allow,
        })
        .await;
    let report = tools.ok("wait_for", json!({ "timeout_secs": 10 })).await;
    assert_eq!(report["summary"], "Decided: Allow");
    let journal = daemon.journal(&child).await;
    let answered_by = last(&journal, |event| match &event.body {
        EventBody::ApprovalResolved { answered_by, .. } => {
            Some((event.by.clone(), answered_by.clone()))
        }
        _ => None,
    });
    assert_eq!(answered_by, (Some(alice()), Answerer::User));
}

#[tokio::test]
async fn a_user_answers_a_request_routed_to_the_primary_first() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).await;
    let primary = daemon.primary(PermissionMode::AutoEdit).await;
    let mut tools = daemon.connect(&primary);
    let child = id(&tools
        .ok("spawn", json!({ "task": "Ask", "prompt": "Ask." }))
        .await["child"]);
    daemon
        .until(&child, |body| {
            matches!(body, EventBody::QuestionAsked { .. })
        })
        .await;
    daemon
        .user_answers(CommandBody::AnswerQuestion {
            session_id: child.clone(),
            question_id: QuestionId::new("question-1"),
            answer: Answer::Choice { index: 0 },
        })
        .await;
    // The request left the primary's queue unseen: its next event is the report.
    let report = tools.ok("wait_for", json!({ "timeout_secs": 10 })).await;
    assert_eq!(report["kind"], "report", "{report}");
    let late = json!({ "child": child.as_str(), "question_id": "question-1", "text": "B" });
    assert_eq!(tools.fails("answer", late).await, "already_resolved");
    assert!(daemon.notifier.taken().is_empty());
}

fn agent_messages(events: &[Event]) -> Vec<(&herder_protocol::AgentMessage, &str)> {
    events
        .iter()
        .filter_map(|event| {
            if let EventBody::ItemAdded { item } = &event.body
                && let Some(message) = &item.agent_message
                && let ItemBody::UserMessage { text, .. } = &item.body
            {
                assert!(
                    event.by.is_none(),
                    "agent prompts must never be attributed to a human"
                );
                Some((message, text.as_str()))
            } else {
                None
            }
        })
        .collect()
}

#[tokio::test]
async fn independent_agent_messages_queue_deduplicate_and_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).await;
    let a = daemon.primary(PermissionMode::Ask).await;
    let b = daemon.primary(PermissionMode::Ask).await;
    daemon
        .user_answers(CommandBody::SendPrompt {
            session_id: b.clone(),
            text: "Hang.".into(),
            images: Vec::new(),
        })
        .await;
    daemon
        .until_n(&b, 2, |event| {
            matches!(event, EventBody::TurnStarted { .. })
        })
        .await;
    let message = json!({"session_id":b,"text":"Please review accounts.","message_id":"review-1"});
    let mut first = daemon.connect(&a);
    let mut second = daemon.connect(&a);
    let (one, two) = tokio::join!(
        first.ok("send_session", message.clone()),
        second.ok("send_session", message.clone())
    );
    assert_eq!(one["queued"], true);
    assert_eq!(two["queued"], true);
    assert_ne!(one["duplicate"], two["duplicate"]);
    let store = Store::open(dir.path().join("herder.db")).unwrap();
    let queued = store.queued_prompts(&b).unwrap();
    assert_eq!(queued.len(), 1);
    let metadata = queued[0].agent_message.as_ref().unwrap();
    assert_eq!(metadata.sender_session_id, a);
    assert_eq!(metadata.hop_count, 1);
    assert_eq!(metadata.permission_ceiling, PermissionMode::Ask);
    drop(store);
    drop(first);
    drop(second);
    drop(daemon);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let daemon = Daemon::open(dir.path()).await;
    daemon.manager.resume().await.unwrap();
    daemon
        .until_n(&b, 2, |event| {
            matches!(event, EventBody::TurnCompleted { .. })
        })
        .await;
    let events = daemon.journal(&b).await;
    let messages = agent_messages(&events);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].0.sender_session_id, a);
    assert_eq!(messages[0].1, "Please review accounts.");
    assert!(
        Store::open(dir.path().join("herder.db"))
            .unwrap()
            .session(&b)
            .unwrap()
            .unwrap()
            .parent
            .is_none()
    );
    daemon.start(&a).await;
    let mut tools = daemon.connect(&a);
    let duplicate = tools.ok("send_session", message.clone()).await;
    assert_eq!(duplicate["duplicate"], true);
    assert_eq!(agent_messages(&daemon.journal(&b).await).len(), 1);
    assert_eq!(
        tools
            .fails(
                "send_session",
                json!({"session_id":b,"text":"Different","message_id":"review-1"})
            )
            .await,
        "not_allowed"
    );
}

#[tokio::test]
async fn agent_messages_wait_in_the_queue_like_prompts_and_can_be_edited() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).await;
    let a = daemon.primary(PermissionMode::Ask).await;
    let b = daemon.primary(PermissionMode::Ask).await;
    daemon
        .user_answers(CommandBody::SendPrompt {
            session_id: b.clone(),
            text: "Hang.".into(),
            images: Vec::new(),
        })
        .await;
    daemon
        .until_n(&b, 1, |event| {
            matches!(event, EventBody::TurnStarted { .. })
        })
        .await;
    let mut tools = daemon.connect(&a);
    for (text, id) in [
        ("Review accounts.", "review-1"),
        ("Review billing.", "review-2"),
    ] {
        let message = json!({ "session_id": b, "text": text, "message_id": id });
        assert_eq!(tools.ok("send_session", message).await["queued"], true);
    }
    daemon
        .user_answers(CommandBody::SendPrompt {
            session_id: b.clone(),
            text: "Later.".into(),
            images: Vec::new(),
        })
        .await;
    let queue = async || {
        let heads = daemon.manager.sessions().await.unwrap();
        let head = heads.into_iter().find(|head| head.session_id == b).unwrap();
        head.queue
    };
    let queued = queue().await;
    let senders: Vec<_> = queued
        .iter()
        .map(|prompt| {
            let sender = prompt.agent_message.as_ref();
            (
                prompt.text.as_str(),
                sender.map(|m| &m.sender_session_id),
                prompt.by.as_ref(),
            )
        })
        .collect();
    assert_eq!(
        senders,
        [
            ("Review accounts.", Some(&a), None),
            ("Review billing.", Some(&a), None),
            ("Later.", None, Some(&alice())),
        ]
    );
    daemon
        .user_answers(CommandBody::MoveQueued {
            session_id: b.clone(),
            prompt_id: queued[2].prompt_id.clone(),
            before: Some(queued[0].prompt_id.clone()),
        })
        .await;
    daemon
        .user_answers(CommandBody::RemoveQueued {
            session_id: b.clone(),
            prompt_id: queued[1].prompt_id.clone(),
        })
        .await;
    let texts: Vec<_> = queue()
        .await
        .into_iter()
        .map(|prompt| prompt.text)
        .collect();
    assert_eq!(texts, ["Later.", "Review accounts."]);
    // The removed message keeps its receipt: resending it is a duplicate, not a new prompt.
    let resent = json!({ "session_id": b, "text": "Review billing.", "message_id": "review-2" });
    assert_eq!(tools.ok("send_session", resent).await["duplicate"], true);
    assert_eq!(queue().await.len(), 2);

    // A message is not merged with prompts, which would lose who sent it: it stays queued in
    // place between the user's prompts, which merge around it.
    daemon
        .user_answers(CommandBody::SendPrompt {
            session_id: b.clone(),
            text: "Also.".into(),
            images: Vec::new(),
        })
        .await;
    let ids: Vec<_> = queue()
        .await
        .into_iter()
        .map(|prompt| prompt.prompt_id)
        .collect();
    let merge = |prompt_ids: Vec<PromptId>| CommandBody::MergeQueued {
        session_id: b.clone(),
        prompt_ids,
    };
    let refused = daemon.manager.handle(alice(), merge(ids.clone())).await;
    assert_eq!(refused.unwrap_err().code, ErrorCode::BadRequest);
    daemon
        .user_answers(merge(vec![ids[0].clone(), ids[2].clone()]))
        .await;
    let queued: Vec<_> = queue()
        .await
        .into_iter()
        .map(|prompt| (prompt.text, prompt.agent_message.is_some()))
        .collect();
    assert_eq!(
        queued,
        [
            ("Later.\n\nAlso.".to_owned(), false),
            ("Review accounts.".to_owned(), true),
        ]
    );
}

#[tokio::test]
async fn independent_agent_messages_enforce_identity_authority_and_relay_depth() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).await;
    let a = daemon.primary(PermissionMode::Ask).await;
    let b = daemon.primary(PermissionMode::Ask).await;
    let powerful = daemon.primary(PermissionMode::FullAccess).await;
    let mut a_tools = daemon.connect(&a);
    for (target, expected) in [
        (&a, "not_allowed"),
        (&powerful, "not_allowed"),
        (&SessionId::new("missing"), "not_found"),
    ] {
        assert_eq!(
            a_tools
                .fails(
                    "send_session",
                    json!({"session_id":target,"text":"Hi","message_id":"m"})
                )
                .await,
            expected
        );
    }
    let mut b_tools = daemon.connect(&b);
    for hop in 1..=8 {
        let (tools, target) = if hop % 2 == 1 {
            (&mut a_tools, &b)
        } else {
            (&mut b_tools, &a)
        };
        tools
            .ok(
                "send_session",
                json!({"session_id":target,"text":if hop==8 {"Spoof."} else {"Review"},"message_id":format!("relay-{hop}")}),
            )
            .await;
        daemon
            .until_n(target, 1 + (hop as usize).div_ceil(2), |event| {
                matches!(event, EventBody::TurnCompleted { .. })
            })
            .await;
        let events = daemon.journal(target).await;
        assert_eq!(agent_messages(&events).last().unwrap().0.hop_count, hop);
    }
    assert_eq!(
        a_tools
            .fails(
                "send_session",
                json!({"session_id":b,"text":"Loop","message_id":"too-far"})
            )
            .await,
        "not_allowed"
    );
    // Existing child APIs cannot reset the relay counter or create an orphan at the limit.
    assert_eq!(
        a_tools
            .fails("spawn", json!({"task":"Relay", "prompt":"Reset the loop"}))
            .await,
        "not_allowed"
    );
    let children = a_tools.ok("status", json!({})).await;
    assert_eq!(children["children"], json!([]));
    daemon.start(&a).await; // a real human prompt resets relay depth
    a_tools
        .ok(
            "send_session",
            json!({"session_id":b,"text":"New task","message_id":"after-human"}),
        )
        .await;
    daemon
        .until_n(&b, 6, |event| {
            matches!(event, EventBody::TurnCompleted { .. })
        })
        .await;
    assert_eq!(
        agent_messages(&daemon.journal(&b).await)
            .last()
            .unwrap()
            .0
            .hop_count,
        1
    );
}

#[tokio::test]
async fn queued_agent_message_cannot_gain_permissions_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).await;
    let a = daemon.primary(PermissionMode::Ask).await;
    let b = daemon.primary(PermissionMode::Ask).await;
    daemon
        .user_answers(CommandBody::SendPrompt {
            session_id: b.clone(),
            text: "Hang.".into(),
            images: Vec::new(),
        })
        .await;
    daemon
        .until_n(&b, 2, |event| {
            matches!(event, EventBody::TurnStarted { .. })
        })
        .await;
    daemon
        .connect(&a)
        .ok(
            "send_session",
            json!({"session_id":b,"text":"Review","message_id":"permission-race"}),
        )
        .await;
    drop(daemon);
    tokio::time::sleep(Duration::from_millis(100)).await;
    // Emulate a separately persisted permission change before the pending message dispatches.
    let mut store = Store::open(dir.path().join("herder.db")).unwrap();
    store
        .append(herder_store::NewEvent {
            session_id: b.clone(),
            at: herder_protocol::Timestamp::now(),
            by: Some(alice()),
            body: EventBody::PermissionModeChanged {
                mode: PermissionMode::FullAccess,
            },
        })
        .unwrap();
    drop(store);
    let daemon = Daemon::open(dir.path()).await;
    daemon.manager.resume().await.unwrap();
    daemon
        .until_n(&b, 2, |event| matches!(event, EventBody::TurnFailed { .. }))
        .await;
    let events = daemon.journal(&b).await;
    assert_eq!(agent_messages(&events).len(), 1);
    assert!(events.iter().any(|event| matches!(&event.body, EventBody::TurnFailed { error, .. } if error.message.contains("authority"))));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.body, EventBody::TurnCompleted { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn agent_delivery_receipts_survive_archive_and_scope_keys_by_sender() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).await;
    let a = daemon.primary(PermissionMode::Ask).await;
    let c = daemon.primary(PermissionMode::Ask).await;
    let b = daemon.primary(PermissionMode::Ask).await;
    let admission = Admission::new(
        ResourcesConfig::default().budget(8),
        Box::new(FakeHost(Arc::new(Mutex::new(1)))),
    );
    daemon.manager.admit_turns(Arc::new(admission)).unwrap();
    let args = json!({"session_id":b,"text":"Review","message_id":"same-key"});
    for source in [&a, &c] {
        let result = daemon
            .connect(source)
            .ok("send_session", args.clone())
            .await;
        assert_eq!(result["duplicate"], false);
    }
    assert_eq!(
        Store::open(dir.path().join("herder.db"))
            .unwrap()
            .queued_prompts(&b)
            .unwrap()
            .len(),
        2
    );
    daemon
        .user_answers(CommandBody::ArchiveSession {
            session_id: b.clone(),
            force: true,
        })
        .await;
    drop(daemon);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let daemon = Daemon::open(dir.path()).await;
    daemon
        .user_answers(CommandBody::UnarchiveSession {
            session_id: b.clone(),
        })
        .await;
    daemon.start(&a).await;
    let result = daemon.connect(&a).ok("send_session", args).await;
    assert_eq!(result["duplicate"], true);
    assert_eq!(result["queued"], false);
    assert!(
        Store::open(dir.path().join("herder.db"))
            .unwrap()
            .queued_prompts(&b)
            .unwrap()
            .is_empty()
    );
    assert!(agent_messages(&daemon.journal(&b).await).is_empty());
}
