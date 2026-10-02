//! Drives the session manager with the fake adapter, the way clients will through the hub.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use herder_adapters::fake::FakeAdapter;
use herder_adapters::{Adapter, StartFuture, StartRequest};
use herder_daemon::session::{AccountConfig, Accounts, Adapters, EventSink, SessionManager, Setup};
use herder_daemon::worktree::Worktrees;
use herder_protocol::{
    AccountId, ApprovalDecision, ApprovalId, CommandBody, CommandResult, ErrorClass, ErrorCode,
    Event, EventBody, Item, ItemBody, ItemId, PermissionMode, Provider, SessionHead, SessionId,
    SessionStatus, TurnId, UserId,
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

/// The fake adapter, keeping every start request so tests can check the seed.
struct Recording {
    fake: FakeAdapter,
    starts: Arc<Mutex<Vec<StartRequest>>>,
}

impl Adapter for Recording {
    fn start(&self, request: StartRequest) -> StartFuture {
        self.starts.lock().unwrap().push(request.clone());
        self.fake.start(request)
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
    shutdown: CancellationToken,
}

impl Daemon {
    /// Opens a manager on `dir`'s database running `script`; `turns` numbers turn ids across
    /// restarts, as the fake scripts expect.
    async fn open(dir: &Path, script: &str, turns: Arc<AtomicU64>) -> Self {
        let (tx, seen) = mpsc::unbounded_channel();
        let starts = Arc::new(Mutex::new(Vec::new()));
        let mut adapters = Adapters::new();
        adapters.register(
            fake(),
            Arc::new(Recording {
                fake: FakeAdapter::new(fixture(script)),
                starts: starts.clone(),
            }),
        );
        let mut accounts = Accounts::new();
        accounts.insert(
            account(),
            AccountConfig {
                provider: fake(),
                config_dir: dir.join("account"),
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
    assert_eq!(start.config_dir, dir.path().join("account"));
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

#[tokio::test]
async fn approval_is_journaled_and_its_answer_reaches_the_adapter() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "approval.jsonl", Default::default()).await;
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "Run the tests.").await;
    daemon.until_status(SessionStatus::NeedsYou).await;

    let answer = |approval_id: &str| CommandBody::AnswerApproval {
        session_id: session.clone(),
        approval_id: ApprovalId::new(approval_id),
        decision: ApprovalDecision::Allow,
    };
    let unknown = daemon.manager.handle(bob(), answer("approval-9")).await;
    assert_eq!(unknown.unwrap_err().code, ErrorCode::NotFound);
    let result = daemon.manager.handle(bob(), answer("approval-1")).await;
    assert_eq!(result.unwrap(), CommandResult::Applied);
    daemon.until_status(SessionStatus::Idle).await;

    assert_eq!(
        describe(&daemon.journal(&session).await),
        [
            "alice: session_created",
            "-: status Running",
            "alice: user turn-1 Run the tests.",
            "-: turn_started turn-1",
            "-: tool_call Bash",
            "-: approval_requested approval-1",
            "-: status NeedsYou",
            "bob: approval_resolved approval-1 Allow",
            "-: status Running",
            "-: tool_result test result: ok",
            "-: turn_completed turn-1",
            "-: status Idle",
        ]
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
    let result = daemon.manager.archive(bob(), session.clone(), false).await;
    assert_eq!(result, Ok(CommandResult::Applied));
    let events = daemon.until_status(SessionStatus::Archived).await;
    assert_eq!(describe(&events).last().unwrap(), "bob: status Archived");
    assert!(!worktree.exists());
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
