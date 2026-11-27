//! Machine addresses: how a typed one is completed, and the order a link's are saved in.
//!
//! The saved order is the order of preference: connecting starts at the first address and gives
//! each later one a head start less. A link lists the daemon's addresses in interface order, so
//! signing in sorts them direct routes first: private network addresses (reached at home
//! or through a VPN into it, such as UniFi Teleport), then other addresses, then Tailscale. The
//! user can reorder them after that with [`crate::Client::set_addresses`]; nothing re-sorts them.
//!
//! An address is `host:port`, or the URL of a tunnel to the daemon, `wss://host[:port][/path]`
//! (or `ws://`), such as a Cloudflare Tunnel's public hostname: the client runs its ordinary
//! connection inside that WebSocket ([`crate::tunnel`]). Tunnels sort last, after Tailscale,
//! as they relay every byte through a third party's edge.
//!
//! A third form, `hub://<hub's host id>`, is the route through a hub this device is
//! signed in to, which forwards the connection to the machine ([`crate::relay`]). The client
//! adds it to a machine's addresses once the hub says it relays there; it sorts after the
//! machine's own routes, as it takes two hops, the hub's own route and the machine's to the
//! hub.
//!
//! Every address is of a [`RouteKind`], and that is the order they sort in
//! (`docs/remote-access.md`): the home network, the user's own VPN through herder's WireGuard
//! (`wg://<tunnel>/<host:port>`), other direct addresses, a system VPN such as Tailscale, a hub
//! through herder's WireGuard on it (`wg://hub/<ip:port>`), a tunnel's URL, a hub's relay, and
//! last the Durable Object relay (`relay+wss://<host>`).
//!
//! A daemon's hello names where it is reached: the addresses it listens on, then the public
//! addresses its owner set (a port forward, a TCP proxy, a VPN, a tunnel's URL). The client
//! follows them ([`follow`]): each one new since the last hello joins the machine's addresses
//! in its kind's place, and each one the daemon stopped naming leaves them. Addresses the user
//! added, and the ones the daemon still names, keep the user's order.

use std::net::{IpAddr, SocketAddr};

use herder_protocol::HostId;

use crate::RouteKind;

/// The port a daemon listens on unless configured otherwise, as `herder daemon` has it.
const DEFAULT_PORT: u16 = 7447;

/// `text`, an address as typed, as `host:port`: trimmed, with the default port added when it
/// has none, and IPv6 addresses bracketed.
pub(crate) fn normalize(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("an address is empty".to_owned());
    }
    if let Some(hub) = text.strip_prefix(HUB) {
        if hub.is_empty() || hub.contains(|c: char| c.is_whitespace() || c == '/') {
            return Err(format!("{text} names no hub"));
        }
        return Ok(text.to_owned());
    }
    if text.contains("://") {
        return normalize_url(text);
    }
    if text.contains(|c: char| c.is_whitespace() || c == '/') {
        return Err(format!("{text} is not a host or host:port"));
    }
    if let Ok(address) = text.parse::<SocketAddr>() {
        return Ok(address.to_string());
    }
    if let Ok(ip) = text.trim_matches(['[', ']']).parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, DEFAULT_PORT).to_string());
    }
    match text.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !host.contains(':') => {
            match port.parse::<u16>() {
                Ok(port) if port > 0 => Ok(format!("{host}:{port}")),
                _ => Err(format!("{text} has no valid port")),
            }
        }
        Some(_) => Err(format!("{text} is not a host or host:port")),
        None => Ok(format!("{text}:{DEFAULT_PORT}")),
    }
}

/// `text`, a tunnel's URL, as saved: `https` read as `wss` and `http` as `ws`, since the
/// hostname a tunnel gets is usually copied as a web address; no default port, and no path
/// when it is `/`.
fn normalize_url(text: &str) -> Result<String, String> {
    let invalid = |why: &str| format!("{text} is not a tunnel URL: {why}");
    let mut url = url::Url::parse(text).map_err(|err| invalid(&err.to_string()))?;
    let scheme = match url.scheme() {
        "wss" | "https" => "wss",
        "ws" | "http" => "ws",
        _ => return Err(invalid("it must start with wss:// or https://")),
    };
    if url.set_scheme(scheme).is_err() {
        return Err(invalid("it must start with wss:// or https://"));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(invalid("it names no host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("it may not carry a user name or password"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(invalid("it may not have a query or fragment"));
    }
    let text = url.as_str();
    Ok(if url.path() == "/" {
        text.trim_end_matches('/').to_owned()
    } else {
        text.to_owned()
    })
}

/// What the address of a route through a hub starts with.
const HUB: &str = "hub://";

/// The address of the route through the hub `hub`.
pub(crate) fn through_hub(hub: &HostId) -> String {
    format!("{HUB}{hub}")
}

/// The hub `address` goes through, for the address of a route through one.
pub(crate) fn hub(address: &str) -> Option<HostId> {
    address.strip_prefix(HUB).map(HostId::new)
}

/// The part of `address` after `wss://` or `ws://`, for a tunnel's URL.
fn tunneled(address: &str) -> Option<&str> {
    address
        .strip_prefix("wss://")
        .or_else(|| address.strip_prefix("ws://"))
}

/// Whether `address` is a tunnel's URL rather than `host:port`.
pub(crate) fn is_tunnel(address: &str) -> bool {
    tunneled(address).is_some()
}

/// `addresses` without duplicates, direct routes first: see the module docs. Stable, so the
/// daemon's order holds within each kind.
pub(crate) fn default_order(addresses: Vec<String>) -> Vec<String> {
    let mut unique: Vec<String> = Vec::new();
    for address in addresses {
        if !unique.contains(&address) {
            unique.push(address);
        }
    }
    unique.sort_by_key(|address| rank(address));
    unique
}

/// What of `named`, the addresses a daemon's hello names, other devices may dial, as saved:
/// normalized, without loopback, routes through a hub, or anything that is no address.
pub(crate) fn advertised(named: &[String]) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for address in named.iter().filter_map(|text| normalize(text).ok()) {
        if hub(&address).is_none() && !is_loopback(&address) && !kept.contains(&address) {
            kept.push(address);
        }
    }
    kept
}

/// `addresses`, a machine's in order of preference, once its daemon names `now` where it last
/// named `before`: each address of `now` not among them is added after the last of its kind
/// or of one that sorts before it ([`default_order`]), first when there is none, and each of
/// `before` the daemon no longer names is
/// dropped, unless it is `using`, the address that reached it. Whether anything changed.
pub(crate) fn follow(
    addresses: &mut Vec<String>,
    before: &[String],
    now: &[String],
    using: Option<&str>,
) -> bool {
    let count = addresses.len();
    addresses.retain(|address| {
        now.contains(address) || !before.contains(address) || using == Some(address.as_str())
    });
    let mut changed = addresses.len() != count;
    for address in now.iter().filter(|a| !before.contains(a)) {
        if addresses.contains(address) {
            continue;
        }
        let at = addresses
            .iter()
            .rposition(|held| rank(held) <= rank(address))
            .map_or(0, |last| last + 1);
        addresses.insert(at, address.clone());
        changed = true;
    }
    changed
}

/// Whether `address`, as `host:port`, is this device itself (`localhost`, `127.0.0.0/8`,
/// `::1`), which no other device reaches it at.
pub(crate) fn is_loopback(address: &str) -> bool {
    if hub(address).is_some() {
        return false;
    }
    let host = host(address);
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// The host of `address`, as `host:port` or a tunnel's URL, without the brackets of an IPv6
/// address.
fn host(address: &str) -> &str {
    let authority = match tunneled(address) {
        Some(rest) => rest.split('/').next().unwrap_or(rest),
        None => address,
    };
    let host = if authority.ends_with(']') || !authority.contains(':') {
        authority
    } else {
        authority
            .rsplit_once(':')
            .map_or(authority, |(host, _)| host)
    };
    host.trim_matches(['[', ']'])
}

/// What carries a connection to `address` (`docs/remote-access.md`, The routes).
pub(crate) fn kind(address: &str) -> RouteKind {
    if let Some(hub) = hub(address) {
        return RouteKind::Hub { hub };
    }
    if address.starts_with(RELAY) || address.starts_with(RELAY_PLAIN) {
        return RouteKind::CloudflareRelay;
    }
    if let Some((tunnel, _)) = wireguard(address) {
        return if tunnel == crate::wireguard::HUB {
            RouteKind::HubWireGuard
        } else {
            RouteKind::OwnVpn {
                tunnel: tunnel.to_owned(),
            }
        };
    }
    if is_tunnel(address) {
        return RouteKind::Tunnel;
    }
    let host = host(address);
    let Ok(ip) = host.parse::<IpAddr>() else {
        return if host.ends_with(".ts.net") {
            RouteKind::SystemVpn
        } else if host.ends_with(".local") || !host.contains('.') {
            RouteKind::HomeNetwork
        } else {
            RouteKind::Direct
        };
    };
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, ..] = ip.octets();
            // Tailscale hands out addresses from the CGNAT range, 100.64.0.0/10.
            if a == 100 && b & 0xc0 == 64 {
                RouteKind::SystemVpn
            } else if ip.is_private() || ip.is_loopback() || ip.is_link_local() {
                RouteKind::HomeNetwork
            } else {
                RouteKind::Direct
            }
        }
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            // Tailscale's unique local prefix, fd7a:115c:a1e0::/48.
            if segments[..3] == [0xfd7a, 0x115c, 0xa1e0] {
                RouteKind::SystemVpn
            } else if ip.is_unique_local() || ip.is_loopback() || ip.is_unicast_link_local() {
                RouteKind::HomeNetwork
            } else {
                RouteKind::Direct
            }
        }
    }
}

/// Where `address` sorts among a machine's addresses: the order of [`RouteKind`]'s variants.
fn rank(address: &str) -> u8 {
    match kind(address) {
        RouteKind::HomeNetwork => 0,
        RouteKind::OwnVpn { .. } => 1,
        RouteKind::Direct => 2,
        RouteKind::SystemVpn => 3,
        RouteKind::HubWireGuard => 4,
        RouteKind::Tunnel => 5,
        RouteKind::Hub { .. } => 6,
        RouteKind::CloudflareRelay => 7,
    }
}

/// What the address of a route through a WireGuard tunnel of herder's starts with.
const WIREGUARD: &str = "wg://";

/// What the address of a route through a Durable Object relay starts with, and with `ws://`
/// inside, as a local relay in a test has it.
const RELAY: &str = "relay+wss://";
const RELAY_PLAIN: &str = "relay+ws://";

/// The tunnel and the `host:port` inside it, for the address of a route through a WireGuard
/// tunnel of herder's, `wg://<tunnel>/<host:port>`.
pub(crate) fn wireguard(address: &str) -> Option<(&str, &str)> {
    address.strip_prefix(WIREGUARD)?.split_once('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hello_names_what_other_devices_may_dial_as_saved() {
        let named = [
            "192.168.1.5:7447",
            "127.0.0.1:7447",
            "https://Box.example.com/",
            "hub://vault",
            "not an address",
            "wss://box.example.com",
        ]
        .map(String::from);
        assert_eq!(
            advertised(&named),
            ["192.168.1.5:7447", "wss://box.example.com"]
        );
    }

    #[test]
    fn a_machine_follows_what_its_daemon_names() {
        let strings = |all: &[&str]| all.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
        // The user typed a Tailscale name and put it first; the hub route came by itself.
        let mut addresses = strings(&["box.tail1234.ts.net:7447", "192.168.1.5:7447", "hub://h"]);
        let before = strings(&["192.168.1.5:7447"]);

        // The owner set a port forward and a tunnel: each joins in its kind's place.
        let now = strings(&[
            "192.168.1.5:7447",
            "203.0.113.7:7447",
            "wss://box.example.com",
        ]);
        assert!(follow(&mut addresses, &before, &now, None));
        assert_eq!(
            addresses,
            [
                "box.tail1234.ts.net:7447",
                "192.168.1.5:7447",
                "203.0.113.7:7447",
                "wss://box.example.com",
                "hub://h",
            ]
        );
        // Named again, nothing moves.
        assert!(!follow(&mut addresses, &now, &now, None));

        // The owner removed the port forward and the lease changed: both go, the user's stay,
        // and the new LAN address, with none left before it, goes first.
        let later = strings(&["192.168.1.9:7447", "wss://box.example.com"]);
        assert!(follow(&mut addresses, &now, &later, None));
        assert_eq!(
            addresses,
            [
                "192.168.1.9:7447",
                "box.tail1234.ts.net:7447",
                "wss://box.example.com",
                "hub://h",
            ]
        );

        // One the user removed stays removed while the daemon keeps naming it.
        addresses.retain(|a| a != "wss://box.example.com");
        assert!(!follow(&mut addresses, &later, &later, None));
        assert!(!addresses.contains(&"wss://box.example.com".to_owned()));

        // The address that reached the daemon is never dropped.
        let mut addresses = strings(&["203.0.113.7:7447"]);
        let before = strings(&["203.0.113.7:7447"]);
        assert!(!follow(
            &mut addresses,
            &before,
            &[],
            Some("203.0.113.7:7447")
        ));
        assert_eq!(addresses, ["203.0.113.7:7447"]);
    }

    #[test]
    fn typed_addresses_get_the_default_port() {
        let ok = |text: &str| normalize(text).unwrap();
        assert_eq!(ok(" box "), "box:7447");
        assert_eq!(ok("box:9000"), "box:9000");
        assert_eq!(ok("box.tail1234.ts.net"), "box.tail1234.ts.net:7447");
        assert_eq!(ok("10.0.0.2"), "10.0.0.2:7447");
        assert_eq!(ok("10.0.0.2:9000"), "10.0.0.2:9000");
        assert_eq!(ok("fd00::1"), "[fd00::1]:7447");
        assert_eq!(ok("[fd00::1]"), "[fd00::1]:7447");
        assert_eq!(ok("[fd00::1]:9000"), "[fd00::1]:9000");
        for bad in [
            "",
            "  ",
            "box:",
            "box:x",
            "box:0",
            "box:70000",
            "a b",
            "box/x",
            ":7447",
        ] {
            assert!(normalize(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn tunnel_urls_are_kept_as_urls() {
        let ok = |text: &str| normalize(text).unwrap();
        assert_eq!(ok("wss://box.example.com"), "wss://box.example.com");
        assert_eq!(ok(" wss://Box.Example.com/ "), "wss://box.example.com");
        assert_eq!(ok("https://box.example.com"), "wss://box.example.com");
        assert_eq!(ok("wss://box.example.com:443"), "wss://box.example.com");
        assert_eq!(
            ok("wss://box.example.com:8443"),
            "wss://box.example.com:8443"
        );
        assert_eq!(
            ok("wss://box.example.com/herder"),
            "wss://box.example.com/herder"
        );
        assert_eq!(ok("http://127.0.0.1:7448"), "ws://127.0.0.1:7448");
        assert_eq!(ok("http://box"), "ws://box");
        assert_eq!(ok("ws://[::1]:7448/"), "ws://[::1]:7448");
        for bad in [
            "ftp://box.example.com",
            "wss://",
            "wss://user:pw@box.example.com",
            "wss://box.example.com/?token=1",
            "wss://box.example.com/#x",
            "box.example.com/herder",
        ] {
            assert!(normalize(bad).is_err(), "{bad:?}");
        }
        assert!(is_tunnel("wss://box.example.com"));
        assert!(!is_tunnel("box.example.com:7447"));
    }

    #[test]
    fn tunnels_come_last_and_know_their_host() {
        let ordered = default_order(
            [
                "wss://box.example.com",
                "box.tail1234.ts.net:7447",
                "203.0.113.7:7447",
                "192.168.1.5:7447",
            ]
            .map(str::to_owned)
            .to_vec(),
        );
        assert_eq!(
            ordered,
            [
                "192.168.1.5:7447",
                "203.0.113.7:7447",
                "box.tail1234.ts.net:7447",
                "wss://box.example.com",
            ]
        );
        assert_eq!(host("wss://box.example.com"), "box.example.com");
        assert_eq!(host("wss://box.example.com:8443/herder"), "box.example.com");
        assert_eq!(host("ws://[::1]:7448"), "::1");
        assert_eq!(host("ws://[::1]"), "::1");
        assert!(is_loopback("ws://127.0.0.1:7448"));
        assert!(is_loopback("ws://localhost/x"));
        assert!(!is_loopback("wss://box.example.com"));
    }

    #[test]
    fn the_route_through_a_hub_is_an_address_that_comes_last() {
        let id = HostId::new("0193a5b2-7c1e-7d3a-9f00-2b6f1c0a9e11");
        let route = through_hub(&id);
        assert_eq!(route, "hub://0193a5b2-7c1e-7d3a-9f00-2b6f1c0a9e11");
        assert_eq!(normalize(&format!(" {route} ")).unwrap(), route);
        assert_eq!(hub(&route), Some(id));
        assert_eq!(hub("hub:7447"), None, "a machine named hub");
        assert_eq!(hub("wss://hub.example.com"), None);
        assert!(!is_tunnel(&route) && !is_loopback(&route));
        for bad in ["hub://", "hub://a b", "hub://a/b"] {
            assert!(normalize(bad).is_err(), "{bad:?}");
        }
        let ordered = default_order(
            [route.as_str(), "wss://box.example.com", "192.168.1.5:7447"]
                .map(str::to_owned)
                .to_vec(),
        );
        assert_eq!(
            ordered,
            ["192.168.1.5:7447", "wss://box.example.com", route.as_str()]
        );
    }

    #[test]
    fn direct_routes_come_before_tailscale() {
        let addresses = [
            "100.101.102.103:7447",
            "203.0.113.7:7447",
            "192.168.1.5:7447",
            "[fd7a:115c:a1e0::1]:7447",
            "box.tail1234.ts.net:7447",
            "[fd00::5]:7447",
            "box.example.com:7447",
            "10.0.0.5:7447",
            "192.168.1.5:7447",
            "box.local:7447",
        ];
        let ordered = default_order(addresses.map(str::to_owned).to_vec());
        assert_eq!(
            ordered,
            [
                "192.168.1.5:7447",
                "[fd00::5]:7447",
                "10.0.0.5:7447",
                "box.local:7447",
                "203.0.113.7:7447",
                "box.example.com:7447",
                "100.101.102.103:7447",
                "[fd7a:115c:a1e0::1]:7447",
                "box.tail1234.ts.net:7447",
            ]
        );
        // 100.128.0.0 is past the CGNAT range.
        assert_eq!(kind("100.128.0.1:7447"), RouteKind::Direct);
    }

    #[test]
    fn routes_sort_best_first_by_kind() {
        let addresses = [
            "relay+wss://herder-relay.example.workers.dev",
            "hub://h",
            "wss://box.example.com",
            "wg://hub/10.247.0.1:7447",
            "100.101.102.103:7447",
            "203.0.113.7:7447",
            "wg://home/192.168.1.5:7447",
            "192.168.1.5:7447",
        ];
        let ordered = default_order(addresses.map(str::to_owned).to_vec());
        let reversed: Vec<_> = addresses.iter().rev().map(|a| (*a).to_owned()).collect();
        assert_eq!(ordered, reversed);
        let kinds: Vec<RouteKind> = ordered.iter().map(|a| kind(a)).collect();
        assert_eq!(
            kinds,
            [
                RouteKind::HomeNetwork,
                RouteKind::OwnVpn {
                    tunnel: "home".into()
                },
                RouteKind::Direct,
                RouteKind::SystemVpn,
                RouteKind::HubWireGuard,
                RouteKind::Tunnel,
                RouteKind::Hub {
                    hub: HostId::new("h")
                },
                RouteKind::CloudflareRelay,
            ]
        );
        assert_eq!(
            wireguard("wg://home/192.168.1.5:7447"),
            Some(("home", "192.168.1.5:7447"))
        );
        assert_eq!(
            kind("relay+ws://127.0.0.1:8787"),
            RouteKind::CloudflareRelay
        );
    }

    #[test]
    fn loopback_addresses_are_this_device_only() {
        for address in [
            "127.0.0.1:7447",
            "127.1.2.3:9000",
            "[::1]:7447",
            "localhost:7447",
            "LocalHost:7447",
        ] {
            assert!(is_loopback(address), "{address}");
        }
        for address in [
            "192.168.1.5:7447",
            "[fd00::1]:7447",
            "100.101.102.103:7447",
            "box.local:7447",
            "localhost.example.com:7447",
        ] {
            assert!(!is_loopback(address), "{address}");
        }
    }
}
