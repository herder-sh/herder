//! What opening a client costs against a machine used for months (`fake_daemon --load`), as
//! the apps open it: every listed session followed. The measurement prints, per launch, how
//! many events the offline cache held, how many the machine sent again, and how long both took:
//!
//! `LOAD_SESSIONS=335 LOAD_EVENTS=16000 cargo test --release -p herder-ffi --test launch_load -- --ignored --nocapture`

#[path = "../examples/fake_daemon/load.rs"]
mod load;
mod support;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context;
use herder_client_core::{Client, ConnectionState, FleetLink};
use herder_daemon::auth::authenticator;
use herder_protocol::{HostId, SessionId, SessionStatus};

struct Opened {
    /// Events the first update of each subscription carried: what the cache held.
    cached: u64,
    /// Events later updates carried: what the machine sent.
    sent: u64,
    /// Until every session was at its head.
    caught_up: Duration,
    /// Until every subscription gave its first update.
    first: Duration,
}

async fn follow(client: &Client, host: &HostId, started: Instant) -> anyhow::Result<Opened> {
    let changes = client.changes();
    let heads = loop {
        let machine = client.machines().into_iter().find(|m| &m.host_id == host);
        if let Some(machine) = machine
            && machine.connection == ConnectionState::Connected
            && !machine.sessions.is_empty()
        {
            break machine.sessions;
        }
        anyhow::ensure!(changes.next().await, "the client stopped");
    };
    let listed = started.elapsed();
    let cached = Arc::new(AtomicU64::new(0));
    let sent = Arc::new(AtomicU64::new(0));
    let first_done = Arc::new(AtomicU64::new(0));
    let mut tasks = Vec::new();
    let want: HashMap<SessionId, u64> = heads
        .iter()
        .map(|head| (head.session_id.clone(), head.head_seq))
        .collect();
    for (session_id, head_seq) in want {
        let subscription = client.subscribe_session(host.clone(), session_id)?;
        let (cached, sent, first_done) = (cached.clone(), sent.clone(), first_done.clone());
        tasks.push(tokio::spawn(async move {
            let mut first = true;
            let mut last = 0;
            while last < head_seq {
                let Some(update) = subscription.next().await else {
                    break;
                };
                let count = update.events.len() as u64;
                if let Some(event) = update.events.last() {
                    last = event.seq;
                }
                if first {
                    cached.fetch_add(count, Ordering::Relaxed);
                    first_done.fetch_max(started.elapsed().as_millis() as u64, Ordering::Relaxed);
                    first = false;
                } else {
                    sent.fetch_add(count, Ordering::Relaxed);
                }
            }
        }));
    }
    for task in tasks {
        task.await?;
    }
    println!("  listed after {listed:?}");
    Ok(Opened {
        cached: cached.load(Ordering::Relaxed),
        sent: sent.load(Ordering::Relaxed),
        caught_up: started.elapsed(),
        first: Duration::from_millis(first_done.load(Ordering::Relaxed)),
    })
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement, not a check"]
async fn opening_a_used_machine() -> anyhow::Result<()> {
    let sessions: usize = std::env::var("LOAD_SESSIONS").map_or(Ok(60), |v| v.parse())?;
    let events: usize = std::env::var("LOAD_EVENTS").map_or(Ok(4000), |v| v.parse())?;
    let seeding = Instant::now();
    let daemon = support::FakeDaemon::start_seeded("load", |store, repo| {
        load::seed(store, "load", repo, sessions, events)
    })
    .await?;
    println!("seeded {sessions} sessions in {:?}", seeding.elapsed());
    let host = HostId::new("load");
    let profile = tempfile::tempdir()?;
    let dir = profile.path().display().to_string();
    let link: FleetLink = daemon.link.parse()?;
    let machine = link
        .machines
        .into_iter()
        .next()
        .context("the link names no machine")?;

    // First launch on a new device: nothing cached.
    let started = Instant::now();
    let client = Client::open(dir.clone(), "load".into())?;
    authenticator::sign_in(
        &daemon.auth,
        &client,
        machine.hosts,
        &machine.fingerprint,
        support::OWNER,
    )
    .await?;
    let opened = follow(&client, &host, started).await?;
    println!(
        "first launch: cached {} sent {} first updates {:?} caught up {:?}",
        opened.cached, opened.sent, opened.first, opened.caught_up
    );

    let view = Instant::now();
    for _ in 0..100 {
        std::hint::black_box(client.machines());
    }
    println!("machines(): {:?} a call", view.elapsed() / 100);

    // Change notifications while nothing happens.
    let changes = client.changes();
    let idle = Instant::now();
    let mut notified = 0;
    while idle.elapsed() < Duration::from_secs(35) {
        if tokio::time::timeout(Duration::from_secs(1), changes.next())
            .await
            .is_ok()
        {
            notified += 1;
        }
    }
    println!("idle: {notified} change notifications in 35 s");

    let saving = Instant::now();
    client.suspend();
    println!("suspend(): {:?}", saving.elapsed());
    let cache: u64 = std::fs::read_dir(profile.path().join("cache"))?
        .filter_map(|entry| entry.ok()?.metadata().ok())
        .map(|meta| meta.len())
        .sum();
    println!("cache: {:.1} MB", cache as f64 / 1e6);
    drop(client);
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Every later launch.
    let started = Instant::now();
    let client = Client::open(dir, "load".into())?;
    println!("open(): {:?}", started.elapsed());
    let opened = follow(&client, &host, started).await?;
    println!(
        "next launch: cached {} sent {} first updates {:?} caught up {:?}",
        opened.cached, opened.sent, opened.first, opened.caught_up
    );
    daemon.stop().await
}

/// The load is the journal it says it is: the apps' measurements stand on its size.
#[tokio::test(flavor = "multi_thread")]
async fn a_loaded_machine_lists_the_journal_it_was_seeded_with() -> anyhow::Result<()> {
    let daemon = support::FakeDaemon::start_seeded("load", |store, repo| {
        load::seed(store, "load", repo, 24, 400)
    })
    .await?;
    let host = HostId::new("load");
    let profile = tempfile::tempdir()?;
    let link: FleetLink = daemon.link.parse()?;
    let machine = link
        .machines
        .into_iter()
        .next()
        .context("the link names no machine")?;
    let client = Client::open(profile.path().display().to_string(), "load".into())?;
    authenticator::sign_in(
        &daemon.auth,
        &client,
        machine.hosts,
        &machine.fingerprint,
        support::OWNER,
    )
    .await?;
    let opened = follow(&client, &host, Instant::now()).await?;
    let heads = client
        .machines()
        .into_iter()
        .find(|machine| machine.host_id == host)
        .context("the machine is not listed")?
        .sessions;
    assert_eq!(heads.len(), 24);
    let archived = heads
        .iter()
        .filter(|head| head.status == SessionStatus::Archived)
        .count();
    assert_eq!(archived, 20, "five in six are archived");
    let first = heads
        .iter()
        .find(|head| head.session_id == SessionId::new("load-load-0000"))
        .map(|head| head.head_seq);
    assert!(
        first.is_some_and(|seq| (400..442).contains(&seq)),
        "the first session has the events asked for, to the end of its turn: {first:?}"
    );
    let total: u64 = heads.iter().map(|head| head.head_seq).sum();
    assert_eq!(opened.cached + opened.sent, total, "every event arrives");
    daemon.stop().await
}
