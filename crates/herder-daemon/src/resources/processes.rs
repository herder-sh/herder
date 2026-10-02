//! A session's live processes, and stopping them once it is archived.
//!
//! With scopes on, they are the processes in every `herder-<session>-<n>.scope` systemd still
//! has, including scopes of earlier starts and of an earlier daemon. Without scopes they are
//! the processes whose environment carries the session's [`SESSION_ENV`] entry, which every
//! CLI is started with and everything it starts inherits. Sessions' CLIs share the daemon's
//! process group, so a process group could not tell them apart from the daemon.

use std::path::{Path, PathBuf};
use std::time::Duration;

use herder_protocol::SessionId;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tokio::process::Command;

/// The variable every session's CLI is started with: the session's id.
pub const SESSION_ENV: &str = "HERDER_SESSION_ID";

/// How long processes get to exit after `SIGTERM` before they are killed.
pub const TERM_GRACE: Duration = Duration::from_secs(3);

/// How often stopping processes are checked on.
const POLL: Duration = Duration::from_millis(100);

/// Where a session's processes are found.
#[derive(Debug, Clone, Copy)]
pub(super) enum Source<'a> {
    /// The cgroups of the session's scopes, under the unit name prefix `herder-<session>-`.
    Scopes(&'a str),
    /// Processes started with this session id in [`SESSION_ENV`].
    Env(&'a SessionId),
}

/// The pids of the session's live processes.
pub(super) async fn list(source: Source<'_>) -> Vec<u32> {
    match source {
        Source::Scopes(prefix) => {
            let mut pids = Vec::new();
            for dir in scope_cgroups(prefix).await {
                pids.extend(cgroup_procs(&dir));
            }
            pids
        }
        Source::Env(session) => by_env(Path::new("/proc"), session),
    }
}

/// Sends `SIGTERM` to the session's processes, then `SIGKILL` to those left after
/// [`TERM_GRACE`]; returns the pids it found at first.
pub(super) async fn stop(source: Source<'_>) -> Vec<u32> {
    let found = list(source).await;
    signal(&found, Signal::SIGTERM);
    let deadline = tokio::time::Instant::now() + TERM_GRACE;
    let mut left = found.clone();
    while !left.is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(POLL).await;
        left = list(source).await;
    }
    // A process may fork while it is being killed: look again until nothing is left.
    for _ in 0..10 {
        if left.is_empty() {
            break;
        }
        signal(&left, Signal::SIGKILL);
        tokio::time::sleep(POLL).await;
        left = list(source).await;
    }
    found
}

fn signal(pids: &[u32], signal: Signal) {
    for &pid in pids {
        if let Ok(pid) = i32::try_from(pid) {
            // It may have exited since it was listed.
            let _ = kill(Pid::from_raw(pid), signal);
        }
    }
}

/// The cgroup directories of the units whose names start with `prefix`.
async fn scope_cgroups(prefix: &str) -> Vec<PathBuf> {
    let pattern = format!("{prefix}*.scope");
    let Ok(output) = Command::new("systemctl")
        .args([
            "--user",
            "list-units",
            "--all",
            "--plain",
            "--no-legend",
            "--",
        ])
        .arg(&pattern)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
    else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some(unit) = line.split_whitespace().next() else {
            continue;
        };
        if let Some(dir) = super::cgroup_of(unit).await {
            dirs.push(dir);
        }
    }
    dirs
}

/// The pids in the cgroup at `dir`; none once it is gone.
fn cgroup_procs(dir: &Path) -> Vec<u32> {
    std::fs::read_to_string(dir.join("cgroup.procs"))
        .map(|procs| procs.lines().filter_map(|pid| pid.parse().ok()).collect())
        .unwrap_or_default()
}

/// The pids under `proc` whose environment has `session` in [`SESSION_ENV`]. Processes of
/// other users cannot be read and are skipped, as is this one.
pub(super) fn by_env(proc: &Path, session: &SessionId) -> Vec<u32> {
    let entry = format!("{SESSION_ENV}={session}");
    let own = std::process::id();
    let Ok(dir) = std::fs::read_dir(proc) else {
        return Vec::new();
    };
    dir.filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|&pid| pid != own)
        .filter(|pid| {
            std::fs::read(proc.join(pid.to_string()).join("environ")).is_ok_and(|environ| {
                environ
                    .split(|&byte| byte == 0)
                    .any(|var| var == entry.as_bytes())
            })
        })
        .collect()
}
