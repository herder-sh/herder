//! `herder hub`: run a hub on this machine, next to its daemon, and look after what it
//! holds.

use std::io::ErrorKind;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use herder_daemon::auth::control::{self, Request, Response};
use herder_daemon::hub::install::cliproxyapi::{self, Proxy, Tls};
use herder_daemon::hub::install::{self as hub_install, LocalHub};
use herder_daemon::service::{self, Service};
use herder_protocol::{RouteState, SecretValue};

use crate::pair;

#[derive(clap::Subcommand)]
pub enum Command {
    /// Run a hub on this machine as its own service, next to the daemon, with its own data
    /// dir, port and secrets key, and a CLIProxyAPI holding the fleet's provider logins as
    /// another service, and show how to become its owner from the app.
    Install {
        /// Port the hub listens on, on every interface [default: 7448]. Only for a new
        /// hub: an installed one keeps the `listen` in its hub.toml.
        #[arg(long)]
        port: Option<u16>,
        #[command(flatten)]
        cliproxyapi: CliProxyApi,
    },
    /// Stop and remove the hub's service and its CLIProxyAPI's. Its data dir, with the
    /// proxy's logins, stays unless `--purge` is given.
    Uninstall {
        /// Delete the hub's data dir too: every session it backed up, its secrets, the key
        /// they are encrypted with, and the proxy's provider logins.
        #[arg(long)]
        purge: bool,
    },
    /// Drop a host that is gone, and every session and image it backed up, from this hub,
    /// and unpair it. Archived sessions go on their own; nothing else does.
    ForgetHost {
        /// The host's name, or its id where two hosts share a name.
        host: String,
        /// Hub config file, to find its data dir [default: $XDG_CONFIG_HOME/herder/daemon.toml].
        #[arg(long, value_name = "PATH", env = "HERDER_CONFIG")]
        config: Option<PathBuf>,
    },
    /// Reach the hub from anywhere at a hostname of yours through a Cloudflare Tunnel that
    /// herder creates, and runs cloudflared for, itself.
    Tunnel {
        #[command(subcommand)]
        command: TunnelCommand,
        /// Hub config file, to find its data dir [default: $XDG_CONFIG_HOME/herder/daemon.toml].
        #[arg(long, value_name = "PATH", env = "HERDER_CONFIG", global = true)]
        config: Option<PathBuf>,
    },
}

/// `herder hub tunnel`.
#[derive(clap::Subcommand)]
pub enum TunnelCommand {
    /// Create the tunnel and the hostname in Cloudflare, and run cloudflared for them; run
    /// again to change the hostname or the token. Asks for a Cloudflare API token, kept as a
    /// hub secret, unless CLOUDFLARE_API_TOKEN holds one; Enter keeps the one given before.
    SetUp {
        /// The hostname to reach the hub at, in a zone of the account, such as
        /// hub.example.com.
        #[arg(long)]
        hostname: String,
        /// The Cloudflare account id, from the account's overview in the dashboard. Only the
        /// first time, or to change it.
        #[arg(long, value_name = "ID")]
        account_id: Option<String>,
    },
    /// Stop cloudflared and delete the tunnel and the hostname's DNS record.
    Remove,
    /// Show the tunnel and whether it is up.
    Status,
}

/// How `herder hub install` sets up the CLIProxyAPI next to the hub. Pass the same flags
/// again when reinstalling: they are not kept.
#[derive(clap::Args)]
pub struct CliProxyApi {
    /// Run no CLIProxyAPI next to the hub, and remove one an earlier install set up; its
    /// logins stay in the hub's dir.
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
    /// to this machine here too, such as in /etc/hosts, for the hub to manage the proxy.
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
            // Hosts on other machines reach it on every interface; the hub on loopback.
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
        Command::Tunnel { command, config } => tunnel(command, config),
    }
}

/// The hub's service, when `herder hub install` installed one on this machine.
pub fn installed_service() -> Option<Service> {
    let service = hub_install::service(&hub_install::env_dir().ok()?);
    service::is_installed(&service).then_some(service)
}

fn install(port: Option<u16>, proxy: Option<Proxy>) -> Result<()> {
    let dir = hub_install::env_dir()?;
    if proxy.is_none() {
        remove_cliproxyapi(&dir)?;
    }
    let hub = LocalHub {
        dir,
        port,
        services: Arc::new(service::System),
        cliproxyapi: proxy
            .clone()
            .map(|proxy| (proxy, cliproxyapi::download as cliproxyapi::Download)),
    };
    hub.install(&mut |step| println!("{step}"))?;
    let info = hub.own()?;
    println!();
    pair::print_own(&info)?;
    println!(
        "\nTo back up a host to this hub, mint it a host code with:\n  herder pair --config {} --host <name>",
        hub_install::config_path(&hub.dir).display()
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

/// Stops and removes the CLIProxyAPI next to the hub in `dir`, if one runs there, and its
/// binary; its logins stay.
fn remove_cliproxyapi(dir: &std::path::Path) -> Result<()> {
    let proxy = cliproxyapi::service(dir);
    if service::is_installed(&proxy) {
        service::uninstall(&proxy)?;
    }
    cliproxyapi::remove(dir, &hub_install::config_path(dir), &mut |step| {
        println!("{step}")
    })
}

fn uninstall(purge: bool) -> Result<()> {
    let dir = hub_install::env_dir()?;
    service::uninstall(&hub_install::service(&dir))?;
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
            "kept the hub's data in {}; `herder hub install` runs it again, \
             `herder hub uninstall --purge` deletes it",
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
        _ => bail!("the hub sent an unexpected answer"),
    }
}

/// `herder hub tunnel`: asks the running hub, which talks to Cloudflare and runs cloudflared,
/// and waits for it to finish.
fn tunnel(command: TunnelCommand, config: Option<PathBuf>) -> Result<()> {
    let config = herder_daemon::Config::load(config.as_deref())?;
    let (request, removing) = match command {
        TunnelCommand::SetUp {
            hostname,
            account_id,
        } => (
            Request::TunnelSetUp {
                account_id,
                hostname,
                api_token: api_token()?,
            },
            false,
        ),
        TunnelCommand::Remove => (Request::TunnelRemove, true),
        TunnelCommand::Status => (Request::TunnelStatus, false),
    };
    let mut answer = control::request(&config.data_dir, &request)?;
    // The hub works in the background; a set-up takes a dozen calls to Cloudflare.
    for _ in 0..180 {
        match &answer {
            Response::Tunnel {
                tunnel: Some(status),
            } if status.state == RouteState::SettingUp => {}
            Response::Tunnel { tunnel: Some(_) } if removing => {}
            _ => break,
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
        answer = control::request(&config.data_dir, &Request::TunnelStatus)?;
    }
    match answer {
        Response::Tunnel { tunnel: None } => {
            println!("this hub has no managed tunnel");
            Ok(())
        }
        Response::Tunnel {
            tunnel: Some(status),
        } => {
            let url = status.url.replacen("wss://", "https://", 1);
            match status.state {
                RouteState::Up => {
                    println!(
                        "the hub is reached at {url}; devices get it with their next connection"
                    );
                }
                RouteState::SettingUp => println!("the hub is still setting up {url}"),
                RouteState::Failed => bail!(
                    "{url} is not up: {}",
                    status.error.as_deref().unwrap_or("no reason given")
                ),
            }
            Ok(())
        }
        Response::Error { message } => bail!("{message}"),
        _ => bail!("the hub sent an unexpected answer"),
    }
}

/// The Cloudflare API token to give the hub: `CLOUDFLARE_API_TOKEN`, else asked for; `None`
/// keeps the one the hub holds.
fn api_token() -> Result<Option<SecretValue>> {
    use std::io::{BufRead, IsTerminal};

    let token = match std::env::var("CLOUDFLARE_API_TOKEN") {
        Ok(token) => token,
        Err(_) if std::io::stdin().is_terminal() => rpassword::prompt_password(
            "Cloudflare API token (Enter keeps the one the hub holds): ",
        )?,
        Err(_) => {
            let mut line = String::new();
            std::io::stdin().lock().read_line(&mut line)?;
            line
        }
    };
    Ok(SecretValue::new(token.trim().to_owned()))
}
