//! A WireGuard endpoint for many peers, taking connections in: a hub's, and a test's server.
//!
//! Each peer is a public key and the one address it has inside the tunnel. The endpoint
//! answers a handshake only from a peer it has: a packet that is not WireGuard, one whose MAC
//! does not prove its sender knows the endpoint's public key, and a valid handshake from a
//! key it does not have all go unanswered. Inside the tunnel it takes TCP connections to the
//! addresses it listens on and nothing else; it forwards no packet anywhere.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};

use boringtun::noise::handshake::parse_handshake_anon;
use boringtun::noise::rate_limiter::RateLimiter;
use boringtun::noise::{Packet, Tunn, TunnResult};
use boringtun::x25519::{PublicKey, StaticSecret};
use smoltcp::wire::IpCidr;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::net::{MTU, Shared, TcpStream};
use crate::{Key, TIMER_TICK};

/// Handshakes a second the endpoint takes before it asks senders to prove their address with a
/// cookie, as WireGuard does under load.
const HANDSHAKES_PER_SECOND: u64 = 100;

/// A peer of an endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    /// Its public key.
    pub public_key: Key,
    /// Its address inside the tunnel: what it sends from, and the only address the endpoint
    /// sends it.
    pub address: IpAddr,
}

/// A connection a peer opened through the endpoint.
pub struct Accepted {
    /// The connection.
    pub stream: TcpStream,
    /// Where the peer connects from, inside the tunnel.
    pub from: SocketAddr,
}

/// A WireGuard endpoint on one UDP socket, up until dropped.
pub struct Endpoint {
    shared: Arc<Shared>,
    peers: Arc<Mutex<Vec<Peer>>>,
    accepted: tokio::sync::Mutex<mpsc::UnboundedReceiver<Accepted>>,
    local_addr: SocketAddr,
    public_key: Key,
    stop: CancellationToken,
}

impl Endpoint {
    /// Binds `udp` with `private_key`, its own address inside the tunnel being `address`,
    /// and takes TCP connections to `listen`, inside the tunnel, from its peers. It has none
    /// until [`Endpoint::set_peers`].
    pub async fn bind(
        udp: SocketAddr,
        private_key: Key,
        address: IpCidr,
        listen: SocketAddr,
    ) -> io::Result<Self> {
        let socket = UdpSocket::bind(udp).await?;
        let local_addr = socket.local_addr()?;
        let shared = Shared::new(&[address]);
        shared.listen(listen);
        let peers = Arc::new(Mutex::new(Vec::new()));
        let (accept, accepted) = mpsc::unbounded_channel();
        let stop = CancellationToken::new();
        let secret = StaticSecret::from(private_key.0);
        let public_key = Key(PublicKey::from(&secret).to_bytes());
        tokio::spawn(drive(
            Arc::clone(&shared),
            Arc::clone(&peers),
            socket,
            secret,
            accept,
            stop.clone(),
        ));
        Ok(Self {
            shared,
            peers,
            accepted: tokio::sync::Mutex::new(accepted),
            local_addr,
            public_key,
            stop,
        })
    }

    /// The UDP address it is bound to.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Its public key, for its peers' configs.
    pub fn public_key(&self) -> Key {
        self.public_key
    }

    /// Takes these peers from now on. A peer no longer among them is cut off at once: its
    /// session and its connections end, and its handshakes go unanswered.
    pub fn set_peers(&self, peers: Vec<Peer>) {
        let mut held = self.peers.lock().unwrap_or_else(PoisonError::into_inner);
        let gone: Vec<IpAddr> = held
            .iter()
            .filter(|peer| !peers.contains(peer))
            .map(|peer| peer.address)
            .collect();
        *held = peers;
        drop(held);
        for address in gone {
            self.shared.abort_with(address);
        }
        self.shared.poke.notify_one();
    }

    /// The next connection a peer opened; `None` once the endpoint stopped.
    pub async fn accept(&self) -> Option<Accepted> {
        self.accepted.lock().await.recv().await
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// A peer's session, while it has one.
struct Session {
    peer: Peer,
    tunn: Tunn,
    /// Where its datagrams last came from: where the endpoint sends.
    from: Option<SocketAddr>,
}

/// What the endpoint's task holds.
struct State {
    shared: Arc<Shared>,
    peers: Arc<Mutex<Vec<Peer>>>,
    udp: UdpSocket,
    secret: StaticSecret,
    public: PublicKey,
    limiter: Arc<RateLimiter>,
    /// By the index boringtun puts in its packets' receiver field, shifted right by 8.
    sessions: HashMap<u32, Session>,
    next_index: u32,
}

/// The endpoint's task.
async fn drive(
    shared: Arc<Shared>,
    peers: Arc<Mutex<Vec<Peer>>>,
    udp: UdpSocket,
    secret: StaticSecret,
    accept: mpsc::UnboundedSender<Accepted>,
    stop: CancellationToken,
) {
    let public = PublicKey::from(&secret);
    let mut state = State {
        shared,
        peers,
        udp,
        limiter: Arc::new(RateLimiter::new(&public, HANDSHAKES_PER_SECOND)),
        secret,
        public,
        sessions: HashMap::new(),
        next_index: 1,
    };
    let mut out = vec![0u8; MTU + 148];
    let mut datagram = vec![0u8; MTU + 148];
    let mut timers = tokio::time::interval(TIMER_TICK);
    timers.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut limiter_reset = Instant::now();
    loop {
        let (packets, delay) = state.shared.lock().poll();
        for packet in packets {
            state.send(&packet, &mut out).await;
        }
        // Peers that are gone lose their sessions, once the resets of their connections went
        // out on them.
        {
            let peers = state.peers.lock().unwrap_or_else(PoisonError::into_inner);
            state
                .sessions
                .retain(|_, session| peers.contains(&session.peer));
        }
        for (stream, from) in state.shared.accepted() {
            let _ = accept.send(Accepted { stream, from });
        }
        let wake = Instant::now() + delay.unwrap_or(TIMER_TICK).min(TIMER_TICK);
        tokio::select! {
            () = stop.cancelled() => break,
            () = state.shared.poke.notified() => {}
            () = tokio::time::sleep_until(wake) => {}
            _ = timers.tick() => {
                if limiter_reset.elapsed() >= std::time::Duration::from_secs(1) {
                    state.limiter.reset_count();
                    limiter_reset = Instant::now();
                }
                for session in state.sessions.values_mut() {
                    if let (TunnResult::WriteToNetwork(sent), Some(from)) =
                        (session.tunn.update_timers(&mut out), session.from)
                    {
                        let _ = state.udp.send_to(sent, from).await;
                    }
                }
            }
            received = state.udp.recv_from(&mut datagram) => {
                let Ok((len, from)) = received else { continue };
                state.take(&datagram[..len], from, &mut out).await;
                // Everything waiting is taken before the stack is polled, so a burst does
                // not overflow the socket's buffer.
                while let Ok((len, from)) = state.udp.try_recv_from(&mut datagram) {
                    state.take(&datagram[..len], from, &mut out).await;
                }
            }
        }
    }
}

impl State {
    /// Sends `packet`, from the stack, to the peer it is addressed to, if it has a session.
    async fn send(&mut self, packet: &[u8], out: &mut [u8]) {
        let Some(to) = Tunn::dst_address(packet) else {
            return;
        };
        let Some(session) = self.sessions.values_mut().find(|s| s.peer.address == to) else {
            return;
        };
        if let (TunnResult::WriteToNetwork(sent), Some(from)) =
            (session.tunn.encapsulate(packet, out), session.from)
        {
            let _ = self.udp.send_to(sent, from).await;
        }
    }

    /// Takes in a datagram from `from`.
    async fn take(&mut self, datagram: &[u8], from: SocketAddr, out: &mut [u8]) {
        let mut cookie = [0u8; 64];
        let index = match self
            .limiter
            .verify_packet(Some(from.ip()), datagram, &mut cookie)
        {
            // Only a sender that knows the endpoint's public key gets a cookie.
            Err(TunnResult::WriteToNetwork(reply)) => {
                let _ = self.udp.send_to(reply, from).await;
                return;
            }
            Err(_) => return,
            Ok(Packet::HandshakeInit(init)) => {
                let Ok(half) = parse_handshake_anon(&self.secret, &self.public, &init) else {
                    return;
                };
                let known = self
                    .peers
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .iter()
                    .find(|peer| peer.public_key.0 == half.peer_static_public)
                    .cloned();
                // A key the endpoint does not have gets no answer.
                let Some(peer) = known else { return };
                match self.sessions.iter().find(|(_, s)| s.peer == peer) {
                    Some((index, _)) => *index,
                    None => {
                        let index = self.next_index;
                        self.next_index = (self.next_index + 1) & 0x00ff_ffff;
                        let tunn = Tunn::new(
                            self.secret.clone(),
                            PublicKey::from(peer.public_key.0),
                            None,
                            None,
                            index,
                            Some(Arc::clone(&self.limiter)),
                        );
                        let session = Session {
                            peer,
                            tunn,
                            from: None,
                        };
                        self.sessions.insert(index, session);
                        index
                    }
                }
            }
            Ok(Packet::HandshakeResponse(response)) => response.receiver_idx >> 8,
            Ok(Packet::PacketCookieReply(reply)) => reply.receiver_idx >> 8,
            Ok(Packet::PacketData(data)) => data.receiver_idx >> 8,
        };
        let Some(session) = self.sessions.get_mut(&index) else {
            return;
        };
        let mut input = datagram;
        loop {
            match session.tunn.decapsulate(Some(from.ip()), input, out) {
                TunnResult::WriteToNetwork(sent) => {
                    session.from = Some(from);
                    let _ = self.udp.send_to(sent, from).await;
                    input = &[];
                }
                TunnResult::WriteToTunnelV4(packet, source) => {
                    session.from = Some(from);
                    // A peer sends from its own address only.
                    if IpAddr::from(source) == session.peer.address {
                        self.shared.lock().receive(packet.to_vec());
                    }
                    break;
                }
                TunnResult::WriteToTunnelV6(packet, source) => {
                    session.from = Some(from);
                    if IpAddr::from(source) == session.peer.address {
                        self.shared.lock().receive(packet.to_vec());
                    }
                    break;
                }
                TunnResult::Done | TunnResult::Err(_) => break,
            }
        }
    }
}
