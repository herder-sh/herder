//! Cloning a repository onto the host, for `clone_project`. git authenticates as the host's
//! user does, with its SSH keys or credential helpers (`gh auth setup-git`, say); herder never
//! sees a credential.

use std::ffi::OsStr;
use std::path::Path;
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
    use super::git_url;

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
