//! Stalled agents ([`herder_daemon::stalls`]): the session manager with the fake adapter, on
//! tokio's paused clock.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use herder_adapters::fake::FakeAdapter;
use herder_daemon::session::{AccountConfig, Accounts, Adapters, EventSink, SessionManager, Setup};
use herder_daemon::stalls::{self, PROMPT, Stalls};
use herder_daemon::worktree::Worktrees;
use herder_protocol::{
    Account, AccountId, CommandBody, CommandResult, Event, EventBody, FollowUp, FollowUpReason,
    Item, ItemBody, ItemId, PermissionMode, Provider, SessionHead, SessionId, SessionStatus,
    TurnId, UserId,
};
use herder_store::Store;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// `[follow_ups] stall_after_secs` of these tests.
const AFTER: Duration = Duration::from_secs(600);

struct Silent;

impl EventSink for Silent {
    fn event(&self, _: &Event) {}
    fn snapshot(&self, _: &SessionId, _: &Item) {}
    fn delta(&self, _: &SessionId, _: &ItemId, _: &str) {}
    fn sessions_changed(&self, _: &[SessionHead]) {}
    fn accounts_changed(&self, _: &[Account]) {}
}

/// A session manager on a temporary directory watching for stalls; its sessions run the fake
/// CLI's script `script.jsonl`.
struct Daemon {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    repo: PathBuf,
    after: Duration,
    max_nudges: u32,
    /// Turns started, across restarts: turn `n` is `turn-n`.
    turns: Arc<AtomicU64>,
    manager: SessionManager,
    stalls: Arc<Stalls>,
    shutdown: CancellationToken,
}

impl Daemon {
    async fn open(after: Duration, max_nudges: u32) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_owned();
        let repo = dir.join("app");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "--quiet", "--initial-branch=main"]);
        git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "init"]);
        let turns = Arc::new(AtomicU64::new(0));
        let (manager, stalls, shutdown) = start(&dir, after, max_nudges, &turns).await;
        Self {
            _tmp: tmp,
            dir,
            repo,
            after,
            max_nudges,
            turns,
            manager,
            stalls,
            shutdown,
        }
    }

    /// Stops the manager and starts another on the same data.
    async fn restart(&mut self) {
        self.shutdown.cancel();
        tokio::time::sleep(Duration::from_millis(50)).await;
        (self.manager, self.stalls, self.shutdown) =
            start(&self.dir, self.after, self.max_nudges, &self.turns).await;
    }

    /// Scripts the fake CLI, from its next start, with `lines`.
    fn script(&self, lines: &[Value]) {
        let script: String = lines.iter().map(|line| format!("{line}\n")).collect();
        std::fs::write(self.dir.join("script.jsonl"), script).unwrap();
    }

    async fn session(&self) -> SessionId {
        let command = CommandBody::CreateSession {
            repo: Some(self.repo.to_str().unwrap().to_owned()),
            project_id: None,
            branch: None,
            account_id: Some(AccountId::new("account-1")),
            provider: None,
            model: None,
            permission_mode: Some(PermissionMode::Ask),
            max_children: None,
            failover_pin: None,
        };
        match self.manager.handle(alice(), command).await.unwrap() {
            CommandResult::SessionCreated { session_id } => session_id,
            other => panic!("expected a session, got {other:?}"),
        }
    }

    async fn prompt(&self, session_id: &SessionId, text: &str) {
        let command = CommandBody::SendPrompt {
            session_id: session_id.clone(),
            text: text.into(),
            images: Vec::new(),
        };
        let result = self.manager.handle(alice(), command).await.unwrap();
        assert_eq!(result, CommandResult::Applied);
    }

    async fn events(&self, session_id: &SessionId) -> Vec<Event> {
        self.manager
            .read_since(session_id, 0, 10_000)
            .await
            .unwrap()
    }

    /// The session's status as its journal last said.
    async fn status(&self, session_id: &SessionId) -> SessionStatus {
        self.events(session_id)
            .await
            .into_iter()
            .filter_map(|event| match event.body {
                EventBody::SessionStatusChanged { status, .. } => Some(status),
                _ => None,
            })
            .next_back()
            .unwrap_or(SessionStatus::Idle)
    }

    /// Waits until turn `turn` started and the session is `status`, then lets the watcher see
    /// it, as its next check would.
    async fn settled(&self, session_id: &SessionId, turn: u64, status: SessionStatus) {
        for _ in 0..500 {
            if self.turns.load(Ordering::SeqCst) >= turn && self.status(session_id).await == status
            {
                self.stalls.check().await.unwrap();
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "the session is {:?}, not {status:?} after turn {turn}",
            self.status(session_id).await
        );
    }

    /// Moves the clock `by` on, then checks for stalls.
    async fn later(&self, by: Duration) {
        tokio::time::pause();
        tokio::time::advance(by).await;
        tokio::time::resume();
        self.stalls.check().await.unwrap();
    }

    /// The stall prompts the session got, with who sent each.
    async fn nudges(&self, session_id: &SessionId) -> Vec<(Option<UserId>, String)> {
        self.events(session_id)
            .await
            .into_iter()
            .filter_map(|event| match event.body {
                EventBody::ItemAdded { item } => match (item.follow_up, item.body) {
                    (Some(follow_up), ItemBody::UserMessage { text, .. }) => {
                        assert_eq!(follow_up, stalled());
                        Some((event.by, text))
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    /// Asserts the session got `count` stall prompts, after giving one more time to arrive.
    async fn nudged(&self, session_id: &SessionId, count: usize) {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let expected = vec![(None, PROMPT.to_owned()); count];
        assert_eq!(self.nudges(session_id).await, expected);
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

async fn start(
    dir: &Path,
    after: Duration,
    max_nudges: u32,
    turns: &Arc<AtomicU64>,
) -> (SessionManager, Arc<Stalls>, CancellationToken) {
    let fake = Provider::Other("fake".into());
    let mut adapters = Adapters::new();
    adapters.register(
        fake.clone(),
        Arc::new(FakeAdapter::new(dir.join("script.jsonl"))),
    );
    let mut accounts = Accounts::new();
    accounts.insert(
        AccountId::new("account-1"),
        AccountConfig {
            provider: fake,
            label: "Account 1".into(),
            config_dir: Some(dir.join("account")),
        },
    );
    let turns = Arc::clone(turns);
    let setup = Setup {
        store: Store::open(dir.join("herder.db")).unwrap(),
        adapters,
        accounts,
        sink: Arc::new(Silent),
        turn_ids: Box::new(move || {
            TurnId::new(format!("turn-{}", turns.fetch_add(1, Ordering::SeqCst) + 1))
        }),
        worktrees: Worktrees::new(dir.join("worktrees")),
        attachments: dir.join("attachments"),
    };
    let shutdown = CancellationToken::new();
    let manager = SessionManager::open(setup, shutdown.clone()).await.unwrap();
    // A long interval: tests check by hand.
    let stalls = manager
        .watch_stalls(stalls::Config {
            after,
            max_nudges,
            pr_events: true,
            interval: Duration::from_secs(24 * 3600),
        })
        .unwrap();
    (manager, stalls, shutdown)
}

fn git(dir: &Path, args: &[&str]) {
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
    assert!(output.status.success(), "git {args:?} failed: {output:?}");
}

fn alice() -> UserId {
    UserId::new("alice")
}

fn stalled() -> FollowUp {
    FollowUp {
        reason: FollowUpReason::Stalled,
        pr: None,
        head_sha: None,
    }
}

/// Turn `n`: it expects `prompt` and answers `reply`.
fn turn(n: u64, prompt: &str, reply: &str) -> [Value; 4] {
    let turn = format!("turn-{n}");
    [
        json!({"expect": {"type": "send_prompt", "turn_id": turn, "text": prompt}}),
        json!({"emit": {"type": "turn_started", "turn_id": turn}}),
        json!({"emit": {"type": "item_completed", "item": {"id": format!("item-{n}"), "turn_id": turn, "body": {"type": "assistant_message", "text": reply}}}}),
        json!({"emit": {"type": "turn_completed", "turn_id": turn}}),
    ]
}

fn script(turns: &[(u64, &str, &str)]) -> Vec<Value> {
    turns
        .iter()
        .flat_map(|(n, prompt, reply)| turn(*n, prompt, reply))
        .collect()
}

#[tokio::test]
async fn an_idle_session_is_prompted_up_to_the_cap_until_a_user_prompts_it() {
    let daemon = Daemon::open(AFTER, 2).await;
    daemon.script(&script(&[
        (1, "Fix the bug.", "Looking into it."),
        (2, PROMPT, "Still on it: the fix needs a test."),
        (3, PROMPT, "The test is written."),
        (4, "Push it.", "Pushed."),
        (5, PROMPT, "Waiting for review."),
    ]));
    let session_id = daemon.session().await;
    daemon.prompt(&session_id, "Fix the bug.").await;
    daemon.settled(&session_id, 1, SessionStatus::Idle).await;

    // Not before it has been idle for stall_after_secs.
    daemon.later(AFTER - Duration::from_secs(1)).await;
    daemon.nudged(&session_id, 0).await;
    daemon.later(Duration::from_secs(1)).await;
    daemon.settled(&session_id, 2, SessionStatus::Idle).await;
    daemon.nudged(&session_id, 1).await;

    // Each idle stretch after a stall prompt waits again.
    daemon.later(AFTER / 2).await;
    daemon.nudged(&session_id, 1).await;
    daemon.later(AFTER / 2).await;
    daemon.settled(&session_id, 3, SessionStatus::Idle).await;
    daemon.nudged(&session_id, 2).await;

    // The cap is reached.
    daemon.later(AFTER * 10).await;
    daemon.nudged(&session_id, 2).await;

    // A user's prompt starts the count over.
    daemon.prompt(&session_id, "Push it.").await;
    daemon.settled(&session_id, 4, SessionStatus::Idle).await;
    daemon.later(AFTER).await;
    daemon.settled(&session_id, 5, SessionStatus::Idle).await;
    daemon.nudged(&session_id, 3).await;
}

#[tokio::test]
async fn an_agent_that_answers_done_is_left_alone() {
    let daemon = Daemon::open(AFTER, 2).await;
    daemon.script(&script(&[
        (1, "Fix the bug.", "Fixed it."),
        (2, PROMPT, "**Done.** The fix is on main."),
    ]));
    let session_id = daemon.session().await;
    daemon.prompt(&session_id, "Fix the bug.").await;
    daemon.settled(&session_id, 1, SessionStatus::Idle).await;
    daemon.later(AFTER).await;
    daemon.settled(&session_id, 2, SessionStatus::Idle).await;

    daemon.later(AFTER * 10).await;
    daemon.nudged(&session_id, 1).await;
}

#[tokio::test]
async fn no_prompt_when_stall_after_secs_is_zero() {
    let daemon = Daemon::open(Duration::ZERO, 2).await;
    daemon.script(&script(&[(1, "Fix the bug.", "Looking into it.")]));
    let session_id = daemon.session().await;
    daemon.prompt(&session_id, "Fix the bug.").await;
    daemon.settled(&session_id, 1, SessionStatus::Idle).await;

    daemon.later(Duration::from_secs(30 * 24 * 3600)).await;
    daemon.nudged(&session_id, 0).await;
}

#[tokio::test]
async fn no_prompt_for_a_session_nobody_prompted() {
    let daemon = Daemon::open(AFTER, 2).await;
    let session_id = daemon.session().await;
    daemon.stalls.check().await.unwrap();

    daemon.later(AFTER * 10).await;
    daemon.nudged(&session_id, 0).await;
}

#[tokio::test]
async fn no_prompt_while_the_session_needs_the_user() {
    let daemon = Daemon::open(AFTER, 2).await;
    daemon.script(&[
        json!({"expect": {"type": "send_prompt", "turn_id": "turn-1", "text": "Clean up."}}),
        json!({"emit": {"type": "turn_started", "turn_id": "turn-1"}}),
        json!({"emit": {"type": "item_completed", "item": {"id": "item-1", "turn_id": "turn-1", "body": {"type": "tool_call", "name": "Bash", "input": {"command": "rm -rf target"}}}}}),
        json!({"emit": {"type": "approval_requested", "approval_id": "approval-1", "turn_id": "turn-1", "tool_call_id": "item-1", "summary": "Run rm -rf target"}}),
        json!({"expect": {"type": "answer_approval", "approval_id": "approval-1", "decision": "allow"}}),
    ]);
    let session_id = daemon.session().await;
    daemon.prompt(&session_id, "Clean up.").await;
    daemon
        .settled(&session_id, 1, SessionStatus::NeedsYou)
        .await;

    daemon.later(AFTER * 10).await;
    daemon.nudged(&session_id, 0).await;
    assert_eq!(daemon.stalls.stuck(), []);
}

#[tokio::test]
async fn a_restart_sends_no_prompt_beyond_the_cap() {
    let mut daemon = Daemon::open(AFTER, 2).await;
    daemon.script(&script(&[
        (1, "Fix the bug.", "Looking into it."),
        (2, PROMPT, "Still on it."),
    ]));
    let session_id = daemon.session().await;
    daemon.prompt(&session_id, "Fix the bug.").await;
    daemon.settled(&session_id, 1, SessionStatus::Idle).await;
    daemon.later(AFTER).await;
    daemon.settled(&session_id, 2, SessionStatus::Idle).await;

    // The count survives a restart: one prompt is left, then none.
    daemon.script(&script(&[(3, PROMPT, "Almost there.")]));
    daemon.restart().await;
    daemon.stalls.check().await.unwrap();
    daemon.later(AFTER).await;
    daemon.settled(&session_id, 3, SessionStatus::Idle).await;
    daemon.nudged(&session_id, 2).await;

    daemon.restart().await;
    daemon.stalls.check().await.unwrap();
    daemon.later(AFTER * 10).await;
    daemon.nudged(&session_id, 2).await;
}

#[tokio::test]
async fn a_session_running_with_no_event_is_flagged_not_prompted() {
    let daemon = Daemon::open(AFTER, 2).await;
    // The turn starts, then the CLI goes quiet.
    daemon.script(&[
        json!({"expect": {"type": "send_prompt", "turn_id": "turn-1", "text": "Fix the bug."}}),
        json!({"emit": {"type": "turn_started", "turn_id": "turn-1"}}),
        json!({"expect": {"type": "shutdown"}}),
    ]);
    let session_id = daemon.session().await;
    daemon.prompt(&session_id, "Fix the bug.").await;
    daemon.settled(&session_id, 1, SessionStatus::Running).await;

    daemon.later(AFTER - Duration::from_secs(1)).await;
    assert_eq!(daemon.stalls.stuck(), []);
    daemon.later(Duration::from_secs(1)).await;
    assert_eq!(daemon.stalls.stuck(), std::slice::from_ref(&session_id));
    daemon.nudged(&session_id, 0).await;
    assert_eq!(daemon.status(&session_id).await, SessionStatus::Running);
}
