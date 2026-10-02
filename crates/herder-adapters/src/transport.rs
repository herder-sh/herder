//! The line-oriented stdio seam every vendor adapter drives its CLI through.
//!
//! A [`Transport`] is a child process seen as lines: lines in on stdin, lines out on stdout, and
//! how it exited. It knows nothing about JSON or any provider. [`Transport::spawn`] runs a real
//! command; [`Transport::replay`] plays a recorded [`Fixture`] instead, so an adapter written
//! against a `Transport` is tested without the vendor CLI.
//!
//! Both are the same channels, so an adapter can read and write at once from a `select!` loop
//! and is tested on exactly the code it runs in production.

use std::io;
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot};

use crate::fixture::Fixture;

/// Lines buffered in each direction before the faster side waits.
const LINE_BUFFER: usize = 64;

/// A child process, as lines.
#[derive(Debug)]
pub struct Transport {
    /// Lines for the child's stdin, without the trailing newline. Dropping it closes stdin.
    /// Sends fail once the child stops reading.
    pub stdin: mpsc::Sender<String>,
    /// Lines from the child's stdout, without the trailing newline; closes at end of output.
    pub stdout: mpsc::Receiver<String>,
    /// Resolves once the child is gone. Dropping it kills the child.
    pub exit: oneshot::Receiver<Exit>,
}

/// How a child ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exit {
    /// The child exited with this code; `None` when a signal killed it.
    Code(Option<i32>),
    /// The transport broke: the child could not be waited on, or a replayed fixture did not
    /// match what the adapter sent. The message says why.
    Failed(String),
}

impl Transport {
    /// Spawns `command` with piped stdin and stdout; must be called inside a tokio runtime.
    ///
    /// Everything else (program, arguments, environment, working directory, stderr) is the
    /// caller's, as set on `command`.
    pub fn spawn(mut command: Command) -> io::Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let missing = || io::Error::other("child spawned without a piped stdio stream");
        let child_stdin = child.stdin.take().ok_or_else(missing)?;
        let child_stdout = child.stdout.take().ok_or_else(missing)?;

        let (stdin, stdin_rx) = mpsc::channel(LINE_BUFFER);
        let (stdout_tx, stdout) = mpsc::channel(LINE_BUFFER);
        let (exit_tx, exit) = oneshot::channel();
        tokio::spawn(write_lines(stdin_rx, child_stdin));
        tokio::spawn(read_lines(child_stdout, stdout_tx));
        tokio::spawn(wait(child, exit_tx));
        Ok(Self {
            stdin,
            stdout,
            exit,
        })
    }

    /// Replays `fixture` as if it were the child; must be called inside a tokio runtime.
    ///
    /// Each `in` record waits for the next line sent and checks it; each `out` record is
    /// delivered once everything before it has happened. The first mismatch ends the replay
    /// with [`Exit::Failed`] naming the fixture line and the difference.
    pub fn replay(fixture: Fixture) -> Self {
        let (stdin, stdin_rx) = mpsc::channel(LINE_BUFFER);
        let (stdout_tx, stdout) = mpsc::channel(LINE_BUFFER);
        let (exit_tx, exit) = oneshot::channel();
        tokio::spawn(fixture.play(stdin_rx, stdout_tx, exit_tx));
        Self {
            stdin,
            stdout,
            exit,
        }
    }
}

/// Writes each line to the child's stdin; returning drops it, which closes the pipe.
async fn write_lines(mut lines: mpsc::Receiver<String>, mut stdin: ChildStdin) {
    while let Some(line) = lines.recv().await {
        let mut bytes = line.into_bytes();
        bytes.push(b'\n');
        if stdin.write_all(&bytes).await.is_err() || stdin.flush().await.is_err() {
            // The child stopped reading; dropping `lines` makes further sends fail.
            return;
        }
    }
}

/// Forwards stdout lines until end of output. Bytes that are not UTF-8 are replaced, not fatal.
async fn read_lines(stdout: ChildStdout, lines: mpsc::Sender<String>) {
    let mut stdout = BufReader::new(stdout);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match stdout.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if buf.last() == Some(&b'\n') {
            buf.pop();
        }
        // A reader that is gone no longer cares; keep draining so the child never blocks.
        let _ = lines.send(String::from_utf8_lossy(&buf).into_owned()).await;
    }
}

/// Reports the child's exit, or kills it once nobody waits for the exit any more.
async fn wait(mut child: Child, mut exit: oneshot::Sender<Exit>) {
    let status = tokio::select! {
        status = child.wait() => status,
        () = exit.closed() => {
            // Already exiting or gone is fine: either way it is not left running.
            let _ = child.kill().await;
            return;
        }
    };
    let outcome = match status {
        Ok(status) => Exit::Code(status.code()),
        Err(err) => Exit::Failed(format!("waiting for the child: {err}")),
    };
    // The receiver may be gone already; there is no one left to tell.
    let _ = exit.send(outcome);
}
