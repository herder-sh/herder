//! Structured logging to stderr.

use std::io::IsTerminal;

use anyhow::{Context, Result, anyhow};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

use crate::config::{LogConfig, LogFormat};

/// Installs the global `tracing` subscriber described by `config`, with the layer that
/// shares herder's spans and errors while diagnostics are on ([`crate::telemetry`]); the
/// log level filters the log only, not what is shared.
pub fn init(config: &LogConfig) -> Result<()> {
    let filter = EnvFilter::builder()
        .parse(&config.level)
        .with_context(|| format!("invalid log level {:?}", config.level))?;
    let log = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);
    let log = match config.format {
        LogFormat::Pretty => log.with_ansi(std::io::stderr().is_terminal()).boxed(),
        LogFormat::Json => log.json().boxed(),
    };
    tracing_subscriber::registry()
        .with(log.with_filter(filter))
        .with(herder_telemetry::layer())
        .try_init()
        .map_err(|err| anyhow!("installing the log subscriber: {err}"))
}
