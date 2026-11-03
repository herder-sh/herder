//! The CLIProxyAPI next to a vault installed on a machine: what `herder vault install` sets up
//! unless told not to, as the Kubernetes pod runs it next to the vault (`deploy/vault.yaml`).
//!
//! It all lives in the vault's dir:
//!
//! - `cliproxyapi.env`: the proxy's management secret, as the vault's service reads it
//!   ([`MANAGEMENT_KEY_ENV`]), generated at the first install and readable by its owner only.
//! - `cliproxyapi/cli-proxy-api`: the proxy, a pinned release ([`VERSION`]) downloaded from
//!   GitHub and checked against its checksum, with `cliproxyapi/version` saying which.
//! - `cliproxyapi/config.yaml`: its config, written again at each install with the management
//!   secret and a random first client key, as the proxy rewrites its config and the vault
//!   keeps its client keys from then on.
//! - `cliproxyapi/auth`: the provider logins, in plain text, kept across reinstalls.
//!
//! The proxy runs as its own service, next to the vault's. By default it listens on loopback
//! over plain HTTP, for hosts on the same machine; given a certificate, it serves TLS on the
//! address given, for hosts elsewhere.

use std::fs::DirBuilder;
use std::net::SocketAddr;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use herder_protocol::SecretValue;
use ring::rand::{SecureRandom, SystemRandom};

use crate::service::Service;
use crate::update::release;
use crate::vault::cliproxyapi::{MANAGEMENT_KEY_ENV, Management};

/// The CLIProxyAPI release installed, the one `deploy/vault.yaml` runs.
pub const VERSION: &str = "8.0.53";

/// The port the proxy listens on unless told otherwise.
pub const PORT: u16 = 8317;

/// Where CLIProxyAPI's releases are downloaded from.
const RELEASES: &str = "https://github.com/router-for-me/CLIProxyAPI/releases/download";

/// The binary in a release tarball, and in the proxy's dir.
const BINARY: &str = "cli-proxy-api";

/// What downloads the proxy's binary: [`download`], or a stub in tests.
pub type Download = fn() -> Result<Vec<u8>>;

/// How the proxy is reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proxy {
    /// Where it listens.
    pub listen: SocketAddr,
    /// Its certificate, for hosts on other machines; plain HTTP on loopback without one.
    pub tls: Option<Tls>,
}

impl Default for Proxy {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], PORT)),
            tls: None,
        }
    }
}

/// The proxy's TLS certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tls {
    /// The certificate chain, PEM.
    pub cert: PathBuf,
    /// Its private key, PEM.
    pub key: PathBuf,
    /// The name the certificate is for, which hosts reach the proxy at.
    pub name: String,
}

impl Proxy {
    /// Fails on a proxy that hosts could reach over plain HTTP, or a certificate that is not
    /// there.
    pub fn check(&self) -> Result<()> {
        match &self.tls {
            None if !self.listen.ip().is_loopback() => bail!(
                "CLIProxyAPI serves plain HTTP on loopback only; give it a certificate to listen \
                 on {}",
                self.listen
            ),
            None => Ok(()),
            Some(tls) => {
                for path in [&tls.cert, &tls.key] {
                    if !path.is_absolute() || !path.is_file() {
                        bail!("{} is not a file at an absolute path", path.display());
                    }
                }
                if tls.name.is_empty() || tls.name.contains(['/', ':', ' ']) {
                    bail!("{:?} is not a host name", tls.name);
                }
                Ok(())
            }
        }
    }

    /// The URL the vault reaches the proxy's management API at, and hosts on this machine
    /// reach the proxy at: loopback, or the certificate's name, which must resolve to this
    /// machine here, so the proxy sees a request from itself.
    pub fn url(&self) -> String {
        let port = self.listen.port();
        match &self.tls {
            None => format!("http://{}", self.listen),
            Some(tls) => format!("https://{}:{port}", tls.name),
        }
    }
}

/// The proxy's dir in the vault's `dir`.
fn proxy_dir(dir: &Path) -> PathBuf {
    dir.join("cliproxyapi")
}

/// The file holding the proxy's management secret, as the vault's service's environment.
pub fn management_key_path(dir: &Path) -> PathBuf {
    dir.join("cliproxyapi.env")
}

/// The proxy's binary.
fn binary_path(dir: &Path) -> PathBuf {
    proxy_dir(dir).join(BINARY)
}

/// The file naming the release the binary is.
fn version_path(dir: &Path) -> PathBuf {
    proxy_dir(dir).join("version")
}

/// The proxy's config.
fn config_path(dir: &Path) -> PathBuf {
    proxy_dir(dir).join("config.yaml")
}

/// Where the proxy keeps its provider logins.
pub fn auth_dir(dir: &Path) -> PathBuf {
    proxy_dir(dir).join("auth")
}

/// The proxy's service, run from the files in the vault's `dir`.
pub fn service(dir: &Path) -> Service {
    Service {
        label: "sh.herder.cliproxyapi",
        unit: "herder-cliproxyapi.service",
        name: "the vault's CLIProxyAPI",
        description: "CLIProxyAPI next to the herder vault",
        installer: "herder vault install",
        program: Some(binary_path(dir)),
        args: vec![
            "-config".to_owned(),
            config_path(dir).to_string_lossy().into_owned(),
        ],
        env_files: Vec::new(),
        log: "cliproxyapi.log",
    }
}

/// Sets the proxy up in the vault's `dir`, reached as `proxy`: its binary, from `download`
/// unless the pinned release is there, its management secret unless it is there, its config,
/// its auth dir, and the vault's `cliproxyapi` URL in `vault_config`. Logins are kept.
/// `report` hears each step done, never a secret.
pub fn set_up(
    dir: &Path,
    vault_config: &Path,
    proxy: &Proxy,
    download: &dyn Fn() -> Result<Vec<u8>>,
    report: &mut dyn FnMut(String),
) -> Result<()> {
    proxy.check()?;
    for path in [proxy_dir(dir), auth_dir(dir)] {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&path)
            .with_context(|| format!("creating {}", path.display()))?;
    }

    let binary = binary_path(dir);
    let installed = std::fs::read_to_string(version_path(dir)).unwrap_or_default();
    if binary.is_file() && installed.trim() == VERSION {
        report(format!(
            "keeping CLIProxyAPI {VERSION} in {}",
            binary.display()
        ));
    } else {
        release::replace_exe(&binary, &download()?)?;
        write(&version_path(dir), &format!("{VERSION}\n"))?;
        report(format!(
            "installed CLIProxyAPI {VERSION} in {}",
            binary.display()
        ));
    }

    let key_path = management_key_path(dir);
    let secret = match management_key(dir)? {
        Some(secret) => {
            report(format!(
                "keeping CLIProxyAPI's management secret in {}",
                key_path.display()
            ));
            secret
        }
        None => {
            let secret = random_hex()?;
            write(&key_path, &management_key_file(&secret))?;
            report(format!(
                "generated CLIProxyAPI's management secret in {}",
                key_path.display()
            ));
            secret
        }
    };

    let config = config_path(dir);
    write(&config, &config_file(dir, proxy, &secret, &random_hex()?))?;
    report(format!("wrote {}", config.display()));

    let url = proxy.url();
    set_vault_url(vault_config, Some(&url))?;
    report(format!(
        "set the vault's cliproxyapi to {url} in {}",
        vault_config.display()
    ));
    Ok(())
}

/// Takes the proxy away from the vault in `dir`, once its service is gone: its binary, and
/// the vault's `cliproxyapi` URL in `vault_config` when there is one. Its config, secret and
/// logins stay, for a later install.
pub fn remove(dir: &Path, vault_config: &Path, report: &mut dyn FnMut(String)) -> Result<()> {
    for path in [binary_path(dir), version_path(dir)] {
        match std::fs::remove_file(&path) {
            Ok(()) => report(format!("removed {}", path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("removing {}", path.display())),
        }
    }
    if vault_config.is_file() {
        set_vault_url(vault_config, None)?;
    }
    Ok(())
}

/// The management API of the proxy at `url`, with the secret in the environment, else the
/// one `herder vault install` generated in the vault's `dir`.
pub fn management(url: &str, dir: &Path) -> Result<Management> {
    if std::env::var_os(MANAGEMENT_KEY_ENV).is_none()
        && let Some(secret) = management_key(dir)?
    {
        let key = SecretValue::new(secret).context("the management secret is empty")?;
        return Ok(Management::new(url, key));
    }
    Management::from_env(url)
}

/// The management secret generated in the vault's `dir`, if there is one.
fn management_key(dir: &Path) -> Result<Option<String>> {
    let path = management_key_path(dir);
    if !path.exists() {
        return Ok(None);
    }
    let vars = crate::service::read_env_file(&path)?;
    match vars
        .into_iter()
        .find(|(name, _)| name == MANAGEMENT_KEY_ENV)
    {
        Some((_, secret)) if !secret.is_empty() => Ok(Some(secret)),
        _ => bail!("{} holds no {MANAGEMENT_KEY_ENV}", path.display()),
    }
}

/// The environment file holding `secret`, the proxy's management secret.
fn management_key_file(secret: &str) -> String {
    format!(
        "# CLIProxyAPI's management secret, which the vault manages it with. The proxy hashes it\n\
         # into its config.yaml when it starts.\n{MANAGEMENT_KEY_ENV}={secret}\n"
    )
}

/// The proxy's `config.yaml` in the vault's `dir`, with `proxy`'s reach, management secret
/// `secret` and a first client key `placeholder` nobody has, with the settings the
/// Kubernetes pod's proxy has (see `deploy/vault.yaml`).
fn config_file(dir: &Path, proxy: &Proxy, secret: &str, placeholder: &str) -> String {
    let quote = |text: &str| serde_json::Value::from(text).to_string();
    let tls = match &proxy.tls {
        None => "    enable: false\n".to_owned(),
        Some(tls) => format!(
            "    enable: true\n    cert: {}\n    key: {}\n",
            quote(&tls.cert.to_string_lossy()),
            quote(&tls.key.to_string_lossy())
        ),
    };
    format!(
        "\
# Written by `herder vault install` at each install, with the management secret and a random
# first client key filled in: the proxy writes its config back, and the vault keeps its client
# keys from then on. Every setting here wins over what the proxy wrote.
config-version: 8
server:
  host: {host}
  port: {port}
  tls:
{tls}management:
  # Only this machine may manage the proxy, with the secret below, which the proxy hashes at
  # start. Never set MANAGEMENT_PASSWORD instead: the proxy takes that one from anywhere.
  allow-remote: false
  secret-key: {secret}
  disable-control-panel: true
access:
  # The vault puts one key per host here. A proxy with no key serves anyone, so it starts
  # with one random key nobody has.
  api-keys: [{placeholder}]
routing:
  # Stay on one login until it hits its limit, as herder's own failover does.
  strategy: fill-first
  session-affinity: true
  # A session moves to another login on a limit or a refused login only, not on a passing
  # error.
  session-affinity-rebind-on: quota
oauth:
  # The provider logins, in plain text: the most sensitive thing herder has.
  auth-dir: {auth}
",
        host = quote(&proxy.listen.ip().to_string()),
        port = proxy.listen.port(),
        secret = quote(secret),
        placeholder = quote(placeholder),
        auth = quote(&auth_dir(dir).to_string_lossy()),
    )
}

/// Sets the `[vault] cliproxyapi` URL in the vault config at `path`, or removes it, keeping
/// the rest of the file as written.
fn set_vault_url(path: &Path, url: Option<&str>) -> Result<()> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("reading {}", path.display()))?;
    match url {
        Some(url) => {
            let table = doc
                .entry("vault")
                .or_insert_with(toml_edit::table)
                .as_table_mut()
                .with_context(|| format!("[vault] in {} is not a table", path.display()))?;
            table.insert("cliproxyapi", toml_edit::value(url));
        }
        None => {
            let Some(table) = doc.get_mut("vault").and_then(toml_edit::Item::as_table_mut) else {
                return Ok(());
            };
            if table.remove("cliproxyapi").is_none() {
                return Ok(());
            }
        }
    }
    crate::config::write_atomically(path, doc.to_string().as_bytes())
        .with_context(|| format!("writing {}", path.display()))
}

/// Writes `contents` to `path`, readable by this user only.
fn write(path: &Path, contents: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    crate::config::write_atomically(path, contents.as_bytes())
        .and_then(|()| std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)))
        .with_context(|| format!("writing {}", path.display()))
}

/// 32 random bytes, in hex.
fn random_hex() -> Result<String> {
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("the system random number generator failed"))?;
    Ok(release::hex(&bytes))
}

/// The pinned release's asset for `os` and `arch`, and its SHA-256 from the release's
/// `checksums.txt`.
fn asset(os: &str, arch: &str) -> Result<(String, &'static str)> {
    let (platform, sha256) = match (os, arch) {
        ("macos", "aarch64") => (
            "darwin_aarch64",
            "24a9116a9be459e9d1df5a9792eca17f04c0bb8505584d40c5b255565d07186f",
        ),
        ("macos", "x86_64") => (
            "darwin_amd64",
            "7105389234f1b2cb7c045a52b3c2d3eced83fed8de1228567759817a0c8c6b2d",
        ),
        ("linux", "aarch64") => (
            "linux_aarch64",
            "b68ecfef89a0ce576332742ae5435353fb64dc28d93f9d1ca93d50385a942989",
        ),
        ("linux", "x86_64") => (
            "linux_amd64",
            "96bdc1ec195adc23baae6eb0413a9dce2fdccf03c0d3fd290e91aa8812e2a70d",
        ),
        (os, arch) => bail!("CLIProxyAPI has no release build for {arch} {os}"),
    };
    Ok((format!("CLIProxyAPI_{VERSION}_{platform}.tar.gz"), sha256))
}

/// Downloads the pinned release for this machine, checks it against its pinned checksum and
/// returns the proxy's binary in it.
pub fn download() -> Result<Vec<u8>> {
    let (asset, sha256) = asset(std::env::consts::OS, std::env::consts::ARCH)?;
    let tarball = release::get(&format!("{RELEASES}/v{VERSION}/{asset}"))?;
    unpack(&tarball, sha256).with_context(|| format!("verifying {asset}"))
}

/// The proxy's binary in `tarball`, once its SHA-256 is `sha256`.
fn unpack(tarball: &[u8], sha256: &str) -> Result<Vec<u8>> {
    release::verify_sha256(tarball, sha256)?;
    release::extract_file(tarball, BINARY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn fake_download() -> Result<Vec<u8>> {
        Ok(b"#!/bin/sh\n".to_vec())
    }

    fn no_download() -> Result<Vec<u8>> {
        bail!("downloaded again")
    }

    /// A vault dir with a vault config in it.
    fn vault() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("vault.toml");
        std::fs::write(
            &config,
            "# mine\nmode = \"vault\"\nlisten = \"0.0.0.0:7448\"\n",
        )
        .unwrap();
        (dir, config)
    }

    fn tls(dir: &Path) -> Tls {
        let cert = dir.join("cpa.crt");
        let key = dir.join("cpa.key");
        std::fs::write(&cert, "cert").unwrap();
        std::fs::write(&key, "key").unwrap();
        Tls {
            cert,
            key,
            name: "cpa.example.com".into(),
        }
    }

    #[test]
    fn config_has_the_pods_settings_on_loopback() {
        let config = config_file(Path::new("/v"), &Proxy::default(), "s3cret", "nobody");
        for line in [
            "config-version: 8\n",
            "  host: \"127.0.0.1\"\n  port: 8317\n  tls:\n    enable: false\n",
            "  allow-remote: false\n  secret-key: \"s3cret\"\n  disable-control-panel: true\n",
            "  api-keys: [\"nobody\"]\n",
            "  strategy: fill-first\n  session-affinity: true\n",
            "  session-affinity-rebind-on: quota\n",
            "  auth-dir: \"/v/cliproxyapi/auth\"\n",
        ] {
            assert!(config.contains(line), "{line:?} not in\n{config}");
        }
        assert!(!config.contains("MANAGEMENT_PASSWORD:"));
    }

    #[test]
    fn config_serves_tls_with_the_certificate_given() {
        let proxy = Proxy {
            listen: "0.0.0.0:9000".parse().unwrap(),
            tls: Some(Tls {
                cert: "/etc/cpa/\"t\".crt".into(),
                key: "/etc/cpa/t.key".into(),
                name: "cpa.example.com".into(),
            }),
        };
        let config = config_file(Path::new("/v"), &proxy, "s", "p");
        assert!(
            config.contains(
                "  host: \"0.0.0.0\"\n  port: 9000\n  tls:\n    enable: true\n    \
                 cert: \"/etc/cpa/\\\"t\\\".crt\"\n    key: \"/etc/cpa/t.key\"\n"
            ),
            "{config}"
        );
        assert_eq!(proxy.url(), "https://cpa.example.com:9000");
        assert_eq!(Proxy::default().url(), "http://127.0.0.1:8317");
    }

    #[test]
    fn plain_http_is_for_loopback_only() {
        let dir = tempfile::tempdir().unwrap();
        let open = Proxy {
            listen: "0.0.0.0:8317".parse().unwrap(),
            tls: None,
        };
        assert!(open.check().is_err());
        assert!(Proxy::default().check().is_ok());
        let mut proxy = Proxy {
            listen: "0.0.0.0:8317".parse().unwrap(),
            tls: Some(tls(dir.path())),
        };
        assert!(proxy.check().is_ok());
        proxy.tls.as_mut().unwrap().cert = dir.path().join("missing.crt");
        assert!(proxy.check().is_err());
    }

    #[test]
    fn set_up_writes_private_files_and_never_reports_the_secret() {
        let (dir, vault_config) = vault();
        let mut steps = Vec::new();
        set_up(
            dir.path(),
            &vault_config,
            &Proxy::default(),
            &fake_download,
            &mut |step| steps.push(step),
        )
        .unwrap();

        let secret = management_key(dir.path()).unwrap().unwrap();
        assert_eq!(secret.len(), 64);
        for step in &steps {
            assert!(!step.contains(&secret), "{step}");
        }
        assert_eq!(mode(&management_key_path(dir.path())), 0o600);
        assert_eq!(mode(&config_path(dir.path())), 0o600);
        assert_eq!(mode(&auth_dir(dir.path())), 0o700);
        assert_eq!(mode(&proxy_dir(dir.path())), 0o700);
        assert_eq!(mode(&binary_path(dir.path())), 0o755);
        assert_eq!(
            std::fs::read(binary_path(dir.path())).unwrap(),
            b"#!/bin/sh\n"
        );
        let config = std::fs::read_to_string(config_path(dir.path())).unwrap();
        assert!(config.contains(&format!("secret-key: \"{secret}\"")));

        // The vault manages the proxy at its URL, with the secret.
        let vault = crate::Config::load_file(&vault_config).unwrap();
        assert_eq!(vault.cliproxyapi.as_deref(), Some("http://127.0.0.1:8317"));
        let text = std::fs::read_to_string(&vault_config).unwrap();
        assert!(text.starts_with("# mine\n"), "{text}");
        assert!(management("http://127.0.0.1:8317", dir.path()).is_ok());
    }

    #[test]
    fn reinstalling_keeps_the_binary_secret_and_logins() {
        let (dir, vault_config) = vault();
        let mut quiet = |_: String| {};
        set_up(
            dir.path(),
            &vault_config,
            &Proxy::default(),
            &fake_download,
            &mut quiet,
        )
        .unwrap();
        let secret = management_key(dir.path()).unwrap();
        let login = auth_dir(dir.path()).join("claude-ann.json");
        std::fs::write(&login, "{}").unwrap();

        let proxy = Proxy {
            listen: "0.0.0.0:8318".parse().unwrap(),
            tls: Some(tls(dir.path())),
        };
        set_up(dir.path(), &vault_config, &proxy, &no_download, &mut quiet).unwrap();
        assert_eq!(management_key(dir.path()).unwrap(), secret);
        assert!(login.is_file());
        let vault = crate::Config::load_file(&vault_config).unwrap();
        assert_eq!(
            vault.cliproxyapi.as_deref(),
            Some("https://cpa.example.com:8318")
        );

        // Another release is downloaded again.
        std::fs::write(version_path(dir.path()), "1.0.0\n").unwrap();
        assert!(set_up(dir.path(), &vault_config, &proxy, &no_download, &mut quiet).is_err());
    }

    #[test]
    fn removing_takes_the_binary_and_url_and_keeps_logins() {
        let (dir, vault_config) = vault();
        let mut quiet = |_: String| {};
        set_up(
            dir.path(),
            &vault_config,
            &Proxy::default(),
            &fake_download,
            &mut quiet,
        )
        .unwrap();
        let login = auth_dir(dir.path()).join("codex-ann.json");
        std::fs::write(&login, "{}").unwrap();
        remove(dir.path(), &vault_config, &mut quiet).unwrap();
        assert!(!binary_path(dir.path()).exists());
        assert!(login.is_file());
        let vault = crate::Config::load_file(&vault_config).unwrap();
        assert_eq!(vault.cliproxyapi, None);
        // Again, and with no vault config, it has nothing left to do.
        remove(dir.path(), &vault_config, &mut quiet).unwrap();
        remove(dir.path(), &dir.path().join("none.toml"), &mut quiet).unwrap();
    }

    #[test]
    fn the_release_is_checked_against_its_pinned_checksum() {
        use sha2::{Digest, Sha256};

        let tarball = release::tarball(&[("README.md", b"x"), (BINARY, b"proxy")]);
        let sum = release::hex(&Sha256::digest(&tarball));
        assert_eq!(unpack(&tarball, &sum).unwrap(), b"proxy");
        let wrong = release::hex(&Sha256::digest(b"other"));
        let err = unpack(&tarball, &wrong).unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
    }

    #[test]
    fn every_supported_machine_has_a_pinned_asset() {
        let (name, sum) = asset("macos", "aarch64").unwrap();
        assert_eq!(name, "CLIProxyAPI_8.0.53_darwin_aarch64.tar.gz");
        assert_eq!(sum.len(), 64);
        assert!(asset("linux", "x86_64").is_ok());
        assert!(asset("linux", "aarch64").is_ok());
        assert!(asset("macos", "x86_64").is_ok());
        assert!(asset("windows", "x86_64").is_err());
    }

    #[test]
    fn the_service_runs_the_proxy_with_its_config() {
        let service = service(Path::new("/v"));
        assert_eq!(
            service.program.as_deref(),
            Some(Path::new("/v/cliproxyapi/cli-proxy-api"))
        );
        assert_eq!(service.args, ["-config", "/v/cliproxyapi/config.yaml"]);
        assert!(service.env_files.is_empty());
    }
}
