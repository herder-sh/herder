//! `herder vault`: look after what the vault running on this machine holds.

use std::path::PathBuf;

use anyhow::{Result, bail};
use herder_daemon::auth::control::{self, Request, Response};

#[derive(clap::Subcommand)]
pub enum Command {
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
    let Command::ForgetHost { host, config } = command;
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
