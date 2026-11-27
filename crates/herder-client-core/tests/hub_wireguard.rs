//! herder's WireGuard on the hub: its owner turns it on, a device enables it with one call
//! and then reaches the hub through it alone, and every machine through the hub; a key the
//! hub does not have gets no answer, and revoking the device takes its key away.
//!
//! The hub and the host are `herder_daemon::hub::serve` and `herder_daemon::serve`, in this
//! process. The client reaches the host through [`Door`], a forwarder that can be shut, as a
//! network the device left.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use herder_client_core::{Client, ConnectionState, Machine, RouteKind, WireGuardKind, route_kind};
use herder_daemon::auth::authenticator::Join;
use herder_daemon::auth::control::{self, Request, Response};
use herder_protocol::{
    AccountId, CommandBody, CommandResult, DeviceId, EventBody, HostId, PermissionMode, Provider,
    RemoteAccessSettings, SessionId, Timestamp, WireGuardSettings,
};
use herder_store::{NewEvent, Store};
use herder_wireguard::{Key, Tunnel, TunnelConfig};
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

/// A UDP port free now, for the hub's WireGuard.
async fn free_udp_port() -> u16 {
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket.local_addr().unwrap().port()
}

/// A tunnel to the hub's endpoint `endpoint`, whose public key is `hub_key`, under `key`
/// at `address` inside.
async fn tunnel(endpoint: &str, hub_key: &str, key: Key, address: &str) -> Tunnel {
    Tunnel::open(TunnelConfig {
        private_key: key,
        peer_public_key: hub_key.parse().unwrap(),
        preshared_key: None,
        endpoint: endpoint.to_owned(),
        addresses: vec![(address.parse().unwrap(), 32)],
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_reaches_every_machine_through_the_hubs_wireguard() {
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
    let (code, addresses, fingerprint) = hub.claim_code().await;
    let (_, on_hub) = Join::claim(code)
        .join(&client, addresses, &fingerprint, "alice")
        .await;
    let (code, addresses, fingerprint) = host.claim_code().await;
    let door = Door::to(addresses[0].clone()).await;
    let (_, on_host) = Join::claim(code)
        .join(&client, vec![door.addr.to_string()], &fingerprint, "alice")
        .await;
    let (hub_id, host_id) = (on_hub.host_id.clone(), on_host.host_id.clone());
    machine_when(&client, |m| m.host_id == hub_id && m.role.is_some()).await;
    machine_when(&client, |m| m.host_id == host_id && m.role.is_some()).await;
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
    let through_hub = format!("hub://{hub_id}");
    machine_when(&client, |m| {
        m.host_id == host_id && m.addresses.contains(&through_hub)
    })
    .await;

    // Off, the hub offers no WireGuard and refuses to enroll a key.
    let CommandResult::RemoteAccess { remote_access } =
        send(&client, &hub_id, CommandBody::GetRemoteAccess).await
    else {
        panic!("no remote access");
    };
    assert_eq!(remote_access.wireguard, None);
    assert!(
        client
            .set_hub_wireguard(hub_id.clone(), true)
            .await
            .is_err()
    );

    // The owner turns it on.
    let port = free_udp_port().await;
    let endpoint = format!("127.0.0.1:{port}");
    let set = CommandBody::SetRemoteAccess {
        settings: Box::new(RemoteAccessSettings {
            wireguard: Some(WireGuardSettings {
                port,
                endpoint: endpoint.clone(),
            }),
            cloudflare: None,
        }),
    };
    let CommandResult::RemoteAccess { remote_access } = send(&client, &hub_id, set).await else {
        panic!("not set");
    };
    assert!(!remote_access.wireguard.unwrap().enrolled);

    // One call on the device, and the hub has a route through its WireGuard.
    client
        .set_hub_wireguard(hub_id.clone(), true)
        .await
        .unwrap();
    let on = machine_when(&client, |m| {
        m.host_id == hub_id && m.addresses.iter().any(|a| a.starts_with("wg://hub/"))
    })
    .await;
    let wireguard_route = on
        .addresses
        .iter()
        .find(|a| a.starts_with("wg://hub/"))
        .unwrap()
        .clone();
    assert!(
        wireguard_route.starts_with("wg://hub/10.247.0.1:"),
        "{wireguard_route}"
    );
    assert_eq!(route_kind(wireguard_route.clone()), RouteKind::HubWireGuard);
    let CommandResult::RemoteAccess { remote_access } =
        send(&client, &hub_id, CommandBody::GetRemoteAccess).await
    else {
        panic!("no remote access");
    };
    assert!(remote_access.wireguard.unwrap().enrolled);
    let tunnels = client.wireguard_tunnels().unwrap();
    assert_eq!(
        tunnels[0].kind,
        WireGuardKind::Hub {
            host_id: hub_id.clone()
        }
    );

    // Away from everything else: the hub only through its WireGuard, the host only through
    // the hub.
    client
        .set_addresses(hub_id.clone(), vec![wireguard_route.clone()])
        .unwrap();
    door.shut();
    machine_when(&client, |m| {
        m.host_id == hub_id
            && m.connection == ConnectionState::Connected
            && m.address.as_ref() == Some(&wireguard_route)
    })
    .await;
    let relayed = machine_when(&client, |m| {
        m.host_id == host_id
            && m.connection == ConnectionState::Connected
            && m.address.as_ref() == Some(&through_hub)
    })
    .await;
    assert!(
        relayed
            .sessions
            .iter()
            .any(|s| s.session_id.as_str() == "s1")
    );
    assert!(client.wireguard_tunnels().unwrap()[0].up);

    // A key the hub does not have gets no answer.
    let hub_key = tunnels_hub_key(&tmp.path().join("client"));
    let stranger = tunnel(&endpoint, &hub_key, Key::generate().unwrap(), "10.247.0.99").await;
    let refused = stranger
        .connect(wireguard_route["wg://hub/".len()..].parse().unwrap())
        .await;
    assert!(refused.is_err());

    // Revoked, the device's key is gone with it: the same key gets no answer.
    let saved_key = enrolled_key(&tmp.path().join("client"));
    let device = enrolled_device(&hub.data_dir);
    let data_dir = hub.data_dir.clone();
    let revoked = tokio::task::spawn_blocking(move || {
        control::request(&data_dir, &Request::Revoke { device_id: device })
    });
    assert!(matches!(revoked.await.unwrap().unwrap(), Response::Revoked));
    tokio::time::sleep(Duration::from_secs(2)).await;
    let (address, private_key) = saved_key;
    let revoked_tunnel = tunnel(&endpoint, &hub_key, private_key, &address).await;
    assert!(
        revoked_tunnel
            .connect(wireguard_route["wg://hub/".len()..].parse().unwrap())
            .await
            .is_err()
    );

    drop(client);
    host.stop().await;
    hub.stop().await;
}

/// The hub's WireGuard public key, as this device saved its enrollment.
fn tunnels_hub_key(config_dir: &Path) -> String {
    let profile: serde_json::Value =
        serde_json::from_slice(&std::fs::read(config_dir.join("machines.json")).unwrap()).unwrap();
    profile["machines"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|m| m["wireguard"]["hub_public_key"].as_str())
        .unwrap()
        .to_owned()
}

/// This device's address and private key inside the hub's tunnel, as saved.
fn enrolled_key(config_dir: &Path) -> (String, Key) {
    let profile: serde_json::Value =
        serde_json::from_slice(&std::fs::read(config_dir.join("machines.json")).unwrap()).unwrap();
    let enrollment = profile["machines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["wireguard"].is_object())
        .unwrap()["wireguard"]
        .clone();
    (
        enrollment["address"].as_str().unwrap().to_owned(),
        enrollment["private_key"].as_str().unwrap().parse().unwrap(),
    )
}

/// The device that enrolled a WireGuard key with the hub, as its auth file has it.
fn enrolled_device(data_dir: &Path) -> DeviceId {
    let auth: serde_json::Value =
        serde_json::from_slice(&std::fs::read(data_dir.join("auth.json")).unwrap()).unwrap();
    let id = auth["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["wireguard"].is_object())
        .unwrap()["device_id"]
        .as_str()
        .unwrap()
        .to_owned();
    DeviceId::new(id)
}
