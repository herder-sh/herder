//! Reading a scope's usage from its cgroup v2 directory.

use std::io;
use std::path::Path;
use std::time::Instant;

use herder_protocol::SessionUsage;

use super::Host;

/// One reading of a cgroup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    /// `memory.current`: bytes charged to the cgroup.
    pub memory_bytes: u64,
    /// `usage_usec` of `cpu.stat`: CPU time used so far, in microseconds.
    pub cpu_usec: u64,
    /// Lines of `cgroup.procs`: processes in the cgroup.
    pub processes: u32,
}

impl Sample {
    /// Reads the cgroup at `dir`; fails once the cgroup is gone.
    pub fn read(dir: &Path) -> io::Result<Self> {
        let invalid = |file: &str| io::Error::new(io::ErrorKind::InvalidData, file.to_owned());
        let memory_bytes = std::fs::read_to_string(dir.join("memory.current"))?
            .trim()
            .parse()
            .map_err(|_| invalid("memory.current"))?;
        let cpu_usec = std::fs::read_to_string(dir.join("cpu.stat"))?
            .lines()
            .find_map(|line| line.strip_prefix("usage_usec "))
            .and_then(|value| value.trim().parse().ok())
            .ok_or_else(|| invalid("cpu.stat"))?;
        let processes = std::fs::read_to_string(dir.join("cgroup.procs"))?
            .lines()
            .count();
        Ok(Self {
            memory_bytes,
            cpu_usec,
            processes: u32::try_from(processes).unwrap_or(u32::MAX),
        })
    }
}

/// The usage `sample`, taken at `now`, shows; CPU is the share of `host`'s CPUs used since
/// `last`, and 0 for a first reading.
pub(super) fn usage(
    last: Option<&(Instant, Sample)>,
    now: Instant,
    sample: &Sample,
    host: Host,
) -> SessionUsage {
    let cpu_percent = last.map_or(0.0, |(at, last)| {
        let wall = now.duration_since(*at).as_secs_f64() * f64::from(host.cores.max(1));
        let used = sample.cpu_usec.saturating_sub(last.cpu_usec) as f64 / 1e6;
        if wall > 0.0 {
            // Whole tenths of a percent, so noise below that is not a change worth sending.
            ((used / wall * 100.0).clamp(0.0, 100.0) * 10.0).round() / 10.0
        } else {
            0.0
        }
    });
    SessionUsage {
        cpu_percent,
        memory_bytes: sample.memory_bytes,
        processes: sample.processes,
        containers: Vec::new(),
    }
}

/// The usage of a session with nothing running.
pub(super) fn zero() -> SessionUsage {
    SessionUsage {
        cpu_percent: 0.0,
        memory_bytes: 0,
        processes: 0,
        containers: Vec::new(),
    }
}

/// `MemTotal` of `/proc/meminfo`, in bytes.
pub(super) fn mem_total(meminfo: &str) -> Option<u64> {
    self::meminfo(meminfo, "MemTotal:")
}

/// The field starting with `key` of `/proc/meminfo`, in bytes.
pub(super) fn meminfo(meminfo: &str, key: &str) -> Option<u64> {
    let kib: u64 = meminfo
        .lines()
        .find_map(|line| line.strip_prefix(key))?
        .trim()
        .strip_suffix("kB")?
        .trim()
        .parse()
        .ok()?;
    kib.checked_mul(1024)
}

/// The niceness in `/proc/<pid>/stat`: its 19th field, counting from the pid. The command name
/// in field 2 may hold spaces and parentheses, so fields are counted after its last `)`.
pub(super) fn nice(stat: &str) -> Option<i8> {
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(16)?.parse().ok()
}
