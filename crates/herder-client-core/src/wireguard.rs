//! WireGuard tunnels herder's own connections run through (`docs/remote-access.md`): a
//! config imported from the user's own WireGuard server, such as a router's, for the "own
//! VPN" route.
//!
//! A config is the standard `wg-quick` text a router exports as a file or a QR code: one
//! `[Interface]` with this device's `PrivateKey` and `Address`, and one `[Peer]` with the
//! server's `PublicKey`, `Endpoint` and `AllowedIPs`, and optionally a `PresharedKey`. `DNS`,
//! `MTU`, `ListenPort` and `PersistentKeepalive` are read and ignored: herder resolves
//! nothing through the tunnel, and its own pings keep the path warm only while a connection
//! uses it.
//!
//! Configs are kept in `<config_dir>/wireguard.json`, private to the user as the device keys
//! are ([`crate::profile`]). The private key never leaves this device.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use herder_wireguard::{Key, TcpStream, Tunnel, TunnelConfig};
use tokio::time::Instant;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use herder_protocol::HostId;
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::profile;

const FILE: &str = "wireguard.json";
const LOCK: &str = "wireguard.lock";

/// The tunnel id reserved for a hub's own WireGuard, as in `wg://hub/<address>` among the
/// hub's addresses.
pub(crate) const HUB: &str = "hub";

/// A WireGuard tunnel herder's connections may run through.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireGuardTunnel {
    /// What names the tunnel in a machine's `wg://<id>/<address>` addresses: lowercase
    /// letters, digits and hyphens, from the name it was imported under.
    pub id: String,
    /// The name it was imported under, or the hub's.
    pub name: String,
    /// The server's `host:port`.
    pub endpoint: String,
    /// This device's addresses inside the tunnel, as CIDR.
    pub addresses: Vec<String>,
    /// What the server routes through the tunnel, as CIDR.
    pub allowed_ips: Vec<String>,
    /// Where the tunnel comes from.
    pub kind: WireGuardKind,
    /// Whether it is up now: a connection or a race is using it.
    pub up: bool,
}

/// Where a [`WireGuardTunnel`] comes from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireGuardKind {
    /// Imported from a config of the user's own WireGuard server.
    Imported,
    /// herder's own WireGuard on a hub, enrolled with [`crate::Client::set_hub_wireguard`].
    Hub {
        /// The hub.
        host_id: HostId,
    },
}

/// An imported config, as saved: secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Imported {
    pub(crate) id: String,
    pub(crate) name: String,
    /// This device's private key, base64; secret.
    pub(crate) private_key: String,
    pub(crate) addresses: Vec<String>,
    pub(crate) peer_public_key: String,
    /// Secret, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) preshared_key: Option<String>,
    pub(crate) endpoint: String,
    pub(crate) allowed_ips: Vec<String>,
}

impl Imported {
    /// The tunnel as the API shows it, without its keys.
    pub(crate) fn view(&self, up: bool) -> WireGuardTunnel {
        WireGuardTunnel {
            id: self.id.clone(),
            name: self.name.clone(),
            endpoint: self.endpoint.clone(),
            addresses: self.addresses.clone(),
            allowed_ips: self.allowed_ips.clone(),
            kind: WireGuardKind::Imported,
            up,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    tunnels: Vec<Imported>,
}

/// The id a tunnel imported as `name` gets: its letters and digits, lowercase, with a hyphen
/// for each run of anything else.
pub(crate) fn id(name: &str) -> Result<String, String> {
    let mut id = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            id.push(c.to_ascii_lowercase());
        } else if !id.is_empty() && !id.ends_with('-') {
            id.push('-');
        }
    }
    let id = id.trim_end_matches('-').to_owned();
    if id.is_empty() {
        return Err(format!(
            "{name:?} names no tunnel: use letters or digits, such as \"home\""
        ));
    }
    if id == HUB {
        return Err(format!(
            "\"{HUB}\" is herder's own WireGuard on a hub; name the tunnel otherwise"
        ));
    }
    Ok(id)
}

/// `text`, a `wg-quick` config, as a tunnel named `name`.
pub(crate) fn parse(name: &str, text: &str) -> Result<Imported, String> {
    let id = id(name)?;
    #[derive(PartialEq)]
    enum Section {
        None,
        Interface,
        Peer,
    }
    let mut section = Section::None;
    let mut peers = 0;
    let (mut private_key, mut addresses) = (None, Vec::new());
    let (mut peer_public_key, mut preshared_key, mut endpoint, mut allowed_ips) =
        (None, None, None, Vec::new());
    for (number, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let at = |why: String| format!("line {}: {why}", number + 1);
        if line.starts_with('[') {
            section = match line.to_ascii_lowercase().as_str() {
                "[interface]" => Section::Interface,
                "[peer]" => {
                    peers += 1;
                    Section::Peer
                }
                _ => return Err(at(format!("{line} is not [Interface] or [Peer]"))),
            };
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(at(format!("{line:?} is not `Key = value`")));
        };
        let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
        let list = || value.split(',').map(str::trim).filter(|v| !v.is_empty());
        match (&section, key.as_str()) {
            (Section::Interface, "privatekey") => private_key = Some(self::key(value).map_err(at)?),
            (Section::Interface, "address") => {
                for cidr in list() {
                    addresses.push(self::cidr(cidr).map_err(at)?);
                }
            }
            (Section::Interface, "dns" | "mtu" | "listenport" | "fwmark" | "table")
            | (Section::Peer, "persistentkeepalive") => {}
            (Section::Interface, "preup" | "postup" | "predown" | "postdown" | "saveconfig") => {}
            (Section::Peer, "publickey") => peer_public_key = Some(self::key(value).map_err(at)?),
            (Section::Peer, "presharedkey") => {
                preshared_key = Some(self::key(value).map_err(at)?);
            }
            (Section::Peer, "endpoint") => endpoint = Some(self::endpoint(value).map_err(at)?),
            (Section::Peer, "allowedips") => {
                for cidr in list() {
                    allowed_ips.push(self::cidr(cidr).map_err(at)?);
                }
            }
            (Section::None, _) => return Err(at(format!("{key} comes before [Interface]"))),
            _ => return Err(at(format!("{key} is not a WireGuard setting herder reads"))),
        }
    }
    if peers != 1 {
        return Err(format!(
            "the config has {peers} [Peer] sections: herder takes one, the server"
        ));
    }
    let missing = |what: &str| format!("the config has no {what}");
    if addresses.is_empty() {
        return Err(missing("Address under [Interface]"));
    }
    if allowed_ips.is_empty() {
        return Err(missing("AllowedIPs under [Peer]"));
    }
    Ok(Imported {
        id,
        name: name.trim().to_owned(),
        private_key: private_key.ok_or_else(|| missing("PrivateKey under [Interface]"))?,
        addresses,
        peer_public_key: peer_public_key.ok_or_else(|| missing("PublicKey under [Peer]"))?,
        preshared_key,
        endpoint: endpoint.ok_or_else(|| {
            missing("Endpoint under [Peer]: herder dials the server, so it needs its address")
        })?,
        allowed_ips,
    })
}

/// `value`, a WireGuard key: 32 bytes in base64.
fn key(value: &str) -> Result<String, String> {
    match STANDARD.decode(value) {
        Ok(bytes) if bytes.len() == 32 => Ok(value.to_owned()),
        _ => Err("a key is 32 bytes in base64, as `wg genkey` prints one".to_owned()),
    }
}

/// `value`, an address with an optional prefix length, as CIDR with one.
fn cidr(value: &str) -> Result<String, String> {
    let (ip, prefix) = match value.split_once('/') {
        Some((ip, prefix)) => (ip, Some(prefix)),
        None => (value, None),
    };
    let ip: IpAddr = ip
        .parse()
        .map_err(|_| format!("{value} is not an IP address or CIDR"))?;
    let max = if ip.is_ipv4() { 32 } else { 128 };
    let prefix = match prefix {
        None => max,
        Some(prefix) => match prefix.parse::<u8>() {
            Ok(prefix) if prefix <= max => prefix,
            _ => return Err(format!("{value} has no valid prefix length")),
        },
    };
    Ok(format!("{ip}/{prefix}"))
}

/// `value`, a server's `host:port`.
fn endpoint(value: &str) -> Result<String, String> {
    match value.rsplit_once(':') {
        Some((host, port))
            if !host.is_empty() && port.parse::<u16>().is_ok_and(|port| port > 0) =>
        {
            Ok(value.to_owned())
        }
        _ => Err(format!("{value} is not host:port")),
    }
}

/// The tunnels imported in `dir`; none when the file does not exist yet.
pub(crate) fn load(dir: &Path) -> Result<Vec<Imported>, Error> {
    let path = dir.join(FILE);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<Saved>(&bytes)
            .map(|saved| saved.tunnels)
            .map_err(|err| Error::Local {
                message: format!("{} is not valid: {err}", path.display()),
            }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(Error::Local {
            message: format!("reading {}: {err}", path.display()),
        }),
    }
}

/// Applies `change` to the tunnels imported in `dir` and saves them, under an exclusive lock
/// every client sharing `dir` takes. Nothing is saved when `change` fails.
pub(crate) fn update<T>(
    dir: &Path,
    change: impl FnOnce(&mut Vec<Imported>) -> Result<T, Error>,
) -> Result<T, Error> {
    let local = |message: String| Error::Local {
        message: format!("saving the WireGuard tunnels: {message}"),
    };
    let lock = profile::lock(dir, LOCK).map_err(local)?;
    let mut tunnels = load(dir)?;
    let changed = change(&mut tunnels)?;
    let json =
        serde_json::to_vec_pretty(&Saved { tunnels }).map_err(|err| local(err.to_string()))?;
    profile::write_private(dir, FILE, &json).map_err(local)?;
    drop(lock);
    Ok(changed)
}

/// How long a tunnel no connection uses stays up: long enough for a reconnect to find it.
const IDLE: Duration = Duration::from_secs(30);

/// The tunnels up now, opened by the first connection through each and closed once none has
/// used it for [`IDLE`]: nothing of a tunnel runs while no route through it is in use.
pub(crate) struct Tunnels {
    config_dir: PathBuf,
    up: Mutex<HashMap<String, Up>>,
}

struct Up {
    tunnel: Arc<Tunnel>,
    /// What it was opened with, keys included: one imported or enrolled again since opens it
    /// afresh.
    opened_with: String,
    /// Since when no connection used it.
    idle_since: Option<Instant>,
}

impl Tunnels {
    /// The tunnels of the configs imported in `config_dir`, and of the hubs enrolled with in
    /// its profile.
    pub(crate) fn new(config_dir: PathBuf) -> Self {
        Self {
            config_dir,
            up: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Up>> {
        // Every change leaves the map whole.
        self.up.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A connection to `address`, `host:port`, through the imported tunnel `id`, which is
    /// opened if it is not up.
    pub(crate) async fn connect(&self, id: &str, address: &str) -> anyhow::Result<TcpStream> {
        let config = load(&self.config_dir)
            .map_err(|err| anyhow!("{err}"))?
            .into_iter()
            .find(|tunnel| tunnel.id == id)
            .with_context(|| format!("no WireGuard tunnel {id} is imported on this device"))?;
        let opened_with = serde_json::to_string(&config)?;
        self.reach(id.to_owned(), opened_with, tunnel_config(&config)?, address)
            .await
    }

    /// A connection to `address`, `ip:port`, through the WireGuard of the hub `hub`, which
    /// this device enrolled with.
    pub(crate) async fn connect_hub(
        &self,
        hub: &HostId,
        address: &str,
    ) -> anyhow::Result<TcpStream> {
        let enrollment = crate::profile::load(&self.config_dir)
            .map_err(|err| anyhow!("{err}"))?
            .into_iter()
            .find(|machine| machine.host_id == *hub)
            .and_then(|machine| machine.wireguard)
            .with_context(|| format!("this device uses no WireGuard of the hub {hub}"))?;
        let opened_with = serde_json::to_string(&enrollment)?;
        let key = |text: &str| text.parse::<Key>().map_err(|err| anyhow!(err));
        let config = TunnelConfig {
            private_key: key(&enrollment.private_key)?,
            peer_public_key: key(&enrollment.hub_public_key)?,
            preshared_key: None,
            endpoint: enrollment.endpoint.clone(),
            addresses: vec![(
                enrollment
                    .address
                    .parse()
                    .with_context(|| format!("{} is not an IP address", enrollment.address))?,
                32,
            )],
        };
        self.reach(hub_key(hub), opened_with, config, address).await
    }

    /// A connection to `address` through the tunnel `key`, opened with `config` unless it is
    /// up with the same `opened_with`.
    async fn reach(
        &self,
        key: String,
        opened_with: String,
        config: TunnelConfig,
        address: &str,
    ) -> anyhow::Result<TcpStream> {
        let remote: SocketAddr = address
            .parse()
            .with_context(|| format!("{address} is not an IP address and port"))?;
        let held = {
            let up = self.lock();
            up.get(&key)
                .filter(|held| held.opened_with == opened_with)
                .map(|held| Arc::clone(&held.tunnel))
        };
        let tunnel = match held {
            Some(tunnel) => tunnel,
            None => {
                let endpoint = config.endpoint.clone();
                let tunnel = Arc::new(
                    Tunnel::open(config)
                        .await
                        .with_context(|| format!("opening the WireGuard tunnel to {endpoint}"))?,
                );
                let mut up = self.lock();
                // Another connection may have opened it meanwhile: the first one stays.
                let held = up
                    .entry(key.clone())
                    .and_modify(|held| {
                        if held.opened_with != opened_with {
                            held.tunnel = Arc::clone(&tunnel);
                            held.opened_with.clone_from(&opened_with);
                        }
                    })
                    .or_insert_with(|| Up {
                        tunnel: Arc::clone(&tunnel),
                        opened_with: opened_with.clone(),
                        idle_since: None,
                    });
                Arc::clone(&held.tunnel)
            }
        };
        let stream = tunnel.connect(remote).await;
        if let Some(held) = self.lock().get_mut(&key) {
            held.idle_since = None;
        }
        Ok(stream?)
    }

    /// Closes each tunnel no connection has used for [`IDLE`].
    pub(crate) fn close_idle(&self) {
        let now = Instant::now();
        self.lock().retain(|_, held| {
            if held.tunnel.connections() > 0 {
                held.idle_since = None;
                return true;
            }
            let since = *held.idle_since.get_or_insert(now);
            now.duration_since(since) < IDLE
        });
    }

    /// Whether the imported tunnel `id` is up.
    pub(crate) fn is_up(&self, id: &str) -> bool {
        self.lock().contains_key(id)
    }

    /// Whether the tunnel to the hub `hub`'s WireGuard is up.
    pub(crate) fn is_hub_up(&self, hub: &HostId) -> bool {
        self.lock().contains_key(&hub_key(hub))
    }
}

/// What names the tunnel to the hub `hub`'s WireGuard among the tunnels up: no imported id
/// has a `/`.
fn hub_key(hub: &HostId) -> String {
    format!("{HUB}/{hub}")
}

/// What `herder-wireguard` opens a tunnel with, from an imported config.
fn tunnel_config(config: &Imported) -> anyhow::Result<TunnelConfig> {
    let key = |text: &str| text.parse::<Key>().map_err(|err| anyhow!(err));
    let addresses = config
        .addresses
        .iter()
        .map(|cidr| parse_cidr(cidr).with_context(|| format!("{cidr} is not CIDR")))
        .collect::<anyhow::Result<_>>()?;
    Ok(TunnelConfig {
        private_key: key(&config.private_key)?,
        peer_public_key: key(&config.peer_public_key)?,
        preshared_key: config.preshared_key.as_deref().map(key).transpose()?,
        endpoint: config.endpoint.clone(),
        addresses,
    })
}

/// `cidr`, as saved: `ip/prefix`.
pub(crate) fn parse_cidr(cidr: &str) -> Option<(IpAddr, u8)> {
    let (ip, prefix) = cidr.split_once('/')?;
    Some((ip.parse().ok()?, prefix.parse().ok()?))
}

/// Whether `ip` is inside `cidr`, as saved.
pub(crate) fn covers(cidr: &str, ip: IpAddr) -> bool {
    let Some((net, prefix)) = parse_cidr(cidr) else {
        return false;
    };
    let (net, ip, bits) = match (net, ip) {
        (IpAddr::V4(net), IpAddr::V4(ip)) => (
            u128::from(u32::from(net)) << 96,
            u128::from(u32::from(ip)) << 96,
            32,
        ),
        (IpAddr::V6(net), IpAddr::V6(ip)) => (u128::from(net), u128::from(ip), 128),
        _ => return false,
    };
    let prefix = u32::from(prefix.min(bits));
    let mask = u128::MAX.checked_shl(128 - prefix).unwrap_or(0);
    net & mask == ip & mask
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// As a UniFi gateway exports a WireGuard VPN server's client.
    const UNIFI: &str = "[Interface]
PrivateKey = yAnz5TF+lXXJte14tji3zlMNq+hd2rYUIgJBgB3fBmk=
Address = 192.168.3.2/32
DNS = 192.168.3.1

[Peer]
PublicKey = xTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg=
AllowedIPs = 0.0.0.0/0, ::/0
Endpoint = home.example.net:51820
PersistentKeepalive = 25
";

    #[test]
    fn a_routers_config_is_read() {
        let tunnel = parse("Home", UNIFI).unwrap();
        assert_eq!(tunnel.id, "home");
        assert_eq!(tunnel.name, "Home");
        assert_eq!(tunnel.addresses, ["192.168.3.2/32"]);
        assert_eq!(tunnel.allowed_ips, ["0.0.0.0/0", "::/0"]);
        assert_eq!(tunnel.endpoint, "home.example.net:51820");
        assert_eq!(tunnel.preshared_key, None);
        assert_eq!(
            tunnel.view(false).kind,
            WireGuardKind::Imported,
            "the view carries no key"
        );
    }

    #[test]
    fn a_config_herder_cannot_use_is_refused_with_why() {
        let refused = |name: &str, text: &str| parse(name, text).unwrap_err();
        assert!(refused("hub", UNIFI).contains("herder's own"));
        assert!(refused("  ", UNIFI).contains("names no tunnel"));
        assert!(
            refused(
                "home",
                &UNIFI.replace("Endpoint = home.example.net:51820\n", "")
            )
            .contains("Endpoint")
        );
        assert!(refused("home", &UNIFI.replace("yAnz5TF+", "short")).contains("line 2"));
        assert!(
            refused(
                "home",
                &format!(
                    "{UNIFI}[Peer]\nPublicKey = xTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg=\n"
                )
            )
            .contains("2 [Peer]")
        );
        assert!(
            refused("home", &UNIFI.replace("192.168.3.2/32", "192.168.3.2/33")).contains("prefix")
        );
        assert!(refused("home", "PrivateKey = x").contains("before [Interface]"));
    }

    #[test]
    fn cidrs_cover_their_addresses() {
        let ip = |text: &str| text.parse::<IpAddr>().unwrap();
        assert!(covers("192.168.1.0/24", ip("192.168.1.5")));
        assert!(!covers("192.168.1.0/24", ip("192.168.2.5")));
        assert!(covers("0.0.0.0/0", ip("10.0.0.1")));
        assert!(!covers("0.0.0.0/0", ip("fd00::1")));
        assert!(covers("::/0", ip("fd00::1")));
        assert!(covers("10.0.0.7/32", ip("10.0.0.7")));
        assert!(!covers("10.0.0.7/32", ip("10.0.0.8")));
        assert!(covers("fd00::/8", ip("fd12::1")));
    }

    #[test]
    fn names_make_ids() {
        assert_eq!(id("Home router").unwrap(), "home-router");
        assert_eq!(id(" UniFi: home! ").unwrap(), "unifi-home");
    }

    #[test]
    fn tunnels_are_saved_privately_and_replaced_by_id() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("herder");
        assert_eq!(load(&dir).unwrap(), Vec::new());
        for _ in 0..2 {
            update(&dir, |tunnels| {
                let tunnel = parse("home", UNIFI).unwrap();
                tunnels.retain(|held| held.id != tunnel.id);
                tunnels.push(tunnel);
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(load(&dir).unwrap().len(), 1);
        let mode = fs::metadata(dir.join(FILE)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
