//! install.sh and `herder update` against a local release dir laid out like GitHub Releases.
//! Tarballs are built with the same `tar` and `sha256sum` commands as release.yml.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TARGET: &str = match std::env::consts::ARCH.as_bytes() {
    b"x86_64" => "x86_64-unknown-linux-musl",
    b"aarch64" => "aarch64-unknown-linux-musl",
    _ => panic!("no release target for this arch"),
};

fn run(command: &mut Command) -> Output {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// Writes `<root>/download/v<version>/herder-<version>-<target>.tar.gz` and its `.sha256`.
fn publish(root: &Path, version: &str, binary: &[u8]) -> PathBuf {
    let dir = root.join(format!("download/v{version}"));
    let name = format!("herder-{version}-{TARGET}");
    std::fs::create_dir_all(dir.join(&name)).unwrap();
    let bin = dir.join(&name).join("herder");
    std::fs::write(&bin, binary).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    run(Command::new("tar")
        .current_dir(&dir)
        .args(["-czf", &format!("{name}.tar.gz"), &name]));
    let sum = run(Command::new("sha256sum")
        .current_dir(&dir)
        .arg(format!("{name}.tar.gz")));
    std::fs::write(dir.join(format!("{name}.tar.gz.sha256")), sum.stdout).unwrap();
    std::fs::remove_dir_all(dir.join(&name)).unwrap();
    dir.join(format!("{name}.tar.gz"))
}

/// Serves `root` over HTTP; `/latest` redirects to `/tag/v<latest>` like GitHub does.
fn serve(root: PathBuf, latest: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let tag_url = format!("{base}/tag/v{latest}");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut request = String::new();
            let mut reader = BufReader::new(&stream);
            reader.read_line(&mut request).unwrap();
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 2 {
                line.clear();
            }
            let path = request.split(' ').nth(1).unwrap_or("/");
            let response = if path == "/latest" {
                format!(
                    "HTTP/1.1 302 Found\r\nLocation: {tag_url}\r\nContent-Length: 0\r\n\
                     Connection: close\r\n\r\n"
                )
                .into_bytes()
            } else if path.starts_with("/tag/") {
                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
            } else if let Ok(body) = std::fs::read(root.join(path.trim_start_matches('/'))) {
                let mut response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .into_bytes();
                response.extend(body);
                response
            } else {
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
            };
            stream.write_all(&response).unwrap();
        }
    });
    base
}

/// Copies `from` to the executable `to` in a `cp` process, never through a file this process
/// opens for writing.
///
/// Tests run on many threads. A file written here stays open for writing in every child that
/// another thread forks until that child execs, and running the file meanwhile fails with
/// `ETXTBSY` ("Text file busy"). `cp` holds the only writable descriptor and has exited by the
/// time the copy is run.
fn install(from: &Path, to: &Path) {
    run(Command::new("cp").arg(from).arg(to));
}

/// A copy of the built herder binary, so `herder update` can replace it.
fn installed_herder(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let exe = bin.join("herder");
    install(Path::new(env!("CARGO_BIN_EXE_herder")), &exe);
    exe
}

/// Installs and runs a small executable on many threads at once, as the tests here do; with
/// the copy written in-process, some runs fail with "Text file busy".
#[test]
fn executables_installed_while_other_threads_spawn_run() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    std::fs::write(&source, FAKE_RELEASE).unwrap();
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::thread::scope(|scope| {
        for thread in 0..8 {
            let (tmp, source) = (tmp.path(), &source);
            scope.spawn(move || {
                for n in 0..25 {
                    let exe = tmp.join(format!("herder-{thread}-{n}"));
                    install(source, &exe);
                    let output = run(&mut Command::new(&exe));
                    assert_eq!(output.stdout, b"herder 9.9.9\n");
                }
            });
        }
    });
}

const FAKE_RELEASE: &[u8] = b"#!/bin/sh\necho 'herder 9.9.9'\n";

fn update(exe: &Path, base: &str, args: &[&str]) -> Output {
    Command::new(exe)
        .arg("update")
        .args(args)
        .env("HERDER_DOWNLOAD_BASE", base)
        .output()
        .unwrap()
}

#[test]
fn update_installs_the_latest_release() {
    let tmp = tempfile::tempdir().unwrap();
    let releases = tmp.path().join("releases");
    publish(&releases, "9.9.9", FAKE_RELEASE);
    let base = serve(releases, "9.9.9");
    let exe = installed_herder(tmp.path());

    let output = update(&exe, &base, &[]);

    assert!(output.status.success(), "{output:?}");
    assert_eq!(std::fs::read(&exe).unwrap(), FAKE_RELEASE);
    assert_eq!(
        String::from_utf8(run(&mut Command::new(&exe)).stdout).unwrap(),
        "herder 9.9.9\n"
    );
}

#[test]
fn update_refuses_a_tampered_tarball() {
    let tmp = tempfile::tempdir().unwrap();
    let releases = tmp.path().join("releases");
    let tarball = publish(&releases, "9.9.9", FAKE_RELEASE);
    let mut data = std::fs::read(&tarball).unwrap();
    data.push(0);
    std::fs::write(&tarball, data).unwrap();
    let base = serve(releases, "9.9.9");
    let exe = installed_herder(tmp.path());

    let output = update(&exe, &base, &["--version", "9.9.9"]);

    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("checksum mismatch"),
        "{output:?}"
    );
    let original = std::fs::read(env!("CARGO_BIN_EXE_herder")).unwrap();
    assert!(std::fs::read(&exe).unwrap() == original, "binary changed");
}

fn install_sh(home: &Path, envs: &[(&str, &str)]) -> Output {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../install.sh");
    Command::new("sh")
        .arg(script)
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .envs(envs.iter().copied())
        .output()
        .unwrap()
}

#[test]
fn install_sh_installs_a_pinned_version_from_a_file_url() {
    let tmp = tempfile::tempdir().unwrap();
    let releases = tmp.path().join("releases");
    publish(&releases, "9.9.9", FAKE_RELEASE);
    let home = tmp.path().join("home");
    let base = format!("file://{}", releases.display());

    let output = install_sh(
        &home,
        &[
            ("HERDER_DOWNLOAD_BASE", &base),
            ("HERDER_VERSION", "v9.9.9"),
        ],
    );

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("installed herder 9.9.9"), "{stdout}");
    assert!(stdout.contains("is not on your PATH"), "{stdout}");
    let installed = home.join(".local/bin/herder");
    let version = run(&mut Command::new(&installed));
    assert_eq!(String::from_utf8(version.stdout).unwrap(), "herder 9.9.9\n");
}

#[test]
fn install_sh_resolves_the_latest_release() {
    let tmp = tempfile::tempdir().unwrap();
    let releases = tmp.path().join("releases");
    publish(&releases, "9.9.9", FAKE_RELEASE);
    let base = serve(releases, "9.9.9");
    let home = tmp.path().join("home");

    let output = install_sh(&home, &[("HERDER_DOWNLOAD_BASE", &base)]);

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read(home.join(".local/bin/herder")).unwrap(),
        FAKE_RELEASE
    );
}

#[test]
fn install_sh_refuses_a_tampered_tarball() {
    let tmp = tempfile::tempdir().unwrap();
    let releases = tmp.path().join("releases");
    let tarball = publish(&releases, "9.9.9", FAKE_RELEASE);
    std::fs::write(tarball.with_extension("gz.sha256"), "0000  x\n").unwrap();
    let home = tmp.path().join("home");
    let base = format!("file://{}", releases.display());

    let output = install_sh(
        &home,
        &[("HERDER_DOWNLOAD_BASE", &base), ("HERDER_VERSION", "9.9.9")],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("checksum mismatch"),
        "{output:?}"
    );
    assert!(!home.join(".local/bin/herder").exists());
}
