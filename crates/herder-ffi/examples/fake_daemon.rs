//! The daemon the Swift and Kotlin samples connect to, as a sidecar process: see
//! `tests/support`. Prints its `herder://fleet` link, its owner's passkey, the repository's
//! path, the account to run on, the account whose turns run until interrupted, a member's
//! passkey, a repository to import a skill from and the account that asks a question, one per
//! line, then runs until stdin closes. A passkey is the recorded test passkey of its person, as
//! `Authenticator::export` gives it, for a client under test to sign in with in place of Face
//! ID.
//!
//! `fake_daemon [NAME]` names the host (`fake-host` by default), to run several side by side.
//! `fake_daemon --share` runs two, `fake-host-1` and `fake-host-2`, with the same owner, and
//! prints instead the link a device signed in to both shares (`Client::share`); the repository
//! is the first's, and the first has a session waiting on an approval, whose id it prints last.
//! `fake_daemon --demo` runs the demo fleet of the site's screenshots ([`demo`]) and prints only
//! the link a device signed in to all of it shares and the user's passkey; `fake_daemon --busy`
//! adds the load the apps are profiled under.
//! `fake_daemon --vault` runs a vault that keeps secrets ([`vault`]) and prints only its link
//! and its owner's passkey.
//! `fake_daemon --own` runs `fake-own`, which waits for a new owner as after `herder claim`, and
//! prints only the `herder://own` link that claim prints.
//! `fake_daemon --record-demo PATH` records the demo fleet for the apps' demo into `PATH`
//! ([`record`]) and exits.

#[path = "fake_daemon/demo.rs"]
mod demo;
#[path = "fake_daemon/record.rs"]
mod record;
#[path = "../tests/support/mod.rs"]
mod support;
#[path = "fake_daemon/vault.rs"]
mod vault;

use std::time::Duration;

use anyhow::{Context, Result, bail};
use herder_client_core::{Client, ConnectionState, FleetLink, OwnUri};
use herder_daemon::auth::authenticator;
use herder_protocol::{AccountId, CommandBody, CommandResult, HostId, SessionId};
use tokio::io::AsyncReadExt;

#[tokio::main]
async fn main() -> Result<()> {
    let (daemons, link, waiting) = match std::env::args().nth(1).as_deref() {
        Some(flag @ ("--demo" | "--busy")) => {
            let demo = demo::Demo::start(flag == "--busy").await?;
            println!("{}\n{}", demo.link, demo.passkey);
            tokio::io::stdin().read_to_end(&mut Vec::new()).await?;
            return demo.stop().await;
        }
        Some("--record-demo") => {
            let out = std::env::args()
                .nth(2)
                .context("--record-demo needs a path")?;
            return record::record(std::path::Path::new(&out)).await;
        }
        Some("--vault") => {
            let vault = vault::Vault::start().await?;
            println!("{}\n{}", vault.link, vault.passkey);
            tokio::io::stdin().read_to_end(&mut Vec::new()).await?;
            return vault.stop().await;
        }
        Some("--own") => {
            let daemon = support::FakeDaemon::start("fake-own").await?;
            println!("{}", own_link(&daemon)?);
            tokio::io::stdin().read_to_end(&mut Vec::new()).await?;
            return daemon.stop().await;
        }
        Some("--share") => {
            let daemons = vec![
                support::FakeDaemon::start("fake-host-1").await?,
                support::FakeDaemon::start("fake-host-2").await?,
            ];
            let (link, waiting) = share(&daemons).await?;
            (daemons, link, Some(waiting))
        }
        name => {
            let daemon = support::FakeDaemon::start(name.unwrap_or("fake-host")).await?;
            let link = daemon.link.clone();
            (vec![daemon], link, None)
        }
    };
    println!(
        "{link}\n{}\n{}\n{}\n{}\n{}\n{}",
        daemons[0].passkey,
        daemons[0].repo,
        support::ACCOUNT,
        support::HOLD_ACCOUNT,
        daemons[0].member_passkey,
        daemons[0].skill_source
    );
    println!("{}", support::QUESTION_ACCOUNT);
    if let Some(waiting) = waiting {
        println!("{waiting}");
    }
    tokio::io::stdin().read_to_end(&mut Vec::new()).await?;
    for daemon in daemons {
        daemon.stop().await?;
    }
    Ok(())
}

/// Signs a device in to every daemon as their owner, starts a session on the first that waits
/// on an approval, and returns the link the device shares and that session's id.
async fn share(daemons: &[support::FakeDaemon]) -> Result<(String, SessionId)> {
    let profile = tempfile::tempdir()?;
    let client = Client::open(profile.path().display().to_string(), "fake-daemon".into())?;
    for daemon in daemons {
        let link: FleetLink = daemon.link.parse()?;
        let machine = link.machines.into_iter().next().context("an empty link")?;
        authenticator::sign_in(
            &daemon.auth,
            &client,
            machine.hosts,
            &machine.fingerprint,
            support::OWNER,
        )
        .await
        .context("the sharing device did not sign in")?;
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
                failover_pin: None,
                chat: false,
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
                session_id: session_id.clone(),
                text: "Run the tests.".into(),
                images: Vec::new(),
                files: Vec::new(),
            },
        )
        .await?;
    let shared = client
        .share()
        .context("the sharing device has no machines")?;
    Ok((shared.to_string(), session_id))
}

/// Opens `daemon` to a new owner, as `herder claim` does, and returns the link it prints.
fn own_link(daemon: &support::FakeDaemon) -> Result<String> {
    let link: FleetLink = daemon.link.parse()?;
    let machine = link.machines.into_iter().next().context("an empty link")?;
    let (code, _) = daemon.auth.open_recovery()?;
    let uri = OwnUri {
        hosts: machine.hosts,
        fingerprint: machine.fingerprint,
        code,
        name: Some("fake-own".into()),
    };
    Ok(uri.to_string())
}
