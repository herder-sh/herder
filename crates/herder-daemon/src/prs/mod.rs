//! Pull request tracking: which GitHub pull requests belong to which session, and their state.
//!
//! # Which pull requests
//!
//! A session's pull requests live in its repository's GitHub repository, the one `gh` would
//! pick: the first of the remotes `upstream`, `github` and `origin`, else the only remote, unless
//! a remote is marked `gh-resolved = base`. A pull request belongs to a session when its head
//! is:
//!
//! - a branch the session owns (see [`crate::worktree`]), or
//! - a branch the session's worktree pushed, as its `pre-push` hook reports (see [`hooks`]), on
//!   whichever remote; this catches `git push origin HEAD:other-name` and pushes to a fork.
//!
//! or when one of its commits carries the session's `Herder-Session` trailer, which the
//! worktree's commit hooks add; this catches a branch renamed on GitHub, or pushed under a
//! name herder never saw. The agent's git commands run the session's hooks in any worktree of
//! the repository, so pushes and commits from worktrees the agent adds itself, such as those
//! of Claude Code's `Agent` tool, count as the session's too. A closed or merged pull request
//! found by branch only counts when it was opened after the session was created, so a reused
//! branch name does not pull in history.
//!
//! Pull requests found this way are linked with `pr_linked` (no `by`). A user can link any pull
//! request of the repository with `link_pr`, and unlink one with `unlink_pr`; an unlinked pull
//! request is not linked automatically again until a user links it.
//!
//! # State
//!
//! Each linked pull request is read from GitHub with its head commit's check runs and statuses,
//! its reviews and its unresolved review threads, and journaled as `pr_updated` whenever any of
//! what [`PullRequest`] holds changes, its head commit included. A merged pull request is final
//! and no longer read.
//!
//! # Polling
//!
//! Every read is a conditional request with the ETag of the last response, so an unchanged
//! resource costs a `304` that does not count against GitHub's rate limit. A session is polled
//! every [`Config::fast`] while its agent works or needs the user, for a while after it pushed,
//! and while one of its open pull requests has checks running or mergeability not computed yet;
//! every [`Config::slow`] otherwise. An archived session is polled while it has an open pull
//! request, and keeps looking for new ones for [`ARCHIVED_DISCOVERY`] after its last event.
//!
//! Only the first 100 pull requests, commits, check runs, reviews and review threads of each
//! list are read. Review threads are only in GraphQL, which has no conditional requests: they
//! are read again only when the pull request, its checks or its reviews changed.
//!
//! # Follow-ups
//!
//! With `[follow_ups] pr_events` on, an idle session is prompted when one of its open pull
//! requests changes so that its agent has something to do: its checks fail (naming them), its
//! checks pass, it stops merging cleanly, or a reviewer requests changes. A change is a
//! `pr_updated` whose pull request meets the condition when the one before did not; one that
//! comes while the session is not idle is delivered once it is, if the pull request still
//! meets it on the same head commit. The prompt is a `user_message` without `by` that carries
//! [`FollowUp`]; at most one is sent per pull request, head commit and reason. Both what
//! changed and what was sent are read back from the journal, so a restart neither loses nor
//! repeats one.

mod github;
pub mod hooks;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use herder_protocol::{
    CiStatus, CommandResult, ErrorCode, ErrorInfo, Event, EventBody, FollowUp, FollowUpReason,
    ItemBody, Mergeable, PrState, PullRequest, ReviewStatus, SessionId, SessionStatus, Timestamp,
    UserId,
};
use herder_store::Session;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

pub use github::{Fetched, GetFuture, GhCli, GhRepo, GitHub, GraphqlFuture, REVIEW_THREADS};

use crate::session::journal::Journal;
use crate::worktree::{self, git};
use github::{ApiCheckRuns, ApiCommit, ApiPull, ApiReview, ApiStatus, encode_query};

/// The reasons a pull request event prompts a session, in the order they are sent.
const PR_REASONS: [FollowUpReason; 4] = [
    FollowUpReason::CiFailed,
    FollowUpReason::Conflicting,
    FollowUpReason::ChangesRequested,
    FollowUpReason::CiPassed,
];
use hooks::{Reply, Report};

/// Default for [`Config::fast`].
pub const FAST: Duration = Duration::from_secs(20);

/// Default for [`Config::slow`].
pub const SLOW: Duration = Duration::from_secs(5 * 60);

/// How long a session is polled fast after it pushed.
const HOT: Duration = Duration::from_secs(15 * 60);

/// How long an archived session keeps looking for new pull requests after its last event.
pub const ARCHIVED_DISCOVERY: jiff::SignedDuration = jiff::SignedDuration::from_hours(7 * 24);

/// Cached responses unused for this long are dropped.
const CACHE_TTL: Duration = Duration::from_secs(60 * 60);

/// Largest report a hook may send.
const MAX_REPORT: u64 = 64 * 1024;

/// How long a hook connection may take to send its report.
const REPORT_TIMEOUT: Duration = Duration::from_secs(5);

/// What the tracker runs on.
pub struct Config {
    /// The daemon's data dir: hooks go in `hooks/`, the hook socket is `hooks.sock`, reported
    /// pushes are kept in `prs/`.
    pub data_dir: PathBuf,
    /// The herder binary the hooks run.
    pub herder: PathBuf,
    /// GitHub; [`GhCli`] outside tests.
    pub github: Arc<dyn GitHub>,
    /// Poll interval of active sessions; [`FAST`] outside tests.
    pub fast: Duration,
    /// Poll interval of everything else; [`SLOW`] outside tests.
    pub slow: Duration,
    /// Whether idle sessions are prompted on their pull requests' events: `[follow_ups]
    /// pr_events`.
    pub follow_ups: bool,
}

/// Sends a session a follow-up prompt if it is idle; resolves to whether it was sent.
pub(crate) type Prompter = Box<
    dyn Fn(SessionId, String, FollowUp) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync,
>;

/// A branch, on the GitHub account `owner`, that may head a session's pull request.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Head {
    owner: String,
    branch: String,
}

/// Finds and follows every session's pull requests. Owned by the session manager.
pub struct PrTracker {
    journal: Journal,
    config: Config,
    prompter: Prompter,
    state: Mutex<State>,
    /// One poll pass at a time.
    polling: tokio::sync::Mutex<()>,
    /// Serializes deciding to link, unlink or update a pull request with journaling it.
    links: tokio::sync::Mutex<()>,
    /// Wakes the poll loop early, after a push.
    wake: Notify,
}

#[derive(Default)]
struct State {
    /// Last response of each `host/path`, for conditional requests.
    cache: HashMap<String, Cached>,
    sessions: HashMap<SessionId, Tracked>,
    /// Sessions named by trailers in each open pull request's commits, with its head commit.
    trailers: HashMap<(GhRepo, u64), (String, Vec<SessionId>)>,
    /// Each open pull request's unresolved review threads.
    threads: HashMap<(GhRepo, u64), Threads>,
    /// The checks that failed on each pull request's head commit when it was last read.
    failing: HashMap<(GhRepo, u64), Vec<String>>,
    /// The last poll failure logged, so a lasting one is logged once.
    last_error: Option<String>,
}

/// How many of a pull request's review threads were unresolved.
struct Threads {
    /// The ETags of the pull request, its head commit's check runs and its reviews when read.
    read_with: Vec<Option<String>>,
    unresolved: Option<u32>,
}

struct Cached {
    etag: String,
    body: serde_json::Value,
    used: Instant,
}

struct Tracked {
    created_at: Timestamp,
    /// Pull requests a user unlinked and has not linked since; read from the journal on first
    /// use.
    unlinked: Option<HashSet<u64>>,
    pushed: Vec<Head>,
    hot_until: Option<Instant>,
    next_due: Instant,
    /// Follow-ups the session is owed and those sent; read from the journal on first use.
    follow_ups: Option<FollowUps>,
}

#[derive(Clone, Default)]
struct FollowUps {
    /// Pull request events not followed up yet, oldest first.
    pending: Vec<FollowUp>,
    sent: Vec<FollowUp>,
}

impl FollowUps {
    /// Notes the follow-ups `pr` is owed now that it changed from `before`.
    fn changed(&mut self, before: &PullRequest, pr: &PullRequest) {
        for follow_up in owed(before, pr) {
            if !self.sent.contains(&follow_up) && !self.pending.contains(&follow_up) {
                self.pending.push(follow_up);
            }
        }
    }
}

impl PrTracker {
    /// Starts tracking: listens for hook reports, installs hooks in every live session's
    /// worktree, and polls until `shutdown`.
    pub(crate) async fn start(
        journal: Journal,
        config: Config,
        prompter: Prompter,
        shutdown: CancellationToken,
    ) -> Result<Arc<Self>> {
        let tracker = Arc::new(Self {
            journal,
            config,
            prompter,
            state: Mutex::new(State::default()),
            polling: tokio::sync::Mutex::new(()),
            links: tokio::sync::Mutex::new(()),
            wake: Notify::new(),
        });
        let socket = tracker.socket();
        // The data-dir lock is held, so a socket file left here is a dead daemon's.
        let _ = std::fs::remove_file(&socket);
        match UnixListener::bind(&socket) {
            Ok(listener) => {
                tokio::spawn(Arc::clone(&tracker).serve_hooks(listener, shutdown.clone()));
            }
            Err(err) => warn!(
                "cannot listen on {}; pushes will be found by polling only: {err}",
                socket.display()
            ),
        }
        // Rewritten at every start, for the current binary and the repositories' hooks dirs.
        let sessions = tracker.journal.sessions().await?;
        let installer = Arc::clone(&tracker);
        tokio::spawn(async move {
            for session in sessions {
                // A session without a branch works in the user's own folder: no hooks there.
                if session.branch.is_some()
                    && !matches!(
                        session.status,
                        SessionStatus::Archived | SessionStatus::Moved
                    )
                {
                    installer
                        .install(&session.session_id, Path::new(&session.worktree))
                        .await;
                }
            }
        });
        tokio::spawn(Arc::clone(&tracker).run(shutdown));
        Ok(tracker)
    }

    /// Polls every session now, whether or not it is due, and returns once done.
    pub async fn poll(&self) -> Result<()> {
        self.poll_due(true).await.map(|_| ())
    }

    /// Installs the session's hooks in its worktree; a failure is logged, as the session works
    /// without them.
    pub(crate) async fn install(&self, session_id: &SessionId, worktree: &Path) {
        if !worktree.exists() {
            return;
        }
        let installed = hooks::install(
            &self.hooks_dir(),
            &self.socket(),
            &self.config.herder,
            session_id,
            worktree,
        )
        .await;
        if let Err(err) = installed {
            warn!(%session_id, "cannot install git hooks: {err:#}");
        }
    }

    /// Points git at the session's hooks in `env`, the environment of the session's CLI, so the
    /// agent's git commands run them in any worktree; see [`hooks`].
    pub(crate) fn add_hooks_to_env(
        &self,
        env: &mut BTreeMap<String, String>,
        session_id: &SessionId,
    ) {
        hooks::add_to_env(env, &self.hooks_dir(), session_id);
    }

    /// Removes the session's hooks, and the repository config they needed once no worktree
    /// needs it; a failure is logged.
    pub(crate) async fn uninstall(&self, session_id: &SessionId, repo: &Path, worktree: &Path) {
        let removed = hooks::uninstall(&self.hooks_dir(), session_id, repo, worktree).await;
        if let Err(err) = removed {
            warn!(%session_id, "cannot remove git hooks: {err:#}");
        }
    }

    /// `link_pr` from `by`: tracks pull request `number` of the session's repository.
    pub(crate) async fn link(
        &self,
        by: UserId,
        session_id: SessionId,
        number: u64,
    ) -> Result<CommandResult, ErrorInfo> {
        let session = self.writable(&session_id).await?;
        let base = base_repo(Path::new(&session.repo)).await.ok_or_else(|| {
            error(
                ErrorCode::BadRequest,
                "the session's repository has no GitHub remote",
            )
        })?;
        self.load(&session).await.map_err(internal)?;
        let pr = self
            .fetch_pr(&base, number)
            .await
            .map_err(internal)?
            .ok_or_else(|| {
                error(
                    ErrorCode::NotFound,
                    format!(
                        "pull request #{number} does not exist in {}",
                        base.full_name()
                    ),
                )
            })?;
        let _links = self.links.lock().await;
        self.unlinked(&session_id).await.map_err(internal)?;
        self.change_unlinked(&session_id, |unlinked| {
            unlinked.remove(&number);
        });
        let linked = self
            .journal
            .prs(session_id.clone())
            .await
            .map_err(internal)?;
        if !linked.iter().any(|known| known.number == number) {
            self.journal
                .record(session_id, Some(by), EventBody::PrLinked { pr })
                .await
                .map_err(internal)?;
        }
        Ok(CommandResult::Applied)
    }

    /// `unlink_pr` from `by`: stops tracking pull request `number`, and keeps it from being
    /// linked automatically again.
    pub(crate) async fn unlink(
        &self,
        by: UserId,
        session_id: SessionId,
        number: u64,
    ) -> Result<CommandResult, ErrorInfo> {
        let session = self.writable(&session_id).await?;
        self.load(&session).await.map_err(internal)?;
        let _links = self.links.lock().await;
        let linked = self
            .journal
            .prs(session_id.clone())
            .await
            .map_err(internal)?;
        if !linked.iter().any(|known| known.number == number) {
            return Err(error(
                ErrorCode::NotFound,
                format!("pull request #{number} is not linked to the session"),
            ));
        }
        self.unlinked(&session_id).await.map_err(internal)?;
        self.journal
            .record(
                session_id.clone(),
                Some(by),
                EventBody::PrUnlinked { number },
            )
            .await
            .map_err(internal)?;
        self.change_unlinked(&session_id, |unlinked| {
            unlinked.insert(number);
        });
        Ok(CommandResult::Applied)
    }

    /// The session, if it exists and takes commands.
    async fn writable(&self, session_id: &SessionId) -> Result<Session, ErrorInfo> {
        let session = self
            .journal
            .session(session_id.clone())
            .await
            .map_err(internal)?
            .ok_or_else(|| {
                error(
                    ErrorCode::NotFound,
                    format!("session {session_id} does not exist"),
                )
            })?;
        if session.status == SessionStatus::Archived {
            return Err(error(
                ErrorCode::Conflict,
                "the session is archived and read-only",
            ));
        }
        if session.status == SessionStatus::Moved {
            return Err(error(
                ErrorCode::Conflict,
                "another host took the session over; it is read-only here",
            ));
        }
        Ok(session)
    }

    async fn run(self: Arc<Self>, shutdown: CancellationToken) {
        loop {
            let next = match self.poll_due(false).await {
                Ok(next) => next,
                Err(err) => {
                    self.failed(&err);
                    Instant::now() + self.config.fast
                }
            };
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep_until(next) => {}
                () = self.wake.notified() => {}
            }
        }
    }

    /// Polls each session that is due, or every session with `force`; returns when the next
    /// one is due.
    async fn poll_due(&self, force: bool) -> Result<Instant> {
        let _pass = self.polling.lock().await;
        let now = Instant::now();
        let mut next = now + self.config.slow;
        let mut due: HashMap<String, Vec<(Session, Vec<PullRequest>, bool)>> = HashMap::new();
        for session in self.journal.sessions().await? {
            self.load(&session).await?;
            let prs = self.journal.prs(session.session_id.clone()).await?;
            let Some((interval, discover)) = self.interval(&session, &prs, now) else {
                continue;
            };
            let mut state = self.lock();
            let Some(tracked) = state.sessions.get_mut(&session.session_id) else {
                continue;
            };
            if force || tracked.next_due <= now {
                tracked.next_due = now + interval;
                due.entry(session.repo.clone())
                    .or_default()
                    .push((session, prs, discover));
            }
            next = next.min(tracked.next_due);
        }
        for (repo, sessions) in due {
            let Some(base) = base_repo(Path::new(&repo)).await else {
                debug!(repo, "no GitHub remote; not looking for pull requests");
                continue;
            };
            if let Err(err) = self.poll_repo(&base, sessions).await {
                self.failed(&err);
            }
        }
        self.lock()
            .cache
            .retain(|_, cached| now.duration_since(cached.used) < CACHE_TTL);
        Ok(next)
    }

    /// How often to poll the session and whether to look for new pull requests; `None` when
    /// there is nothing to poll.
    fn interval(
        &self,
        session: &Session,
        prs: &[PullRequest],
        now: Instant,
    ) -> Option<(Duration, bool)> {
        let has_open = prs.iter().any(open);
        let settling = prs
            .iter()
            .filter(|pr| open(pr))
            .any(|pr| pr.ci == CiStatus::Pending || pr.mergeable == Mergeable::Unknown);
        // The host that took it over tracks its pull requests now.
        if session.status == SessionStatus::Moved {
            return None;
        }
        let archived = session.status == SessionStatus::Archived;
        let discover =
            !archived || Timestamp::now().duration_since(session.updated_at) < ARCHIVED_DISCOVERY;
        if archived && !has_open && !discover {
            return None;
        }
        let hot = self
            .lock()
            .sessions
            .get(&session.session_id)
            .and_then(|tracked| tracked.hot_until)
            .is_some_and(|until| until > now);
        let working = matches!(
            session.status,
            SessionStatus::Running | SessionStatus::NeedsYou | SessionStatus::WaitingForCapacity
        );
        let fast = hot || settling || working;
        Some((
            if fast {
                self.config.fast
            } else {
                self.config.slow
            },
            discover,
        ))
    }

    /// Polls the sessions of one repository: finds new pull requests, then refreshes linked ones.
    async fn poll_repo(
        &self,
        base: &GhRepo,
        sessions: Vec<(Session, Vec<PullRequest>, bool)>,
    ) -> Result<()> {
        let by_trailer = if sessions.iter().any(|(_, _, discover)| *discover) {
            self.scan_trailers(base).await?
        } else {
            HashMap::new()
        };
        for (session, linked, discover) in sessions {
            let mut found = Vec::new();
            if discover {
                found = self.find_by_branch(base, &session).await?;
                found.extend(by_trailer.get(&session.session_id).into_iter().flatten());
            }
            let mut linked_now: HashSet<u64> = linked.iter().map(|pr| pr.number).collect();
            for number in found {
                if linked_now.insert(number) {
                    self.link_found(base, &session.session_id, number).await?;
                }
            }
            for pr in linked {
                if pr.state != PrState::Merged {
                    self.refresh(base, &session.session_id, &pr).await?;
                }
            }
            if self.config.follow_ups && session.status == SessionStatus::Idle {
                self.follow_up(base, &session.session_id).await?;
            }
        }
        Ok(())
    }

    /// Sends the session the oldest follow-up it is owed whose pull request still calls for it;
    /// drops those whose pull request no longer does.
    async fn follow_up(&self, base: &GhRepo, session_id: &SessionId) -> Result<()> {
        let pending = self.follow_ups(session_id).await?.pending;
        if pending.is_empty() {
            return Ok(());
        }
        let prs = self.journal.prs(session_id.clone()).await?;
        for follow_up in pending {
            let Some(pr) = due(&follow_up, &prs) else {
                self.change_follow_ups(session_id, |follow_ups| {
                    follow_ups.pending.retain(|pending| *pending != follow_up);
                });
                continue;
            };
            let failing = self
                .lock()
                .failing
                .get(&(base.clone(), pr.number))
                .cloned()
                .unwrap_or_default();
            let text = prompt(pr, follow_up.reason, &failing);
            // Not sent: the session got busy since; it is tried again once idle.
            if (self.prompter)(session_id.clone(), text, follow_up.clone()).await {
                self.change_follow_ups(session_id, |follow_ups| {
                    follow_ups.pending.retain(|pending| *pending != follow_up);
                    follow_ups.sent.push(follow_up);
                });
            }
            break;
        }
        Ok(())
    }

    /// The session's follow-ups, read from the journal the first time.
    async fn follow_ups(&self, session_id: &SessionId) -> Result<FollowUps> {
        let loaded = self
            .lock()
            .sessions
            .get(session_id)
            .and_then(|tracked| tracked.follow_ups.clone());
        if let Some(follow_ups) = loaded {
            return Ok(follow_ups);
        }
        let follow_ups = replay(&self.journal.all(session_id.clone()).await?);
        if let Some(tracked) = self.lock().sessions.get_mut(session_id) {
            tracked.follow_ups = Some(follow_ups.clone());
        }
        Ok(follow_ups)
    }

    /// Runs `change` on the session's follow-ups, if they are loaded.
    fn change_follow_ups(&self, session_id: &SessionId, change: impl FnOnce(&mut FollowUps)) {
        let mut state = self.lock();
        if let Some(follow_ups) = state
            .sessions
            .get_mut(session_id)
            .and_then(|tracked| tracked.follow_ups.as_mut())
        {
            change(follow_ups);
        }
    }

    /// Pull requests headed by a branch the session owns or pushed.
    async fn find_by_branch(&self, base: &GhRepo, session: &Session) -> Result<Vec<u64>> {
        let mut heads = Vec::new();
        let owners = owners(Path::new(&session.repo), base).await;
        for branch in self.owned_branches(session).await? {
            for owner in &owners {
                heads.push(Head {
                    owner: owner.clone(),
                    branch: branch.clone(),
                });
            }
        }
        let (created_at, pushed) = match self.lock().sessions.get(&session.session_id) {
            Some(tracked) => (tracked.created_at, tracked.pushed.clone()),
            None => return Ok(Vec::new()),
        };
        for head in pushed {
            if !heads.contains(&head) {
                heads.push(head);
            }
        }
        let mut found = Vec::new();
        for head in heads {
            let path = format!(
                "repos/{}/pulls?head={}&state=all&per_page=100",
                base.full_name(),
                encode_query(&format!("{}:{}", head.owner, head.branch))
            );
            for pull in self
                .fetch::<Vec<ApiPull>>(&base.host, &path)
                .await?
                .unwrap_or_default()
            {
                if (pull.state == "open" || pull.created_at >= created_at)
                    && !found.contains(&pull.number)
                {
                    found.push(pull.number);
                }
            }
        }
        Ok(found)
    }

    /// Every branch the session owns: journaled ones, and ones its worktree has checked out
    /// since its last turn ended.
    async fn owned_branches(&self, session: &Session) -> Result<Vec<String>> {
        let mut owned = self.journal.branches(session.session_id.clone()).await?;
        if let Ok(checked_out) =
            worktree::branches(Path::new(&session.worktree), session.branch.as_deref()).await
        {
            for branch in checked_out {
                if !owned.contains(&branch) {
                    owned.push(branch);
                }
            }
        }
        Ok(owned)
    }

    /// Open pull requests of `base` by the sessions their commits' trailers name.
    async fn scan_trailers(&self, base: &GhRepo) -> Result<HashMap<SessionId, Vec<u64>>> {
        let repo = base.full_name();
        let path = format!("repos/{repo}/pulls?state=open&per_page=100");
        let open = self
            .fetch::<Vec<ApiPull>>(&base.host, &path)
            .await?
            .unwrap_or_default();
        let mut by_session: HashMap<SessionId, Vec<u64>> = HashMap::new();
        for pull in &open {
            let key = (base.clone(), pull.number);
            let known = self
                .lock()
                .trailers
                .get(&key)
                .filter(|(sha, _)| *sha == pull.head.sha)
                .map(|(_, sessions)| sessions.clone());
            let sessions = match known {
                Some(sessions) => sessions,
                None => {
                    let path = format!("repos/{repo}/pulls/{}/commits?per_page=100", pull.number);
                    let commits = self
                        .fetch::<Vec<ApiCommit>>(&base.host, &path)
                        .await?
                        .unwrap_or_default();
                    let mut sessions: Vec<SessionId> = Vec::new();
                    for commit in &commits {
                        for id in hooks::trailer_sessions(&commit.commit.message) {
                            if !sessions.iter().any(|known| known.as_str() == id) {
                                sessions.push(SessionId::new(id));
                            }
                        }
                    }
                    self.lock()
                        .trailers
                        .insert(key, (pull.head.sha.clone(), sessions.clone()));
                    sessions
                }
            };
            for session_id in sessions {
                by_session.entry(session_id).or_default().push(pull.number);
            }
        }
        let open: HashSet<u64> = open.iter().map(|pull| pull.number).collect();
        self.lock()
            .trailers
            .retain(|(repo, number), _| repo != base || open.contains(number));
        Ok(by_session)
    }

    /// Links a pull request found for the session, unless a user unlinked it or it is linked.
    async fn link_found(&self, base: &GhRepo, session_id: &SessionId, number: u64) -> Result<()> {
        let Some(pr) = self.fetch_pr(base, number).await? else {
            return Ok(());
        };
        let _links = self.links.lock().await;
        if self.unlinked(session_id).await?.contains(&number) {
            return Ok(());
        }
        let linked = self.journal.prs(session_id.clone()).await?;
        if linked.iter().any(|known| known.number == number) {
            return Ok(());
        }
        self.journal
            .record(session_id.clone(), None, EventBody::PrLinked { pr })
            .await?;
        Ok(())
    }

    /// Reads a linked pull request again and journals it if it changed.
    async fn refresh(
        &self,
        base: &GhRepo,
        session_id: &SessionId,
        known: &PullRequest,
    ) -> Result<()> {
        let Some(pr) = self.fetch_pr(base, known.number).await? else {
            return Ok(());
        };
        if pr == *known {
            return Ok(());
        }
        if self.config.follow_ups {
            // Before the update is journaled, so reading the journal does not count it twice.
            self.follow_ups(session_id).await?;
        }
        let _links = self.links.lock().await;
        let linked = self.journal.prs(session_id.clone()).await?;
        let Some(current) = linked.iter().find(|linked| linked.number == pr.number) else {
            return Ok(());
        };
        if *current != pr {
            self.journal
                .record(
                    session_id.clone(),
                    None,
                    EventBody::PrUpdated { pr: pr.clone() },
                )
                .await?;
            self.change_follow_ups(session_id, |follow_ups| follow_ups.changed(current, &pr));
        }
        Ok(())
    }

    /// Pull request `number` of `base` with its checks, reviews and review threads; `None` when
    /// it does not exist. Notes which checks failed.
    async fn fetch_pr(&self, base: &GhRepo, number: u64) -> Result<Option<PullRequest>> {
        let repo = base.full_name();
        let host = &base.host;
        let Some(pull) = self
            .fetch::<ApiPull>(host, &format!("repos/{repo}/pulls/{number}"))
            .await?
        else {
            return Ok(None);
        };
        let sha = &pull.head.sha;
        let runs_path = format!("repos/{repo}/commits/{sha}/check-runs?per_page=100");
        let runs = self
            .fetch::<ApiCheckRuns>(host, &runs_path)
            .await?
            .unwrap_or_default();
        let status = self
            .fetch::<ApiStatus>(host, &format!("repos/{repo}/commits/{sha}/status"))
            .await?
            .unwrap_or_default();
        let reviews_path = format!("repos/{repo}/pulls/{number}/reviews?per_page=100");
        let reviews = self
            .fetch::<Vec<ApiReview>>(host, &reviews_path)
            .await?
            .unwrap_or_default();
        let key = (base.clone(), number);
        let threads = if matches!(github::state(&pull), PrState::Open | PrState::Draft) {
            // A new thread comes with a review; a resolved one is read again once the pull
            // request or its checks change, as when its checks pass.
            let pull_path = format!("repos/{repo}/pulls/{number}");
            let etags: Vec<Option<String>> = {
                let state = self.lock();
                [&pull_path, &runs_path, &reviews_path]
                    .into_iter()
                    .map(|path| {
                        let cached = state.cache.get(&format!("{host}/{path}"));
                        cached.map(|cached| cached.etag.clone())
                    })
                    .collect()
            };
            let known = self
                .lock()
                .threads
                .get(&key)
                .filter(|threads| threads.read_with == etags && etags.iter().all(Option::is_some))
                .map(|threads| threads.unresolved);
            match known {
                Some(threads) => threads,
                None => {
                    let variables = serde_json::json!({
                        "owner": base.owner,
                        "name": base.name,
                        "number": number,
                    });
                    let data = self
                        .config
                        .github
                        .graphql(host, github::REVIEW_THREADS, &variables)
                        .await?;
                    let unresolved = github::unresolved_threads(&data);
                    let threads = Threads {
                        read_with: etags,
                        unresolved,
                    };
                    self.lock().threads.insert(key.clone(), threads);
                    unresolved
                }
            }
        } else {
            self.lock().threads.remove(&key);
            None
        };
        let failing = github::failing_checks(&runs, &status);
        self.lock().failing.insert(key, failing);
        Ok(Some(github::pull_request(
            &pull, &runs, &status, &reviews, threads,
        )))
    }

    /// `GET`s `path`, conditionally when a response is cached; `None` on a 404.
    async fn fetch<T: DeserializeOwned>(&self, host: &str, path: &str) -> Result<Option<T>> {
        let key = format!("{host}/{path}");
        let etag = self
            .lock()
            .cache
            .get(&key)
            .map(|cached| cached.etag.clone());
        let body = match self.config.github.get(host, path, etag.as_deref()).await? {
            Fetched::Modified { etag, body } => {
                if let Some(etag) = etag {
                    let cached = Cached {
                        etag,
                        body: body.clone(),
                        used: Instant::now(),
                    };
                    self.lock().cache.insert(key, cached);
                }
                body
            }
            Fetched::NotModified => {
                let mut state = self.lock();
                let cached = state
                    .cache
                    .get_mut(&key)
                    .ok_or_else(|| anyhow!("{path} answered 304 to an unconditional request"))?;
                cached.used = Instant::now();
                cached.body.clone()
            }
            Fetched::NotFound => {
                self.lock().cache.remove(&key);
                return Ok(None);
            }
        };
        let value = serde_json::from_value(body).with_context(|| format!("decoding {path}"))?;
        Ok(Some(value))
    }

    /// Starts tracking the session in memory, once.
    async fn load(&self, session: &Session) -> Result<()> {
        let id = &session.session_id;
        if self.lock().sessions.contains_key(id) {
            return Ok(());
        }
        let created_at = self
            .journal
            .read_since(id.clone(), 0, 1)
            .await?
            .first()
            .map_or(session.updated_at, |event| event.at);
        let pushed = read_pushes(&self.pushes_file(id)).await;
        let slow = self.config.slow;
        self.lock().sessions.entry(id.clone()).or_insert(Tracked {
            created_at,
            unlinked: None,
            pushed,
            hot_until: None,
            next_due: Instant::now() + slow,
            follow_ups: None,
        });
        Ok(())
    }

    /// Pull requests a user unlinked from the session and has not linked again since, read
    /// from the journal the first time. Callers hold `links`.
    async fn unlinked(&self, session_id: &SessionId) -> Result<HashSet<u64>> {
        let loaded = self
            .lock()
            .sessions
            .get(session_id)
            .and_then(|tracked| tracked.unlinked.clone());
        if let Some(unlinked) = loaded {
            return Ok(unlinked);
        }
        let mut unlinked = HashSet::new();
        for event in self.journal.all(session_id.clone()).await? {
            match event.body {
                EventBody::PrUnlinked { number } => {
                    unlinked.insert(number);
                }
                EventBody::PrLinked { pr } => {
                    unlinked.remove(&pr.number);
                }
                _ => {}
            }
        }
        if let Some(tracked) = self.lock().sessions.get_mut(session_id) {
            tracked.unlinked = Some(unlinked.clone());
        }
        Ok(unlinked)
    }

    /// Runs `change` on the session's unlinked set, if it is loaded.
    fn change_unlinked(&self, session_id: &SessionId, change: impl FnOnce(&mut HashSet<u64>)) {
        let mut state = self.lock();
        if let Some(unlinked) = state
            .sessions
            .get_mut(session_id)
            .and_then(|tracked| tracked.unlinked.as_mut())
        {
            change(unlinked);
        }
    }

    /// Takes a hook's report.
    async fn report(&self, report: Report) -> Result<()> {
        let Report::PrePush {
            session_id,
            remote,
            url,
            branches,
        } = report;
        let session = self
            .journal
            .session(session_id.clone())
            .await?
            .ok_or_else(|| anyhow!("session {session_id} does not exist"))?;
        self.load(&session).await?;
        let configured = git(
            Path::new(&session.worktree),
            ["config", "--get", &format!("remote.{remote}.url")],
        )
        .await
        .ok();
        let Some(pushed) = GhRepo::from_url(configured.as_deref().unwrap_or(&url)) else {
            debug!(%session_id, url, "push to a remote not on GitHub");
            return Ok(());
        };
        let fast = self.config.fast;
        let pushes = {
            let mut state = self.lock();
            let Some(tracked) = state.sessions.get_mut(&session_id) else {
                return Ok(());
            };
            for branch in branches {
                let head = Head {
                    owner: pushed.owner.clone(),
                    branch,
                };
                if !tracked.pushed.contains(&head) {
                    tracked.pushed.push(head);
                }
            }
            let now = Instant::now();
            tracked.hot_until = Some(now + HOT);
            tracked.next_due = tracked.next_due.min(now + fast);
            tracked.pushed.clone()
        };
        write_pushes(&self.pushes_file(&session_id), &pushes).await?;
        self.wake.notify_one();
        Ok(())
    }

    async fn serve_hooks(self: Arc<Self>, listener: UnixListener, shutdown: CancellationToken) {
        loop {
            let accepted = tokio::select! {
                () = shutdown.cancelled() => break,
                accepted = listener.accept() => accepted,
            };
            match accepted {
                Ok((stream, _)) => {
                    let tracker = Arc::clone(&self);
                    tokio::spawn(async move { tracker.answer(stream).await });
                }
                Err(err) => {
                    warn!("cannot accept a hook connection: {err}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
        let _ = std::fs::remove_file(self.socket());
    }

    /// Reads one report from a hook and answers it.
    async fn answer(&self, stream: UnixStream) {
        let (read, mut write) = stream.into_split();
        let mut line = String::new();
        let mut reader = BufReader::new(read.take(MAX_REPORT));
        let reply = match tokio::time::timeout(REPORT_TIMEOUT, reader.read_line(&mut line)).await {
            Ok(Ok(_)) => match serde_json::from_str::<Report>(&line) {
                Ok(report) => match self.report(report).await {
                    Ok(()) => Reply { error: None },
                    Err(err) => Reply {
                        error: Some(format!("{err:#}")),
                    },
                },
                Err(err) => Reply {
                    error: Some(format!("malformed report: {err}")),
                },
            },
            Ok(Err(err)) => Reply {
                error: Some(format!("reading the report: {err}")),
            },
            Err(_) => return,
        };
        if let Ok(mut answer) = serde_json::to_string(&reply) {
            answer.push('\n');
            let _ = write.write_all(answer.as_bytes()).await;
        }
    }

    fn failed(&self, err: &anyhow::Error) {
        let message = format!("{err:#}");
        let mut state = self.lock();
        if state.last_error.as_ref() != Some(&message) {
            warn!("pull request polling failed: {message}");
            state.last_error = Some(message);
        }
    }

    fn hooks_dir(&self) -> PathBuf {
        self.config.data_dir.join("hooks")
    }

    fn socket(&self) -> PathBuf {
        self.config.data_dir.join("hooks.sock")
    }

    fn pushes_file(&self, session_id: &SessionId) -> PathBuf {
        self.config
            .data_dir
            .join("prs")
            .join(format!("{session_id}.json"))
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // Every update is a single insert, remove or field write, so a poisoned state is
        // still consistent.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Whether `pr` is still open, a draft or not.
pub(crate) fn open(pr: &PullRequest) -> bool {
    matches!(pr.state, PrState::Open | PrState::Draft)
}

/// Whether `pr` calls for a follow-up for `reason`.
fn meets(pr: &PullRequest, reason: FollowUpReason) -> bool {
    match reason {
        FollowUpReason::CiFailed => pr.ci == CiStatus::Failing,
        FollowUpReason::CiPassed => pr.ci == CiStatus::Passing,
        FollowUpReason::Conflicting => pr.mergeable == Mergeable::Conflicting,
        FollowUpReason::ChangesRequested => pr.review == ReviewStatus::ChangesRequested,
        FollowUpReason::Stalled | FollowUpReason::Unknown => false,
    }
}

/// The pull request among `prs` that still calls for `follow_up`, if one does.
fn due<'a>(follow_up: &FollowUp, prs: &'a [PullRequest]) -> Option<&'a PullRequest> {
    prs.iter().find(|pr| {
        Some(pr.number) == follow_up.pr
            && open(pr)
            && pr.head_sha == follow_up.head_sha
            && meets(pr, follow_up.reason)
    })
}

/// Whether a session whose journal is `events` and whose pull requests are `prs` is owed a
/// pull request follow-up it gets once idle.
pub(crate) fn owes_follow_up(events: &[Event], prs: &[PullRequest]) -> bool {
    replay(events)
        .pending
        .iter()
        .any(|follow_up| due(follow_up, prs).is_some())
}

/// The follow-ups `pr` is owed, having changed from `before`: each condition it meets now and
/// did not before. None for a pull request without a known head commit, as those tracked
/// before head commits were.
fn owed(before: &PullRequest, pr: &PullRequest) -> Vec<FollowUp> {
    if !open(pr) || pr.head_sha.is_none() {
        return Vec::new();
    }
    PR_REASONS
        .into_iter()
        .filter(|reason| meets(pr, *reason) && !meets(before, *reason))
        .map(|reason| FollowUp {
            reason,
            pr: Some(pr.number),
            head_sha: pr.head_sha.clone(),
        })
        .collect()
}

/// The follow-ups a session's journal says it is owed and has been sent.
fn replay(events: &[Event]) -> FollowUps {
    let mut follow_ups = FollowUps::default();
    let mut prs: HashMap<u64, PullRequest> = HashMap::new();
    for event in events {
        match &event.body {
            EventBody::PrLinked { pr } => {
                prs.insert(pr.number, pr.clone());
            }
            EventBody::PrUpdated { pr } => {
                if let Some(before) = prs.get_mut(&pr.number) {
                    follow_ups.changed(before, pr);
                    *before = pr.clone();
                }
            }
            EventBody::PrUnlinked { number } => {
                prs.remove(number);
            }
            EventBody::ItemAdded { item } => {
                if let (Some(follow_up), ItemBody::UserMessage { .. }) =
                    (&item.follow_up, &item.body)
                {
                    follow_ups.pending.retain(|pending| pending != follow_up);
                    follow_ups.sent.push(follow_up.clone());
                }
            }
            _ => {}
        }
    }
    follow_ups
}

/// The prompt of a follow-up for `reason` on `pr`, whose head commit's `failing` checks failed.
fn prompt(pr: &PullRequest, reason: FollowUpReason, failing: &[String]) -> String {
    let number = pr.number;
    match reason {
        FollowUpReason::CiFailed if failing.is_empty() => {
            format!("CI failed on #{number}. Find out why, fix it and push.")
        }
        FollowUpReason::CiFailed => format!(
            "CI failed on #{number}: {}. Find out why, fix it and push.",
            failing.join(", ")
        ),
        FollowUpReason::CiPassed => {
            let mut text = format!(
                "CI is green on #{number}. Finish it the way your instructions say, e.g. merge \
                 it and close the todo, or report that it is done."
            );
            match pr.unresolved_threads {
                Some(1) => text.push_str(" It has 1 unresolved review thread: resolve it first."),
                Some(threads) if threads > 1 => text.push_str(&format!(
                    " It has {threads} unresolved review threads: resolve them first."
                )),
                _ => {}
            }
            text
        }
        FollowUpReason::Conflicting => format!(
            "#{number} has merge conflicts with its base branch. Rebase or merge the base, \
             resolve them and push."
        ),
        FollowUpReason::ChangesRequested => {
            format!("A reviewer requested changes on #{number}. Address the review and push.")
        }
        FollowUpReason::Stalled | FollowUpReason::Unknown => String::new(),
    }
}

/// The GitHub repository pull requests of `repo` go to; see the module docs.
async fn base_repo(repo: &Path) -> Option<GhRepo> {
    let remotes = git(repo, ["remote"]).await.ok()?;
    let remotes: Vec<&str> = remotes.lines().collect();
    let mut chosen = None;
    for remote in &remotes {
        let resolved = git(
            repo,
            ["config", "--get", &format!("remote.{remote}.gh-resolved")],
        )
        .await;
        if resolved.is_ok_and(|resolved| resolved == "base") {
            chosen = Some(*remote);
        }
    }
    let chosen = chosen
        .or_else(|| {
            ["upstream", "github", "origin"]
                .into_iter()
                .find(|name| remotes.contains(name))
        })
        .or_else(|| (remotes.len() == 1).then(|| remotes[0]))?;
    let url = git(repo, ["config", "--get", &format!("remote.{chosen}.url")])
        .await
        .ok()?;
    GhRepo::from_url(&url)
}

/// The accounts of every remote of `repo` on `base`'s host, `base`'s owner first: where a
/// branch the session owns may have been pushed to.
async fn owners(repo: &Path, base: &GhRepo) -> Vec<String> {
    let mut owners = vec![base.owner.clone()];
    let urls = git(repo, ["config", "--get-regexp", r"^remote\..*\.url$"])
        .await
        .unwrap_or_default();
    for url in urls.lines().filter_map(|line| line.split_once(' ')) {
        if let Some(remote) = GhRepo::from_url(url.1)
            && remote.host == base.host
            && !owners.contains(&remote.owner)
        {
            owners.push(remote.owner);
        }
    }
    owners
}

async fn read_pushes(path: &Path) -> Vec<Head> {
    match tokio::fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
            warn!("ignoring {}: {err}", path.display());
            Vec::new()
        }),
        Err(_) => Vec::new(),
    }
}

async fn write_pushes(path: &Path, pushes: &[Head]) -> Result<()> {
    let bytes = serde_json::to_vec(pushes)?;
    let dir = path.parent().unwrap_or(path);
    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("creating {}", dir.display()))?;
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, bytes)
        .await
        .with_context(|| format!("writing {}", tmp.display()))?;
    tokio::fs::rename(&tmp, path)
        .await
        .with_context(|| format!("writing {}", path.display()))
}

fn error(code: ErrorCode, message: impl Into<String>) -> ErrorInfo {
    ErrorInfo {
        code,
        message: message.into(),
    }
}

fn internal(err: anyhow::Error) -> ErrorInfo {
    warn!("pull request command failed: {err:#}");
    error(ErrorCode::Internal, format!("{err:#}"))
}
