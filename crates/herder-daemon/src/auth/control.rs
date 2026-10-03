//! The local control socket `herder pair` and `herder recover` talk to, at
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
use crate::vault::recover::{self, Recovery};

/// File name of the socket in the data dir.
pub const SOCKET: &str = "control.sock";

/// Longest request line accepted.
const MAX_REQUEST: u64 = 64 * 1024;

/// Time a request gets to arrive, and to be answered.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Time a recovery gets to be answered: it reads the vault and fetches from `origin`.
const RECOVER_TIMEOUT: Duration = Duration::from_secs(600);

/// What `herder pair` and `herder recover` ask the daemon.
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
    /// Recover a session from the vault onto this host (`herder recover`).
    Recover(recover::Request),
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
    /// The session was recovered onto this host.
    Recovered(recover::Outcome),
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

/// What the daemon tells `herder pair` about itself, and how it recovers sessions.
#[derive(Clone)]
pub struct Daemon {
    /// SHA-256 of its TLS certificate.
    pub fingerprint: String,
    /// The address its WebSocket server is bound to.
    pub listen: SocketAddr,
    /// Recovers sessions from the vault; `None` without a `[vault]`, and on the vault itself.
    pub recovery: Option<Arc<Recovery>>,
    /// Whether it is a vault, the only daemon hosts pair with.
    pub vault: bool,
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

async fn recover(request: recover::Request, daemon: &Daemon) -> Response {
    let Some(recovery) = &daemon.recovery else {
        return Response::Error {
            message: "this daemon has no [vault] to recover sessions from".to_owned(),
        };
    };
    match recovery.recover(request).await {
        Ok(outcome) => Response::Recovered(outcome),
        Err(err) => Response::Error {
            message: format!("{err:#}"),
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
        Request::PairHost { .. } if !daemon.vault => Response::Error {
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
        Request::Recover(request) => recover(request, daemon).await,
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
        addresses: addresses(daemon.listen),
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
        Request::Recover(_) => RECOVER_TIMEOUT,
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

/// Where clients can reach a daemon bound to `listen`: that address itself, or for a wildcard
/// bind the addresses of this machine's interfaces that are up, except container bridges and
/// loopback; loopback only when there is nothing else.
fn addresses(listen: SocketAddr) -> Vec<String> {
    if !listen.ip().is_unspecified() {
        return vec![listen.to_string()];
    }
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
            if listen.is_ipv4() || v6.ip().is_unicast_link_local() {
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
    if found.is_empty() {
        found.push(match listen {
            SocketAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
            SocketAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        });
    }
    // Stable, so IPv4 first in interface order.
    found.sort_by_key(IpAddr::is_ipv6);
    found
        .into_iter()
        .map(|ip| SocketAddr::new(ip, listen.port()).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_specific_bind_is_its_own_address() {
        let listen = "127.0.0.1:7447".parse().unwrap();
        assert_eq!(addresses(listen), ["127.0.0.1:7447"]);
    }

    #[test]
    fn a_wildcard_bind_lists_interface_addresses() {
        let found = addresses("0.0.0.0:7447".parse().unwrap());
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
            listen: "127.0.0.1:7447".parse().unwrap(),
            recovery: None,
            vault: false,
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
        let recover = Request::Recover(recover::Request {
            session_id: herder_protocol::SessionId::new("s1"),
            account_id: None,
            force: false,
        });
        let Response::Error { message } = ask(recover).await.unwrap() else {
            panic!("expected a refusal");
        };
        assert!(message.contains("no [vault]"), "{message}");
        shutdown.cancel();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_vault_pairs_hosts_and_lists_device_roles() {
        let tmp = tempfile::tempdir().unwrap();
        let auth = Arc::new(Auth::open(tmp.path()).unwrap());
        let daemon = Daemon {
            fingerprint: "ab".repeat(32),
            listen: "127.0.0.1:7447".parse().unwrap(),
            recovery: None,
            vault: true,
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
}
