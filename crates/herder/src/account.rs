//! `herder account`: manage a paired machine's provider accounts from a terminal.
//!
//! Like `herder session`, every command connects through the client profile and never asks
//! anything interactively.

use anyhow::{Context, Result};
use herder_protocol::{AccountId, CommandBody};
use serde::Serialize;

use crate::session;

#[derive(clap::Args)]
pub struct Args {
    /// Paired machine, by name or host id [default: the only paired machine].
    #[arg(long, global = true, value_name = "NAME")]
    machine: Option<String>,
    /// Print JSON on stdout instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Remove an account from the machine; owners only. Its config dir, with the provider's
    /// login, stays on disk. Archive the sessions running on it first.
    Remove {
        /// The account, by id or label.
        account: String,
    },
}

pub fn run(args: Args) -> Result<()> {
    session::connected(args.machine.as_deref(), args.json, |cli| async move {
        match args.command {
            Command::Remove { account } => {
                let account_id = cli
                    .account(Some(&account))?
                    .context("name the account to remove")?;
                cli.send(CommandBody::RemoveAccount {
                    account_id: account_id.clone(),
                })
                .await?;
                #[derive(Serialize)]
                struct Removed<'a> {
                    account_id: &'a AccountId,
                }
                cli.print(
                    &Removed {
                        account_id: &account_id,
                    },
                    || {
                        format!(
                            "removed account {account_id} from {}; its config dir is kept\n",
                            cli.machine.name
                        )
                    },
                )
            }
        }
    })
}
