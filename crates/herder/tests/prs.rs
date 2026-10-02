//! Pull request tracking end to end: real git hooks in real session worktrees, calling the
//! built `herder` binary, pushing to a bare "GitHub" remote, with GitHub's REST API faked from
//! that remote's contents.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use herder_adapters::fake::FakeAdapter;
use herder_daemon::prs::{self, Fetched, GetFuture, GitHub, PrTracker};
use herder_daemon::session::{AccountConfig, Accounts, Adapters, EventSink, SessionManager, Setup};
use herder_daemon::worktree::Worktrees;
use herder_protocol::{
    AccountId, CiStatus, CommandBody, CommandResult, ErrorCode, Event, EventBody, Item, ItemId,
    Mergeable, PermissionMode, PrState, Provider, PullRequest, ReviewStatus, SessionHead,
    SessionId, Timestamp, UserId,
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

fn git_sync(dir: &Path, args: &[String]) -> String {
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
    /// Check runs on its head commit: status and conclusion.
    checks: Vec<(&'static str, Option<&'static str>)>,
    /// Reviews: reviewer and verdict.
    reviews: Vec<(&'static str, &'static str)>,
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
                            .map(|(status, conclusion)| {
                                json!({"status": status, "conclusion": conclusion})
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
    shutdown: CancellationToken,
}

impl World {
    async fn new() -> Self {
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

        let fake = Provider::Other("fake".into());
        let mut adapters = Adapters::new();
        adapters.register(
            fake.clone(),
            Arc::new(FakeAdapter::new(root.join("unused.jsonl"))),
        );
        let mut accounts = Accounts::new();
        accounts.insert(
            AccountId::new("account-1"),
            AccountConfig {
                provider: fake,
                config_dir: root.join("account"),
            },
        );
        let data = root.join("data");
        std::fs::create_dir(&data).unwrap();
        let setup = Setup {
            store: Store::open(data.join("herder.db")).unwrap(),
            adapters,
            accounts,
            sink: Arc::new(Silent),
            turn_ids: herder_daemon::session::ulid_turn_ids(),
            worktrees: Worktrees::new(data.join("worktrees")),
        };
        let shutdown = CancellationToken::new();
        let manager = SessionManager::open(setup, shutdown.clone()).await.unwrap();
        let github = Arc::new(FakeGitHub {
            bare: bare.clone(),
            prs: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
        });
        // Long intervals: tests poll by hand.
        let tracker = manager
            .track_prs(prs::Config {
                data_dir: data.clone(),
                herder: PathBuf::from(env!("CARGO_BIN_EXE_herder")),
                github: github.clone(),
                fast: Duration::from_secs(3600),
                slow: Duration::from_secs(3600),
            })
            .await
            .unwrap();
        Self {
            _tmp: tmp,
            root,
            repo,
            bare,
            data,
            manager,
            tracker,
            github,
            shutdown,
        }
    }

    /// Creates a session; returns it with its worktree and branch.
    async fn session(&self) -> (SessionId, PathBuf, String) {
        let result = self
            .manager
            .handle(
                alice(),
                CommandBody::CreateSession {
                    repo: self.repo.to_str().unwrap().to_owned(),
                    branch: None,
                    account_id: AccountId::new("account-1"),
                    model: None,
                    permission_mode: PermissionMode::Ask,
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

fn pr(number: u64, title: &str) -> PullRequest {
    PullRequest {
        number,
        url: format!("https://github.com/acme/app/pull/{number}"),
        title: title.to_owned(),
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
                pr: pr(number, "Add a")
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
    let mut expected = pr(number, "Add a");
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
        .change(number, |pr| pr.checks = vec![("in_progress", None)]);
    expected.ci = CiStatus::Pending;
    world.updated(&session_id, &expected).await;

    world.github.change(number, |pr| {
        pr.checks = vec![("completed", Some("success"))];
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
                pr: pr(number, "Add a")
            }
        )]
    );
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
                    pr: pr(number, "Add a")
                }
            ),
            (Some(alice()), EventBody::PrUnlinked { number }),
            (
                Some(alice()),
                EventBody::PrLinked {
                    pr: pr(number, "Add a")
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
                pr: pr(unrelated, "Unrelated")
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
        force: false,
    };
    world.command(archive(&first)).await.unwrap();
    assert!(!hooks.join(first.as_str()).exists());
    assert!(!first_worktree.exists());
    // The second session still needs per-worktree config.
    assert!(config_set(&world.repo, "extensions.worktreeConfig").await);
    assert!(hooks.join(second.as_str()).join("pre-push").is_file());

    world.command(archive(&second)).await.unwrap();
    assert!(!second_worktree.exists());
    assert!(!hooks.join(second.as_str()).exists());
    assert!(!config_set(&world.repo, "extensions.worktreeConfig").await);
    let config = std::fs::read_to_string(world.repo.join(".git/config")).unwrap();
    assert!(!config.contains("[herder]"), "{config}");
}
