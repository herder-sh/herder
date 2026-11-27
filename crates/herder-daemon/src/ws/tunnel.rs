//! The tunnel listener: where `cloudflared`, on this machine, delivers clients that reach the
//! daemon at a public hostname.
//!
//! Cloudflare terminates the client's TLS at its edge, so neither the daemon's pinned
//! certificate nor the device certificate would survive it. The client therefore runs its
//! ordinary connection (TLS with its device certificate, then the WebSocket and hello) inside
//! the binary frames of the WebSocket Cloudflare relays: [`Frames`] turns those frames back
//! into the byte stream a direct connection's TCP socket carries, and from there the
//! connection is served exactly as a direct one. Cloudflare relays only ciphertext.
//!
//! The listener takes nothing but WebSocket upgrades, and refuses any with an `Origin` header:
//! a browser always sends one, and a herder client never does.
//!
//! Every tunneled connection comes from `cloudflared` on loopback, so the internet reaches the
//! daemon through it as one peer. A [`Gate`] caps the connections that have not authenticated
//! yet, per client address as Cloudflare names it and in all, so that a flood of strangers
//! can neither exhaust the daemon nor bury a person's apps in login requests; one past a cap
//! is refused (429) before its TLS starts. Direct connections pass a gate of their own, per
//! peer address, so that strangers flooding one way in do not lock everyone out of the other.
//!
//! Until it authenticates, a connection may read only [`UNAUTHENTICATED_BYTES`]: a stranger
//! gets no more of the daemon's memory than a sign-in takes, however large a message it sends.

use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, ready};

use anyhow::Context as _;
use futures_util::{Sink, Stream};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Bytes;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tracing::warn;

/// The header Cloudflare names the client's address in; trusted only because the tunnel
/// listener is on loopback, where only `cloudflared` reaches it.
const CONNECTING_IP: &str = "cf-connecting-ip";

/// A byte stream: a TCP socket, TLS on one, or a WebSocket's frames.
pub(crate) trait Io: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}

/// A WebSocket whose binary frames carry a byte stream, read and written as one: the outer
/// WebSocket of a tunnel connection, or a relay stream through the hub.
pub(crate) struct Frames<S = TcpStream> {
    ws: WebSocketStream<S>,
    /// What is left of the binary frame read last.
    read: Bytes,
}

/// Tunneled connections not yet authenticated, in all: each holds a little memory for at most
/// the handshake timeout, or a login request until it is approved or expires.
pub(crate) const UNAUTHENTICATED: usize = 64;

/// Tunneled connections not yet authenticated from one client address: a few devices of one
/// household signing in at once, with room for their retries.
pub(crate) const UNAUTHENTICATED_PER_CLIENT: usize = 8;

/// Direct connections not yet authenticated from one peer address. Wider than a tunnel
/// client's: a peer may be a router or a cluster node that every device behind it comes
/// through.
pub(crate) const UNAUTHENTICATED_PER_PEER: usize = 32;

/// Bytes a connection may read before it authenticates. Its TLS handshake, upgrade and hello,
/// or a sign-in with a new passkey, take a few kilobytes; a hello resuming a thousand sessions
/// some 60 KiB.
pub(crate) const UNAUTHENTICATED_BYTES: usize = 256 * 1024;

/// The largest frame, and message, the tunnel listener reads. Each frame carries one write of
/// the client's TLS, a record of at most about 16 KiB; a larger one is held in memory whole
/// before any of its bytes count against [`UNAUTHENTICATED_BYTES`].
const TUNNEL_FRAME_BYTES: usize = 256 * 1024;

/// The gates of a daemon's or a hub's connections: tunneled ones, by the client address
/// Cloudflare names, and direct ones, by peer address.
pub(crate) struct Gates {
    pub(crate) tunnel: Arc<Gate>,
    pub(crate) direct: Arc<Gate>,
}

impl Gates {
    pub(crate) fn new() -> Self {
        Self {
            tunnel: Gate::new(UNAUTHENTICATED, UNAUTHENTICATED_PER_CLIENT),
            direct: Gate::new(UNAUTHENTICATED, UNAUTHENTICATED_PER_PEER),
        }
    }

    /// A pass for a direct connection from `peer`, unless that would pass a cap.
    pub(crate) fn direct(&self, peer: std::net::SocketAddr) -> anyhow::Result<Pass> {
        let peer = peer.ip().to_string();
        let pass = self.direct.enter(Some(&peer));
        if pass.is_none() {
            warn!(%peer, "refused a connection: too many from it are not signed in yet");
        }
        pass.context("too many connections are not signed in yet")
    }
}

/// Counts the connections that have not authenticated yet.
pub(crate) struct Gate {
    total: usize,
    per_client: usize,
    open: Mutex<Open>,
}

#[derive(Default)]
struct Open {
    total: usize,
    by_client: HashMap<String, usize>,
}

impl Gate {
    pub(crate) fn new(total: usize, per_client: usize) -> Arc<Self> {
        Arc::new(Self {
            total,
            per_client,
            open: Mutex::new(Open::default()),
        })
    }

    /// A pass for one more connection from `client`, unless that would pass a cap. A
    /// connection Cloudflare names no address for counts as its own client, `unknown`.
    pub(crate) fn enter(self: &Arc<Self>, client: Option<&str>) -> Option<Pass> {
        let client = client.unwrap_or("unknown").to_owned();
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        let from_client = open.by_client.get(&client).copied().unwrap_or(0);
        if open.total >= self.total || from_client >= self.per_client {
            return None;
        }
        open.total += 1;
        open.by_client.insert(client.clone(), from_client + 1);
        Some(Pass {
            gate: Arc::clone(self),
            client,
            budget: Budget(Arc::new(AtomicUsize::new(UNAUTHENTICATED_BYTES))),
        })
    }
}

/// One unauthenticated connection, counted, and its reads limited, until it is dropped: once
/// the connection authenticated, or ended.
pub(crate) struct Pass {
    gate: Arc<Gate>,
    client: String,
    budget: Budget,
}

impl Pass {
    /// `transport`, reading at most [`UNAUTHENTICATED_BYTES`] until this pass is dropped.
    pub(crate) fn meter(&self, transport: Transport) -> Metered {
        Metered {
            transport,
            budget: self.budget.clone(),
        }
    }
}

impl Drop for Pass {
    fn drop(&mut self) {
        self.budget.lift();
        let mut open = self
            .gate
            .open
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        open.total -= 1;
        if let Some(count) = open.by_client.get_mut(&self.client) {
            *count -= 1;
            if *count == 0 {
                open.by_client.remove(&self.client);
            }
        }
    }
}

/// Takes `cloudflared`'s WebSocket upgrade on `stream`, if `gate` lets one more
/// unauthenticated connection in: the connection's frames, its pass, and the client's address
/// as Cloudflare names it, if it does.
pub(crate) async fn accept(
    stream: TcpStream,
    gate: &Arc<Gate>,
) -> anyhow::Result<(Frames, Pass, Option<String>)> {
    let mut client = None;
    let mut pass = None;
    // tungstenite's handshake callback returns its `ErrorResponse` by value; the type is
    // theirs, so its size is not ours to change.
    #[allow(clippy::result_large_err)]
    let check = |request: &Request, response: Response| {
        if request.headers().contains_key("origin") {
            let mut refused = ErrorResponse::new(Some("browsers may not connect".to_owned()));
            *refused.status_mut() = StatusCode::FORBIDDEN;
            return Err(refused);
        }
        client = request
            .headers()
            .get(CONNECTING_IP)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        pass = gate.enter(client.as_deref());
        if pass.is_none() {
            warn!(
                client = client.as_deref().unwrap_or("unknown"),
                "refused a tunneled connection: too many are not signed in yet"
            );
            let mut refused = ErrorResponse::new(Some("too many connections".to_owned()));
            *refused.status_mut() = StatusCode::TOO_MANY_REQUESTS;
            return Err(refused);
        }
        Ok(response)
    };
    let config = config()
        .max_frame_size(Some(TUNNEL_FRAME_BYTES))
        .max_message_size(Some(TUNNEL_FRAME_BYTES));
    let ws = tokio_tungstenite::accept_hdr_async_with_config(stream, check, Some(config)).await?;
    let pass = pass.context("the upgrade was taken without a pass")?;
    Ok((Frames::new(ws), pass, client))
}

/// The WebSocket config a tunnel's frames want: each frame goes out as soon as it is written,
/// as the TLS inside decides what to batch.
pub(crate) fn config() -> WebSocketConfig {
    WebSocketConfig::default().write_buffer_size(0)
}

impl<S> Frames<S> {
    /// The frames of `ws`, on either end of it.
    pub(crate) fn new(ws: WebSocketStream<S>) -> Self {
        Self {
            ws,
            read: Bytes::new(),
        }
    }
}

fn broken(err: tokio_tungstenite::tungstenite::Error) -> io::Error {
    io::Error::other(err)
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for Frames<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            if !self.read.is_empty() {
                let n = self.read.len().min(buf.remaining());
                buf.put_slice(&self.read.split_to(n));
                return Poll::Ready(Ok(()));
            }
            match ready!(Pin::new(&mut self.ws).poll_next(cx)) {
                Some(Ok(Message::Binary(data))) => self.read = data,
                // Pings are answered by the WebSocket itself.
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | None => return Poll::Ready(Ok(())),
                Some(Ok(Message::Text(_) | Message::Frame(_))) => {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "a tunnel carries binary frames only",
                    )));
                }
                Some(Err(err)) => return Poll::Ready(Err(broken(err))),
            }
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for Frames<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        ready!(Pin::new(&mut self.ws).poll_ready(cx)).map_err(broken)?;
        Pin::new(&mut self.ws)
            .start_send(Message::binary(buf.to_vec()))
            .map_err(broken)?;
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.ws).poll_flush(cx).map_err(broken)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.ws).poll_close(cx).map_err(broken)
    }
}

/// The byte stream a client's TLS runs on: its TCP socket, a tunnel's frames, or a relay
/// stream through the hub.
pub(crate) enum Transport {
    Tcp(TcpStream),
    Tunnel(Box<Frames>),
    Relay(Box<dyn Io>),
}

impl From<TcpStream> for Transport {
    fn from(stream: TcpStream) -> Self {
        Self::Tcp(stream)
    }
}

impl AsyncRead for Transport {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tunnel(frames) => Pin::new(frames.as_mut()).poll_read(cx, buf),
            Self::Relay(stream) => Pin::new(stream.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Transport {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tunnel(frames) => Pin::new(frames.as_mut()).poll_write(cx, buf),
            Self::Relay(stream) => Pin::new(stream.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tunnel(frames) => Pin::new(frames.as_mut()).poll_flush(cx),
            Self::Relay(stream) => Pin::new(stream.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tunnel(frames) => Pin::new(frames.as_mut()).poll_shutdown(cx),
            Self::Relay(stream) => Pin::new(stream.as_mut()).poll_shutdown(cx),
        }
    }
}

/// What a connection may still read before it authenticates; [`Budget::LIFTED`] once it did.
#[derive(Clone)]
struct Budget(Arc<AtomicUsize>);

impl Budget {
    const LIFTED: usize = usize::MAX;

    /// Takes `bytes` off what is left; false when that is less.
    fn spend(&self, bytes: usize) -> bool {
        self.0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| match left {
                Self::LIFTED => Some(left),
                _ => left.checked_sub(bytes),
            })
            .is_ok()
    }

    fn lift(&self) {
        self.0.store(Self::LIFTED, Ordering::Relaxed);
    }
}

/// A connection's [`Transport`], whose reads its [`Pass`] limits until it is dropped.
pub(crate) struct Metered {
    transport: Transport,
    budget: Budget,
}

impl Metered {
    /// The connection metered.
    pub(crate) fn transport(&self) -> &Transport {
        &self.transport
    }
}

impl AsyncRead for Metered {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        ready!(Pin::new(&mut self.transport).poll_read(cx, buf))?;
        if !self.budget.spend(buf.filled().len() - before) {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("over {UNAUTHENTICATED_BYTES} bytes sent before signing in"),
            )));
        }
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for Metered {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.transport).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.transport).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.transport).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gate_caps_each_client_and_everyone_and_frees_a_dropped_pass() {
        let gate = Gate::new(3, 2);
        let a1 = gate.enter(Some("a")).unwrap();
        let _a2 = gate.enter(Some("a")).unwrap();
        assert!(gate.enter(Some("a")).is_none(), "a third from a");
        let b = gate.enter(Some("b")).unwrap();
        assert!(gate.enter(Some("c")).is_none(), "a fourth in all");
        drop(a1);
        let _a3 = gate.enter(Some("a")).unwrap();
        assert!(gate.enter(None).is_none());
        drop(b);
        let unknown = gate.enter(None).unwrap();
        drop(unknown);
        assert!(!gate.open.lock().unwrap().by_client.contains_key("unknown"));
    }

    #[test]
    fn a_pass_limits_what_is_read_until_it_is_dropped() {
        let gate = Gate::new(2, 2);
        let pass = gate.enter(Some("a")).unwrap();
        let budget = pass.budget.clone();
        assert!(budget.spend(UNAUTHENTICATED_BYTES - 1));
        assert!(budget.spend(1));
        assert!(!budget.spend(1), "past the budget");
        drop(pass);
        assert!(budget.spend(usize::MAX - 1), "authenticated: no limit");
    }

    /// Fuzz target, run briefly by `cargo test`, at length with `PROPTEST_CASES`: the gate
    /// against a model of it, over any order of strangers coming and going.
    mod fuzz {
        use proptest::prelude::*;

        use super::*;

        proptest! {
            #[test]
            fn fuzz_the_gate(
                total in 1usize..8,
                per_client in 1usize..4,
                steps in prop::collection::vec((any::<bool>(), proptest::option::of(0u8..4), any::<prop::sample::Index>()), 0..64),
            ) {
                let gate = Gate::new(total, per_client);
                let mut passes: Vec<(Option<u8>, Pass)> = Vec::new();
                for (enter, client, which) in steps {
                    if enter || passes.is_empty() {
                        let name = client.map(|client| client.to_string());
                        let from = passes.iter().filter(|(of, _)| *of == client).count();
                        let fits = passes.len() < total && from < per_client;
                        let pass = gate.enter(name.as_deref());
                        prop_assert_eq!(pass.is_some(), fits);
                        if let Some(pass) = pass {
                            passes.push((client, pass));
                        }
                    } else {
                        passes.swap_remove(which.index(passes.len()));
                    }
                }
                drop(passes);
                let open = gate.open.lock().unwrap();
                prop_assert_eq!(open.total, 0);
                prop_assert!(open.by_client.is_empty());
            }
        }
    }
}
