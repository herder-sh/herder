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
//! is refused (429) before its TLS starts.

use std::collections::HashMap;
use std::io;
use std::pin::Pin;
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

/// The outer WebSocket of a tunnel connection, read and written as a byte stream.
pub(crate) struct Frames {
    ws: WebSocketStream<TcpStream>,
    /// What is left of the binary frame read last.
    read: Bytes,
}

/// Tunneled connections not yet authenticated, in all: each holds a little memory for at most
/// the handshake timeout, or a login request until it is approved or expires.
pub(crate) const UNAUTHENTICATED: usize = 64;

/// Tunneled connections not yet authenticated from one client address: a few devices of one
/// household signing in at once, with room for their retries.
pub(crate) const UNAUTHENTICATED_PER_CLIENT: usize = 8;

/// Counts the tunneled connections that have not authenticated yet.
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
        })
    }
}

/// One unauthenticated connection, counted until it is dropped: once the connection
/// authenticated, or ended.
pub(crate) struct Pass {
    gate: Arc<Gate>,
    client: String,
}

impl Drop for Pass {
    fn drop(&mut self) {
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
    let ws = tokio_tungstenite::accept_hdr_async_with_config(stream, check, Some(config())).await?;
    let pass = pass.context("the upgrade was taken without a pass")?;
    Ok((Frames::new(ws), pass, client))
}

/// The WebSocket config a tunnel's frames want: each frame goes out as soon as it is written,
/// as the TLS inside decides what to batch.
pub(crate) fn config() -> WebSocketConfig {
    WebSocketConfig::default().write_buffer_size(0)
}

impl Frames {
    /// The frames of `ws`, on either end of it.
    pub(crate) fn new(ws: WebSocketStream<TcpStream>) -> Self {
        Self {
            ws,
            read: Bytes::new(),
        }
    }
}

fn broken(err: tokio_tungstenite::tungstenite::Error) -> io::Error {
    io::Error::other(err)
}

impl AsyncRead for Frames {
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

impl AsyncWrite for Frames {
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

/// The byte stream a client's TLS runs on: its TCP socket, or a tunnel's frames.
pub(crate) enum Transport {
    Tcp(TcpStream),
    Tunnel(Box<Frames>),
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
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tunnel(frames) => Pin::new(frames.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tunnel(frames) => Pin::new(frames.as_mut()).poll_shutdown(cx),
        }
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
}
