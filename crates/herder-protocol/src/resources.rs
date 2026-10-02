//! Resource usage of a host and of its sessions, so clients can show what loads a machine.
//!
//! Ephemeral, never journaled: the daemon pushes the current figures to every connection and
//! a client keeps the latest it received.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Load and admission state of the daemon's host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HostResources {
    /// Logical CPU cores.
    pub cpu_cores: u32,
    /// Share of all cores busy over the last sample, from 0 to 100.
    pub cpu_percent: f64,
    /// One-minute load average.
    pub load_1m: f64,
    /// Physical memory.
    pub memory_total_bytes: u64,
    /// Memory available to new work without swapping, as the kernel estimates it.
    pub memory_available_bytes: u64,
    /// Pressure stall information; absent when the kernel does not report it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressure: Option<Pressure>,
    /// Turns running on this host now.
    pub running_turns: u32,
    /// Most turns the host runs at once; further turns wait for capacity.
    pub max_turns: u32,
    /// Turns waiting for capacity, in sessions whose status is `waiting_for_capacity`.
    pub waiting_turns: u32,
    /// What keeps the host from starting another turn now; absent while it would start one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constraint: Option<Constraint>,
}

/// Share of the last 10 seconds in which tasks stalled on a resource, each from 0 to 100.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Pressure {
    /// Some tasks waited for CPU.
    pub cpu_some: f64,
    /// Some tasks waited for memory.
    pub memory_some: f64,
    /// Every non-idle task waited for memory at once; the sign of a machine about to freeze.
    pub memory_full: f64,
    /// Some tasks waited for I/O.
    pub io_some: f64,
}

/// Why a host starts no more turns for now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Constraint {
    /// `max_turns` turns already run.
    MaxTurns,
    /// Too little memory is available.
    Memory,
    /// The load average is too high for the cores.
    Load,
    /// Tasks stall too often on CPU, memory or I/O.
    Pressure,
}

/// What one session's processes and containers use.
///
/// A session with nothing running gets one message with no processes and no containers, and
/// no more until something starts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionUsage {
    /// Share of the host's total CPU used by the session's processes, from 0 to 100.
    pub cpu_percent: f64,
    /// Memory used by the session's processes.
    pub memory_bytes: u64,
    /// Processes the session runs: its agent and everything the agent started.
    pub processes: u32,
    /// Docker containers started from the session's worktree.
    pub containers: Vec<Container>,
}

/// A Docker container tracked for a session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Container {
    /// Docker's container id.
    pub id: String,
    /// Container name, without Docker's leading slash.
    pub name: String,
    /// Compose project the container belongs to; absent when Compose did not start it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compose_project: Option<String>,
    /// Image the container runs, as named when it started.
    pub image: String,
    /// Lifecycle state.
    pub state: ContainerState,
}

/// Lifecycle state of a Docker container, in Docker's naming.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContainerState {
    /// Created, never started.
    Created,
    /// Running.
    Running,
    /// Paused.
    Paused,
    /// Restarting under its restart policy.
    Restarting,
    /// Being removed.
    Removing,
    /// Stopped.
    Exited,
    /// Failed to stop or be removed cleanly.
    Dead,
}
