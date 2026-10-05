//! The single herder binary: `herder daemon` runs the daemon, bare `herder` opens the TUI.

mod connect;
mod dev;
mod doctor;
mod fork;
mod hook;
mod pair;
mod service;
mod session;
mod update;
mod vault;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "herder",
    version,
    about = "Run coding agents across many machines"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the herder daemon on this machine.
    Daemon {
        /// Config file [default: $XDG_CONFIG_HOME/herder/daemon.toml].
        #[arg(long, value_name = "PATH", env = "HERDER_CONFIG")]
        config: Option<PathBuf>,
        /// Run as the vault, keeping the sessions hosts replicate to it, as `mode = "vault"`.
        #[arg(long)]
        vault: bool,
    },
    /// Pair a device with the daemon running on this machine, or list and revoke devices.
    Pair(pair::Args),
    /// Pair this device with a daemon, from the link `herder pair` printed on its machine.
    Connect {
        /// The `herder://pair?...` link.
        link: String,
    },
    /// Create, prompt, wait on and archive sessions from scripts.
    Session(session::Args),
    /// Fork a session onto this host: its history goes on in a new session here.
    Fork(fork::Args),
    /// Look after what the vault on this machine holds.
    Vault {
        #[command(subcommand)]
        command: vault::Command,
    },
    /// Manage the systemd user service that runs the daemon at boot.
    Service {
        #[command(subcommand)]
        action: service::Action,
    },
    /// Check that this machine is ready to run herder, with how to fix what is not.
    Doctor {
        /// Config file [default: $XDG_CONFIG_HOME/herder/daemon.toml].
        #[arg(long, value_name = "PATH", env = "HERDER_CONFIG")]
        config: Option<PathBuf>,
    },
    /// Replace this binary with a herder release.
    Update {
        /// Install this version instead of the latest release.
        #[arg(long, value_name = "VERSION")]
        version: Option<String>,
        /// Restart the running service without asking.
        #[arg(long, short)]
        yes: bool,
        /// Allow installing a version older than this one.
        #[arg(long)]
        allow_downgrade: bool,
    },
    /// Tools for developing herder itself.
    Dev {
        #[command(subcommand)]
        command: dev::Command,
    },
    /// Serve herder's task tools over MCP on stdio; the agent CLI of a session runs this.
    #[command(hide = true)]
    Mcp {
        /// The daemon's data dir.
        #[arg(long, value_name = "PATH")]
        data_dir: PathBuf,
        /// The session whose agent runs this.
        #[arg(long, value_name = "ID")]
        session: String,
    },
    /// Run by the git hooks herder installs in session worktrees, and by Claude Code's hook.
    #[command(hide = true)]
    Hook {
        #[command(subcommand)]
        hook: hook::Hook,
    },
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let _ = err.print();
            // Usage errors exit 64 (EX_USAGE), not clap's 2, which `herder session wait`
            // means as "needs you".
            return if err.use_stderr() {
                ExitCode::from(64)
            } else {
                ExitCode::SUCCESS
            };
        }
    };
    let result = match cli.command {
        Some(Command::Daemon { config, vault }) => {
            daemon(config, vault).map(|()| ExitCode::SUCCESS)
        }
        Some(Command::Pair(args)) => pair::run(args).map(|()| ExitCode::SUCCESS),
        Some(Command::Connect { link }) => connect::run(&link).map(|()| ExitCode::SUCCESS),
        Some(Command::Session(args)) => session::run(args),
        Some(Command::Fork(args)) => fork::run(args).map(|()| ExitCode::SUCCESS),
        Some(Command::Vault { command }) => vault::run(command).map(|()| ExitCode::SUCCESS),
        Some(Command::Service { action }) => service::run(action),
        Some(Command::Doctor { config }) => Ok(doctor::run(config)),
        Some(Command::Update {
            version,
            yes,
            allow_downgrade,
        }) => update::run(update::Args {
            version,
            yes,
            allow_downgrade,
        })
        .map(|()| ExitCode::SUCCESS),
        Some(Command::Dev { command }) => dev::run(command),
        Some(Command::Mcp { data_dir, session }) => {
            herder_daemon::mcp::run_shim(&data_dir, herder_protocol::SessionId::new(session))
                .map(|()| ExitCode::SUCCESS)
        }
        Some(Command::Hook { hook }) => hook::run(hook),
        None => herder_tui::run().map(|()| ExitCode::SUCCESS),
    };
    result.unwrap_or_else(|err| {
        eprintln!("herder: {err:#}");
        ExitCode::FAILURE
    })
}

fn daemon(config: Option<PathBuf>, vault: bool) -> anyhow::Result<()> {
    let mut config = herder_daemon::Config::load(config.as_deref())?;
    if vault {
        anyhow::ensure!(
            config.vault.is_none(),
            "a vault does not replicate to another vault; remove the [vault] table"
        );
        config.mode = herder_daemon::config::Mode::Vault;
    }
    herder_daemon::logging::init(&config.log)?;
    match herder_daemon::run(config)? {
        herder_daemon::Exit::Stopped => Ok(()),
        herder_daemon::Exit::Restart => restart(),
    }
}

/// Replaces this process with the herder binary run with the same arguments, so a restarted
/// daemon keeps its process id, as a service manager expects, and reads its config afresh.
fn restart() -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;

    let exe = std::env::current_exe()?;
    // After an update replaced the binary, Linux reports the old path with ` (deleted)`.
    let path = exe.to_string_lossy();
    let exe = path
        .strip_suffix(" (deleted)")
        .map_or(exe.clone(), PathBuf::from);
    let err = std::process::Command::new(&exe)
        .args(std::env::args_os().skip(1))
        .exec();
    Err(anyhow::Error::new(err).context(format!("restarting {}", exe.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_daemon_subcommand() {
        let cli = Cli::try_parse_from(["herder", "daemon"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Daemon { .. })));
    }

    #[test]
    fn parses_config_flag() {
        let cli =
            Cli::try_parse_from(["herder", "daemon", "--config", "/etc/herder.toml"]).unwrap();
        let Some(Command::Daemon { config, .. }) = cli.command else {
            panic!("expected daemon");
        };
        assert_eq!(config, Some(PathBuf::from("/etc/herder.toml")));
    }

    #[test]
    fn parses_doctor_config_flag() {
        let cli = Cli::try_parse_from(["herder", "doctor", "--config", "/etc/herder.toml"]);
        let Some(Command::Doctor { config }) = cli.unwrap().command else {
            panic!("expected doctor");
        };
        assert_eq!(config, Some(PathBuf::from("/etc/herder.toml")));
    }

    #[test]
    fn parses_vault_flag() {
        let cli = Cli::try_parse_from(["herder", "daemon", "--vault"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Daemon { vault: true, .. })
        ));
    }

    #[test]
    fn parses_vault_forget_host() {
        let cli = Cli::try_parse_from(["herder", "vault", "forget-host", "old-box"]);
        assert!(matches!(cli.unwrap().command, Some(Command::Vault { .. })));
        assert!(Cli::try_parse_from(["herder", "vault", "forget-host"]).is_err());
    }

    #[test]
    fn parses_the_claude_hook_the_adapter_passes() {
        let args = ["herder"]
            .into_iter()
            .chain(herder_adapters::claude::PRE_TOOL_USE_ARGS);
        let cli = Cli::try_parse_from(args).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Hook {
                hook: hook::Hook::ClaudePreToolUse
            })
        ));
    }

    #[test]
    fn parses_service_actions() {
        for action in ["install", "uninstall", "status", "restart"] {
            let cli = Cli::try_parse_from(["herder", "service", action]).unwrap();
            assert!(
                matches!(cli.command, Some(Command::Service { .. })),
                "{action}"
            );
        }
    }

    #[test]
    fn parses_pair_flags() {
        let cli = Cli::try_parse_from(["herder", "pair", "--user", "bob", "--role", "member"]);
        assert!(matches!(cli.unwrap().command, Some(Command::Pair(_))));
        for args in [
            &["herder", "pair", "--list"][..],
            &["herder", "pair", "--revoke", "01J"],
            &["herder", "pair", "--host", "devbox"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok(), "{args:?}");
        }
        for args in [
            &["herder", "pair", "--list", "--revoke", "01J"][..],
            &["herder", "pair", "--list", "--user", "bob"],
            &["herder", "pair", "--role", "admin"],
            &["herder", "pair", "--host", "devbox", "--user", "bob"],
            &["herder", "pair", "--host", "devbox", "--role", "owner"],
            &["herder", "pair", "--host", "devbox", "--list"],
        ] {
            assert!(Cli::try_parse_from(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn parses_session_commands() {
        for args in [
            &[
                "herder",
                "session",
                "new",
                "--repo",
                "/r",
                "--mode",
                "full-access",
            ][..],
            &["herder", "session", "list", "--json", "--machine", "box"],
            &[
                "herder",
                "session",
                "--json",
                "wait",
                "01J",
                "--timeout",
                "60",
            ],
            &["herder", "session", "send", "01J"],
            &["herder", "session", "send", "01J", "--approve", "a1"],
            &[
                "herder", "session", "send", "01J", "--answer", "q1", "SQLite",
            ],
            &["herder", "session", "archive", "01J"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok(), "{args:?}");
        }
        for args in [
            &["herder", "session", "new"][..],
            &[
                "herder",
                "session",
                "send",
                "01J",
                "--approve",
                "a1",
                "--deny",
                "a2",
            ],
            &["herder", "session", "send", "01J", "--answer", "q1"],
            &["herder", "session", "new", "--repo", "/r", "--mode", "yolo"],
        ] {
            assert!(Cli::try_parse_from(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn parses_mcp_flags() {
        let cli =
            Cli::try_parse_from(["herder", "mcp", "--data-dir", "/d", "--session", "01J"]).unwrap();
        let Some(Command::Mcp { data_dir, session }) = cli.command else {
            panic!("expected mcp");
        };
        assert_eq!((data_dir, session.as_str()), (PathBuf::from("/d"), "01J"));
        assert!(Cli::try_parse_from(["herder", "mcp", "--data-dir", "/d"]).is_err());
    }

    #[test]
    fn parses_update_flags() {
        let cli = Cli::try_parse_from([
            "herder",
            "update",
            "--version",
            "0.2.0",
            "--yes",
            "--allow-downgrade",
        ])
        .unwrap();
        let Some(Command::Update {
            version,
            yes,
            allow_downgrade,
        }) = cli.command
        else {
            panic!("expected update");
        };
        assert_eq!(version.as_deref(), Some("0.2.0"));
        assert!(yes && allow_downgrade);
    }

    #[test]
    fn parses_dev_record() {
        let cli = Cli::try_parse_from([
            "herder",
            "dev",
            "record",
            "claude",
            "hello",
            "--redact",
            "acct-[0-9]+",
            "--",
            "claude",
            "-p",
            "--output-format",
            "stream-json",
        ])
        .unwrap();
        assert!(matches!(cli.command, Some(Command::Dev { .. })));
        assert!(Cli::try_parse_from(["herder", "dev", "record", "claude", "hello"]).is_err());
    }
}
