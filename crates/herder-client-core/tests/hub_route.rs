//! A machine reached through its hub: the client adds the route once the hub says it
//! relays to the machine, falls back to it when the machine's own address stops answering,
//! works through it as it does directly, and moves back once the address answers again.
//!
//! The hub and the host are `herder_daemon::hub::serve` and `herder_daemon::serve`, the
//! functions `herder daemon` runs, in this process. The client reaches the host through
//! [`Door`], a forwarder that can be shut, as a network the device left.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use herder_client_core::{Client, ConnectionState, Machine};
use herder_daemon::auth::authenticator::Join;
use herder_daemon::auth::control::{self, Request, Response};
use herder_protocol::{
    AccountId, CommandBody, CommandResult, EventBody, HostId, PermissionMode, Provider, SessionId,
    Timestamp,
};
use herder_store::{NewEvent, Store};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(30);

/// A daemon's data dir and the task serving it.
struct Daemon {
    data_dir: PathBuf,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Daemon {
    /// Starts a daemon on `dir`; a hub if `hub`.
    async fn start(dir: &Path, hub: bool) -> Self {
        let data_dir = dir.join("data");
        let config = dir.join("daemon.toml");
        let mode = if hub { "mode = \"hub\"\n" } else { "" };
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            &config,
            format!(
                "listen = \"127.0.0.1:0\"\ndata_dir = {:?}\n{mode}\n[projects]\ndir = {:?}\n",
                data_dir.to_str().unwrap(),
                dir.join("Projects").to_str().unwrap()
            ),
        )
        .unwrap();
        let loaded = herder_daemon::Config::load(Some(&config)).unwrap();
        let shutdown = CancellationToken::new();
        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                if hub {
                    return herder_daemon::hub::serve(&loaded, shutdown).await;
                }
                let (adapters, accounts) = (Default::default(), Default::default());
                herder_daemon::serve(
                    &loaded,
                    adapters,
                    Default::default(),
                    accounts,
                    None,
                    shutdown,
                )
                .await
            }
        });
        let socket = data_dir.join(control::SOCKET);
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        while !socket.exists() {
            assert!(tokio::time::Instant::now() < deadline, "no control socket");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Self {
            data_dir,
            shutdown,
            task,
        }
    }

    /// The code that claims the daemon, the addresses it listens on and its fingerprint.
    async fn claim_code(&self) -> (String, Vec<String>, String) {
        let data_dir = self.data_dir.clone();
        let own = tokio::task::spawn_blocking(move || control::request(&data_dir, &Request::Own));
        let Response::Own(info) = own.await.unwrap().unwrap() else {
            panic!("no claim code");
        };
        (info.code, info.addresses, info.fingerprint)
    }

    async fn stop(self) {
        self.shutdown.cancel();
        self.task.await.unwrap().unwrap();
    }
}

/// Forwards connections to a daemon while open; shut, it ends the ones it carries and takes
/// no more, as the network of a machine the device left.
struct Door {
    addr: SocketAddr,
    open: Arc<AtomicBool>,
    carried: Arc<Mutex<CancellationToken>>,
}

impl Door {
    async fn to(daemon: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let open = Arc::new(AtomicBool::new(true));
        let carried = Arc::new(Mutex::new(CancellationToken::new()));
        tokio::spawn({
            let (open, carried) = (Arc::clone(&open), Arc::clone(&carried));
            async move {
                loop {
                    let Ok((mut near, _)) = listener.accept().await else {
                        return;
                    };
                    if !open.load(Ordering::SeqCst) {
                        continue;
                    }
                    let cut = carried.lock().unwrap().clone();
                    let daemon = daemon.clone();
                    tokio::spawn(async move {
                        let Ok(mut far) = TcpStream::connect(daemon).await else {
                            return;
                        };
                        let _ = (near.set_nodelay(true), far.set_nodelay(true));
                        tokio::select! {
                            () = cut.cancelled() => {}
                            _ = tokio::io::copy_bidirectional(&mut near, &mut far) => {}
                        }
                    });
                }
            }
        });
        Self {
            addr,
            open,
            carried,
        }
    }

    fn shut(&self) {
        self.open.store(false, Ordering::SeqCst);
        let carried = std::mem::take(&mut *self.carried.lock().unwrap());
        carried.cancel();
    }

    fn open(&self) {
        self.open.store(true, Ordering::SeqCst);
    }
}

/// Puts one session in a host's journal before its daemon starts.
fn seed(data_dir: &Path, session: &str) {
    std::fs::create_dir_all(data_dir.join("db")).unwrap();
    let mut store = Store::open(data_dir.join("db/herder.db")).unwrap();
    store
        .append(NewEvent {
            session_id: SessionId::new(session),
            at: Timestamp::now(),
            by: None,
            body: EventBody::SessionCreated {
                repo: "/home/dev/app".into(),
                worktree: "/home/dev/worktrees/s1".into(),
                branch: Some("herder/s1".into()),
                provider: Provider::Claude,
                account_id: AccountId::new("main"),
                model: "m0".into(),
                permission_mode: PermissionMode::Ask,
                parent: None,
                parent_host: None,
                task: None,
                failover_pin: None,
                chat: false,
                origin: None,
            },
        })
        .unwrap();
}

async fn machine_when(client: &Client, ready: impl Fn(&Machine) -> bool) -> Machine {
    let changes = client.changes();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        if let Some(machine) = client.machines().into_iter().find(&ready) {
            return machine;
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        // The profile is followed on a timer, which changes nothing a client is told of.
        let _ = tokio::time::timeout(left.min(Duration::from_millis(100)), changes.next()).await;
        let shown = |m: Machine| (m.name, m.addresses, m.address, m.connection);
        assert!(
            tokio::time::Instant::now() < deadline,
            "the client never saw it: {:?}",
            client.machines().into_iter().map(shown).collect::<Vec<_>>()
        );
    }
}

async fn send(client: &Client, machine: &HostId, command: CommandBody) -> CommandResult {
    tokio::time::timeout(TIMEOUT, client.send(machine.clone(), command))
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_machine_out_of_reach_is_reached_through_its_hub_until_it_answers_again() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = Daemon::start(&tmp.path().join("hub"), true).await;
    let host_dir = tmp.path().join("host");
    seed(&host_dir.join("data"), "s1");
    let host = Daemon::start(&host_dir, false).await;
    let client = Client::open(
        tmp.path().join("client").to_str().unwrap().to_owned(),
        "herder-test".into(),
    )
    .unwrap();

    // The device is signed in to both: to the hub at its address, to the host through a
    // door, the network they share for now.
    let (code, addresses, fingerprint) = hub.claim_code().await;
    let (_, on_hub) = Join::claim(code)
        .join(&client, addresses, &fingerprint, "alice")
        .await;
    let (code, addresses, fingerprint) = host.claim_code().await;
    let door = Door::to(addresses[0].clone()).await;
    let at_door = door.addr.to_string();
    let (_, on_host) = Join::claim(code)
        .join(&client, vec![at_door.clone()], &fingerprint, "alice")
        .await;
    let (hub_id, host_id) = (on_hub.host_id.clone(), on_host.host_id.clone());
    let through = format!("hub://{hub_id}");
    let host_when = |ready: fn(&Machine) -> bool| {
        let host_id = host_id.clone();
        machine_when(&client, move |m| m.host_id == host_id && ready(m))
    };
    host_when(|m| m.role.is_some()).await;
    machine_when(&client, |m| m.host_id == hub_id && m.role.is_some()).await;

    // Until the hub relays to the host, the host has no route through it.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        host_when(|_| true).await.addresses,
        std::slice::from_ref(&at_door)
    );

    let pairing = CommandBody::PairHubHost {
        host_name: "devbox".into(),
    };
    let CommandResult::HostPairing { code, .. } = send(&client, &hub_id, pairing).await else {
        panic!("no host code");
    };
    let linking = CommandBody::LinkHub {
        addresses: on_hub.addresses.clone(),
        fingerprint: on_hub.fingerprint.clone(),
        pairing_code: code,
        backup: true,
        secrets: true,
    };
    assert_eq!(
        send(&client, &host_id, linking).await,
        CommandResult::Applied
    );
    // Linked, the route is added, last: the host's own address still comes first, and the
    // connection stays on it.
    let routed = host_when(|m| m.addresses.len() == 2).await;
    assert_eq!(routed.addresses, [at_door.clone(), through.clone()]);
    assert_eq!(routed.address, Some(at_door.clone()));
    // It is saved, for the next launch away from the host's network.
    let profile = std::fs::read_to_string(tmp.path().join("client/machines.json")).unwrap();
    assert!(profile.contains(&through), "{profile}");

    // The device leaves the host's network: the host is reached through the hub, and
    // works as before.
    door.shut();
    let relayed = machine_when(&client, |m| {
        m.host_id == host_id
            && m.connection == ConnectionState::Connected
            && m.address.as_ref() == Some(&through)
    })
    .await;
    assert!(
        relayed
            .sessions
            .iter()
            .any(|s| s.session_id.as_str() == "s1")
    );
    let linked = send(&client, &host_id, CommandBody::GetHubLink).await;
    assert!(
        matches!(&linked, CommandResult::HubLink { hub: Some(_), .. }),
        "{linked:?}"
    );
    let journal = client.subscribe_session(host_id.clone(), SessionId::new("s1"));
    let journal = journal.unwrap();
    // The first update is what the client had cached, which is nothing.
    let events = loop {
        let update = tokio::time::timeout(TIMEOUT, journal.next()).await.unwrap();
        let events = update.unwrap().events;
        if !events.is_empty() {
            break events;
        }
    };
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(matches!(events[0].body, EventBody::SessionCreated { .. }));

    // Back on the host's network, a wake finds its own address answering and moves there.
    door.open();
    client.wake();
    let direct = machine_when(&client, |m| {
        m.host_id == host_id
            && m.connection == ConnectionState::Connected
            && m.address.as_ref() == Some(&at_door)
    })
    .await;
    assert_eq!(direct.addresses, [at_door.clone(), through.clone()]);

    // A device that leaves the hub reaches nothing through it.
    client.forget(hub_id.clone()).unwrap();
    assert_eq!(
        host_when(|m| m.addresses.len() == 1).await.addresses,
        [at_door]
    );

    drop(client);
    host.stop().await;
    hub.stop().await;
}
