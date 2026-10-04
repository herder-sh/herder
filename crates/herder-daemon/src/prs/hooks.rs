//! The git hooks herder installs in session worktrees, and the client side of them that runs
//! as `herder hook <name>`.
//!
//! # What changes in the repository
//!
//! Worktrees share the repository's hooks dir, so herder never writes there. Instead each
//! session gets its own hooks dir, `<data_dir>/hooks/<session>`, which git uses through:
//!
//! - `core.hooksPath = <data_dir>/hooks/<session>` in the session worktree's
//!   `config.worktree`, which goes away with the worktree. This needs
//!   `extensions.worktreeConfig = true` in the repository's config; when it is not on yet,
//!   herder turns it on and records that with `herder.worktreeConfig = true`. Both are removed
//!   again when the last session worktree is archived and no worktree has a `config.worktree`
//!   left.
//! - the same `core.hooksPath` in the environment of the session's CLI, as
//!   `GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_<n>` and `GIT_CONFIG_VALUE_<n>` (see [`add_to_env`]).
//!   Everything the agent runs inherits it, so its git commands use the session's hooks in
//!   whichever worktree they run: also in worktrees the agent adds itself, which git creates
//!   without the session worktree's config. Claude Code's `Agent` tool, for one, adds them
//!   under the main worktree's `.claude/worktrees/`.
//!
//! A repository whose shared config sets `core.worktree` is left alone: turning the extension
//! on would change how git reads it. Its sessions get no hooks.
//!
//! # The hooks
//!
//! Every client-side hook in a session's hooks dir first runs the repository's own hook of
//! that name, from the hooks dir it would use without herder's (the `core.hooksPath` of any
//! scope but the worktree's and the environment's, else the repository's `hooks` dir), and
//! fails when it fails. In the session's repository, three also report to herder; in any other
//! repository the agent works in, they only run its own hooks:
//!
//! - `pre-push` sends the pushed branches to the daemon over the Unix socket
//!   `<data_dir>/hooks.sock`. It never fails a push: an unreachable daemon is a warning.
//! - `prepare-commit-msg` and `commit-msg` add a `Herder-Session: <session id>` trailer to the
//!   message, unless it is empty. `prepare-commit-msg` runs even under `--no-verify`;
//!   `commit-msg` catches a message written in the editor.
//!
//! The scripts are rewritten every time the daemon starts, picking up the current binary.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use herder_protocol::SessionId;
use serde::{Deserialize, Serialize};

use super::git;

/// Name of the commit trailer naming the session that made a commit.
pub const TRAILER: &str = "Herder-Session";

/// How long a hook waits for the daemon before giving up.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(3);

/// Client-side hooks: each one in a session's hooks dir chains to the repository's own.
const HOOKS: &[&str] = &[
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-auto-gc",
    "post-rewrite",
    "sendemail-validate",
    "reference-transaction",
    "post-index-change",
];

/// What a hook tells the daemon, one JSON line per connection; answered with a [`Reply`].
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "hook", rename_all = "kebab-case")]
pub(crate) enum Report {
    /// Branches about to be pushed from a session worktree.
    PrePush {
        session_id: SessionId,
        /// The remote's name, or its URL when pushed to a URL.
        remote: String,
        /// The URL pushed to.
        url: String,
        /// Remote branches created or updated, as short names.
        branches: Vec<String>,
    },
}

/// The daemon's answer to a [`Report`].
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Reply {
    /// Why the report was refused; absent when it was taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

/// Installs `session_id`'s hooks in its worktree at `worktree`, calling `herder` from them and
/// reporting to `socket`. Safe to repeat.
pub(crate) async fn install(
    hooks_dir: &Path,
    socket: &Path,
    herder: &Path,
    session_id: &SessionId,
    worktree: &Path,
) -> Result<()> {
    if git(
        worktree,
        ["config", "--bool", "--get", "extensions.worktreeConfig"],
    )
    .await
    .ok()
    .as_deref()
        != Some("true")
    {
        if let Ok(core_worktree) = git(worktree, ["config", "--get", "core.worktree"]).await {
            bail!("the repository sets core.worktree = {core_worktree}; not installing hooks");
        }
        git(worktree, ["config", "extensions.worktreeConfig", "true"]).await?;
        git(worktree, ["config", "herder.worktreeConfig", "true"]).await?;
    }
    let common = git(
        worktree,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await?;
    let dir = hooks_dir.join(session_id.as_str());
    let scripts = HOOKS
        .iter()
        .map(|name| (*name, script(name, &common, herder, socket, session_id)))
        .collect::<Vec<_>>();
    let written = dir.clone();
    tokio::task::spawn_blocking(move || write_scripts(&written, &scripts))
        .await
        .context("the hook writer panicked")??;
    git(
        worktree,
        [
            "config".as_ref(),
            "--worktree".as_ref(),
            "core.hooksPath".as_ref(),
            dir.as_os_str(),
        ],
    )
    .await?;
    Ok(())
}

/// Removes `session_id`'s hooks: its hooks dir, its worktree's `core.hooksPath` if the
/// worktree is still there, and the repository's `extensions.worktreeConfig` once herder
/// turned it on and no worktree uses per-worktree config any more.
pub(crate) async fn uninstall(
    hooks_dir: &Path,
    session_id: &SessionId,
    repo: &Path,
    worktree: &Path,
) -> Result<()> {
    if worktree.exists() {
        // Fails when it is not set, which is fine.
        let _ = git(
            worktree,
            ["config", "--worktree", "--unset", "core.hooksPath"],
        )
        .await;
    }
    let dir = hooks_dir.join(session_id.as_str());
    match tokio::fs::remove_dir_all(&dir).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err).with_context(|| format!("removing {}", dir.display())),
    }
    if !repo.exists()
        || git(repo, ["config", "--bool", "--get", "herder.worktreeConfig"])
            .await
            .ok()
            .as_deref()
            != Some("true")
    {
        return Ok(());
    }
    let common = PathBuf::from(
        git(
            repo,
            ["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .await?,
    );
    if worktree_configs_left(&common)? {
        return Ok(());
    }
    git(repo, ["config", "--unset", "extensions.worktreeConfig"]).await?;
    git(repo, ["config", "--unset", "herder.worktreeConfig"]).await?;
    if git(repo, ["config", "--get-regexp", "^herder\\."])
        .await
        .is_err()
    {
        let _ = git(repo, ["config", "--remove-section", "herder"]).await;
    }
    Ok(())
}

/// Whether the main worktree or any linked one still has a `config.worktree`.
fn worktree_configs_left(common: &Path) -> Result<bool> {
    if common.join("config.worktree").exists() {
        return Ok(true);
    }
    let linked = match fs::read_dir(common.join("worktrees")) {
        Ok(linked) => linked,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err).context("listing worktrees"),
    };
    for entry in linked {
        if entry?.path().join("config.worktree").exists() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn write_scripts(dir: &Path, scripts: &[(&str, String)]) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    for (name, script) in scripts {
        let path = dir.join(name);
        let tmp = dir.join(format!(".{name}.tmp"));
        fs::write(&tmp, script)
            .and_then(|()| fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)))
            .and_then(|()| fs::rename(&tmp, &path))
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

/// The script of hook `name` for `session_id`, whose repository's common dir is `common`: run
/// the repository's own hook, then, in the session's repository, report what herder needs.
fn script(
    name: &str,
    common: &str,
    herder: &Path,
    socket: &Path,
    session_id: &SessionId,
) -> String {
    let common = quote(common);
    let herder = quote(&herder.to_string_lossy());
    let session = quote(session_id.as_str());
    // A relative `core.hooksPath` stays relative: git runs hooks from the worktree's top level,
    // where it resolves as git would resolve it.
    let header = format!(
        "#!/bin/sh\n# Installed by herder for session {session_id}. Runs this repository's own\n\
         # {name} hook, then, in the session's repository, reports to the herder daemon.\n\
         own=$(git config --type=path --show-scope --get-all core.hooksPath 2>/dev/null |\n\
         \x20 grep -v -e '^worktree' -e '^command' | tail -n 1 | cut -f 2-)\n\
         own=\"${{own:-$(git rev-parse --path-format=absolute --git-common-dir)/hooks}}/{name}\"\n\
         session_repo() {{ [ \"$(git rev-parse --path-format=absolute --git-common-dir)\" = {common} ]; }}\n"
    );
    match name {
        "pre-push" => {
            let socket = quote(&socket.to_string_lossy());
            format!(
                "{header}input=$(cat)\n\
                 if [ -x \"$own\" ]; then\n\
                 \x20 if [ -n \"$input\" ]; then printf '%s\\n' \"$input\"; fi | \"$own\" \"$@\" || exit $?\n\
                 fi\n\
                 session_repo || exit 0\n\
                 if [ -n \"$input\" ]; then printf '%s\\n' \"$input\"; fi |\n\
                 \x20 {herder} hook pre-push --socket {socket} --session {session} \"$@\" || true\n"
            )
        }
        "prepare-commit-msg" | "commit-msg" => format!(
            "{header}if [ -x \"$own\" ]; then \"$own\" \"$@\" || exit $?; fi\n\
             session_repo || exit 0\n\
             {herder} hook {name} --session {session} \"$1\" || true\n"
        ),
        _ => format!("{header}if [ -x \"$own\" ]; then exec \"$own\" \"$@\"; fi\n"),
    }
}

/// Adds `core.hooksPath = <hooks_dir>/<session>` to `env`, the environment of the session's
/// CLI, after any config entries it carries already. Leaves `env` alone when the session has
/// no hooks, so the repository's own hooks keep running.
pub(crate) fn add_to_env(
    env: &mut BTreeMap<String, String>,
    hooks_dir: &Path,
    session_id: &SessionId,
) {
    let dir = hooks_dir.join(session_id.as_str());
    if !dir.is_dir() {
        return;
    }
    let count: usize = env
        .get("GIT_CONFIG_COUNT")
        .and_then(|count| count.parse().ok())
        .unwrap_or(0);
    env.insert(
        format!("GIT_CONFIG_KEY_{count}"),
        "core.hooksPath".to_owned(),
    );
    env.insert(
        format!("GIT_CONFIG_VALUE_{count}"),
        dir.to_string_lossy().into_owned(),
    );
    env.insert("GIT_CONFIG_COUNT".to_owned(), (count + 1).to_string());
}

/// Single-quotes `value` for `sh`.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// `herder hook pre-push`: reads git's pre-push lines from `input` and reports the branches
/// being created or updated on the remote to the daemon at `socket`.
pub fn pre_push(
    socket: &Path,
    session_id: SessionId,
    remote: String,
    url: String,
    input: impl Read,
) -> Result<()> {
    let mut branches = Vec::new();
    for line in BufReader::new(input).lines() {
        let line = line.context("reading the refs being pushed")?;
        // <local ref> <local sha> <remote ref> <remote sha>
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [_, local_sha, remote_ref, _] = fields[..] else {
            continue;
        };
        let deleted = local_sha.bytes().all(|b| b == b'0');
        if let Some(branch) = remote_ref.strip_prefix("refs/heads/")
            && !deleted
            && !branches.iter().any(|known| known == branch)
        {
            branches.push(branch.to_owned());
        }
    }
    if branches.is_empty() {
        return Ok(());
    }
    let report = Report::PrePush {
        session_id,
        remote,
        url,
        branches,
    };
    let reply = send(socket, &report).with_context(|| {
        format!(
            "cannot report the push to the herder daemon at {}",
            socket.display()
        )
    })?;
    match reply.error {
        Some(error) => Err(anyhow!(
            "the herder daemon refused the push report: {error}"
        )),
        None => Ok(()),
    }
}

fn send(socket: &Path, report: &Report) -> Result<Reply> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(CLIENT_TIMEOUT))?;
    stream.set_write_timeout(Some(CLIENT_TIMEOUT))?;
    let mut line = serde_json::to_string(report)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer)?;
    serde_json::from_str(&answer).context("decoding the daemon's reply")
}

/// `herder hook prepare-commit-msg` and `herder hook commit-msg`: adds the session's trailer to
/// the message in `file`, unless the message is empty (so git still aborts an empty commit) or
/// already carries it.
pub fn add_trailer(session_id: &SessionId, file: &Path) -> Result<()> {
    let message =
        fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let comment = comment_prefix();
    let has_content = message
        .lines()
        .take_while(|line| !line.starts_with(&format!("{comment} ------------------------ >8")))
        .any(|line| !line.trim().is_empty() && !line.starts_with(&comment));
    if !has_content {
        return Ok(());
    }
    let output = std::process::Command::new("git")
        .args([
            "interpret-trailers",
            "--in-place",
            "--if-exists",
            "addIfDifferent",
        ])
        .arg("--trailer")
        .arg(format!("{TRAILER}: {session_id}"))
        .arg(file)
        .output()
        .context("running git interpret-trailers")?;
    if !output.status.success() {
        bail!(
            "git interpret-trailers failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// The prefix of comment lines in commit messages: `core.commentString`, else
/// `core.commentChar`, else `#`.
fn comment_prefix() -> String {
    for key in ["core.commentString", "core.commentChar"] {
        let configured = std::process::Command::new("git")
            .args(["config", "--get", key])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
        match configured.as_deref() {
            None | Some("") | Some("auto") => continue,
            Some(prefix) => return prefix.to_owned(),
        }
    }
    "#".to_owned()
}

/// The session ids named by `Herder-Session` trailers in a commit message.
pub(crate) fn trailer_sessions(message: &str) -> impl Iterator<Item = &str> {
    message.lines().filter_map(|line| {
        let (key, value) = line.split_once(':')?;
        let value = value.trim();
        (key.trim().eq_ignore_ascii_case(TRAILER) && !value.is_empty()).then_some(value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailers_name_sessions() {
        let message = "Fix it\n\nBody text.\n\nHerder-Session: 01ABC\nCo-Authored-By: x\n";
        assert_eq!(trailer_sessions(message).collect::<Vec<_>>(), ["01ABC"]);
        assert_eq!(trailer_sessions("herder-session:  01X  ").count(), 1);
        assert_eq!(trailer_sessions("Herder-Session:").count(), 0);
    }

    #[test]
    fn the_hooks_path_goes_after_the_config_already_in_the_environment() {
        let hooks = tempfile::tempdir().unwrap();
        let session = SessionId::new("01ABC");
        let mut env = BTreeMap::from([
            ("GIT_CONFIG_COUNT".to_owned(), "1".to_owned()),
            ("GIT_CONFIG_KEY_0".to_owned(), "user.name".to_owned()),
            ("GIT_CONFIG_VALUE_0".to_owned(), "me".to_owned()),
        ]);
        let unchanged = env.clone();
        // No hooks installed: the repository's own hooks must keep running.
        add_to_env(&mut env, hooks.path(), &session);
        assert_eq!(env, unchanged);

        fs::create_dir(hooks.path().join("01ABC")).unwrap();
        add_to_env(&mut env, hooks.path(), &session);
        assert_eq!(env["GIT_CONFIG_COUNT"], "2");
        assert_eq!(env["GIT_CONFIG_KEY_0"], "user.name");
        assert_eq!(env["GIT_CONFIG_KEY_1"], "core.hooksPath");
        assert_eq!(
            Path::new(&env["GIT_CONFIG_VALUE_1"]),
            hooks.path().join("01ABC")
        );
    }

    #[test]
    fn quoting_survives_single_quotes() {
        assert_eq!(quote("a'b c"), r"'a'\''b c'");
    }
}
