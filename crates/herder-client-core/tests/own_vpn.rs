//! A machine reached through the user's own WireGuard server, with a config imported once:
//! the client adds the route from the config's `AllowedIPs`, ranks it right after the home
//! network, connects through it when the home address does not answer, rides out a lossy and
//! a cut path, and closes the tunnel once no route through it is in use.
//!
//! The daemon is `herder_daemon::serve`, in this process. The WireGuard server is
//! `herder_wireguard::Endpoint`, which forwards each connection to the daemon as a router's
//! server routes it to the machine. Between the device's tunnel and the server is [`Path`], a
//! UDP forwarder that can lose, delay and cut datagrams.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use herder_client_core::{Client, ConnectionState, Machine, RouteKind, route_kind};
use herder_daemon::auth::authenticator::Join;
use herder_daemon::auth::control::{self, Request, Response};
use herder_protocol::{AccountId, EventBody, PermissionMode, Provider, SessionId, Timestamp};
use herder_store::{NewEvent, Store};
use herder_wireguard::{Endpoint, IpCidr, Key, Peer};
use tokio::net::{TcpStream, UdpSocket};
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(60);

/// The machine's address at home, which only the tunnel reaches.
const HOME: Ipv4Addr = Ipv4Addr::new(10, 77, 0, 1);
/// The device's address inside the tunnel.
const DEVICE: Ipv4Addr = Ipv4Addr::new(10, 77, 0, 2);

struct Daemon {
    data_dir: PathBuf,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Daemon {
    async fn start(dir: &FsPath) -> Self {
        let data_dir = dir.join("data");
        let config = dir.join("daemon.toml");
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            &config,
            format!(
                "listen = \"127.0.0.1:0\"\ndata_dir = {:?}\n\n[projects]\ndir = {:?}\n",
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

fn seed(data_dir: &FsPath, session: &str) {
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

/// A user's WireGuard server: it routes each connection to the machine's home address to the
/// daemon.
async fn server(device: Key, daemon: SocketAddr) -> Arc<Endpoint> {
    let endpoint = Endpoint::bind(
        "127.0.0.1:0".parse().unwrap(),
        Key::generate().unwrap(),
        IpCidr::new(HOME.into(), 24),
        SocketAddr::new(HOME.into(), 7447),
    )
    .await
    .unwrap();
    endpoint.set_peers(vec![Peer {
        public_key: device.public(),
        address: DEVICE.into(),
    }]);
    let endpoint = Arc::new(endpoint);
    tokio::spawn({
        let endpoint = Arc::clone(&endpoint);
        async move {
            while let Some(accepted) = endpoint.accept().await {
                tokio::spawn(async move {
                    let Ok(mut far) = TcpStream::connect(daemon).await else {
                        return;
                    };
                    let _ = far.set_nodelay(true);
                    let mut near = accepted.stream;
                    let _ = tokio::io::copy_bidirectional(&mut near, &mut far).await;
                });
            }
        }
    });
    endpoint
}

/// The network between the device and its WireGuard server: UDP both ways, losing one
/// datagram in `loss` (none at 0), each delayed by `delay_ms`, or none at all while cut.
struct Path {
    addr: SocketAddr,
    cut: Arc<AtomicBool>,
    loss: Arc<AtomicU64>,
    delay_ms: Arc<AtomicU64>,
}

impl Path {
    async fn to(server: SocketAddr) -> Self {
        let near = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let far = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        far.connect(server).await.unwrap();
        let addr = near.local_addr().unwrap();
        let (cut, loss, delay_ms) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        );
        let device = Arc::new(std::sync::Mutex::new(None::<SocketAddr>));
        let count = Arc::new(AtomicU64::new(0));
        // Whether the next datagram goes through.
        let passes = {
            let (cut, loss, count) = (Arc::clone(&cut), Arc::clone(&loss), Arc::clone(&count));
            move || {
                let loss = loss.load(Ordering::SeqCst);
                let n = count.fetch_add(1, Ordering::SeqCst);
                !cut.load(Ordering::SeqCst) && (loss == 0 || n % loss != 0)
            }
        };
        tokio::spawn({
            let (near, far, device, delay_ms, passes) = (
                Arc::clone(&near),
                Arc::clone(&far),
                Arc::clone(&device),
                Arc::clone(&delay_ms),
                passes.clone(),
            );
            async move {
                let mut buf = vec![0u8; 2048];
                while let Ok((len, from)) = near.recv_from(&mut buf).await {
                    *device.lock().unwrap() = Some(from);
                    if passes() {
                        let (far, datagram) = (Arc::clone(&far), buf[..len].to_vec());
                        let delay = Duration::from_millis(delay_ms.load(Ordering::SeqCst));
                        tokio::spawn(async move {
                            tokio::time::sleep(delay).await;
                            let _ = far.send(&datagram).await;
                        });
                    }
                }
            }
        });
        tokio::spawn({
            let delay_ms = Arc::clone(&delay_ms);
            async move {
                let mut buf = vec![0u8; 2048];
                while let Ok(len) = far.recv(&mut buf).await {
                    let to = *device.lock().unwrap();
                    if let (Some(to), true) = (to, passes()) {
                        let (near, datagram) = (Arc::clone(&near), buf[..len].to_vec());
                        let delay = Duration::from_millis(delay_ms.load(Ordering::SeqCst));
                        tokio::spawn(async move {
                            tokio::time::sleep(delay).await;
                            let _ = near.send_to(&datagram, to).await;
                        });
                    }
                }
            }
        });
        Self {
            addr,
            cut,
            loss,
            delay_ms,
        }
    }
}

async fn machine_when(client: &Client, ready: impl Fn(&Machine) -> bool) -> Machine {
    let changes = client.changes();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        if let Some(machine) = client.machines().into_iter().find(&ready) {
            return machine;
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let _ = tokio::time::timeout(left.min(Duration::from_millis(100)), changes.next()).await;
        let shown = |m: Machine| (m.addresses, m.address, m.connection);
        assert!(
            tokio::time::Instant::now() < deadline,
            "the client never saw it: {:?}",
            client.machines().into_iter().map(shown).collect::<Vec<_>>()
        );
    }
}

async fn tunnel_up(client: &Client, up: bool, within: Duration) {
    let deadline = tokio::time::Instant::now() + within;
    while client.wireguard_tunnels().unwrap()[0].up != up {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the tunnel is not {}",
            if up { "up" } else { "down" }
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn away_from_home_the_machine_is_reached_through_the_own_vpn() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("host");
    seed(&dir.join("data"), "s1");
    let daemon = Daemon::start(&dir).await;
    let (code, addresses, fingerprint) = daemon.claim_code().await;
    let daemon_addr: SocketAddr = addresses[0].parse().unwrap();
    let client = Client::open(
        tmp.path().join("client").to_str().unwrap().to_owned(),
        "herder-test".into(),
    )
    .unwrap();
    let (_, machine) = Join::claim(code)
        .join(&client, addresses.clone(), &fingerprint, "alice")
        .await;
    let host_id = machine.host_id.clone();

    // The device is away: the machine's home address does not answer.
    let home = SocketAddr::new(HOME.into(), 7447).to_string();
    client
        .set_addresses(host_id.clone(), vec![home.clone()])
        .unwrap();

    // The user imports their router's config once.
    let device = Key::generate().unwrap();
    let endpoint = server(device, daemon_addr).await;
    let path = Path::to(endpoint.local_addr()).await;
    let config = format!(
        "[Interface]\nPrivateKey = {}\nAddress = {DEVICE}/32\nDNS = 10.77.0.1\n\n\
         [Peer]\nPublicKey = {}\nAllowedIPs = 10.77.0.0/24\nEndpoint = {}\n\
         PersistentKeepalive = 25\n",
        device.to_base64(),
        endpoint.public_key().to_base64(),
        path.addr
    );
    let imported = client.import_wireguard("Home".into(), config).unwrap();
    assert_eq!(imported.id, "home");

    // The route is added right after the home address, and the connection takes it.
    let through = format!("wg://home/{home}");
    let reached = machine_when(&client, |m| {
        m.address.as_ref() == Some(&through) && m.connection == ConnectionState::Connected
    })
    .await;
    assert_eq!(reached.addresses, [home.clone(), through.clone()]);
    assert_eq!(
        route_kind(through.clone()),
        RouteKind::OwnVpn {
            tunnel: "home".into()
        }
    );
    tunnel_up(&client, true, Duration::ZERO).await;
    // It works through the tunnel as it does directly.
    let journal = client
        .subscribe_session(host_id.clone(), SessionId::new("s1"))
        .unwrap();
    loop {
        let update = tokio::time::timeout(TIMEOUT, journal.next()).await.unwrap();
        if !update.unwrap().events.is_empty() {
            break;
        }
    }
    drop(journal);

    // A lossy, slow path: the connection carries on.
    path.loss.store(5, Ordering::SeqCst);
    path.delay_ms.store(80, Ordering::SeqCst);
    client.synced(host_id.clone()).await.unwrap();
    let rtt = client.reconnect(host_id.clone()).await;
    assert_eq!(rtt, Ok(through.clone()));
    client.synced(host_id.clone()).await.unwrap();

    // Cut: the connection is lost and found again through the tunnel once the path is back.
    path.loss.store(0, Ordering::SeqCst);
    path.delay_ms.store(0, Ordering::SeqCst);
    path.cut.store(true, Ordering::SeqCst);
    assert!(matches!(
        client.reconnect(host_id.clone()).await,
        Err(herder_client_core::Error::Unreachable { .. })
    ));
    path.cut.store(false, Ordering::SeqCst);
    let back = machine_when(&client, |m| m.connection == ConnectionState::Connected).await;
    assert_eq!(back.address, Some(through.clone()));
    client.synced(host_id.clone()).await.unwrap();

    // Back home: the machine is reached directly and the tunnel, used by no route, closes.
    client
        .set_addresses(host_id.clone(), vec![daemon_addr.to_string()])
        .unwrap();
    machine_when(&client, |m| {
        m.address == Some(daemon_addr.to_string()) && m.connection == ConnectionState::Connected
    })
    .await;
    tunnel_up(&client, false, Duration::from_secs(45)).await;

    // Removed, the config takes its routes with it.
    client
        .set_addresses(host_id.clone(), vec![home.clone(), daemon_addr.to_string()])
        .unwrap();
    machine_when(&client, |m| m.addresses.contains(&through)).await;
    client.remove_wireguard("home".into()).unwrap();
    machine_when(&client, |m| !m.addresses.contains(&through)).await;

    drop(client);
    daemon.stop().await;
}
