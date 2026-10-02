//! A project's setup command, run once in each new session worktree.

use std::ffi::OsString;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tokio::io::AsyncReadExt;
use tokio::net::unix::pipe;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

/// How much of the command's output is kept: its tail, in bytes.
const OUTPUT_MAX: usize = 64 * 1024;

/// How many of the output's last lines a failure's error message quotes.
const ERROR_LINES: usize = 20;

/// How long output is still read once the command has ended. A process it left running in the
/// background may hold the output open for good.
const DRAIN: Duration = Duration::from_millis(200);

/// How a setup command ended.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Outcome {
    /// Its stdout and stderr, interleaved; the tail when it wrote more than [`OUTPUT_MAX`].
    pub(super) output: String,
    /// Why it failed; `None` when it exited 0.
    pub(super) failure: Option<String>,
}

impl Outcome {
    /// The error a failed setup leaves the session with: why, and the output's last lines.
    pub(super) fn error_message(&self, command: &str) -> Option<String> {
        let failure = self.failure.as_ref()?;
        let lines: Vec<&str> = self.output.lines().collect();
        let tail = lines[lines.len().saturating_sub(ERROR_LINES)..].join("\n");
        let mut message = format!("the setup command `{command}` {failure}");
        if !tail.is_empty() {
            message.push_str(":\n");
            message.push_str(&tail);
        }
        Some(message)
    }
}

/// Runs `command` with `sh -c` in `cwd` after `launcher`, with `env`, until it exits, `timeout`
/// passes or `cancel` fires; then kills its process group.
pub(super) async fn run(
    command: &str,
    cwd: &Path,
    launcher: &[OsString],
    env: &[(String, String)],
    timeout: Duration,
    cancel: CancellationToken,
) -> Outcome {
    match run_inner(command, cwd, launcher, env, timeout, cancel).await {
        Ok(outcome) => outcome,
        Err(err) => Outcome {
            output: String::new(),
            failure: Some(format!("could not start: {err}")),
        },
    }
}

async fn run_inner(
    command: &str,
    cwd: &Path,
    launcher: &[OsString],
    env: &[(String, String)],
    timeout: Duration,
    cancel: CancellationToken,
) -> std::io::Result<Outcome> {
    let (reader, writer) = std::io::pipe()?;
    let mut child = {
        let shell = [OsString::from("sh"), "-c".into(), command.into()];
        let mut argv = launcher.iter().chain(&shell);
        let program = argv.next().unwrap_or(&shell[0]);
        // The command keeps the pipe's write end until it is dropped, after the spawn: only
        // the child's copies may hold the output open.
        let mut cmd = Command::new(program);
        cmd.args(argv)
            .current_dir(cwd)
            .envs(env.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::null())
            .stderr(writer.try_clone()?)
            .stdout(writer)
            // Its own group, so a timeout kills whatever it started too.
            .process_group(0)
            .kill_on_drop(true);
        cmd.spawn()?
    };
    let mut output = pipe::Receiver::from_owned_fd(OwnedFd::from(reader))?;
    let mut tail = Vec::new();
    let mut chunk = [0; 8192];
    let mut open = true;
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let ended = loop {
        tokio::select! {
            read = output.read(&mut chunk), if open => match read {
                Ok(0) | Err(_) => open = false,
                Ok(n) => keep(&mut tail, &chunk[..n]),
            },
            status = child.wait() => break Ended::Exited(status?),
            () = &mut deadline => break Ended::TimedOut,
            () = cancel.cancelled() => break Ended::Interrupted,
        }
    };
    if !matches!(ended, Ended::Exited(_)) {
        if let Some(pid) = child.id().and_then(|pid| i32::try_from(pid).ok()) {
            // The group is gone already when its last process exited meanwhile.
            let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
        }
        let _ = child.wait().await;
    }
    while open {
        match tokio::time::timeout(DRAIN, output.read(&mut chunk)).await {
            Ok(Ok(n)) if n > 0 => keep(&mut tail, &chunk[..n]),
            _ => open = false,
        }
    }
    let failure = match ended {
        Ended::Exited(status) if status.success() => None,
        Ended::Exited(status) => Some(format!("failed ({status})")),
        Ended::TimedOut => Some(format!("timed out after {}", human(timeout))),
        Ended::Interrupted => Some("was interrupted".to_owned()),
    };
    Ok(Outcome {
        output: String::from_utf8_lossy(&tail).into_owned(),
        failure,
    })
}

enum Ended {
    Exited(ExitStatus),
    TimedOut,
    Interrupted,
}

/// Appends `bytes` to `tail`, keeping its last [`OUTPUT_MAX`] bytes.
fn keep(tail: &mut Vec<u8>, bytes: &[u8]) {
    tail.extend_from_slice(bytes);
    if tail.len() > OUTPUT_MAX {
        tail.drain(..tail.len() - OUTPUT_MAX);
    }
}

/// `timeout` as an error message gives it: `10 min`, `90 s`, `200 ms`.
fn human(timeout: Duration) -> String {
    let secs = timeout.as_secs();
    if secs == 0 {
        format!("{} ms", timeout.as_millis())
    } else if secs.is_multiple_of(60) {
        format!("{} min", secs / 60)
    } else {
        format!("{secs} s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn sh(command: &str, timeout: Duration) -> Outcome {
        let dir = tempfile::tempdir().unwrap();
        run(
            command,
            dir.path(),
            &[],
            &[],
            timeout,
            CancellationToken::new(),
        )
        .await
    }

    #[tokio::test]
    async fn a_command_that_exits_0_succeeds_with_its_output_interleaved() {
        let outcome = sh(
            "echo one; echo two >&2; echo three",
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(
            outcome,
            Outcome {
                output: "one\ntwo\nthree\n".to_owned(),
                failure: None,
            }
        );
        assert_eq!(outcome.error_message("x"), None);
    }

    #[tokio::test]
    async fn a_failing_command_reports_its_status_and_output_tail() {
        let outcome = sh(
            "for i in $(seq 1 30); do echo line $i; done; exit 3",
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(outcome.failure.as_deref(), Some("failed (exit status: 3)"));
        let message = outcome.error_message("make dev").unwrap();
        let expected: Vec<String> = (11..=30).map(|i| format!("line {i}")).collect();
        assert_eq!(
            message,
            format!(
                "the setup command `make dev` failed (exit status: 3):\n{}",
                expected.join("\n")
            )
        );
    }

    #[tokio::test]
    async fn a_command_past_its_timeout_is_killed_with_what_it_started() {
        let dir = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let outcome = run(
            "echo started; (sleep 30; touch late) & sleep 30",
            dir.path(),
            &[],
            &[],
            Duration::from_millis(300),
            CancellationToken::new(),
        )
        .await;
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(outcome.output, "started\n");
        assert_eq!(outcome.failure.as_deref(), Some("timed out after 300 ms"));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!dir.path().join("late").exists());
    }

    #[tokio::test]
    async fn a_process_left_in_the_background_does_not_hold_setup_up() {
        let started = std::time::Instant::now();
        let outcome = sh("sleep 3 & echo done", Duration::from_secs(10)).await;
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(outcome.failure, None);
        assert_eq!(outcome.output, "done\n");
    }

    #[tokio::test]
    async fn only_the_output_tail_is_kept() {
        let outcome = sh(
            "head -c 100000 /dev/zero | tr '\\0' x",
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(outcome.output.len(), OUTPUT_MAX);
    }

    #[test]
    fn timeouts_read_naturally() {
        assert_eq!(human(Duration::from_secs(600)), "10 min");
        assert_eq!(human(Duration::from_secs(90)), "90 s");
        assert_eq!(human(Duration::from_millis(200)), "200 ms");
    }
}
