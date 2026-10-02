//! `herder dev`: tools for developing herder itself.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Args, Subcommand};
use herder_adapters::fixture::Header;
use herder_adapters::record::{self, Redactor};
use herder_protocol::Timestamp;
use tokio::io::BufReader;

#[derive(Subcommand)]
pub enum Command {
    /// Run a CLI from this terminal and record its stdio as a test fixture.
    ///
    /// Lines typed here go to the CLI's stdin and its stdout lines are shown here; both are
    /// written to the fixture as they happen. Bearer tokens, API keys and email addresses are
    /// redacted from the fixture. End input with Ctrl-D; Ctrl-C stops the CLI, not the
    /// recording.
    Record(RecordArgs),
}

#[derive(Args)]
pub struct RecordArgs {
    /// Provider the CLI belongs to, e.g. `claude`.
    provider: String,
    /// Fixture name; written to fixtures/<provider>/<name>.jsonl.
    name: String,
    /// Write the fixture here instead.
    #[arg(long, value_name = "PATH")]
    out: Option<PathBuf>,
    /// Also redact matches of this regex; repeatable.
    #[arg(long, value_name = "REGEX")]
    redact: Vec<String>,
    /// JSON key to ignore when replay matches lines sent to the CLI; repeatable.
    #[arg(long, value_name = "KEY")]
    ignore_key: Vec<String>,
    /// Version of the CLI being recorded, for the fixture header.
    #[arg(long, value_name = "VERSION")]
    cli_version: Option<String>,
    /// The CLI and its arguments.
    #[arg(last = true, required = true, value_name = "COMMAND")]
    command: Vec<String>,
}

pub fn run(command: Command) -> anyhow::Result<ExitCode> {
    match command {
        Command::Record(args) => {
            let runtime = tokio::runtime::Runtime::new()?;
            let code = runtime.block_on(record(args));
            // Reading the terminal blocks a thread that only returns on the next line typed;
            // do not wait for it.
            runtime.shutdown_background();
            code
        }
    }
}

async fn record(args: RecordArgs) -> anyhow::Result<ExitCode> {
    let redactor = Redactor::new(&args.redact).context("invalid --redact pattern")?;
    let path = args.out.unwrap_or_else(|| {
        PathBuf::from("fixtures")
            .join(&args.provider)
            .join(format!("{}.jsonl", args.name))
    });
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        tokio::fs::create_dir_all(dir)
            .await
            .with_context(|| format!("creating {}", dir.display()))?;
    }
    let fixture = tokio::fs::File::create(&path)
        .await
        .with_context(|| format!("creating {}", path.display()))?;
    let header = Header {
        provider: args.provider,
        cli_version: args.cli_version,
        recorded_at: Some(Timestamp::now()),
        ignore_keys: args.ignore_key,
    };
    let (program, program_args) = args.command.split_first().context("no command to record")?;
    let mut command = tokio::process::Command::new(program);
    command.args(program_args);

    // Ctrl-C reaches the CLI too, as it shares the terminal; outlive it to record its exit.
    tokio::spawn(async { while tokio::signal::ctrl_c().await.is_ok() {} });

    let code = record::record(
        command,
        &header,
        &redactor,
        BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
        fixture,
    )
    .await
    .with_context(|| format!("recording {program}"))?;
    eprintln!("herder: recorded {}", path.display());
    Ok(code
        .and_then(|code| u8::try_from(code).ok())
        .map_or(ExitCode::FAILURE, ExitCode::from))
}
