//! `herder recover`: take over a session whose host died, from the vault, on this host.

use std::path::PathBuf;

use anyhow::{Result, bail};
use herder_daemon::auth::control::{self, Request, Response};
use herder_daemon::vault::recover;
use herder_protocol::{AccountId, SessionId};

#[derive(clap::Args)]
pub struct Args {
    /// The session to recover.
    session: String,
    /// Account of this host to run it on [default: its own account if this host has it, else
    /// the project's default account, else the first account of its provider].
    #[arg(long, value_name = "ID")]
    account: Option<String>,
    /// Recover even though the vault shows the session's host online: only for a host that is
    /// gone or cut off before the vault noticed. Its copy turns read-only if it comes back.
    #[arg(long)]
    force: bool,
    /// Daemon config file, to find its data dir [default: $XDG_CONFIG_HOME/herder/daemon.toml].
    #[arg(long, value_name = "PATH", env = "HERDER_CONFIG")]
    config: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<()> {
    let config = herder_daemon::Config::load(args.config.as_deref())?;
    let request = Request::Recover(recover::Request {
        session_id: SessionId::new(args.session),
        account_id: args.account.map(AccountId::new),
        force: args.force,
    });
    match control::request(&config.data_dir, &request)? {
        Response::Recovered(outcome) => {
            let session = &outcome.session;
            println!(
                "recovered {} from {} ({}) on account {}",
                session.session_id,
                outcome.origin.host_name,
                outcome.origin.host_id,
                session.account_id
            );
            println!("worktree {} on {}", session.worktree, session.branch);
            match &session.checkpoint {
                Some(checkpoint) => println!("files restored from {checkpoint}"),
                None => println!(
                    "origin had no checkpoint of the session; the worktree starts at the \
                     default branch"
                ),
            }
            Ok(())
        }
        Response::Error { message } => bail!("{message}"),
        _ => bail!("the daemon sent an unexpected answer"),
    }
}
