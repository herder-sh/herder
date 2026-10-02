//! `herder hook`: the client side of the git hooks in session worktrees.
//!
//! A hook must not break git when herder cannot help: every failure is a warning on stderr,
//! and the command still succeeds.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Subcommand;
use herder_daemon::prs::hooks;
use herder_protocol::SessionId;

#[derive(Subcommand)]
pub enum Hook {
    /// Report the branches being pushed to the daemon; reads git's pre-push input on stdin.
    PrePush {
        /// The daemon's hook socket.
        #[arg(long, value_name = "PATH")]
        socket: PathBuf,
        /// The session whose worktree pushes.
        #[arg(long, value_name = "ID")]
        session: String,
        /// The remote's name, or its URL.
        remote: String,
        /// The URL pushed to.
        url: String,
    },
    /// Add the session's trailer to the commit message being prepared.
    PrepareCommitMsg {
        /// The session whose worktree commits.
        #[arg(long, value_name = "ID")]
        session: String,
        /// The commit message file.
        file: PathBuf,
    },
    /// Add the session's trailer to the commit message as written.
    CommitMsg {
        /// The session whose worktree commits.
        #[arg(long, value_name = "ID")]
        session: String,
        /// The commit message file.
        file: PathBuf,
    },
}

pub fn run(hook: Hook) -> anyhow::Result<ExitCode> {
    let result = match hook {
        Hook::PrePush {
            socket,
            session,
            remote,
            url,
        } => hooks::pre_push(
            &socket,
            SessionId::new(session),
            remote,
            url,
            std::io::stdin().lock(),
        ),
        Hook::PrepareCommitMsg { session, file } | Hook::CommitMsg { session, file } => {
            hooks::add_trailer(&SessionId::new(session), &file)
        }
    };
    if let Err(err) = result {
        eprintln!("herder: {err:#}");
    }
    Ok(ExitCode::SUCCESS)
}
