//! The single herder binary: `herder daemon` runs the daemon, bare `herder` opens the TUI.

mod account;
mod dev;
mod doctor;
mod fork;
mod hook;
mod hub;
mod login;
mod pair;
mod secret;
mod service;
mod session;
mod update;
mod vpn;

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
        /// Run as the hub, keeping the sessions hosts replicate to it, as `mode = "hub"`.
        #[arg(long)]
        hub: bool,
    },
    /// Pair a host with the hub running on this machine, or list and revoke devices.
    Pair(pair::Args),
    /// Give the daemon on this machine a new owner, from the herder app or `herder login`,
    /// though it has one: for someone who lost every device.
    Claim {
        /// Daemon config file, to find its data dir [default: $XDG_CONFIG_HOME/herder/daemon.toml].
        #[arg(long, value_name = "PATH", env = "HERDER_CONFIG")]
        config: Option<PathBuf>,
    },
    /// Sign this terminal in as a person: with its own passkey, made when it claims a machine
    /// from a `herder://own` link, or by a code an app of yours approves.
    Login(login::Args),
    /// Create, prompt, wait on and archive sessions from scripts.
    Session(session::Args),
    /// Manage a paired machine's provider accounts.
    Account(account::Args),
    /// Fork a session onto this host: its history goes on in a new session here.
    Fork(fork::Args),
    /// Use the secrets this host's hub grants it, from a session herder runs.
    Secret {
        #[command(subcommand)]
        command: secret::Command,
    },
    /// Run a hub on this machine, next to the daemon, and look after what it holds.
    Hub {
        #[command(subcommand)]
        command: hub::Command,
    },
    /// Manage the service that runs the daemon at boot: systemd on Linux, launchd on macOS.
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
    /// Reach your machines through your own WireGuard server, for herder only.
    Vpn {
        #[command(subcommand)]
        command: vpn::Command,
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
        Some(Command::Daemon { config, hub }) => daemon(config, hub).map(|()| ExitCode::SUCCESS),
        Some(Command::Pair(args)) => pair::run(args).map(|()| ExitCode::SUCCESS),
        Some(Command::Claim { config }) => pair::claim(config).map(|()| ExitCode::SUCCESS),
        Some(Command::Login(args)) => login::run(args).map(|()| ExitCode::SUCCESS),
        Some(Command::Session(args)) => session::run(args),
        Some(Command::Account(args)) => account::run(args).map(|()| ExitCode::SUCCESS),
        Some(Command::Fork(args)) => fork::run(args).map(|()| ExitCode::SUCCESS),
        Some(Command::Secret { command }) => secret::run(command).map(|()| ExitCode::SUCCESS),
        Some(Command::Hub { command }) => hub::run(command).map(|()| ExitCode::SUCCESS),
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
        Some(Command::Vpn { command }) => vpn::run(command).map(|()| ExitCode::SUCCESS),
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

fn daemon(config: Option<PathBuf>, hub: bool) -> anyhow::Result<()> {
    let mut config = herder_daemon::Config::load(config.as_deref())?;
    if hub {
        anyhow::ensure!(
            config.hub.is_none(),
            "a hub does not replicate to another hub; remove the [hub] table"
        );
        config.mode = herder_daemon::config::Mode::Hub;
    }
    let own = if hub {
        hub::installed_service()
    } else {
        Some(herder_daemon::service::Service::daemon())
    };
    // Only the service herder installed restarts onto a new binary, as `update_daemon` needs.
    if own
        .as_ref()
        .is_some_and(herder_daemon::service::runs_this_process)
    {
        config.self_update = Some(herder_daemon::update::SelfUpdate {
            exe: std::env::current_exe()?,
            base: herder_daemon::update::release::download_base(),
        });
    }
    herder_daemon::logging::init(&config.log)?;
    match herder_daemon::run(config)? {
        herder_daemon::Exit::Stopped => Ok(()),
        herder_daemon::Exit::Restart => restart(),
        herder_daemon::Exit::Updated => {
            if let Some(sibling) = sibling(hub)
                && let Err(err) = herder_daemon::service::restart(&sibling)
            {
                eprintln!("herder: restarting the {}: {err:#}", sibling.name);
            }
            restart()
        }
    }
}

/// The service next to this daemon that an update restarts with it, as `herder update` does,
/// so a daemon and its hub run the same version: an installed hub's next to a daemon, the
/// running daemon's next to a hub.
fn sibling(hub: bool) -> Option<herder_daemon::service::Service> {
    if hub {
        herder_daemon::service::is_active().then(herder_daemon::service::Service::daemon)
    } else {
        hub::installed_service()
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
    fn parses_hub_flag() {
        let cli = Cli::try_parse_from(["herder", "daemon", "--hub"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Daemon { hub: true, .. })
        ));
    }

    #[test]
    fn parses_hub_forget_host() {
        let cli = Cli::try_parse_from(["herder", "hub", "forget-host", "old-box"]);
        assert!(matches!(cli.unwrap().command, Some(Command::Hub { .. })));
        assert!(Cli::try_parse_from(["herder", "hub", "forget-host"]).is_err());
    }

    #[test]
    fn parses_hub_install_and_uninstall() {
        for args in [
            &["herder", "hub", "install"][..],
            &["herder", "hub", "install", "--port", "7500"],
            &["herder", "hub", "uninstall"],
            &["herder", "hub", "uninstall", "--purge"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok(), "{args:?}");
        }
        assert!(Cli::try_parse_from(["herder", "hub", "install", "--port", "x"]).is_err());
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
    fn login_takes_the_local_daemon_a_link_or_an_address_with_its_fingerprint() {
        let local = Cli::try_parse_from(["herder", "login"]);
        assert!(matches!(local.unwrap().command, Some(Command::Login(_))));
        let remote =
            Cli::try_parse_from(["herder", "login", "10.0.4.17:7447", "--fingerprint", "ab"]);
        assert!(matches!(remote.unwrap().command, Some(Command::Login(_))));
        let link = Cli::try_parse_from(["herder", "login", "herder://own?host=a&fp=ab&code=c"]);
        assert!(matches!(link.unwrap().command, Some(Command::Login(_))));
        assert!(Cli::try_parse_from(["herder", "login", "--fingerprint", "ab"]).is_err());
    }

    #[test]
    fn parses_pair_flags() {
        let cli = Cli::try_parse_from(["herder", "pair", "--host", "devbox"]);
        assert!(matches!(cli.unwrap().command, Some(Command::Pair(_))));
        for args in [
            &["herder", "pair", "--list"][..],
            &["herder", "pair", "--revoke", "01J"],
            &["herder", "pair", "--host", "devbox"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok(), "{args:?}");
        }
        for args in [
            &["herder", "pair"][..],
            &["herder", "pair", "--list", "--revoke", "01J"],
            &["herder", "pair", "--user", "bob"],
            &["herder", "pair", "--role", "owner"],
            &["herder", "pair", "--host", "devbox", "--list"],
        ] {
            assert!(Cli::try_parse_from(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn parses_account_remove() {
        for args in [
            &["herder", "account", "remove", "work"][..],
            &[
                "herder",
                "account",
                "remove",
                "Work account",
                "--machine",
                "box",
            ],
        ] {
            assert!(
                matches!(
                    Cli::try_parse_from(args).map(|cli| cli.command),
                    Ok(Some(Command::Account(_)))
                ),
                "{args:?}"
            );
        }
        assert!(Cli::try_parse_from(["herder", "account", "remove"]).is_err());
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
    fn parses_secret_run() {
        let cli = Cli::try_parse_from([
            "herder",
            "secret",
            "run",
            "TOKEN",
            "prod/KEY",
            "--",
            "sh",
            "-c",
            "echo $TOKEN",
        ]);
        let Some(Command::Secret { .. }) = cli.unwrap().command else {
            panic!("expected secret");
        };
        for args in [
            &["herder", "secret", "run", "TOKEN"][..],
            &["herder", "secret", "run", "--", "env"],
        ] {
            assert!(Cli::try_parse_from(args).is_err(), "{args:?}");
        }
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
