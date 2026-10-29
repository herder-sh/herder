//! `herder fork`: copy a session's history into a new session on this machine, and go on with
//! it here; the original is left as it is.
//!
//! The session may be another machine's, up or gone. This terminal, signed in with `herder
//! login` to this machine and to one that holds the session (its host, or the vault it backs
//! up to), reads it there as the person signed in and relays it here. Hosts never read each
//! other's sessions.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::future::join_all;
use herder_client_core::Client;
use herder_daemon::auth::control::{self, Request, Response};
use herder_protocol::{AccountId, CommandResult, EventBody, SessionId};

/// How long each machine gets to say what it holds.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(clap::Args)]
pub struct Args {
    /// The session to fork, of this machine or of another one this terminal is signed in to,
    /// such as the vault.
    session: String,
    /// Account of this machine to run the fork on [default: the session's account if this
    /// machine has it, else the project's default account, else the first account of its
    /// provider].
    #[arg(long, value_name = "ID")]
    account: Option<String>,
    /// Daemon config file, to find its data dir [default: $XDG_CONFIG_HOME/herder/daemon.toml].
    #[arg(long, value_name = "PATH", env = "HERDER_CONFIG")]
    config: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<()> {
    let config = herder_daemon::Config::load(args.config.as_deref())?;
    let fingerprint = match control::request(&config.data_dir, &Request::Address)? {
        Response::Address { fingerprint, .. } => fingerprint,
        Response::Error { message } => bail!("{message}"),
        _ => bail!("the daemon sent an unexpected answer"),
    };
    let config_dir = herder_tui::config_dir()?
        .into_os_string()
        .into_string()
        .map_err(|_| anyhow::anyhow!("the config dir is not valid UTF-8"))?;
    let session_id = SessionId::new(args.session);
    let account_id = args.account.map(AccountId::new);
    let runtime = tokio::runtime::Runtime::new().context("starting the tokio runtime")?;
    runtime.block_on(async {
        let client = Client::open(
            config_dir,
            format!("herder-cli/{}", env!("CARGO_PKG_VERSION")),
        )?;
        let Some(here) = client
            .machines()
            .into_iter()
            .find(|machine| machine.fingerprint == fingerprint)
        else {
            bail!("this terminal is not signed in to this machine; run `herder login` first");
        };
        // A machine that does not answer cannot relay the session; the others are enough.
        let synced = client.machines().into_iter().map(|machine| {
            tokio::time::timeout(CONNECT_TIMEOUT, client.synced(machine.host_id))
        });
        join_all(synced).await;
        let Some(source) = client.machines().into_iter().find(|machine| {
            machine
                .sessions
                .iter()
                .any(|head| head.session_id == session_id)
        }) else {
            bail!(
                "no machine this terminal is signed in to holds session {session_id}; sign in \
                 to its vault with `herder login <address> --fingerprint <hex>`"
            );
        };
        let forked = client
            .fork_session(
                source.host_id,
                session_id,
                here.host_id.clone(),
                account_id,
            )
            .await?;
        let CommandResult::SessionForked {
            session_id,
            account_id,
            forked_from,
            from_host_id,
        } = forked
        else {
            bail!("{} sent an unexpected answer", here.name);
        };
        println!("forked {session_id} from {forked_from} of {from_host_id} on account {account_id}");
        let subscription = client.subscribe_session(here.host_id.clone(), session_id.clone())?;
        tokio::time::timeout(CONNECT_TIMEOUT, client.synced(here.host_id))
            .await
            .with_context(|| format!("{} did not send the fork", here.name))??;
        let created = subscription.next().await.and_then(|update| {
            update.events.into_iter().find_map(|event| match event.body {
                EventBody::SessionCreated {
                    worktree, branch, ..
                } => Some((worktree, branch)),
                _ => None,
            })
        });
        match created {
            Some((worktree, Some(branch))) => println!("worktree {worktree} on {branch}"),
            Some((worktree, None)) => println!("works in {worktree} itself, without a branch"),
            None => bail!("{} did not send the fork {session_id}", here.name),
        }
        Ok(())
    })
}
