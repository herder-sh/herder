//! The daemon the Swift and Kotlin samples connect to, as a sidecar process: see
//! `tests/support`. Prints a pairing link, the repository's path, the account to run on, the
//! account whose turns run until interrupted, a member's pairing link and a repository to
//! import a skill from, one per line, then runs until its stdin closes.
//!
//! `fake_daemon [NAME]` names the host (`fake-host` by default), to run several side by side.
//! `fake_daemon --share` runs two, `fake-host-1` and `fake-host-2`, and prints instead the link
//! a device paired with both shares (`Client::share`), which pairs with both at once; the
//! repository is the first's, and the first has a session waiting on an approval.
//! `fake_daemon --demo` runs the demo fleet of the site's screenshots ([`demo`]) and prints
//! only the link a device paired with all of it shares.

#[path = "fake_daemon/demo.rs"]
mod demo;
#[path = "../tests/support/mod.rs"]
mod support;

use std::time::Duration;

use anyhow::{Context, Result, bail};
use herder_client_core::{Client, ConnectionState, PairResult};
use herder_protocol::{AccountId, CommandBody, CommandResult, HostId};
use tokio::io::AsyncReadExt;

#[tokio::main]
async fn main() -> Result<()> {
    let (daemons, link) = match std::env::args().nth(1).as_deref() {
        Some("--demo") => {
            let demo = demo::Demo::start().await?;
            println!("{}", demo.link);
            tokio::io::stdin().read_to_end(&mut Vec::new()).await?;
            return demo.stop().await;
        }
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
    println!(
        "{link}\n{}\n{}\n{}\n{}\n{}",
        daemons[0].repo,
        support::ACCOUNT,
        support::HOLD_ACCOUNT,
        daemons[0].member_link,
        daemons[0].skill_source
    );
    tokio::io::stdin().read_to_end(&mut Vec::new()).await?;
    for daemon in daemons {
        daemon.stop().await?;
    }
    Ok(())
}

/// Pairs a device with every daemon, starts a session on the first that waits on an approval,
/// and returns the link the device shares.
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
    let host = HostId::new("fake-host-1");
    let created = client
        .send(
            host.clone(),
            CommandBody::CreateSession {
                repo: Some(daemons[0].repo.clone()),
                project_id: None,
                branch: None,
                account_id: Some(AccountId::new(support::APPROVAL_ACCOUNT)),
                provider: None,
                model: None,
                permission_mode: None,
                max_children: None,
                failover_pin: None,
            },
        )
        .await?;
    let CommandResult::SessionCreated { session_id } = created else {
        bail!("the daemon did not create a session: {created:?}");
    };
    client
        .send(
            host,
            CommandBody::SendPrompt {
                session_id,
                text: "Run the tests.".into(),
                images: Vec::new(),
            },
        )
        .await?;
    let shared = client.share().await?;
    if let Some(skipped) = shared.skipped.first() {
        bail!("{} was not shared: {}", skipped.host_id, skipped.error);
    }
    Ok(shared.link.to_string())
}
