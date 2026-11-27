//! `cloudflared`, as the hub runs it for its managed tunnel: a pinned release ([`VERSION`]),
//! downloaded from GitHub and checked against its pinned SHA-256, run as the hub's child
//! with nothing of the hub's environment but the tunnel's token, and started again when it
//! stops.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use herder_protocol::SecretValue;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::settings::PublicAddresses;
use crate::update::release;

/// The cloudflared release the hub runs.
pub const VERSION: &str = "2026.11.2";

/// Where cloudflared's releases are downloaded from.
const RELEASES: &str = "https://github.com/cloudflare/cloudflared/releases/download";

/// The binary, in the managed tunnel's dir and in a macOS release's tarball.
const BINARY: &str = "cloudflared";

/// Longest wait before starting a cloudflared that stopped again.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// A cloudflared that ran this long had started fine; the next stop waits from scratch.
const RAN_FINE: Duration = Duration::from_secs(60);

/// What gets the cloudflared binary: [`download`], or a stub in tests.
pub type Download = fn() -> Result<Vec<u8>>;

/// What cloudflared runs: the tunnel's token, and the hostname it serves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub token: SecretValue,
    pub hostname: String,
}

/// What cloudflared is doing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Running {
    /// Nothing to run.
    #[default]
    Stopped,
    /// Being downloaded or started.
    Starting,
    /// Running.
    Running,
    /// Stopped or never started, and started again soon; why.
    Failed(String),
}

/// What cloudflared is doing, kept current by [`supervise`].
pub type Status = Arc<Mutex<Running>>;

/// The pinned release's asset for `os` and `arch`, its SHA-256 from the release, and
/// whether it is a tarball rather than the bare binary.
fn asset(os: &str, arch: &str) -> Result<(&'static str, &'static str, bool)> {
    Ok(match (os, arch) {
        ("linux", "x86_64") => (
            "cloudflared-linux-amd64",
            "56f13335143ec0e22772963290683a84a04114a35ec64cbc794a4d39ff9bb8cf",
            false,
        ),
        ("linux", "aarch64") => (
            "cloudflared-linux-arm64",
            "04a7b7f08bce85d56b26ffe1fdbfbb21a074495a363d16f0783c40e25b5c5281",
            false,
        ),
        ("macos", "aarch64") => (
            "cloudflared-darwin-arm64.tgz",
            "f6d58eeec91ad1ec5fd1f7e4a587303772929b2625be5d24068033851c36ff55",
            true,
        ),
        ("macos", "x86_64") => (
            "cloudflared-darwin-amd64.tgz",
            "6153d9ec629174882a3adbff2f7dc5a24b4f3bdd7ccc38eaa84fba26eb890311",
            true,
        ),
        (os, arch) => bail!("cloudflared has no release build for {arch} {os}"),
    })
}

/// Downloads the pinned release for this machine, checks it against its pinned checksum and
/// returns the binary.
pub fn download() -> Result<Vec<u8>> {
    let (asset, sha256, packed) = asset(std::env::consts::OS, std::env::consts::ARCH)?;
    let data = release::get(&format!("{RELEASES}/{VERSION}/{asset}"))?;
    unpack(&data, sha256, packed).with_context(|| format!("verifying {asset}"))
}

/// The binary in `data`, once its SHA-256 is `sha256`.
fn unpack(data: &[u8], sha256: &str, packed: bool) -> Result<Vec<u8>> {
    release::verify_sha256(data, sha256)?;
    if packed {
        release::extract_file(data, BINARY)
    } else {
        Ok(data.to_vec())
    }
}

/// The binary in `dir`, from `download` unless the pinned release is there.
pub fn install(dir: &Path, download: Download) -> Result<PathBuf> {
    let binary = dir.join(BINARY);
    let version = dir.join("cloudflared.version");
    let installed = std::fs::read_to_string(&version).unwrap_or_default();
    if binary.is_file() && installed.trim() == VERSION {
        return Ok(binary);
    }
    info!(version = VERSION, "downloading cloudflared");
    release::replace_exe(&binary, &download()?)?;
    crate::config::write_atomically(&version, format!("{VERSION}\n").as_bytes())
        .with_context(|| format!("writing {}", version.display()))?;
    info!(version = VERSION, binary = %binary.display(), "cloudflared installed");
    Ok(binary)
}

/// Runs cloudflared from `dir` for whatever `run` says, until `shutdown`: none while it says
/// nothing, and a new one each time it changes. Advertises the tunnel's hostname among
/// `public`'s while it runs, and says what it does in `status`.
pub async fn supervise(
    dir: PathBuf,
    download: Download,
    mut run: watch::Receiver<Option<Run>>,
    public: Arc<PublicAddresses>,
    status: Status,
    shutdown: CancellationToken,
) {
    let say = |now: Running| *status.lock().unwrap_or_else(PoisonError::into_inner) = now;
    let mut backoff = Duration::from_secs(1);
    loop {
        let current = run.borrow_and_update().clone();
        let Some(current) = current else {
            public.set_managed(None);
            say(Running::Stopped);
            tokio::select! {
                () = shutdown.cancelled() => return,
                changed = run.changed() => if changed.is_err() { return },
            }
            continue;
        };
        public.set_managed(Some(format!("wss://{}", current.hostname)));
        say(Running::Starting);
        let wait = match start(&dir, download, &current).await {
            Ok(mut child) => {
                let pid = child.id().unwrap_or_default();
                say(Running::Running);
                info!(pid, hostname = current.hostname, "cloudflared started");
                let started = Instant::now();
                tokio::select! {
                    () = shutdown.cancelled() => {
                        stop(&mut child).await;
                        return;
                    }
                    changed = run.changed() => {
                        stop(&mut child).await;
                        if changed.is_err() {
                            return;
                        }
                        backoff = Duration::from_secs(1);
                        continue;
                    }
                    exited = child.wait() => {
                        if started.elapsed() >= RAN_FINE {
                            backoff = Duration::from_secs(1);
                        }
                        let why = match exited {
                            Ok(status) => format!("cloudflared stopped ({status})"),
                            Err(err) => format!("cloudflared could not be watched: {err}"),
                        };
                        warn!("{why}; starting it again in {}s", backoff.as_secs());
                        say(Running::Failed(why));
                        backoff
                    }
                }
            }
            Err(err) => {
                warn!("cannot start cloudflared: {err:#}");
                say(Running::Failed(format!(
                    "cannot start cloudflared: {err:#}"
                )));
                backoff
            }
        };
        backoff = (backoff * 2).min(MAX_BACKOFF);
        tokio::select! {
            () = shutdown.cancelled() => return,
            changed = run.changed() => if changed.is_err() { return },
            () = tokio::time::sleep(wait) => {}
        }
    }
}

/// Starts cloudflared for `run`, with the token in its environment, where `ps` does not show
/// it, and none of the hub's own secrets there; its log goes to the hub's.
async fn start(dir: &Path, download: Download, run: &Run) -> Result<tokio::process::Child> {
    let dir = dir.to_owned();
    let binary = tokio::task::spawn_blocking(move || install(&dir, download))
        .await
        .context("the cloudflared download panicked")??;
    let mut child = Command::new(&binary)
        .args(["tunnel", "--no-autoupdate", "run"])
        .env_clear()
        .env("TUNNEL_TOKEN", run.token.expose())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("running {}", binary.display()))?;
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                info!(target: "cloudflared", "{line}");
            }
        });
    }
    Ok(child)
}

/// Stops `child` and waits for it.
async fn stop(child: &mut tokio::process::Child) {
    if let Err(err) = child.kill().await {
        warn!("cannot stop cloudflared: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_is_taken_only_with_its_pinned_checksum() {
        // `printf hello | sha256sum`
        let hello = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert_eq!(unpack(b"hello", hello, false).unwrap(), b"hello");
        assert!(unpack(b"hellO", hello, false).is_err());

        let tarball = release::tarball(&[("cloudflared", b"#!/bin/sh\n")]);
        let sha256 = release::hex(&sha2::Sha256::digest(&tarball));
        assert_eq!(unpack(&tarball, &sha256, true).unwrap(), b"#!/bin/sh\n");
    }

    #[test]
    fn every_release_target_has_an_asset() {
        for (os, arch) in [
            ("linux", "x86_64"),
            ("linux", "aarch64"),
            ("macos", "aarch64"),
            ("macos", "x86_64"),
        ] {
            let (asset, sha256, _) = asset(os, arch).unwrap();
            assert!(asset.starts_with("cloudflared-"));
            assert_eq!(sha256.len(), 64);
        }
        assert!(asset("windows", "x86_64").is_err());
    }

    fn no_download() -> Result<Vec<u8>> {
        bail!("downloaded")
    }

    #[test]
    fn the_pinned_release_is_downloaded_once() {
        fn fake() -> Result<Vec<u8>> {
            Ok(b"#!/bin/sh\n".to_vec())
        }
        let dir = tempfile::tempdir().unwrap();
        let binary = install(dir.path(), fake).unwrap();
        assert_eq!(std::fs::read(&binary).unwrap(), b"#!/bin/sh\n");
        assert_eq!(install(dir.path(), no_download).unwrap(), binary);
    }

    use sha2::Digest;
}
