//! Drives the session manager with the fake adapter, the way clients will through the hub.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use herder_adapters::acp::{AcpAdapter, AgentProfile};
use herder_adapters::fake::FakeAdapter;
use herder_adapters::{Adapter, AdapterCommand, StartFuture, StartRequest};
use herder_daemon::handoff;
use herder_daemon::projects::{Overrides, ProjectEntry, ProjectsConfig};
use herder_daemon::resources::{self, Admission, Host, ReadHost, Reading, ResourcesConfig, Scopes};
use herder_daemon::session::titles::INSTRUCTION;
use herder_daemon::session::{
    AccountConfig, Accounts, Adapters, EventSink, FailoverConfig, SessionManager, Setup,
    TaskLimits, TitleCli, TitleClis, TitlesConfig,
};
use herder_daemon::usage::{self, Probe, ProbeFuture, Probes};
use herder_daemon::worktree::{Worktrees, checkpoint};
use herder_protocol::{
    Account, AccountId, Answer, Answerer, ApprovalDecision, ApprovalId, ApprovalOutcome,
    Attachment, AttachmentId, Bytes, CommandBody, CommandId, CommandResult, Constraint, ErrorClass,
    ErrorCode, ErrorInfo, Event, EventBody, HostId, Image, Item, ItemBody, ItemId, MAX_TITLE_CHARS,
    PermissionMode, Project, ProjectId, PromptId, Provider, QuestionId, SessionHead, SessionId,
    SessionStatus, Timestamp, TitleSource, TurnError, TurnId, UsageWindow, UserId,
};
use herder_store::{NativeSession, Store};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Something the manager published.
#[derive(Debug)]
enum Seen {
    Event(Event),
    Snapshot(Item),
    Delta(ItemId, String),
    Sessions(Vec<SessionHead>),
    Accounts(Vec<Account>),
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
    fn accounts_changed(&self, accounts: &[Account]) {
        let _ = self.0.send(Seen::Accounts(accounts.to_vec()));
    }
}

/// An adapter, the fake one unless a test says otherwise, keeping every start request so tests
/// can check the seed, and every command the daemon sent it.
struct Recording {
    adapter: Box<dyn Adapter>,
    starts: Arc<Mutex<Vec<StartRequest>>>,
    commands: Arc<Mutex<Vec<AdapterCommand>>>,
    /// Held by a test, keeps the daemon's commands from reaching the CLI until released.
    gate: Gate,
}

type Gate = Arc<tokio::sync::Mutex<()>>;

impl Adapter for Recording {
    fn accepts_images(&self) -> bool {
        self.adapter.accepts_images()
    }

    fn start(&self, request: StartRequest) -> StartFuture {
        self.starts.lock().unwrap().push(request.clone());
        let started = self.adapter.start(request);
        let (commands, gate) = (self.commands.clone(), self.gate.clone());
        Box::pin(async move {
            let mut session = started.await?;
            let (tx, mut rx) = mpsc::unbounded_channel();
            let fake = std::mem::replace(&mut session.commands, tx);
            tokio::spawn(async move {
                while let Some(command) = rx.recv().await {
                    drop(gate.lock().await);
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
    gate: Gate,
    shutdown: CancellationToken,
}

impl Daemon {
    /// Opens a manager on `dir`'s database running `script`; `turns` numbers turn ids across
    /// restarts, as the fake scripts expect.
    async fn open(dir: &Path, script: &str, turns: Arc<AtomicU64>) -> Self {
        let recording = Recording {
            adapter: Box::new(FakeAdapter::new(fixture(script))),
            starts: Default::default(),
            commands: Default::default(),
            gate: Default::default(),
        };
        let (starts, commands) = (recording.starts.clone(), recording.commands.clone());
        let gate = recording.gate.clone();
        Self::open_with(dir, Arc::new(recording), starts, commands, gate, turns).await
    }

    /// Opens a manager whose CLI starts play `scripts`, one per start, in order.
    async fn open_scripts(dir: &Path, scripts: &[&str], turns: Arc<AtomicU64>) -> Self {
        let scripted = Scripted::new(scripts);
        let (starts, commands) = (scripted.starts.clone(), scripted.commands.clone());
        let gate = scripted.gate.clone();
        Self::open_with(dir, scripted, starts, commands, gate, turns).await
    }

    /// Opens a manager running the fake provider's sessions on `adapter`, which records into
    /// `starts` and `commands` and holds commands while `gate` is held.
    async fn open_with(
        dir: &Path,
        adapter: Arc<dyn Adapter>,
        starts: Arc<Mutex<Vec<StartRequest>>>,
        commands: Arc<Mutex<Vec<AdapterCommand>>>,
        gate: Gate,
        turns: Arc<AtomicU64>,
    ) -> Self {
        let (tx, seen) = mpsc::unbounded_channel();
        let mut adapters = Adapters::new();
        adapters.register(fake(), adapter);
        let mut accounts = Accounts::new();
        accounts.insert(
            account(),
            AccountConfig {
                provider: fake(),
                label: "Account 1".into(),
                config_dir: Some(dir.join("account")),
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
            attachments: dir.join("attachments"),
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
            gate,
            shutdown,
        }
    }

    /// Holds the daemon's commands back from the CLI until the guard drops, so a test knows
    /// what it sends meanwhile arrives while the turn is still open.
    async fn hold(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.gate.clone().lock_owned().await
    }

    async fn create(&self) -> SessionId {
        let repo = &self.repo;
        let result = self
            .manager
            .handle(
                alice(),
                CommandBody::CreateSession {
                    repo: Some(repo.to_str().unwrap().to_owned()),
                    project_id: None,
                    branch: None,
                    account_id: Some(account()),
                    provider: None,
                    model: None,
                    permission_mode: Some(PermissionMode::Ask),
                    max_children: None,
                    failover_pin: None,
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
            images: Vec::new(),
        };
        let result = self.manager.handle(by, command).await.unwrap();
        assert_eq!(result, CommandResult::Applied);
    }

    /// Published durable events up to and including the first `done` accepts.
    async fn events_until(&mut self, done: impl Fn(&EventBody) -> bool) -> Vec<Event> {
        self.until_event(|event| done(&event.body)).await
    }

    /// Published durable events up to and including the first `done` accepts.
    async fn until_event(&mut self, done: impl Fn(&Event) -> bool) -> Vec<Event> {
        let mut events = Vec::new();
        loop {
            let seen = tokio::time::timeout(Duration::from_secs(5), self.seen.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out; got {:#?}", describe(&events)))
                .unwrap();
            if let Seen::Event(event) = &seen {
                let stop = done(event);
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
            |body| matches!(body, EventBody::SessionStatusChanged { status: s, .. } if *s == status),
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

    /// The next account list published, skipping everything else.
    async fn next_accounts(&mut self) -> Vec<Account> {
        loop {
            let seen = tokio::time::timeout(Duration::from_secs(5), self.seen.recv())
                .await
                .expect("timed out waiting for an account list")
                .unwrap();
            if let Seen::Accounts(accounts) = seen {
                return accounts;
            }
            self.log.push(seen);
        }
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
                EventBody::SessionStatusChanged { status, .. } => format!("status {status:?}"),
                EventBody::TurnStarted { turn_id } => format!("turn_started {turn_id}"),
                EventBody::TurnCompleted { turn_id } => format!("turn_completed {turn_id}"),
                EventBody::TurnInterrupted { turn_id } => format!("turn_interrupted {turn_id}"),
                EventBody::TurnFailed { turn_id, error } => {
                    format!("turn_failed {turn_id} {:?}", error.class)
                }
                EventBody::ItemAdded { item } => match &item.body {
                    ItemBody::UserMessage { text, .. } => format!("user {} {text}", item.turn_id),
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
                EventBody::AccountSwitched { account_id } => {
                    format!("account_switched {account_id}")
                }
                EventBody::ProviderSwitched {
                    provider,
                    account_id,
                    model,
                } => format!(
                    "provider_switched {} {account_id} {model:?}",
                    provider.as_str()
                ),
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
    let held = daemon.hold().await;
    daemon.prompt(alice(), &session, "First.").await;
    daemon.prompt(bob(), &session, "Second.").await;
    drop(held);

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
    let held = daemon.hold().await;
    daemon.prompt(alice(), &session, "First.").await;
    daemon.prompt(alice(), &session, "Second.").await;
    drop(held);
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
    // Each status change publishes the list again, right after the event.
    let Some(Seen::Sessions(heads)) = daemon.seen.recv().await else {
        panic!("the idle status is not followed by the session list");
    };
    let last = daemon.journal(&session).await.last().unwrap().seq;
    assert_eq!(
        (heads[0].status, heads[0].head_seq, &heads[0].account_id),
        (SessionStatus::Idle, last, &account())
    );
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
            host_id: None,
            head_seq: 7,
            status: SessionStatus::Idle,
            parent: None,
            task: None,
            title: None,
            project_id: None,
            account_id: account(),
            children_need_you: 0,
            queue: Vec::new(),
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
                text: "First.".into(),
                attachments: Vec::new(),
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
async fn the_clis_session_id_is_kept_with_its_account_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let turns = Arc::new(AtomicU64::new(0));
    let mut daemon = Daemon::open(dir.path(), "identified.jsonl", turns.clone()).await;
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "First.").await;
    daemon.until_status(SessionStatus::Idle).await;
    daemon.stop().await;

    let daemon = Daemon::open(dir.path(), "second.jsonl", turns).await;
    let store = Store::open(dir.path().join("herder.db")).unwrap();
    assert_eq!(
        store.native_session(&session).unwrap(),
        Some(NativeSession {
            provider: fake(),
            account_id: account(),
            native_id: "cli-session-1".into(),
        })
    );
    daemon.stop().await;
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

/// The texts of `session_id`'s queue in the latest session list published, and their ids.
fn published_queue(daemon: &mut Daemon, session_id: &SessionId) -> Vec<(String, PromptId)> {
    let mut latest = None;
    while let Ok(seen) = daemon.seen.try_recv() {
        if let Seen::Sessions(heads) = &seen {
            latest = Some(heads.clone());
        }
        daemon.log.push(seen);
    }
    let heads = latest.expect("no session list was published");
    let head = heads
        .iter()
        .find(|head| head.session_id == *session_id)
        .unwrap();
    head.queue
        .iter()
        .map(|prompt| (prompt.text.clone(), prompt.prompt_id.clone()))
        .collect()
}

#[tokio::test]
async fn queued_prompts_are_removed_moved_and_sent_now_until_they_start() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "queue.jsonl", Default::default()).await;
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "Hold.").await;
    daemon
        .events_until(|body| matches!(body, EventBody::TurnStarted { .. }))
        .await;
    for text in ["A.", "B.", "C.", "D."] {
        daemon.prompt(bob(), &session, text).await;
    }
    let queue = published_queue(&mut daemon, &session);
    let texts = |queue: &[(String, PromptId)]| -> Vec<String> {
        queue.iter().map(|(text, _)| text.clone()).collect()
    };
    assert_eq!(texts(&queue), ["A.", "B.", "C.", "D."]);
    let id = |text: &str| {
        queue
            .iter()
            .find(|(queued, _)| queued == text)
            .map(|(_, id)| id.clone())
            .unwrap()
    };
    let heads = daemon.manager.sessions().await.unwrap();
    let listed = &heads[0].queue[0];
    assert_eq!(
        (listed.by.as_ref(), listed.images, &listed.agent_message),
        (Some(&bob()), 0, &None)
    );

    let edit = async |daemon: &mut Daemon, command: CommandBody| {
        daemon.manager.handle(bob(), command).await?;
        Ok::<_, ErrorInfo>(texts(&published_queue(daemon, &session)))
    };
    let remove = |prompt_id: PromptId| CommandBody::RemoveQueued {
        session_id: session.clone(),
        prompt_id,
    };
    let move_before = |prompt_id: PromptId, before: Option<PromptId>| CommandBody::MoveQueued {
        session_id: session.clone(),
        prompt_id,
        before,
    };
    let send_now = |prompt_id: PromptId| CommandBody::SendQueuedNow {
        session_id: session.clone(),
        prompt_id,
    };
    assert_eq!(
        edit(&mut daemon, remove(id("B."))).await.unwrap(),
        ["A.", "C.", "D."]
    );
    // To the front, into the middle, and to the end.
    assert_eq!(
        edit(&mut daemon, move_before(id("D."), Some(id("A."))))
            .await
            .unwrap(),
        ["D.", "A.", "C."]
    );
    assert_eq!(
        edit(&mut daemon, move_before(id("D."), Some(id("C."))))
            .await
            .unwrap(),
        ["A.", "D.", "C."]
    );
    assert_eq!(
        edit(&mut daemon, move_before(id("A."), None))
            .await
            .unwrap(),
        ["D.", "C.", "A."]
    );

    // Unknown prompts, and a removed one, are not found; nothing changes.
    let unknown = PromptId::new("unknown");
    for command in [
        remove(unknown.clone()),
        remove(id("B.")),
        move_before(unknown.clone(), None),
        move_before(id("A."), Some(unknown.clone())),
        send_now(unknown.clone()),
    ] {
        let refused = daemon.manager.handle(bob(), command).await.unwrap_err();
        assert_eq!(refused.code, ErrorCode::NotFound, "{refused:?}");
    }

    // Sending the middle prompt now interrupts the turn and runs it first; the rest keep
    // their order.
    assert_eq!(
        edit(&mut daemon, send_now(id("C."))).await.unwrap(),
        ["C.", "D.", "A."]
    );
    daemon
        .events_until(|body| matches!(body, EventBody::TurnCompleted { turn_id } if turn_id.as_str() == "turn-4"))
        .await;
    daemon.until_status(SessionStatus::Idle).await;
    let journal = daemon.journal(&session).await;
    let prompts: Vec<String> = describe(&journal)
        .into_iter()
        .filter(|line| line.contains(": user ") || line.contains("turn_interrupted"))
        .collect();
    assert_eq!(
        prompts,
        [
            "alice: user turn-1 Hold.",
            "-: turn_interrupted turn-1",
            "bob: user turn-2 C.",
            "bob: user turn-3 D.",
            "bob: user turn-4 A.",
        ]
    );
    assert!(daemon.manager.sessions().await.unwrap()[0].queue.is_empty());

    // Once a prompt has started, it can no longer be edited.
    for command in [
        remove(id("C.")),
        move_before(id("D."), None),
        send_now(id("A.")),
    ] {
        let refused = daemon.manager.handle(bob(), command).await.unwrap_err();
        assert_eq!(refused.code, ErrorCode::Conflict, "{refused:?}");
    }
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
            decision: ApprovalOutcome::Allow,
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
    daemon
        .events_until(|body| matches!(body, EventBody::ItemAdded { item } if matches!(item.body, ItemBody::ToolResult { .. })))
        .await;
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
async fn ending_a_turn_expires_its_open_approvals() {
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
            "-: approval_resolved approval-1 Expired",
            "-: turn_interrupted turn-1",
            "-: status Idle",
        ]
    );
    let late = daemon.answer(alice(), &session, "approval-1", ApprovalDecision::Allow);
    assert_eq!(late.await.unwrap_err().code, ErrorCode::Conflict);
    assert!(daemon.answers().is_empty());
}

#[tokio::test]
async fn restart_expires_an_approval_the_previous_daemon_left_open() {
    let dir = tempfile::tempdir().unwrap();
    let turns = Arc::new(AtomicU64::new(0));
    let mut daemon = Daemon::open(dir.path(), "approval.jsonl", turns.clone()).await;
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "Run the tests.").await;
    daemon.until_status(SessionStatus::NeedsYou).await;
    daemon.stop().await;

    let daemon = Daemon::open(dir.path(), "approval.jsonl", turns).await;
    let journal = daemon.journal(&session).await;
    // Still needs-you: the turn failed. The expiry carries no `by`: the daemon made it.
    assert_eq!(
        describe(&journal[5..]),
        [
            "-: approval_requested approval-1",
            "-: status NeedsYou",
            "-: approval_resolved approval-1 Expired",
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
async fn each_turn_end_checkpoints_the_worktree_without_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "two_prompts.jsonl", Default::default()).await;
    let checkpoints = dir.path().join("checkpoints");
    daemon
        .manager
        .checkpoint_turns(checkpoint::Config {
            dir: checkpoints.clone(),
            keep: checkpoint::KEEP,
            push_timeout: Duration::from_secs(30),
        })
        .unwrap();
    let session = daemon.create().await;
    let worktree = daemon.manager.worktree(&session).await.unwrap();
    std::fs::write(worktree.join("notes.txt"), "notes\n").unwrap();
    std::fs::write(worktree.join(".env"), "TOKEN=secret\n").unwrap();

    daemon.prompt(alice(), &session, "First.").await;
    daemon.until_status(SessionStatus::Idle).await;

    // Made before the turn's end settles the session.
    let name = format!("refs/herder/{session}/turn-1");
    assert_eq!(
        git(&daemon.repo, &["ls-tree", "-r", "--name-only", &name]),
        "notes.txt"
    );
    assert_eq!(
        git(&worktree, &["status", "--porcelain"]),
        "?? .env\n?? notes.txt"
    );
    // Without an origin it is bundled, in the background.
    let bundle = checkpoints.join(session.as_str()).join("turn-1.bundle");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !bundle.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "no bundle at {}",
            bundle.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn commands_for_unknown_sessions_and_accounts_are_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path(), "first.jsonl", Default::default()).await;
    let prompt = CommandBody::SendPrompt {
        session_id: SessionId::new("nope"),
        text: "Hi.".into(),
        images: Vec::new(),
    };
    let error = daemon.manager.handle(alice(), prompt).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);

    let create = CommandBody::CreateSession {
        repo: Some(dir.path().to_str().unwrap().to_owned()),
        project_id: None,
        branch: None,
        account_id: Some(AccountId::new("account-9")),
        provider: None,
        model: None,
        permission_mode: Some(PermissionMode::Ask),
        max_children: None,
        failover_pin: None,
    };
    let error = daemon.manager.handle(alice(), create).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn create_by_project_or_repo_falls_back_to_the_projects_default_account() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path(), "first.jsonl", Default::default()).await;
    let repo = daemon.repo.to_str().unwrap().to_owned();
    let project = |default_account: Option<AccountId>| Project {
        project_id: ProjectId::new("github.com/org/app"),
        name: "app".into(),
        paths: vec![repo.clone()],
        default_permission_mode: None,
        default_account,
        setup_command: None,
        icon: None,
    };
    let create = |repo: Option<String>, project_id: Option<&str>| CommandBody::CreateSession {
        repo,
        project_id: project_id.map(ProjectId::new),
        branch: None,
        account_id: None,
        provider: None,
        model: None,
        permission_mode: Some(PermissionMode::Ask),
        max_children: None,
        failover_pin: None,
    };
    let code = |result: Result<CommandResult, ErrorInfo>| result.unwrap_err().code;

    daemon.manager.set_projects(&[project(None)]).await;
    let refused = daemon
        .manager
        .handle(alice(), create(Some(repo.clone()), None))
        .await;
    assert_eq!(code(refused), ErrorCode::BadRequest);
    let refused = daemon.manager.handle(alice(), create(None, None)).await;
    assert_eq!(code(refused), ErrorCode::BadRequest);
    let both = create(Some(repo.clone()), Some("github.com/org/app"));
    assert_eq!(
        code(daemon.manager.handle(alice(), both).await),
        ErrorCode::BadRequest
    );
    let unknown = create(None, Some("github.com/org/other"));
    assert_eq!(
        code(daemon.manager.handle(alice(), unknown).await),
        ErrorCode::NotFound
    );

    daemon
        .manager
        .set_projects(&[project(Some(account()))])
        .await;
    for create in [
        create(None, Some("github.com/org/app")),
        create(Some(repo.clone()), None),
    ] {
        let Ok(CommandResult::SessionCreated { session_id }) =
            daemon.manager.handle(alice(), create).await
        else {
            panic!("expected a created session");
        };
        let journal = daemon.journal(&session_id).await;
        let EventBody::SessionCreated {
            repo: created_in,
            account_id,
            ..
        } = &journal[0].body
        else {
            panic!("expected session_created");
        };
        assert_eq!((created_in, account_id), (&repo, &account()));
    }
    let heads = daemon.manager.sessions().await.unwrap();
    assert!(
        heads
            .iter()
            .all(|head| head.project_id == Some(ProjectId::new("github.com/org/app")))
    );
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
        repo: Some(daemon.repo.to_str().unwrap().to_owned()),
        project_id: None,
        branch: Some(branch.to_owned()),
        account_id: Some(account()),
        provider: None,
        model: None,
        permission_mode: Some(PermissionMode::Ask),
        max_children: None,
        failover_pin: None,
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
        images: Vec::new(),
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
        images: Vec::new(),
    };
    let error = daemon.manager.handle(alice(), prompt).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
}

#[tokio::test]
async fn rename_and_retitle_until_the_session_is_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "first.jsonl", Default::default()).await;
    let session = daemon.create().await;
    let rename = |title: &str| CommandBody::RenameSession {
        session_id: session.clone(),
        title: title.into(),
    };

    let result = daemon
        .manager
        .handle(bob(), rename("  Flaky auth tests \n"))
        .await;
    assert_eq!(result, Ok(CommandResult::Applied));
    let events = daemon
        .events_until(|body| matches!(body, EventBody::TitleChanged { .. }))
        .await;
    let titled = events.last().unwrap();
    assert_eq!(titled.by, Some(bob()));
    assert_eq!(
        titled.body,
        EventBody::TitleChanged {
            title: "Flaky auth tests".into(),
            source: TitleSource::User,
        }
    );
    // The session list follows the event, with the new title.
    let Some(Seen::Sessions(heads)) = daemon.seen.recv().await else {
        panic!("a title change is not followed by the session list");
    };
    assert_eq!(heads[0].title.as_deref(), Some("Flaky auth tests"));

    // The title it already has changes nothing.
    let journaled = daemon.journal(&session).await.len();
    let result = daemon
        .manager
        .handle(alice(), rename("Flaky auth tests"))
        .await;
    assert_eq!(result, Ok(CommandResult::Applied));
    assert_eq!(daemon.journal(&session).await.len(), journaled);

    let too_long = "x".repeat(MAX_TITLE_CHARS + 1);
    for invalid in ["", "  ", "two\nlines", too_long.as_str()] {
        let error = daemon.manager.handle(alice(), rename(invalid)).await;
        assert_eq!(
            error.unwrap_err().code,
            ErrorCode::BadRequest,
            "{invalid:?}"
        );
    }
    let missing = CommandBody::RenameSession {
        session_id: SessionId::new("missing"),
        title: "Anything".into(),
    };
    let error = daemon.manager.handle(alice(), missing).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);

    // Retitling waits for title generation, and changes nothing until then.
    let retitle = || CommandBody::RetitleSession {
        session_id: session.clone(),
    };
    let error = daemon.manager.handle(bob(), retitle()).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Unsupported);
    assert_eq!(daemon.journal(&session).await.len(), journaled);
    let missing = CommandBody::RetitleSession {
        session_id: SessionId::new("missing"),
    };
    let error = daemon.manager.handle(bob(), missing).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);

    daemon
        .manager
        .archive(alice(), session.clone(), false)
        .await
        .unwrap();
    let error = daemon.manager.handle(alice(), rename("Later")).await;
    assert_eq!(error.unwrap_err().code, ErrorCode::Conflict);
    let error = daemon.manager.handle(alice(), retitle()).await;
    assert_eq!(error.unwrap_err().code, ErrorCode::Conflict);
    assert_eq!(
        daemon.manager.sessions().await.unwrap()[0].title.as_deref(),
        Some("Flaky auth tests")
    );
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
        adapter: Box::new(FakeAdapter::new(fixture("first.jsonl"))),
        starts: Default::default(),
        commands: Default::default(),
        gate: Default::default(),
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
            },
        ),
        (
            AccountId::new("codex-work"),
            AccountConfig {
                provider: Provider::Codex,
                label: "Work".into(),
                config_dir: Some(codex_home.clone()),
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
        attachments: dir.path().join("attachments"),
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
            repo: Some(repo.to_str().unwrap().to_owned()),
            project_id: None,
            branch: None,
            account_id: Some(AccountId::new(account)),
            provider: None,
            model: None,
            permission_mode: Some(PermissionMode::Ask),
            max_children: None,
            failover_pin: None,
        };
        let Ok(CommandResult::SessionCreated { session_id }) =
            manager.handle(alice(), create).await
        else {
            panic!("{account}: no session");
        };
        let prompt = CommandBody::SendPrompt {
            session_id: session_id.clone(),
            text: "First.".into(),
            images: Vec::new(),
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

#[tokio::test]
async fn each_start_registers_herders_mcp_server_with_a_token_for_that_session() {
    use herder_daemon::mcp;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "first.jsonl", Default::default()).await;
    let data_dir = dir.path().join("data");
    std::fs::create_dir(&data_dir).unwrap();
    daemon
        .manager
        .serve_mcp(
            mcp::Config {
                data_dir: data_dir.clone(),
                herder: PathBuf::from("/opt/herder"),
            },
            TaskLimits::default(),
        )
        .unwrap();
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "First.").await;
    daemon.until_status(SessionStatus::Idle).await;

    let starts = daemon.starts.lock().unwrap().clone();
    let server = starts[0].mcp.clone().expect("an MCP server for the CLI");
    assert_eq!(server.command, Path::new("/opt/herder"));
    assert_eq!(
        server.args,
        [
            "mcp",
            "--data-dir",
            data_dir.to_str().unwrap(),
            "--session",
            session.as_str()
        ]
    );

    // The shim the CLI would run authenticates as the session and reaches the tools.
    let call = |session: SessionId| {
        let data_dir = data_dir.clone();
        async move {
            let (mut input, shim_input) = tokio::io::duplex(1 << 16);
            let (shim_output, output) = tokio::io::duplex(1 << 16);
            let shim = tokio::spawn(async move {
                mcp::shim(&data_dir, &session, shim_input, shim_output).await
            });
            input
                .write_all(
                    b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\
                      \"params\":{\"name\":\"status\",\"arguments\":{}}}\n",
                )
                .await
                .unwrap();
            let mut line = String::new();
            BufReader::new(output).read_line(&mut line).await.unwrap();
            drop(input);
            (line, shim.await.unwrap())
        }
    };
    let (line, shim) = call(session.clone()).await;
    shim.unwrap();
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        response["result"]["structuredContent"],
        serde_json::json!({ "children": [] })
    );

    // Archiving withdraws the token.
    let archive = CommandBody::ArchiveSession {
        session_id: session.clone(),
        force: true,
    };
    daemon.manager.handle(alice(), archive).await.unwrap();
    let (_, shim) = call(session).await;
    assert!(shim.is_err());
    daemon.stop().await;
}

/// A fake script of `turns` turns, each a prompt `Request <n>` answered by a tool call, a
/// 6000-character tool result and a reply.
fn long_script(dir: &Path, turns: usize) -> PathBuf {
    let mut script = String::new();
    for n in 1..=turns {
        let turn = format!("turn-{n}");
        let item = |id: String, body: serde_json::Value| serde_json::json!({"emit": {"type": "item_completed", "item": {"id": id, "turn_id": turn, "body": body}}});
        let lines = [
            serde_json::json!({"expect": {"type": "send_prompt", "turn_id": turn, "text": format!("Request {n}")}}),
            serde_json::json!({"emit": {"type": "turn_started", "turn_id": turn}}),
            item(
                format!("call-{n}"),
                serde_json::json!({"type": "tool_call", "name": "Bash", "input": {"command": "cargo test"}}),
            ),
            item(
                format!("result-{n}"),
                serde_json::json!({"type": "tool_result", "call_id": format!("call-{n}"), "output": "x".repeat(6_000), "is_error": false}),
            ),
            item(
                format!("reply-{n}"),
                serde_json::json!({"type": "assistant_message", "text": format!("Reply {n}")}),
            ),
            serde_json::json!({"emit": {"type": "turn_completed", "turn_id": turn}}),
        ];
        for line in lines {
            script.push_str(&format!("{line}\n"));
        }
    }
    let path = dir.join(format!("long-{turns}.jsonl"));
    std::fs::write(&path, script).unwrap();
    path
}

#[tokio::test]
async fn a_200_turn_session_hands_off_within_budget_keeping_the_first_request() {
    let dir = tempfile::tempdir().unwrap();
    let turns = Arc::new(AtomicU64::new(0));
    let script = long_script(dir.path(), 200);
    let mut daemon = Daemon::open(dir.path(), script.to_str().unwrap(), turns.clone()).await;
    let session = daemon.create().await;
    // Queued prompts run one turn after another.
    for n in 1..=200 {
        daemon
            .prompt(alice(), &session, &format!("Request {n}"))
            .await;
    }
    daemon
        .events_until(|body| matches!(body, EventBody::TurnCompleted { turn_id } if turn_id.as_str() == "turn-200"))
        .await;
    daemon.stop().await;

    let next = dir.path().join("next.jsonl");
    std::fs::write(
        &next,
        r#"{"expect": {"type": "send_prompt", "turn_id": "turn-201", "text": "Next."}}"#,
    )
    .unwrap();
    let daemon = Daemon::open(dir.path(), next.to_str().unwrap(), turns).await;
    daemon.prompt(alice(), &session, "Next.").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while daemon.starts.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let seed = daemon.starts.lock().unwrap()[0].seed.clone();
    let budget = handoff::budget(&fake(), "");
    assert!(handoff::estimate(&seed) <= budget);
    assert_eq!(
        seed[0].body,
        ItemBody::UserMessage {
            text: "Request 1".into(),
            attachments: Vec::new(),
        }
    );
    assert_eq!(
        seed[1].body,
        ItemBody::UserMessage {
            text: handoff::NOTE.into(),
            attachments: Vec::new(),
        }
    );
    let last: Vec<_> = seed[seed.len() - 4..]
        .iter()
        .map(|item| &item.body)
        .collect();
    assert_eq!(
        last[0],
        &ItemBody::UserMessage {
            text: "Request 200".into(),
            attachments: Vec::new(),
        }
    );
    let ItemBody::ToolResult {
        call_id, output, ..
    } = last[2]
    else {
        panic!("expected a tool result, got {:?}", last[2]);
    };
    assert_eq!(call_id, &ItemId::new("call-200"));
    // The newest turn keeps its output whole; older ones were cut to make room.
    assert_eq!(output.len(), 6_000);
    assert!(seed.iter().any(|item| matches!(
        &item.body,
        ItemBody::ToolResult { output, .. } if output.contains("[… 3000 chars elided …]")
    )));
    daemon.stop().await;
}

#[tokio::test]
async fn with_limits_on_each_cli_start_runs_in_a_scope_of_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "first.jsonl", Default::default()).await;
    let scopes = Arc::new(Scopes::new(
        ResourcesConfig::default(),
        Host {
            memory_total: 10_000,
            cores: 2,
            nice: 0,
        },
        true,
    ));
    daemon.manager.limit_resources(Arc::clone(&scopes)).unwrap();
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "First.").await;
    daemon.until_status(SessionStatus::Idle).await;

    let starts = daemon.starts.lock().unwrap().clone();
    let [start] = starts.as_slice() else {
        panic!("expected one start, got {starts:?}");
    };
    let unit = scopes.unit(&session).unwrap();
    assert!(unit.starts_with(&format!("herder-{session}-")), "{unit}");
    let limits = scopes.limits(false);
    assert_eq!(start.launcher, resources::launcher(&unit, &limits));
    assert_eq!(limits.cpu_weight, 100);
    assert_eq!(limits.memory_max, 4_000);
    daemon.stop().await;
}

#[tokio::test]
async fn without_limits_the_cli_runs_directly() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "first.jsonl", Default::default()).await;
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "First.").await;
    daemon.until_status(SessionStatus::Idle).await;
    let starts = daemon.starts.lock().unwrap().clone();
    assert!(starts.iter().all(|start| start.launcher.is_empty()));
    daemon.stop().await;
}

#[tokio::test]
async fn archive_stops_what_the_session_left_running() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "first.jsonl", Default::default()).await;
    // Limits off, as on a host without a systemd user session: the CLI's environment marks it.
    let scopes = Arc::new(Scopes::new(
        ResourcesConfig::default(),
        Host {
            memory_total: 10_000,
            cores: 2,
            nice: 0,
        },
        false,
    ));
    daemon.manager.limit_resources(Arc::clone(&scopes)).unwrap();
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "First.").await;
    daemon.until_status(SessionStatus::Idle).await;

    // A dev server the agent started and left behind, with the environment it inherited.
    let start = daemon.starts.lock().unwrap()[0].clone();
    assert_eq!(
        start.env.get(resources::processes::SESSION_ENV),
        Some(&session.to_string())
    );
    let mut leftover = tokio::process::Command::new("sleep")
        .arg("60")
        .env_clear()
        .envs(&start.env)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    assert_eq!(scopes.processes(&session).await, [leftover.id().unwrap()]);

    let archive = CommandBody::ArchiveSession {
        session_id: session.clone(),
        force: true,
    };
    daemon.manager.handle(alice(), archive).await.unwrap();
    let status = tokio::time::timeout(Duration::from_secs(10), leftover.wait())
        .await
        .expect("archive left the process running")
        .unwrap();
    assert!(!status.success());
    assert!(scopes.processes(&session).await.is_empty());
    daemon.stop().await;
}

#[tokio::test]
async fn compose_down_brings_down_a_project_the_session_started() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path(), "first.jsonl", Default::default()).await;
    let session = daemon.create().await;
    let worktree = daemon.manager.worktree(&session).await.unwrap();
    // A fake docker: lists one container of compose project `app` in the worktree and records
    // every call.
    let line = serde_json::json!({
        "id": "c1",
        "name": "app-db-1",
        "image": "postgres:16",
        "state": "running",
        "project": "app",
        "working_dir": worktree,
    });
    std::fs::write(dir.path().join("ps"), line.to_string()).unwrap();
    let program = dir.path().join("docker");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{calls}'\nif [ \"$1\" = ps ]; then cat '{ps}'; fi\n",
            calls = dir.path().join("calls").display(),
            ps = dir.path().join("ps").display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let down = |project: &str| CommandBody::ComposeDown {
        session_id: session.clone(),
        project: project.to_owned(),
    };

    // Before containers are tracked there is nothing to bring down.
    let refused = daemon.manager.handle(alice(), down("app")).await;
    assert_eq!(refused.unwrap_err().code, ErrorCode::Unsupported);

    let docker = Arc::new(resources::Docker::new(&program));
    daemon
        .manager
        .track_containers(Arc::clone(&docker))
        .unwrap();
    docker.poll(|| daemon.manager.worktrees()).await;
    // Only a project among the session's containers.
    let refused = daemon.manager.handle(alice(), down("other")).await;
    assert_eq!(refused.unwrap_err().code, ErrorCode::NotFound);
    assert_eq!(
        daemon.manager.handle(alice(), down("app")).await,
        Ok(CommandResult::Applied)
    );
    let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
    let calls: Vec<&str> = calls.lines().collect();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[1], "compose --project-name app down");
    daemon.stop().await;
}

/// Adapters that play one script per start, in order, recording every start request and
/// every command across starts.
struct Scripted {
    scripts: Mutex<Vec<PathBuf>>,
    /// The adapter that plays a script.
    play: fn(PathBuf) -> Box<dyn Adapter>,
    starts: Arc<Mutex<Vec<StartRequest>>>,
    commands: Arc<Mutex<Vec<AdapterCommand>>>,
    gate: Gate,
}

impl Scripted {
    /// The fake adapter, playing `scripts`.
    fn new(scripts: &[&str]) -> Arc<Self> {
        Self::playing(scripts, |script| Box::new(FakeAdapter::new(script)))
    }

    /// The ACP adapter with OpenCode's profile, replaying recordings of `opencode acp`.
    fn opencode(recordings: &[&str]) -> Arc<Self> {
        Self::playing(recordings, |recording| {
            Box::new(AcpAdapter::replaying(AgentProfile::opencode(), recording))
        })
    }

    fn playing(scripts: &[&str], play: fn(PathBuf) -> Box<dyn Adapter>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(scripts.iter().rev().map(|name| fixture(name)).collect()),
            play,
            starts: Default::default(),
            commands: Default::default(),
            gate: Default::default(),
        })
    }

    fn starts(&self) -> Vec<StartRequest> {
        self.starts.lock().unwrap().clone()
    }

    fn commands(&self) -> Vec<AdapterCommand> {
        self.commands.lock().unwrap().clone()
    }
}

impl Adapter for Scripted {
    fn start(&self, request: StartRequest) -> StartFuture {
        let script = self
            .scripts
            .lock()
            .unwrap()
            .pop()
            .expect("a script per start");
        Recording {
            adapter: (self.play)(script),
            starts: self.starts.clone(),
            commands: self.commands.clone(),
            gate: self.gate.clone(),
        }
        .start(request)
    }
}

/// A manager running `adapters` on `accounts`, each `(id, provider)` with its config
/// dir under `dir`, over a store where `events` were journaled first.
struct Switching {
    manager: SessionManager,
    repo: PathBuf,
    seen: mpsc::UnboundedReceiver<Seen>,
    shutdown: CancellationToken,
}

impl Switching {
    async fn open(
        dir: &Path,
        adapters: &[(Provider, Arc<Scripted>)],
        accounts: &[(&str, Provider)],
        events: Vec<(SessionId, EventBody)>,
    ) -> Self {
        let mut registry = Adapters::new();
        for (provider, adapter) in adapters {
            registry.register(provider.clone(), adapter.clone());
        }
        let accounts = accounts
            .iter()
            .map(|(id, provider)| {
                let config = AccountConfig {
                    provider: provider.clone(),
                    label: id.to_string(),
                    config_dir: Some(dir.join(id)),
                };
                (AccountId::new(*id), config)
            })
            .collect();
        let mut store = Store::open(dir.join("herder.db")).unwrap();
        for (session_id, body) in events {
            let event = herder_store::NewEvent {
                session_id,
                at: herder_protocol::Timestamp::now(),
                by: None,
                body,
            };
            store.append(event).unwrap();
        }
        let turns = AtomicU64::new(0);
        let (tx, seen) = mpsc::unbounded_channel();
        let setup = Setup {
            store,
            adapters: registry,
            accounts,
            sink: Arc::new(Recorder(tx)),
            turn_ids: Box::new(move || {
                TurnId::new(format!("turn-{}", turns.fetch_add(1, Ordering::SeqCst) + 1))
            }),
            worktrees: Worktrees::new(dir.join("worktrees")),
            attachments: dir.join("attachments"),
        };
        let shutdown = CancellationToken::new();
        let manager = SessionManager::open(setup, shutdown.clone()).await.unwrap();
        let repo = dir.join("app");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "--quiet", "--initial-branch=main"]);
        git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "init"]);
        Self {
            manager,
            repo,
            seen,
            shutdown,
        }
    }

    async fn create(&self, account: &str) -> SessionId {
        self.create_pinned(account, None).await
    }

    /// A session on `account` with its own failover pin.
    async fn create_pinned(&self, account: &str, failover_pin: Option<bool>) -> SessionId {
        self.create_on(account, None, failover_pin).await
    }

    /// A session on `account` and `model`, with its own failover pin.
    async fn create_on(
        &self,
        account: &str,
        model: Option<&str>,
        failover_pin: Option<bool>,
    ) -> SessionId {
        let create = CommandBody::CreateSession {
            repo: Some(self.repo.to_str().unwrap().to_owned()),
            project_id: None,
            branch: None,
            account_id: Some(AccountId::new(account)),
            provider: None,
            model: model.map(str::to_owned),
            permission_mode: Some(PermissionMode::Ask),
            max_children: None,
            failover_pin,
        };
        let Ok(CommandResult::SessionCreated { session_id }) =
            self.manager.handle(alice(), create).await
        else {
            panic!("no session on {account}");
        };
        session_id
    }

    async fn handle(&self, command: CommandBody) -> Result<CommandResult, ErrorInfo> {
        self.manager.handle(alice(), command).await
    }

    /// Sends a prompt and waits for its turn to end; returns how it ended.
    async fn turn(&mut self, session_id: &SessionId, text: &str) -> EventBody {
        let prompt = CommandBody::SendPrompt {
            session_id: session_id.clone(),
            text: text.into(),
            images: Vec::new(),
        };
        assert_eq!(self.handle(prompt).await, Ok(CommandResult::Applied));
        self.until(|body| {
            matches!(
                body,
                EventBody::TurnCompleted { .. }
                    | EventBody::TurnInterrupted { .. }
                    | EventBody::TurnFailed { .. }
            )
        })
        .await
    }

    async fn until(&mut self, done: impl Fn(&EventBody) -> bool) -> EventBody {
        loop {
            let next = tokio::time::timeout(Duration::from_secs(5), self.seen.recv());
            if let Seen::Event(event) = next.await.unwrap().unwrap()
                && done(&event.body)
            {
                return event.body;
            }
        }
    }
}

fn switch_account(session_id: &SessionId, account: &str) -> CommandBody {
    CommandBody::SwitchAccount {
        session_id: session_id.clone(),
        account_id: AccountId::new(account),
    }
}

fn switch_provider(session_id: &SessionId, account: &str, model: Option<&str>) -> CommandBody {
    CommandBody::SwitchProvider {
        session_id: session_id.clone(),
        account_id: AccountId::new(account),
        model: model.map(Into::into),
    }
}

fn seed_texts(start: &StartRequest) -> Vec<String> {
    start
        .seed
        .iter()
        .map(|item| match &item.body {
            ItemBody::UserMessage { text, .. } => format!("user: {text}"),
            ItemBody::AssistantMessage { text } => format!("assistant: {text}"),
            other => format!("{other:?}"),
        })
        .collect()
}

#[tokio::test]
async fn one_session_moves_from_claude_to_codex_to_cursor_and_keeps_going() {
    let dir = tempfile::tempdir().unwrap();
    let claude = Scripted::new(&["switch_claude_a.jsonl", "switch_claude_b.jsonl"]);
    let codex = Scripted::new(&["switch_codex.jsonl"]);
    let cursor = Scripted::new(&["switch_cursor.jsonl"]);
    let mut daemon = Switching::open(
        dir.path(),
        &[
            (Provider::Claude, claude.clone()),
            (Provider::Codex, codex.clone()),
            (Provider::Cursor, cursor.clone()),
        ],
        &[
            ("claude-a", Provider::Claude),
            ("claude-b", Provider::Claude),
            ("codex-work", Provider::Codex),
            ("cursor-work", Provider::Cursor),
        ],
        Vec::new(),
    )
    .await;
    let session = daemon.create("claude-a").await;
    let completed = |body: &EventBody| matches!(body, EventBody::TurnCompleted { .. });

    assert!(completed(
        &daemon.turn(&session, "Add a health check endpoint.").await
    ));
    let set_model = CommandBody::SetModel {
        session_id: session.clone(),
        model: "opus".into(),
    };
    assert_eq!(daemon.handle(set_model).await, Ok(CommandResult::Applied));
    let applied = Ok(CommandResult::Applied);
    assert_eq!(
        daemon.handle(switch_account(&session, "claude-b")).await,
        applied
    );
    assert!(completed(
        &daemon.turn(&session, "Now add a test for it.").await
    ));
    let to_codex = switch_provider(&session, "codex-work", Some("gpt-5"));
    assert_eq!(daemon.handle(to_codex).await, applied);
    assert!(completed(
        &daemon.turn(&session, "Document it in the README.").await
    ));
    let to_cursor = switch_provider(&session, "cursor-work", None);
    assert_eq!(daemon.handle(to_cursor).await, applied);
    assert!(completed(&daemon.turn(&session, "Commit it.").await));

    // Each switch stopped the CLI it left; the model switch went to the running one.
    let claude_commands: Vec<_> = claude
        .commands()
        .into_iter()
        .filter(|command| !matches!(command, AdapterCommand::SendPrompt { .. }))
        .collect();
    assert_eq!(
        claude_commands,
        [
            AdapterCommand::SetModel {
                model: "opus".into()
            },
            AdapterCommand::Shutdown,
            AdapterCommand::Shutdown,
        ]
    );
    assert_eq!(codex.commands().last(), Some(&AdapterCommand::Shutdown));

    let claude_starts = claude.starts();
    let [on_a, on_b] = claude_starts.as_slice() else {
        panic!("expected two Claude starts, got {claude_starts:?}");
    };
    assert_eq!(on_a.config_dir, Some(dir.path().join("claude-a")));
    assert_eq!(on_b.config_dir, Some(dir.path().join("claude-b")));
    assert_eq!(on_b.model.as_deref(), Some("opus"));
    let [on_codex] = codex.starts().try_into().unwrap();
    assert_eq!(on_codex.config_dir, Some(dir.path().join("codex-work")));
    assert_eq!(on_codex.model.as_deref(), Some("gpt-5"));
    assert_eq!(
        seed_texts(&on_codex),
        [
            "user: Add a health check endpoint.",
            "assistant: Added GET /health.",
            "user: Now add a test for it.",
            "assistant: Added a test for /health.",
        ]
    );
    let [on_cursor] = cursor.starts().try_into().unwrap();
    assert_eq!(on_cursor.config_dir, Some(dir.path().join("cursor-work")));
    assert_eq!(on_cursor.model, None);
    assert_eq!(
        seed_texts(&on_cursor),
        [
            "user: Add a health check endpoint.",
            "assistant: Added GET /health.",
            "user: Now add a test for it.",
            "assistant: Added a test for /health.",
            "user: Document it in the README.",
            "assistant: Documented /health.",
        ]
    );

    let journal = daemon.manager.read_since(&session, 0, 1000).await.unwrap();
    let switches: Vec<_> = journal
        .iter()
        .filter(|event| {
            matches!(
                event.body,
                EventBody::ModelSwitched { .. }
                    | EventBody::AccountSwitched { .. }
                    | EventBody::ProviderSwitched { .. }
            )
        })
        .map(|event| (event.by.clone(), event.body.clone()))
        .collect();
    assert_eq!(
        switches,
        [
            (
                Some(alice()),
                EventBody::ModelSwitched {
                    model: "opus".into()
                }
            ),
            (
                Some(alice()),
                EventBody::AccountSwitched {
                    account_id: AccountId::new("claude-b")
                }
            ),
            (
                Some(alice()),
                EventBody::ProviderSwitched {
                    provider: Provider::Codex,
                    account_id: AccountId::new("codex-work"),
                    model: "gpt-5".into(),
                }
            ),
            (
                Some(alice()),
                EventBody::ProviderSwitched {
                    provider: Provider::Cursor,
                    account_id: AccountId::new("cursor-work"),
                    model: String::new(),
                }
            ),
        ]
    );
    daemon.shutdown.cancel();
}

/// Two Claude accounts whose CLIs play `scripts`; account `claude-a`'s config dir holds the
/// transcript of CLI session `claude-session-1` (unless `transcript` is false) beside its
/// credentials. Returns the manager, the adapter, the session after its first turn on
/// `claude-a`, and the transcript's path relative to a config dir.
async fn switching_claude(
    dir: &Path,
    scripts: &[&str],
    transcript: bool,
) -> (Switching, Arc<Scripted>, SessionId, PathBuf) {
    let claude = Scripted::new(scripts);
    let mut daemon = Switching::open(
        dir,
        &[(Provider::Claude, claude.clone())],
        &[
            ("claude-a", Provider::Claude),
            ("claude-b", Provider::Claude),
        ],
        Vec::new(),
    )
    .await;
    let relative = PathBuf::from("projects/-app/claude-session-1.jsonl");
    let a = dir.join("claude-a");
    std::fs::create_dir_all(a.join("projects/-app")).unwrap();
    std::fs::write(a.join(".credentials.json"), "a's login").unwrap();
    std::fs::write(a.join("projects/-app/claude-session-0.jsonl"), "older\n").unwrap();
    if transcript {
        std::fs::write(a.join(&relative), "the whole conversation\n").unwrap();
    }
    std::fs::create_dir_all(dir.join("claude-b")).unwrap();
    let session = daemon.create("claude-a").await;
    let ended = daemon.turn(&session, "Add a health check endpoint.").await;
    assert!(
        matches!(ended, EventBody::TurnCompleted { .. }),
        "{ended:?}"
    );
    assert_eq!(
        daemon.handle(switch_account(&session, "claude-b")).await,
        Ok(CommandResult::Applied)
    );
    (daemon, claude, session, relative)
}

/// Every file under `dir`, relative to it, sorted.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_owned()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path.strip_prefix(dir).unwrap().to_owned());
            }
        }
    }
    found.sort();
    found
}

#[tokio::test]
async fn a_same_provider_switch_resumes_the_clis_own_session_without_replay() {
    let dir = tempfile::tempdir().unwrap();
    let (mut daemon, claude, session, relative) =
        switching_claude(dir.path(), &["native_a.jsonl", "native_b.jsonl"], true).await;
    let ended = daemon.turn(&session, "Now add a test for it.").await;
    assert!(
        matches!(ended, EventBody::TurnCompleted { .. }),
        "{ended:?}"
    );

    let starts = claude.starts();
    let [on_a, on_b] = starts.as_slice() else {
        panic!("expected two starts, got {starts:?}");
    };
    assert_eq!(on_a.resume, None);
    assert_eq!(on_b.config_dir, Some(dir.path().join("claude-b")));
    assert_eq!(on_b.resume.as_deref(), Some("claude-session-1"));
    assert!(on_b.seed.is_empty(), "{:?}", on_b.seed);
    // Only the transcript moved, to the same project key; no credentials, no other session.
    let b = dir.path().join("claude-b");
    assert_eq!(files_under(&b), std::slice::from_ref(&relative));
    assert_eq!(
        std::fs::read_to_string(b.join(&relative)).unwrap(),
        "the whole conversation\n"
    );
    // The resumed CLI's id is now kept with the account it runs on.
    let store = Store::open(dir.path().join("herder.db")).unwrap();
    assert_eq!(
        store.native_session(&session).unwrap(),
        Some(NativeSession {
            provider: Provider::Claude,
            account_id: AccountId::new("claude-b"),
            native_id: "claude-session-1".into(),
        })
    );
    daemon.shutdown.cancel();
}

#[tokio::test]
async fn a_native_resume_the_cli_refuses_falls_back_to_replay() {
    let dir = tempfile::tempdir().unwrap();
    // No script for the second start: the fake fails it, as a CLI that cannot resume would.
    let scripts = [
        "native_a.jsonl",
        "unresumable.jsonl",
        "switch_claude_b.jsonl",
    ];
    let (mut daemon, claude, session, _) = switching_claude(dir.path(), &scripts, true).await;
    let ended = daemon.turn(&session, "Now add a test for it.").await;
    assert!(
        matches!(ended, EventBody::TurnCompleted { .. }),
        "{ended:?}"
    );

    let starts = claude.starts();
    let [_, resumed, replayed] = starts.as_slice() else {
        panic!("expected three starts, got {starts:?}");
    };
    assert_eq!(resumed.resume.as_deref(), Some("claude-session-1"));
    assert_eq!(replayed.resume, None);
    assert_eq!(replayed.config_dir, Some(dir.path().join("claude-b")));
    assert_eq!(
        seed_texts(replayed),
        [
            "user: Add a health check endpoint.",
            "assistant: Added GET /health."
        ]
    );
    daemon.shutdown.cancel();
}

#[tokio::test]
async fn a_switch_without_the_clis_transcript_replays() {
    let dir = tempfile::tempdir().unwrap();
    let scripts = ["native_a.jsonl", "switch_claude_b.jsonl"];
    let (mut daemon, claude, session, _) = switching_claude(dir.path(), &scripts, false).await;
    let ended = daemon.turn(&session, "Now add a test for it.").await;
    assert!(
        matches!(ended, EventBody::TurnCompleted { .. }),
        "{ended:?}"
    );

    let starts = claude.starts();
    let [_, on_b] = starts.as_slice() else {
        panic!("expected two starts, got {starts:?}");
    };
    assert_eq!(on_b.resume, None);
    assert_eq!(seed_texts(on_b).len(), 2);
    assert!(files_under(&dir.path().join("claude-b")).is_empty());
    daemon.shutdown.cancel();
}

#[tokio::test]
async fn a_session_moves_to_grok_and_back_to_claude() {
    let dir = tempfile::tempdir().unwrap();
    let claude = Scripted::new(&["grok_switch_claude.jsonl", "grok_switch_back.jsonl"]);
    let grok = Scripted::new(&["grok_switch_grok.jsonl"]);
    let mut daemon = Switching::open(
        dir.path(),
        &[
            (Provider::Claude, claude.clone()),
            (Provider::Grok, grok.clone()),
        ],
        &[
            ("claude-a", Provider::Claude),
            ("grok-work", Provider::Grok),
        ],
        Vec::new(),
    )
    .await;
    let session = daemon.create("claude-a").await;
    let completed = |body: &EventBody| matches!(body, EventBody::TurnCompleted { .. });
    let applied = Ok(CommandResult::Applied);

    assert!(completed(
        &daemon.turn(&session, "Add a health check endpoint.").await
    ));
    let to_grok = switch_provider(&session, "grok-work", Some("grok-4.5"));
    assert_eq!(daemon.handle(to_grok).await, applied);
    assert!(completed(
        &daemon.turn(&session, "Now add a test for it.").await
    ));
    let set_model = CommandBody::SetModel {
        session_id: session.clone(),
        model: "grok-4.6".into(),
    };
    assert_eq!(daemon.handle(set_model).await, applied);
    let to_claude = switch_provider(&session, "claude-a", None);
    assert_eq!(daemon.handle(to_claude).await, applied);
    assert!(completed(&daemon.turn(&session, "Commit it.").await));

    // Grok ran on its own account's config dir, seeded with the Claude turn, and took the model
    // switch natively before it was stopped.
    let [on_grok] = grok.starts().try_into().unwrap();
    assert_eq!(on_grok.config_dir, Some(dir.path().join("grok-work")));
    assert_eq!(on_grok.model.as_deref(), Some("grok-4.5"));
    assert_eq!(
        seed_texts(&on_grok),
        [
            "user: Add a health check endpoint.",
            "assistant: Added GET /health.",
        ]
    );
    let grok_commands: Vec<_> = grok
        .commands()
        .into_iter()
        .filter(|command| !matches!(command, AdapterCommand::SendPrompt { .. }))
        .collect();
    assert_eq!(
        grok_commands,
        [
            AdapterCommand::SetModel {
                model: "grok-4.6".into()
            },
            AdapterCommand::Shutdown,
        ]
    );
    let claude_starts = claude.starts();
    let [_, back] = claude_starts.as_slice() else {
        panic!("expected two Claude starts, got {claude_starts:?}");
    };
    assert_eq!(back.config_dir, Some(dir.path().join("claude-a")));
    assert_eq!(back.model, None);
    assert_eq!(
        seed_texts(back),
        [
            "user: Add a health check endpoint.",
            "assistant: Added GET /health.",
            "user: Now add a test for it.",
            "assistant: Added a test for /health.",
        ]
    );

    let journal = daemon.manager.read_since(&session, 0, 1000).await.unwrap();
    let switches: Vec<_> = journal
        .iter()
        .filter_map(|event| match &event.body {
            body @ EventBody::ProviderSwitched { .. } => Some(body.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        switches,
        [
            EventBody::ProviderSwitched {
                provider: Provider::Grok,
                account_id: AccountId::new("grok-work"),
                model: "grok-4.5".into(),
            },
            EventBody::ProviderSwitched {
                provider: Provider::Claude,
                account_id: AccountId::new("claude-a"),
                model: String::new(),
            },
        ]
    );
    daemon.shutdown.cancel();
}

#[tokio::test]
async fn switching_is_refused_while_a_turn_runs_and_applies_once_it_ends() {
    let dir = tempfile::tempdir().unwrap();
    let fakes = Scripted::new(&["interrupt.jsonl", "second.jsonl"]);
    let codex = Scripted::new(&[]);
    let mut daemon = Switching::open(
        dir.path(),
        &[(fake(), fakes.clone()), (Provider::Codex, codex.clone())],
        &[
            ("account-1", fake()),
            ("account-2", fake()),
            ("codex-work", Provider::Codex),
        ],
        Vec::new(),
    )
    .await;
    let session = daemon.create("account-1").await;
    let prompt = CommandBody::SendPrompt {
        session_id: session.clone(),
        text: "Work forever.".into(),
        images: Vec::new(),
    };
    daemon.handle(prompt).await.unwrap();
    daemon
        .until(|body| matches!(body, EventBody::TurnStarted { .. }))
        .await;

    for switch in [
        switch_account(&session, "account-2"),
        switch_provider(&session, "codex-work", None),
    ] {
        let refused = daemon.handle(switch).await.unwrap_err();
        assert_eq!(refused.code, ErrorCode::Conflict, "{refused:?}");
    }
    let interrupt = CommandBody::Interrupt {
        session_id: session.clone(),
    };
    daemon.handle(interrupt).await.unwrap();
    daemon
        .until(|body| matches!(body, EventBody::TurnInterrupted { .. }))
        .await;

    let applied = daemon.handle(switch_account(&session, "account-2")).await;
    assert_eq!(applied, Ok(CommandResult::Applied));
    let ended = daemon.turn(&session, "Second.").await;
    assert!(
        matches!(ended, EventBody::TurnCompleted { .. }),
        "{ended:?}"
    );

    let starts = fakes.starts();
    let [first, second] = starts.as_slice() else {
        panic!("expected two starts, got {starts:?}");
    };
    assert_eq!(first.config_dir, Some(dir.path().join("account-1")));
    assert_eq!(second.config_dir, Some(dir.path().join("account-2")));
    assert_eq!(seed_texts(second), ["user: Work forever."]);
    assert!(codex.starts().is_empty());
    let journal = daemon.manager.read_since(&session, 0, 1000).await.unwrap();
    let switched: Vec<_> = journal
        .iter()
        .filter(|event| {
            matches!(
                event.body,
                EventBody::AccountSwitched { .. } | EventBody::ProviderSwitched { .. }
            )
        })
        .collect();
    assert_eq!(switched.len(), 1, "{switched:?}");
    daemon.shutdown.cancel();
}

#[tokio::test]
async fn switches_to_the_wrong_kind_of_account_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Switching::open(
        dir.path(),
        &[(Provider::Claude, Scripted::new(&[]))],
        &[
            ("claude-a", Provider::Claude),
            ("claude-b", Provider::Claude),
            ("codex-work", Provider::Codex),
        ],
        Vec::new(),
    )
    .await;
    let session = daemon.create("claude-a").await;
    let cases = [
        (switch_account(&session, "nowhere"), ErrorCode::NotFound),
        (
            switch_account(&session, "codex-work"),
            ErrorCode::BadRequest,
        ),
        (
            switch_provider(&session, "claude-b", None),
            ErrorCode::BadRequest,
        ),
        // No adapter runs Codex here.
        (
            switch_provider(&session, "codex-work", None),
            ErrorCode::Unsupported,
        ),
    ];
    for (command, code) in cases {
        let refused = daemon.handle(command.clone()).await.unwrap_err();
        assert_eq!(refused.code, code, "{command:?}: {refused:?}");
    }
    let journal = daemon.manager.read_since(&session, 0, 1000).await.unwrap();
    assert!(!journal.iter().any(|event| matches!(
        event.body,
        EventBody::AccountSwitched { .. } | EventBody::ProviderSwitched { .. }
    )));
    daemon.shutdown.cancel();
}

#[tokio::test]
async fn a_child_switches_to_any_account_like_its_primary() {
    let dir = tempfile::tempdir().unwrap();
    let created = |account: &str, parent: Option<SessionId>| EventBody::SessionCreated {
        repo: "/nowhere".into(),
        worktree: "/nowhere".into(),
        branch: "herder/x".into(),
        provider: Provider::Claude,
        account_id: AccountId::new(account),
        model: String::new(),
        permission_mode: PermissionMode::Ask,
        task: parent.as_ref().map(|_| "help".into()),
        parent,
        max_children: None,
        failover_pin: None,
    };
    let (primary, child) = (SessionId::new("primary"), SessionId::new("child"));
    let daemon = Switching::open(
        dir.path(),
        &[
            (Provider::Claude, Scripted::new(&[])),
            (Provider::Codex, Scripted::new(&[])),
        ],
        &[
            ("claude-a", Provider::Claude),
            ("claude-b", Provider::Claude),
            ("claude-spare", Provider::Claude),
            ("codex-spare", Provider::Codex),
        ],
        vec![
            (primary.clone(), created("claude-b", None)),
            (child.clone(), created("claude-a", Some(primary.clone()))),
        ],
    )
    .await;

    let applied = Ok(CommandResult::Applied);
    for switch in [
        // A switch to its own account changes nothing.
        switch_account(&child, "claude-a"),
        switch_account(&child, "claude-spare"),
        switch_account(&child, "claude-b"),
        switch_provider(&child, "codex-spare", None),
        // Neither its primary's account nor one that once opted in.
        switch_provider(&child, "claude-a", None),
    ] {
        assert_eq!(daemon.handle(switch).await, applied);
    }
    assert_eq!(
        daemon.handle(switch_account(&primary, "claude-a")).await,
        applied
    );
    daemon.shutdown.cancel();
}

fn window(name: &str, used_percent: f64, resets_at: &str) -> UsageWindow {
    UsageWindow {
        window: name.into(),
        used_percent,
        resets_at: Some(resets_at.parse::<Timestamp>().unwrap()),
    }
}

#[tokio::test]
async fn usage_a_session_reports_reaches_clients_with_its_account() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "usage.jsonl", Default::default()).await;
    let session = daemon.create().await;
    daemon
        .prompt(alice(), &session, "Refactor the parser.")
        .await;

    let five_hour = |used| window("five_hour", used, "2026-10-02T15:00:00Z");
    let seven_day = window("seven_day", 7.0, "2026-10-09T07:00:00Z");
    let accounts = daemon.next_accounts().await;
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].account_id, account());
    assert_eq!(accounts[0].usage, [five_hour(40.0), seven_day.clone()]);
    // The second report names only the five-hour window; the seven-day one stays.
    let accounts = daemon.next_accounts().await;
    assert_eq!(accounts[0].usage, [five_hour(41.5), seven_day.clone()]);
    assert_eq!(daemon.manager.accounts(), accounts);
    daemon.until_status(SessionStatus::Idle).await;
    daemon.stop().await;
}

/// A probe answering with how many times it ran, as the five-hour window's percentage.
struct Counting {
    requests: Arc<Mutex<Vec<StartRequest>>>,
}

impl Probe for Counting {
    fn read(&self, request: StartRequest) -> ProbeFuture {
        let mut requests = self.requests.lock().unwrap();
        requests.push(request);
        let used = requests.len() as f64;
        Box::pin(async move { Ok(vec![window("five_hour", used, "2026-10-02T15:00:00Z")]) })
    }
}

#[tokio::test]
async fn idle_accounts_are_probed_at_once_and_again_on_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "first.jsonl", Default::default()).await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let probe: Arc<dyn Probe> = Arc::new(Counting {
        requests: requests.clone(),
    });
    daemon
        .manager
        .track_usage(usage::Config {
            probes: Probes::from([(fake(), probe)]),
            dir: dir.path().join("usage"),
            interval: Duration::from_secs(3600),
            fresh: Duration::ZERO,
        })
        .unwrap();

    let accounts = daemon.next_accounts().await;
    assert_eq!(accounts[0].usage[0].used_percent, 1.0);
    // A client opening asks again; the interval alone would not for an hour.
    daemon.manager.refresh_usage();
    let accounts = daemon.next_accounts().await;
    assert_eq!(accounts[0].usage[0].used_percent, 2.0);

    // The probe runs under the account's own config dir, in the usage dir, which exists.
    let requests = requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].config_dir, Some(dir.path().join("account")));
    assert_eq!(requests[0].cwd, dir.path().join("usage"));
    assert_eq!(requests[0].permission_mode, PermissionMode::ReadOnly);
    assert!(requests[0].mcp.is_none() && requests[0].seed.is_empty());
    assert!(dir.path().join("usage").is_dir());
    daemon.stop().await;
}

const GIB: u64 = 1024 * 1024 * 1024;

/// A host whose readings the test sets.
#[derive(Clone)]
struct FakeHost(Arc<Mutex<Reading>>);

impl FakeHost {
    fn new(memory_available: u64) -> Self {
        let reading = Reading {
            memory_total: 16 * GIB,
            memory_available,
            load_1m: 0.5,
            cpu_percent: 10.0,
            pressure: None,
        };
        Self(Arc::new(Mutex::new(reading)))
    }

    fn set_memory_available(&self, bytes: u64) {
        self.0.lock().unwrap().memory_available = bytes;
    }
}

impl ReadHost for FakeHost {
    fn read(&self) -> anyhow::Result<Reading> {
        Ok(self.0.lock().unwrap().clone())
    }
}

/// Admits `daemon`'s turns on a 4-core `host`, at most `max_turns` at once, keeping a new
/// limit in `dir`'s `daemon.toml`.
fn admit(daemon: &Daemon, dir: &Path, max_turns: u32, host: &FakeHost) -> Arc<Admission> {
    let config = ResourcesConfig {
        max_turns: Some(max_turns),
        ..ResourcesConfig::default()
    };
    let admission = Arc::new(Admission::new(config.budget(4), Box::new(host.clone())));
    daemon
        .manager
        .admit_turns(Arc::clone(&admission), dir.join("daemon.toml"))
        .unwrap();
    admission
}

#[tokio::test]
async fn with_one_turn_allowed_a_second_sessions_turn_waits_until_the_first_ends() {
    let dir = tempfile::tempdir().unwrap();
    let scripts = ["hold.jsonl", "admitted.jsonl"];
    let mut daemon = Daemon::open_scripts(dir.path(), &scripts, Default::default()).await;
    let admission = admit(&daemon, dir.path(), 1, &FakeHost::new(8 * GIB));
    let first = daemon.create().await;
    let second = daemon.create().await;
    daemon.prompt(alice(), &first, "Hold the machine.").await;
    daemon
        .events_until(|body| matches!(body, EventBody::TurnStarted { .. }))
        .await;

    daemon.prompt(bob(), &second, "Wait your turn.").await;
    daemon.until_status(SessionStatus::WaitingForCapacity).await;
    let host = admission.resources();
    assert_eq!(
        (host.running_turns, host.waiting_turns, host.constraint),
        (1, 1, Some(Constraint::MaxTurns))
    );
    assert_eq!(
        describe(&daemon.journal(&second).await),
        ["alice: session_created", "-: status WaitingForCapacity"]
    );

    let interrupt = CommandBody::Interrupt {
        session_id: first.clone(),
    };
    daemon.manager.handle(alice(), interrupt).await.unwrap();
    let events = daemon
        .events_until(|body| matches!(body, EventBody::TurnCompleted { .. }))
        .await;
    let at = |what: &dyn Fn(&Event) -> bool| events.iter().position(what).unwrap();
    let interrupted =
        at(&|e| e.session_id == first && matches!(e.body, EventBody::TurnInterrupted { .. }));
    let admitted = at(&|e| {
        e.session_id == second
            && matches!(
                e.body,
                EventBody::SessionStatusChanged {
                    retry_at: None,
                    status: SessionStatus::Running
                }
            )
    });
    assert!(interrupted < admitted, "{:#?}", describe(&events));
    // The first session's own `idle` may come before or after this.
    daemon
        .until_event(|event| {
            event.session_id == second
                && event.body
                    == EventBody::SessionStatusChanged {
                        retry_at: None,
                        status: SessionStatus::Idle,
                    }
        })
        .await;
    assert_eq!(
        describe(&daemon.journal(&second).await),
        [
            "alice: session_created",
            "-: status WaitingForCapacity",
            "-: status Running",
            "bob: user turn-2 Wait your turn.",
            "-: turn_started turn-2",
            "-: assistant turn-2 Done.",
            "-: turn_completed turn-2",
            "-: status Idle",
        ]
    );
    let host = admission.resources();
    assert_eq!((host.running_turns, host.waiting_turns), (0, 0));
    daemon.stop().await;
}

#[tokio::test]
async fn a_raised_turn_limit_starts_a_waiting_turn_and_a_lowered_one_stops_none() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("daemon.toml");
    std::fs::write(&config, "# mine\n[resources]\nnice = 5\n").unwrap();
    let scripts = ["hold.jsonl", "hold_second.jsonl"];
    let mut daemon = Daemon::open_scripts(dir.path(), &scripts, Default::default()).await;
    let admission = admit(&daemon, dir.path(), 1, &FakeHost::new(8 * GIB));
    let first = daemon.create().await;
    let second = daemon.create().await;
    daemon.prompt(alice(), &first, "Hold the machine.").await;
    daemon
        .events_until(|body| matches!(body, EventBody::TurnStarted { .. }))
        .await;
    daemon.prompt(bob(), &second, "Wait your turn.").await;
    daemon.until_status(SessionStatus::WaitingForCapacity).await;

    let limit = |max_turns| CommandBody::SetResourceLimits { max_turns };
    for out_of_range in [0, herder_protocol::MAX_TURNS_LIMIT + 1] {
        let refused = daemon.manager.handle(alice(), limit(out_of_range)).await;
        assert_eq!(refused.unwrap_err().code, ErrorCode::BadRequest);
    }
    // Raised: the waiting turn starts at once, with no recheck.
    daemon.manager.handle(alice(), limit(2)).await.unwrap();
    daemon
        .until_event(|event| {
            event.session_id == second && matches!(event.body, EventBody::TurnStarted { .. })
        })
        .await;
    let host = admission.resources();
    assert_eq!(
        (host.running_turns, host.max_turns, host.waiting_turns),
        (2, 2, 0)
    );

    // Lowered: both turns run on; only new ones would wait.
    daemon.manager.handle(alice(), limit(1)).await.unwrap();
    let host = admission.resources();
    assert_eq!(
        (host.running_turns, host.max_turns, host.constraint),
        (2, 1, Some(Constraint::MaxTurns))
    );
    for session in [&first, &second] {
        let journal = describe(&daemon.journal(session).await);
        assert!(
            journal.last().unwrap().starts_with("-: turn_started"),
            "{journal:#?}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        "# mine\n[resources]\nnice = 5\nmax_turns = 1\n"
    );
    daemon.stop().await;
}

#[tokio::test]
async fn a_turn_waits_while_memory_is_short_and_starts_once_there_is_room() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "first.jsonl", Default::default()).await;
    let host = FakeHost::new(GIB);
    let admission = admit(&daemon, dir.path(), 4, &host);
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "First.").await;
    daemon.until_status(SessionStatus::WaitingForCapacity).await;
    assert_eq!(admission.resources().constraint, Some(Constraint::Memory));
    admission.recheck();
    assert!(daemon.starts.lock().unwrap().is_empty());

    host.set_memory_available(4 * GIB);
    admission.recheck();
    daemon.until_status(SessionStatus::Idle).await;
    let journal = describe(&daemon.journal(&session).await);
    assert_eq!(
        journal[1..4],
        [
            "-: status WaitingForCapacity",
            "-: status Running",
            "alice: user turn-1 First."
        ]
    );
    daemon.stop().await;
}

#[tokio::test]
async fn restart_keeps_a_prompt_waiting_for_capacity_and_runs_it_once_there_is_room() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "first.jsonl", Default::default()).await;
    admit(&daemon, dir.path(), 4, &FakeHost::new(GIB));
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "First.").await;
    daemon.until_status(SessionStatus::WaitingForCapacity).await;
    daemon.stop().await;

    let mut daemon = Daemon::open(dir.path(), "first.jsonl", Default::default()).await;
    let journal = describe(&daemon.journal(&session).await);
    assert_eq!(
        journal,
        ["alice: session_created", "-: status WaitingForCapacity"]
    );
    let host = FakeHost::new(GIB);
    let admission = admit(&daemon, dir.path(), 4, &host);
    daemon.manager.resume().await.unwrap();
    // Back in the host's line, without a command.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(admission.resources().waiting_turns, 1);
    host.set_memory_available(4 * GIB);
    admission.recheck();
    daemon.until_status(SessionStatus::Idle).await;
    assert_eq!(
        describe(&daemon.journal(&session).await),
        [
            "alice: session_created",
            "-: status WaitingForCapacity",
            "-: status Running",
            "alice: user turn-1 First.",
            "-: turn_started turn-1",
            "-: assistant turn-1 One.",
            "-: turn_completed turn-1",
            "-: status Idle",
        ]
    );
    daemon.stop().await;

    // Nothing is left queued to run again.
    let daemon = Daemon::open(dir.path(), "first.jsonl", Default::default()).await;
    daemon.manager.resume().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(daemon.starts.lock().unwrap().is_empty());
    assert_eq!(daemon.journal(&session).await.len(), 8);
    daemon.stop().await;
}

#[tokio::test]
async fn restart_mid_queue_fails_the_open_turn_then_runs_the_queued_prompt_once() {
    let dir = tempfile::tempdir().unwrap();
    let turns = Arc::new(AtomicU64::new(0));
    let mut daemon = Daemon::open(dir.path(), "hold.jsonl", turns.clone()).await;
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "Hold the machine.").await;
    daemon
        .events_until(|body| matches!(body, EventBody::TurnStarted { .. }))
        .await;
    daemon.prompt(bob(), &session, "Second.").await;
    daemon.stop().await;

    let mut daemon = Daemon::open(dir.path(), "second.jsonl", turns).await;
    daemon.manager.resume().await.unwrap();
    daemon.until_status(SessionStatus::Idle).await;
    assert_eq!(
        describe(&daemon.journal(&session).await),
        [
            "alice: session_created",
            "-: status Running",
            "alice: user turn-1 Hold the machine.",
            "-: turn_started turn-1",
            "-: turn_failed turn-1 Transient",
            "-: status NeedsYou",
            "-: status Running",
            "bob: user turn-2 Second.",
            "-: turn_started turn-2",
            "-: assistant turn-2 Two.",
            "-: turn_completed turn-2",
            "-: status Idle",
        ]
    );
    let starts = daemon.starts.lock().unwrap().clone();
    let [start] = starts.as_slice() else {
        panic!("expected one start, got {starts:?}");
    };
    assert_eq!(seed_texts(start), ["user: Hold the machine."]);
    daemon.stop().await;
}

#[tokio::test]
async fn a_command_resent_after_a_restart_is_not_applied_again() {
    let dir = tempfile::tempdir().unwrap();
    let turns = Arc::new(AtomicU64::new(0));
    let mut daemon = Daemon::open(dir.path(), "first.jsonl", turns.clone()).await;
    let create = CommandBody::CreateSession {
        repo: Some(daemon.repo.to_str().unwrap().to_owned()),
        project_id: None,
        branch: None,
        account_id: Some(account()),
        provider: None,
        model: None,
        permission_mode: Some(PermissionMode::Ask),
        max_children: None,
        failover_pin: None,
    };
    let (c1, c2) = (CommandId::new("c1"), CommandId::new("c2"));
    let created = daemon
        .manager
        .handle_once(alice(), c1.clone(), create.clone())
        .await
        .unwrap();
    let CommandResult::SessionCreated {
        session_id: session,
    } = created.clone()
    else {
        panic!("expected a created session, got {created:?}");
    };
    let prompt = |text: &str| CommandBody::SendPrompt {
        session_id: session.clone(),
        text: text.into(),
        images: Vec::new(),
    };
    let sent = daemon
        .manager
        .handle_once(alice(), c2.clone(), prompt("First."));
    assert_eq!(sent.await, Ok(CommandResult::Applied));
    daemon.until_status(SessionStatus::Idle).await;
    daemon.stop().await;

    // The client never saw the answers, so it resends both on its next connection.
    let mut daemon = Daemon::open(dir.path(), "second.jsonl", turns).await;
    let resent = daemon.manager.handle_once(alice(), c1, create).await;
    assert_eq!(resent, Ok(created));
    let resent = daemon
        .manager
        .handle_once(alice(), c2.clone(), prompt("First."));
    assert_eq!(resent.await, Ok(CommandResult::Applied));
    assert_eq!(daemon.manager.sessions().await.unwrap().len(), 1);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(daemon.starts.lock().unwrap().is_empty());
    assert_eq!(daemon.journal(&session).await.len(), 7);

    // Command ids are per user: bob's `c2` is a command of its own.
    let sent = daemon.manager.handle_once(bob(), c2, prompt("Second."));
    assert_eq!(sent.await, Ok(CommandResult::Applied));
    daemon.until_status(SessionStatus::Idle).await;
    let journal = describe(&daemon.journal(&session).await);
    let prompts: Vec<_> = journal
        .iter()
        .filter(|line| line.contains(": user "))
        .collect();
    assert_eq!(
        prompts,
        ["alice: user turn-1 First.", "bob: user turn-2 Second."]
    );
    daemon.stop().await;
}

impl Switching {
    /// Sends a prompt from alice.
    async fn send(&self, session_id: &SessionId, text: &str) {
        let prompt = CommandBody::SendPrompt {
            session_id: session_id.clone(),
            text: text.into(),
            images: Vec::new(),
        };
        assert_eq!(self.handle(prompt).await, Ok(CommandResult::Applied));
    }

    /// Waits for `session_id` to settle on `status`; returns its whole journal.
    async fn settled(&mut self, session_id: &SessionId, status: SessionStatus) -> Vec<Event> {
        self.until(|body| {
            *body
                == EventBody::SessionStatusChanged {
                    status,
                    retry_at: None,
                }
        })
        .await;
        self.manager.read_since(session_id, 0, 1000).await.unwrap()
    }
}

/// The journal from the first turn on, without the session's creation and worktree events.
fn from_first_turn(journal: &[Event]) -> Vec<String> {
    let described = describe(journal);
    let start = described
        .iter()
        .position(|line| line.contains("status Running"))
        .unwrap();
    described[start..].to_vec()
}

#[tokio::test]
async fn a_limit_on_one_account_rotates_the_turn_to_another_of_its_provider() {
    let dir = tempfile::tempdir().unwrap();
    let claude = Scripted::new(&["failover_limit.jsonl", "failover_retry.jsonl"]);
    let mut daemon = Switching::open(
        dir.path(),
        &[(Provider::Claude, claude.clone())],
        &[
            ("claude-a", Provider::Claude),
            // Every account takes part; with no usage known, the first by id has most room.
            ("claude-b", Provider::Claude),
            ("claude-c", Provider::Claude),
            ("codex", Provider::Codex),
        ],
        Vec::new(),
    )
    .await;
    let session = daemon.create("claude-a").await;
    daemon.send(&session, "Refactor the parser.").await;

    let journal = daemon.settled(&session, SessionStatus::Idle).await;
    assert_eq!(
        from_first_turn(&journal),
        [
            "-: status Running",
            "alice: user turn-1 Refactor the parser.",
            "-: turn_started turn-1",
            "-: assistant turn-1 Splitting the lexer out.",
            "-: turn_failed turn-1 LimitReached",
            "-: account_switched claude-b",
            "alice: user turn-2 Refactor the parser.",
            "-: turn_started turn-2",
            "-: assistant turn-2 Refactored the parser.",
            "-: turn_completed turn-2",
            "-: status Idle",
        ]
    );
    let starts = claude.starts();
    let [on_a, on_b] = starts.as_slice() else {
        panic!("expected two starts, got {starts:?}");
    };
    assert_eq!(on_a.config_dir, Some(dir.path().join("claude-a")));
    assert_eq!(on_b.config_dir, Some(dir.path().join("claude-b")));
    assert_eq!(
        seed_texts(on_b),
        [
            "user: Refactor the parser.",
            "assistant: Splitting the lexer out."
        ]
    );
    daemon.shutdown.cancel();
}

#[tokio::test]
async fn a_pinned_session_does_not_fail_over() {
    let dir = tempfile::tempdir().unwrap();
    let claude = Scripted::new(&["failover_limit.jsonl"]);
    let mut daemon = Switching::open(
        dir.path(),
        &[(Provider::Claude, claude.clone())],
        &[
            ("claude-a", Provider::Claude),
            ("claude-b", Provider::Claude),
        ],
        Vec::new(),
    )
    .await;
    let pin = FailoverConfig { pin: true };
    daemon.manager.configure_failover(pin).unwrap();
    let session = daemon.create("claude-a").await;
    daemon.send(&session, "Refactor the parser.").await;

    let journal = daemon.settled(&session, SessionStatus::NeedsYou).await;
    let lines = from_first_turn(&journal);
    assert_eq!(
        lines[lines.len() - 2..],
        ["-: turn_failed turn-1 LimitReached", "-: status NeedsYou"]
    );
    assert_eq!(claude.starts().len(), 1);
    daemon.shutdown.cancel();
}

#[tokio::test]
async fn a_session_pinned_at_creation_does_not_fail_over() {
    let dir = tempfile::tempdir().unwrap();
    let claude = Scripted::new(&["failover_limit.jsonl"]);
    let mut daemon = Switching::open(
        dir.path(),
        &[(Provider::Claude, claude.clone())],
        &[
            ("claude-a", Provider::Claude),
            ("claude-b", Provider::Claude),
        ],
        Vec::new(),
    )
    .await;
    let session = daemon.create_pinned("claude-a", Some(true)).await;
    daemon.send(&session, "Refactor the parser.").await;

    let journal = daemon.settled(&session, SessionStatus::NeedsYou).await;
    let lines = from_first_turn(&journal);
    assert_eq!(
        lines[lines.len() - 2..],
        ["-: turn_failed turn-1 LimitReached", "-: status NeedsYou"]
    );
    assert_eq!(claude.starts().len(), 1);
    daemon.shutdown.cancel();
}

#[tokio::test]
async fn a_session_unpinned_at_creation_fails_over_on_a_pinning_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let claude = Scripted::new(&["failover_limit.jsonl", "failover_retry.jsonl"]);
    let mut daemon = Switching::open(
        dir.path(),
        &[(Provider::Claude, claude.clone())],
        &[
            ("claude-a", Provider::Claude),
            ("claude-b", Provider::Claude),
        ],
        Vec::new(),
    )
    .await;
    let pin = FailoverConfig { pin: true };
    daemon.manager.configure_failover(pin).unwrap();
    let session = daemon.create_pinned("claude-a", Some(false)).await;
    daemon.send(&session, "Refactor the parser.").await;

    let journal = daemon.settled(&session, SessionStatus::Idle).await;
    assert!(
        from_first_turn(&journal).contains(&"-: account_switched claude-b".to_owned()),
        "{journal:?}"
    );
    assert_eq!(claude.starts().len(), 2);
    daemon.shutdown.cancel();
}

#[tokio::test]
async fn an_account_that_hit_its_limit_is_passed_over_and_with_none_left_the_session_needs_you() {
    let dir = tempfile::tempdir().unwrap();
    let claude = Scripted::new(&[
        "failover_limit.jsonl",
        "failover_retry.jsonl",
        "failover_limit_again.jsonl",
    ]);
    let mut daemon = Switching::open(
        dir.path(),
        &[(Provider::Claude, claude.clone())],
        &[
            ("claude-a", Provider::Claude),
            ("claude-b", Provider::Claude),
        ],
        Vec::new(),
    )
    .await;
    let first = daemon.create("claude-a").await;
    daemon.send(&first, "Refactor the parser.").await;
    daemon.settled(&first, SessionStatus::Idle).await;

    // claude-a, which just hit its limit, is not chosen again before it resets.
    let second = daemon.create("claude-b").await;
    daemon.send(&second, "Add tests.").await;
    let journal = daemon.settled(&second, SessionStatus::NeedsYou).await;
    let lines = from_first_turn(&journal);
    assert_eq!(
        lines[lines.len() - 2..],
        ["-: turn_failed turn-3 LimitReached", "-: status NeedsYou"]
    );
    assert!(
        !lines.iter().any(|line| line.contains("switched")),
        "{lines:?}"
    );
    assert_eq!(claude.starts().len(), 3);
    daemon.shutdown.cancel();
}

#[tokio::test]
async fn a_retry_that_hits_a_limit_too_is_not_retried_again() {
    let dir = tempfile::tempdir().unwrap();
    let claude = Scripted::new(&["failover_limit.jsonl", "failover_retry_limit.jsonl"]);
    let mut daemon = Switching::open(
        dir.path(),
        &[(Provider::Claude, claude.clone())],
        &[
            ("claude-a", Provider::Claude),
            ("claude-b", Provider::Claude),
            ("claude-c", Provider::Claude),
        ],
        Vec::new(),
    )
    .await;
    let session = daemon.create("claude-a").await;
    daemon.send(&session, "Refactor the parser.").await;

    let journal = daemon.settled(&session, SessionStatus::NeedsYou).await;
    let lines = from_first_turn(&journal);
    assert_eq!(
        lines[4..],
        [
            "-: turn_failed turn-1 LimitReached",
            "-: account_switched claude-b",
            "alice: user turn-2 Refactor the parser.",
            "-: turn_started turn-2",
            "-: turn_failed turn-2 LimitReached",
            "-: status NeedsYou",
        ]
    );
    assert_eq!(claude.starts().len(), 2);
    daemon.shutdown.cancel();
}

#[tokio::test]
async fn failover_keeps_the_sessions_provider_and_model() {
    let dir = tempfile::tempdir().unwrap();
    let claude = Scripted::new(&["failover_limit.jsonl", "failover_retry.jsonl"]);
    let codex = Scripted::new(&[]);
    let mut daemon = Switching::open(
        dir.path(),
        &[
            (Provider::Claude, claude.clone()),
            (Provider::Codex, codex.clone()),
        ],
        &[
            ("claude-a", Provider::Claude),
            // Opted in and first by id, but of another provider.
            ("a-codex", Provider::Codex),
            ("claude-b", Provider::Claude),
        ],
        Vec::new(),
    )
    .await;
    let session = daemon
        .create_on("claude-a", Some("claude-opus-4-5"), None)
        .await;
    daemon.send(&session, "Refactor the parser.").await;

    let journal = daemon.settled(&session, SessionStatus::Idle).await;
    let lines = from_first_turn(&journal);
    assert_eq!(
        lines[4..7],
        [
            "-: turn_failed turn-1 LimitReached",
            "-: account_switched claude-b",
            "alice: user turn-2 Refactor the parser.",
        ]
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("model_switched") || line.contains("provider_switched")),
        "{lines:?}"
    );
    let starts = claude.starts();
    let [on_a, on_b] = starts.as_slice() else {
        panic!("expected two starts, got {starts:?}");
    };
    assert_eq!(on_b.config_dir, Some(dir.path().join("claude-b")));
    assert_eq!(on_a.model.as_deref(), Some("claude-opus-4-5"));
    assert_eq!(on_b.model.as_deref(), Some("claude-opus-4-5"));
    assert!(codex.starts().is_empty());
    daemon.shutdown.cancel();
}

#[tokio::test]
async fn a_failover_target_that_rejects_the_model_leaves_the_session_needing_you() {
    let dir = tempfile::tempdir().unwrap();
    let claude = Scripted::new(&[
        "failover_limit.jsonl",
        "failover_retry_model_rejected.jsonl",
    ]);
    let codex = Scripted::new(&[]);
    let mut daemon = Switching::open(
        dir.path(),
        &[
            (Provider::Claude, claude.clone()),
            (Provider::Codex, codex.clone()),
        ],
        &[
            ("claude-a", Provider::Claude),
            ("claude-b", Provider::Claude),
            // Eligible too: the session moves on to neither.
            ("claude-c", Provider::Claude),
            ("codex", Provider::Codex),
        ],
        Vec::new(),
    )
    .await;
    let session = daemon
        .create_on("claude-a", Some("claude-opus-4-5"), None)
        .await;
    daemon.send(&session, "Refactor the parser.").await;

    let journal = daemon.settled(&session, SessionStatus::NeedsYou).await;
    let lines = from_first_turn(&journal);
    assert_eq!(
        lines[4..],
        [
            "-: turn_failed turn-1 LimitReached",
            "-: account_switched claude-b",
            "alice: user turn-2 Refactor the parser.",
            "-: turn_started turn-2",
            "-: turn_failed turn-2 Fatal",
            "-: status NeedsYou",
        ]
    );
    let starts = claude.starts();
    assert_eq!(starts.len(), 2, "{starts:?}");
    assert_eq!(starts[1].model.as_deref(), Some("claude-opus-4-5"));
    assert!(codex.starts().is_empty());
    daemon.shutdown.cancel();
}

#[tokio::test]
async fn an_opencode_account_at_its_limit_fails_over_to_another_by_replay() {
    let dir = tempfile::tempdir().unwrap();
    let opencode = Scripted::opencode(&["opencode_limit.jsonl", "opencode_retry.jsonl"]);
    let mut daemon = Switching::open(
        dir.path(),
        &[(Provider::Opencode, opencode.clone())],
        &[
            ("opencode-a", Provider::Opencode),
            ("opencode-b", Provider::Opencode),
        ],
        Vec::new(),
    )
    .await;
    let session = daemon.create("opencode-a").await;
    daemon.send(&session, "reply with the word ok").await;

    let journal = daemon.settled(&session, SessionStatus::Idle).await;
    let lines = from_first_turn(&journal);
    assert_eq!(
        lines,
        [
            "-: status Running",
            "alice: user turn-1 reply with the word ok",
            // The model OpenCode reports through its model config option.
            "-: model_switched opencode/big-pickle",
            "-: turn_started turn-1",
            "-: turn_failed turn-1 LimitReached",
            "-: account_switched opencode-b",
            "alice: user turn-2 reply with the word ok",
            "-: turn_started turn-2",
            "-: assistant turn-2 ok",
            "-: turn_completed turn-2",
            "-: status Idle",
        ]
    );
    let starts = opencode.starts();
    let [on_a, on_b] = starts.as_slice() else {
        panic!("expected two starts, got {starts:?}");
    };
    assert_eq!(on_a.config_dir, Some(dir.path().join("opencode-a")));
    assert_eq!(on_b.config_dir, Some(dir.path().join("opencode-b")));
    // The retry's recording only matches a prompt that carries this transcript.
    assert_eq!(seed_texts(on_b), ["user: reply with the word ok"]);
    daemon.shutdown.cancel();
}

/// Gives `daemon`'s repo the setup command `command`, which may run for `timeout`.
fn set_up_with(daemon: &Daemon, command: &str, timeout: Duration) {
    let dir = daemon.repo.parent().unwrap();
    let projects = ProjectsConfig {
        setup_timeout: timeout,
        entries: vec![ProjectEntry {
            paths: vec![daemon.repo.clone()],
            setup_command: Some(command.to_owned()),
            ..ProjectEntry::default()
        }],
        ..ProjectsConfig::default()
    };
    let overrides = Overrides::new(dir.join("daemon.toml"), projects);
    daemon
        .manager
        .manage_projects(HostId::new("host-1"), Arc::new(overrides))
        .unwrap();
}

/// The error the session's failed turn journaled.
fn turn_error(events: &[Event]) -> TurnError {
    events
        .iter()
        .find_map(|event| match &event.body {
            EventBody::TurnFailed { error, .. } => Some(error.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no turn failed: {:#?}", describe(events)))
}

#[tokio::test]
async fn a_new_worktree_runs_its_setup_command_once_before_the_first_turn() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "after_setup.jsonl", Default::default()).await;
    // Ends only once the test lets it, so the prompt surely arrives while it runs.
    let go = dir.path().join("go");
    let command = format!(
        "until [ -e '{}' ]; do sleep 0.01; done; echo ran >> .setup-runs; echo ready; echo warn >&2",
        go.display()
    );
    set_up_with(&daemon, &command, Duration::from_secs(60));
    let session = daemon.create().await;
    // Sent while the setup runs: it waits for it.
    daemon.prompt(alice(), &session, "First.").await;
    std::fs::write(&go, "").unwrap();
    daemon.until_status(SessionStatus::Idle).await;
    daemon.prompt(alice(), &session, "Second.").await;
    daemon.until_status(SessionStatus::Idle).await;

    let journal = daemon.journal(&session).await;
    assert_eq!(
        describe(&journal),
        vec![
            "alice: session_created",
            "-: status Running",
            "-: turn_started turn-1",
            "-: tool_call herder_setup",
            "-: tool_result ready\nwarn\n",
            "-: turn_completed turn-1",
            "alice: user turn-2 First.",
            "-: turn_started turn-2",
            "-: assistant turn-2 One.",
            "-: turn_completed turn-2",
            "-: status Idle",
            "-: status Running",
            "alice: user turn-3 Second.",
            "-: turn_started turn-3",
            "-: assistant turn-3 Two.",
            "-: turn_completed turn-3",
            "-: status Idle",
        ]
    );
    let call = journal
        .iter()
        .find_map(|event| match &event.body {
            EventBody::ItemAdded { item } => match &item.body {
                ItemBody::ToolCall { input, .. } => Some(input.clone()),
                _ => None,
            },
            _ => None,
        })
        .unwrap();
    assert_eq!(call, serde_json::json!({ "command": command }));
    let worktree = daemon.manager.worktree(&session).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(worktree.join(".setup-runs")).unwrap(),
        "ran\n"
    );
    assert_eq!(daemon.starts.lock().unwrap().len(), 1);
    daemon.stop().await;
}

#[tokio::test]
async fn a_failing_setup_command_marks_the_session_error_and_blocks_the_first_turn() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "after_setup.jsonl", Default::default()).await;
    // Fails only once the test lets it, so the prompt surely arrives while it runs.
    let go = dir.path().join("go");
    let command = format!(
        "until [ -e '{}' ]; do sleep 0.01; done; echo missing .env; exit 2",
        go.display()
    );
    set_up_with(&daemon, &command, Duration::from_secs(60));
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "First.").await;
    std::fs::write(&go, "").unwrap();
    let events = daemon.until_status(SessionStatus::Error).await;

    assert_eq!(
        turn_error(&events),
        TurnError {
            class: ErrorClass::Fatal,
            message: format!(
                "the setup command `{command}` failed (exit status: 2):\nmissing .env"
            ),
        }
    );
    let result = events.iter().find_map(|event| match &event.body {
        EventBody::ItemAdded { item } => match &item.body {
            ItemBody::ToolResult { is_error, .. } => Some(*is_error),
            _ => None,
        },
        _ => None,
    });
    assert_eq!(result, Some(true));
    // The prompt sent during the setup never reached an agent.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(daemon.starts.lock().unwrap().is_empty());
    assert_eq!(
        describe(&daemon.journal(&session).await),
        [
            "alice: session_created",
            "-: status Running",
            "-: turn_started turn-1",
            "-: tool_call herder_setup",
            "-: tool_result missing .env\n",
            "-: turn_failed turn-1 Fatal",
            "-: status Error",
        ]
    );
    daemon.stop().await;
}

#[tokio::test]
async fn a_failed_setup_runs_again_before_the_next_prompt_starts_the_agent() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon =
        Daemon::open(dir.path(), "after_retried_setup.jsonl", Default::default()).await;
    // Fails the first time only, as a missing `.env` the user then adds would.
    set_up_with(
        &daemon,
        "if [ -e .tried ]; then echo ready; else touch .tried; echo missing .env; exit 2; fi",
        Duration::from_secs(60),
    );
    let session = daemon.create().await;
    daemon.until_status(SessionStatus::Error).await;
    daemon.prompt(alice(), &session, "First.").await;
    daemon.until_status(SessionStatus::Idle).await;

    assert_eq!(
        describe(&daemon.journal(&session).await),
        [
            "alice: session_created",
            "-: status Running",
            "-: turn_started turn-1",
            "-: tool_call herder_setup",
            "-: tool_result missing .env\n",
            "-: turn_failed turn-1 Fatal",
            "-: status Error",
            "-: status Running",
            "-: turn_started turn-2",
            "-: tool_call herder_setup",
            "-: tool_result ready\n",
            "-: turn_completed turn-2",
            "alice: user turn-3 First.",
            "-: turn_started turn-3",
            "-: assistant turn-3 One.",
            "-: turn_completed turn-3",
            "-: status Idle",
        ]
    );
    assert_eq!(daemon.starts.lock().unwrap().len(), 1);
    daemon.stop().await;
}

#[tokio::test]
async fn a_setup_cut_short_by_a_restart_runs_again_before_the_next_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let turns = Arc::new(AtomicU64::new(0));
    let mut daemon = Daemon::open(dir.path(), "after_retried_setup.jsonl", turns.clone()).await;
    set_up_with(&daemon, "sleep 30", Duration::from_secs(60));
    let session = daemon.create().await;
    daemon
        .events_until(|body| matches!(body, EventBody::ItemAdded { .. }))
        .await;
    daemon.stop().await;

    let mut daemon = Daemon::open(dir.path(), "after_retried_setup.jsonl", turns).await;
    set_up_with(&daemon, "echo ready", Duration::from_secs(60));
    daemon.prompt(alice(), &session, "First.").await;
    daemon.until_status(SessionStatus::Idle).await;
    assert_eq!(
        describe(&daemon.journal(&session).await)[4..],
        [
            "-: turn_failed turn-1 Transient",
            "-: status NeedsYou",
            "-: status Running",
            "-: turn_started turn-2",
            "-: tool_call herder_setup",
            "-: tool_result ready\n",
            "-: turn_completed turn-2",
            "alice: user turn-3 First.",
            "-: turn_started turn-3",
            "-: assistant turn-3 One.",
            "-: turn_completed turn-3",
            "-: status Idle",
        ]
    );
    daemon.stop().await;
}

#[tokio::test]
async fn an_agent_oom_killed_in_its_scope_fails_clearly_and_restarts_seeded() {
    let dir = tempfile::tempdir().unwrap();
    let scripts = ["oom_killed.jsonl", "second.jsonl"];
    let mut daemon = Daemon::open_scripts(dir.path(), &scripts, Default::default()).await;
    let scopes = Scopes::new(
        ResourcesConfig::default(),
        Host {
            memory_total: 10 * 1024 * 1024 * 1024,
            cores: 2,
            nice: 0,
        },
        true,
    )
    .with_oom_check(Box::new(|_| Box::pin(async { true })));
    let limit = scopes.limits(false).memory_max / (1024 * 1024);
    daemon.manager.limit_resources(Arc::new(scopes)).unwrap();
    let session = daemon.create().await;
    // Queued behind the turn the kill ends: it must not go to the dying CLI.
    let held = daemon.hold().await;
    daemon.prompt(alice(), &session, "First.").await;
    daemon.prompt(alice(), &session, "Second.").await;
    drop(held);
    daemon.until_status(SessionStatus::Idle).await;

    let journal = daemon.journal(&session).await;
    assert_eq!(
        turn_error(&journal),
        TurnError {
            class: ErrorClass::Fatal,
            message: format!(
                "the agent ran out of memory: the kernel killed it, or a process it ran, at its \
                 session's {limit} MiB limit (claude was killed by a signal). The next prompt \
                 restarts the agent from the session's transcript"
            ),
        }
    );
    assert_eq!(
        describe(&journal)[1..],
        [
            "-: status Running",
            "alice: user turn-1 First.",
            "-: turn_started turn-1",
            "-: assistant turn-1 Building.",
            "-: turn_failed turn-1 Fatal",
            "alice: user turn-2 Second.",
            "-: turn_started turn-2",
            "-: assistant turn-2 Two.",
            "-: turn_completed turn-2",
            "-: status Idle",
        ]
    );
    let starts = daemon.starts.lock().unwrap().clone();
    assert_eq!(starts.len(), 2);
    assert_eq!(
        seed_texts(&starts[1]),
        ["user: First.", "assistant: Building."]
    );
    daemon.stop().await;
}

#[tokio::test]
async fn an_agent_oom_killed_mid_turn_leaves_the_session_error_until_the_next_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let scripts = ["oom_killed.jsonl", "second.jsonl"];
    let mut daemon = Daemon::open_scripts(dir.path(), &scripts, Default::default()).await;
    let scopes = Scopes::new(
        ResourcesConfig::default(),
        Host {
            memory_total: 10 * 1024 * 1024 * 1024,
            cores: 2,
            nice: 0,
        },
        true,
    )
    .with_oom_check(Box::new(|_| Box::pin(async { true })));
    daemon.manager.limit_resources(Arc::new(scopes)).unwrap();
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "First.").await;
    daemon.until_status(SessionStatus::Error).await;
    daemon.prompt(alice(), &session, "Second.").await;
    daemon.until_status(SessionStatus::Idle).await;
    let starts = daemon.starts.lock().unwrap().clone();
    assert_eq!(starts.len(), 2);
    assert_eq!(
        seed_texts(&starts[1]),
        ["user: First.", "assistant: Building."]
    );
    daemon.stop().await;
}

#[tokio::test]
async fn a_setup_command_past_its_timeout_is_killed_and_fails_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "after_setup.jsonl", Default::default()).await;
    set_up_with(
        &daemon,
        "echo installing; sleep 30",
        Duration::from_millis(300),
    );
    let started = std::time::Instant::now();
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "First.").await;
    let events = daemon.until_status(SessionStatus::Error).await;

    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(
        turn_error(&events).message,
        "the setup command `echo installing; sleep 30` timed out after 300 ms:\ninstalling"
    );
    assert!(daemon.starts.lock().unwrap().is_empty());
    daemon.stop().await;
}

#[tokio::test]
async fn a_running_setup_command_blocks_archive_and_stops_on_interrupt() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "after_setup.jsonl", Default::default()).await;
    set_up_with(&daemon, "sleep 30", Duration::from_secs(60));
    let session = daemon.create().await;

    let archive = daemon.manager.archive(alice(), session.clone(), true).await;
    assert_eq!(archive.unwrap_err().code, ErrorCode::Conflict);
    let interrupt = CommandBody::Interrupt {
        session_id: session.clone(),
    };
    assert_eq!(
        daemon.manager.handle(alice(), interrupt).await.unwrap(),
        CommandResult::Applied
    );
    let events = daemon.until_status(SessionStatus::Error).await;
    assert_eq!(
        turn_error(&events).message,
        "the setup command `sleep 30` was interrupted"
    );
    daemon.stop().await;
}

fn png() -> Image {
    Image {
        media_type: "image/png".into(),
        data: Bytes(b"\x89PNG\r\n\x1a\npng".to_vec()),
    }
}

fn prompt_with(session_id: &SessionId, text: &str, images: Vec<Image>) -> CommandBody {
    CommandBody::SendPrompt {
        session_id: session_id.clone(),
        text: text.into(),
        images,
    }
}

/// The attachments of the first prompt in `events`.
fn attachments(events: &[Event]) -> Vec<Attachment> {
    events
        .iter()
        .find_map(|event| match &event.body {
            EventBody::ItemAdded { item } => match &item.body {
                ItemBody::UserMessage { attachments, .. } => Some(attachments.clone()),
                _ => None,
            },
            _ => None,
        })
        .expect("a prompt")
}

#[tokio::test]
async fn a_prompts_images_reach_the_agent_and_clients_fetch_them() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "image.jsonl", Default::default()).await;
    let session = daemon.create().await;
    let prompt = prompt_with(&session, "Like this.", vec![png()]);
    assert_eq!(
        daemon.manager.handle(alice(), prompt).await,
        Ok(CommandResult::Applied)
    );
    let events = daemon.until_status(SessionStatus::Idle).await;
    // The script only matches a prompt that carries the image.
    assert!(
        describe(&events).contains(&"-: assistant turn-1 Done.".to_owned()),
        "{:?}",
        describe(&events)
    );
    let kept = attachments(&events);
    assert_eq!(kept.len(), 1);
    assert_eq!(
        (kept[0].media_type.as_str(), kept[0].size),
        ("image/png", 11)
    );
    let sent: Vec<_> = daemon
        .commands
        .lock()
        .unwrap()
        .iter()
        .filter_map(|command| match command {
            AdapterCommand::SendPrompt { images, .. } => Some(images.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(sent, [vec![png()]]);

    // Any user fetches it, as often as asked: a fetch is not remembered as applied.
    let fetch = |attachment_id: AttachmentId| CommandBody::GetAttachment {
        session_id: session.clone(),
        attachment_id,
    };
    let fetched = CommandResult::Attachment {
        media_type: "image/png".into(),
        data: png().data,
    };
    for _ in 0..2 {
        let id = CommandId::new("fetch-1");
        let answer = daemon
            .manager
            .handle_once(bob(), id, fetch(kept[0].attachment_id.clone()))
            .await;
        assert_eq!(answer, Ok(fetched.clone()));
    }
    let missing = fetch(AttachmentId::new("01J9NOPE"));
    let error = daemon.manager.handle(bob(), missing).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    let elsewhere = CommandBody::GetAttachment {
        session_id: SessionId::new("nope"),
        attachment_id: kept[0].attachment_id.clone(),
    };
    let error = daemon.manager.handle(bob(), elsewhere).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    daemon.stop().await;
}

#[tokio::test]
async fn images_that_are_not_images_or_go_to_a_cli_without_images_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path(), "first.jsonl", Default::default()).await;
    let session = daemon.create().await;
    let fake_png = Image {
        media_type: "image/png".into(),
        data: Bytes(b"GIF89a".to_vec()),
    };
    let prompt = prompt_with(&session, "First.", vec![fake_png]);
    let error = daemon.manager.handle(alice(), prompt).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest, "{error:?}");
    daemon.stop().await;

    let dir = tempfile::tempdir().unwrap();
    let mut blind = FakeAdapter::new(fixture("first.jsonl"));
    blind.images = false;
    let recording = Recording {
        adapter: Box::new(blind),
        starts: Default::default(),
        commands: Default::default(),
        gate: Default::default(),
    };
    let (starts, commands) = (recording.starts.clone(), recording.commands.clone());
    let daemon = Daemon::open_with(
        dir.path(),
        Arc::new(recording),
        starts,
        commands,
        Default::default(),
        Default::default(),
    )
    .await;
    let session = daemon.create().await;
    let prompt = prompt_with(&session, "First.", vec![png()]);
    let error = daemon.manager.handle(alice(), prompt).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Unsupported, "{error:?}");
    // Nothing was queued or journaled.
    assert_eq!(daemon.journal(&session).await.len(), 1);
    daemon.stop().await;
}

/// A probe answering each account's five-hour window from `used`, by its config dir's name.
struct ByAccount {
    used: Vec<(&'static str, f64)>,
}

impl Probe for ByAccount {
    fn read(&self, request: StartRequest) -> ProbeFuture {
        let name = request
            .config_dir
            .as_deref()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();
        let used = self
            .used
            .iter()
            .find(|(account, _)| *account == name)
            .map_or(0.0, |(_, used)| *used);
        Box::pin(async move { Ok(vec![window("five_hour", used, "2099-01-01T00:00:00Z")]) })
    }
}

#[tokio::test]
async fn a_session_created_by_provider_starts_on_its_account_with_most_room() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Switching::open(
        dir.path(),
        &[
            (Provider::Claude, Scripted::new(&[])),
            (Provider::Codex, Scripted::new(&[])),
        ],
        &[
            ("claude-a", Provider::Claude),
            ("claude-b", Provider::Claude),
            ("codex", Provider::Codex),
        ],
        Vec::new(),
    )
    .await;
    let probe: Arc<dyn Probe> = Arc::new(ByAccount {
        used: vec![("claude-a", 90.0), ("claude-b", 20.0)],
    });
    daemon
        .manager
        .track_usage(usage::Config {
            probes: Probes::from([(Provider::Claude, probe)]),
            dir: dir.path().join("usage"),
            interval: Duration::from_secs(3600),
            fresh: Duration::ZERO,
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while daemon
            .manager
            .accounts()
            .iter()
            .filter(|account| account.provider == Provider::Claude)
            .any(|account| account.usage.is_empty())
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("usage of both claude accounts");

    let create =
        |account_id: Option<&str>, provider: Option<Provider>| CommandBody::CreateSession {
            repo: Some(daemon.repo.to_str().unwrap().to_owned()),
            project_id: None,
            branch: None,
            account_id: account_id.map(AccountId::new),
            provider,
            model: None,
            permission_mode: None,
            max_children: None,
            failover_pin: None,
        };
    let Ok(CommandResult::SessionCreated { session_id }) =
        daemon.handle(create(None, Some(Provider::Claude))).await
    else {
        panic!("expected a session");
    };
    let journal = daemon.manager.read_since(&session_id, 0, 10).await.unwrap();
    let EventBody::SessionCreated {
        account_id,
        permission_mode,
        ..
    } = &journal[0].body
    else {
        panic!("expected session_created");
    };
    // Without a mode, nor a project default, a session asks.
    assert_eq!(
        (account_id, *permission_mode),
        (&AccountId::new("claude-b"), PermissionMode::Ask)
    );

    let code = |result: Result<CommandResult, ErrorInfo>| result.unwrap_err().code;
    let mismatch = create(Some("codex"), Some(Provider::Claude));
    assert_eq!(code(daemon.handle(mismatch).await), ErrorCode::BadRequest);
    let none = create(None, Some(Provider::Grok));
    assert_eq!(code(daemon.handle(none).await), ErrorCode::NotFound);
    let neither = create(None, None);
    assert_eq!(code(daemon.handle(neither).await), ErrorCode::BadRequest);
    daemon.shutdown.cancel();
}

#[tokio::test]
async fn a_session_starts_in_its_projects_default_permission_mode_unless_given_one() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path(), "first.jsonl", Default::default()).await;
    let repo = daemon.repo.to_str().unwrap().to_owned();
    daemon
        .manager
        .set_projects(&[Project {
            project_id: ProjectId::new("github.com/org/app"),
            name: "app".into(),
            paths: vec![repo.clone()],
            default_permission_mode: Some(PermissionMode::FullAccess),
            default_account: Some(account()),
            setup_command: None,
            icon: None,
        }])
        .await;
    for (given, started) in [
        (None, PermissionMode::FullAccess),
        (Some(PermissionMode::ReadOnly), PermissionMode::ReadOnly),
    ] {
        let create = CommandBody::CreateSession {
            repo: None,
            project_id: Some(ProjectId::new("github.com/org/app")),
            branch: None,
            account_id: None,
            provider: None,
            model: None,
            permission_mode: given,
            max_children: None,
            failover_pin: None,
        };
        let Ok(CommandResult::SessionCreated { session_id }) =
            daemon.manager.handle(alice(), create).await
        else {
            panic!("expected a session");
        };
        let journal = daemon.journal(&session_id).await;
        let EventBody::SessionCreated {
            permission_mode, ..
        } = &journal[0].body
        else {
            panic!("expected session_created");
        };
        assert_eq!(*permission_mode, started);
    }
    daemon.stop().await;
}

#[tokio::test]
async fn unarchive_brings_the_worktree_back_on_the_kept_branch_and_the_session_runs_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "first.jsonl", Default::default()).await;
    let session = daemon.create().await;
    let created = daemon.journal(&session).await;
    let EventBody::SessionCreated {
        worktree, branch, ..
    } = &created[0].body
    else {
        panic!("expected session_created");
    };
    let worktree = PathBuf::from(worktree);
    std::fs::write(worktree.join("work.txt"), "kept").unwrap();
    git(&worktree, &["add", "work.txt"]);
    git(&worktree, &["commit", "--quiet", "-m", "work"]);

    let unarchive = CommandBody::UnarchiveSession {
        session_id: session.clone(),
    };
    let error = daemon
        .manager
        .handle(alice(), unarchive.clone())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict, "not archived: {error:?}");
    daemon
        .manager
        .archive(alice(), session.clone(), false)
        .await
        .unwrap();
    daemon.until_status(SessionStatus::Archived).await;
    assert!(!worktree.exists());

    let result = daemon.manager.handle(bob(), unarchive.clone()).await;
    assert_eq!(result, Ok(CommandResult::Applied));
    let events = daemon.until_status(SessionStatus::Idle).await;
    assert_eq!(describe(&events).last().unwrap(), "bob: status Idle");
    assert_eq!(git(&worktree, &["branch", "--show-current"]), *branch);
    assert_eq!(
        std::fs::read_to_string(worktree.join("work.txt")).unwrap(),
        "kept"
    );
    assert_eq!(
        daemon.manager.worktree(&session).await,
        Ok(worktree.clone())
    );

    daemon.prompt(alice(), &session, "First.").await;
    daemon.until_status(SessionStatus::Idle).await;
    assert_eq!(daemon.starts.lock().unwrap()[0].cwd, worktree);

    // Without its branch, there is nothing to bring back.
    daemon
        .manager
        .archive(alice(), session.clone(), false)
        .await
        .unwrap();
    daemon.until_status(SessionStatus::Archived).await;
    git(&daemon.repo, &["branch", "--quiet", "-D", branch]);
    let error = daemon.manager.handle(alice(), unarchive).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict, "{error:?}");
    daemon.stop().await;
}

struct ResetUsage(Timestamp);
impl Probe for ResetUsage {
    fn read(&self, _: StartRequest) -> ProbeFuture {
        let at = self.0;
        Box::pin(async move {
            Ok(vec![UsageWindow {
                window: "five_hour".into(),
                used_percent: 100.0,
                resets_at: Some(at),
            }])
        })
    }
}

async fn wait_for_limit(daemon: &mut Daemon, dir: &Path) -> (SessionId, Timestamp) {
    let at = Timestamp::now() + Duration::from_secs(3600);
    let probe: Arc<dyn Probe> = Arc::new(ResetUsage(at));
    daemon
        .manager
        .track_usage(usage::Config {
            probes: Probes::from([(fake(), probe)]),
            dir: dir.join("usage"),
            interval: Duration::from_secs(86400),
            fresh: Duration::from_secs(86400),
        })
        .unwrap();
    daemon.next_accounts().await;
    let session = daemon.create().await;
    daemon
        .prompt(alice(), &session, "Refactor the parser.")
        .await;
    let events = daemon
        .events_until(|body| {
            matches!(
                body,
                EventBody::SessionStatusChanged {
                    retry_at: Some(_),
                    ..
                }
            )
        })
        .await;
    assert!(events.iter().any(|event| event.body
        == EventBody::SessionStatusChanged {
            status: SessionStatus::WaitingForCapacity,
            retry_at: Some(at),
        }));
    assert_eq!(
        Store::open(dir.join("herder.db"))
            .unwrap()
            .queued_prompts(&session)
            .unwrap()[0]
            .retry_at,
        Some(at)
    );
    (session, at)
}

#[tokio::test]
async fn limit_reset_retries_at_the_deadline_and_releases_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open_scripts(
        dir.path(),
        &["failover_limit.jsonl", "failover_retry.jsonl"],
        Default::default(),
    )
    .await;
    let host = FakeHost::new(8 * GIB);
    let admission = admit(&daemon, dir.path(), 1, &host);
    let (session, _) = wait_for_limit(&mut daemon, dir.path()).await;
    assert_eq!(admission.resources().running_turns, 0);
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(3590)).await;
    assert_eq!(
        daemon.starts.lock().unwrap().len(),
        1,
        "must not retry early"
    );
    tokio::time::advance(Duration::from_secs(11)).await;
    tokio::time::resume();
    daemon
        .events_until(|body| matches!(body, EventBody::TurnCompleted { .. }))
        .await;
    daemon.until_status(SessionStatus::Idle).await;
    assert_eq!(daemon.starts.lock().unwrap().len(), 2);
    let journal = daemon.journal(&session).await;
    assert_eq!(
        journal
            .iter()
            .filter(|e| matches!(e.body, EventBody::TurnStarted { .. }))
            .count(),
        2
    );
    assert!(
        Store::open(dir.path().join("herder.db"))
            .unwrap()
            .queued_prompts(&session)
            .unwrap()
            .is_empty()
    );
    daemon.stop().await;
}

#[tokio::test]
async fn limit_reset_wait_survives_a_restart_without_usage_in_memory() {
    let dir = tempfile::tempdir().unwrap();
    let turns = Arc::new(AtomicU64::new(0));
    let mut daemon = Daemon::open(dir.path(), "failover_limit.jsonl", turns.clone()).await;
    let (session, at) = wait_for_limit(&mut daemon, dir.path()).await;
    daemon.stop().await;
    let mut daemon = Daemon::open(dir.path(), "failover_retry.jsonl", turns).await;
    daemon.manager.resume().await.unwrap();
    daemon
        .events_until(|body| {
            matches!(body,
        EventBody::SessionStatusChanged { retry_at: Some(reset), .. } if *reset == at)
        })
        .await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(3590)).await;
    assert!(daemon.starts.lock().unwrap().is_empty());
    tokio::time::advance(Duration::from_secs(11)).await;
    tokio::time::resume();
    daemon
        .events_until(|body| matches!(body, EventBody::TurnCompleted { .. }))
        .await;
    daemon.until_status(SessionStatus::Idle).await;
    assert_eq!(daemon.starts.lock().unwrap().len(), 1);
    daemon.stop().await;
    let daemon = Daemon::open(dir.path(), "failover_retry.jsonl", Default::default()).await;
    daemon.manager.resume().await.unwrap();
    assert!(
        Store::open(dir.path().join("herder.db"))
            .unwrap()
            .queued_prompts(&session)
            .unwrap()
            .is_empty()
    );
    assert!(daemon.starts.lock().unwrap().is_empty());
    daemon.stop().await;
}

#[tokio::test]
async fn user_prompt_replaces_the_scheduled_limit_retry() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open_scripts(
        dir.path(),
        &["failover_limit.jsonl", "second.jsonl"],
        Default::default(),
    )
    .await;
    let (session, _) = wait_for_limit(&mut daemon, dir.path()).await;
    daemon.prompt(bob(), &session, "Second.").await;
    daemon
        .events_until(|body| matches!(body, EventBody::TurnCompleted { .. }))
        .await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(3601)).await;
    tokio::time::resume();
    let prompts: Vec<_> = daemon
        .journal(&session)
        .await
        .into_iter()
        .filter_map(|event| match event.body {
            EventBody::ItemAdded {
                item:
                    Item {
                        body: ItemBody::UserMessage { text, .. },
                        ..
                    },
            } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(prompts, ["Refactor the parser.", "Second."]);
    assert!(
        Store::open(dir.path().join("herder.db"))
            .unwrap()
            .queued_prompts(&session)
            .unwrap()
            .is_empty()
    );
    daemon.stop().await;
}

#[tokio::test]
async fn successful_switches_cancel_limit_waits_and_invalid_switches_keep_them() {
    for kind in ["model", "account", "interrupt"] {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::open(dir.path(), "failover_limit.jsonl", Default::default()).await;
        let (session, at) = wait_for_limit(&mut daemon, dir.path()).await;
        assert!(
            daemon
                .manager
                .handle(alice(), switch_account(&session, "missing"))
                .await
                .is_err()
        );
        assert_eq!(
            Store::open(dir.path().join("herder.db"))
                .unwrap()
                .queued_prompts(&session)
                .unwrap()[0]
                .retry_at,
            Some(at)
        );
        let command = match kind {
            "model" => CommandBody::SetModel {
                session_id: session.clone(),
                model: "new".into(),
            },
            "account" => {
                assert!(daemon.manager.add_account(
                    AccountId::new("another"),
                    AccountConfig {
                        provider: fake(),
                        label: "Another".into(),
                        config_dir: None,
                    }
                ));
                switch_account(&session, "another")
            }
            _ => CommandBody::Interrupt {
                session_id: session.clone(),
            },
        };
        daemon.manager.handle(alice(), command).await.unwrap();
        assert!(
            Store::open(dir.path().join("herder.db"))
                .unwrap()
                .queued_prompts(&session)
                .unwrap()
                .is_empty()
        );
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(3601)).await;
        tokio::time::resume();
        assert_eq!(daemon.starts.lock().unwrap().len(), 1);
        daemon.stop().await;
    }
}

/// A stand-in title CLI writing into `dir`: run `n` saves its arguments, its stdin and its
/// config dir as `args-<n>`, `stdin-<n>` and `config-<n>`, then runs `answer`, which may use
/// `$n`.
fn title_cli(dir: &Path, answer: &str) -> TitleCli {
    let program = dir.join("title-cli");
    let script = format!(
        "#!/bin/sh\nd='{}'\necho run >> \"$d/runs\"\nn=$(wc -l < \"$d/runs\" | tr -d ' ')\n\
         printf '%s\\n' \"$@\" > \"$d/args-$n\"\n\
         cat > \"$d/stdin-$n\"\nprintf '%s' \"$FAKE_CONFIG_DIR\" > \"$d/config-$n\"\n{answer}\n",
        dir.display()
    );
    let staged = dir.join("title-cli.tmp");
    std::fs::write(&staged, script).unwrap();
    std::fs::set_permissions(&staged, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    // Renamed into place, so no test thread forks while it is still open for writing.
    std::fs::rename(&staged, &program).unwrap();
    TitleCli {
        program,
        args: vec!["--one-shot".into()],
        config_dir_env: "FAKE_CONFIG_DIR".into(),
        model: "fake-small".into(),
    }
}

/// How many times the stand-in title CLI in `dir` ran.
/// How many times the stand-in title CLI ran: one line each, appended, so runs that overlap
/// are all counted.
fn title_runs(dir: &Path) -> u32 {
    std::fs::read_to_string(dir.join("runs"))
        .map_or(0, |runs| runs.lines().count().try_into().unwrap())
}

/// Opens a daemon on `dir` playing the four turns of `titles.jsonl`, titling sessions with a
/// stand-in CLI in `dir/cli` that answers with `answer`, as `config` says.
async fn titling(dir: &Path, config: TitlesConfig, answer: &str) -> Daemon {
    let daemon = Daemon::open(dir, "titles.jsonl", Default::default()).await;
    let cli = dir.join("cli");
    std::fs::create_dir(&cli).unwrap();
    let clis = TitleClis::from([(fake(), title_cli(&cli, answer))]);
    daemon.manager.generate_titles(config, clis).unwrap();
    daemon
}

/// The session's `title_changed` events.
async fn titles(daemon: &Daemon, session: &SessionId) -> Vec<Event> {
    let events = daemon.journal(session).await;
    events
        .into_iter()
        .filter(|event| matches!(event.body, EventBody::TitleChanged { .. }))
        .collect()
}

/// Waits until the session's journal holds `count` `title_changed` events.
async fn until_titles(daemon: &Daemon, session: &SessionId, count: usize) -> Vec<Event> {
    for _ in 0..200 {
        let titles = titles(daemon, session).await;
        if titles.len() >= count {
            return titles;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "no {count} titles; got {:#?}",
        titles(daemon, session).await
    );
}

/// Prompts `text` and waits until its turn, `turn-<n>`, completed.
async fn prompt_turn(daemon: &mut Daemon, session: &SessionId, text: &str, n: u32) {
    daemon.prompt(alice(), session, text).await;
    let turn = TurnId::new(format!("turn-{n}"));
    daemon
        .events_until(
            |body| matches!(body, EventBody::TurnCompleted { turn_id } if *turn_id == turn),
        )
        .await;
}

/// Gives a title run that should not happen time to show up.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(300)).await;
}

fn titled(title: &str, source: TitleSource) -> EventBody {
    EventBody::TitleChanged {
        title: title.into(),
        source,
    }
}

#[tokio::test]
async fn a_title_follows_the_first_prompt_and_is_refreshed_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = titling(
        dir.path(),
        TitlesConfig::default(),
        "echo \"  \\\"Title $n.\\\"\"; echo 'Because it says so.'",
    )
    .await;
    let session = daemon.create().await;

    daemon.prompt(alice(), &session, "First.").await;
    let titles = until_titles(&daemon, &session, 1).await;
    assert_eq!(titles[0].by, None);
    assert_eq!(titles[0].body, titled("Title 1", TitleSource::Auto));
    let cli = dir.path().join("cli");
    let read = |name: &str| std::fs::read_to_string(cli.join(name)).unwrap();
    assert_eq!(read("args-1"), "--one-shot\n--model\nfake-small\n");
    assert_eq!(
        read("config-1"),
        dir.path().join("account").to_str().unwrap()
    );
    // The run reads the conversation so far, which may hold the first reply already.
    let stdin = read("stdin-1");
    assert!(
        stdin.starts_with(&format!("{INSTRUCTION}\n\n<conversation>\nUser: First.\n"))
            && stdin.ends_with("</conversation>\n"),
        "{stdin}"
    );
    let Some(head) = daemon.manager.sessions().await.unwrap().pop() else {
        panic!("no session");
    };
    assert_eq!(head.title.as_deref(), Some("Title 1"));

    // The third turn's end titles it again, from the conversation so far.
    daemon
        .events_until(|body| matches!(body, EventBody::TurnCompleted { .. }))
        .await;
    prompt_turn(&mut daemon, &session, "Second.", 2).await;
    prompt_turn(&mut daemon, &session, "Third.", 3).await;
    let titles = until_titles(&daemon, &session, 2).await;
    assert_eq!(titles[1].body, titled("Title 2", TitleSource::Auto));
    assert!(read("stdin-2").contains(
        "User: First.\n\nAgent: One.\n\nUser: Second.\n\nAgent: Two.\n\nUser: Third.\n\n\
         Agent: Three.\n"
    ));

    // And never again.
    prompt_turn(&mut daemon, &session, "Fourth.", 4).await;
    settle().await;
    assert_eq!(title_runs(&cli), 2);
    assert_eq!(self::titles(&daemon, &session).await.len(), 2);
    daemon.stop().await;
}

#[tokio::test]
async fn automatic_titles_never_replace_one_a_user_chose() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = titling(dir.path(), TitlesConfig::default(), "echo \"Title $n\"").await;
    let session = daemon.create().await;
    let cli = dir.path().join("cli");
    let rename = CommandBody::RenameSession {
        session_id: session.clone(),
        title: "Mine".into(),
    };
    daemon.manager.handle(alice(), rename).await.unwrap();

    prompt_turn(&mut daemon, &session, "First.", 1).await;
    settle().await;
    assert_eq!(title_runs(&cli), 0);

    // A requested title replaces the user's, from the conversation so far, `by` who asked.
    let retitle = CommandBody::RetitleSession {
        session_id: session.clone(),
    };
    let result = daemon.manager.handle(bob(), retitle).await;
    assert_eq!(result, Ok(CommandResult::Applied));
    let titles = until_titles(&daemon, &session, 2).await;
    assert_eq!(titles[1].by, Some(bob()));
    assert_eq!(titles[1].body, titled("Title 1", TitleSource::AiRequested));
    let stdin = std::fs::read_to_string(cli.join("stdin-1")).unwrap();
    assert!(stdin.contains("User: First.\n\nAgent: One.\n"), "{stdin}");

    // The refresh leaves the requested title alone too.
    prompt_turn(&mut daemon, &session, "Second.", 2).await;
    prompt_turn(&mut daemon, &session, "Third.", 3).await;
    settle().await;
    assert_eq!(title_runs(&cli), 1);
    assert_eq!(self::titles(&daemon, &session).await.len(), 2);
    daemon.stop().await;
}

#[tokio::test]
async fn disabled_titles_run_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let config = TitlesConfig {
        enabled: false,
        ..TitlesConfig::default()
    };
    let mut daemon = titling(dir.path(), config, "echo Title").await;
    let session = daemon.create().await;
    prompt_turn(&mut daemon, &session, "First.", 1).await;
    let retitle = CommandBody::RetitleSession {
        session_id: session.clone(),
    };
    let error = daemon.manager.handle(bob(), retitle).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Unsupported);
    settle().await;
    assert_eq!(title_runs(&dir.path().join("cli")), 0);
    assert!(titles(&daemon, &session).await.is_empty());
    daemon.stop().await;
}

#[tokio::test]
async fn a_failed_title_run_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = titling(dir.path(), TitlesConfig::default(), "echo Title; exit 3").await;
    let session = daemon.create().await;
    prompt_turn(&mut daemon, &session, "First.", 1).await;
    let retitle = CommandBody::RetitleSession {
        session_id: session.clone(),
    };
    let result = daemon.manager.handle(bob(), retitle).await;
    assert_eq!(result, Ok(CommandResult::Applied));
    for _ in 0..200 {
        if title_runs(&dir.path().join("cli")) == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    settle().await;
    assert_eq!(title_runs(&dir.path().join("cli")), 2);
    assert!(titles(&daemon, &session).await.is_empty());
    let sessions = daemon.manager.sessions().await.unwrap();
    assert_eq!(sessions[0].status, SessionStatus::Idle);
    assert_eq!(sessions[0].title, None);
    daemon.stop().await;
}

#[tokio::test]
async fn a_turn_the_cli_starts_is_journaled_without_a_prompt_and_queues_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "background_turn.jsonl", Default::default()).await;
    let session = daemon.create().await;
    daemon
        .prompt(alice(), &session, "Review it in the background.")
        .await;
    daemon
        .events_until(|body| {
            matches!(body, EventBody::ItemAdded { item }
                if item.turn_id == TurnId::new("cli-1")
                    && matches!(item.body, ItemBody::AssistantMessage { .. }))
        })
        .await;
    // The CLI's own turn runs: the prompt waits behind it, and an interrupt reaches it.
    daemon.prompt(alice(), &session, "Fix them.").await;
    let interrupt = CommandBody::Interrupt {
        session_id: session.clone(),
    };
    let result = daemon.manager.handle(bob(), interrupt).await.unwrap();
    assert_eq!(result, CommandResult::Applied);
    daemon.until_status(SessionStatus::Idle).await;

    let journal = daemon.journal(&session).await;
    assert_eq!(
        describe(&journal),
        [
            "alice: session_created",
            "-: status Running",
            "alice: user turn-1 Review it in the background.",
            "-: turn_started turn-1",
            "-: tool_call Agent",
            "-: tool_result Async agent launched successfully.",
            "-: turn_completed turn-1",
            "-: status Idle",
            "-: status Running",
            "-: turn_started cli-1",
            "-: tool_result Found 2 issues.",
            "-: assistant cli-1 The review found 2 issues.",
            "-: turn_interrupted cli-1",
            "alice: user turn-2 Fix them.",
            "-: turn_started turn-2",
            "-: assistant turn-2 Fixed.",
            "-: turn_completed turn-2",
            "-: status Idle",
        ]
    );
    // The agent's result answers the call that started it.
    let result = journal.iter().find_map(|event| match &event.body {
        EventBody::ItemAdded { item } if item.turn_id == TurnId::new("cli-1") => Some(&item.body),
        _ => None,
    });
    assert!(matches!(
        result,
        Some(ItemBody::ToolResult { call_id, .. }) if *call_id == ItemId::new("item-1")
    ));
    daemon.stop().await;
}

#[tokio::test]
async fn a_turn_the_cli_starts_ahead_of_a_sent_prompt_runs_first() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "cli_turn_ahead.jsonl", Default::default()).await;
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "First.").await;
    daemon.until_status(SessionStatus::Idle).await;
    assert_eq!(
        describe(&daemon.journal(&session).await),
        [
            "alice: session_created",
            "-: status Running",
            "alice: user turn-1 First.",
            "-: turn_started cli-1",
            "-: assistant cli-1 The review finished.",
            "-: turn_completed cli-1",
            "-: turn_started turn-1",
            "-: assistant turn-1 One.",
            "-: turn_completed turn-1",
            "-: status Idle",
        ]
    );
    daemon.stop().await;
}

#[tokio::test]
async fn a_turn_the_cli_starts_fails_on_a_spent_limit_without_a_retry() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::open(dir.path(), "cli_turn_limit.jsonl", Default::default()).await;
    let session = daemon.create().await;
    daemon.prompt(alice(), &session, "First.").await;
    daemon.until_status(SessionStatus::NeedsYou).await;
    let journal = daemon.journal(&session).await;
    assert_eq!(
        describe(&journal[5..]),
        [
            "-: status Idle",
            "-: status Running",
            "-: turn_started cli-1",
            "-: turn_failed cli-1 LimitReached",
            "-: status NeedsYou",
        ]
    );
    daemon.stop().await;
}
