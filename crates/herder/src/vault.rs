//! `herder vault`: run a vault on this machine, next to its daemon, and look after what it
//! holds.

use std::io::ErrorKind;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use herder_daemon::auth::control::{self, Request, Response};
use herder_daemon::service::{self, Service};
use herder_daemon::vault::install::cliproxyapi::{self, Proxy, Tls};
use herder_daemon::vault::install::{self as vault_install, LocalVault};

use crate::pair;

#[derive(clap::Subcommand)]
pub enum Command {
    /// Run a vault on this machine as its own service, next to the daemon, with its own data
    /// dir, port and secrets key, and a CLIProxyAPI holding the fleet's provider logins as
    /// another service, and show how to become its owner from the app.
    Install {
        /// Port the vault listens on, on every interface [default: 7448]. Only for a new
        /// vault: an installed one keeps the `listen` in its vault.toml.
        #[arg(long)]
        port: Option<u16>,
        #[command(flatten)]
        cliproxyapi: CliProxyApi,
    },
    /// Stop and remove the vault's service and its CLIProxyAPI's. Its data dir, with the
    /// proxy's logins, stays unless `--purge` is given.
    Uninstall {
        /// Delete the vault's data dir too: every session it backed up, its secrets, the key
        /// they are encrypted with, and the proxy's provider logins.
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

/// How `herder vault install` sets up the CLIProxyAPI next to the vault. Pass the same flags
/// again when reinstalling: they are not kept.
#[derive(clap::Args)]
pub struct CliProxyApi {
    /// Run no CLIProxyAPI next to the vault, and remove one an earlier install set up; its
    /// logins stay in the vault's dir.
    #[arg(long, conflicts_with_all = ["cliproxyapi_listen", "cliproxyapi_tls_cert"])]
    no_cliproxyapi: bool,
    /// Address the CLIProxyAPI listens on [default: 127.0.0.1:8317]. Plain HTTP on loopback,
    /// for hosts on this machine; any other address needs a certificate.
    #[arg(long, value_name = "ADDR")]
    cliproxyapi_listen: Option<SocketAddr>,
    /// The CLIProxyAPI's TLS certificate chain (PEM), from a CA the hosts trust, for hosts on
    /// other machines; the Claude and Codex CLIs refuse a self-signed one.
    #[arg(long, value_name = "PATH", requires_all = ["cliproxyapi_tls_key", "cliproxyapi_name"])]
    cliproxyapi_tls_cert: Option<PathBuf>,
    /// The certificate's private key (PEM).
    #[arg(long, value_name = "PATH", requires = "cliproxyapi_tls_cert")]
    cliproxyapi_tls_key: Option<PathBuf>,
    /// The name the certificate is for, which hosts reach the CLIProxyAPI at. It must resolve
    /// to this machine here too, such as in /etc/hosts, for the vault to manage the proxy.
    #[arg(long, value_name = "NAME", requires = "cliproxyapi_tls_cert")]
    cliproxyapi_name: Option<String>,
}

impl CliProxyApi {
    /// The proxy to set up; `None` for none.
    fn proxy(self) -> Result<Option<Proxy>> {
        if self.no_cliproxyapi {
            return Ok(None);
        }
        let tls = match (
            self.cliproxyapi_tls_cert,
            self.cliproxyapi_tls_key,
            self.cliproxyapi_name,
        ) {
            (Some(cert), Some(key), Some(name)) => Some(Tls {
                cert: std::path::absolute(cert)?,
                key: std::path::absolute(key)?,
                name,
            }),
            (None, None, None) => None,
            _ => bail!(
                "give --cliproxyapi-tls-cert, --cliproxyapi-tls-key and --cliproxyapi-name together"
            ),
        };
        let listen = self.cliproxyapi_listen.unwrap_or(match tls {
            // Hosts on other machines reach it on every interface; the vault on loopback.
            Some(_) => SocketAddr::from(([0, 0, 0, 0], cliproxyapi::PORT)),
            None => Proxy::default().listen,
        });
        Ok(Some(Proxy { listen, tls }))
    }
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Install { port, cliproxyapi } => install(port, cliproxyapi.proxy()?),
        Command::Uninstall { purge } => uninstall(purge),
        Command::ForgetHost { host, config } => forget_host(host, config),
    }
}

/// The vault's service, when `herder vault install` installed one on this machine.
pub fn installed_service() -> Option<Service> {
    let service = vault_install::service(&vault_install::env_dir().ok()?);
    service::is_installed(&service).then_some(service)
}

fn install(port: Option<u16>, proxy: Option<Proxy>) -> Result<()> {
    let dir = vault_install::env_dir()?;
    if proxy.is_none() {
        remove_cliproxyapi(&dir)?;
    }
    let vault = LocalVault {
        dir,
        port,
        services: Arc::new(service::System),
        cliproxyapi: proxy
            .clone()
            .map(|proxy| (proxy, cliproxyapi::download as cliproxyapi::Download)),
    };
    vault.install(&mut |step| println!("{step}"))?;
    let info = vault.own()?;
    println!();
    pair::print_own(&info)?;
    println!(
        "\nTo back up a host to this vault, mint it a host code with:\n  herder pair --config {} --host <name>",
        vault_install::config_path(&vault.dir).display()
    );
    if let Some(proxy) = &proxy {
        println!(
            "\nPaired hosts reach the CLIProxyAPI with an endpoint account, once it holds a login:\n  \
             endpoint = \"{}\"\n  key_secret = \"CLIPROXYAPI_KEY\"",
            proxy.url()
        );
    }
    Ok(())
}

/// Stops and removes the CLIProxyAPI next to the vault in `dir`, if one runs there, and its
/// binary; its logins stay.
fn remove_cliproxyapi(dir: &std::path::Path) -> Result<()> {
    let proxy = cliproxyapi::service(dir);
    if service::is_installed(&proxy) {
        service::uninstall(&proxy)?;
    }
    cliproxyapi::remove(dir, &vault_install::config_path(dir), &mut |step| {
        println!("{step}")
    })
}

fn uninstall(purge: bool) -> Result<()> {
    let dir = vault_install::env_dir()?;
    service::uninstall(&vault_install::service(&dir))?;
    remove_cliproxyapi(&dir)?;
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
