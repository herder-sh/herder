//! Linking a running host to a hub from a client, as the TUI's machines view does: a client
//! that owns both asks the hub for a host-only code and hands it to the host with the
//! hub's address and fingerprint; the host keeps `[hub]` in its config and replicates
//! without a restart, or only once its backup switch is turned on. Stopping removes `[hub]` and
//! stops replicating, and the client revokes
//! the host's device on the hub. Members can do none of it, and a host code pairs no client.
//!
//! A linked host is reached through the hub too: a device paired with both opens a relay
//! stream at the hub and runs its ordinary connection to the host inside it, authenticated
//! by the host exactly as a direct one, at a cost in round-trip time the test reports.
//!
//! The hub and the host are `herder_daemon::hub::serve` and `herder_daemon::serve`, the
//! functions `herder daemon` runs, in this process, reached through their control sockets and
//! their TLS WebSocket ports.

use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, ready};
use std::time::Duration;

use futures_util::{Sink, SinkExt, Stream, StreamExt};
use herder_client_core::auth::{DeviceKey, client_config};
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Bytes, Message};

use herder_client_core::{Client, Error, Machine};
use herder_daemon::auth::authenticator::{Authenticator, Join};
use herder_daemon::auth::control::{self, Request, Response};
use herder_daemon::hub::install::LocalHub;
use herder_daemon::service::{Service, Services};
use herder_protocol::{
    AccountId, ClientHello, ClientMessage, CommandBody, CommandResult, Cursor, ErrorCode,
    EventBody, HostId, LinkedHub, PROTOCOL_VERSION, PermissionMode, Provider, RELAY_VERSION,
    RelayAnswer, RelayErrorCode, RelayHello, Role, ServerMessage, SessionId, Timestamp,
};
use herder_store::{NewEvent, Store};
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(30);

/// A daemon's config file, data dir, and the task serving it.
struct Daemon {
    config: PathBuf,
    data_dir: PathBuf,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Daemon {
    /// Starts a daemon on `dir` whose config file starts with `extra`; a hub if `hub`.
    async fn start(dir: &Path, extra: &str, hub: bool) -> Self {
        Self::start_with(dir, extra, hub, None).await
    }

    /// Starts a daemon as [`Daemon::start`] does, that may run `local` on its machine.
    async fn start_with(dir: &Path, extra: &str, hub: bool, local: Option<LocalHub>) -> Self {
        let data_dir = dir.join("data");
        let config = dir.join("daemon.toml");
        let mode = if hub { "mode = \"hub\"\n" } else { "" };
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            &config,
            format!(
                "{extra}listen = \"127.0.0.1:0\"\ndata_dir = {:?}\n{mode}\n[projects]\ndir = {:?}\n",
                data_dir.to_str().unwrap(),
                dir.join("Projects").to_str().unwrap()
            ),
        )
        .unwrap();
        Self::run(dir, hub, local).await
    }

    /// Runs the daemon whose config file is already in `dir`, as a restart does.
    async fn run(dir: &Path, hub: bool, local: Option<LocalHub>) -> Self {
        let data_dir = dir.join("data");
        let config = dir.join("daemon.toml");
        let loaded = herder_daemon::Config::load(Some(&config)).unwrap();
        let shutdown = CancellationToken::new();
        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                if hub {
                    herder_daemon::hub::serve(&loaded, shutdown).await
                } else {
                    let accounts = Default::default();
                    let adapters = Default::default();
                    herder_daemon::serve(
                        &loaded,
                        adapters,
                        Default::default(),
                        accounts,
                        local,
                        shutdown,
                    )
                    .await
                }
            }
        });
        let socket = data_dir.join(control::SOCKET);
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        while !socket.exists() {
            assert!(tokio::time::Instant::now() < deadline, "no control socket");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Self {
            config,
            data_dir,
            shutdown,
            task,
        }
    }

    async fn control(&self, request: Request) -> Response {
        let data_dir = self.data_dir.clone();
        tokio::task::spawn_blocking(move || control::request(&data_dir, &request).unwrap())
            .await
            .unwrap()
    }

    /// Signs `client` in as the daemon's first owner, the new person `name`, with the code
    /// `herder service install` shows.
    async fn claim(&self, client: &Client, name: &str) -> (Authenticator, Machine) {
        let Response::Own(info) = self.control(Request::Own).await else {
            panic!("no claim code");
        };
        Join::claim(info.code)
            .join(client, info.addresses, &info.fingerprint, name)
            .await
    }

    async fn stop(self) {
        self.shutdown.cancel();
        self.task.await.unwrap().unwrap();
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
        assert!(
            tokio::time::timeout(left, changes.next()).await.is_ok(),
            "the client never saw it: {:?}",
            client.machines()
        );
    }
}

async fn send(client: &Client, machine: &HostId, command: CommandBody) -> CommandResult {
    tokio::time::timeout(TIMEOUT, client.send(machine.clone(), command))
        .await
        .unwrap()
        .unwrap()
}

async fn refused(client: &Client, machine: &HostId, command: CommandBody) -> ErrorCode {
    match tokio::time::timeout(TIMEOUT, client.send(machine.clone(), command))
        .await
        .unwrap()
    {
        Err(Error::Rejected { info }) => info.code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn open(dir: &Path) -> Client {
    Client::open(dir.to_str().unwrap().to_owned(), "herder-test".into()).unwrap()
}

/// Signs `member` in to `machine` as the new person `name`, a member, by an invite `owner`
/// makes with `passkey`.
async fn invite(
    owner: &Client,
    passkey: &Authenticator,
    machine: &HostId,
    member: &Client,
    name: &str,
) -> HostId {
    let CommandResult::PasskeyChallenge { challenge, .. } =
        send(owner, machine, CommandBody::PasskeyChallenge).await
    else {
        panic!("expected a passkey challenge");
    };
    let invite = CommandBody::Invite {
        role: Role::Member,
        assertion: Box::new(passkey.assert(&challenge)),
    };
    let CommandResult::Invite {
        invite,
        fingerprint,
        addresses,
        ..
    } = send(owner, machine, invite).await
    else {
        panic!("expected an invite");
    };
    let (_, joined) = Join::invite(invite)
        .join(member, addresses, &fingerprint, name)
        .await;
    joined.host_id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owner_links_a_host_to_the_hub_and_unlinks_it() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = Daemon::start(&tmp.path().join("hub"), "", true).await;
    let host_dir = tmp.path().join("host");
    seed(&host_dir.join("data"), "s1");
    let original = "# devbox\n";
    let host = Daemon::start(&host_dir, original, false).await;
    let original = std::fs::read_to_string(&host.config).unwrap();

    // The owner pairs with both; a member pairs with the host.
    let owner = open(&tmp.path().join("owner"));
    let (alice_on_hub, on_hub) = hub.claim(&owner, "alice").await;
    let (alice, on_host) = host.claim(&owner, "alice").await;
    let (hub_id, host_id) = (on_hub.host_id.clone(), on_host.host_id.clone());
    let member = open(&tmp.path().join("member"));
    let as_member = invite(&owner, &alice, &host_id, &member, "bob").await;
    machine_when(&owner, |m| {
        m.host_id == host_id && m.role == Some(Role::Owner)
    })
    .await;
    machine_when(&owner, |m| {
        m.host_id == hub_id && m.role == Some(Role::Owner)
    })
    .await;

    // Which machine is a hub, and where the host backs up: nowhere yet.
    // The hub says how full its disk is, for the fleet view's warning.
    let linked = send(&owner, &hub_id, CommandBody::GetHubLink).await;
    let CommandResult::HubLink {
        is_hub: true,
        hub: None,
        volume: Some(volume),
        local_hub: false,
    } = linked
    else {
        panic!("expected the hub's answer, got {linked:?}");
    };
    assert!(volume.total_bytes > 0 && volume.used_bytes <= volume.total_bytes);
    assert_eq!(
        send(&owner, &host_id, CommandBody::GetHubLink).await,
        CommandResult::HubLink {
            is_hub: false,
            hub: None,
            volume: None,
            local_hub: false,
        }
    );

    let missing_fork = CommandBody::ForkSession {
        session_id: SessionId::new("not-on-this-host"),
        account_id: None,
        relay: None,
    };
    // The running daemon forks its own sessions, and histories a client relays; a session it
    // does not hold is not found, rather than forks being unsupported.
    assert_eq!(
        refused(&owner, &host_id, missing_fork.clone()).await,
        ErrorCode::NotFound
    );

    let CommandResult::HostPairing { code, .. } = send(
        &owner,
        &hub_id,
        CommandBody::PairHubHost {
            host_name: "devbox".into(),
        },
    )
    .await
    else {
        panic!("no host code");
    };
    let linking = CommandBody::LinkHub {
        addresses: on_hub.addresses.clone(),
        fingerprint: on_hub.fingerprint.clone(),
        pairing_code: code,
        backup: false,
        secrets: true,
    };
    let switch = |backup| CommandBody::SetHubLink {
        backup,
        secrets: true,
    };

    // Members can do none of it, on either daemon.
    for (machine, command) in [
        (&as_member, linking.clone()),
        (&as_member, switch(true)),
        (&as_member, CommandBody::UnlinkHub),
        (&as_member, CommandBody::GetHubLink),
    ] {
        assert_eq!(
            refused(&member, machine, command).await,
            ErrorCode::Forbidden
        );
    }
    let hub_member = open(&tmp.path().join("hub-member"));
    let carol_hub = invite(&owner, &alice_on_hub, &hub_id, &hub_member, "carol").await;
    for command in [
        CommandBody::PairHubHost {
            host_name: "devbox".into(),
        },
        CommandBody::RevokeHubHost {
            host_id: host_id.clone(),
        },
    ] {
        assert_eq!(
            refused(&hub_member, &carol_hub, command).await,
            ErrorCode::Forbidden
        );
    }
    // Nothing replicated meanwhile, and there is no link to switch.
    assert!(owner.machines()[0].sessions.is_empty());
    assert_eq!(
        refused(&owner, &host_id, switch(true)).await,
        ErrorCode::NotFound
    );
    assert_eq!(
        refused(&owner, &hub_id, switch(true)).await,
        ErrorCode::Unsupported
    );

    // Link with backup off: the host pairs and connects, with no restart, but replicates
    // nothing.
    assert_eq!(
        send(&owner, &host_id, linking.clone()).await,
        CommandResult::Applied
    );
    machine_when(&owner, |m| {
        m.host_id == hub_id && m.hosts.iter().any(|h| h.host_id == host_id && h.online)
    })
    .await;
    let config = std::fs::read_to_string(&host.config).unwrap();
    assert!(
        config.contains("backup = false\nsecrets = true\n"),
        "{config}"
    );
    let linked = |backup| CommandResult::HubLink {
        is_hub: false,
        hub: Some(LinkedHub {
            address: on_hub.addresses[0].clone(),
            fingerprint: on_hub.fingerprint.clone(),
            backup,
            secrets: true,
            cliproxyapi: None,
        }),
        volume: None,
        local_hub: false,
    };
    assert_eq!(
        send(&owner, &host_id, CommandBody::GetHubLink).await,
        linked(false)
    );
    assert!(
        owner
            .machines()
            .iter()
            .all(|m| m.host_id != hub_id || m.sessions.is_empty())
    );

    // Turning backup on replicates at once, without relinking.
    assert_eq!(
        send(&owner, &host_id, switch(true)).await,
        CommandResult::Applied
    );
    let seen = machine_when(&owner, |m| {
        m.host_id == hub_id
            && m.sessions.iter().any(|s| s.session_id.as_str() == "s1")
            && m.hosts.iter().any(|h| h.host_id == host_id && h.online)
    })
    .await;
    assert_eq!(seen.sessions[0].host_id.as_ref(), Some(&host_id));
    let config = std::fs::read_to_string(&host.config).unwrap();
    assert!(config.starts_with(&original), "{config}");
    assert!(config.contains("[hub]"), "{config}");
    assert!(!config.contains("pairing_code"), "{config}");
    assert!(
        config.contains("backup = true\nsecrets = true\n"),
        "{config}"
    );
    assert_eq!(
        send(&owner, &host_id, CommandBody::GetHubLink).await,
        linked(true)
    );
    // The host paired as a host-only device.
    let Response::Users { users } = hub.control(Request::Users).await else {
        panic!("no users");
    };
    assert!(
        users
            .iter()
            .any(|u| u.name == "devbox" && u.devices.iter().any(|d| d.host_only)),
        "{users:?}"
    );
    // Linked, the host still never reads the hub to fork.
    assert_eq!(
        refused(&owner, &host_id, missing_fork.clone()).await,
        ErrorCode::NotFound
    );
    // Replication goes on.
    assert!(
        owner.machines().iter().any(
            |m| m.host_id == hub_id && m.hosts.iter().any(|h| h.host_id == host_id && h.online)
        )
    );

    // A second link waits for the first to stop.
    assert_eq!(
        refused(&owner, &host_id, linking).await,
        ErrorCode::Conflict
    );

    // Stop backing up: replication stops and `[hub]` goes, the rest of the file as it was.
    assert_eq!(
        send(&owner, &host_id, CommandBody::UnlinkHub).await,
        CommandResult::Applied
    );
    // Unlinked, it forks as before.
    assert_eq!(
        refused(&owner, &host_id, missing_fork).await,
        ErrorCode::NotFound
    );
    machine_when(&owner, |m| {
        m.host_id == hub_id && m.hosts.iter().any(|h| h.host_id == host_id && !h.online)
    })
    .await;
    assert_eq!(std::fs::read_to_string(&host.config).unwrap(), original);
    assert_eq!(
        refused(&owner, &host_id, CommandBody::UnlinkHub).await,
        ErrorCode::NotFound
    );
    // Then the client revokes the host's device on the hub; its sessions stay there.
    let revoke = CommandBody::RevokeHubHost {
        host_id: host_id.clone(),
    };
    assert_eq!(
        send(&owner, &hub_id, revoke.clone()).await,
        CommandResult::Applied
    );
    let Response::Users { users } = hub.control(Request::Users).await else {
        panic!("no users");
    };
    assert!(
        users.iter().flat_map(|u| &u.devices).all(|d| !d.host_only),
        "{users:?}"
    );
    assert_eq!(refused(&owner, &hub_id, revoke).await, ErrorCode::NotFound);
    assert!(
        owner
            .machines()
            .iter()
            .any(|m| m.host_id == hub_id && !m.sessions.is_empty())
    );

    drop((owner, member, hub_member));
    host.stop().await;
    hub.stop().await;
}

/// A service manager that runs an installed hub in this process, on loopback, rather than as
/// a service: what `install_hub` installs.
struct FakeServices {
    runtime: tokio::runtime::Handle,
    installed: Mutex<bool>,
    shutdown: CancellationToken,
}

impl Services for FakeServices {
    fn install(&self, service: &Service) -> anyhow::Result<()> {
        let config = service.args.last().expect("the hub's config");
        let mut config = herder_daemon::Config::load_file(Path::new(config))?;
        assert_eq!(config.mode, herder_daemon::config::Mode::Hub);
        config.listen = vec!["127.0.0.1:0".parse().unwrap()];
        self.runtime.spawn({
            let shutdown = self.shutdown.clone();
            async move { herder_daemon::hub::serve(&config, shutdown).await }
        });
        *self.installed.lock().unwrap() = true;
        Ok(())
    }

    fn is_installed(&self, _: &Service) -> bool {
        *self.installed.lock().unwrap()
    }

    fn log_hint(&self, _: &Service) -> String {
        "the test's output".into()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owner_runs_a_hub_on_the_host_from_a_client() {
    let tmp = tempfile::tempdir().unwrap();
    let services = Arc::new(FakeServices {
        runtime: tokio::runtime::Handle::current(),
        installed: Mutex::new(false),
        shutdown: CancellationToken::new(),
    });
    let local = LocalHub {
        dir: tmp.path().join("herder-hub"),
        port: Some(0),
        services: Arc::clone(&services) as Arc<dyn Services>,
        cliproxyapi: None,
    };
    let host = Daemon::start_with(&tmp.path().join("host"), "", false, Some(local)).await;
    let owner = open(&tmp.path().join("owner"));
    let (alice, on_host) = host.claim(&owner, "alice").await;
    let host_id = on_host.host_id;
    let member = open(&tmp.path().join("member"));
    let as_member = invite(&owner, &alice, &host_id, &member, "bob").await;
    machine_when(&owner, |m| {
        m.host_id == host_id && m.role == Some(Role::Owner)
    })
    .await;

    // Members can do none of it.
    assert_eq!(
        refused(&member, &as_member, CommandBody::InstallHub).await,
        ErrorCode::Forbidden
    );
    let unlinked = CommandResult::HubLink {
        is_hub: false,
        hub: None,
        volume: None,
        local_hub: false,
    };
    assert_eq!(
        send(&owner, &host_id, CommandBody::GetHubLink).await,
        unlinked
    );
    // The owner has the host run one: its config and secrets key are written, its service
    // installed, the host linked with secrets on and backup off, and the owner gets the
    // window in which they claim the hub as its first owner.
    let CommandResult::OwnershipClaim {
        code,
        fingerprint,
        addresses,
        ..
    } = send(&owner, &host_id, CommandBody::InstallHub).await
    else {
        panic!("no claim of the hub");
    };
    let hub_dir = tmp.path().join("herder-hub");
    assert!(hub_dir.join("hub.toml").is_file());
    assert!(hub_dir.join("hub.env").is_file());
    assert!(*services.installed.lock().unwrap());
    let CommandResult::HubLink {
        hub: Some(linked),
        local_hub: true,
        ..
    } = send(&owner, &host_id, CommandBody::GetHubLink).await
    else {
        panic!("the host is not linked to its hub");
    };
    assert_eq!(
        (linked.fingerprint.as_str(), linked.backup, linked.secrets),
        (fingerprint.as_str(), false, true)
    );
    assert!(addresses.contains(&linked.address));
    let config = std::fs::read_to_string(&host.config).unwrap();
    assert!(config.contains("[hub]"), "{config}");
    assert!(!config.contains(&code), "the claim code is not kept");
    let (_, on_hub) = Join::claim(code)
        .join(&owner, addresses, &fingerprint, "alice")
        .await;
    machine_when(&owner, |m| {
        m.host_id == on_hub.host_id && m.role == Some(Role::Owner)
    })
    .await;

    // A second run is refused.
    assert_eq!(
        refused(&owner, &host_id, CommandBody::InstallHub).await,
        ErrorCode::Conflict
    );

    // Unlinked, the installed hub is found, and installing again is still refused: the
    // host links to it with a host code, as to any hub.
    send(&owner, &host_id, CommandBody::UnlinkHub).await;
    let CommandResult::HubLink {
        hub: None,
        local_hub: true,
        ..
    } = send(&owner, &host_id, CommandBody::GetHubLink).await
    else {
        panic!("the installed hub is not found");
    };
    assert_eq!(
        refused(&owner, &host_id, CommandBody::InstallHub).await,
        ErrorCode::Conflict
    );

    host.stop().await;
    services.shutdown.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_daemon_without_a_local_hub_refuses_to_install_one() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Daemon::start(&tmp.path().join("host"), "", false).await;
    let owner = open(&tmp.path().join("owner"));
    let host_id = host.claim(&owner, "alice").await.1.host_id;
    assert_eq!(
        refused(&owner, &host_id, CommandBody::InstallHub).await,
        ErrorCode::Unsupported
    );
    host.stop().await;
}

/// A relay stream at the hub, read and written as the byte stream it carries; what
/// `herder-client-core` runs a connection in.
struct Frames {
    ws: WebSocketStream<TlsStream<TcpStream>>,
    read: Bytes,
}

impl AsyncRead for Frames {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            if !self.read.is_empty() {
                let n = self.read.len().min(buf.remaining());
                buf.put_slice(&self.read.split_to(n));
                return Poll::Ready(Ok(()));
            }
            match ready!(Pin::new(&mut self.ws).poll_next(cx)) {
                Some(Ok(Message::Binary(data))) => self.read = data,
                Some(Ok(Message::Close(_))) | None => return Poll::Ready(Ok(())),
                Some(Ok(_)) => {}
                Some(Err(err)) => return Poll::Ready(Err(io::Error::other(err))),
            }
        }
    }
}

impl AsyncWrite for Frames {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        ready!(Pin::new(&mut self.ws).poll_ready(cx)).map_err(io::Error::other)?;
        Pin::new(&mut self.ws)
            .start_send(Message::binary(buf.to_vec()))
            .map_err(io::Error::other)?;
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.ws)
            .poll_flush(cx)
            .map_err(io::Error::other)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.ws)
            .poll_close(cx)
            .map_err(io::Error::other)
    }
}

/// The key `client`, whose profile is in `dir`, holds for `machine`.
fn device_key(dir: &Path, machine: &HostId) -> DeviceKey {
    let profile = std::fs::read(dir.join("machines.json")).unwrap();
    let profile: serde_json::Value = serde_json::from_slice(&profile).unwrap();
    let saved = profile["machines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|saved| saved["host_id"] == machine.as_str())
        .unwrap();
    DeviceKey::from_pem(saved["device_key"].as_str().unwrap()).unwrap()
}

/// A daemon connection on `stream` up to its WebSocket upgrade: TLS pinned to `fingerprint`,
/// presenting `device`.
async fn secure<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    fingerprint: &str,
    device: &DeviceKey,
) -> anyhow::Result<WebSocketStream<TlsStream<S>>> {
    let config = client_config(fingerprint, device)?;
    let tls = TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from("herder")?, stream)
        .await?;
    let config = WebSocketConfig::default().write_buffer_size(0);
    let request = "wss://herder/".into_client_request()?;
    Ok(
        tokio_tungstenite::client_async_with_config(request, tls, Some(config))
            .await?
            .0,
    )
}

async fn tcp(address: &str) -> TcpStream {
    let tcp = TcpStream::connect(address).await.unwrap();
    tcp.set_nodelay(true).unwrap();
    tcp
}

/// What `hub` answers `device` asking for a stream to `host`, and the stream.
async fn stream_to(hub: &Machine, device: &DeviceKey, host: &HostId) -> (RelayAnswer, Frames) {
    let mut ws = secure(tcp(&hub.addresses[0]).await, &hub.fingerprint, device)
        .await
        .unwrap();
    let connect = RelayHello::Connect {
        relay_version: RELAY_VERSION,
        host_id: host.clone(),
    };
    ws.send(Message::text(serde_json::to_string(&connect).unwrap()))
        .await
        .unwrap();
    let answer = loop {
        match tokio::time::timeout(TIMEOUT, ws.next()).await.unwrap() {
            Some(Ok(Message::Text(text))) => break serde_json::from_str(&text).unwrap(),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            other => panic!("no answer from the hub: {other:?}"),
        }
    };
    let read = Bytes::new();
    (answer, Frames { ws, read })
}

/// The next message of the client protocol on `ws`.
async fn next_message<S>(ws: &mut S) -> ServerMessage
where
    S: Stream<Item = tokio_tungstenite::tungstenite::Result<Message>> + Unpin,
{
    loop {
        match tokio::time::timeout(TIMEOUT, ws.next()).await.unwrap() {
            Some(Ok(Message::Text(text))) => return serde_json::from_str(&text).unwrap(),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            other => panic!("the daemon closed the connection: {other:?}"),
        }
    }
}

/// Says hello on `ws`, a connection to a daemon; what the daemon answers first.
async fn hello<S>(ws: &mut S) -> ServerMessage
where
    S: Stream<Item = tokio_tungstenite::tungstenite::Result<Message>>
        + Sink<Message, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin,
{
    let hello = ClientMessage::Hello(ClientHello {
        protocol_version: PROTOCOL_VERSION,
        client: "herder-test".into(),
        resume: Vec::new(),
        pairing_code: None,
        sign_in: false,
        passkey: false,
    });
    ws.send(Message::text(serde_json::to_string(&hello).unwrap()))
        .await
        .unwrap();
    next_message(ws).await
}

/// The median round trip of `rounds` pings on `ws`, each answered by the daemon at its end.
async fn round_trip<S>(ws: &mut S, rounds: u32) -> Duration
where
    S: Stream<Item = tokio_tungstenite::tungstenite::Result<Message>>
        + Sink<Message, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin,
{
    let mut samples = Vec::new();
    for round in 0..rounds {
        let payload = Bytes::from(round.to_be_bytes().to_vec());
        let started = std::time::Instant::now();
        ws.send(Message::Ping(payload.clone())).await.unwrap();
        loop {
            match tokio::time::timeout(TIMEOUT, ws.next()).await.unwrap() {
                Some(Ok(Message::Pong(pong))) if pong == payload => break,
                Some(Ok(_)) => {}
                other => panic!("the connection ended: {other:?}"),
            }
        }
        samples.push(started.elapsed());
    }
    samples.sort();
    samples[samples.len() / 2]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_reaches_a_linked_host_through_the_hub_as_it_does_directly() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = Daemon::start(&tmp.path().join("hub"), "", true).await;
    let host_dir = tmp.path().join("host");
    seed(&host_dir.join("data"), "s1");
    let host = Daemon::start(&host_dir, "", false).await;
    let owner_dir = tmp.path().join("owner");
    let owner = open(&owner_dir);
    let (_, on_hub) = hub.claim(&owner, "alice").await;
    let (_, on_host) = host.claim(&owner, "alice").await;
    let (hub_id, host_id) = (on_hub.host_id.clone(), on_host.host_id.clone());
    machine_when(&owner, |m| m.host_id == hub_id && m.role.is_some()).await;
    machine_when(&owner, |m| m.host_id == host_id && m.role.is_some()).await;

    // Linked, the host takes relayed connections, and the hub says so.
    let pairing = CommandBody::PairHubHost {
        host_name: "devbox".into(),
    };
    let CommandResult::HostPairing { code, .. } = send(&owner, &hub_id, pairing).await else {
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
        send(&owner, &host_id, linking).await,
        CommandResult::Applied
    );
    let relayed = |m: &Machine| {
        m.host_id == hub_id && m.hosts.iter().any(|h| h.host_id == host_id && h.relay)
    };
    machine_when(&owner, relayed).await;

    // The device holds a key for each machine: the hub knows it by one, the host by the
    // other, and neither key is of use to the other machine.
    let at_hub = device_key(&owner_dir, &hub_id);
    let at_host = device_key(&owner_dir, &host_id);

    // Through the hub the host answers the hello as it does directly, and streams a
    // session's journal.
    let (answer, stream) = stream_to(&on_hub, &at_hub, &host_id).await;
    assert_eq!(answer, RelayAnswer::Ready);
    let mut through = secure(stream, &on_host.fingerprint, &at_host)
        .await
        .unwrap();
    let ServerMessage::Hello(relayed_hello) = hello(&mut through).await else {
        panic!("the host did not say hello through the hub");
    };
    let direct = tcp(&on_host.addresses[0]).await;
    let mut direct = secure(direct, &on_host.fingerprint, &at_host)
        .await
        .unwrap();
    let ServerMessage::Hello(direct_hello) = hello(&mut direct).await else {
        panic!("the host did not say hello");
    };
    assert_eq!(relayed_hello.host_id, host_id);
    assert_eq!(
        (relayed_hello.user_id, relayed_hello.role),
        (direct_hello.user_id, direct_hello.role)
    );
    let subscribe = ClientMessage::Subscribe(Cursor {
        session_id: SessionId::new("s1"),
        after_seq: 0,
    });
    through
        .send(Message::text(serde_json::to_string(&subscribe).unwrap()))
        .await
        .unwrap();
    loop {
        if let ServerMessage::Event(event) = next_message(&mut through).await {
            assert_eq!((event.session_id.as_str(), event.seq), ("s1", 1));
            assert!(matches!(event.body, EventBody::SessionCreated { .. }));
            break;
        }
    }

    // What the hub adds to a round trip, on loopback: both hops and the copy between.
    let rounds = 300;
    round_trip(&mut direct, 20).await;
    round_trip(&mut through, 20).await;
    let (direct_rtt, relayed_rtt) = (
        round_trip(&mut direct, rounds).await,
        round_trip(&mut through, rounds).await,
    );
    println!(
        "relay round trip: direct {direct_rtt:?}, through the hub {relayed_rtt:?}, overhead {:?}",
        relayed_rtt.saturating_sub(direct_rtt)
    );
    assert!(
        relayed_rtt < direct_rtt + Duration::from_millis(5),
        "the hub adds too much: direct {direct_rtt:?}, relayed {relayed_rtt:?}"
    );

    // The host authenticates inside the stream: a device it does not know is refused as it
    // is directly, though the hub forwarded for the device it knows.
    let stranger = DeviceKey::generate().unwrap();
    let (answer, stream) = stream_to(&on_hub, &at_hub, &host_id).await;
    assert_eq!(answer, RelayAnswer::Ready);
    let mut unknown = secure(stream, &on_host.fingerprint, &stranger)
        .await
        .unwrap();
    let ServerMessage::Error { error: relayed, .. } = hello(&mut unknown).await else {
        panic!("the host took a stranger through the hub");
    };
    let mut unknown = secure(
        tcp(&on_host.addresses[0]).await,
        &on_host.fingerprint,
        &stranger,
    )
    .await
    .unwrap();
    let ServerMessage::Error { error: direct, .. } = hello(&mut unknown).await else {
        panic!("the host took a stranger");
    };
    assert_eq!(relayed, direct);
    assert_eq!(relayed.code, ErrorCode::Forbidden);

    // The host's certificate is pinned inside the stream: the hub cannot answer for it.
    let (_, stream) = stream_to(&on_hub, &at_hub, &host_id).await;
    assert!(
        secure(stream, &on_hub.fingerprint, &at_host).await.is_err(),
        "a connection pinned to another certificate went through"
    );
    // And the hub forwards only for a device it knows.
    let (answer, _) = stream_to(&on_hub, &at_host, &host_id).await;
    assert!(
        matches!(&answer, RelayAnswer::Refused { error } if error.code == RelayErrorCode::Forbidden),
        "{answer:?}"
    );

    // A host that takes no relayed connections is listed without the route, and gets none.
    drop((through, direct));
    host.stop().await;
    let config = std::fs::read_to_string(host_dir.join("daemon.toml")).unwrap();
    assert!(config.contains("[hub]\n"), "{config}");
    std::fs::write(
        host_dir.join("daemon.toml"),
        config.replace("[hub]\n", "[hub]\nrelay = false\n"),
    )
    .unwrap();
    machine_when(&owner, |m| {
        m.host_id == hub_id && m.hosts.iter().all(|h| !h.online)
    })
    .await;
    let host = Daemon::run(&host_dir, false, None).await;
    let unrelayed = machine_when(&owner, |m| {
        m.host_id == hub_id && m.hosts.iter().any(|h| h.online)
    })
    .await;
    assert!(unrelayed.hosts.iter().all(|h| !h.relay), "{unrelayed:?}");
    let (answer, _) = stream_to(&on_hub, &at_hub, &host_id).await;
    assert!(
        matches!(&answer, RelayAnswer::Refused { error } if error.code == RelayErrorCode::UnknownHost),
        "{answer:?}"
    );
    host.stop().await;
    hub.stop().await;
}
