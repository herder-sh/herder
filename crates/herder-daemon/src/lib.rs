//! Daemon runtime: the WebSocket server and the agent sessions it hosts.

pub mod config;
pub mod data_dir;
pub mod logging;
pub mod session;

use std::sync::Arc;

use anyhow::{Context, Result};
use herder_protocol::{Event, Item, ItemId, SessionHead, SessionId};
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

pub use config::Config;
pub use data_dir::DataDir;

/// Runs the daemon until SIGTERM or Ctrl-C, then shuts down cleanly.
pub fn run(config: Config) -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the tokio runtime")?
        .block_on(async {
            let shutdown = CancellationToken::new();
            let mut sigterm =
                signal(SignalKind::terminate()).context("installing the SIGTERM handler")?;
            let token = shutdown.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = sigterm.recv() => info!("received SIGTERM, shutting down"),
                    result = tokio::signal::ctrl_c() => match result {
                        Ok(()) => info!("received Ctrl-C, shutting down"),
                        Err(err) => error!("cannot listen for Ctrl-C, shutting down: {err}"),
                    },
                }
                token.cancel();
            });
            serve(&config, shutdown).await
        })
}

/// Opens the data dir and runs until `shutdown` is cancelled. Later components hang off
/// `shutdown`.
pub async fn serve(config: &Config, shutdown: CancellationToken) -> Result<()> {
    let data_dir = DataDir::open(&config.data_dir)?;
    let store_path = data_dir.root().join("db/herder.db");
    let store = herder_store::Store::open(&store_path)
        .with_context(|| format!("opening the journal {}", store_path.display()))?;
    // Adapters register here as they land (Claude: P1.6), accounts with the accounts
    // component, and the sink becomes the WebSocket hub (P1.2).
    let setup = session::Setup {
        store,
        adapters: session::Adapters::new(),
        accounts: session::Accounts::new(),
        sink: Arc::new(NoClients),
        turn_ids: session::ulid_turn_ids(),
    };
    let _sessions = session::SessionManager::open(setup, shutdown.clone()).await?;
    info!(
        host_id = %data_dir.host_id(),
        data_dir = %data_dir.root().display(),
        listen = %config.listen,
        "herder daemon started"
    );
    shutdown.cancelled().await;
    info!("herder daemon stopped");
    Ok(())
}

/// The event sink until clients can connect: nobody to publish to.
struct NoClients;

impl session::EventSink for NoClients {
    fn event(&self, _: &Event) {}
    fn snapshot(&self, _: &SessionId, _: &Item) {}
    fn delta(&self, _: &SessionId, _: &ItemId, _: &str) {}
    fn sessions_changed(&self, _: &[SessionHead]) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn serve_returns_once_shutdown_is_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            data_dir: tmp.path().join("data"),
            log: config::LogConfig::default(),
        };
        let shutdown = CancellationToken::new();
        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            async move { serve(&config, shutdown).await }
        });
        shutdown.cancel();
        task.await.unwrap().unwrap();
        assert!(tmp.path().join("data/host-id").is_file());
        assert!(tmp.path().join("data/db/herder.db").is_file());
    }
}
