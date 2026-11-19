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

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use futures_util::{Sink, Stream};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Bytes;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

/// The header Cloudflare names the client's address in; trusted only because the tunnel
/// listener is on loopback, where only `cloudflared` reaches it.
const CONNECTING_IP: &str = "cf-connecting-ip";

/// The outer WebSocket of a tunnel connection, read and written as a byte stream.
pub(crate) struct Frames {
    ws: WebSocketStream<TcpStream>,
    /// What is left of the binary frame read last.
    read: Bytes,
}

/// Takes `cloudflared`'s WebSocket upgrade on `stream`: the connection's frames, and the
/// client's address as Cloudflare names it, if it does.
pub(crate) async fn accept(stream: TcpStream) -> anyhow::Result<(Frames, Option<String>)> {
    let mut client = None;
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
        Ok(response)
    };
    let ws = tokio_tungstenite::accept_hdr_async_with_config(stream, check, Some(config())).await?;
    Ok((Frames::new(ws), client))
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
