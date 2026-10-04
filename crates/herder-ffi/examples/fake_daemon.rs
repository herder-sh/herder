//! The daemon the Swift and Kotlin samples connect to, as a sidecar process: see
//! `tests/support`. Prints a pairing link, the repository's path, the account to run on and the
//! account whose turns run until interrupted, one per line, then runs until its stdin closes.

#[path = "../tests/support/mod.rs"]
mod support;

use tokio::io::AsyncReadExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let daemon = support::FakeDaemon::start().await?;
    println!(
        "{}\n{}\n{}\n{}",
        daemon.link,
        daemon.repo,
        support::ACCOUNT,
        support::HOLD_ACCOUNT
    );
    tokio::io::stdin().read_to_end(&mut Vec::new()).await?;
    daemon.stop().await
}
