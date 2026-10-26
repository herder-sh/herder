//! `herder vault`: run a vault on this machine, next to its daemon, and look after what it
//! holds.

use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use herder_daemon::auth::control::{self, Request, Response};
use herder_daemon::service::{self, Service};
use herder_daemon::vault::install::{self as vault_install, LocalVault};

use crate::pair;

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

/// The vault's service, when `herder vault install` installed one on this machine.
pub fn installed_service() -> Option<Service> {
    let service = vault_install::service(&vault_install::env_dir().ok()?);
    service::is_installed(&service).then_some(service)
}

fn install(port: Option<u16>) -> Result<()> {
    let vault = LocalVault {
        dir: vault_install::env_dir()?,
        port,
        services: Arc::new(service::System),
    };
    vault.install(&mut |step| println!("{step}"))?;
    let info = vault.pair(&Request::Pair {
        user: pair::default_user(),
        role: None,
    })?;
    println!();
    pair::print_pairing(&info)?;
    println!(
        "\nTo back up a host to this vault, mint it a host code with:\n  herder pair --config {} --host <name>",
        vault_install::config_path(&vault.dir).display()
    );
    Ok(())
}

fn uninstall(purge: bool) -> Result<()> {
    let dir = vault_install::env_dir()?;
    service::uninstall(&vault_install::service(&dir))?;
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
