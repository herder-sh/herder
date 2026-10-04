//! Machine addresses: how a typed one is completed, and the order a pairing link's are saved in.
//!
//! The saved order is the order of preference: connecting starts at the first address and gives
//! each later one a head start less. A pairing link lists the daemon's addresses in interface
//! order, so pairing sorts them direct routes first: private network addresses (reached at home
//! or through a VPN into it, such as UniFi Teleport), then other addresses, then Tailscale. The
//! user can reorder them after that with [`crate::Client::set_addresses`]; nothing re-sorts them.

use std::net::{IpAddr, SocketAddr};

/// The port a daemon listens on unless configured otherwise, as `herder daemon` has it.
const DEFAULT_PORT: u16 = 7447;

/// `text`, an address as typed, as `host:port`: trimmed, with the default port added when it
/// has none, and IPv6 addresses bracketed.
pub(crate) fn normalize(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("an address is empty".to_owned());
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

/// 0 for a private network address, 1 for any other, 2 for a Tailscale one.
fn rank(address: &str) -> u8 {
    let host = address
        .rsplit_once(':')
        .map_or(address, |(host, _)| host)
        .trim_matches(['[', ']']);
    let Ok(ip) = host.parse::<IpAddr>() else {
        return if host.ends_with(".ts.net") {
            2
        } else if host.ends_with(".local") || !host.contains('.') {
            0
        } else {
            1
        };
    };
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, ..] = ip.octets();
            // Tailscale hands out addresses from the CGNAT range, 100.64.0.0/10.
            if a == 100 && b & 0xc0 == 64 {
                2
            } else if ip.is_private() || ip.is_loopback() || ip.is_link_local() {
                0
            } else {
                1
            }
        }
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            // Tailscale's unique local prefix, fd7a:115c:a1e0::/48.
            if segments[..3] == [0xfd7a, 0x115c, 0xa1e0] {
                2
            } else if ip.is_unique_local() || ip.is_loopback() || ip.is_unicast_link_local() {
                0
            } else {
                1
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            "http://box",
            ":7447",
        ] {
            assert!(normalize(bad).is_err(), "{bad:?}");
        }
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
        assert_eq!(rank("100.128.0.1:7447"), 1);
    }
}
