//! Reaching a daemon through a tunnel's URL, such as a Cloudflare Tunnel's public hostname.
//!
//! The tunnel terminates TLS at its edge with a certificate of the public web PKI, checked
//! here against Mozilla's roots. Inside that WebSocket's binary frames the client then runs
//! its ordinary connection: TLS pinning the daemon's certificate and presenting the device's,
//! the WebSocket and the hello. The tunnel relays only ciphertext, and the daemon knows the
//! device as on a direct connection.

use std::io;
use std::pin::Pin;
use std::sync::{Arc, LazyLock};
use std::task::{Context, Poll, ready};

use anyhow::{Context as _, anyhow};
use futures_util::{Sink, Stream};
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Bytes, Message};

/// A byte stream: a TCP socket, outer TLS on one, or a tunnel's frames.
pub(crate) trait Io: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}

/// Outer TLS, for a `wss://` tunnel: the public web PKI, as a browser would check it.
static WEB_PKI: LazyLock<Result<Arc<rustls::ClientConfig>, String>> = LazyLock::new(|| {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let provider = rustls::crypto::ring::default_provider();
    rustls::ClientConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .map(|builder| Arc::new(builder.with_root_certificates(roots).with_no_client_auth()))
        .map_err(|err| err.to_string())
});

/// Opens the tunnel at `url`, a saved `wss://` or `ws://` address: the frames to run the
/// daemon connection in.
pub(crate) async fn open(url: &str) -> anyhow::Result<Frames> {
    let parsed = url::Url::parse(url).context("the tunnel URL")?;
    let host = parsed.host_str().context("the tunnel URL names no host")?;
    let port = parsed
        .port_or_known_default()
        .context("the tunnel URL names no port")?;
    let tcp = TcpStream::connect((host.trim_matches(['[', ']']), port))
        .await
        .context("connecting")?;
    tcp.set_nodelay(true)?;
    let io: Box<dyn Io> = if parsed.scheme() == "wss" {
        let config = WEB_PKI.as_ref().map_err(|err| anyhow!("TLS: {err}"))?;
        let name = ServerName::try_from(host.trim_matches(['[', ']']).to_owned())
            .context("the tunnel's TLS server name")?;
        let tls = TlsConnector::from(Arc::clone(config))
            .connect(name, tcp)
            .await
            .context("the tunnel's TLS")?;
        Box::new(tls)
    } else {
        Box::new(tcp)
    };
    let request = url.into_client_request()?;
    let (ws, _) = tokio_tungstenite::client_async_with_config(request, io, Some(config()))
        .await
        .context("the tunnel's WebSocket upgrade")?;
    Ok(Frames::new(ws))
}

/// Each frame goes out as soon as it is written, as the TLS inside decides what to batch.
fn config() -> WebSocketConfig {
    WebSocketConfig::default().write_buffer_size(0)
}

/// A tunnel's WebSocket, read and written as a byte stream.
pub(crate) struct Frames {
    ws: WebSocketStream<Box<dyn Io>>,
    /// What is left of the binary frame read last.
    read: Bytes,
}

impl Frames {
    fn new(ws: WebSocketStream<Box<dyn Io>>) -> Self {
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
