//! Capacity admission: a turn starts only while the host has room for one more, so the sum of
//! every session's agents cannot exceed the machine.
//!
//! The host admits a turn while all of these hold, checked in this order; the first that fails
//! is the [`Constraint`] clients see:
//!
//! 1. fewer than [`Budget::max_turns`] turns run (`max_turns`);
//! 2. `MemAvailable` is at least [`Budget::min_memory_available`] (`memory`);
//! 3. the one-minute load average is below [`Budget::max_load`], the core count by default
//!    (`load`);
//! 4. PSI memory `some avg10` is below [`Budget::max_memory_pressure`] (`pressure`); skipped
//!    on kernels without `/proc/pressure`.
//!
//! A turn the host cannot admit waits in one FIFO queue across all sessions. Waiting turns
//! are admitted in order when a running turn ends and every [`RECHECK_INTERVAL`]; a turn
//! never overtakes an earlier one. A running turn holds a [`Permit`]; dropping it frees the
//! turn's slot.
//!
//! A turn blocked in a tool call that waits for its children ([`Admission::park`]) uses no
//! CPU, so it gives its slot to them meanwhile: otherwise primaries waiting on their children
//! could take every slot and no child would ever start. When the call returns, the turn takes
//! its slot back at once, ahead of every waiting turn, even when that puts the host one over
//! [`Budget::max_turns`] until a turn ends; waiting for a slot there could deadlock with a
//! child whose question waits for that primary.
//!
//! `spawn` asks [`Admission::host_constraint`] and is refused as `host_busy` while memory,
//! load or pressure binds; a child spawned while only the turn limit binds waits for a slot
//! like any other turn.
//!
//! [`Admission::run`] publishes
//! [`ServerMessage::HostResources`](herder_protocol::ServerMessage::HostResources) through the
//! [`Hub`] each [`RECHECK_INTERVAL`] when it changed.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::Context;
use herder_protocol::{Constraint, HostResources, Pressure, SessionId};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::cgroup;
use crate::hub::Hub;

/// How often waiting turns are checked again and the host's resources published; the
/// contract's limit for `host_resources`.
pub const RECHECK_INTERVAL: Duration = Duration::from_secs(2);

/// How long `spawn` tells an agent to wait before it tries again.
pub const RETRY_AFTER_SECS: u32 = 30;

/// What the host may run: from the `[resources]` table and the host's cores.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Budget {
    /// CPUs this process may run on.
    pub cores: u32,
    /// Most turns running at once.
    pub max_turns: u32,
    /// Least `MemAvailable` a new turn needs, in bytes.
    pub min_memory_available: u64,
    /// PSI memory `some avg10`, in percent, at which no turn starts.
    pub max_memory_pressure: f64,
    /// One-minute load average at which no turn starts.
    pub max_load: f64,
}

/// One reading of the host.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    /// `MemTotal`, in bytes.
    pub memory_total: u64,
    /// `MemAvailable`, in bytes.
    pub memory_available: u64,
    /// One-minute load average.
    pub load_1m: f64,
    /// Share of all cores busy since the previous reading, 0 to 100.
    pub cpu_percent: f64,
    /// PSI avg10 values; `None` on kernels without PSI.
    pub pressure: Option<Pressure>,
}

/// Where readings of the host come from: [`ProcHost`], or numbers a test injects.
pub trait ReadHost: Send + Sync + 'static {
    /// Reads the host now.
    fn read(&self) -> anyhow::Result<Reading>;
}

/// Reads this host from `/proc`.
#[derive(Debug, Default)]
pub struct ProcHost {
    /// The last CPU sample; readings closer together than [`CPU_WINDOW`] repeat its share.
    cpu: Mutex<Option<CpuSample>>,
}

#[derive(Debug, Clone, Copy)]
struct CpuSample {
    at: Instant,
    /// Busy and total CPU time, in clock ticks.
    times: (u64, u64),
    /// The share busy since the sample before.
    percent: f64,
}

/// The shortest window the CPU share is measured over.
const CPU_WINDOW: Duration = Duration::from_secs(1);

impl ReadHost for ProcHost {
    fn read(&self) -> anyhow::Result<Reading> {
        let read = |path: &str| std::fs::read_to_string(path).with_context(|| path.to_owned());
        let meminfo = read("/proc/meminfo")?;
        let memory_total =
            cgroup::meminfo(&meminfo, "MemTotal:").context("/proc/meminfo has no MemTotal")?;
        let memory_available = cgroup::meminfo(&meminfo, "MemAvailable:")
            .context("/proc/meminfo has no MemAvailable")?;
        let load_1m = load(&read("/proc/loadavg")?).context("/proc/loadavg is malformed")?;
        let times = cpu_times(&read("/proc/stat")?).context("/proc/stat has no cpu line")?;
        let now = Instant::now();
        let mut cpu = self.cpu.lock().unwrap_or_else(PoisonError::into_inner);
        let cpu_percent = match *cpu {
            Some(sample) if now.duration_since(sample.at) < CPU_WINDOW => sample.percent,
            previous => {
                let percent = previous.map_or(0.0, |then| cpu_percent(then.times, times));
                *cpu = Some(CpuSample {
                    at: now,
                    times,
                    percent,
                });
                percent
            }
        };
        drop(cpu);
        let pressure = (|| {
            let psi =
                |resource: &str| std::fs::read_to_string(format!("/proc/pressure/{resource}")).ok();
            let (cpu, memory, io) = (psi("cpu")?, psi("memory")?, psi("io")?);
            Some(Pressure {
                cpu_some: avg10(&cpu, "some")?,
                memory_some: avg10(&memory, "some")?,
                memory_full: avg10(&memory, "full")?,
                io_some: avg10(&io, "some")?,
            })
        })();
        Ok(Reading {
            memory_total,
            memory_available,
            load_1m,
            cpu_percent,
            pressure,
        })
    }
}

/// Admits turns within the host's [`Budget`]. Cheap to share as an `Arc`.
pub struct Admission {
    shared: Arc<Shared>,
}

struct Shared {
    budget: Budget,
    host: Box<dyn ReadHost>,
    /// Whether a failed reading was logged already; later ones are not.
    read_failed: AtomicBool,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Turns holding a permit and not parked: the turns counted against the limit.
    running: u32,
    /// Each session holding a permit, with how many of its tool calls park it now.
    holders: HashMap<SessionId, u32>,
    /// Turns waiting for a permit, oldest first; a closed one was given up.
    waiting: VecDeque<(SessionId, oneshot::Sender<Permit>)>,
    /// What clients were sent last.
    published: Option<HostResources>,
}

/// Whether a turn may start now.
pub enum Ticket {
    /// It may: it runs while the permit lives.
    Admitted(Permit),
    /// It waits in line; the permit arrives once the host has room.
    Waiting(oneshot::Receiver<Permit>),
}

/// One running turn's slot; dropping it frees the slot and admits the next waiting turn.
pub struct Permit {
    /// `None` once the slot was never taken or is already freed.
    shared: Option<Arc<Shared>>,
    session: SessionId,
}

impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.take() {
            let mut state = shared.lock();
            // A parked turn's slot is free already.
            if state.holders.remove(&self.session) == Some(0) {
                state.running = state.running.saturating_sub(1);
            }
            shared.admit(&mut state);
        }
    }
}

/// A turn's slot lent out while one of its tool calls waits for children; dropping it takes the
/// slot back.
pub struct Parked {
    shared: Arc<Shared>,
    session: SessionId,
}

impl Drop for Parked {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        // Nothing to take back once the turn ended meanwhile.
        if let Some(parked) = state.holders.get_mut(&self.session) {
            *parked = parked.saturating_sub(1);
            if *parked == 0 {
                state.running += 1;
            }
        }
    }
}

impl Admission {
    /// Admission within `budget`, reading the host from `host`.
    pub fn new(budget: Budget, host: Box<dyn ReadHost>) -> Self {
        Self {
            shared: Arc::new(Shared {
                budget,
                host,
                read_failed: AtomicBool::new(false),
                state: Mutex::default(),
            }),
        }
    }

    /// The budget turns are admitted within.
    pub fn budget(&self) -> Budget {
        self.shared.budget
    }

    /// Admits `session`'s next turn now when the host has room and no turn waits; otherwise
    /// puts it in line.
    pub fn request(&self, session: &SessionId) -> Ticket {
        let shared = &self.shared;
        let mut state = shared.lock();
        // Earlier turns first, should the host have room for them now.
        shared.admit(&mut state);
        let reading = shared.read();
        if state.waiting.is_empty()
            && shared
                .budget
                .constraint(state.running, reading.as_ref())
                .is_none()
        {
            state.running += 1;
            state.holders.insert(session.clone(), 0);
            return Ticket::Admitted(Permit {
                shared: Some(Arc::clone(shared)),
                session: session.clone(),
            });
        }
        let (permit, waiting) = oneshot::channel();
        state.waiting.push_back((session.clone(), permit));
        Ticket::Waiting(waiting)
    }

    /// Lends `session`'s slot to other turns while its turn waits for its children, until the
    /// guard drops; `None` when the session holds no slot.
    pub fn park(&self, session: &SessionId) -> Option<Parked> {
        let shared = &self.shared;
        let mut state = shared.lock();
        let parked = state.holders.get_mut(session)?;
        *parked += 1;
        if *parked == 1 {
            state.running = state.running.saturating_sub(1);
            shared.admit(&mut state);
        }
        Some(Parked {
            shared: Arc::clone(shared),
            session: session.clone(),
        })
    }

    /// What about the host itself, its memory, load or pressure, keeps it from starting another
    /// turn now, whatever the turn limit says.
    pub fn host_constraint(&self) -> Option<Constraint> {
        self.shared
            .budget
            .constraint(0, self.shared.read().as_ref())
    }

    /// Why the host starts no more turns for `constraint`, for an agent or a log.
    pub fn explain(&self, constraint: Constraint) -> String {
        let budget = &self.shared.budget;
        match constraint {
            Constraint::MaxTurns => format!(
                "it already runs {} agent turns, its limit",
                budget.max_turns
            ),
            Constraint::Memory => format!(
                "less than {} MiB of memory is available",
                budget.min_memory_available / MIB
            ),
            Constraint::Load => format!(
                "its load average is above {} on {} CPU cores",
                budget.max_load, budget.cores
            ),
            Constraint::Pressure => "its processes stall waiting for memory".to_owned(),
        }
    }

    /// Admits waiting turns while the host has room.
    pub fn recheck(&self) {
        let mut state = self.shared.lock();
        self.shared.admit(&mut state);
    }

    /// The host's resources and turns as clients see them.
    pub fn resources(&self) -> HostResources {
        let state = self.shared.lock();
        self.shared.resources(&state, self.shared.read().as_ref())
    }

    /// Admits waiting turns and publishes the host's resources to `hub` when they changed,
    /// each [`RECHECK_INTERVAL`] until `shutdown`.
    pub async fn run(&self, hub: &Hub, shutdown: CancellationToken) {
        let mut tick = tokio::time::interval(RECHECK_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                _ = tick.tick() => self.publish(hub),
            }
        }
    }

    /// Admits waiting turns, then sends `hub` the host's resources if they changed.
    pub fn publish(&self, hub: &Hub) {
        let shared = &self.shared;
        let mut state = shared.lock();
        shared.admit(&mut state);
        let resources = shared.resources(&state, shared.read().as_ref());
        if state.published.as_ref() != Some(&resources) {
            state.published = Some(resources.clone());
            drop(state);
            hub.host_resources(resources);
        }
    }
}

impl Shared {
    /// Hands permits to waiting turns, oldest first, while the host has room.
    fn admit(self: &Arc<Self>, state: &mut State) {
        while let Some((_, waiting)) = state.waiting.front() {
            if waiting.is_closed() {
                state.waiting.pop_front();
                continue;
            }
            if self
                .budget
                .constraint(state.running, self.read().as_ref())
                .is_some()
            {
                return;
            }
            let Some((session, waiting)) = state.waiting.pop_front() else {
                return;
            };
            state.running += 1;
            state.holders.insert(session.clone(), 0);
            let permit = Permit {
                shared: Some(Arc::clone(self)),
                session: session.clone(),
            };
            if let Err(mut permit) = waiting.send(permit) {
                // The turn was given up meanwhile; its slot was never used. Freed here rather
                // than by the drop, which would take the lock this holds.
                permit.shared = None;
                state.running -= 1;
                state.holders.remove(&session);
            }
        }
    }

    fn resources(&self, state: &State, reading: Option<&Reading>) -> HostResources {
        let waiting = state.waiting.iter().filter(|(_, w)| !w.is_closed()).count();
        HostResources {
            cpu_cores: self.budget.cores,
            cpu_percent: reading.map_or(0.0, |r| r.cpu_percent),
            load_1m: reading.map_or(0.0, |r| r.load_1m),
            memory_total_bytes: reading.map_or(0, |r| r.memory_total),
            memory_available_bytes: reading.map_or(0, |r| r.memory_available),
            pressure: reading.and_then(|r| r.pressure.clone()),
            running_turns: state.running,
            max_turns: self.budget.max_turns,
            waiting_turns: u32::try_from(waiting).unwrap_or(u32::MAX),
            constraint: self.budget.constraint(state.running, reading),
        }
    }

    /// Reads the host; `None` when it cannot be read, logged the first time.
    fn read(&self) -> Option<Reading> {
        match self.host.read() {
            Ok(reading) => Some(reading),
            Err(err) => {
                if !self.read_failed.swap(true, Ordering::Relaxed) {
                    warn!(
                        "cannot read this host's memory and load ({err:#}); turns are \
                         admitted by the turn limit only"
                    );
                }
                None
            }
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // Every update is a single counter change or queue push or pop, so a panic mid-update
        // leaves the state consistent.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Budget {
    /// What keeps a host with `running` turns, read as `reading`, from starting another;
    /// without a reading, only the turn limit applies.
    pub fn constraint(&self, running: u32, reading: Option<&Reading>) -> Option<Constraint> {
        if running >= self.max_turns {
            return Some(Constraint::MaxTurns);
        }
        let reading = reading?;
        if reading.memory_available < self.min_memory_available {
            Some(Constraint::Memory)
        } else if reading.load_1m >= self.max_load {
            Some(Constraint::Load)
        } else if reading
            .pressure
            .as_ref()
            .is_some_and(|pressure| pressure.memory_some >= self.max_memory_pressure)
        {
            Some(Constraint::Pressure)
        } else {
            None
        }
    }
}

const MIB: u64 = 1024 * 1024;

/// The one-minute load average in `/proc/loadavg`.
fn load(loadavg: &str) -> Option<f64> {
    loadavg.split_whitespace().next()?.parse().ok()
}

/// Busy and total CPU time in `/proc/stat`'s `cpu` line, in clock ticks: `user nice system
/// idle iowait irq softirq steal`, idle and iowait not busy. Guest time is already in user.
fn cpu_times(stat: &str) -> Option<(u64, u64)> {
    let line = stat.lines().find(|line| line.starts_with("cpu "))?;
    let ticks: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .take(8)
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let total: u64 = ticks.iter().sum();
    let idle = ticks.get(3)? + ticks.get(4).copied().unwrap_or(0);
    Some((total - idle, total))
}

/// The share of CPU time busy between two `cpu_times`, 0 to 100, in tenths of a percent.
fn cpu_percent((busy_then, total_then): (u64, u64), (busy, total): (u64, u64)) -> f64 {
    let total = total.saturating_sub(total_then);
    if total == 0 {
        return 0.0;
    }
    let busy = busy.saturating_sub(busy_then) as f64 / total as f64;
    ((busy * 100.0).clamp(0.0, 100.0) * 10.0).round() / 10.0
}

/// `avg10` of the `kind` (`some` or `full`) line of a `/proc/pressure` file.
fn avg10(psi: &str, kind: &str) -> Option<f64> {
    psi.lines()
        .find_map(|line| line.strip_prefix(kind)?.strip_prefix(' '))?
        .split_whitespace()
        .find_map(|field| field.strip_prefix("avg10="))?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests;
