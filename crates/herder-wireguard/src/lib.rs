//! herder's userspace WireGuard (`docs/remote-access.md`): tunnels that carry herder's own
//! connections only, with no TUN device, no system VPN profile and no privileges.
//!
//! WireGuard itself is `boringtun`'s `noise::Tunn`, the protocol's state machine; the
//! TCP/IP inside the tunnel is `smoltcp`. A [`Tunnel`] connects out through one server (the
//! user's own VPN, or a hub's endpoint); an [`Endpoint`] takes connections in from many
//! peers (a hub). Either way the connections are [`TcpStream`]s, which tokio reads and writes
//! like sockets, and everything else on the device is untouched.

use std::fmt;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use boringtun::x25519::{PublicKey, StaticSecret};
use ring::rand::{SecureRandom, SystemRandom};

mod client;
mod net;
mod server;

pub use client::{Tunnel, TunnelConfig};
pub use net::TcpStream;
pub use server::{Accepted, Endpoint, Peer};
pub use smoltcp::wire::IpCidr;

/// How often WireGuard's timers run: handshake retries, rekeys, expiry.
const TIMER_TICK: Duration = Duration::from_millis(250);

/// A WireGuard key, private or public: 32 bytes, written in base64.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Key(pub [u8; 32]);

impl Key {
    /// A fresh private key.
    pub fn generate() -> Result<Self, String> {
        let mut bytes = [0u8; 32];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| "the system has no randomness for a key".to_owned())?;
        Ok(Self(StaticSecret::from(bytes).to_bytes()))
    }

    /// The public key of this private key.
    pub fn public(&self) -> Self {
        Self(PublicKey::from(&StaticSecret::from(self.0)).to_bytes())
    }

    /// The key as WireGuard writes it.
    pub fn to_base64(&self) -> String {
        STANDARD.encode(self.0)
    }
}

impl std::str::FromStr for Key {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        STANDARD
            .decode(text.trim())
            .ok()
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            .map(Self)
            .ok_or_else(|| "a key is 32 bytes in base64, as `wg genkey` prints one".to_owned())
    }
}

impl fmt::Debug for Key {
    /// Shows no key, which may be private.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Key(..)")
    }
}
