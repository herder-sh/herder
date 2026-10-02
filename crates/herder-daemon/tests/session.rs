//! Drives the session manager with the fake adapter, the way clients will through the hub.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use herder_adapters::fake::FakeAdapter;
use herder_adapters::{Adapter, AdapterCommand, StartFuture, StartRequest};
use herder_daemon::session::{AccountConfig, Accounts, Adapters, EventSink, SessionManager, Setup};
use herder_daemon::worktree::Worktrees;
use herder_protocol::{
    AccountId, Answer, Answerer, ApprovalDecision, ApprovalId, CommandBody, CommandResult,
    ErrorClass, ErrorCode, ErrorInfo, Event, EventBody, Item, ItemBody, ItemId, PermissionMode,
    Provider, QuestionId, SessionHead, SessionId, SessionStatus, TurnId, UserId,
};
use herder_store::Store;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Something the manager published.
#[derive(Debug)]
enum Seen {
    Event(Event),
    Snapshot(Item),
    Delta(ItemId, String),
    Sessions(Vec<SessionHead>),
}

struct Recorder(mpsc::UnboundedSender<Seen>);

impl EventSink for Recorder {
    fn event(&self, event: &Event) {
        let _ = self.0.send(Seen::Event(event.clone()));
    }
    fn snapshot(&self, _: &SessionId, item: &Item) {
        let _ = self.0.send(Seen::Snapshot(item.clone()));
    }
    fn delta(&self, _: &SessionId, item_id: &ItemId, text: &str) {
        let _ = self.0.send(Seen::Delta(item_id.clone(), text.to_owned()));
    }
    fn sessions_changed(&self, sessions: &[SessionHead]) {
        let _ = self.0.send(Seen::Sessions(sessions.to_vec()));
    }
}

/// The fake adapter, keeping every start request so tests can check the seed, and every
/// command the daemon sent it.
struct Recording {
    fake: FakeAdapter,
    starts: Arc<Mutex<Vec<StartRequest>>>,
    commands: Arc<Mutex<Vec<AdapterCommand>>>,
}

impl Adapter for Recording {
    fn start(&self, request: StartRequest) -> StartFuture {
        self.starts.lock().unwrap().push(request.clone());
        let started = self.fake.start(request);
        let commands = self.commands.clone();
        Box::pin(async move {
            let mut session = started.await?;
            let (tx, mut rx) = mpsc::unbounded_channel();
            let fake = std::mem::replace(&mut session.commands, tx);
            tokio::spawn(async move {
                while let Some(command) = rx.recv().await {
                    commands.lock().unwrap().push(command.clone());
                    if fake.send(command).is_err() {
                        return;
                    }
                }
            });
            Ok(session)
        })
    }
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/session")
        .join(name)
}

fn fake() -> Provider {
    Provider::Other("fake".into())
}

fn account() -> AccountId {
    AccountId::new("account-1")
}

fn alice() -> UserId {
    UserId::new("alice")
}

fn bob() -> UserId {
    UserId::new("bob")
}

/// Runs git in `dir` with a fixed identity, panicking on failure; returns trimmed stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A daemon's session manager over a database that outlives it.
struct Daemon {
    manager: SessionManager,
    /// A git repository with one commit on `main`, for sessions to work on.
    repo: PathBuf,
    seen: mpsc::UnboundedReceiver<Seen>,
    /// Everything `events_until` has received so far.
    log: Vec<Seen>,
    starts: Arc<Mutex<Vec<StartRequest>>>,
    /// Every command the adapter received, across starts.
    commands: Arc<Mutex<Vec<AdapterCommand>>>,
    shutdown: CancellationToken,
}

impl Daemon {
    /// Opens a manager on `dir`'s database running `script`; `turns` numbers turn ids across
    /// restarts, as the fake scripts expect.
    async fn open(dir: &Path, script: &str, turns: Arc<AtomicU64>) -> Self {
        let (tx, seen) = mpsc::unbounded_channel();
        let starts = Arc::new(Mutex::new(Vec::new()));
        let commands = Arc::new(Mutex::new(Vec::new()));
        let mut adapters = Adapters::new();
        adapters.register(
            fake(),
            Arc::new(Recording {
                fake: FakeAdapter::new(fixture(script)),
                starts: starts.clone(),
                commands: commands.clone(),
            }),
        );
        let mut accounts = Accounts::new();
        accounts.insert(
            account(),
            AccountConfig {
                provider: fake(),
                label: "Account 1".into(),
                config_dir: Some(dir.join("account")),
                failover: false,
            },
        );
        let setup = Setup {
            store: Store::open(dir.join("herder.db")).unwrap(),
            adapters,
            accounts,
            sink: Arc::new(Recorder(tx)),
            turn_ids: Box::new(move || {
                TurnId::new(format!("turn-{}", turns.fetch_add(1, Ordering::SeqCst) + 1))
            }),
            worktrees: Worktrees::new(dir.join("worktrees")),
        };
        let shutdown = CancellationToken::new();
        let manager = SessionManager::open(setup, shutdown.clone()).await.unwrap();
        let repo = dir.join("app");
        if !repo.exists() {
            std::fs::create_dir(&repo).unwrap();
            git(&repo, &["init", "--quiet", "--initial-branch=main"]);
            git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "init"]);
        }
        Self {
            manager,
            repo,
            seen,
            log: Vec::new(),
            starts,
            commands,
            shutdown,
        }
    }

    async fn create(&self) -> SessionId {
        let repo = &self.repo;
        let result = self
            .manager
            .handle(
                alice(),
                CommandBody::CreateSession {
                    repo: repo.to_str().unwrap().to_owned(),
                    branch: None,
                    account_id: account(),
                    model: None,
                    permission_mode: PermissionMode::Ask,
                },
            )
            .await
            .unwrap();
        let CommandResult::SessionCreated { session_id } = result else {
            panic!("expected a created session, got {result:?}");
        };
        session_id
    }

    async fn prompt(&self, by: UserId, session_id: &SessionId, text: &str) {
        let command = CommandBody::SendPrompt {
            session_id: session_id.clone(),
            text: text.into(),
        };
        let result = self.manager.handle(by, command).await.unwrap();
        assert_eq!(result, CommandResult::Applied);
    }

    /// Published durable events up to and including the first `done` accepts.
    async fn events_until(&mut self, done: impl Fn(&EventBody) -> bool) -> Vec<Event> {
        let mut events = Vec::new();
        loop {
            let seen = tokio::time::timeout(Duration::from_secs(5), self.seen.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out; got {:#?}", describe(&events)))
                .unwrap();
            if let Seen::Event(event) = &seen {
                let stop = done(&event.body);
                events.push(event.clone());
                self.log.push(seen);
                if stop {
                    return events;
                }
            } else {
                self.log.push(seen);
            }
        }
    }

    async fn until_status(&mut self, status: SessionStatus) -> Vec<Event> {
        self.events_until(
            |body| matches!(body, EventBody::SessionStatusChanged { status: s } if *s == status),
        )
        .await
    }

    async fn answer(
        &self,
        by: UserId,
        session_id: &SessionId,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> Result<CommandResult, ErrorInfo> {
        let command = CommandBody::AnswerApproval {
            session_id: session_id.clone(),
            approval_id: ApprovalId::new(approval_id),
            decision,
        };
        self.manager.handle(by, command).await
    }

    /// The approval answers the adapter received.
    fn answers(&self) -> Vec<AdapterCommand> {
        let commands = self.commands.lock().unwrap();
        commands
            .iter()
            .filter(|command| matches!(command, AdapterCommand::AnswerApproval { .. }))
            .cloned()
            .collect()
    }

    async fn journal(&self, session_id: &SessionId) -> Vec<Event> {
        self.manager.read_since(session_id, 0, 1000).await.unwrap()
    }

    /// Stops the manager's sessions and gives their tasks a moment to exit.
    async fn stop(self) {
        self.shutdown.cancel();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// One line per event, `user: what`, so a history reads at a glance.
fn describe(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|event| {
            let what = match &event.body {
                EventBody::SessionCreated { .. } => "session_created".to_owned(),
                EventBody::SessionStatusChanged { status } => format!("status {status:?}"),
                EventBody::TurnStarted { turn_id } => format!("turn_started {turn_id}"),
                EventBody::TurnCompleted { turn_id } => format!("turn_completed {turn_id}"),
                EventBody::TurnInterrupted { turn_id } => format!("turn_interrupted {turn_id}"),
                EventBody::TurnFailed { turn_id, error } => {
                    format!("turn_failed {turn_id} {:?}", error.class)
                }
                EventBody::ItemAdded { item } => match &item.body {
                    ItemBody::UserMessage { text } => format!("user {} {text}", item.turn_id),
                    ItemBody::AssistantMessage { text } => {
                        format!("assistant {} {text}", item.turn_id)
                    }
                    ItemBody::ToolCall { name, .. } => format!("tool_call {name}"),
                    ItemBody::ToolResult { output, .. } => format!("tool_result {output}"),
                    other => format!("{other:?}"),
                },
                EventBody::ApprovalRequested { approval_id, .. } => {
                    format!("approval_requested {approval_id}")
                }
                EventBody::ApprovalResolved {
                    approval_id,
                    decision,
                    ..
                } => format!("approval_resolved {approval_id} {decision:?}"),
                EventBody::QuestionAsked {
                    question_id,
                    routed_to,
                    ..
                } => format!("question_asked {question_id} {routed_to:?}"),
                EventBody::QuestionAnswered {
                    question_id,
                    answer,
                    ..
                } => format!("question_answered {question_id} {answer:?}"),
                EventBody::BranchCheckedOut { branch } => format!("branch_checked_out {branch}"),
                EventBody::ModelSwitched { model } => format!("model_switched {model}"),
                other => format!("{other:?}"),
            };
            match &event.by {
                Some(user) => format!("{user}: {what}"),
                None => format!("-: {what}"),
            }
        })
        .collect()
}

fn seqs(events: &[Event]) -> Vec<u64> {
    events.iter().map(|event| event.seq).collect()
}

#[tokio::test]
async fn two_clients_prompting_one_session_share_one_ordered_history() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "two_prompts.jsonl", Default::default()).await;
    let session = daemon.create().await;

    // Bob's prompt arrives while Alice's turn is still running: it is queued, not refused.
    daemon.prompt(alice(), &session, "First.").await;
    daemon.prompt(bob(), &session, "Second.").await;

    let mut published = daemon.until_status(SessionStatus::Idle).await;
    let history = vec![
        "alice: session_created",
        "-: status Running",
        "alice: user turn-1 First.",
        "-: model_switched fake-model-1",
        "-: turn_started turn-1",
        "-: assistant turn-1 One.",
        "-: turn_completed turn-1",
        "bob: user turn-2 Second.",
        "-: turn_started turn-2",
        "-: assistant turn-2 Two.",
        "-: turn_completed turn-2",
        "-: status Idle",
    ];
    // Each client resumes from the journal; both see the same history, in seq order.
    let for_alice = daemon.journal(&session).await;
    let for_bob = daemon.journal(&session).await;
    assert_eq!(describe(&for_alice), history);
    assert_eq!(for_alice, for_bob);
    assert_eq!(seqs(&for_alice), (1..=12).collect::<Vec<_>>());
    // Live subscribers get exactly the stored events, in the same order.
    published.retain(|event| event.session_id == session);
    assert_eq!(published, for_alice);
}

#[tokio::test]
async fn streaming_items_publish_a_snapshot_then_deltas() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "two_prompts.jsonl", Default::default()).await;
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "First.").await;
    daemon.prompt(alice(), &session, "Second.").await;
    daemon.until_status(SessionStatus::Idle).await;

    // The new list follows the stored `session_created`.
    let Some(Seen::Sessions(heads)) = daemon.log.get(1) else {
        panic!(
            "creating a session publishes the session list: {:?}",
            daemon.log
        );
    };
    assert_eq!(heads.len(), 1);
    assert_eq!(heads[0].session_id, session);
    let streamed: Vec<_> = daemon
        .log
        .iter()
        .filter(|seen| matches!(seen, Seen::Snapshot(_) | Seen::Delta(..)))
        .collect();
    assert!(
        matches!(
            streamed.as_slice(),
            [Seen::Snapshot(item), Seen::Delta(id, text)]
                if item.id == ItemId::new("item-1") && *id == item.id && text == "One."
        ),
        "{streamed:?}"
    );
    // Deltas are never journaled; only the completed item is.
    let journal = describe(&daemon.journal(&session).await);
    assert_eq!(
        journal.iter().filter(|line| line.contains("One.")).count(),
        1
    );
}

#[tokio::test]
async fn restart_lists_sessions_and_resumes_seeded_from_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    let turns = Arc::new(AtomicU64::new(0));
    let mut daemon = Daemon::open(dir.path(), "first.jsonl", turns.clone()).await;
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "First.").await;
    daemon.until_status(SessionStatus::Idle).await;
    daemon.stop().await;

    let mut daemon = Daemon::open(dir.path(), "second.jsonl", turns).await;
    let heads = daemon.manager.sessions().await.unwrap();
    assert_eq!(
        heads,
        [SessionHead {
            session_id: session.clone(),
            head_seq: 7
        }]
    );
    // Nothing starts until the next prompt.
    assert!(daemon.starts.lock().unwrap().is_empty());

    daemon.prompt(bob(), &session, "Second.").await;
    daemon.until_status(SessionStatus::Idle).await;

    let starts = daemon.starts.lock().unwrap().clone();
    let [start] = starts.as_slice() else {
        panic!("expected one start, got {starts:?}");
    };
    let EventBody::SessionCreated { worktree, .. } = &daemon.journal(&session).await[0].body else {
        panic!("expected session_created");
    };
    assert_eq!(start.cwd, Path::new(worktree));
    assert_eq!(start.config_dir, Some(dir.path().join("account")));
    assert_eq!(start.permission_mode, PermissionMode::Ask);
    let seed: Vec<_> = start.seed.iter().map(|item| &item.body).collect();
    assert_eq!(
        seed,
        [
            &ItemBody::UserMessage {
                text: "First.".into()
            },
            &ItemBody::AssistantMessage {
                text: "One.".into()
            },
        ]
    );
    let journal = daemon.journal(&session).await;
    assert_eq!(seqs(&journal), (1..=13).collect::<Vec<_>>());
    assert_eq!(
        describe(&journal[7..]),
        [
            "-: status Running",
            "bob: user turn-2 Second.",
            "-: turn_started turn-2",
            "-: assistant turn-2 Two.",
            "-: turn_completed turn-2",
            "-: status Idle",
        ]
    );
}

#[tokio::test]
async fn restart_fails_a_turn_the_previous_daemon_left_open() {
    let dir = tempfile::tempdir().unwrap();
    let turns = Arc::new(AtomicU64::new(0));
    let mut daemon = Daemon::open(dir.path(), "interrupt.jsonl", turns.clone()).await;
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "Work forever.").await;
    daemon
        .events_until(|body| matches!(body, EventBody::TurnStarted { .. }))
        .await;
    daemon.stop().await;

    let daemon = Daemon::open(dir.path(), "interrupt.jsonl", turns).await;
    let journal = daemon.journal(&session).await;
    assert_eq!(
        describe(&journal[4..]),
        ["-: turn_failed turn-1 Transient", "-: status NeedsYou"]
    );
}

#[tokio::test]
async fn interrupt_stops_the_running_turn() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "interrupt.jsonl", Default::default()).await;
    let session = daemon.create().await;

    let interrupt = CommandBody::Interrupt {
        session_id: session.clone(),
    };
    let refused = daemon.manager.handle(bob(), interrupt.clone()).await;
    assert_eq!(refused.unwrap_err().code, ErrorCode::Conflict);

    daemon.prompt(alice(), &session, "Work forever.").await;
    daemon
        .events_until(|body| matches!(body, EventBody::TurnStarted { .. }))
        .await;
    let result = daemon.manager.handle(bob(), interrupt).await.unwrap();
    assert_eq!(result, CommandResult::Applied);
    let events = daemon.until_status(SessionStatus::Idle).await;
    assert_eq!(
        describe(&events),
        ["-: turn_interrupted turn-1", "-: status Idle"]
    );
}

#[tokio::test]
async fn limit_reached_fails_the_turn_and_needs_you() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "limit_reached.jsonl", Default::default()).await;
    let session = daemon.create().await;
    daemon
        .prompt(alice(), &session, "Refactor the parser.")
        .await;

    let events = daemon.until_status(SessionStatus::NeedsYou).await;
    let [.., failed, _] = events.as_slice() else {
        panic!("{events:?}");
    };
    let EventBody::TurnFailed { turn_id, error } = &failed.body else {
        panic!("expected turn_failed, got {failed:?}");
    };
    assert_eq!(*turn_id, TurnId::new("turn-1"));
    assert_eq!(error.class, ErrorClass::LimitReached);
    assert_eq!(error.message, "5-hour limit reached");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_answers_to_one_approval_apply_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "approval.jsonl", Default::default()).await;
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "Run the tests.").await;
    daemon.until_status(SessionStatus::NeedsYou).await;

    // Every client saw the request; each answers it at once, from its own task.
    let clients: Vec<UserId> = (0..8).map(|n| UserId::new(format!("user-{n}"))).collect();
    let answers: Vec<_> = clients
        .iter()
        .map(|user| {
            let manager = daemon.manager.clone();
            let command = CommandBody::AnswerApproval {
                session_id: session.clone(),
                approval_id: ApprovalId::new("approval-1"),
                decision: ApprovalDecision::Allow,
            };
            let user = user.clone();
            tokio::spawn(async move { (user.clone(), manager.handle(user, command).await) })
        })
        .collect();
    let mut applied = Vec::new();
    for answer in answers {
        match answer.await.unwrap() {
            (user, Ok(result)) => {
                assert_eq!(result, CommandResult::Applied);
                applied.push(user);
            }
            (_, Err(error)) => {
                assert_eq!(error.code, ErrorCode::Conflict);
                assert_eq!(error.message, "approval approval-1 is already resolved");
            }
        }
    }
    let [winner] = applied.as_slice() else {
        panic!("expected exactly one applied answer, got {applied:?}");
    };
    daemon.until_status(SessionStatus::Idle).await;

    let journal = daemon.journal(&session).await;
    let resolved: Vec<_> = journal
        .iter()
        .filter(|event| matches!(event.body, EventBody::ApprovalResolved { .. }))
        .collect();
    let [resolved] = resolved.as_slice() else {
        panic!("expected one approval_resolved, got {resolved:?}");
    };
    assert_eq!(resolved.by.as_ref(), Some(winner));
    assert_eq!(
        resolved.body,
        EventBody::ApprovalResolved {
            approval_id: ApprovalId::new("approval-1"),
            decision: ApprovalDecision::Allow,
            answered_by: Answerer::User,
        }
    );
    assert_eq!(
        describe(&journal)[5..],
        [
            "-: approval_requested approval-1",
            "-: status NeedsYou",
            &format!("{winner}: approval_resolved approval-1 Allow"),
            "-: status Running",
            "-: tool_result test result: ok",
            "-: turn_completed turn-1",
            "-: status Idle",
        ]
    );
    // Only the winning answer reached the agent.
    assert_eq!(
        daemon.answers(),
        [AdapterCommand::AnswerApproval {
            approval_id: ApprovalId::new("approval-1"),
            decision: ApprovalDecision::Allow,
        }]
    );
}

#[tokio::test]
async fn answers_to_unknown_or_resolved_approvals_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "approval.jsonl", Default::default()).await;
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "Run the tests.").await;
    daemon.until_status(SessionStatus::NeedsYou).await;

    let allow = ApprovalDecision::Allow;
    let unknown = daemon.answer(bob(), &session, "approval-9", allow).await;
    let unknown = unknown.unwrap_err();
    assert_eq!(unknown.code, ErrorCode::NotFound);
    assert_eq!(unknown.message, "approval approval-9 does not exist");
    let result = daemon.answer(bob(), &session, "approval-1", allow).await;
    assert_eq!(result, Ok(CommandResult::Applied));
    daemon.until_status(SessionStatus::Idle).await;
    // After the turn has ended too.
    let late = daemon.answer(alice(), &session, "approval-1", ApprovalDecision::Deny);
    assert_eq!(late.await.unwrap_err().code, ErrorCode::Conflict);
    assert_eq!(daemon.answers().len(), 1);
}

#[tokio::test]
async fn needs_you_holds_until_the_last_of_several_approvals_is_answered() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "two_approvals.jsonl", Default::default()).await;
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "Build and deploy.").await;
    daemon
        .events_until(
            |body| matches!(body, EventBody::ApprovalRequested { approval_id, .. } if approval_id.as_str() == "approval-2"),
        )
        .await;

    let first = daemon.answer(alice(), &session, "approval-1", ApprovalDecision::Allow);
    assert_eq!(first.await, Ok(CommandResult::Applied));
    let second = daemon.answer(bob(), &session, "approval-2", ApprovalDecision::Deny);
    assert_eq!(second.await, Ok(CommandResult::Applied));
    daemon.until_status(SessionStatus::Idle).await;

    assert_eq!(
        describe(&daemon.journal(&session).await)[3..],
        [
            "-: turn_started turn-1",
            "-: tool_call Bash",
            "-: approval_requested approval-1",
            "-: status NeedsYou",
            "-: tool_call Bash",
            // Already needs-you: the second request journals no status change.
            "-: approval_requested approval-2",
            "alice: approval_resolved approval-1 Allow",
            "-: tool_result built",
            "bob: approval_resolved approval-2 Deny",
            "-: status Running",
            "-: tool_result denied",
            "-: turn_completed turn-1",
            "-: status Idle",
        ]
    );
}

#[tokio::test]
async fn ending_a_turn_denies_its_open_approvals() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon =
        Daemon::open(dir.path(), "approval_interrupted.jsonl", Default::default()).await;
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "Run the tests.").await;
    daemon.until_status(SessionStatus::NeedsYou).await;

    let interrupt = CommandBody::Interrupt {
        session_id: session.clone(),
    };
    assert_eq!(
        daemon.manager.handle(bob(), interrupt).await,
        Ok(CommandResult::Applied)
    );
    let events = daemon.until_status(SessionStatus::Idle).await;
    assert_eq!(
        describe(&events),
        [
            "-: approval_resolved approval-1 Deny",
            "-: turn_interrupted turn-1",
            "-: status Idle",
        ]
    );
    let late = daemon.answer(alice(), &session, "approval-1", ApprovalDecision::Allow);
    assert_eq!(late.await.unwrap_err().code, ErrorCode::Conflict);
    assert!(daemon.answers().is_empty());
}

#[tokio::test]
async fn restart_denies_an_approval_the_previous_daemon_left_open() {
    let dir = tempfile::tempdir().unwrap();
    let turns = Arc::new(AtomicU64::new(0));
    let mut daemon = Daemon::open(dir.path(), "approval.jsonl", turns.clone()).await;
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "Run the tests.").await;
    daemon.until_status(SessionStatus::NeedsYou).await;
    daemon.stop().await;

    let daemon = Daemon::open(dir.path(), "approval.jsonl", turns).await;
    let journal = daemon.journal(&session).await;
    // Still needs-you: the turn failed. The denial carries no `by`: the daemon made it.
    assert_eq!(
        describe(&journal[5..]),
        [
            "-: approval_requested approval-1",
            "-: status NeedsYou",
            "-: approval_resolved approval-1 Deny",
            "-: turn_failed turn-1 Transient",
        ]
    );
    let late = daemon.answer(bob(), &session, "approval-1", ApprovalDecision::Allow);
    let late = late.await.unwrap_err();
    assert_eq!(late.code, ErrorCode::Conflict);
    assert!(daemon.starts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn question_is_journaled_for_the_user_and_its_answer_reaches_the_adapter() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "question.jsonl", Default::default()).await;
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "Add a migration.").await;
    daemon.until_status(SessionStatus::NeedsYou).await;

    let answer = |question_id: &str, answer: Answer| CommandBody::AnswerQuestion {
        session_id: session.clone(),
        question_id: QuestionId::new(question_id),
        answer,
    };
    let unknown = daemon
        .manager
        .handle(bob(), answer("question-9", Answer::Choice { index: 0 }))
        .await;
    assert_eq!(unknown.unwrap_err().code, ErrorCode::NotFound);
    let no_such_choice = daemon
        .manager
        .handle(bob(), answer("question-1", Answer::Choice { index: 2 }))
        .await;
    assert_eq!(no_such_choice.unwrap_err().code, ErrorCode::BadRequest);
    let result = daemon
        .manager
        .handle(bob(), answer("question-1", Answer::Choice { index: 1 }))
        .await;
    assert_eq!(result.unwrap(), CommandResult::Applied);
    daemon.until_status(SessionStatus::Idle).await;

    let journal = daemon.journal(&session).await;
    assert_eq!(
        describe(&journal),
        [
            "alice: session_created",
            "-: status Running",
            "alice: user turn-1 Add a migration.",
            "-: turn_started turn-1",
            "-: question_asked question-1 User",
            "-: status NeedsYou",
            "bob: question_answered question-1 Choice { index: 1 }",
            "-: status Running",
            "-: assistant turn-1 Done.",
            "-: turn_completed turn-1",
            "-: status Idle",
        ]
    );
    let EventBody::QuestionAsked { text, choices, .. } = &journal[4].body else {
        panic!("expected question_asked");
    };
    assert_eq!(
        (text.as_str(), choices.as_slice()),
        (
            "Which database?",
            &["SQLite".to_owned(), "Postgres".to_owned()][..]
        )
    );
}

#[tokio::test]
async fn a_branch_checked_out_during_a_session_is_journaled_when_the_turn_ends() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "two_prompts.jsonl", Default::default()).await;
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "First.").await;
    daemon.until_status(SessionStatus::Idle).await;
    let EventBody::SessionCreated {
        worktree, branch, ..
    } = &daemon.journal(&session).await[0].body
    else {
        panic!("expected session_created");
    };
    let (worktree, branch) = (PathBuf::from(worktree), branch.clone());
    // Seen live before any turn ends, and not journaled yet.
    git(&worktree, &["checkout", "--quiet", "-b", "spike"]);
    git(&worktree, &["checkout", "--quiet", &branch]);
    assert_eq!(
        daemon.manager.branches(&session).await.unwrap(),
        [branch.clone(), "spike".to_owned()]
    );

    daemon.prompt(alice(), &session, "Second.").await;
    daemon.until_status(SessionStatus::Idle).await;
    let journal = describe(&daemon.journal(&session).await);
    let tail = &journal[journal.len() - 4..];
    assert_eq!(
        tail,
        [
            "-: assistant turn-2 Two.",
            "-: turn_completed turn-2",
            "-: branch_checked_out spike",
            "-: status Idle",
        ]
    );
    // Once only: archiving reads the reflog again and finds nothing new.
    let archive = CommandBody::ArchiveSession {
        session_id: session.clone(),
        force: false,
    };
    daemon.manager.handle(alice(), archive).await.unwrap();
    let journal = describe(&daemon.journal(&session).await);
    assert_eq!(journal.last().unwrap(), "alice: status Archived");
    assert_eq!(
        journal
            .iter()
            .filter(|line| line.contains("branch_checked_out"))
            .count(),
        1
    );
}

#[tokio::test]
async fn commands_for_unknown_sessions_and_accounts_are_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path(), "first.jsonl", Default::default()).await;
    let prompt = CommandBody::SendPrompt {
        session_id: SessionId::new("nope"),
        text: "Hi.".into(),
    };
    let error = daemon.manager.handle(alice(), prompt).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);

    let create = CommandBody::CreateSession {
        repo: dir.path().to_str().unwrap().to_owned(),
        branch: None,
        account_id: AccountId::new("account-9"),
        model: None,
        permission_mode: PermissionMode::Ask,
    };
    let error = daemon.manager.handle(alice(), create).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn create_puts_the_session_on_its_own_worktree_and_branch() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "first.jsonl", Default::default()).await;
    let session = daemon.create().await;
    let slug = session.as_str()[18..].to_lowercase();
    let journal = daemon.journal(&session).await;
    let EventBody::SessionCreated {
        repo,
        worktree,
        branch,
        ..
    } = &journal[0].body
    else {
        panic!("expected session_created, got {:?}", journal[0].body);
    };
    assert_eq!(repo, daemon.repo.to_str().unwrap());
    assert_eq!(
        Path::new(worktree),
        dir.path().join(format!("worktrees/app-{slug}"))
    );
    assert_eq!(*branch, format!("herder/{slug}"));
    assert_eq!(
        git(Path::new(worktree), &["branch", "--show-current"]),
        *branch
    );

    // The agent runs in the worktree.
    daemon.prompt(alice(), &session, "First.").await;
    daemon.until_status(SessionStatus::Idle).await;
    assert_eq!(daemon.starts.lock().unwrap()[0].cwd, Path::new(worktree));

    git(Path::new(worktree), &["checkout", "--quiet", "-b", "spike"]);
    assert_eq!(
        daemon.manager.branches(&session).await.unwrap(),
        [branch.clone(), "spike".to_owned()]
    );
}

#[tokio::test]
async fn create_with_a_branch_name_uses_it_and_rejects_a_taken_one() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path(), "first.jsonl", Default::default()).await;
    let create = |branch: &str| CommandBody::CreateSession {
        repo: daemon.repo.to_str().unwrap().to_owned(),
        branch: Some(branch.to_owned()),
        account_id: account(),
        model: None,
        permission_mode: PermissionMode::Ask,
    };
    let result = daemon.manager.handle(alice(), create("fix/login")).await;
    let Ok(CommandResult::SessionCreated { session_id }) = result else {
        panic!("expected a created session, got {result:?}");
    };
    assert_eq!(
        daemon.manager.branches(&session_id).await.unwrap(),
        ["fix/login"]
    );
    let error = daemon
        .manager
        .handle(alice(), create("fix/login"))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    let error = daemon
        .manager
        .handle(alice(), create("bad..name"))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest);
}

#[tokio::test]
async fn archive_removes_the_worktree_keeps_the_branches_and_makes_the_session_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "first.jsonl", Default::default()).await;
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "First.").await;
    daemon.until_status(SessionStatus::Idle).await;
    let created = daemon.journal(&session).await;
    let EventBody::SessionCreated {
        worktree, branch, ..
    } = &created[0].body
    else {
        panic!("expected session_created");
    };
    let worktree = PathBuf::from(worktree);
    git(&worktree, &["checkout", "--quiet", "-b", "side"]);
    std::fs::write(worktree.join("draft.txt"), "wip").unwrap();

    let error = daemon
        .manager
        .archive(bob(), session.clone(), false)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert!(worktree.is_dir());

    std::fs::remove_file(worktree.join("draft.txt")).unwrap();
    let archive = CommandBody::ArchiveSession {
        session_id: session.clone(),
        force: false,
    };
    let result = daemon.manager.handle(bob(), archive).await;
    assert_eq!(result, Ok(CommandResult::Applied));
    let events = daemon.until_status(SessionStatus::Archived).await;
    assert_eq!(
        describe(&events)[events.len() - 2..],
        ["-: branch_checked_out side", "bob: status Archived"]
    );
    assert!(!worktree.exists());
    // The session still owns both once the worktree and its reflog are gone.
    assert_eq!(
        daemon.manager.branches(&session).await.unwrap(),
        [branch.clone(), "side".to_owned()]
    );
    let branches = git(&daemon.repo, &["branch", "--format=%(refname:short)"]);
    assert!(branches.lines().any(|line| line == branch), "{branches}");
    assert!(branches.lines().any(|line| line == "side"), "{branches}");

    let prompt = CommandBody::SendPrompt {
        session_id: session.clone(),
        text: "Again.".into(),
    };
    let error = daemon.manager.handle(alice(), prompt).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    let error = daemon
        .manager
        .archive(alice(), session.clone(), true)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);

    // Still archived and read-only after a restart.
    daemon.stop().await;
    let daemon = Daemon::open(dir.path(), "first.jsonl", Default::default()).await;
    let prompt = CommandBody::SendPrompt {
        session_id: session,
        text: "Again.".into(),
    };
    let error = daemon.manager.handle(alice(), prompt).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
}

#[tokio::test]
async fn terminals_get_the_worktree_until_the_session_is_archived() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path(), "first.jsonl", Default::default()).await;
    let session = daemon.create().await;
    let EventBody::SessionCreated { worktree, .. } = &daemon.journal(&session).await[0].body else {
        panic!("expected session_created");
    };
    assert_eq!(
        daemon.manager.worktree(&session).await.unwrap(),
        PathBuf::from(worktree)
    );
    let missing = SessionId::new("missing");
    let error = daemon.manager.worktree(&missing).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    daemon
        .manager
        .archive(alice(), session.clone(), false)
        .await
        .unwrap();
    let error = daemon.manager.worktree(&session).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
}

#[tokio::test]
async fn a_session_runs_on_its_accounts_provider_adapter_and_config_dir() {
    let dir = tempfile::tempdir().unwrap();
    let recording = || Recording {
        fake: FakeAdapter::new(fixture("first.jsonl")),
        starts: Default::default(),
        commands: Default::default(),
    };
    let (claude, codex) = (recording(), recording());
    let (claude_starts, codex_starts) = (claude.starts.clone(), codex.starts.clone());
    let mut adapters = Adapters::new();
    adapters.register(Provider::Claude, Arc::new(claude));
    adapters.register(Provider::Codex, Arc::new(codex));
    let codex_home = dir.path().join("codex-work");
    let accounts = Accounts::from([
        (
            AccountId::new("claude-main"),
            AccountConfig {
                provider: Provider::Claude,
                label: "Main".into(),
                config_dir: None,
                failover: false,
            },
        ),
        (
            AccountId::new("codex-work"),
            AccountConfig {
                provider: Provider::Codex,
                label: "Work".into(),
                config_dir: Some(codex_home.clone()),
                failover: true,
            },
        ),
    ]);
    let (tx, mut seen) = mpsc::unbounded_channel();
    let setup = Setup {
        store: Store::open(dir.path().join("herder.db")).unwrap(),
        adapters,
        accounts,
        sink: Arc::new(Recorder(tx)),
        turn_ids: Box::new(|| TurnId::new("turn-1")),
        worktrees: Worktrees::new(dir.path().join("worktrees")),
    };
    let shutdown = CancellationToken::new();
    let manager = SessionManager::open(setup, shutdown.clone()).await.unwrap();
    let repo = dir.path().join("app");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "init"]);

    let labels: Vec<_> = manager
        .accounts()
        .into_iter()
        .map(|account| {
            (
                account.account_id.to_string(),
                account.provider,
                account.label,
            )
        })
        .collect();
    assert_eq!(
        labels,
        [
            ("claude-main".into(), Provider::Claude, "Main".into()),
            ("codex-work".into(), Provider::Codex, "Work".into()),
        ]
    );

    for account in ["codex-work", "claude-main"] {
        let create = CommandBody::CreateSession {
            repo: repo.to_str().unwrap().to_owned(),
            branch: None,
            account_id: AccountId::new(account),
            model: None,
            permission_mode: PermissionMode::Ask,
        };
        let Ok(CommandResult::SessionCreated { session_id }) =
            manager.handle(alice(), create).await
        else {
            panic!("{account}: no session");
        };
        let prompt = CommandBody::SendPrompt {
            session_id: session_id.clone(),
            text: "First.".into(),
        };
        manager.handle(alice(), prompt).await.unwrap();
        loop {
            let next = tokio::time::timeout(Duration::from_secs(5), seen.recv());
            if let Seen::Event(event) = next.await.unwrap().unwrap()
                && event.session_id == session_id
                && matches!(event.body, EventBody::TurnCompleted { .. })
            {
                break;
            }
        }
    }

    let config_dirs = |starts: &Mutex<Vec<StartRequest>>| -> Vec<Option<PathBuf>> {
        starts
            .lock()
            .unwrap()
            .iter()
            .map(|start| start.config_dir.clone())
            .collect()
    };
    assert_eq!(config_dirs(&codex_starts), [Some(codex_home)]);
    assert_eq!(config_dirs(&claude_starts), [None]);
    shutdown.cancel();
}
