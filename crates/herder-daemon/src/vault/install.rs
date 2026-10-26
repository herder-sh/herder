//! A vault run on this machine, next to its daemon, as a service of its own with its own data
//! dir, port and secrets key: what `herder vault install` sets up, and what an owner's
//! `install_vault` sets up from a client.

use std::ffi::OsStr;
use std::fs::{DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::{SECRETS_KEY_ENV, SecretsKey};
use crate::auth::control::{self, PairingInfo, Request, Response};
use crate::config::DEFAULT_PORT;
use crate::service::{Service, Services};

/// The port an installed vault listens on unless told otherwise: the one after the daemon's.
pub const PORT: u16 = DEFAULT_PORT + 1;

/// How long the installer waits for the vault to take a pairing request.
const START_TIMEOUT: Duration = Duration::from_secs(30);

/// The vault this machine runs, or may run, next to its daemon.
#[derive(Clone)]
pub struct LocalVault {
    /// Where it keeps everything: its data, config and secrets key.
    pub dir: PathBuf,
    /// The port a new vault listens on; [`PORT`] when absent.
    pub port: Option<u16>,
    /// What installs its service.
    pub services: Arc<dyn Services>,
}

impl LocalVault {
    /// The vault `herder vault install` sets up for this user, run by this machine's service
    /// manager.
    pub fn system() -> Result<Self> {
        Ok(Self {
            dir: env_dir()?,
            port: None,
            services: Arc::new(crate::service::System),
        })
    }

    /// Its service.
    pub fn service(&self) -> Service {
        service(&self.dir)
    }

    /// Whether its service is installed.
    pub fn is_installed(&self) -> bool {
        self.services.is_installed(&self.service())
    }

    /// Writes its config and secrets key unless they are there, then installs and starts its
    /// service. `report` hears each step done.
    pub fn install(&self, report: &mut dyn FnMut(String)) -> Result<()> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;

        let config = config_path(&self.dir);
        if config.exists() {
            if self.port.is_some() {
                bail!(
                    "a vault is already set up in {}; change `listen` in {} instead",
                    self.dir.display(),
                    config.display()
                );
            }
            report(format!("keeping {}", config.display()));
        } else {
            let port = self.port.unwrap_or(PORT);
            // The daemon would only find out once it runs as a service, restarting over and over.
            std::net::TcpListener::bind(("0.0.0.0", port))
                .with_context(|| format!("port {port} is taken; pick another with --port"))?;
            write_new(&config, &config_file(&self.dir, port)?)?;
            report(format!("wrote {}", config.display()));
        }
        // Fails now, not in the service, if the file was edited into something invalid.
        crate::Config::load_file(&config)?;

        let key = key_path(&self.dir);
        if key.exists() {
            report(format!("keeping the secrets key in {}", key.display()));
        } else {
            write_new(&key, &key_file(&SecretsKey::generate_base64()?))?;
            report(format!("generated a secrets key in {}", key.display()));
        }

        self.services.install(&self.service())
    }

    /// Asks the vault for `request`, a pairing, once it takes requests: it may be starting.
    pub fn pair(&self, request: &Request) -> Result<PairingInfo> {
        let started = Instant::now();
        let answer = loop {
            match control::request(&self.dir, request) {
                Ok(answer) => break answer,
                Err(err) if started.elapsed() >= START_TIMEOUT => {
                    return Err(err).with_context(|| {
                        format!(
                            "the vault did not start; see why in {}",
                            self.services.log_hint(&self.service())
                        )
                    });
                }
                Err(_) => std::thread::sleep(Duration::from_millis(250)),
            }
        };
        match answer {
            Response::Paired(info) => Ok(info),
            Response::Error { message } => bail!("{message}"),
            _ => bail!("the vault sent an unexpected answer"),
        }
    }
}

/// Where an installed vault keeps everything: `$XDG_DATA_HOME/herder-vault`. It is the vault's
/// data dir, and holds its config and secrets key next to its data, so they go together.
fn dir(xdg_data_home: Option<&OsStr>, home: Option<&OsStr>) -> Result<PathBuf> {
    let data = match xdg_data_home.map(Path::new) {
        Some(dir) if dir.is_absolute() => dir.to_path_buf(),
        _ => match home.map(Path::new) {
            Some(home) if home.is_absolute() => home.join(".local/share"),
            _ => bail!("cannot find the data dir: HOME is not set to an absolute path"),
        },
    };
    Ok(data.join("herder-vault"))
}

/// The vault's dir for this user, as the environment has it.
pub fn env_dir() -> Result<PathBuf> {
    dir(
        std::env::var_os("XDG_DATA_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// The vault's service, run from the files in `dir`.
pub fn service(dir: &Path) -> Service {
    Service {
        label: "sh.herder.vault",
        unit: "herder-vault.service",
        name: "herder vault",
        description: "herder vault",
        installer: "herder vault install",
        args: vec![
            "daemon".to_owned(),
            "--vault".to_owned(),
            "--config".to_owned(),
            config_path(dir).to_string_lossy().into_owned(),
        ],
        env_file: Some(key_path(dir)),
        log: "vault.log",
    }
}

/// The vault's config file in `dir`.
pub fn config_path(dir: &Path) -> PathBuf {
    dir.join("vault.toml")
}

/// The file holding the vault's secrets key, as its service's environment.
fn key_path(dir: &Path) -> PathBuf {
    dir.join("vault.env")
}

/// The vault's `daemon.toml`: a vault on every interface at `port`, keeping its data in `dir`.
fn config_file(dir: &Path, port: u16) -> Result<String> {
    #[derive(serde::Serialize)]
    struct File<'a> {
        mode: &'static str,
        listen: String,
        data_dir: &'a Path,
    }
    let file = toml::to_string(&File {
        mode: "vault",
        listen: format!("0.0.0.0:{port}"),
        data_dir: dir,
    })
    .context("writing the vault's config")?;
    Ok(format!(
        "# Written by `herder vault install`: the vault running next to this machine's daemon.\n\
         # Restart it after a change: herder vault install\n{file}"
    ))
}

/// The environment file holding `key`, the vault's secrets key.
fn key_file(key: &str) -> String {
    format!(
        "# The vault's secrets key: back it up with this dir, as nothing stored under it can be\n\
         # read without it.\n{SECRETS_KEY_ENV}={key}\n"
    )
}

/// Writes `contents` to a new file at `path`, readable by this user only.
fn write_new(path: &Path, contents: &str) -> Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| file.write_all(contents.as_bytes()))
        .with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_follows_xdg_data_home() {
        let dir_of = |xdg, home| dir(xdg.map(OsStr::new), home.map(OsStr::new));
        let found = dir_of(Some("/x/data"), Some("/home/ann")).unwrap();
        assert_eq!(found, Path::new("/x/data/herder-vault"));
        let found = dir_of(Some("rel"), Some("/home/ann")).unwrap();
        assert_eq!(found, Path::new("/home/ann/.local/share/herder-vault"));
        assert!(dir_of(None, None).is_err());
    }

    #[test]
    fn config_runs_a_vault_on_its_own_port_and_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = config_path(dir.path());
        std::fs::write(&path, config_file(dir.path(), 7448).unwrap()).unwrap();
        let config = crate::Config::load_file(&path).unwrap();
        assert_eq!(config.mode, crate::config::Mode::Vault);
        assert_eq!(config.listen, ["0.0.0.0:7448".parse().unwrap()]);
        assert_eq!(config.data_dir, dir.path());
    }

    #[test]
    fn key_file_holds_a_key_the_vault_reads() {
        let key = SecretsKey::generate_base64().unwrap();
        let file = key_file(&key);
        let line = file.lines().find(|line| !line.starts_with('#')).unwrap();
        let (name, value) = line.split_once('=').unwrap();
        assert_eq!(name, SECRETS_KEY_ENV);
        assert!(SecretsKey::from_base64(value).is_ok());
    }

    #[test]
    fn new_files_are_private_and_never_overwritten() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.env");
        write_new(&path, "A=1\n").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(write_new(&path, "A=2\n").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "A=1\n");
    }
}
