//! The single herder binary: `herder daemon` runs the daemon, bare `herder` opens the TUI.

mod dev;
mod pair;
mod service;
mod update;

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
    },
    /// Pair a device with the daemon running on this machine, or list and revoke devices.
    Pair(pair::Args),
    /// Manage the systemd user service that runs the daemon at boot.
    Service {
        #[command(subcommand)]
        action: service::Action,
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
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Some(Command::Daemon { config }) => daemon(config).map(|()| ExitCode::SUCCESS),
        Some(Command::Pair(args)) => pair::run(args).map(|()| ExitCode::SUCCESS),
        Some(Command::Service { action }) => service::run(action),
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
        None => {
            println!("{}", herder_tui::run());
            Ok(ExitCode::SUCCESS)
        }
    };
    result.unwrap_or_else(|err| {
        eprintln!("herder: {err:#}");
        ExitCode::FAILURE
    })
}

fn daemon(config: Option<PathBuf>) -> anyhow::Result<()> {
    let config = herder_daemon::Config::load(config.as_deref())?;
    herder_daemon::logging::init(&config.log)?;
    herder_daemon::run(config)
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
        let Some(Command::Daemon { config }) = cli.command else {
            panic!("expected daemon");
        };
        assert_eq!(config, Some(PathBuf::from("/etc/herder.toml")));
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
        ] {
            assert!(Cli::try_parse_from(args).is_ok(), "{args:?}");
        }
        for args in [
            &["herder", "pair", "--list", "--revoke", "01J"][..],
            &["herder", "pair", "--list", "--user", "bob"],
            &["herder", "pair", "--role", "admin"],
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
