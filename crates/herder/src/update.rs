//! `herder update`: replace this binary with a release from GitHub.
//!
//! Releases live at `$HERDER_DOWNLOAD_BASE` (default: the GitHub Releases of herder-sh/herder):
//! `<base>/latest` redirects to `<base>/tag/v<version>`, and assets are served from
//! `<base>/download/v<version>/herder-<version>-<target>.tar.gz` plus a `.sha256` file.
//! install.sh reads the same layout.

use std::io::{IsTerminal, Read, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use semver::Version;
use sha2::{Digest, Sha256};

use crate::service;

const DEFAULT_DOWNLOAD_BASE: &str = "https://github.com/herder-sh/herder/releases";

pub struct Args {
    pub version: Option<String>,
    pub yes: bool,
    pub allow_downgrade: bool,
}

pub fn run(args: Args) -> Result<()> {
    let current = Version::parse(env!("CARGO_PKG_VERSION")).context("parsing own version")?;
    let target = target(std::env::consts::ARCH)?;
    let base = std::env::var("HERDER_DOWNLOAD_BASE")
        .ok()
        .filter(|base| !base.is_empty())
        .unwrap_or_else(|| DEFAULT_DOWNLOAD_BASE.to_owned());
    let base = base.trim_end_matches('/');

    let wanted = match &args.version {
        Some(version) => parse_version(version)?,
        None => latest(base)?,
    };
    if !should_install(&current, &wanted, args.allow_downgrade)? {
        println!("herder {current} is already installed");
        return Ok(());
    }

    let asset = asset_name(&wanted, target);
    let url = format!("{base}/download/v{wanted}/{asset}");
    println!("downloading {url}");
    let tarball = get(&url)?;
    let checksum = String::from_utf8(get(&format!("{url}.sha256"))?)
        .with_context(|| format!("{asset}.sha256 is not text"))?;
    verify_sha256(&tarball, &checksum).with_context(|| format!("verifying {asset}"))?;
    let binary = extract_binary(&tarball, &wanted, target)?;

    let exe = std::env::current_exe().context("finding the herder binary")?;
    replace_exe(&exe, &binary)?;
    println!("updated {} from {current} to {wanted}", exe.display());

    if service::is_active() {
        if args.yes || confirm("the herder service is running the old version; restart it now?")? {
            service::restart()?;
        } else {
            println!("restart it later with: herder service restart");
        }
    }
    Ok(())
}

/// The release target for this machine's CPU. Releases are static musl builds.
fn target(arch: &str) -> Result<&'static str> {
    match arch {
        "x86_64" => Ok("x86_64-unknown-linux-musl"),
        "aarch64" => Ok("aarch64-unknown-linux-musl"),
        other => bail!("herder has no release build for {other}"),
    }
}

/// Accepts `1.2.3` and `v1.2.3`.
fn parse_version(text: &str) -> Result<Version> {
    let text = text.trim();
    Version::parse(text.strip_prefix('v').unwrap_or(text))
        .with_context(|| format!("{text:?} is not a version like 1.2.3"))
}

/// Whether to install `wanted` over `current`: newer installs, equal is a no-op, older needs
/// `allow_downgrade`.
fn should_install(current: &Version, wanted: &Version, allow_downgrade: bool) -> Result<bool> {
    if wanted == current {
        return Ok(false);
    }
    if wanted < current && !allow_downgrade {
        bail!(
            "{wanted} is older than the installed {current}; \
             pass --allow-downgrade to install it anyway"
        );
    }
    Ok(true)
}

fn asset_name(version: &Version, target: &str) -> String {
    format!("herder-{version}-{target}.tar.gz")
}

/// The newest release: `<base>/latest` redirects to `<base>/tag/v<version>`.
fn latest(base: &str) -> Result<Version> {
    let url = format!("{base}/latest");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .max_redirects(0)
        .build()
        .into();
    let response = agent
        .get(&url)
        .call()
        .with_context(|| format!("fetching {url}"))?;
    let location = response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok());
    match location.map(version_from_tag_url) {
        Some(Ok(version)) => Ok(version),
        _ => bail!("no herder release is published at {base}"),
    }
}

fn version_from_tag_url(url: &str) -> Result<Version> {
    let Some((_, tag)) = url.rsplit_once("/tag/") else {
        bail!("{url} is not a release tag URL");
    };
    parse_version(tag)
}

fn get(url: &str) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    ureq::get(url)
        .call()
        .with_context(|| format!("downloading {url}"))?
        .into_body()
        .into_reader()
        .read_to_end(&mut body)
        .with_context(|| format!("downloading {url}"))?;
    Ok(body)
}

/// Checks `data` against a `sha256sum`-style line: `<hex digest>  <file name>`.
fn verify_sha256(data: &[u8], checksum_file: &str) -> Result<()> {
    let expected = checksum_file
        .split_whitespace()
        .next()
        .context("checksum file is empty")?;
    let actual = hex(&Sha256::digest(data));
    if !expected.eq_ignore_ascii_case(&actual) {
        bail!("checksum mismatch: expected {expected}, got {actual}");
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Reads `herder-<version>-<target>/herder` out of a release tarball.
fn extract_binary(tarball: &[u8], version: &Version, target: &str) -> Result<Vec<u8>> {
    let wanted = format!("herder-{version}-{target}/herder");
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tarball));
    for entry in archive.entries().context("reading the release tarball")? {
        let mut entry = entry.context("reading the release tarball")?;
        if entry.header().entry_type().is_file()
            && entry.path().context("reading the release tarball")? == Path::new(&wanted)
        {
            let mut binary = Vec::new();
            entry
                .read_to_end(&mut binary)
                .context("reading the release tarball")?;
            return Ok(binary);
        }
    }
    bail!("the release tarball has no {wanted}")
}

/// Replaces `exe` with `binary` atomically: a crash leaves either the old or the new binary,
/// never a partial one. The temp file sits in the same dir so the rename stays on one fs.
fn replace_exe(exe: &Path, binary: &[u8]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let dir = exe
        .parent()
        .context("the herder binary has no parent dir")?;
    let mut temp = tempfile::Builder::new()
        .prefix(".herder-update-")
        .permissions(std::fs::Permissions::from_mode(0o755))
        .tempfile_in(dir)
        .with_context(|| format!("cannot write to {}", dir.display()))?;
    temp.write_all(binary)
        .and_then(|()| temp.as_file().sync_all())
        .with_context(|| format!("writing the new binary to {}", dir.display()))?;
    temp.persist(exe)
        .with_context(|| format!("replacing {}", exe.display()))?;
    Ok(())
}

fn confirm(question: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        return Ok(false);
    }
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn targets_are_static_musl_builds() {
        assert_eq!(target("x86_64").unwrap(), "x86_64-unknown-linux-musl");
        assert_eq!(target("aarch64").unwrap(), "aarch64-unknown-linux-musl");
        assert!(target("riscv64").is_err());
    }

    #[test]
    fn versions_parse_with_or_without_v() {
        assert_eq!(parse_version("v1.2.3").unwrap(), v("1.2.3"));
        assert_eq!(parse_version("1.2.3\n").unwrap(), v("1.2.3"));
        assert!(parse_version("latest").is_err());
    }

    #[test]
    fn newer_installs_equal_skips_older_needs_flag() {
        assert!(should_install(&v("0.1.0"), &v("0.2.0"), false).unwrap());
        assert!(should_install(&v("0.9.0"), &v("0.10.0"), false).unwrap());
        assert!(!should_install(&v("0.2.0"), &v("0.2.0"), false).unwrap());
        assert!(should_install(&v("0.2.0-rc.1"), &v("0.2.0"), false).unwrap());

        let err = should_install(&v("0.2.0"), &v("0.1.0"), false).unwrap_err();
        assert!(err.to_string().contains("--allow-downgrade"), "{err}");
        assert!(should_install(&v("0.2.0"), &v("0.1.0"), true).unwrap());
    }

    #[test]
    fn latest_version_comes_from_the_tag_url() {
        let url = "https://github.com/herder-sh/herder/releases/tag/v0.3.1";
        assert_eq!(version_from_tag_url(url).unwrap(), v("0.3.1"));
        // With no release published, GitHub redirects to the release list instead.
        assert!(version_from_tag_url("https://github.com/herder-sh/herder/releases").is_err());
    }

    #[test]
    fn asset_names_match_the_release_workflow() {
        assert_eq!(
            asset_name(&v("0.3.1"), "aarch64-unknown-linux-musl"),
            "herder-0.3.1-aarch64-unknown-linux-musl.tar.gz"
        );
    }

    #[test]
    fn sha256_matches_sha256sum_output() {
        // `printf hello | sha256sum`
        let line =
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824  hello.tar.gz\n";
        verify_sha256(b"hello", line).unwrap();
        let err = verify_sha256(b"hellO", line).unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
        assert!(verify_sha256(b"hello", "  \n").is_err());
    }

    fn tarball(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(gz);
        for (path, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, path, *data).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn extracts_the_binary_from_the_release_dir() {
        let target = "x86_64-unknown-linux-musl";
        let data = tarball(&[
            ("herder-0.3.1-x86_64-unknown-linux-musl/LICENSE", b"license"),
            (
                "herder-0.3.1-x86_64-unknown-linux-musl/herder",
                b"new binary",
            ),
        ]);
        assert_eq!(
            extract_binary(&data, &v("0.3.1"), target).unwrap(),
            b"new binary"
        );
        assert!(extract_binary(&data, &v("0.3.2"), target).is_err());
    }

    #[test]
    fn replace_swaps_the_file_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("herder");
        std::fs::write(&exe, b"old").unwrap();

        replace_exe(&exe, b"new").unwrap();

        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        let mode = std::fs::metadata(&exe).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["herder"], "temp file left behind");
    }

    #[test]
    fn replace_keeps_a_running_binary_intact() {
        // A process holding the old file open keeps reading the old contents: the rename
        // swaps the directory entry, it does not overwrite the old inode.
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("herder");
        std::fs::write(&exe, b"old").unwrap();
        let mut open = std::fs::File::open(&exe).unwrap();

        replace_exe(&exe, b"new").unwrap();

        let mut old = Vec::new();
        open.read_to_end(&mut old).unwrap();
        assert_eq!(old, b"old");
    }
}
