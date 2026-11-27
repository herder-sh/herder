//! The userspace TCP/IP stack inside a tunnel: a `smoltcp` interface whose packets are the
//! tunnel's, and its TCP sockets as tokio streams.
//!
//! The interface and its sockets sit behind one lock, shared by the tunnel's driver task and
//! the streams. A stream reads and writes its socket's buffers under the lock and pokes the
//! driver, which polls the interface: that moves the buffers' bytes into packets and the
//! packets' into the buffers, and wakes the streams waiting on them.

use std::collections::VecDeque;
use std::future::poll_fn;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{self, DeviceCapabilities, Medium};
use smoltcp::socket::tcp;
use smoltcp::time::Instant;
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr, IpEndpoint, IpListenEndpoint};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::Notify;

/// The largest IP packet inside the tunnel: WireGuard's default MTU, which fits a 1500-byte
/// path with WireGuard's and UDP's headers over IPv6.
pub(crate) const MTU: usize = 1420;

/// Bytes each TCP socket buffers each way.
const BUFFER: usize = 256 * 1024;

/// The first local port a connection from inside the tunnel uses.
const FIRST_PORT: u16 = 49152;

/// The interface's packets in and out.
#[derive(Default)]
struct Queue {
    /// Packets from the tunnel, for the interface.
    rx: VecDeque<Vec<u8>>,
    /// Packets from the interface, for the tunnel.
    tx: VecDeque<Vec<u8>>,
}

impl phy::Device for Queue {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx<'a>;

    fn receive(&mut self, _: Instant) -> Option<(Rx, Tx<'_>)> {
        let packet = self.rx.pop_front()?;
        Some((Rx(packet), Tx(&mut self.tx)))
    }

    fn transmit(&mut self, _: Instant) -> Option<Tx<'_>> {
        Some(Tx(&mut self.tx))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip;
        caps.max_transmission_unit = MTU;
        caps
    }
}

struct Rx(Vec<u8>);

impl phy::RxToken for Rx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

struct Tx<'a>(&'a mut VecDeque<Vec<u8>>);

impl phy::TxToken for Tx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut packet = vec![0; len];
        let result = f(&mut packet);
        self.0.push_back(packet);
        result
    }
}

/// The stack.
pub(crate) struct Net {
    iface: Interface,
    sockets: SocketSet<'static>,
    queue: Queue,
    next_port: u16,
    /// Sockets whose stream is gone: removed once closed.
    dropped: Vec<SocketHandle>,
    /// Sockets listening for connections, for an endpoint.
    listening: Vec<SocketHandle>,
}

/// What the driver and the streams share.
pub(crate) struct Shared {
    net: Mutex<Net>,
    /// Tells the driver to poll the interface.
    pub(crate) poke: Notify,
}

impl Shared {
    /// A stack whose own addresses are `addresses`.
    pub(crate) fn new(addresses: &[IpCidr]) -> Arc<Self> {
        let mut queue = Queue::default();
        let mut iface = Interface::new(Config::new(HardwareAddress::Ip), &mut queue, now());
        iface.update_ip_addrs(|ips| {
            for address in addresses {
                // The interface holds a few addresses; a config with more uses the first.
                let _ = ips.push(*address);
            }
        });
        // Everything not on the tunnel's own addresses goes into the tunnel; the far end
        // routes it. A gateway on the interface's own address says that for an IP medium.
        if let Some(v4) = addresses.iter().find_map(|cidr| match cidr.address() {
            IpAddress::Ipv4(v4) => Some(v4),
            IpAddress::Ipv6(_) => None,
        }) {
            let _ = iface.routes_mut().add_default_ipv4_route(v4);
        }
        if let Some(v6) = addresses.iter().find_map(|cidr| match cidr.address() {
            IpAddress::Ipv6(v6) => Some(v6),
            IpAddress::Ipv4(_) => None,
        }) {
            let _ = iface.routes_mut().add_default_ipv6_route(v6);
        }
        Arc::new(Self {
            net: Mutex::new(Net {
                iface,
                sockets: SocketSet::new(Vec::new()),
                queue,
                next_port: FIRST_PORT,
                dropped: Vec::new(),
                listening: Vec::new(),
            }),
            poke: Notify::new(),
        })
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Net> {
        // Every change leaves the stack whole, so a poisoned lock holds a consistent one.
        self.net.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A TCP connection from inside the tunnel to `remote`, once established.
    pub(crate) async fn connect(self: &Arc<Self>, remote: SocketAddr) -> io::Result<TcpStream> {
        let handle = {
            let mut net = self.lock();
            let mut socket = new_socket();
            let port = net.port();
            let Net { iface, .. } = &mut *net;
            socket
                .connect(iface.context(), endpoint(remote), port)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
            net.sockets.add(socket)
        };
        let stream = TcpStream {
            shared: Arc::clone(self),
            handle,
        };
        self.poke.notify_one();
        poll_fn(|cx| {
            let mut net = self.lock();
            let socket = net.sockets.get_mut::<tcp::Socket>(handle);
            match socket.state() {
                tcp::State::Established => Poll::Ready(Ok(())),
                tcp::State::Closed | tcp::State::TimeWait | tcp::State::Closing => {
                    Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::ConnectionRefused,
                        format!("{remote} refused the connection through the tunnel"),
                    )))
                }
                _ => {
                    socket.register_send_waker(cx.waker());
                    socket.register_recv_waker(cx.waker());
                    Poll::Pending
                }
            }
        })
        .await?;
        Ok(stream)
    }

    /// Listens for connections to `local`, keeping one socket listening at all times.
    pub(crate) fn listen(&self, local: SocketAddr) {
        let mut net = self.lock();
        let mut socket = new_socket();
        let listen = IpListenEndpoint {
            addr: Some(IpAddress::from(local.ip())),
            port: local.port(),
        };
        // A fresh socket with a port always listens.
        let _ = socket.listen(listen);
        let handle = net.sockets.add(socket);
        net.listening.push(handle);
    }

    /// The connections a listening socket took since the last call, as streams, each with
    /// the address it comes from; a socket listens again in each one's place.
    pub(crate) fn accepted(self: &Arc<Self>) -> Vec<(TcpStream, SocketAddr)> {
        let mut accepted = Vec::new();
        let mut net = self.lock();
        let listening = std::mem::take(&mut net.listening);
        for handle in listening {
            let socket = net.sockets.get_mut::<tcp::Socket>(handle);
            if socket.state() == tcp::State::Listen {
                net.listening.push(handle);
                continue;
            }
            let (Some(remote), local) = (socket.remote_endpoint(), socket.listen_endpoint()) else {
                net.listening.push(handle);
                continue;
            };
            accepted.push((
                TcpStream {
                    shared: Arc::clone(self),
                    handle,
                },
                SocketAddr::new(IpAddr::from(remote.addr), remote.port),
            ));
            let mut socket = new_socket();
            let _ = socket.listen(local);
            let handle = net.sockets.add(socket);
            net.listening.push(handle);
        }
        accepted
    }

    /// Aborts every connection with `peer`.
    pub(crate) fn abort_with(&self, peer: IpAddr) {
        let mut net = self.lock();
        for (_, socket) in net.sockets.iter_mut() {
            if let smoltcp::socket::Socket::Tcp(socket) = socket
                && socket
                    .remote_endpoint()
                    .is_some_and(|remote| IpAddr::from(remote.addr) == peer)
            {
                socket.abort();
            }
        }
        drop(net);
        self.poke.notify_one();
    }
}

impl Net {
    /// Takes in a packet from the tunnel.
    pub(crate) fn receive(&mut self, packet: Vec<u8>) {
        self.queue.rx.push_back(packet);
    }

    /// Polls the interface: the packets for the tunnel, and how soon to poll again at the
    /// latest.
    pub(crate) fn poll(&mut self) -> (Vec<Vec<u8>>, Option<Duration>) {
        let now = now();
        let Net {
            iface,
            sockets,
            queue,
            dropped,
            ..
        } = self;
        iface.poll(now, queue, sockets);
        dropped.retain(|handle| {
            let state = sockets.get::<tcp::Socket>(*handle).state();
            let gone = matches!(state, tcp::State::Closed | tcp::State::TimeWait);
            if gone {
                sockets.remove(*handle);
            }
            !gone
        });
        let delay = iface
            .poll_delay(now, sockets)
            .map(|delay| Duration::from_micros(delay.total_micros()));
        (queue.tx.drain(..).collect(), delay)
    }

    /// How many connections are open or closing.
    pub(crate) fn connections(&self) -> usize {
        self.sockets.iter().count() - self.listening.len() - self.dropped.len()
    }

    fn port(&mut self) -> u16 {
        let port = self.next_port;
        self.next_port = self.next_port.checked_add(1).unwrap_or(FIRST_PORT);
        port
    }
}

fn new_socket() -> tcp::Socket<'static> {
    let mut socket = tcp::Socket::new(
        tcp::SocketBuffer::new(vec![0; BUFFER]),
        tcp::SocketBuffer::new(vec![0; BUFFER]),
    );
    // Each write goes out as written: the TLS inside decides what to batch.
    socket.set_nagle_enabled(false);
    // Without it a loss makes the socket send its whole window again at once, which on a
    // path that just lost a datagram loses more, and each loss doubles the wait.
    socket.set_congestion_control(tcp::CongestionControl::Cubic);
    socket
}

/// Whether `socket` is still in its TCP handshake, as an accepted one may be.
fn opening(socket: &tcp::Socket<'_>) -> bool {
    matches!(
        socket.state(),
        tcp::State::Listen | tcp::State::SynSent | tcp::State::SynReceived
    )
}

fn endpoint(address: SocketAddr) -> IpEndpoint {
    IpEndpoint::new(IpAddress::from(address.ip()), address.port())
}

/// Now, by the clock smoltcp's timers run on: tokio's, so a paused test clock pauses them.
fn now() -> Instant {
    static START: std::sync::OnceLock<tokio::time::Instant> = std::sync::OnceLock::new();
    let start = *START.get_or_init(tokio::time::Instant::now);
    let elapsed = tokio::time::Instant::now().saturating_duration_since(start);
    Instant::from_micros(i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX))
}

/// A TCP connection inside a tunnel.
pub struct TcpStream {
    shared: Arc<Shared>,
    handle: SocketHandle,
}

impl TcpStream {
    fn socket<R>(&self, f: impl FnOnce(&mut tcp::Socket<'static>) -> R) -> R {
        let mut net = self.shared.lock();
        let result = f(net.sockets.get_mut::<tcp::Socket>(self.handle));
        drop(net);
        self.shared.poke.notify_one();
        result
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.socket(|socket| {
            if socket.can_recv() {
                let read = socket
                    .recv_slice(buf.initialize_unfilled())
                    .map_err(|err| io::Error::other(err.to_string()))?;
                buf.advance(read);
                return Poll::Ready(Ok(()));
            }
            if !socket.may_recv() && !opening(socket) {
                return match socket.state() {
                    // A reset, or the tunnel's timeout, rather than a FIN.
                    tcp::State::Closed => Poll::Ready(Err(io::ErrorKind::ConnectionReset.into())),
                    _ => Poll::Ready(Ok(())),
                };
            }
            socket.register_recv_waker(cx.waker());
            Poll::Pending
        })
    }
}

impl AsyncWrite for TcpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.socket(|socket| {
            if !socket.may_send() && !opening(socket) {
                return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
            }
            if socket.can_send() {
                let sent = socket
                    .send_slice(buf)
                    .map_err(|err| io::Error::other(err.to_string()))?;
                return Poll::Ready(Ok(sent));
            }
            socket.register_send_waker(cx.waker());
            Poll::Pending
        })
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        // The driver sends what is buffered as soon as it is poked, which every write does.
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.socket(tcp::Socket::close);
        Poll::Ready(Ok(()))
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        let mut net = self.shared.lock();
        let socket = net.sockets.get_mut::<tcp::Socket>(self.handle);
        socket.close();
        // A far end that never answers the FIN does not keep the socket.
        socket.set_timeout(Some(smoltcp::time::Duration::from_secs(10)));
        net.dropped.push(self.handle);
        drop(net);
        self.shared.poke.notify_one();
    }
}
