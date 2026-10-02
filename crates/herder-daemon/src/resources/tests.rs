use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::sync::Arc;

use herder_protocol::{Role, ServerMessage};

use super::*;
use crate::hub::Outbox;

const GIB: u64 = 1024 * 1024 * 1024;

fn host() -> Host {
    Host {
        memory_total: 16 * GIB,
        cores: 4,
        nice: 0,
    }
}

fn strings(argv: &[OsString]) -> Vec<&str> {
    argv.iter().map(|arg| arg.to_str().unwrap()).collect()
}

#[test]
fn launcher_runs_the_cli_in_a_limited_collected_scope() {
    let limits = Limits {
        cpu_weight: 50,
        memory_high: 1000,
        memory_max: 2000,
        nice: 10,
    };
    assert_eq!(
        strings(&launcher("herder-s1-7.scope", &limits)),
        [
            "systemd-run",
            "--user",
            "--scope",
            "--quiet",
            "--collect",
            "--unit=herder-s1-7.scope",
            "--property=CPUWeight=50",
            "--property=MemoryHigh=1000",
            "--property=MemoryMax=2000",
            "--property=MemorySwapMax=0",
            "--property=OOMPolicy=continue",
            "--nice=10",
            "--",
        ]
    );
}

#[test]
fn limits_are_shares_of_the_host_and_children_weigh_less() {
    let config = ResourcesConfig::default();
    let primary = config.limits(&host(), false);
    let memory_max = 16 * GIB / 100 * 40;
    assert_eq!(
        primary,
        Limits {
            cpu_weight: 100,
            memory_high: memory_max / 100 * 80,
            memory_max,
            nice: 10,
        }
    );
    let child = config.limits(&host(), true);
    assert_eq!(child.cpu_weight, 50);
    assert_eq!(child.memory_max, primary.memory_max);
    let niced = Host { nice: 15, ..host() };
    assert_eq!(config.limits(&niced, false).nice, 15);
}

#[test]
fn nice_is_read_past_a_command_name_with_spaces() {
    let stat = "42 (a b) c) S 1 42 42 0 -1 4194560 100 0 0 0 1 2 0 0 20 5 1 0 100 1000 10";
    assert_eq!(cgroup::nice(stat), Some(5));
    let own = std::fs::read_to_string("/proc/self/stat").unwrap();
    assert!(cgroup::nice(&own).is_some());
}

#[test]
fn unit_names_keep_only_what_systemd_allows() {
    assert_eq!(
        unit_name(&SessionId::new("01JABC"), 3),
        "herder-01JABC-3.scope"
    );
    assert_eq!(
        unit_name(&SessionId::new("a b/c-d"), 1),
        "herder-a_b_c_d-1.scope"
    );
}

#[test]
fn each_launch_is_a_new_scope_and_none_while_limits_are_off() {
    let session = SessionId::new("s1");
    let off = Scopes::new(ResourcesConfig::default(), host(), false);
    assert!(!off.limits_on());
    assert!(off.launch(&session, &off.limits(false)).is_empty());
    assert_eq!(off.unit(&session), None);

    let scopes = Scopes::new(ResourcesConfig::default(), host(), true);
    let first = scopes.launch(&session, &scopes.limits(false));
    let unit = scopes.unit(&session).unwrap();
    assert!(strings(&first).contains(&format!("--unit={unit}").as_str()));
    scopes.launch(&session, &scopes.limits(false));
    let second = scopes.unit(&session).unwrap();
    assert_ne!(unit, second);
    assert!(second.starts_with("herder-s1-"));
}

/// A cgroup directory as the kernel lays it out.
fn write_cgroup(dir: &Path, memory: u64, cpu_usec: u64, pids: &[u32]) {
    std::fs::write(dir.join("memory.current"), format!("{memory}\n")).unwrap();
    std::fs::write(
        dir.join("cpu.stat"),
        format!("usage_usec {cpu_usec}\nuser_usec 1\nsystem_usec 1\n"),
    )
    .unwrap();
    let procs: String = pids.iter().map(|pid| format!("{pid}\n")).collect();
    std::fs::write(dir.join("cgroup.procs"), procs).unwrap();
}

#[test]
fn a_sample_reads_memory_cpu_and_processes() {
    let dir = tempfile::tempdir().unwrap();
    write_cgroup(dir.path(), 4096, 1_500_000, &[10, 11, 12]);
    assert_eq!(
        Sample::read(dir.path()).unwrap(),
        Sample {
            memory_bytes: 4096,
            cpu_usec: 1_500_000,
            processes: 3,
        }
    );
    assert!(Sample::read(&dir.path().join("gone")).is_err());
}

#[test]
fn cpu_is_the_share_of_every_core_since_the_last_sample() {
    let at = Instant::now();
    let sample = |cpu_usec| Sample {
        memory_bytes: 1,
        cpu_usec,
        processes: 1,
    };
    let first = cgroup::usage(None, at, &sample(0), host());
    assert_eq!(first.cpu_percent, 0.0);
    // Two CPU-seconds in two seconds on four cores.
    let usage = cgroup::usage(
        Some(&(at, sample(0))),
        at + Duration::from_secs(2),
        &sample(2_000_000),
        host(),
    );
    assert_eq!(usage.cpu_percent, 25.0);
}

#[test]
fn mem_total_is_read_in_bytes() {
    let meminfo = "MemTotal:       16000000 kB\nMemFree:  1 kB\n";
    assert_eq!(cgroup::mem_total(meminfo), Some(16_000_000 * 1024));
    assert_eq!(cgroup::mem_total("MemFree: 1 kB\n"), None);
}

fn drain(outbox: &Outbox) -> Vec<ServerMessage> {
    std::iter::from_fn(|| outbox.pop()).collect()
}

#[tokio::test]
async fn the_sampler_publishes_changes_then_one_zero_once_the_scope_is_gone() {
    let session = SessionId::new("s1");
    let scopes = Scopes::new(ResourcesConfig::default(), host(), true);
    scopes.launch(&session, &scopes.limits(false));
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("herder-s1.scope");
    std::fs::create_dir(&dir).unwrap();
    write_cgroup(&dir, 4096, 0, &[10, 11]);
    scopes.lock().scopes.get_mut(&session).unwrap().cgroup = Some(dir.clone());

    let hub = Hub::default();
    let outbox = Arc::new(Outbox::default());
    hub.connect(&outbox, Role::Member);
    let usage = |memory_bytes, processes| ServerMessage::SessionResources {
        session_id: session.clone(),
        usage: SessionUsage {
            cpu_percent: 0.0,
            memory_bytes,
            processes,
            containers: Vec::new(),
        },
    };

    scopes.sample(&hub).await;
    assert_eq!(drain(&outbox), [usage(4096, 2)]);
    // Unchanged: nothing to send.
    scopes.sample(&hub).await;
    assert!(drain(&outbox).is_empty());

    std::fs::remove_dir_all(&dir).unwrap();
    scopes.sample(&hub).await;
    assert_eq!(drain(&outbox), [usage(0, 0)]);
    scopes.sample(&hub).await;
    assert!(drain(&outbox).is_empty());
    assert_eq!(scopes.unit(&session), None);
}

/// The output of `script` run by `sh` in a fresh scope with `limits`.
async fn run_in_scope(unit: &str, limits: &Limits, script: &str) -> std::process::Output {
    let argv = launcher(unit, limits);
    Command::new(&argv[0])
        .args(&argv[1..])
        .args(["sh", "-c", script])
        .kill_on_drop(true)
        .output()
        .await
        .unwrap()
}

/// A runaway child, such as a build that eats memory, is OOM-killed inside its session's
/// scope, while another session and the daemon keep running.
///
/// The agent that started it usually survives too, but not always: while the killed child's
/// memory is being released, the kernel may pick the agent as a second victim.
///
/// Needs a systemd user session with the memory controller delegated, so CI does not run it:
/// `cargo test -p herder-daemon -- --ignored runaway`. It uses at most 64 MiB.
#[tokio::test]
#[ignore = "needs a systemd user session"]
async fn a_runaway_child_is_killed_in_its_scope_and_everything_else_keeps_running() {
    let pid = std::process::id();
    // No throttling band: above `MemoryHigh` without swap a runaway crawls instead of dying,
    // which is the throttling half and would make this test wait for minutes.
    let tight = Limits {
        cpu_weight: 50,
        memory_high: 64 * 1024 * 1024,
        memory_max: 64 * 1024 * 1024,
        nice: Host::read().unwrap().nice.max(10),
    };
    let roomy = Limits {
        memory_high: 256 * 1024 * 1024,
        memory_max: 512 * 1024 * 1024,
        ..tight
    };
    let (runaway_unit, other_unit) = (
        format!("herder-test-runaway-{pid}.scope"),
        format!("herder-test-other-{pid}.scope"),
    );
    // `tail` buffers input that has no newline, so it grows until the scope's limit.
    let runaway = run_in_scope(
        &runaway_unit,
        &tight,
        "grep -o 'herder-test-runaway[^/]*' /proc/self/cgroup; \
         head -c 512M /dev/zero | tail > /dev/null; echo child=$?; echo agent alive",
    );
    let other = run_in_scope(&other_unit, &roomy, "sleep 2; echo other alive");
    let (runaway, other) = tokio::time::timeout(Duration::from_secs(60), async {
        tokio::join!(runaway, other)
    })
    .await
    .unwrap();
    let status = runaway.status;
    let stderr = String::from_utf8_lossy(&runaway.stderr).into_owned();
    let runaway = String::from_utf8_lossy(&runaway.stdout).into_owned();
    let lines: Vec<_> = runaway.lines().collect();
    let context = format!("{status:?}\n{runaway}{stderr}");
    assert_eq!(lines.first(), Some(&runaway_unit.as_str()), "{context}");
    // 128 + SIGKILL: the kernel's OOM killer, inside the scope; or the agent went with it.
    let killed = lines[1..] == ["child=137", "agent alive"]
        || (lines.len() == 1 && status.signal() == Some(nix::sys::signal::Signal::SIGKILL as i32));
    assert!(killed, "{context}");
    assert_eq!(String::from_utf8_lossy(&other.stdout), "other alive\n");
}
