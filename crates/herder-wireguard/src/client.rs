//! A tunnel to one WireGuard server, for connections out: the user's own VPN server, or a
//! hub's endpoint.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use boringtun::noise::errors::WireGuardError;
use boringtun::noise::{Tunn, TunnResult};
use boringtun::x25519::{PublicKey, StaticSecret};
use smoltcp::wire::IpCidr;
use tokio::net::UdpSocket;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::net::{MTU, Shared, TcpStream};
use crate::{Key, TIMER_TICK};

/// Time a connection through the tunnel gets to be established, handshake included: past it,
/// the server is taken not to answer.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// What a tunnel to a WireGuard server is made of, as a `wg-quick` config has it.
#[derive(Clone)]
pub struct TunnelConfig {
    /// This device's private key.
    pub private_key: Key,
    /// The server's public key.
    pub peer_public_key: Key,
    /// The pre-shared key, when the config has one.
    pub preshared_key: Option<Key>,
    /// The server, as `host:port`.
    pub endpoint: String,
    /// This device's addresses inside the tunnel, with their prefix lengths.
    pub addresses: Vec<(std::net::IpAddr, u8)>,
}

/// A tunnel to a WireGuard server, up from [`Tunnel::open`] until it is dropped: one UDP
/// socket and a task that drives it. Only connections made with [`Tunnel::connect`] go
/// through it.
pub struct Tunnel {
    shared: Arc<Shared>,
    endpoint: String,
    stop: CancellationToken,
}

impl Tunnel {
    /// Resolves the server and opens the tunnel's socket; the handshake waits for the first
    /// connection. Needs a tokio runtime.
    pub async fn open(config: TunnelConfig) -> io::Result<Self> {
        let server = tokio::net::lookup_host(&config.endpoint)
            .await?
            .next()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{} resolves to no address", config.endpoint),
                )
            })?;
        let local: SocketAddr = if server.is_ipv4() {
            ([0, 0, 0, 0], 0).into()
        } else {
            ([0u16; 8], 0).into()
        };
        let udp = UdpSocket::bind(local).await?;
        udp.connect(server).await?;
        let addresses: Vec<IpCidr> = config
            .addresses
            .iter()
            .map(|(ip, prefix)| IpCidr::new((*ip).into(), *prefix))
            .collect();
        let shared = Shared::new(&addresses);
        let tunn = Tunn::new(
            StaticSecret::from(config.private_key.0),
            PublicKey::from(config.peer_public_key.0),
            config.preshared_key.map(|key| key.0),
            None,
            0,
            None,
        );
        let stop = CancellationToken::new();
        tokio::spawn(drive(
            Arc::clone(&shared),
            tunn,
            udp,
            stop.clone(),
            config.endpoint.clone(),
        ));
        Ok(Self {
            shared,
            endpoint: config.endpoint,
            stop,
        })
    }

    /// A TCP connection through the tunnel to `remote`, which the server routes.
    pub async fn connect(&self, remote: SocketAddr) -> io::Result<TcpStream> {
        tokio::time::timeout(CONNECT_TIMEOUT, self.shared.connect(remote))
            .await
            .unwrap_or_else(|_| {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "no answer from {remote} through the WireGuard server {} in {}s",
                        self.endpoint,
                        CONNECT_TIMEOUT.as_secs()
                    ),
                ))
            })
    }

    /// How many connections through the tunnel are open.
    pub fn connections(&self) -> usize {
        self.shared.lock().connections()
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// The tunnel's task: packets between the stack and the server, and WireGuard's timers.
async fn drive(
    shared: Arc<Shared>,
    mut tunn: Tunn,
    udp: UdpSocket,
    stop: CancellationToken,
    endpoint: String,
) {
    let mut out = vec![0u8; MTU + 148];
    let mut datagram = vec![0u8; MTU + 148];
    let mut timers = tokio::time::interval(TIMER_TICK);
    timers.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let (packets, delay) = shared.lock().poll();
        for packet in packets {
            if let TunnResult::WriteToNetwork(sent) = tunn.encapsulate(&packet, &mut out) {
                // A datagram lost here is one lost on the way: TCP sends it again.
                let _ = udp.send(sent).await;
            }
        }
        let wake = Instant::now() + delay.unwrap_or(TIMER_TICK).min(TIMER_TICK);
        tokio::select! {
            () = stop.cancelled() => break,
            () = shared.poke.notified() => {}
            () = tokio::time::sleep_until(wake) => {}
            _ = timers.tick() => {
                match tunn.update_timers(&mut out) {
                    TunnResult::WriteToNetwork(sent) => {
                        let _ = udp.send(sent).await;
                    }
                    TunnResult::Err(WireGuardError::ConnectionExpired) => {
                        debug!(%endpoint, "no WireGuard handshake answered");
                    }
                    _ => {}
                }
            }
            received = udp.recv(&mut datagram) => {
                let Ok(len) = received else { continue };
                take(&mut tunn, &udp, &shared, &datagram[..len], &mut out).await;
                // Everything waiting is taken before the stack is polled, so a burst does
                // not overflow the socket's buffer.
                while let Ok(len) = udp.try_recv(&mut datagram) {
                    take(&mut tunn, &udp, &shared, &datagram[..len], &mut out).await;
                }
            }
        }
    }
}

/// Takes in a datagram from the server.
async fn take(tunn: &mut Tunn, udp: &UdpSocket, shared: &Shared, datagram: &[u8], out: &mut [u8]) {
    let mut input = datagram;
    loop {
        match tunn.decapsulate(None, input, out) {
            TunnResult::WriteToNetwork(sent) => {
                let _ = udp.send(sent).await;
                // What the handshake held back goes out now.
                input = &[];
            }
            TunnResult::WriteToTunnelV4(packet, _) | TunnResult::WriteToTunnelV6(packet, _) => {
                shared.lock().receive(packet.to_vec());
                break;
            }
            TunnResult::Done | TunnResult::Err(_) => break,
        }
    }
}
