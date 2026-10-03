//! `herder fork`: copy a session's history into a new session on this host, from this host or
//! the vault, and go on with it here; the original is left as it is.

use std::path::PathBuf;

use anyhow::{Result, bail};
use herder_daemon::auth::control::{self, Request, Response};
use herder_daemon::session::fork;
use herder_protocol::{AccountId, SessionId};

#[derive(clap::Args)]
pub struct Args {
    /// The session to fork, of this host or of another host replicating to the same vault.
    session: String,
    /// Account of this host to run the fork on [default: the session's account if this host
    /// has it, else the project's default account, else the first account of its provider].
    #[arg(long, value_name = "ID")]
    account: Option<String>,
    /// Daemon config file, to find its data dir [default: $XDG_CONFIG_HOME/herder/daemon.toml].
    #[arg(long, value_name = "PATH", env = "HERDER_CONFIG")]
    config: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<()> {
    let config = herder_daemon::Config::load(args.config.as_deref())?;
    let request = Request::Fork(fork::Request {
        session_id: SessionId::new(args.session),
        account_id: args.account.map(AccountId::new),
    });
    match control::request(&config.data_dir, &request)? {
        Response::Forked(forked) => {
            println!(
                "forked {} from {} of {} on account {}",
                forked.session_id, forked.forked_from, forked.from_host_id, forked.account_id
            );
            println!("worktree {} on {}", forked.worktree, forked.branch);
            match &forked.checkpoint {
                Some(checkpoint) => println!("files restored from {checkpoint}"),
                None => println!(
                    "the session had no checkpoint; the worktree starts at the default branch"
                ),
            }
            Ok(())
        }
        Response::Error { message } => bail!("{message}"),
        _ => bail!("the daemon sent an unexpected answer"),
    }
}
