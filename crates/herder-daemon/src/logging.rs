//! Structured logging to stderr.

use std::io::IsTerminal;

use anyhow::{Context, Result, anyhow};
use tracing_subscriber::EnvFilter;

use crate::config::{LogConfig, LogFormat};

/// Installs the global `tracing` subscriber described by `config`.
pub fn init(config: &LogConfig) -> Result<()> {
    let filter = EnvFilter::builder()
        .parse(&config.level)
        .with_context(|| format!("invalid log level {:?}", config.level))?;
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    match config.format {
        LogFormat::Pretty => builder
            .with_ansi(std::io::stderr().is_terminal())
            .try_init(),
        LogFormat::Json => builder.json().try_init(),
    }
    .map_err(|err| anyhow!("installing the log subscriber: {err}"))
}
