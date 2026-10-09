//! End-to-end: the built binary starts, writes its data dir and stops cleanly on SIGTERM.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

const TIMEOUT: Duration = Duration::from_secs(20);

fn write_config(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("daemon.toml");
    let data_dir = dir.join("data");
    std::fs::write(
        &path,
        format!(
            "listen = \"127.0.0.1:0\"\ndata_dir = {:?}\n\n[log]\nformat = \"json\"\n\n[projects]\ndir = {:?}\n",
            data_dir.to_str().unwrap(),
            dir.join("Projects").to_str().unwrap()
        ),
    )
    .unwrap();
    path
}

fn spawn(config: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_herder"))
        .args(["daemon", "--config"])
        .arg(config)
        .env_remove("HERDER_CONFIG")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// Forwards the child's stderr lines to a channel so reads can time out.
fn stderr_lines(child: &mut Child) -> mpsc::Receiver<String> {
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

fn wait_for_line(lines: &mpsc::Receiver<String>, needle: &str) -> String {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = lines
            .recv_timeout(left)
            .unwrap_or_else(|_| panic!("no log line containing {needle:?}"));
        if line.contains(needle) {
            return line;
        }
    }
}

fn wait_with_timeout(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            panic!("daemon did not exit");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn sigterm(child: &Child) {
    kill(
        Pid::from_raw(i32::try_from(child.id()).unwrap()),
        Signal::SIGTERM,
    )
    .unwrap();
}

#[test]
fn starts_writes_data_dir_and_stops_on_sigterm() {
    let tmp = tempfile::tempdir().unwrap();
    let config = write_config(tmp.path());
    let mut child = spawn(&config);
    let lines = stderr_lines(&mut child);

    let started = wait_for_line(&lines, "herder daemon started");
    let data = tmp.path().join("data");
    for sub in ["db", "tls", "sessions"] {
        assert!(data.join(sub).is_dir(), "{sub} missing");
    }
    let host_id = std::fs::read_to_string(data.join("host-id")).unwrap();
    assert!(started.contains(host_id.trim()), "{started}");
    // Port 0: the daemon logs the port it actually bound.
    assert!(started.contains("127.0.0.1:"), "{started}");
    assert!(started.contains("tls_fingerprint"), "{started}");

    sigterm(&child);
    let status = wait_with_timeout(&mut child);
    assert_eq!(status.code(), Some(0), "{status}");
    wait_for_line(&lines, "herder daemon stopped");

    // A restart reuses the same host id.
    let mut child = spawn(&config);
    let lines = stderr_lines(&mut child);
    let started = wait_for_line(&lines, "herder daemon started");
    assert!(started.contains(host_id.trim()), "{started}");
    sigterm(&child);
    assert_eq!(wait_with_timeout(&mut child).code(), Some(0));
}

#[test]
fn second_daemon_on_the_same_data_dir_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let config = write_config(tmp.path());
    let mut first = spawn(&config);
    let first_lines = stderr_lines(&mut first);
    wait_for_line(&first_lines, "herder daemon started");

    let mut second = spawn(&config);
    let second_lines = stderr_lines(&mut second);
    let status = wait_with_timeout(&mut second);
    assert_eq!(status.code(), Some(1), "{status}");
    let message = wait_for_line(&second_lines, "already using the data dir");
    assert!(
        message.contains(&format!("pid {}", first.id())),
        "{message}"
    );

    sigterm(&first);
    assert_eq!(wait_with_timeout(&mut first).code(), Some(0));
}

#[test]
fn unknown_config_key_fails_to_start() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("daemon.toml");
    std::fs::write(&config, "bogus = 1\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_herder"))
        .args(["daemon", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown field"));
}

fn pair(config: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_herder"))
        .args(["pair", "--config"])
        .arg(config)
        .args(args)
        .env_remove("HERDER_CONFIG")
        .output()
        .unwrap()
}

#[test]
fn pair_mints_a_code_from_the_running_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let config = write_config(tmp.path());

    let output = pair(&config, &[]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("is it running?"), "{stderr}");

    let mut child = spawn(&config);
    let lines = stderr_lines(&mut child);
    let started = wait_for_line(&lines, "herder daemon started");
    let started: serde_json::Value = serde_json::from_str(&started).unwrap();
    let fingerprint = started["fields"]["tls_fingerprint"].as_str().unwrap();

    let output = pair(&config, &["--user", "alice"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(stdout.contains("as alice (owner)"), "{stdout}");
    assert!(
        stdout.contains(&format!("fingerprint  {fingerprint}")),
        "{stdout}"
    );
    assert!(stdout.contains("address      127.0.0.1:"), "{stdout}");
    assert!(
        stdout.contains("herder://pair?host=127.0.0.1%3A"),
        "{stdout}"
    );
    assert!(
        stdout.contains('▀') || stdout.contains('▄'),
        "no QR code: {stdout}"
    );

    let output = pair(&config, &["--user", "bob", "--role", "member"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("first user"));
    let output = pair(&config, &["--list"]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("no paired devices"));
    let output = pair(&config, &["--revoke", "nope"]);
    assert_eq!(output.status.code(), Some(1));

    sigterm(&child);
    assert_eq!(wait_with_timeout(&mut child).code(), Some(0));
}

fn connect(client_config: &Path, link: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_herder"))
        .arg("connect")
        .arg(link)
        .env("XDG_CONFIG_HOME", client_config)
        .output()
        .unwrap()
}

#[test]
fn connect_pairs_this_device_with_two_daemons() {
    let tmp = tempfile::tempdir().unwrap();
    let client_config = tmp.path().join("client");
    let mut daemons = Vec::new();
    for name in ["one", "two"] {
        let dir = tmp.path().join(name);
        std::fs::create_dir(&dir).unwrap();
        let config = write_config(&dir);
        let mut child = spawn(&config);
        let lines = stderr_lines(&mut child);
        wait_for_line(&lines, "herder daemon started");
        // Keep reading its log, or the daemon blocks once the pipe fills.
        daemons.push((config, child, lines));
    }

    let mut fingerprints = Vec::new();
    for (config, ..) in &daemons {
        let output = pair(config, &["--user", "alice"]);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(stdout.contains("herder connect"), "{stdout}");
        let link = stdout
            .lines()
            .find(|line| line.starts_with("herder://pair?"))
            .unwrap();
        let fingerprint = stdout
            .lines()
            .find_map(|line| line.trim().strip_prefix("fingerprint  "))
            .unwrap()
            .to_owned();

        let output = connect(&client_config, link);
        let out = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{out}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(out.starts_with("paired with "), "{out}");
        assert!(
            out.contains(&format!("fingerprint  {fingerprint}")),
            "{out}"
        );

        // The code works once.
        let output = connect(&client_config, link);
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("pairing failed"), "{stderr}");

        let output = pair(config, &["--list"]);
        let devices = String::from_utf8_lossy(&output.stdout);
        assert!(devices.contains("herder-cli/"), "{devices}");
        fingerprints.push(fingerprint);
    }

    // Both machines are in this device's profile, each with its own pinned certificate.
    let profile: serde_json::Value =
        serde_json::from_slice(&std::fs::read(client_config.join("herder/machines.json")).unwrap())
            .unwrap();
    let pinned: Vec<&str> = profile["machines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|machine| machine["fingerprint"].as_str().unwrap())
        .collect();
    assert_eq!(pinned, fingerprints);

    let output = connect(&client_config, "https://example.com");
    assert_eq!(output.status.code(), Some(1));
    for (_, child, _) in &mut daemons {
        sigterm(child);
        assert_eq!(wait_with_timeout(child).code(), Some(0));
    }
}
