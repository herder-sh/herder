//! A scripted adapter for tests: no process, no network.
//!
//! A script is a JSON Lines file of steps, run top to bottom. Blank lines and lines starting
//! with `#` are skipped. Each step is one of:
//!
//! - `{"expect": <command>}`: wait for the next [`AdapterCommand`]; it must equal this one.
//!   An expected `shutdown` ends the session with a clean `exited`.
//! - `{"emit": <event>}`: send this [`AdapterEvent`]. Emitting `exited` ends the session.
//! - `{"sleep_ms": <n>}`: wait `n` milliseconds.
//!
//! Commands and events use their serde form, tagged on `"type"`. After the last step the fake
//! waits for `shutdown` (or every sender dropped) and emits `exited` with no error.
//!
//! Any other command, at any point, is a script mismatch: the fake emits `exited` with a
//! `fatal` error naming the script line, the expected and the received command, and stops.
//! Waiting for `answer_approval` or `answer_question` is how a script holds an approval or a
//! question round-trip open.

use std::path::{Path, PathBuf};
use std::time::Duration;

use herder_protocol::{ErrorClass, TurnError};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::{
    Adapter, AdapterCommand, AdapterEvent, AdapterSession, Capabilities, StartFuture, StartRequest,
};

/// Events the fake buffers before it waits for the reader.
const EVENT_BUFFER: usize = 64;

/// An adapter that replays a script instead of running a CLI; every start replays it afresh.
#[derive(Clone, Debug)]
pub struct FakeAdapter {
    /// Path of the JSON Lines script.
    pub script: PathBuf,
    /// Capabilities every session reports.
    pub capabilities: Capabilities,
}

impl FakeAdapter {
    /// A fake running `script`, reporting every capability.
    pub fn new(script: impl Into<PathBuf>) -> Self {
        Self {
            script: script.into(),
            capabilities: Capabilities {
                native_model_switch: true,
                native_permission_mode_switch: true,
                reports_usage: true,
                native_resume: true,
            },
        }
    }
}

impl Adapter for FakeAdapter {
    fn start(&self, _request: StartRequest) -> StartFuture {
        let script = load(&self.script);
        let capabilities = self.capabilities;
        Box::pin(async move {
            let steps = script?;
            let (commands, command_rx) = mpsc::unbounded_channel();
            let (event_tx, events) = mpsc::channel(EVENT_BUFFER);
            tokio::spawn(run(steps, command_rx, event_tx));
            Ok(AdapterSession {
                capabilities,
                commands,
                events,
            })
        })
    }
}

/// One script step.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Step {
    Expect(AdapterCommand),
    Emit(AdapterEvent),
    SleepMs(u64),
}

/// Reads a script into its steps, each with its 1-based line number.
fn load(path: &Path) -> Result<Vec<(usize, Step)>, TurnError> {
    let fatal = |message: String| TurnError {
        class: ErrorClass::Fatal,
        message,
    };
    let text = std::fs::read_to_string(path)
        .map_err(|err| fatal(format!("fake script {}: {err}", path.display())))?;
    text.lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line.trim()))
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'))
        .map(|(number, line)| {
            serde_json::from_str(line)
                .map(|step| (number, step))
                .map_err(|err| fatal(format!("fake script {}:{number}: {err}", path.display())))
        })
        .collect()
}

/// Plays the script against the session's channels.
async fn run(
    steps: Vec<(usize, Step)>,
    mut commands: mpsc::UnboundedReceiver<AdapterCommand>,
    events: mpsc::Sender<AdapterEvent>,
) {
    for (line, step) in steps {
        match step {
            Step::Expect(expected) => match commands.recv().await {
                Some(AdapterCommand::Shutdown) if expected == AdapterCommand::Shutdown => {
                    return exit(&events, None).await;
                }
                Some(received) if received == expected => {}
                Some(received) => {
                    let message = format!(
                        "fake script line {line}: expected {expected:?}, received {received:?}"
                    );
                    return exit(&events, Some(message)).await;
                }
                None => return exit(&events, None).await,
            },
            Step::Emit(event) => {
                let last = matches!(event, AdapterEvent::Exited { .. });
                if events.send(event).await.is_err() || last {
                    return;
                }
            }
            Step::SleepMs(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
        }
    }
    match commands.recv().await {
        Some(AdapterCommand::Shutdown) | None => exit(&events, None).await,
        Some(received) => {
            let message = format!("fake script ended, received {received:?}");
            exit(&events, Some(message)).await;
        }
    }
}

/// Sends the final `Exited`, with a fatal error when `mismatch` says why.
async fn exit(events: &mpsc::Sender<AdapterEvent>, mismatch: Option<String>) {
    let error = mismatch.map(|message| TurnError {
        class: ErrorClass::Fatal,
        message,
    });
    // The reader may be gone already; there is no one left to tell.
    let _ = events.send(AdapterEvent::Exited { error }).await;
}
