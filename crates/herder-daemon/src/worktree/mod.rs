//! Session worktrees: every session works in its own `git worktree`, on a branch it owns.
//!
//! Everything goes through the `git` CLI, so the user's git config and hooks apply as they do
//! in their own checkouts.
//!
//! # Layout and naming
//!
//! A session's worktree is `<data_dir>/worktrees/<repo-name>-<slug>`, where `<repo-name>` is
//! the name of the repository's top-level directory and `<slug>` is the session's short id
//! (see [`slug`]): the task label and first prompt do not exist yet when a session is created.
//! The branch is the one the `create_session` command names, or `herder/<slug>` when it names
//! none.
//!
//! # Base
//!
//! The branch starts at the repository's default branch: the local branch `origin/HEAD` names
//! when it exists, else `origin/HEAD` itself, else the repository's `HEAD` when it has no
//! `origin/HEAD`. Nothing is fetched. The branch never tracks the base, so a plain `git push`
//! cannot land on the default branch.
//!
//! # Branches checked out
//!
//! [`branches`] lists every branch the worktree has had checked out, read from the worktree's
//! own `HEAD` reflog, which git writes on every checkout, switch and rename, whoever ran it
//! and whenever. It lives in the worktree's admin dir, so it survives daemon restarts and goes
//! away with the worktree; the session manager journals each new branch as `branch_checked_out`
//! at turn end and before removal, so the session keeps it.
//!
//! # Removal
//!
//! [`Worktrees::remove`] removes the worktree and keeps its branches, refusing while the
//! worktree has uncommitted or untracked changes unless forced.

pub mod checkpoint;

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use herder_protocol::SessionId;
use tokio::process::Command;

/// Why a worktree operation failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The request cannot work on this repository as given.
    #[error("{0}")]
    BadRequest(String),
    /// The repository's state is in the way.
    #[error("{0}")]
    Conflict(String),
    /// git failed, or could not be run.
    #[error("{0}")]
    Git(String),
}

/// A session's worktree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    /// Absolute path of the worktree.
    pub path: PathBuf,
    /// Branch created for the session and checked out in the worktree.
    pub branch: String,
}

/// The session worktrees under one directory, `<data_dir>/worktrees`.
#[derive(Clone, Debug)]
pub struct Worktrees {
    root: PathBuf,
}

/// The short form of a session id used in worktree and branch names: the last 8 characters of
/// its ULID, lowercased. They are random, where the leading ones are a timestamp that sessions
/// created close together share.
pub fn slug(session_id: &SessionId) -> String {
    let id = session_id.as_str();
    let start = id.char_indices().rev().nth(7).map_or(0, |(at, _)| at);
    id[start..].to_lowercase()
}

impl Worktrees {
    /// Worktrees under `root`, created when the first one is.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Adds a worktree of `repo` for the session `slug`, on a new branch: `branch`, or
    /// `herder/<slug>` when absent.
    pub async fn create(
        &self,
        repo: &Path,
        slug: &str,
        branch: Option<String>,
    ) -> Result<Worktree, Error> {
        self.add(repo, slug, branch, None).await
    }

    /// Adds a worktree of `repo` for the session `slug`, forked from another session, on a new
    /// branch `branch`: at the parent of the commit `checkpoint` (see [`checkpoint`]), with the
    /// checkpoint's files on disk as uncommitted changes, so the worktree is as the other
    /// session left it. Without a checkpoint it starts at the default base, as [`Self::create`] does.
    pub async fn restore(
        &self,
        repo: &Path,
        slug: &str,
        branch: String,
        checkpoint: Option<&str>,
    ) -> Result<Worktree, Error> {
        let Some(checkpoint) = checkpoint else {
            return self.add(repo, slug, Some(branch), None).await;
        };
        let parent = format!("{checkpoint}^");
        let base = git(repo, ["rev-parse", "--verify", "--quiet", &parent])
            .await
            .ok();
        let worktree = self.add(repo, slug, Some(branch), base).await?;
        // No-overlay: files the checkpoint does not have are removed too.
        git(
            &worktree.path,
            [
                "restore",
                "--no-overlay",
                &format!("--source={checkpoint}"),
                "--worktree",
                "--",
                ".",
            ],
        )
        .await?;
        Ok(worktree)
    }

    /// Adds the worktree on a new branch starting at `base`, or the default base.
    async fn add(
        &self,
        repo: &Path,
        slug: &str,
        branch: Option<String>,
        base: Option<String>,
    ) -> Result<Worktree, Error> {
        if !repo.is_absolute() || !repo.is_dir() {
            return Err(Error::BadRequest(format!(
                "{} is not an absolute path to a directory",
                repo.display()
            )));
        }
        let toplevel = git(repo, ["rev-parse", "--show-toplevel"])
            .await
            .map_err(|_| {
                Error::BadRequest(format!("{} is not a git repository", repo.display()))
            })?;
        let name = Path::new(&toplevel)
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("repo");
        let branch = branch.unwrap_or_else(|| format!("herder/{slug}"));
        if git(repo, ["check-ref-format", "--branch", &branch])
            .await
            .is_err()
        {
            return Err(Error::BadRequest(format!(
                "{branch} is not a valid branch name"
            )));
        }
        if is_branch(repo, &branch).await? {
            return Err(Error::Conflict(format!("branch {branch} already exists")));
        }
        let base = match base {
            Some(base) => base,
            None => default_base(repo).await?,
        };
        // One mkdir; not worth a blocking-pool hop.
        std::fs::create_dir_all(&self.root)
            .map_err(|err| Error::Git(format!("creating {}: {err}", self.root.display())))?;
        let path = self.root.join(format!("{name}-{slug}"));
        if path.exists() {
            return Err(Error::Conflict(format!(
                "{} already exists",
                path.display()
            )));
        }
        git(
            repo,
            [
                OsStr::new("worktree"),
                OsStr::new("add"),
                OsStr::new("--no-track"),
                OsStr::new("-b"),
                OsStr::new(&branch),
                path.as_os_str(),
                OsStr::new(&base),
            ],
        )
        .await?;
        Ok(Worktree { path, branch })
    }

    /// Adds a worktree of `repo` back at `path`, where [`Self::remove`] removed it, on the
    /// existing branch `branch`. Refuses when the branch is gone or the path is taken.
    pub async fn reopen(&self, repo: &Path, path: &Path, branch: &str) -> Result<(), Error> {
        if !is_branch(repo, branch).await? {
            return Err(Error::Conflict(format!(
                "branch {branch} no longer exists in {}",
                repo.display()
            )));
        }
        if path.exists() {
            return Err(Error::Conflict(format!(
                "{} already exists",
                path.display()
            )));
        }
        if let Some(parent) = path.parent() {
            // One mkdir; not worth a blocking-pool hop.
            std::fs::create_dir_all(parent)
                .map_err(|err| Error::Git(format!("creating {}: {err}", parent.display())))?;
        }
        // Forgets the removed worktree, should git still list it.
        git(repo, ["worktree", "prune"]).await?;
        git(
            repo,
            [
                OsStr::new("worktree"),
                OsStr::new("add"),
                path.as_os_str(),
                OsStr::new(branch),
            ],
        )
        .await?;
        Ok(())
    }

    /// Removes the worktree at `path` of `repo`, keeping every branch. Refuses while it has
    /// uncommitted or untracked changes, unless `force`. A worktree that is already gone is
    /// pruned; a path outside this directory, such as the repository itself, is left alone.
    pub async fn remove(&self, repo: &Path, path: &Path, force: bool) -> Result<(), Error> {
        if path == self.root || !path.starts_with(&self.root) {
            return Ok(());
        }
        if !path.exists() {
            git(repo, ["worktree", "prune"]).await?;
            return Ok(());
        }
        if !force {
            let changes = git(path, ["status", "--porcelain"]).await?;
            if !changes.is_empty() {
                return Err(Error::Conflict(format!(
                    "{} has uncommitted or untracked changes; commit or discard them, or force",
                    path.display()
                )));
            }
        }
        // `--force` once the check passed or the user asked: it also removes ignored files.
        git(
            repo,
            [
                OsStr::new("worktree"),
                OsStr::new("remove"),
                OsStr::new("--force"),
                path.as_os_str(),
            ],
        )
        .await?;
        Ok(())
    }
}

/// Every branch the worktree at `path` has had checked out, in the order first checked out,
/// starting with `own`, the branch it was created on. Detached checkouts (of a commit, tag or
/// remote-tracking branch) are not branches and are left out; branches deleted since are kept.
/// Only `own` once the worktree is gone.
pub async fn branches(path: &Path, own: &str) -> Result<Vec<String>, Error> {
    let mut seen = vec![own.to_owned()];
    if !path.exists() {
        return Ok(seen);
    }
    // Newest first; `%gs` is the reflog subject.
    let reflog = git(path, ["reflog", "show", "--format=%gs", "HEAD"]).await?;
    let mut targets: Vec<&str> = reflog.lines().rev().filter_map(checked_out).collect();
    let current = git(path, ["branch", "--show-current"]).await?;
    targets.push(&current);
    let local = git(
        path,
        ["for-each-ref", "--format=%(refname:short)", "refs/heads/"],
    )
    .await?;
    let local: HashSet<&str> = local.lines().collect();
    for target in targets {
        if target.is_empty() || seen.iter().any(|branch| branch == target) {
            continue;
        }
        // Not a branch now: a deleted branch, unless it names a commit (a detached checkout).
        let is_branch = local.contains(target)
            || git(
                path,
                [
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "--end-of-options",
                    &format!("{target}^{{commit}}"),
                ],
            )
            .await
            .is_err();
        if is_branch {
            seen.push(target.to_owned());
        }
    }
    Ok(seen)
}

/// The name a reflog subject checked out, if it records a checkout or a rename of `HEAD`'s
/// branch.
fn checked_out(subject: &str) -> Option<&str> {
    if let Some(moved) = subject.strip_prefix("checkout: moving from ") {
        // Ref names cannot hold spaces, so the last " to " separates the two.
        return moved.rsplit_once(" to ").map(|(_, to)| to);
    }
    let renamed = subject.strip_prefix("Branch: renamed ")?;
    renamed.rsplit_once(" to ")?.1.strip_prefix("refs/heads/")
}

/// The commit-ish a session branch starts at; see the module docs.
async fn default_base(repo: &Path) -> Result<String, Error> {
    let Ok(remote) = git(
        repo,
        [
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .await
    else {
        return Ok("HEAD".to_owned());
    };
    if let Some(local) = remote.strip_prefix("origin/")
        && is_branch(repo, local).await?
    {
        return Ok(local.to_owned());
    }
    Ok(remote)
}

async fn is_branch(repo: &Path, name: &str) -> Result<bool, Error> {
    let status = command(
        repo,
        [
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{name}"),
        ],
    )
    .status()
    .await
    .map_err(|err| Error::Git(format!("running git: {err}")))?;
    Ok(status.success())
}

/// Runs git in `dir`, returning its trimmed stdout, or its stderr as the error.
pub(crate) async fn git<I, S>(dir: &Path, args: I) -> Result<String, Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = command(dir, args)
        .output()
        .await
        .map_err(|err| Error::Git(format!("running git: {err}")))?;
    if !output.status.success() {
        return Err(Error::Git(format!(
            "git failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn command<I, S>(dir: &Path, args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    // Set when the daemon itself runs under a git hook; they would point git at that repo.
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
    ] {
        command.env_remove(var);
    }
    command
}
