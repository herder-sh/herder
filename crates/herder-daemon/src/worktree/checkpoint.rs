//! Code checkpoints: after every turn, the session's worktree as it is, committed under
//! `refs/herder/<session>/<turn>`.
//!
//! # The snapshot
//!
//! A checkpoint is a commit parented on the worktree's `HEAD` whose tree is every file
//! `git add -A` would stage: tracked files as they are on disk, plus untracked files that
//! `.gitignore` does not ignore. It is built in a temporary index seeded from the worktree's
//! own, so the user's index, `HEAD` and branches never change. Paths matching [`DENY`] are
//! left out even when tracked or not ignored, in any directory: `.env`, `.env.*`, `*.pem`,
//! `*.key`, `id_rsa*`, `.npmrc`, `.pypirc`, `credentials.json` and `*.p12`. Commits are made by
//! `herder <herder@localhost>`, unsigned, without the `Herder-Session` trailer: they are no
//! one's work on a branch.
//!
//! Every git command runs with `core.hooksPath=/dev/null`, so neither the user's hooks nor
//! the session's own (see [`crate::prs`]) see checkpoints: no `pre-push` report, no
//! `reference-transaction` or `post-index-change` hooks.
//!
//! # Refs
//!
//! Turn ids are ULIDs, so a session's refs sort in turn order; only its latest [`KEEP`] are
//! kept. The refs live in the repository, outside `refs/heads`, and outlive the worktree.
//!
//! # Publishing
//!
//! Once a checkpoint is made, the session's refs are pushed to `origin` in the background as
//! `refs/herder/<session>/*`, with `--prune` so the remote keeps the same latest [`KEEP`].
//! Nothing else is pushed and nothing is forced. When the repository has no `origin`, or the
//! push fails, the checkpoint is written as a git bundle to
//! `<data_dir>/checkpoints/<session>/<turn>.bundle` instead, leaving out commits already on a
//! remote-tracking branch; the latest [`KEEP`] bundles are kept.
//!
//! A host recovering the session after its host died fetches the pushed refs from `origin`
//! ([`fetch_latest`]); bundles stay on the host that wrote them.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use herder_protocol::{SessionId, TurnId};

use super::{Error, command};

/// Paths never checkpointed, in any directory, whether or not git ignores them: files that
/// usually hold secrets.
pub const DENY: &[&str] = &[
    ".env",
    ".env.*",
    "*.pem",
    "*.key",
    "id_rsa*",
    ".npmrc",
    ".pypirc",
    "credentials.json",
    "*.p12",
];

/// How many checkpoints, refs and bundles each, a session keeps.
pub const KEEP: usize = 20;

/// How long a push may take before the checkpoint is bundled instead.
pub const PUSH_TIMEOUT: Duration = Duration::from_secs(120);

/// Where checkpoints go and how many are kept.
#[derive(Clone, Debug)]
pub struct Config {
    /// `<data_dir>/checkpoints`: bundles and temporary indexes, one directory per session.
    pub dir: PathBuf,
    /// Checkpoints kept per session.
    pub keep: usize,
    /// How long a push may take.
    pub push_timeout: Duration,
}

/// Where a checkpoint was published.
#[derive(Debug, PartialEq, Eq)]
pub enum Published {
    /// Pushed to `origin`.
    Pushed,
    /// Written as a bundle at this path.
    Bundled(PathBuf),
}

/// The ref of `session_id`'s checkpoint after `turn_id`.
pub fn ref_name(session_id: &SessionId, turn_id: &TurnId) -> String {
    format!("refs/herder/{session_id}/{turn_id}")
}

/// Commits the worktree at `worktree` as `session_id`'s checkpoint after `turn_id` and prunes
/// the session's older ones; returns the checkpoint's ref.
pub async fn snapshot(
    config: &Config,
    worktree: &Path,
    session_id: &SessionId,
    turn_id: &TurnId,
) -> Result<String, Error> {
    let dir = config.dir.join(session_id.as_str());
    create_private_dir(&dir).await?;
    let index = dir.join(format!("index-{turn_id}"));
    let tree = write_tree(worktree, &index).await;
    // Best effort: a leftover index is overwritten next time.
    let _ = tokio::fs::remove_file(&index).await;
    let tree = tree?;
    let mut args = vec!["commit-tree".to_owned(), "--no-gpg-sign".to_owned(), tree];
    if let Ok(head) = git(
        worktree,
        ["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
    )
    .await
    {
        args.extend(["-p".to_owned(), head]);
    }
    args.extend([
        "-m".to_owned(),
        format!("herder checkpoint\n\nSession: {session_id}\nTurn: {turn_id}"),
    ]);
    let commit = run(
        worktree,
        &args,
        &[
            ("GIT_AUTHOR_NAME", OsStr::new("herder")),
            ("GIT_AUTHOR_EMAIL", OsStr::new("herder@localhost")),
            ("GIT_COMMITTER_NAME", OsStr::new("herder")),
            ("GIT_COMMITTER_EMAIL", OsStr::new("herder@localhost")),
        ],
    )
    .await?;
    let name = ref_name(session_id, turn_id);
    git(worktree, ["update-ref", &name, &commit]).await?;
    let refs = refs(worktree, session_id).await?;
    for old in &refs[..refs.len().saturating_sub(config.keep)] {
        git(worktree, ["update-ref", "-d", old]).await?;
    }
    Ok(name)
}

/// Pushes `session_id`'s checkpoints to `origin`, or bundles its checkpoint after `turn_id`
/// when it cannot.
pub async fn publish(
    config: &Config,
    worktree: &Path,
    session_id: &SessionId,
    turn_id: &TurnId,
) -> Result<Published, Error> {
    if git(worktree, ["remote", "get-url", "origin"]).await.is_ok() {
        let refspec = format!("refs/herder/{session_id}/*:refs/herder/{session_id}/*");
        let env = [("GIT_TERMINAL_PROMPT", OsStr::new("0"))];
        let push = run(
            worktree,
            [
                "push",
                "--no-verify",
                "--prune",
                "--quiet",
                "origin",
                &refspec,
            ],
            &env,
        );
        match tokio::time::timeout(config.push_timeout, push).await {
            Ok(Ok(_)) => return Ok(Published::Pushed),
            Ok(Err(err)) => {
                tracing::warn!(%session_id, "cannot push checkpoints, bundling: {err}");
            }
            Err(_) => tracing::warn!(%session_id, "pushing checkpoints timed out, bundling"),
        }
    }
    bundle(config, worktree, session_id, turn_id)
        .await
        .map(Published::Bundled)
}

/// Fetches `session_id`'s checkpoint refs from `repo`'s `origin`, as another host pushed them,
/// within `timeout`; returns the latest, or `None` when the repository has no `origin` or
/// `origin` has none. Bundles stay on the host that wrote them and are not looked for.
pub async fn fetch_latest(
    repo: &Path,
    session_id: &SessionId,
    timeout: Duration,
) -> Result<Option<String>, Error> {
    if git(repo, ["remote", "get-url", "origin"]).await.is_err() {
        return Ok(None);
    }
    let refspec = format!("+refs/herder/{session_id}/*:refs/herder/{session_id}/*");
    let env = [("GIT_TERMINAL_PROMPT", OsStr::new("0"))];
    let fetch = run(
        repo,
        ["fetch", "--quiet", "--no-tags", "origin", &refspec],
        &env,
    );
    tokio::time::timeout(timeout, fetch)
        .await
        .map_err(|_| Error::Git(format!("fetching from origin took over {timeout:?}")))??;
    Ok(refs(repo, session_id).await?.pop())
}

async fn bundle(
    config: &Config,
    worktree: &Path,
    session_id: &SessionId,
    turn_id: &TurnId,
) -> Result<PathBuf, Error> {
    let dir = config.dir.join(session_id.as_str());
    create_private_dir(&dir).await?;
    let path = dir.join(format!("{turn_id}.bundle"));
    git(
        worktree,
        [
            OsStr::new("bundle"),
            OsStr::new("create"),
            OsStr::new("--quiet"),
            path.as_os_str(),
            OsStr::new(&ref_name(session_id, turn_id)),
            OsStr::new("--not"),
            OsStr::new("--remotes"),
        ],
    )
    .await?;
    let read = dir.clone();
    let mut bundles = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<PathBuf>> {
        let mut bundles = Vec::new();
        for entry in std::fs::read_dir(read)? {
            let path = entry?.path();
            if path.extension() == Some(OsStr::new("bundle")) {
                bundles.push(path);
            }
        }
        Ok(bundles)
    })
    .await
    .map_err(|err| Error::Git(format!("listing bundles: {err}")))?
    .map_err(|err| Error::Git(format!("listing {}: {err}", dir.display())))?;
    bundles.sort();
    for old in &bundles[..bundles.len().saturating_sub(config.keep)] {
        // Best effort: the next bundle tries again.
        let _ = tokio::fs::remove_file(old).await;
    }
    Ok(path)
}

/// Stages the worktree in a fresh index at `index`, seeded from the worktree's own so
/// unchanged files are not hashed again, and writes its tree.
async fn write_tree(worktree: &Path, index: &Path) -> Result<String, Error> {
    let own = git(
        worktree,
        ["rev-parse", "--path-format=absolute", "--git-path", "index"],
    )
    .await?;
    match tokio::fs::copy(&own, index).await {
        Ok(_) => {}
        // An empty repository has no index yet.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(Error::Git(format!("copying {own}: {err}"))),
    }
    let env = [("GIT_INDEX_FILE", index.as_os_str())];
    run(worktree, ["add", "--all", "--", "."], &env).await?;
    let mut rm = vec![
        "rm".to_owned(),
        "--cached".to_owned(),
        "-r".to_owned(),
        "-f".to_owned(),
        "--quiet".to_owned(),
        "--ignore-unmatch".to_owned(),
        "--".to_owned(),
    ];
    rm.extend(
        DENY.iter()
            .map(|pattern| format!(":(top,glob)**/{pattern}")),
    );
    run(worktree, &rm, &env).await?;
    run(worktree, ["write-tree"], &env).await
}

/// `session_id`'s checkpoint refs, oldest first.
async fn refs(worktree: &Path, session_id: &SessionId) -> Result<Vec<String>, Error> {
    let listed = git(
        worktree,
        [
            "for-each-ref",
            "--sort=refname",
            "--format=%(refname)",
            &format!("refs/herder/{session_id}/"),
        ],
    )
    .await?;
    Ok(listed.lines().map(str::to_owned).collect())
}

async fn create_private_dir(dir: &Path) -> Result<(), Error> {
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(dir)
        .await
        .map_err(|err| Error::Git(format!("creating {}: {err}", dir.display())))
}

async fn git<I, S>(dir: &Path, args: I) -> Result<String, Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run(dir, args, &[]).await
}

/// Runs git in `dir` with `env` and no hooks, returning its trimmed stdout, or its stderr as
/// the error.
async fn run<I, S>(dir: &Path, args: I, env: &[(&str, &OsStr)]) -> Result<String, Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let hookless = [OsStr::new("-c"), OsStr::new("core.hooksPath=/dev/null")];
    let args: Vec<_> = args.into_iter().collect();
    let mut command = command(
        dir,
        hookless.into_iter().chain(args.iter().map(AsRef::as_ref)),
    );
    command.envs(env.iter().copied());
    let output = command
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

#[cfg(test)]
mod tests;
