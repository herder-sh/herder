//! Pull request tracking end to end: real git hooks in real session worktrees, calling the
//! built `herder` binary, pushing to a bare "GitHub" remote, with GitHub's REST API faked from
//! that remote's contents.

use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use herder_adapters::fake::FakeAdapter;
use herder_adapters::{Adapter, StartFuture, StartRequest};
use herder_daemon::prs::{self, Fetched, GetFuture, GitHub, GraphqlFuture, PrTracker};
use herder_daemon::session::{AccountConfig, Accounts, Adapters, EventSink, SessionManager, Setup};
use herder_daemon::worktree::Worktrees;
use herder_protocol::{
    Account, AccountId, ApprovalDecision, CiStatus, CommandBody, CommandResult, ErrorCode, Event,
    EventBody, FollowUp, FollowUpReason, Item, ItemBody, ItemId, Mergeable, PermissionMode,
    PrState, Provider, PullRequest, ReviewStatus, SessionHead, SessionId, SessionStatus, Timestamp,
    TurnId, UserId,
};
use herder_store::Store;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn alice() -> UserId {
    UserId::new("alice")
}

/// Runs git in `dir` off the async workers (its hooks call back into this process's daemon),
/// panicking on failure; returns trimmed stdout.
async fn git(dir: &Path, args: &[&str]) -> String {
    let dir = dir.to_owned();
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    tokio::task::spawn_blocking(move || git_sync(&dir, &args))
        .await
        .unwrap()
}

/// [`git`] with `env` added to its environment, as the session's agent runs it.
async fn agent_git(dir: &Path, env: &BTreeMap<String, String>, args: &[&str]) -> String {
    let (dir, env) = (dir.to_owned(), env.clone());
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    tokio::task::spawn_blocking(move || git_env(&dir, &env, &args))
        .await
        .unwrap()
}

fn git_sync(dir: &Path, args: &[String]) -> String {
    git_env(dir, &BTreeMap::new(), args)
}

fn git_env(dir: &Path, env: &BTreeMap<String, String>, args: &[String]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
        .args(args)
        .envs(env)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// Whether `git config --get key` finds a value in `dir`.
async fn config_set(dir: &Path, key: &str) -> bool {
    let dir = dir.to_owned();
    let key = key.to_owned();
    tokio::task::spawn_blocking(move || {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["config", "--get", &key])
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    })
    .await
    .unwrap()
}

struct Silent;

impl EventSink for Silent {
    fn event(&self, _: &Event) {}
    fn snapshot(&self, _: &SessionId, _: &Item) {}
    fn delta(&self, _: &SessionId, _: &ItemId, _: &str) {}
    fn sessions_changed(&self, _: &[SessionHead]) {}
    fn accounts_changed(&self, _: &[Account]) {}
}

/// The fake adapter, keeping every start request so tests can see the CLI's environment.
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

/// A pull request on the fake GitHub.
#[derive(Clone)]
struct FakePr {
    number: u64,
    /// Branch of the bare remote that heads it.
    head: String,
    title: String,
    state: &'static str,
    draft: bool,
    merged: bool,
    mergeable: Option<bool>,
    created_at: Timestamp,
    /// Check runs on its head commit: name, status and conclusion.
    checks: Vec<(&'static str, &'static str, Option<&'static str>)>,
    /// Reviews: reviewer and verdict.
    reviews: Vec<(&'static str, &'static str)>,
    /// Review threads: whether each is resolved.
    threads: Vec<bool>,
}

/// One request the fake answered.
#[derive(Clone, Debug, PartialEq)]
struct Call {
    path: String,
    conditional: bool,
    answer: &'static str,
}

/// GitHub's REST API for `acme/app`, served from the bare repository the tests push to: pull
/// request heads and commits come from its branches. ETags are hashes of the body.
struct FakeGitHub {
    bare: PathBuf,
    prs: Mutex<Vec<FakePr>>,
    calls: Mutex<Vec<Call>>,
}

impl FakeGitHub {
    /// Opens a pull request on `head`, as `gh pr create` or the web UI would.
    fn open(&self, head: &str, title: &str) -> u64 {
        let mut prs = self.prs.lock().unwrap();
        let number = prs.len() as u64 + 1;
        prs.push(FakePr {
            number,
            head: head.to_owned(),
            title: title.to_owned(),
            state: "open",
            draft: false,
            merged: false,
            mergeable: Some(true),
            created_at: Timestamp::now(),
            checks: Vec::new(),
            reviews: Vec::new(),
            threads: Vec::new(),
        });
        number
    }

    fn change(&self, number: u64, change: impl FnOnce(&mut FakePr)) {
        let mut prs = self.prs.lock().unwrap();
        change(prs.iter_mut().find(|pr| pr.number == number).unwrap());
    }

    fn take_calls(&self) -> Vec<Call> {
        std::mem::take(&mut self.calls.lock().unwrap())
    }

    fn head_sha(&self, branch: &str) -> String {
        git_sync(
            &self.bare,
            &["rev-parse".into(), format!("refs/heads/{branch}")],
        )
    }

    fn pull(&self, pr: &FakePr) -> Value {
        json!({
            "number": pr.number,
            "html_url": format!("https://github.com/acme/app/pull/{}", pr.number),
            "title": pr.title,
            "state": pr.state,
            "draft": pr.draft,
            "merged_at": pr.merged.then_some("2026-10-02T00:00:00Z"),
            "mergeable": pr.mergeable,
            "created_at": pr.created_at.to_string(),
            "head": {"ref": pr.head, "sha": self.head_sha(&pr.head)},
            "requested_reviewers": [],
            "requested_teams": [],
        })
    }

    fn body(&self, path: &str) -> Option<Value> {
        let (route, query) = path.split_once('?').unwrap_or((path, ""));
        let query: Vec<(&str, String)> = query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .map(|(key, value)| (key, decode(value)))
            .collect();
        let param = |name: &str| {
            query
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.clone())
        };
        let parts: Vec<&str> = route.strip_prefix("repos/acme/app/")?.split('/').collect();
        let prs = self.prs.lock().unwrap().clone();
        let by_number = |number: &str| {
            let number: u64 = number.parse().ok()?;
            prs.iter().find(|pr| pr.number == number).cloned()
        };
        match parts[..] {
            ["pulls"] => {
                let head = param("head");
                let state = param("state").unwrap_or_else(|| "open".into());
                let listed = prs
                    .iter()
                    .filter(|pr| state == "all" || pr.state == state)
                    .filter(|pr| {
                        head.as_ref()
                            .is_none_or(|head| *head == format!("acme:{}", pr.head))
                    })
                    .map(|pr| self.pull(pr))
                    .collect();
                Some(Value::Array(listed))
            }
            ["pulls", number] => Some(self.pull(&by_number(number)?)),
            ["pulls", number, "commits"] => {
                let pr = by_number(number)?;
                let log = git_sync(
                    &self.bare,
                    &[
                        "log".into(),
                        "--format=%B%x00".into(),
                        format!("main..refs/heads/{}", pr.head),
                    ],
                );
                let commits = log
                    .split('\0')
                    .map(str::trim)
                    .filter(|message| !message.is_empty())
                    .map(|message| json!({"commit": {"message": message}}))
                    .collect();
                Some(Value::Array(commits))
            }
            ["pulls", number, "reviews"] => {
                let reviews = by_number(number)?
                    .reviews
                    .iter()
                    .map(|(user, state)| json!({"user": {"login": user}, "state": state}))
                    .collect();
                Some(Value::Array(reviews))
            }
            ["commits", sha, "check-runs"] => {
                let runs: Vec<Value> = prs
                    .iter()
                    .find(|pr| self.head_sha(&pr.head) == sha)
                    .map(|pr| {
                        pr.checks
                            .iter()
                            .map(|(name, status, conclusion)| {
                                json!({"name": name, "status": status, "conclusion": conclusion})
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Some(json!({"total_count": runs.len(), "check_runs": runs}))
            }
            ["commits", _, "status"] => Some(json!({"state": "pending", "total_count": 0})),
            _ => None,
        }
    }
}

impl GitHub for FakeGitHub {
    fn get<'a>(&'a self, host: &'a str, path: &'a str, etag: Option<&'a str>) -> GetFuture<'a> {
        assert_eq!(host, "github.com");
        let (fetched, answer) = match self.body(path) {
            None => (Fetched::NotFound, "404"),
            Some(body) => {
                let mut hasher = DefaultHasher::new();
                body.to_string().hash(&mut hasher);
                let current = format!("W/\"{:x}\"", hasher.finish());
                if etag == Some(current.as_str()) {
                    (Fetched::NotModified, "304")
                } else {
                    let etag = Some(current);
                    (Fetched::Modified { etag, body }, "200")
                }
            }
        };
        self.calls.lock().unwrap().push(Call {
            path: path.to_owned(),
            conditional: etag.is_some(),
            answer,
        });
        Box::pin(async move { Ok(fetched) })
    }

    /// Answers [`prs::REVIEW_THREADS`] as GitHub does: the shape of
    /// `herder-daemon/tests/fixtures/github/review_threads.json`.
    fn graphql<'a>(
        &'a self,
        host: &'a str,
        query: &'a str,
        variables: &'a Value,
    ) -> GraphqlFuture<'a> {
        assert_eq!(host, "github.com");
        assert_eq!(query, prs::REVIEW_THREADS);
        assert_eq!(
            (&variables["owner"], &variables["name"]),
            (&json!("acme"), &json!("app"))
        );
        let number = variables["number"].as_u64().unwrap();
        let prs = self.prs.lock().unwrap();
        let pr = prs.iter().find(|pr| pr.number == number).unwrap();
        let nodes: Vec<Value> = pr
            .threads
            .iter()
            .map(|resolved| json!({"isResolved": resolved}))
            .collect();
        let data = json!({"repository": {"pullRequest": {"reviewThreads": {"nodes": nodes}}}});
        self.calls.lock().unwrap().push(Call {
            path: format!("graphql review threads #{number}"),
            conditional: false,
            answer: "200",
        });
        Box::pin(async move { Ok(data) })
    }
}

fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            out.push(u8::from_str_radix(&value[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

/// A daemon's session manager tracking pull requests of `acme/app`, whose `origin` is a bare
/// repository standing in for GitHub.
struct World {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    bare: PathBuf,
    data: PathBuf,
    manager: SessionManager,
    tracker: Arc<PrTracker>,
    github: Arc<FakeGitHub>,
    starts: Arc<Mutex<Vec<StartRequest>>>,
    shutdown: CancellationToken,
    /// Turns started, across restarts: turn `n` is `turn-n`.
    turns: Arc<AtomicU64>,
    follow_ups: bool,
}

impl World {
    /// A world whose daemon prompts no session on its own.
    async fn new() -> Self {
        Self::open(false).await
    }

    /// A world whose daemon prompts idle sessions on pull request events with `follow_ups`.
    async fn open(follow_ups: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_owned();
        let bare = root.join("github/acme/app.git");
        std::fs::create_dir_all(&bare).unwrap();
        git(
            &bare,
            &["init", "--quiet", "--bare", "--initial-branch=main"],
        )
        .await;
        let repo = root.join("app");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "--quiet", "--initial-branch=main"]).await;
        git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "init"]).await;
        // The remote is GitHub as far as anyone reading the config can tell; git itself
        // pushes to the bare repository.
        git(
            &repo,
            &["remote", "add", "origin", "https://github.com/acme/app.git"],
        )
        .await;
        let local = format!("file://{}/", root.join("github").display());
        git(
            &repo,
            &[
                "config",
                &format!("url.{local}.insteadOf"),
                "https://github.com/",
            ],
        )
        .await;
        git(&repo, &["push", "--quiet", "origin", "main"]).await;
        // The user's own hooks, which session worktrees must keep running.
        let hooks = repo.join(".git/hooks");
        for (name, marker) in [
            ("pre-push", "user-pre-push"),
            ("prepare-commit-msg", "user-prepare"),
        ] {
            let script = format!(
                "#!/bin/sh\ncat > /dev/null\necho \"$@\" >> '{}'\n",
                root.join(marker).display()
            );
            std::fs::write(hooks.join(name), script).unwrap();
            std::fs::set_permissions(
                hooks.join(name),
                std::os::unix::fs::PermissionsExt::from_mode(0o755),
            )
            .unwrap();
        }

        let data = root.join("data");
        std::fs::create_dir(&data).unwrap();
        let github = Arc::new(FakeGitHub {
            bare: bare.clone(),
            prs: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
        });
        let turns = Arc::new(AtomicU64::new(0));
        let (manager, tracker, starts, shutdown) =
            daemon(&root, &data, &github, &turns, follow_ups).await;
        Self {
            _tmp: tmp,
            root,
            repo,
            bare,
            data,
            manager,
            tracker,
            github,
            starts,
            shutdown,
            turns,
            follow_ups,
        }
    }

    /// Stops the daemon and starts another on the same data.
    async fn restart(&mut self) {
        self.shutdown.cancel();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (manager, tracker, starts, shutdown) = daemon(
            &self.root,
            &self.data,
            &self.github,
            &self.turns,
            self.follow_ups,
        )
        .await;
        (self.manager, self.tracker, self.starts, self.shutdown) =
            (manager, tracker, starts, shutdown);
    }

    /// Scripts the session's fake CLI, from its next start: it completes a turn for each of
    /// `prompts`, in order, `turn-n` for each `(n, text)`.
    fn script(&self, prompts: &[(u64, &str)]) {
        let lines: Vec<Value> = prompts
            .iter()
            .flat_map(|(n, text)| turn(*n, text))
            .collect();
        self.script_lines(&lines);
    }

    /// Scripts the session's fake CLI with `lines`, from its next start.
    fn script_lines(&self, lines: &[Value]) {
        let script: String = lines.iter().map(|line| format!("{line}\n")).collect();
        std::fs::write(self.root.join("script.jsonl"), script).unwrap();
    }

    /// The follow-up prompts the session got: who sent each, why, and its text.
    async fn follow_ups(&self, session_id: &SessionId) -> Vec<(Option<UserId>, FollowUp, String)> {
        self.manager
            .read_since(session_id, 0, 10_000)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|event| match event.body {
                EventBody::ItemAdded { item } => match (item.follow_up, item.body) {
                    (Some(follow_up), ItemBody::UserMessage { text, .. }) => {
                        Some((event.by, follow_up, text))
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    /// The session's status as its journal last said.
    async fn status(&self, session_id: &SessionId) -> SessionStatus {
        let events = self
            .manager
            .read_since(session_id, 0, 10_000)
            .await
            .unwrap();
        events
            .into_iter()
            .filter_map(|event| match event.body {
                EventBody::SessionStatusChanged { status, .. } => Some(status),
                _ => None,
            })
            .next_back()
            .unwrap_or(SessionStatus::Idle)
    }

    /// Waits until the session's status is `status`, after turn `turn` ended.
    async fn settled(&self, session_id: &SessionId, turn: u64, status: SessionStatus) {
        for _ in 0..500 {
            if self.turns.load(Ordering::SeqCst) >= turn && self.status(session_id).await == status
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let events = self
            .manager
            .read_since(session_id, 0, 10_000)
            .await
            .unwrap();
        let failures: Vec<&EventBody> = events
            .iter()
            .map(|event| &event.body)
            .filter(|body| matches!(body, EventBody::TurnFailed { .. }))
            .collect();
        panic!(
            "the session is {:?}, not {status:?} after turn {turn}: {failures:?}",
            self.status(session_id).await
        );
    }

    /// Creates a session; returns it with its worktree and branch.
    async fn session(&self) -> (SessionId, PathBuf, String) {
        let result = self
            .manager
            .handle(
                alice(),
                CommandBody::CreateSession {
                    repo: Some(self.repo.to_str().unwrap().to_owned()),
                    project_id: None,
                    branch: None,
                    account_id: Some(AccountId::new("account-1")),
                    provider: None,
                    model: None,
                    permission_mode: Some(PermissionMode::Ask),
                    failover_pin: None,
                },
            )
            .await
            .unwrap();
        let CommandResult::SessionCreated { session_id } = result else {
            panic!("expected a session, got {result:?}");
        };
        let worktree = self.data.join(format!(
            "worktrees/app-{}",
            herder_daemon::worktree::slug(&session_id)
        ));
        let branch = git(&worktree, &["branch", "--show-current"]).await;
        (session_id, worktree, branch)
    }

    /// The git config entries in the environment the session's CLI starts with, once a prompt
    /// starts it.
    async fn cli_git_config(&self, session_id: &SessionId) -> BTreeMap<String, String> {
        let command = CommandBody::SendPrompt {
            session_id: session_id.clone(),
            text: "Go.".into(),
            images: Vec::new(),
            files: Vec::new(),
        };
        assert_eq!(self.command(command).await, Ok(CommandResult::Applied));
        for _ in 0..500 {
            if let Some(start) = self.starts.lock().unwrap().first() {
                return start
                    .env
                    .iter()
                    .filter(|(var, _)| var.starts_with("GIT_CONFIG_"))
                    .map(|(var, value)| (var.clone(), value.clone()))
                    .collect();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the session's CLI never started");
    }

    /// The session's pull request events, in order.
    async fn pr_events(&self, session_id: &SessionId) -> Vec<(Option<UserId>, EventBody)> {
        self.manager
            .read_since(session_id, 0, 10_000)
            .await
            .unwrap()
            .into_iter()
            .filter(|event| {
                matches!(
                    event.body,
                    EventBody::PrLinked { .. }
                        | EventBody::PrUpdated { .. }
                        | EventBody::PrUnlinked { .. }
                )
            })
            .map(|event| (event.by, event.body))
            .collect()
    }

    /// Polls, then checks the session's latest event is a `pr_updated` carrying `pr`.
    async fn updated(&self, session_id: &SessionId, pr: &PullRequest) {
        self.poll().await;
        let events = self.pr_events(session_id).await;
        let pr = pr.clone();
        assert_eq!(events.last(), Some(&(None, EventBody::PrUpdated { pr })));
    }

    async fn poll(&self) {
        self.tracker.poll().await.unwrap();
    }

    async fn command(&self, command: CommandBody) -> Result<CommandResult, ErrorCode> {
        self.manager
            .handle(alice(), command)
            .await
            .map_err(|error| error.code)
    }
}

/// A daemon's session manager on `data`, tracking pull requests on `github`: its sessions run
/// the fake CLI's script `root/script.jsonl`.
async fn daemon(
    root: &Path,
    data: &Path,
    github: &Arc<FakeGitHub>,
    turns: &Arc<AtomicU64>,
    follow_ups: bool,
) -> (
    SessionManager,
    Arc<PrTracker>,
    Arc<Mutex<Vec<StartRequest>>>,
    CancellationToken,
) {
    let fake = Provider::Other("fake".into());
    let starts = Arc::new(Mutex::new(Vec::new()));
    let mut adapters = Adapters::new();
    adapters.register(
        fake.clone(),
        Arc::new(Recording {
            fake: FakeAdapter::new(root.join("script.jsonl")),
            starts: starts.clone(),
        }),
    );
    let mut accounts = Accounts::new();
    accounts.insert(
        AccountId::new("account-1"),
        AccountConfig {
            provider: fake,
            label: "Account 1".into(),
            config_dir: Some(root.join("account")),
        },
    );
    let turns = Arc::clone(turns);
    let setup = Setup {
        store: Store::open(data.join("herder.db")).unwrap(),
        adapters,
        accounts,
        sink: Arc::new(Silent),
        turn_ids: Box::new(move || {
            TurnId::new(format!("turn-{}", turns.fetch_add(1, Ordering::SeqCst) + 1))
        }),
        worktrees: Worktrees::new(data.join("worktrees")),
        attachments: data.join("attachments"),
    };
    let shutdown = CancellationToken::new();
    let manager = SessionManager::open(setup, shutdown.clone()).await.unwrap();
    // Long intervals: tests poll by hand.
    let tracker = manager
        .track_prs(prs::Config {
            data_dir: data.to_owned(),
            herder: PathBuf::from(env!("CARGO_BIN_EXE_herder")),
            github: github.clone(),
            fast: Duration::from_secs(3600),
            slow: Duration::from_secs(3600),
            follow_ups,
        })
        .await
        .unwrap();
    (manager, tracker, starts, shutdown)
}

impl Drop for World {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// Commits a new file in `worktree` with `message`; returns the commit's message as stored.
async fn commit(worktree: &Path, file: &str, message: &str, hooks: bool) -> String {
    std::fs::write(worktree.join(file), file).unwrap();
    git(worktree, &["add", file]).await;
    if hooks {
        git(worktree, &["commit", "--quiet", "-m", message]).await;
    } else {
        git(
            worktree,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--quiet",
                "-m",
                message,
            ],
        )
        .await;
    }
    git(worktree, &["log", "-1", "--format=%B"]).await
}

/// Pull request `number` as herder tracks it, headed by `branch` of the fake GitHub.
fn pr(world: &World, number: u64, title: &str, branch: &str) -> PullRequest {
    PullRequest {
        number,
        url: format!("https://github.com/acme/app/pull/{number}"),
        title: title.to_owned(),
        head_branch: Some(branch.to_owned()),
        head_sha: Some(world.github.head_sha(branch)),
        unresolved_threads: Some(0),
        state: PrState::Open,
        ci: CiStatus::None,
        review: ReviewStatus::None,
        mergeable: Mergeable::Clean,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_commits_carry_the_session_trailer_and_keep_the_users_hooks() {
    let world = World::new().await;
    let (session_id, worktree, _) = world.session().await;
    let message = commit(&worktree, "a.txt", "Add a", true).await;
    assert_eq!(message, format!("Add a\n\nHerder-Session: {session_id}"));
    // Amending keeps a single trailer.
    git(&worktree, &["commit", "--quiet", "--amend", "-m", &message]).await;
    let amended = git(&worktree, &["log", "-1", "--format=%B"]).await;
    assert_eq!(amended.matches("Herder-Session").count(), 1, "{amended}");
    git(&worktree, &["push", "--quiet", "origin", "HEAD"]).await;
    let prepare = std::fs::read_to_string(world.root.join("user-prepare")).unwrap();
    assert_eq!(prepare.lines().count(), 2, "{prepare}");
    let pre_push = std::fs::read_to_string(world.root.join("user-pre-push")).unwrap();
    assert!(pre_push.contains("origin"), "{pre_push}");
    // The repository's own worktree keeps its own hooks.
    assert!(!config_set(&world.repo, "core.hooksPath").await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pr_on_a_branch_the_agent_pushed_under_another_name_is_linked() {
    let world = World::new().await;
    let (session_id, worktree, _) = world.session().await;
    // No trailer: only the pre-push report can tell which session this is.
    let message = commit(&worktree, "a.txt", "Add a", false).await;
    assert!(!message.contains("Herder-Session"), "{message}");
    git(&worktree, &["push", "--quiet", "origin", "HEAD:feature-x"]).await;
    world.poll().await;
    assert_eq!(world.pr_events(&session_id).await, []);

    let number = world.github.open("feature-x", "Add a");
    world.poll().await;
    assert_eq!(
        world.pr_events(&session_id).await,
        [(
            None,
            EventBody::PrLinked {
                pr: pr(&world, number, "Add a", "feature-x")
            }
        )]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pr_opened_elsewhere_on_the_session_branch_is_linked_and_followed() {
    let world = World::new().await;
    let (session_id, worktree, branch) = world.session().await;
    commit(&worktree, "a.txt", "Add a", false).await;
    // Pushed without hooks: the branch is the session's own, so polling finds it anyway.
    git(
        &worktree,
        &["push", "--quiet", "--no-verify", "origin", "HEAD"],
    )
    .await;
    let other = world.session().await;
    let number = world.github.open(&branch, "Add a");
    world.poll().await;
    let mut expected = pr(&world, number, "Add a", &branch);
    assert_eq!(
        world.pr_events(&session_id).await,
        [(
            None,
            EventBody::PrLinked {
                pr: expected.clone()
            }
        )]
    );
    assert_eq!(world.pr_events(&other.0).await, []);

    world
        .github
        .change(number, |pr| pr.checks = vec![("test", "in_progress", None)]);
    expected.ci = CiStatus::Pending;
    world.updated(&session_id, &expected).await;

    world.github.change(number, |pr| {
        pr.checks = vec![("test", "completed", Some("success"))];
        pr.reviews = vec![("bob", "APPROVED")];
    });
    expected.ci = CiStatus::Passing;
    expected.review = ReviewStatus::Approved;
    world.updated(&session_id, &expected).await;

    world.github.change(number, |pr| pr.mergeable = Some(false));
    expected.mergeable = Mergeable::Conflicting;
    world.updated(&session_id, &expected).await;

    world.github.change(number, |pr| {
        pr.state = "closed";
        pr.merged = true;
        pr.mergeable = None;
    });
    expected.state = PrState::Merged;
    expected.mergeable = Mergeable::Unknown;
    // Review threads are not read for a pull request no longer open.
    expected.unresolved_threads = None;
    world.updated(&session_id, &expected).await;
    assert_eq!(world.pr_events(&session_id).await.len(), 5);

    // A merged pull request is final: it is not read again.
    world.github.take_calls();
    world.poll().await;
    let calls = world.github.take_calls();
    assert!(
        !calls.iter().any(|call| call
            .path
            .starts_with(&format!("repos/acme/app/pulls/{number}"))),
        "{calls:#?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pr_on_a_renamed_branch_is_found_by_its_trailer() {
    let world = World::new().await;
    let (session_id, worktree, branch) = world.session().await;
    commit(&worktree, "a.txt", "Add a", true).await;
    git(&worktree, &["push", "--quiet", "origin", "HEAD"]).await;
    // Renamed on GitHub: neither the session's branch nor its push names the new one.
    git(&world.bare, &["branch", "-m", &branch, "renamed-on-github"]).await;
    let number = world.github.open("renamed-on-github", "Add a");
    world.poll().await;
    assert_eq!(
        world.pr_events(&session_id).await,
        [(
            None,
            EventBody::PrLinked {
                pr: pr(&world, number, "Add a", "renamed-on-github")
            }
        )]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pr_from_a_worktree_the_agent_added_is_linked() {
    let world = World::new().await;
    let (session_id, _, _) = world.session().await;
    let env = world.cli_git_config(&session_id).await;
    // Claude Code's Agent tool adds its worktrees under the main worktree, running git there:
    // they get none of the session worktree's config.
    let agent = world.repo.join(".claude/worktrees/agent-1");
    agent_git(
        &world.repo,
        &env,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "agent-branch",
            agent.to_str().unwrap(),
            "main",
        ],
    )
    .await;
    std::fs::write(agent.join("a.txt"), "a").unwrap();
    agent_git(&agent, &env, &["add", "a.txt"]).await;
    agent_git(&agent, &env, &["commit", "--quiet", "-m", "Add a"]).await;
    let message = git(&agent, &["log", "-1", "--format=%B"]).await;
    assert_eq!(message, format!("Add a\n\nHerder-Session: {session_id}"));
    agent_git(
        &agent,
        &env,
        &["push", "--quiet", "origin", "HEAD:feature-y"],
    )
    .await;
    // The repository's own hooks still ran.
    let prepare = std::fs::read_to_string(world.root.join("user-prepare")).unwrap();
    assert_eq!(prepare.lines().count(), 1, "{prepare}");
    let pre_push = std::fs::read_to_string(world.root.join("user-pre-push")).unwrap();
    assert!(pre_push.contains("origin"), "{pre_push}");

    let number = world.github.open("feature-y", "Add a");
    world.poll().await;
    assert_eq!(
        world.pr_events(&session_id).await,
        [(
            None,
            EventBody::PrLinked {
                pr: pr(&world, number, "Add a", "feature-y")
            }
        )]
    );

    // Another repository the agent commits in only runs its own hooks.
    let other = world.root.join("other");
    std::fs::create_dir(&other).unwrap();
    git(&other, &["init", "--quiet"]).await;
    agent_git(
        &other,
        &env,
        &["commit", "--quiet", "--allow-empty", "-m", "Elsewhere"],
    )
    .await;
    let message = git(&other, &["log", "-1", "--format=%B"]).await;
    assert_eq!(message, "Elsewhere");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unlink_keeps_a_pr_unlinked_until_a_user_links_it_again() {
    let world = World::new().await;
    let (session_id, worktree, branch) = world.session().await;
    commit(&worktree, "a.txt", "Add a", true).await;
    git(&worktree, &["push", "--quiet", "origin", "HEAD"]).await;
    let number = world.github.open(&branch, "Add a");
    world.poll().await;

    let unlink = |number| CommandBody::UnlinkPr {
        session_id: session_id.clone(),
        number,
    };
    let link = |number| CommandBody::LinkPr {
        session_id: session_id.clone(),
        number,
    };
    assert_eq!(
        world.command(unlink(number)).await,
        Ok(CommandResult::Applied)
    );
    assert_eq!(
        world.command(unlink(number)).await,
        Err(ErrorCode::NotFound)
    );
    world.poll().await;
    assert_eq!(world.command(link(99)).await, Err(ErrorCode::NotFound));
    assert_eq!(
        world.command(link(number)).await,
        Ok(CommandResult::Applied)
    );
    assert_eq!(
        world.pr_events(&session_id).await,
        [
            (
                None,
                EventBody::PrLinked {
                    pr: pr(&world, number, "Add a", &branch)
                }
            ),
            (Some(alice()), EventBody::PrUnlinked { number }),
            (
                Some(alice()),
                EventBody::PrLinked {
                    pr: pr(&world, number, "Add a", &branch)
                }
            ),
        ]
    );

    // Any pull request of the repository can be linked by hand.
    git(
        &world.repo,
        &["push", "--quiet", "origin", "main:unrelated"],
    )
    .await;
    let unrelated = world.github.open("unrelated", "Unrelated");
    assert_eq!(
        world.command(link(unrelated)).await,
        Ok(CommandResult::Applied)
    );
    let events = world.pr_events(&session_id).await;
    assert_eq!(
        events.last(),
        Some(&(
            Some(alice()),
            EventBody::PrLinked {
                pr: pr(&world, unrelated, "Unrelated", "unrelated")
            }
        ))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn polling_an_unchanged_pr_costs_only_not_modified_answers() {
    let world = World::new().await;
    let (session_id, worktree, branch) = world.session().await;
    commit(&worktree, "a.txt", "Add a", true).await;
    git(&worktree, &["push", "--quiet", "origin", "HEAD"]).await;
    world.github.open(&branch, "Add a");
    world.poll().await;
    world.poll().await;
    world.github.take_calls();
    let before = world.pr_events(&session_id).await;

    world.poll().await;
    let calls = world.github.take_calls();
    assert!(!calls.is_empty());
    for call in &calls {
        assert!(call.conditional && call.answer == "304", "{call:?}");
    }
    assert_eq!(world.pr_events(&session_id).await, before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_head_commit_journals_one_update() {
    let world = World::new().await;
    let (session_id, worktree, branch) = world.session().await;
    commit(&worktree, "a.txt", "Add a", true).await;
    git(&worktree, &["push", "--quiet", "origin", "HEAD"]).await;
    let number = world.github.open(&branch, "Add a");
    world.poll().await;
    let linked = pr(&world, number, "Add a", &branch);
    assert_eq!(
        world.pr_events(&session_id).await,
        [(None, EventBody::PrLinked { pr: linked.clone() })]
    );

    commit(&worktree, "b.txt", "Add b", true).await;
    git(&worktree, &["push", "--quiet", "origin", "HEAD"]).await;
    world.poll().await;
    world.poll().await;
    let updated = pr(&world, number, "Add a", &branch);
    assert_ne!(updated.head_sha, linked.head_sha);
    assert_eq!(
        world.pr_events(&session_id).await,
        [
            (None, EventBody::PrLinked { pr: linked }),
            (None, EventBody::PrUpdated { pr: updated }),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archive_removes_the_hooks_and_the_repository_config_with_the_last_session() {
    let world = World::new().await;
    let (first, first_worktree, _) = world.session().await;
    let (second, second_worktree, _) = world.session().await;
    let hooks = world.data.join("hooks");
    let hooks_path = git(
        &first_worktree,
        &["config", "--worktree", "--get", "core.hooksPath"],
    )
    .await;
    assert_eq!(PathBuf::from(hooks_path), hooks.join(first.as_str()));
    assert_eq!(
        git(
            &world.repo,
            &["config", "--get", "extensions.worktreeConfig"]
        )
        .await,
        "true"
    );

    let archive = |session_id: &SessionId| CommandBody::ArchiveSession {
        session_id: session_id.clone(),
    };
    world.command(archive(&first)).await.unwrap();
    assert!(!hooks.join(first.as_str()).exists());
    // The worktree stays for days, without the hooks.
    assert!(first_worktree.is_dir());
    // The second session still needs per-worktree config.
    assert!(config_set(&world.repo, "extensions.worktreeConfig").await);
    assert!(hooks.join(second.as_str()).join("pre-push").is_file());

    world.command(archive(&second)).await.unwrap();
    assert!(second_worktree.is_dir());
    assert!(!hooks.join(second.as_str()).exists());
    assert!(!config_set(&world.repo, "extensions.worktreeConfig").await);
    let config = std::fs::read_to_string(world.repo.join(".git/config")).unwrap();
    assert!(!config.contains("[herder]"), "{config}");
}

/// The fake CLI's script of a turn `turn-n` that the prompt `text` starts and that completes.
fn turn(n: u64, text: &str) -> [Value; 3] {
    let turn = format!("turn-{n}");
    [
        json!({"expect": {"type": "send_prompt", "turn_id": turn, "text": text}}),
        json!({"emit": {"type": "turn_started", "turn_id": turn}}),
        json!({"emit": {"type": "turn_completed", "turn_id": turn}}),
    ]
}

/// A session with pull request #1 open on its branch, linked, with its checks running: the
/// world, the session and the pull request's head commit.
async fn pr_with_checks_running(follow_ups: bool) -> (World, SessionId, String) {
    let world = World::open(follow_ups).await;
    let (session_id, worktree, branch) = world.session().await;
    commit(&worktree, "a.txt", "Add a", true).await;
    git(&worktree, &["push", "--quiet", "origin", "HEAD"]).await;
    let number = world.github.open(&branch, "Add a");
    world.poll().await;
    world
        .github
        .change(number, |pr| pr.checks = vec![("test", "in_progress", None)]);
    world.poll().await;
    let sha = world.github.head_sha(&branch);
    (world, session_id, sha)
}

fn follow_up(reason: FollowUpReason, sha: &str) -> FollowUp {
    FollowUp {
        reason,
        pr: Some(1),
        head_sha: Some(sha.to_owned()),
    }
}

const CI_FAILED: &str = "CI failed on #1: test, lint. Find out why, fix it and push.";

/// Fails #1's checks `test` and `lint`.
fn fail_checks(world: &World) {
    world.github.change(1, |pr| {
        pr.checks = vec![
            ("test", "completed", Some("failure")),
            ("lint", "completed", Some("timed_out")),
            ("build", "completed", Some("success")),
        ];
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failing_checks_prompt_an_idle_session_once_naming_them() {
    let (world, session_id, sha) = pr_with_checks_running(true).await;
    world.script(&[(1, CI_FAILED)]);
    fail_checks(&world);
    world.poll().await;
    world.settled(&session_id, 1, SessionStatus::Idle).await;
    world.poll().await;
    assert_eq!(
        world.follow_ups(&session_id).await,
        [(
            None,
            follow_up(FollowUpReason::CiFailed, &sha),
            CI_FAILED.into()
        )]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn passing_checks_prompt_to_finish_and_count_unresolved_threads() {
    let (world, session_id, sha) = pr_with_checks_running(true).await;
    let text = "CI is green on #1. Finish it the way your instructions say, e.g. merge it and \
                close the todo, or report that it is done. It has 2 unresolved review threads: \
                resolve them first.";
    world.script(&[(1, text)]);
    // The threads came with a review while the checks ran.
    world.github.change(1, |pr| {
        pr.reviews = vec![("bob", "COMMENTED")];
        pr.threads = vec![false, false];
    });
    world.poll().await;
    world.github.change(1, |pr| {
        pr.checks = vec![("test", "completed", Some("success"))];
        pr.threads = vec![false, true, false];
    });
    world.poll().await;
    world.settled(&session_id, 1, SessionStatus::Idle).await;
    assert_eq!(
        world.follow_ups(&session_id).await,
        [(None, follow_up(FollowUpReason::CiPassed, &sha), text.into())]
    );

    // Polling the unchanged pull request reads its review threads no more.
    world.github.take_calls();
    world.poll().await;
    let calls = world.github.take_calls();
    assert!(calls.iter().all(|call| call.answer == "304"), "{calls:#?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_conflict_and_requested_changes_prompt_one_after_another() {
    let (world, session_id, sha) = pr_with_checks_running(true).await;
    let conflicting = "#1 has merge conflicts with its base branch. Rebase or merge the base, \
                       resolve them and push.";
    let changes = "A reviewer requested changes on #1. Address the review and push.";
    world.script(&[(1, conflicting), (2, changes)]);
    world.github.change(1, |pr| {
        pr.mergeable = Some(false);
        pr.reviews = vec![("bob", "CHANGES_REQUESTED")];
    });
    world.poll().await;
    world.settled(&session_id, 1, SessionStatus::Idle).await;
    // The second waited for the session to be idle again.
    world.poll().await;
    world.settled(&session_id, 2, SessionStatus::Idle).await;
    world.poll().await;
    assert_eq!(
        world.follow_ups(&session_id).await,
        [
            (
                None,
                follow_up(FollowUpReason::Conflicting, &sha),
                conflicting.into()
            ),
            (
                None,
                follow_up(FollowUpReason::ChangesRequested, &sha),
                changes.into()
            ),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_follow_up_is_not_sent_again_after_a_restart() {
    let (mut world, session_id, sha) = pr_with_checks_running(true).await;
    world.script(&[(1, CI_FAILED)]);
    fail_checks(&world);
    world.poll().await;
    world.settled(&session_id, 1, SessionStatus::Idle).await;

    world.restart().await;
    world.poll().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        world.follow_ups(&session_id).await,
        [(
            None,
            follow_up(FollowUpReason::CiFailed, &sha),
            CI_FAILED.into()
        )]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_follow_up_waits_while_the_session_needs_the_user() {
    let (world, session_id, sha) = pr_with_checks_running(true).await;
    // The user's turn waits on an approval: the session needs the user.
    let mut lines = vec![
        json!({"expect": {"type": "send_prompt", "turn_id": "turn-1", "text": "Go."}}),
        json!({"emit": {"type": "turn_started", "turn_id": "turn-1"}}),
        json!({"emit": {"type": "item_completed", "item": {"id": "item-1", "turn_id": "turn-1", "body": {"type": "tool_call", "name": "Bash", "input": {"command": "rm -rf target"}}}}}),
        json!({"emit": {"type": "approval_requested", "approval_id": "approval-1", "turn_id": "turn-1", "tool_call_id": "item-1", "summary": "Run rm -rf target"}}),
        json!({"expect": {"type": "answer_approval", "approval_id": "approval-1", "decision": "allow"}}),
        json!({"emit": {"type": "turn_completed", "turn_id": "turn-1"}}),
    ];
    lines.extend(turn(2, CI_FAILED));
    world.script_lines(&lines);
    let prompt = CommandBody::SendPrompt {
        session_id: session_id.clone(),
        text: "Go.".into(),
        images: Vec::new(),
        files: Vec::new(),
    };
    assert_eq!(world.command(prompt).await, Ok(CommandResult::Applied));
    world.settled(&session_id, 1, SessionStatus::NeedsYou).await;

    fail_checks(&world);
    world.poll().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(world.follow_ups(&session_id).await, []);
    assert_eq!(world.status(&session_id).await, SessionStatus::NeedsYou);

    // Once the user lets the turn finish, the session gets it.
    let events = world
        .manager
        .read_since(&session_id, 0, 10_000)
        .await
        .unwrap();
    let approval_id = events
        .into_iter()
        .find_map(|event| match event.body {
            EventBody::ApprovalRequested { approval_id, .. } => Some(approval_id),
            _ => None,
        })
        .unwrap();
    let answer = CommandBody::AnswerApproval {
        session_id: session_id.clone(),
        approval_id,
        decision: ApprovalDecision::Allow,
    };
    assert_eq!(world.command(answer).await, Ok(CommandResult::Applied));
    world.settled(&session_id, 1, SessionStatus::Idle).await;
    world.poll().await;
    world.settled(&session_id, 2, SessionStatus::Idle).await;
    assert_eq!(
        world.follow_ups(&session_id).await,
        [(
            None,
            follow_up(FollowUpReason::CiFailed, &sha),
            CI_FAILED.into()
        )]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_follow_up_when_pr_events_are_off() {
    let (world, session_id, _) = pr_with_checks_running(false).await;
    fail_checks(&world);
    world.poll().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(world.follow_ups(&session_id).await, []);
    assert_eq!(world.status(&session_id).await, SessionStatus::Idle);
}
