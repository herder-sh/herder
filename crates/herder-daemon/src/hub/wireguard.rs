//! herder's WireGuard on the hub (`docs/remote-access.md`): a userspace endpoint on one UDP
//! port, no TUN device and no privileges.
//!
//! Its peers are the WireGuard keys paired client devices enrolled (`enroll_wireguard`),
//! kept on each device's record ([`crate::auth::WireGuardPeer`]), so revoking a device drops
//! its key within [`PEERS_EVERY`] and cuts its connections. The endpoint answers no one
//! else. Inside the tunnel it takes TCP connections to the hub's herder port at
//! [`HUB_ADDRESS`] and nothing more, and serves each as a connection to that port: the
//! hub's TLS, the device's certificate, the hello, and relay streams to every machine
//! (`hub://`). It forwards no packet to any network.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{Context, Result};
use herder_protocol::{ErrorCode, ErrorInfo, WireGuardEnrollment, WireGuardSettings};
use herder_wireguard::{Endpoint, IpCidr, Key, Peer};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::Shared;
use super::conn::{self, Incoming};
use crate::auth::WIREGUARD_NETWORK;

/// The hub's own address inside the tunnel.
pub const HUB_ADDRESS: Ipv4Addr = Ipv4Addr::new(10, 247, 0, 1);

/// How often the endpoint takes the enrolled keys afresh.
const PEERS_EVERY: Duration = Duration::from_secs(1);

/// The hub's WireGuard: its key, and the endpoint while it is on.
pub struct HubWireGuard {
    key: Key,
    /// The hub's herder port, which the tunnel reaches.
    port: u16,
    shared: Weak<Shared>,
    shutdown: CancellationToken,
    running: tokio::sync::Mutex<Option<Running>>,
}

struct Running {
    settings: WireGuardSettings,
    stop: CancellationToken,
}

impl HubWireGuard {
    /// The hub's WireGuard, with its key in `<data_dir>/wireguard.key` (made the first time),
    /// reaching the hub's herder `port`; off until [`HubWireGuard::apply`].
    pub(super) fn open(
        data_dir: &Path,
        port: u16,
        shared: Weak<Shared>,
        shutdown: CancellationToken,
    ) -> Result<Self> {
        Ok(Self {
            key: key(&data_dir.join("wireguard.key"))?,
            port,
            shared,
            shutdown,
            running: tokio::sync::Mutex::new(None),
        })
    }

    /// The hub's herder port inside the tunnel.
    pub fn hub_address(&self) -> SocketAddr {
        SocketAddr::new(HUB_ADDRESS.into(), self.port)
    }

    /// Runs the endpoint as `settings` say: off with none, restarted when they changed. A
    /// port it cannot bind is refused, and the endpoint runs on as it did.
    pub async fn apply(&self, settings: Option<&WireGuardSettings>) -> Result<(), ErrorInfo> {
        let mut running = self.running.lock().await;
        if running.as_ref().map(|r| &r.settings) == settings {
            return Ok(());
        }
        let old = running.take();
        if let Some(old) = &old {
            old.stop.cancel();
        }
        let Some(settings) = settings else {
            info!("herder's WireGuard is off");
            return Ok(());
        };
        match self.start(settings).await {
            Ok(started) => {
                info!(
                    port = settings.port,
                    endpoint = settings.endpoint,
                    "herder's WireGuard is on"
                );
                *running = Some(started);
                Ok(())
            }
            Err(err) => {
                if let Some(old) = old {
                    match self.start(&old.settings).await {
                        Ok(again) => *running = Some(again),
                        Err(again) => warn!("herder's WireGuard is off: {}", again.message),
                    }
                }
                Err(err)
            }
        }
    }

    /// The endpoint, serving, on `settings`' port. A port something else on this machine
    /// holds is refused rather than taken over ([`port_holder`]).
    async fn start(&self, settings: &WireGuardSettings) -> Result<Running, ErrorInfo> {
        if let Some(holder) = port_holder(Path::new("/proc"), Path::new("/sys"), settings.port) {
            return Err(ErrorInfo {
                code: ErrorCode::BadRequest,
                message: format!(
                    "UDP port {} is already in use on this machine, by {holder}; herder will \
                     not take it over: choose another port for herder's WireGuard",
                    settings.port
                ),
            });
        }
        let network = IpCidr::new(HUB_ADDRESS.into(), WIREGUARD_NETWORK.1);
        let any6 = SocketAddr::new(std::net::Ipv6Addr::UNSPECIFIED.into(), settings.port);
        let endpoint = match Endpoint::bind(any6, self.key, network, self.hub_address()).await {
            Ok(endpoint) => endpoint,
            // A host without IPv6 binds IPv4 alone.
            Err(_) => {
                let any4 = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), settings.port);
                Endpoint::bind(any4, self.key, network, self.hub_address())
                    .await
                    .map_err(|err| ErrorInfo {
                        code: ErrorCode::BadRequest,
                        message: format!("cannot bind UDP port {}: {err}", settings.port),
                    })?
            }
        };
        let stop = self.shutdown.child_token();
        tokio::spawn(serve(Arc::new(endpoint), self.shared.clone(), stop.clone()));
        Ok(Running {
            settings: settings.clone(),
            stop,
        })
    }

    /// What a device that enrolled at `address` needs, with its private key, to reach the hub,
    /// while the endpoint is on.
    pub async fn enrollment(&self, address: Ipv4Addr) -> Option<WireGuardEnrollment> {
        let running = self.running.lock().await;
        let settings = &running.as_ref()?.settings;
        Some(WireGuardEnrollment {
            endpoint: settings.endpoint.clone(),
            hub_public_key: self.key.public().to_base64(),
            address: address.to_string(),
            hub_address: self.hub_address().to_string(),
        })
    }

    /// The endpoint's settings while it is on.
    pub async fn settings(&self) -> Option<WireGuardSettings> {
        let running = self.running.lock().await;
        running.as_ref().map(|r| r.settings.clone())
    }
}

/// Serves the endpoint until `stop`: its peers kept to the enrolled keys, each connection it
/// takes served as one to the hub's herder port.
async fn serve(endpoint: Arc<Endpoint>, shared: Weak<Shared>, stop: CancellationToken) {
    let mut every = tokio::time::interval(PEERS_EVERY);
    let mut peers: Vec<Peer> = Vec::new();
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            _ = every.tick() => {
                let Some(shared) = shared.upgrade() else { return };
                let now: Vec<Peer> = shared
                    .auth
                    .wireguard_peers()
                    .into_iter()
                    .filter_map(|peer| {
                        let public_key = peer.public_key.parse().ok()?;
                        Some(Peer { public_key, address: peer.address.into() })
                    })
                    .collect();
                if now != peers {
                    endpoint.set_peers(now.clone());
                    peers = now;
                }
            }
            accepted = endpoint.accept() => {
                let Some(accepted) = accepted else { return };
                let Some(shared) = shared.upgrade() else { return };
                tokio::spawn(conn::run(
                    Incoming::WireGuard(accepted.stream),
                    accepted.from,
                    shared,
                    stop.child_token(),
                ));
            }
        }
    }
}

/// What already holds UDP `port` in this network namespace, other than this process, read
/// from `proc` and `sys` (`/proc` and `/sys` on Linux; elsewhere nothing is found): a kernel
/// socket, as a WireGuard interface (flannel's `wireguard-native` backend, Calico, Cilium,
/// `wg-quick`) or a VXLAN tunnel holds, or another program's.
///
/// It sees only the namespace the hub runs in. On a machine that is the host's, so a node's
/// WireGuard is found. In a pod without `hostNetwork` it is the pod's own, and the node's
/// ports are out of sight: an unprivileged pod cannot read them, and a Service publishing
/// the port as a host port on every node (k3s's ServiceLB) is the cluster admin's to
/// check (`deploy/README.md`).
fn port_holder(proc: &Path, sys: &Path, port: u16) -> Option<String> {
    let own = own_sockets(proc);
    let inode = ["net/udp", "net/udp6"]
        .iter()
        .filter_map(|table| std::fs::read_to_string(proc.join(table)).ok())
        .flat_map(|text| {
            text.lines()
                .skip(1)
                .filter_map(|line| udp_socket(line, port))
                .collect::<Vec<_>>()
        })
        .find(|inode| !own.contains(inode))?;
    if inode != 0 {
        return Some("another program".to_owned());
    }
    // A kernel socket has no inode; a WireGuard interface's own port is not in `sys`, so
    // the interfaces there are named as the likely holder.
    let interfaces = wireguard_interfaces(sys);
    Some(if interfaces.is_empty() {
        "a kernel socket, such as a WireGuard or VXLAN tunnel".to_owned()
    } else {
        format!(
            "a kernel socket, likely the WireGuard interface {}",
            interfaces.join(" or ")
        )
    })
}

/// The inode of the socket in `line` of `/proc/net/udp{,6}` when it is bound to local `port`.
fn udp_socket(line: &str, port: u16) -> Option<u64> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    let (_, local_port) = fields.get(1)?.rsplit_once(':')?;
    (u16::from_str_radix(local_port, 16).ok()? == port)
        .then(|| fields.get(9)?.parse().ok())
        .flatten()
}

/// The inodes of this process's sockets, from `<proc>/self/fd`.
fn own_sockets(proc: &Path) -> Vec<u64> {
    let Ok(fds) = std::fs::read_dir(proc.join("self/fd")) else {
        return Vec::new();
    };
    fds.filter_map(|fd| {
        let target = std::fs::read_link(fd.ok()?.path()).ok()?;
        let target = target.to_str()?;
        target
            .strip_prefix("socket:[")?
            .strip_suffix(']')?
            .parse()
            .ok()
    })
    .collect()
}

/// The names of the WireGuard interfaces in `<sys>/class/net`, sorted.
fn wireguard_interfaces(sys: &Path) -> Vec<String> {
    let Ok(links) = std::fs::read_dir(sys.join("class/net")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = links
        .filter_map(|link| {
            let link = link.ok()?;
            let uevent = std::fs::read_to_string(link.path().join("uevent")).ok()?;
            uevent
                .lines()
                .any(|line| line == "DEVTYPE=wireguard")
                .then(|| link.file_name().to_str().map(str::to_owned))
                .flatten()
        })
        .collect();
    names.sort();
    names
}

/// The hub's WireGuard key in `path`, made and kept there, private, the first time.
fn key(path: &PathBuf) -> Result<Key> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .parse()
            .map_err(|err: String| anyhow::anyhow!(err))
            .with_context(|| format!("reading {}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let key = Key::generate().map_err(|err| anyhow::anyhow!(err))?;
            let dir = path.parent().context("the key's directory")?;
            let file = path
                .file_name()
                .and_then(|name| name.to_str())
                .context("the key's file name")?;
            crate::data_dir::write_private(dir, file, key.to_base64().as_bytes())?;
            Ok(key)
        }
        Err(err) => {
            warn!("reading {}: {err}", path.display());
            Err(err).with_context(|| format!("reading {}", path.display()))
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    const HEADER: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when \
                          retrnsmt   uid  timeout inode ref pointer drops\n";

    /// A `/proc/net/udp` line for a socket on `port` with `inode`.
    fn socket(port: u16, inode: u64) -> String {
        format!(
            "  7: 00000000:{port:04X} 00000000:0000 07 00000000:00000000 00:00000000 \
             00000000     0        0 {inode} 2 0000000000000000 0\n"
        )
    }

    fn machine(udp: &str, udp6: &str, wireguard: &[&str], own: &[u64]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let proc = root.path().join("proc");
        std::fs::create_dir_all(proc.join("net")).unwrap();
        std::fs::create_dir_all(proc.join("self/fd")).unwrap();
        std::fs::write(proc.join("net/udp"), format!("{HEADER}{udp}")).unwrap();
        std::fs::write(proc.join("net/udp6"), format!("{HEADER}{udp6}")).unwrap();
        for (fd, inode) in own.iter().enumerate() {
            std::os::unix::fs::symlink(
                format!("socket:[{inode}]"),
                proc.join(format!("self/fd/{fd}")),
            )
            .unwrap();
        }
        for (name, devtype) in wireguard
            .iter()
            .map(|name| (name, "wireguard"))
            .chain([(&"eth0", "")])
        {
            let link = root.path().join("sys/class/net").join(name);
            std::fs::create_dir_all(&link).unwrap();
            std::fs::write(
                link.join("uevent"),
                format!("DEVTYPE={devtype}\nINTERFACE={name}\n"),
            )
            .unwrap();
        }
        root
    }

    fn holder(root: &tempfile::TempDir, port: u16) -> Option<String> {
        port_holder(&root.path().join("proc"), &root.path().join("sys"), port)
    }

    #[test]
    fn a_node_wireguard_port_is_found_with_its_interface() {
        // flannel's wireguard-native: kernel sockets on 51820, v4 and v6.
        let root = machine(&socket(51820, 0), &socket(51820, 0), &["flannel-wg"], &[]);
        assert_eq!(
            holder(&root, 51820).as_deref(),
            Some("a kernel socket, likely the WireGuard interface flannel-wg")
        );
        assert_eq!(holder(&root, 51900), None);
    }

    #[test]
    fn a_kernel_socket_without_wireguard_is_named_as_such() {
        let root = machine("", &socket(4789, 0), &[], &[]);
        assert_eq!(
            holder(&root, 4789).as_deref(),
            Some("a kernel socket, such as a WireGuard or VXLAN tunnel")
        );
    }

    #[test]
    fn another_program_is_found_but_not_the_hub_itself() {
        let root = machine(&socket(51900, 4242), "", &[], &[]);
        assert_eq!(holder(&root, 51900).as_deref(), Some("another program"));
        // The hub's own endpoint, on while it restarts on the same port.
        let root = machine(&socket(51900, 4242), "", &[], &[4242]);
        assert_eq!(holder(&root, 51900), None);
    }

    #[test]
    fn nothing_is_found_without_proc() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(holder(&root, 51900), None);
    }
}
