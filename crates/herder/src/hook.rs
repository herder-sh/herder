//! `herder hook`: the client side of the git hooks in session worktrees, and of the Claude
//! Code hook herder passes to the Claude sessions it runs.
//!
//! A hook must not break git or Claude Code when herder cannot help: every failure is a
//! warning on stderr, and the command still succeeds.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::Subcommand;
use herder_adapters::claude;
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
    /// Answer Claude Code's PreToolUse hook; reads its input on stdin.
    ClaudePreToolUse,
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
        Hook::ClaudePreToolUse => claude_pre_tool_use(),
    };
    if let Err(err) = result {
        eprintln!("herder: {err:#}");
    }
    Ok(ExitCode::SUCCESS)
}

/// Prints the hook's decision for the call on stdin, or nothing when it has none.
fn claude_pre_tool_use() -> anyhow::Result<()> {
    let input: serde_json::Value =
        serde_json::from_reader(std::io::stdin().lock()).context("reading the hook input")?;
    if let Some(output) = claude::pre_tool_use(&input) {
        println!("{output}");
    }
    Ok(())
}
