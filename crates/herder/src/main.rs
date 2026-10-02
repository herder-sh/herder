//! The single herder binary: `herder daemon` runs the daemon, bare `herder` opens the TUI.

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
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Some(Command::Daemon { config }) => match daemon(config) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("herder: {err:#}");
                ExitCode::FAILURE
            }
        },
        None => {
            println!("{}", herder_tui::run());
            ExitCode::SUCCESS
        }
    }
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
}
