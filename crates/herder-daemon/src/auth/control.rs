//! The local control socket `herder pair` and `herder fork` talk to, at
//! `<data_dir>/control.sock`.
//!
//! Only this user can reach it: the data dir is private to them. Each connection carries one
//! JSON request line and gets one JSON response line back. Both ends are the same binary, so
//! these types are not part of the wire protocol.

use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use herder_protocol::{DeviceId, Role, Timestamp, UserId};
use nix::ifaddrs::getifaddrs;
use nix::net::if_::InterfaceFlags;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::{Auth, DeviceRole, PAIRING_TTL, Pairing};
use crate::session::SessionManager;
use crate::session::fork;
use crate::vault::{Admin, Forgot};

/// File name of the socket in the data dir.
pub const SOCKET: &str = "control.sock";

/// Longest request line accepted.
const MAX_REQUEST: u64 = 64 * 1024;

/// Time a request gets to arrive, and to be answered.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Time a fork gets to be answered: it may read the vault and fetch from `origin`.
const FORK_TIMEOUT: Duration = Duration::from_secs(600);

/// What `herder pair` and `herder fork` ask the daemon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Mint a pairing code for `user`.
    Pair {
        /// User name; created when the device pairs, if new.
        user: String,
        /// Role of a new user; the daemon picks when absent.
        role: Option<Role>,
    },
    /// Mint a pairing code for a host to replicate to this vault, and only that.
    PairHost {
        /// The host's name; the user its device acts as.
        host: String,
    },
    /// List the paired devices.
    Devices,
    /// Unpair a device.
    Revoke {
        /// The device.
        device_id: DeviceId,
    },
    /// Fork a session onto this host (`herder fork`).
    Fork(fork::Request),
    /// Drop a host and everything it replicated from this vault (`herder vault forget-host`).
    ForgetHost {
        /// The host's name or id.
        host: String,
    },
}

/// The daemon's answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// A pairing code was minted.
    Paired(PairingInfo),
    /// The paired devices.
    Devices {
        /// Oldest first.
        devices: Vec<DeviceInfo>,
    },
    /// The device was unpaired and disconnected.
    Revoked,
    /// The session was forked onto this host.
    Forked(fork::Forked),
    /// The host was forgotten.
    ForgotHost(Forgot),
    /// The request failed.
    Error {
        /// Why.
        message: String,
    },
}

/// Everything a device needs to pair.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingInfo {
    /// The one-time code.
    pub code: String,
    /// User the device will act as.
    pub user: String,
    /// That user's role.
    pub role: Role,
    /// What the device will connect as.
    pub device_role: DeviceRole,
    /// When the code stops working.
    pub expires_at: Timestamp,
    /// SHA-256 of the daemon's TLS certificate, lowercase hex.
    pub fingerprint: String,
    /// Addresses the daemon listens on, as `host:port`, most likely reachable first.
    pub addresses: Vec<String>,
}

/// A paired device, as `herder pair --list` shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// The device.
    pub device_id: DeviceId,
    /// The client it paired with.
    pub client: String,
    /// When it paired.
    pub paired_at: Timestamp,
    /// Its user.
    pub user_id: UserId,
    /// The user's name.
    pub user: String,
    /// The user's role.
    pub role: Role,
    /// What the device may connect as.
    pub device_role: DeviceRole,
}

/// What the daemon tells `herder pair` about itself, and how it forks sessions.
#[derive(Clone)]
pub struct Daemon {
    /// SHA-256 of its TLS certificate.
    pub fingerprint: String,
    /// The addresses its WebSocket server is bound to, as configured.
    pub listen: Vec<SocketAddr>,
    /// Where forks go on; `None` on the vault, which runs no sessions.
    pub sessions: Option<SessionManager>,
    /// What it holds, when it is a vault: the only daemon hosts pair with, and that forgets
    /// them.
    pub vault: Option<Admin>,
}

/// Binds the socket in `data_dir`, replacing a stale one; the caller holds the data-dir lock,
/// so no other daemon owns it.
pub fn bind(data_dir: &Path) -> Result<UnixListener> {
    let path = data_dir.join(SOCKET);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err).with_context(|| format!("removing {}", path.display())),
    }
    UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))
}

/// Answers requests on `listener` until `shutdown`.
pub async fn serve(
    listener: UnixListener,
    auth: Arc<Auth>,
    daemon: Daemon,
    shutdown: CancellationToken,
) {
    let daemon = Arc::new(daemon);
    loop {
        let accepted = tokio::select! {
            () = shutdown.cancelled() => return,
            accepted = listener.accept() => accepted,
        };
        match accepted {
            Ok((stream, _)) => {
                let (auth, daemon) = (Arc::clone(&auth), Arc::clone(&daemon));
                tokio::spawn(async move {
                    if let Err(err) = answer(stream, &auth, &daemon).await {
                        debug!("control request failed: {err:#}");
                    }
                });
            }
            Err(err) => {
                warn!("cannot accept a control connection: {err}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn answer(stream: UnixStream, auth: &Auth, daemon: &Daemon) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut line = String::new();
    tokio::time::timeout(
        TIMEOUT,
        tokio::io::BufReader::new(read.take(MAX_REQUEST)).read_line(&mut line),
    )
    .await
    .context("no request in time")??;
    let response = match serde_json::from_str(&line) {
        Ok(request) => handle(request, auth, daemon).await,
        Err(err) => Response::Error {
            message: format!("invalid request: {err}"),
        },
    };
    let mut text = serde_json::to_string(&response)?;
    text.push('\n');
    write.write_all(text.as_bytes()).await?;
    Ok(())
}

/// Forks as the daemon's first owner: only the system user running the daemon reaches the
/// socket, and that is who paired as its owner.
async fn fork(request: fork::Request, auth: &Auth, daemon: &Daemon) -> Response {
    let Some(sessions) = &daemon.sessions else {
        return Response::Error {
            message: "the vault runs no sessions; fork on a host".to_owned(),
        };
    };
    match sessions.fork(request, auth.owner()).await {
        Ok(forked) => Response::Forked(forked),
        Err(error) => Response::Error {
            message: error.message,
        },
    }
}

async fn handle(request: Request, auth: &Auth, daemon: &Daemon) -> Response {
    let failed = |err: anyhow::Error| Response::Error {
        message: format!("{err:#}"),
    };
    match request {
        Request::Pair { user, role } => match auth.mint(&user, role, PAIRING_TTL) {
            Ok(pairing) => paired(pairing, daemon),
            Err(err) => failed(err),
        },
        Request::PairHost { .. } if daemon.vault.is_none() => Response::Error {
            message: "hosts pair with a vault; `--host` works on the vault only".to_owned(),
        },
        Request::PairHost { host } => match auth.mint_host(&host, PAIRING_TTL) {
            Ok(pairing) => paired(pairing, daemon),
            Err(err) => failed(err),
        },
        Request::Devices => Response::Devices {
            devices: auth
                .devices()
                .into_iter()
                .map(|(device, user)| DeviceInfo {
                    device_id: device.device_id,
                    client: device.client,
                    paired_at: device.paired_at,
                    user_id: user.user_id,
                    user: user.name,
                    role: user.role,
                    device_role: device.role,
                })
                .collect(),
        },
        Request::Revoke { device_id } => match auth.revoke(&device_id) {
            Ok(true) => Response::Revoked,
            Ok(false) => Response::Error {
                message: format!("no paired device {device_id}"),
            },
            Err(err) => failed(err),
        },
        Request::Fork(request) => fork(request, auth, daemon).await,
        Request::ForgetHost { host } => match &daemon.vault {
            Some(admin) => match admin.forget_host(&host).await {
                Ok(forgot) => Response::ForgotHost(forgot),
                Err(err) => failed(err),
            },
            None => Response::Error {
                message: "only a vault forgets hosts; run this on the vault".to_owned(),
            },
        },
    }
}

fn paired(pairing: Pairing, daemon: &Daemon) -> Response {
    Response::Paired(PairingInfo {
        code: pairing.code,
        user: pairing.user,
        role: pairing.role,
        device_role: pairing.device_role,
        expires_at: pairing.expires_at,
        fingerprint: daemon.fingerprint.clone(),
        addresses: addresses(&daemon.listen),
    })
}

/// Sends `request` to the daemon running on `data_dir` and waits for its answer.
pub fn request(data_dir: &Path, request: &Request) -> Result<Response> {
    let path = data_dir.join(SOCKET);
    let mut stream = StdUnixStream::connect(&path).with_context(|| {
        format!(
            "cannot reach the herder daemon at {}; is it running?",
            path.display()
        )
    })?;
    let timeout = match request {
        Request::Fork(_) => FORK_TIMEOUT,
        _ => TIMEOUT,
    };
    stream.set_read_timeout(Some(timeout))?;
    let mut text = serde_json::to_string(request)?;
    text.push('\n');
    stream.write_all(text.as_bytes())?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .context("reading the daemon's answer")?;
    serde_json::from_str(&line).context("the daemon sent an invalid answer")
}

/// Name prefixes of container and VM bridges: other devices cannot reach their addresses.
const LOCAL_BRIDGES: [&str; 7] = ["docker", "br-", "veth", "virbr", "podman", "cni", "lxcbr"];

/// Where clients can reach a daemon bound to every one of `listen`, in that order and none
/// twice: each specific address itself, and for a wildcard bind the addresses of this machine's
/// interfaces that are up, except container bridges. Loopback addresses are left out unless
/// there is nothing else.
pub(crate) fn addresses(listen: &[SocketAddr]) -> Vec<String> {
    let mut found: Vec<SocketAddr> = Vec::new();
    for &bound in listen {
        let reachable = if bound.ip().is_unspecified() {
            interface_addresses(bound)
        } else {
            vec![bound]
        };
        for address in reachable {
            if !found.contains(&address) {
                found.push(address);
            }
        }
    }
    if found.iter().any(|address| !address.ip().is_loopback()) {
        found.retain(|address| !address.ip().is_loopback());
    } else if found.is_empty() {
        // Only wildcard binds, on a machine with no interface but loopback up.
        for bound in listen {
            let loopback = match bound {
                SocketAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
                SocketAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
            };
            let address = SocketAddr::new(loopback, bound.port());
            if !found.contains(&address) {
                found.push(address);
            }
        }
    }
    found.iter().map(SocketAddr::to_string).collect()
}

/// The addresses of this machine's interfaces a wildcard bind to `wildcard` accepts on, except
/// loopback and container bridges; IPv4 first, in interface order.
fn interface_addresses(wildcard: SocketAddr) -> Vec<SocketAddr> {
    let mut found: Vec<IpAddr> = Vec::new();
    for interface in getifaddrs().into_iter().flatten() {
        let name = &interface.interface_name;
        if !interface.flags.contains(InterfaceFlags::IFF_UP)
            || interface.flags.contains(InterfaceFlags::IFF_LOOPBACK)
            || LOCAL_BRIDGES.iter().any(|prefix| name.starts_with(prefix))
        {
            continue;
        }
        let Some(address) = interface.address else {
            continue;
        };
        let ip = if let Some(v4) = address.as_sockaddr_in() {
            IpAddr::V4(v4.ip())
        } else if let Some(v6) = address.as_sockaddr_in6() {
            // A wildcard IPv4 bind does not accept IPv6, and link-local addresses need a
            // scope a pairing link cannot carry.
            if wildcard.is_ipv4() || v6.ip().is_unicast_link_local() {
                continue;
            }
            IpAddr::V6(v6.ip())
        } else {
            continue;
        };
        if !found.contains(&ip) {
            found.push(ip);
        }
    }
    // Stable, so IPv4 first in interface order.
    found.sort_by_key(IpAddr::is_ipv6);
    found
        .into_iter()
        .map(|ip| SocketAddr::new(ip, wildcard.port()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_specific_bind_is_its_own_address() {
        let listen = ["127.0.0.1:7447".parse().unwrap()];
        assert_eq!(addresses(&listen), ["127.0.0.1:7447"]);
    }

    #[test]
    fn a_list_gives_its_addresses_in_order_without_loopback() {
        let listen: Vec<SocketAddr> = [
            "127.0.0.1:7447",
            "100.124.135.114:7447",
            "[fd7a:115c:a1e0::1]:7447",
            "192.168.1.5:7000",
        ]
        .iter()
        .map(|addr| addr.parse().unwrap())
        .collect();
        assert_eq!(
            addresses(&listen),
            [
                "100.124.135.114:7447",
                "[fd7a:115c:a1e0::1]:7447",
                "192.168.1.5:7000"
            ]
        );
    }

    #[test]
    fn loopback_only_lists_keep_every_address() {
        let listen: Vec<SocketAddr> = ["127.0.0.1:7447", "[::1]:7447"]
            .iter()
            .map(|addr| addr.parse().unwrap())
            .collect();
        assert_eq!(addresses(&listen), ["127.0.0.1:7447", "[::1]:7447"]);
    }

    #[test]
    fn a_wildcard_in_a_list_expands_once_after_the_addresses_before_it() {
        let wildcard = addresses(&["0.0.0.0:7447".parse().unwrap()]);
        let first: SocketAddr = wildcard[0].parse().unwrap();
        let listen = [
            "10.99.0.1:7447".parse().unwrap(),
            "0.0.0.0:7447".parse().unwrap(),
            first,
        ];
        let found = addresses(&listen);
        assert_eq!(found[0], "10.99.0.1:7447");
        if first.ip().is_loopback() {
            // No interface up but loopback: the specific address is all there is.
            assert_eq!(found, ["10.99.0.1:7447"]);
        } else {
            let expected: Vec<String> = std::iter::once("10.99.0.1:7447".to_owned())
                .chain(wildcard.into_iter().filter(|addr| addr != "10.99.0.1:7447"))
                .collect();
            assert_eq!(found, expected);
        }
    }

    #[test]
    fn a_wildcard_bind_lists_interface_addresses() {
        let found = addresses(&["0.0.0.0:7447".parse().unwrap()]);
        assert!(!found.is_empty());
        let found: Vec<SocketAddr> = found.iter().map(|addr| addr.parse().unwrap()).collect();
        assert!(
            found
                .iter()
                .all(|addr| addr.is_ipv4() && addr.port() == 7447)
        );
        assert!(found.len() == 1 || found.iter().all(|addr| !addr.ip().is_loopback()));
    }

    #[tokio::test]
    async fn requests_are_answered_over_the_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let auth = Arc::new(Auth::open(tmp.path()).unwrap());
        let listener = bind(tmp.path()).unwrap();
        let daemon = Daemon {
            fingerprint: "ab".repeat(32),
            listen: vec!["127.0.0.1:7447".parse().unwrap()],
            sessions: None,
            vault: None,
        };
        let shutdown = CancellationToken::new();
        let server = tokio::spawn(serve(listener, auth, daemon, shutdown.clone()));
        let dir = tmp.path().to_owned();
        let ask = move |req: Request| {
            let dir = dir.clone();
            tokio::task::spawn_blocking(move || request(&dir, &req).unwrap())
        };

        let pair = Request::Pair {
            user: "alice".into(),
            role: None,
        };
        let Response::Paired(info) = ask(pair).await.unwrap() else {
            panic!("expected a pairing code");
        };
        assert_eq!((info.user.as_str(), info.role), ("alice", Role::Owner));
        assert_eq!(info.addresses, ["127.0.0.1:7447"]);
        let member = Request::Pair {
            user: "bob".into(),
            role: Some(Role::Member),
        };
        assert!(matches!(ask(member).await.unwrap(), Response::Error { .. }));
        assert_eq!(
            ask(Request::Devices).await.unwrap(),
            Response::Devices {
                devices: Vec::new()
            }
        );
        let host = Request::PairHost {
            host: "devbox".into(),
        };
        let Response::Error { message } = ask(host).await.unwrap() else {
            panic!("expected a refusal");
        };
        assert!(message.contains("vault only"), "{message}");
        let revoke = Request::Revoke {
            device_id: DeviceId::new("nope"),
        };
        assert!(matches!(ask(revoke).await.unwrap(), Response::Error { .. }));
        let fork = Request::Fork(fork::Request {
            session_id: herder_protocol::SessionId::new("s1"),
            account_id: None,
        });
        let Response::Error { message } = ask(fork).await.unwrap() else {
            panic!("expected a refusal");
        };
        assert!(message.contains("fork on a host"), "{message}");
        shutdown.cancel();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_vault_pairs_hosts_and_lists_device_roles() {
        let tmp = tempfile::tempdir().unwrap();
        let auth = Arc::new(Auth::open(tmp.path()).unwrap());
        let daemon = Daemon {
            fingerprint: "ab".repeat(32),
            listen: vec!["127.0.0.1:7447".parse().unwrap()],
            sessions: None,
            vault: Some(vault_admin(tmp.path(), &auth)),
        };
        let pair_host = Request::PairHost {
            host: "devbox".into(),
        };
        let Response::Paired(info) = handle(pair_host, &auth, &daemon).await else {
            panic!("expected a pairing code");
        };
        assert_eq!(
            (info.user.as_str(), info.device_role),
            ("devbox", DeviceRole::Host)
        );
        auth.authenticate(
            "fp-h",
            Some(&info.code),
            "herder/test",
            DeviceRole::Host,
            &CancellationToken::new(),
        )
        .unwrap();
        let Response::Devices { devices } = handle(Request::Devices, &auth, &daemon).await else {
            panic!("expected the devices");
        };
        assert_eq!(devices.len(), 1);
        assert_eq!(
            (devices[0].user.as_str(), devices[0].device_role),
            ("devbox", DeviceRole::Host)
        );
    }

    /// The admin of a vault keeping its database in `dir`.
    fn vault_admin(dir: &Path, auth: &Arc<Auth>) -> Admin {
        std::fs::create_dir_all(dir.join("tls")).unwrap();
        let tls = crate::ws::Tls::load_or_create(&dir.join("tls"), "vault").unwrap();
        let store = crate::vault::VaultStore::open(dir.join("vault.db")).unwrap();
        let host = crate::ws::Host {
            id: herder_protocol::HostId::new("vault"),
            name: "vault".into(),
        };
        crate::vault::Server::new(
            tls,
            Arc::clone(auth),
            store,
            host,
            crate::vault::LIVENESS_TIMEOUT,
            crate::config::Retention::default(),
        )
        .admin()
    }

    #[tokio::test]
    async fn only_a_vault_forgets_hosts_and_only_ones_it_holds() {
        let tmp = tempfile::tempdir().unwrap();
        let auth = Arc::new(Auth::open(tmp.path()).unwrap());
        let mut daemon = Daemon {
            fingerprint: "ab".repeat(32),
            listen: vec!["127.0.0.1:7447".parse().unwrap()],
            sessions: None,
            vault: None,
        };
        let forget = || Request::ForgetHost {
            host: "devbox".into(),
        };
        let Response::Error { message } = handle(forget(), &auth, &daemon).await else {
            panic!("expected a refusal");
        };
        assert!(message.contains("only a vault"), "{message}");
        daemon.vault = Some(vault_admin(tmp.path(), &auth));
        let Response::Error { message } = handle(forget(), &auth, &daemon).await else {
            panic!("expected a refusal");
        };
        assert!(message.contains("no host devbox"), "{message}");
    }
}
