//! Cloning a repository onto the host, for `clone_project`. git authenticates as the host's
//! user does, with its SSH keys or credential helpers (`gh auth setup-git`, say); herder never
//! sees a credential.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use herder_protocol::{ErrorCode, ErrorInfo};

use crate::skills::{redact, redact_in};

/// How long a clone may take before it is given up.
const TIMEOUT: Duration = Duration::from_secs(600);

/// Clones `url` into `into`, a folder that must not exist yet. A clone that fails leaves
/// nothing behind.
pub(crate) async fn clone(url: &str, into: &Path) -> Result<(), ErrorInfo> {
    let url = git_url(url.trim());
    if url.is_empty() {
        return Err(error(
            ErrorCode::BadRequest,
            "the repository needs a git URL",
        ));
    }
    if into.symlink_metadata().is_ok() {
        return Err(error(
            ErrorCode::Conflict,
            format!("{} exists already", into.display()),
        ));
    }
    let Some(parent) = into.parent() else {
        return Err(error(
            ErrorCode::BadRequest,
            format!("cannot clone into {}", into.display()),
        ));
    };
    tokio::fs::create_dir_all(parent).await.map_err(|err| {
        error(
            ErrorCode::BadRequest,
            format!("cannot create {}: {err}", parent.display()),
        )
    })?;
    let run = crate::worktree::command(
        parent,
        [
            OsStr::new("clone"),
            OsStr::new("--quiet"),
            OsStr::new("--"),
            OsStr::new(&url),
            into.as_os_str(),
        ],
    )
    // Fail rather than wait on a password prompt nobody sees.
    .env("GIT_TERMINAL_PROMPT", "0")
    .output();
    let failed = match tokio::time::timeout(TIMEOUT, run).await {
        Ok(Ok(output)) if output.status.success() => return Ok(()),
        Ok(Ok(output)) => String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        Ok(Err(err)) => format!("running git: {err}"),
        Err(_) => format!("git did not finish within {} s", TIMEOUT.as_secs()),
    };
    if let Err(err) = tokio::fs::remove_dir_all(into).await
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!("cannot remove the failed clone {}: {err}", into.display());
    }
    Err(error(
        ErrorCode::BadRequest,
        format!(
            "cannot clone {}: {}",
            redact(&url),
            redact_in(&failed, &url)
        ),
    ))
}

/// The folder in `dir` a clone of `url` goes into when none is given: one named after the
/// repository, as `~/Projects/herder` for `git@github.com:herder-sh/herder.git`. `None` when
/// the URL names no repository, or one whose name would leave `dir` or hide the clone.
pub(crate) fn folder(dir: &Path, url: &str) -> Option<PathBuf> {
    let url = git_url(url.trim());
    let name = url
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .map(|name| name.strip_suffix(".git").unwrap_or(name))
        .filter(|name| !name.is_empty() && !name.starts_with('.'))?;
    Some(dir.join(name))
}

/// `url` without the password a `scheme://user:password@host/...` URL may carry, which is
/// never listed nor sent to another host.
pub(crate) fn without_password(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let end = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    match authority.rsplit_once('@') {
        Some((user, host)) => {
            let user = user.split_once(':').map_or(user, |(user, _)| user);
            format!("{scheme}://{user}@{host}{path}")
        }
        None => url.to_owned(),
    }
}

/// The git URL `url` names: GitHub's for `owner/repo`, else `url` itself.
fn git_url(url: &str) -> String {
    let shorthand = url.split_once('/').filter(|(owner, repo)| {
        let name = |part: &str| {
            !part.is_empty()
                && !part.starts_with('.')
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        };
        name(owner) && name(repo)
    });
    match shorthand {
        Some((owner, repo)) => {
            let repo = repo.strip_suffix(".git").unwrap_or(repo);
            format!("https://github.com/{owner}/{repo}.git")
        }
        None => url.to_owned(),
    }
}

fn error(code: ErrorCode, message: impl Into<String>) -> ErrorInfo {
    ErrorInfo {
        code,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{folder, git_url, without_password};

    #[test]
    fn a_clone_goes_into_the_projects_dir_under_the_repositorys_name() {
        let dir = Path::new("/home/dev/Projects");
        for url in [
            "git@github.com:herder-sh/herder.git",
            "https://github.com/herder-sh/herder/",
            "herder-sh/herder",
            " ssh://git@host:22/team/herder.git ",
        ] {
            assert_eq!(folder(dir, url), Some(dir.join("herder")), "{url}");
        }
        for url in [
            "",
            "https://github.com/org/..",
            "git@github.com:org/.hidden",
        ] {
            assert_eq!(folder(dir, url), None, "{url}");
        }
    }

    #[test]
    fn listed_remotes_keep_their_user_but_never_a_password() {
        for (url, listed) in [
            (
                "https://me:ghp_secret@github.com/org/repo.git",
                "https://me@github.com/org/repo.git",
            ),
            (
                "ssh://git@github.com:22/org/repo",
                "ssh://git@github.com:22/org/repo",
            ),
            ("git@github.com:org/repo.git", "git@github.com:org/repo.git"),
            ("https://github.com/org/repo", "https://github.com/org/repo"),
        ] {
            assert_eq!(without_password(url), listed);
        }
    }

    #[test]
    fn owner_slash_repo_names_a_github_repository() {
        assert_eq!(
            git_url("herder-sh/herder"),
            "https://github.com/herder-sh/herder.git"
        );
        assert_eq!(
            git_url("herder-sh/herder.git"),
            "https://github.com/herder-sh/herder.git"
        );
        for url in [
            "git@github.com:herder-sh/herder.git",
            "https://github.com/herder-sh/herder",
            "/srv/git/app",
            "../app",
            "a/b/c",
        ] {
            assert_eq!(git_url(url), url);
        }
    }
}
