//! Docker containers that sessions started: those Compose started from inside a session's
//! worktree, by the `com.docker.compose.project.working_dir` label it puts on each.
//!
//! [`Docker::poll`] runs `docker ps` at most once every [`POLL_INTERVAL`]. Containers outlive
//! the session that started them, archived or not, so they stay listed until they are removed;
//! [`Docker::compose_down`] removes a Compose project's. Without a `docker` command, or when
//! it fails, no containers are tracked and nothing is reported beyond a debug log.

use std::collections::HashMap;
use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use herder_protocol::{Container, ContainerState, SessionId};
use serde::Deserialize;
use tokio::process::Command;
use tracing::{debug, info};

/// How often `docker ps` runs at most.
pub const POLL_INTERVAL: Duration = Duration::from_secs(10);

/// How long one `docker` command may take.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The label Compose records the project directory in.
const WORKING_DIR: &str = "com.docker.compose.project.working_dir";

/// One line of `docker ps` per container, as JSON with every value quoted by Docker itself.
const FORMAT: &str = concat!(
    r#"{"id":{{json .ID}},"name":{{json .Names}},"image":{{json .Image}},"#,
    r#""state":{{json .State}},"project":{{json (.Label "com.docker.compose.project")}},"#,
    r#""working_dir":{{json (.Label "com.docker.compose.project.working_dir")}}}"#,
);

/// Containers by session, from the `docker` command.
#[derive(Debug)]
pub struct Docker {
    program: OsString,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    /// When `docker ps` last ran.
    polled: Option<Instant>,
    /// False once the command turned out not to exist.
    present: bool,
    containers: HashMap<SessionId, Vec<Container>>,
}

/// One line of `docker ps`.
#[derive(Debug, Deserialize)]
struct Line {
    id: String,
    name: String,
    image: String,
    state: ContainerState,
    project: String,
    working_dir: PathBuf,
}

impl Docker {
    /// Containers from `program`, the `docker` command.
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            state: Mutex::new(State {
                present: true,
                ..State::default()
            }),
        }
    }

    /// Each session's containers, from `docker ps` when the last run is [`POLL_INTERVAL`] old;
    /// `worktrees` lists every session's worktree and is only asked when a Compose project is
    /// running.
    pub async fn poll<F, Fut>(&self, worktrees: F) -> HashMap<SessionId, Vec<Container>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = anyhow::Result<Vec<(SessionId, PathBuf)>>>,
    {
        {
            let mut state = self.lock();
            let due = state.polled.is_none_or(|at| at.elapsed() >= POLL_INTERVAL);
            if !state.present || !due {
                return state.containers.clone();
            }
            state.polled = Some(Instant::now());
        }
        let lines = match self.ps().await {
            Ok(lines) => lines,
            Err(Missing) => {
                info!("no docker command: containers are not tracked");
                let mut state = self.lock();
                state.present = false;
                state.containers.clear();
                return HashMap::new();
            }
        };
        let containers = if lines.is_empty() {
            HashMap::new()
        } else {
            match worktrees().await {
                Ok(worktrees) => assign(lines, &worktrees),
                Err(err) => {
                    debug!("listing worktrees for containers: {err:#}");
                    return self.lock().containers.clone();
                }
            }
        };
        self.lock().containers = containers.clone();
        containers
    }

    /// Stops and removes the containers and networks of Compose project `project`.
    pub async fn compose_down(&self, project: &str) -> anyhow::Result<()> {
        let mut command = Command::new(&self.program);
        command.args(["compose", "--project-name", project, "down"]);
        let output = run(command)
            .await
            .map_err(|err| anyhow::anyhow!("running docker compose down: {err}"))?;
        anyhow::ensure!(
            output.status.success(),
            "docker compose down failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        // The next poll shows what is left.
        self.lock().polled = None;
        Ok(())
    }

    /// The Compose containers `docker ps` lists; none when docker fails.
    async fn ps(&self) -> Result<Vec<Line>, Missing> {
        let mut command = Command::new(&self.program);
        command.args([
            "ps",
            "--all",
            "--no-trunc",
            "--filter",
            &format!("label={WORKING_DIR}"),
            "--format",
            FORMAT,
        ]);
        let output = match run(command).await {
            Ok(output) => output,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Err(Missing),
            Err(err) => {
                debug!("running docker ps: {err}");
                return Ok(Vec::new());
            }
        };
        if !output.status.success() {
            debug!(
                "docker ps failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            return Ok(Vec::new());
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| match serde_json::from_str(line) {
                Ok(line) => Some(line),
                Err(err) => {
                    debug!("unreadable docker ps line {line:?}: {err}");
                    None
                }
            })
            .collect())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // Every update is a single field write, so a panic mid-update leaves it consistent.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The `docker` command does not exist.
struct Missing;

async fn run(mut command: Command) -> std::io::Result<std::process::Output> {
    command
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    tokio::time::timeout(TIMEOUT, command.output())
        .await
        .unwrap_or_else(|_| Err(std::io::Error::other("docker did not answer")))
}

/// The containers of each session whose worktree holds their Compose project's directory.
fn assign(
    lines: Vec<Line>,
    worktrees: &[(SessionId, PathBuf)],
) -> HashMap<SessionId, Vec<Container>> {
    let mut containers: HashMap<SessionId, Vec<Container>> = HashMap::new();
    for line in lines {
        let Some((session, _)) = worktrees
            .iter()
            .find(|(_, worktree)| inside(&line.working_dir, worktree))
        else {
            continue;
        };
        containers
            .entry(session.clone())
            .or_default()
            .push(Container {
                id: line.id,
                name: line.name,
                compose_project: Some(line.project).filter(|project| !project.is_empty()),
                image: line.image,
                state: line.state,
            });
    }
    for list in containers.values_mut() {
        list.sort_by(|a, b| a.name.cmp(&b.name));
    }
    containers
}

fn inside(dir: &Path, worktree: &Path) -> bool {
    !worktree.as_os_str().is_empty() && dir.starts_with(worktree)
}
