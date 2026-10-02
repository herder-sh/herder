//! Per-session systemd scopes: every agent CLI runs inside its own transient user scope, so a
//! runaway build in one session is throttled or killed there instead of freezing the host.
//!
//! Each start of a session's CLI runs it as
//!
//! ```text
//! systemd-run --user --scope --quiet --collect --unit=herder-<session>-<n>
//!     --property=CPUWeight=… --property=MemoryHigh=… --property=MemoryMax=…
//!     --property=MemorySwapMax=0 --property=OOMPolicy=continue --nice=… -- <cli> <args>
//! ```
//!
//! through [`StartRequest::launcher`](herder_adapters::StartRequest::launcher). `systemd-run
//! --scope` execs into the CLI, so the CLI keeps its pid, environment, working directory and
//! pipes, and everything it starts lands in the scope too.
//!
//! - `MemoryHigh` throttles the scope by reclaiming hard; `MemoryMax` is where the kernel
//!   OOM-kills inside it. Swap is off for the scope: the host has gigabytes of it, and a scope
//!   paging out to disk stalls the whole machine just as running out of memory does.
//! - `OOMPolicy=continue` kills only the process that ran out, such as one `rustc`, and leaves
//!   the agent running to see its command fail; systemd's default would stop the whole scope.
//! - The limits come from the daemon's `[resources]` table ([`ResourcesConfig`]), computed from
//!   the host's memory at start. Child sessions get a smaller CPU weight than primaries.
//!
//! Support is probed once at startup ([`Scopes::detect`]). Without a systemd user session the
//! CLIs run unwrapped, a warning is logged, and [`Scopes::limits_on`] is false.
//!
//! [`run_sampler`] reads each scope's cgroup every [`SAMPLE_INTERVAL`], adds the containers
//! [`Docker`] tracks for the session, and publishes
//! [`ServerMessage::SessionResources`](herder_protocol::ServerMessage::SessionResources)
//! through the [`Hub`] when it changed; once neither is left, it publishes one zero usage.
//!
//! Archiving a session stops what it left running ([`Scopes::stop`], [`processes`]); its
//! containers stay up and listed, for the user to bring down.
//!
//! Scopes bound each session; [`Admission`] bounds their sum: a turn starts only while the
//! host has room for it ([`admission`]).

pub mod admission;
mod cgroup;
pub mod docker;
pub mod processes;

use std::collections::HashMap;
use std::ffi::OsString;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use herder_protocol::{Container, SessionId, SessionUsage};
use serde::Deserialize;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::hub::Hub;

pub use admission::{Admission, Budget, Parked, Permit, ProcHost, ReadHost, Reading, Ticket};
pub use cgroup::Sample;
pub use docker::Docker;

/// How often scopes are read; the contract's limit for `session_resources`.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

/// How long a launched scope may take to appear before it is forgotten: its CLI never started.
const APPEAR_TIMEOUT: Duration = Duration::from_secs(60);

/// How long the startup probe may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-session limits and the host's turn budget: the `[resources]` table.
///
/// ```toml
/// [resources]
/// memory_max_percent = 40   # of the host's RAM, per session
/// memory_high_percent = 80  # of memory_max, where throttling starts
/// cpu_weight = 100          # primaries; systemd's default for everything else is 100
/// child_cpu_weight = 50     # child sessions
/// nice = 10
/// max_turns = 3             # turns running at once; max(1, cores / 4) when absent
/// min_memory_available_mib = 2048  # MemAvailable a new turn needs
/// max_memory_pressure = 20  # PSI memory `some avg10`, in percent, that stops new turns
/// max_load_percent = 100    # 1-min load, in percent of the cores, that stops new turns
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourcesConfig {
    /// Memory one session may use, in percent of the host's RAM.
    pub memory_max_percent: u8,
    /// Where the scope starts being throttled, in percent of the session's memory maximum.
    pub memory_high_percent: u8,
    /// CPU weight of a primary session's scope, 1 to 10000.
    pub cpu_weight: u16,
    /// CPU weight of a child session's scope, 1 to 10000.
    pub child_cpu_weight: u16,
    /// Niceness of every agent CLI, -20 to 19.
    pub nice: i8,
    /// Most turns running on this host at once; `None` is a quarter of its cores, at least 1.
    pub max_turns: Option<u32>,
    /// Least memory available, in MiB, for a new turn to start.
    pub min_memory_available_mib: u64,
    /// PSI memory `some avg10`, in percent, at which no new turn starts, 1 to 100.
    pub max_memory_pressure: u8,
    /// One-minute load average, in percent of the cores, at which no new turn starts.
    pub max_load_percent: u16,
}

impl Default for ResourcesConfig {
    fn default() -> Self {
        Self {
            memory_max_percent: 40,
            memory_high_percent: 80,
            cpu_weight: 100,
            child_cpu_weight: 50,
            nice: 10,
            max_turns: None,
            min_memory_available_mib: 2048,
            max_memory_pressure: 20,
            max_load_percent: 100,
        }
    }
}

impl ResourcesConfig {
    /// Checks every value is in the range systemd accepts.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (1..=100).contains(&self.memory_max_percent),
            "resources.memory_max_percent must be 1 to 100"
        );
        anyhow::ensure!(
            (1..=100).contains(&self.memory_high_percent),
            "resources.memory_high_percent must be 1 to 100"
        );
        for (key, weight) in [
            ("cpu_weight", self.cpu_weight),
            ("child_cpu_weight", self.child_cpu_weight),
        ] {
            anyhow::ensure!(
                (1..=10_000).contains(&weight),
                "resources.{key} must be 1 to 10000"
            );
        }
        anyhow::ensure!(
            (-20..=19).contains(&self.nice),
            "resources.nice must be -20 to 19"
        );
        anyhow::ensure!(
            self.max_turns != Some(0),
            "resources.max_turns must be at least 1"
        );
        anyhow::ensure!(
            (1..=100).contains(&self.max_memory_pressure),
            "resources.max_memory_pressure must be 1 to 100"
        );
        anyhow::ensure!(
            self.max_load_percent >= 1,
            "resources.max_load_percent must be at least 1"
        );
        Ok(())
    }

    /// The limits of one session on `host`. Its niceness is never below the daemon's own: an
    /// unprivileged process cannot lower it, and `systemd-run` would refuse to start the CLI.
    pub fn limits(&self, host: &Host, child: bool) -> Limits {
        let memory_max = host.memory_total / 100 * u64::from(self.memory_max_percent);
        Limits {
            cpu_weight: if child {
                self.child_cpu_weight
            } else {
                self.cpu_weight
            },
            memory_high: memory_max / 100 * u64::from(self.memory_high_percent),
            memory_max,
            nice: self.nice.max(host.nice),
        }
    }
}

impl ResourcesConfig {
    /// The turn budget of a host with `cores` CPUs.
    pub fn budget(&self, cores: u32) -> Budget {
        Budget {
            cores,
            max_turns: self.max_turns.unwrap_or((cores / 4).max(1)),
            min_memory_available: self.min_memory_available_mib.saturating_mul(1024 * 1024),
            max_memory_pressure: f64::from(self.max_memory_pressure),
            max_load: f64::from(cores) * f64::from(self.max_load_percent) / 100.0,
        }
    }
}

/// The limits one scope runs with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// systemd `CPUWeight`.
    pub cpu_weight: u16,
    /// systemd `MemoryHigh`, in bytes.
    pub memory_high: u64,
    /// systemd `MemoryMax`, in bytes.
    pub memory_max: u64,
    /// Niceness.
    pub nice: i8,
}

/// The host totals limits and usage are computed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Host {
    /// RAM, in bytes.
    pub memory_total: u64,
    /// CPUs this process may run on.
    pub cores: u32,
    /// The daemon's own niceness.
    pub nice: i8,
}

impl Host {
    /// Reads this host's totals.
    pub fn read() -> anyhow::Result<Self> {
        let meminfo = std::fs::read_to_string("/proc/meminfo")?;
        let memory_total = cgroup::mem_total(&meminfo)
            .ok_or_else(|| anyhow::anyhow!("/proc/meminfo has no MemTotal"))?;
        let cores = cores();
        let stat = std::fs::read_to_string("/proc/self/stat")?;
        let nice = cgroup::nice(&stat)
            .ok_or_else(|| anyhow::anyhow!("/proc/self/stat has no niceness"))?;
        Ok(Self {
            memory_total,
            cores,
            nice,
        })
    }
}

/// CPUs this process may run on.
pub fn cores() -> u32 {
    std::thread::available_parallelism().map_or(1, |n| u32::try_from(n.get()).unwrap_or(u32::MAX))
}

/// The `systemd-run` launcher for a scope named `unit` with `limits`; the CLI follows it.
pub fn launcher(unit: &str, limits: &Limits) -> Vec<OsString> {
    let mut argv: Vec<String> = ["systemd-run", "--user", "--scope", "--quiet", "--collect"]
        .map(str::to_owned)
        .to_vec();
    argv.push(format!("--unit={unit}"));
    for property in [
        format!("CPUWeight={}", limits.cpu_weight),
        format!("MemoryHigh={}", limits.memory_high),
        format!("MemoryMax={}", limits.memory_max),
        "MemorySwapMax=0".to_owned(),
        "OOMPolicy=continue".to_owned(),
    ] {
        argv.push(format!("--property={property}"));
    }
    argv.push(format!("--nice={}", limits.nice));
    argv.push("--".to_owned());
    argv.into_iter().map(OsString::from).collect()
}

/// The scope unit for the `n`th start of `session`'s CLI: `herder-<session>-<n>.scope`, with
/// anything systemd does not allow in a unit name replaced by `_`.
pub fn unit_name(session: &SessionId, n: u64) -> String {
    let session: String = session
        .as_str()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("herder-{session}-{n}.scope")
}

/// Every session's current scope, and whether scopes are used at all.
#[derive(Debug)]
pub struct Scopes {
    config: ResourcesConfig,
    host: Host,
    on: bool,
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    /// Number of the next scope; starts at the daemon's start time, so a scope left behind by
    /// an earlier daemon never has the name of a new one.
    next: u64,
    scopes: HashMap<SessionId, Live>,
    /// The last usage published for each session still showing something.
    published: HashMap<SessionId, SessionUsage>,
}

/// One session's latest scope.
#[derive(Debug, Clone)]
struct Live {
    unit: String,
    launched: Instant,
    /// The scope's cgroup directory, once systemd has created it.
    cgroup: Option<PathBuf>,
    /// The previous reading, for the CPU share since then.
    last: Option<(Instant, Sample)>,
}

impl Scopes {
    /// Scopes with `config` on `host`; `on` says whether this host can run them.
    pub fn new(config: ResourcesConfig, host: Host, on: bool) -> Self {
        let next = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        Self {
            config,
            host,
            on,
            state: Mutex::new(State {
                next,
                scopes: HashMap::new(),
                published: HashMap::new(),
            }),
        }
    }

    /// Reads the host and probes once for a systemd user session that can run scopes. Without
    /// one, or when the host cannot be read, sessions run without limits and a warning says so.
    pub async fn detect(config: ResourcesConfig) -> Self {
        let probed = match Host::read() {
            Ok(host) => probe().await.map(|()| host),
            Err(err) => Err(format!("reading the host's memory: {err:#}")),
        };
        let host = probed.as_ref().copied().unwrap_or(Host {
            memory_total: 0,
            cores: 1,
            nice: 0,
        });
        let on = match probed {
            Ok(_) => {
                let limits = config.limits(&host, false);
                info!(
                    memory_max = limits.memory_max,
                    memory_high = limits.memory_high,
                    cpu_weight = limits.cpu_weight,
                    child_cpu_weight = config.child_cpu_weight,
                    nice = limits.nice,
                    "agent sessions run in systemd scopes"
                );
                true
            }
            Err(reason) => {
                warn!(
                    "resource limits are off: agent CLIs cannot run in systemd scopes \
                     ({reason}), so they run unconfined and can exhaust this host"
                );
                false
            }
        };
        Self::new(config, host, on)
    }

    /// Whether sessions run in scopes; false where the host has no systemd user session.
    pub fn limits_on(&self) -> bool {
        self.on
    }

    /// The limits a session gets: a child's CPU weight is the smaller one.
    pub fn limits(&self, child: bool) -> Limits {
        self.config.limits(&self.host, child)
    }

    /// The launcher for the next start of `session`'s CLI with `limits`, recording its scope as
    /// the session's; empty when limits are off.
    pub fn launch(&self, session: &SessionId, limits: &Limits) -> Vec<OsString> {
        if !self.on {
            return Vec::new();
        }
        let mut state = self.lock();
        let unit = unit_name(session, state.next);
        state.next += 1;
        let launcher = launcher(&unit, limits);
        state.scopes.insert(
            session.clone(),
            Live {
                unit,
                launched: Instant::now(),
                cgroup: None,
                last: None,
            },
        );
        launcher
    }

    /// The unit of `session`'s latest scope, while the daemon tracks it.
    pub fn unit(&self, session: &SessionId) -> Option<String> {
        self.lock()
            .scopes
            .get(session)
            .map(|live| live.unit.clone())
    }

    /// The pids of `session`'s live processes: those in its scopes, or with limits off,
    /// those started with its id in [`processes::SESSION_ENV`].
    pub async fn processes(&self, session: &SessionId) -> Vec<u32> {
        processes::list(self.source(session, &unit_prefix(session))).await
    }

    /// Stops everything `session` still runs, once its CLI is gone: `SIGTERM`, then `SIGKILL`
    /// after [`processes::TERM_GRACE`]. Returns how many processes there were.
    pub async fn stop(&self, session: &SessionId) -> usize {
        let prefix = unit_prefix(session);
        let stopped = processes::stop(self.source(session, &prefix)).await;
        if !stopped.is_empty() {
            info!(session_id = %session, pids = ?stopped, "stopped the archived session's leftover processes");
        }
        stopped.len()
    }

    fn source<'a>(&self, session: &'a SessionId, prefix: &'a str) -> processes::Source<'a> {
        if self.on {
            processes::Source::Scopes(prefix)
        } else {
            processes::Source::Env(session)
        }
    }

    /// Reads every scope once, adds each session's `containers`, and publishes the usages
    /// that changed.
    async fn sample(&self, hub: &Hub, containers: &HashMap<SessionId, Vec<Container>>) {
        let pending: Vec<(SessionId, String)> = self
            .lock()
            .scopes
            .iter()
            .filter(|(_, live)| live.cgroup.is_none())
            .map(|(session, live)| (session.clone(), live.unit.clone()))
            .collect();
        for (session, unit) in pending {
            if let Some(dir) = cgroup_of(&unit).await {
                let mut state = self.lock();
                if let Some(live) = state.scopes.get_mut(&session).filter(|l| l.unit == unit) {
                    live.cgroup = Some(dir);
                }
            }
        }
        let now = Instant::now();
        let mut publish = Vec::new();
        {
            let mut state = self.lock();
            let mut usages = HashMap::new();
            state.scopes.retain(|session, live| {
                let Some(dir) = &live.cgroup else {
                    return now.duration_since(live.launched) < APPEAR_TIMEOUT;
                };
                match Sample::read(dir) {
                    Ok(sample) => {
                        let usage = cgroup::usage(live.last.as_ref(), now, &sample, self.host);
                        live.last = Some((now, sample));
                        usages.insert(session.clone(), usage);
                        true
                    }
                    // The scope is gone: every process in it exited.
                    Err(_) => false,
                }
            });
            for (session, list) in containers {
                usages
                    .entry(session.clone())
                    .or_insert_with(cgroup::zero)
                    .containers
                    .clone_from(list);
            }
            let gone: Vec<SessionId> = state
                .published
                .keys()
                .filter(|session| !usages.contains_key(*session))
                .cloned()
                .collect();
            for session in gone {
                state.published.remove(&session);
                publish.push((session, cgroup::zero()));
            }
            for (session, usage) in usages {
                if state.published.get(&session) != Some(&usage) {
                    state.published.insert(session.clone(), usage.clone());
                    publish.push((session, usage));
                }
            }
        }
        for (session, usage) in publish {
            hub.session_resources(&session, usage);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // Every update is a single insert, remove or field write, so a panic mid-update leaves
        // the state consistent.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Publishes every session's usage, from `scopes` and `docker`, to `hub` each
/// [`SAMPLE_INTERVAL`] until `shutdown`; `worktrees` lists every session's worktree.
pub async fn run_sampler<F, Fut>(
    scopes: &Scopes,
    docker: &Docker,
    worktrees: F,
    hub: &Hub,
    shutdown: CancellationToken,
) where
    F: Fn() -> Fut,
    Fut: Future<Output = anyhow::Result<Vec<(SessionId, PathBuf)>>>,
{
    let mut tick = tokio::time::interval(SAMPLE_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            _ = tick.tick() => {
                let containers = docker.poll(&worktrees).await;
                scopes.sample(hub, &containers).await;
            }
        }
    }
}

/// The start of every scope unit name of `session`.
fn unit_prefix(session: &SessionId) -> String {
    let unit = unit_name(session, 0);
    unit.strip_suffix("0.scope").unwrap_or(&unit).to_owned()
}

/// Runs a no-op command in a scope, the way sessions will.
async fn probe() -> Result<(), String> {
    let mut command = Command::new("systemd-run");
    command
        .args(["--user", "--scope", "--quiet", "--collect", "--", "true"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(PROBE_TIMEOUT, command.output())
        .await
        .map_err(|_| "systemd-run did not answer".to_owned())?
        .map_err(|err| format!("running systemd-run: {err}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

/// The cgroup directory of `unit`, once systemd has created it.
async fn cgroup_of(unit: &str) -> Option<PathBuf> {
    let output = Command::new("systemctl")
        .args(["--user", "show", "--property=ControlGroup", "--value", unit])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let path = path.strip_prefix('/')?;
    let dir = PathBuf::from("/sys/fs/cgroup").join(path);
    dir.is_dir().then_some(dir)
}

#[cfg(test)]
mod tests;
