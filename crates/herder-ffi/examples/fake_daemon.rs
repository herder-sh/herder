//! The daemon the Swift and Kotlin samples connect to, as a sidecar process: see
//! `tests/support`. Prints a pairing link, the repository's path and the account to run on, one
//! per line, then runs until its stdin closes.
//!
//! `fake_daemon [NAME]` names the host (`fake-host` by default), to run several side by side.
//! `fake_daemon --share` runs two, `fake-host-1` and `fake-host-2`, and prints instead the link
//! a device paired with both shares (`Client::share`), which pairs with both at once; the
//! repository is the first's.

#[path = "../tests/support/mod.rs"]
mod support;

use std::time::Duration;

use anyhow::{Context, Result, bail};
use herder_client_core::{Client, ConnectionState, PairResult};
use tokio::io::AsyncReadExt;

#[tokio::main]
async fn main() -> Result<()> {
    let (daemons, link) = match std::env::args().nth(1).as_deref() {
        Some("--share") => {
            let daemons = vec![
                support::FakeDaemon::start("fake-host-1").await?,
                support::FakeDaemon::start("fake-host-2").await?,
            ];
            let link = share(&daemons).await?;
            (daemons, link)
        }
        name => {
            let daemon = support::FakeDaemon::start(name.unwrap_or("fake-host")).await?;
            let link = daemon.link.clone();
            (vec![daemon], link)
        }
    };
    println!("{link}\n{}\n{}", daemons[0].repo, support::ACCOUNT);
    tokio::io::stdin().read_to_end(&mut Vec::new()).await?;
    for daemon in daemons {
        daemon.stop().await?;
    }
    Ok(())
}

/// Pairs a device with every daemon and returns the link it shares.
async fn share(daemons: &[support::FakeDaemon]) -> Result<String> {
    let profile = tempfile::tempdir()?;
    let client = Client::open(profile.path().display().to_string(), "fake-daemon".into())?;
    for daemon in daemons {
        for result in client.pair(daemon.link.clone()).await? {
            if let PairResult::Failed { error, .. } = result {
                bail!("the sharing device did not pair: {error}");
            }
        }
    }
    let changes = client.changes();
    tokio::time::timeout(Duration::from_secs(30), async {
        while !client
            .machines()
            .iter()
            .all(|machine| machine.connection == ConnectionState::Connected)
        {
            if !changes.next().await {
                break;
            }
        }
    })
    .await
    .context("the sharing device did not connect")?;
    let shared = client.share().await?;
    if let Some(skipped) = shared.skipped.first() {
        bail!("{} was not shared: {}", skipped.host_id, skipped.error);
    }
    Ok(shared.link.to_string())
}
