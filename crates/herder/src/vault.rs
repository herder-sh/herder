//! `herder vault`: run a vault on this machine, next to its daemon, and look after what it
//! holds.

use std::ffi::OsStr;
use std::fs::{DirBuilder, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use herder_daemon::auth::control::{self, Request, Response};
use herder_daemon::config::DEFAULT_PORT;
use herder_daemon::vault::{SECRETS_KEY_ENV, SecretsKey};

use crate::pair;
use crate::service::{self, Service};

/// The port an installed vault listens on unless told otherwise: the one after the daemon's.
const VAULT_PORT: u16 = DEFAULT_PORT + 1;

/// How long `herder vault install` waits for the vault to take a pairing request.
const START_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(clap::Subcommand)]
pub enum Command {
    /// Run a vault on this machine as its own service, next to the daemon, with its own data
    /// dir, port and secrets key, and print a code to pair the app with it.
    Install {
        /// Port the vault listens on, on every interface [default: 7448]. Only for a new
        /// vault: an installed one keeps the `listen` in its vault.toml.
        #[arg(long)]
        port: Option<u16>,
    },
    /// Stop and remove the vault's service. Its data dir stays unless `--purge` is given.
    Uninstall {
        /// Delete the vault's data dir too: every session it backed up, its secrets and the
        /// key they are encrypted with.
        #[arg(long)]
        purge: bool,
    },
    /// Drop a host that is gone, and every session and image it backed up, from this vault,
    /// and unpair it. Archived sessions go on their own; nothing else does.
    ForgetHost {
        /// The host's name, or its id where two hosts share a name.
        host: String,
        /// Vault config file, to find its data dir [default: $XDG_CONFIG_HOME/herder/daemon.toml].
        #[arg(long, value_name = "PATH", env = "HERDER_CONFIG")]
        config: Option<PathBuf>,
    },
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Install { port } => install(port),
        Command::Uninstall { purge } => uninstall(purge),
        Command::ForgetHost { host, config } => forget_host(host, config),
    }
}

/// Where an installed vault keeps everything: `$XDG_DATA_HOME/herder-vault`. It is the vault's
/// data dir, and holds its config and secrets key next to its data, so they go together.
fn vault_dir(xdg_data_home: Option<&OsStr>, home: Option<&OsStr>) -> Result<PathBuf> {
    let data = match xdg_data_home.map(Path::new) {
        Some(dir) if dir.is_absolute() => dir.to_path_buf(),
        _ => match home.map(Path::new) {
            Some(home) if home.is_absolute() => home.join(".local/share"),
            _ => bail!("cannot find the data dir: HOME is not set to an absolute path"),
        },
    };
    Ok(data.join("herder-vault"))
}

fn env_vault_dir() -> Result<PathBuf> {
    vault_dir(
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

fn config_path(dir: &Path) -> PathBuf {
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

fn install(port: Option<u16>) -> Result<()> {
    let dir = env_vault_dir()?;
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;

    let config = config_path(&dir);
    if config.exists() {
        if port.is_some() {
            bail!(
                "a vault is already set up in {}; change `listen` in {} instead",
                dir.display(),
                config.display()
            );
        }
        println!("keeping {}", config.display());
    } else {
        let port = port.unwrap_or(VAULT_PORT);
        // The daemon would only find out once it runs as a service, restarting over and over.
        std::net::TcpListener::bind(("0.0.0.0", port))
            .with_context(|| format!("port {port} is taken; pick another with --port"))?;
        write_new(&config, &config_file(&dir, port)?)?;
        println!("wrote {}", config.display());
    }
    // Fails now, not in the service, if the file was edited into something invalid.
    herder_daemon::Config::load_file(&config)?;

    let key = key_path(&dir);
    if key.exists() {
        println!("keeping the secrets key in {}", key.display());
    } else {
        write_new(&key, &key_file(&SecretsKey::generate_base64()?))?;
        println!("generated a secrets key in {}", key.display());
    }

    let service = service(&dir);
    service::install(&service)?;
    let info = wait_for_pairing(&dir).with_context(|| {
        format!(
            "the vault did not start; see why in {}",
            service::log_hint(&service).unwrap_or_else(|_| "its log".to_owned())
        )
    })?;
    println!();
    pair::print_pairing(&info)?;
    println!(
        "\nTo back up a host to this vault, mint it a host code with:\n  herder pair --config {} --host <name>",
        config.display()
    );
    Ok(())
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

/// Asks the vault running in `dir` for a pairing code for this user, once it takes requests.
fn wait_for_pairing(dir: &Path) -> Result<control::PairingInfo> {
    let request = Request::Pair {
        user: pair::default_user(),
        role: None,
    };
    let started = Instant::now();
    loop {
        match control::request(dir, &request) {
            Ok(Response::Paired(info)) => return Ok(info),
            Ok(Response::Error { message }) => bail!("{message}"),
            Ok(_) => bail!("the vault sent an unexpected answer"),
            Err(err) if started.elapsed() >= START_TIMEOUT => return Err(err),
            Err(_) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

fn uninstall(purge: bool) -> Result<()> {
    let dir = env_vault_dir()?;
    service::uninstall(&service(&dir))?;
    if purge {
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => println!("deleted {}", dir.display()),
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| format!("deleting {}", dir.display()));
            }
        }
    } else if dir.exists() {
        println!(
            "kept the vault's data in {}; `herder vault install` runs it again, \
             `herder vault uninstall --purge` deletes it",
            dir.display()
        );
    }
    Ok(())
}

fn forget_host(host: String, config: Option<PathBuf>) -> Result<()> {
    let config = herder_daemon::Config::load(config.as_deref())?;
    match control::request(&config.data_dir, &Request::ForgetHost { host })? {
        Response::ForgotHost(forgot) => {
            let sessions = match forgot.sessions {
                1 => "1 session".to_owned(),
                n => format!("{n} sessions"),
            };
            println!(
                "forgot {} ({}): {sessions} and its images dropped, {} unpaired",
                forgot.host_name,
                forgot.host_id,
                match forgot.devices {
                    1 => "1 device".to_owned(),
                    n => format!("{n} devices"),
                }
            );
            Ok(())
        }
        Response::Error { message } => bail!("{message}"),
        _ => bail!("the vault sent an unexpected answer"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_dir_follows_xdg_data_home() {
        let dir = vault_dir(Some(OsStr::new("/x/data")), Some(OsStr::new("/home/ann"))).unwrap();
        assert_eq!(dir, Path::new("/x/data/herder-vault"));
        let dir = vault_dir(Some(OsStr::new("rel")), Some(OsStr::new("/home/ann"))).unwrap();
        assert_eq!(dir, Path::new("/home/ann/.local/share/herder-vault"));
        assert!(vault_dir(None, None).is_err());
    }

    #[test]
    fn config_runs_a_vault_on_its_own_port_and_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = config_path(dir.path());
        std::fs::write(&path, config_file(dir.path(), 7448).unwrap()).unwrap();
        let config = herder_daemon::Config::load_file(&path).unwrap();
        assert_eq!(config.mode, herder_daemon::config::Mode::Vault);
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
