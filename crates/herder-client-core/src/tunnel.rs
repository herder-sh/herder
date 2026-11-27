//! Reaching a daemon through a tunnel's URL, such as a Cloudflare Tunnel's public hostname.
//!
//! The tunnel terminates TLS at its edge with a certificate of the public web PKI, checked
//! here against Mozilla's roots. Inside that WebSocket's binary frames the client then runs
//! its ordinary connection: TLS pinning the daemon's certificate and presenting the device's,
//! the WebSocket and the hello. The tunnel relays only ciphertext, and the daemon knows the
//! device as on a direct connection.
//!
//! A proxy in front of the tunnel may admit only requests that carry some header, as
//! Cloudflare Access does with a service token. The device keeps such headers per address
//! ([`Headers`]) and sends them on the outer upgrade only: they never reach the daemon, which
//! sees only what runs inside, nor a link shared with another device. A refused upgrade says
//! so, and what to check.

use std::collections::BTreeMap;
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
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
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

/// The headers a tunnel's upgrade carries, by name, as this device keeps them.
pub(crate) type Sent = BTreeMap<String, String>;

/// The headers each tunnel address of a machine sends on its upgrade, by address.
pub(crate) type Headers = BTreeMap<String, Sent>;

/// No headers.
pub(crate) static NONE: Sent = BTreeMap::new();

/// Headers a request needs to be a WebSocket upgrade at all, which the client sets itself.
const RESERVED: [&str; 6] = [
    "host",
    "connection",
    "upgrade",
    "content-length",
    "transfer-encoding",
    "origin",
];

/// `name: value` as a header a tunnel's upgrade may carry: a valid HTTP header, and none the
/// upgrade sets itself; the name as sent, lowercase.
pub(crate) fn header(name: &str, value: &str) -> Result<(String, String), String> {
    let name = name.trim();
    let parsed = HeaderName::from_bytes(name.as_bytes())
        .map_err(|_| format!("{name:?} is not a header name"))?;
    let lower = parsed.as_str();
    if RESERVED.contains(&lower) || lower.starts_with("sec-websocket-") {
        return Err(format!("{name} is set by herder itself"));
    }
    let value = value.trim();
    HeaderValue::from_str(value)
        .map_err(|_| format!("the value of {name} is not a header value"))?;
    Ok((lower.to_owned(), value.to_owned()))
}

/// Opens the tunnel at `url`, a saved `wss://` or `ws://` address, sending `headers` on its
/// upgrade: the frames to run the daemon connection in.
pub(crate) async fn open(url: &str, headers: &Sent) -> anyhow::Result<Frames> {
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
    let mut request = url.into_client_request()?;
    for (name, value) in headers {
        request.headers_mut().insert(
            HeaderName::from_bytes(name.as_bytes())?,
            HeaderValue::from_str(value)?,
        );
    }
    let (ws, _) = tokio_tungstenite::client_async_with_config(request, io, Some(config()))
        .await
        .map_err(|err| refused(err, headers))?;
    Ok(Frames::new(ws))
}

/// Why the tunnel did not take the upgrade, which sent `headers`: what a proxy in front of
/// it that refused says, and what to check.
fn refused(err: tokio_tungstenite::tungstenite::Error, headers: &Sent) -> anyhow::Error {
    use tokio_tungstenite::tungstenite::Error;
    let Error::Http(response) = err else {
        return anyhow::Error::new(err).context("the tunnel's WebSocket upgrade");
    };
    let status = response.status();
    let by_proxy = status.is_redirection() || status.as_u16() == 401 || status.as_u16() == 403;
    let check = match (by_proxy, headers.is_empty()) {
        (false, _) => String::new(),
        (true, true) => ": a proxy in front of it wants something this device does not send; \
                         if it takes headers, such as a Cloudflare Access service token, set \
                         them for this address"
            .to_owned(),
        (true, false) => {
            let names: Vec<&str> = headers.keys().map(String::as_str).collect();
            format!(
                ": the proxy in front of it refused the headers set for this address ({}); \
                 check them",
                names.join(", ")
            )
        }
    };
    anyhow!("the tunnel answered {status} to the WebSocket upgrade{check}")
}

/// Each frame goes out as soon as it is written, as the TLS inside decides what to batch.
pub(crate) fn config() -> WebSocketConfig {
    WebSocketConfig::default().write_buffer_size(0)
}

/// A WebSocket whose binary frames carry a byte stream, read and written as one: a tunnel's,
/// or a relay stream through a hub.
pub(crate) struct Frames<S = Box<dyn Io>> {
    ws: WebSocketStream<S>,
    /// What is left of the binary frame read last.
    read: Bytes,
}

impl<S> Frames<S> {
    pub(crate) fn new(ws: WebSocketStream<S>) -> Self {
        Self {
            ws,
            read: Bytes::new(),
        }
    }
}

/// The error a failed frame read or write is: the transport's own, as it said it, or the
/// WebSocket's.
fn broken(err: tokio_tungstenite::tungstenite::Error) -> io::Error {
    match err {
        tokio_tungstenite::tungstenite::Error::Io(err) => err,
        err => io::Error::other(err),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_is_valid_http_and_none_the_upgrade_sets_itself() {
        assert_eq!(
            header(" CF-Access-Client-Id ", " abc.access "),
            Ok(("cf-access-client-id".into(), "abc.access".into()))
        );
        for (name, value, why) in [
            ("Host", "x", "set by herder"),
            ("Origin", "https://evil", "set by herder"),
            ("Sec-WebSocket-Protocol", "x", "set by herder"),
            ("bad name", "x", "not a header name"),
            ("", "x", "not a header name"),
            ("X-Token", "line\nbreak", "not a header value"),
        ] {
            let err = header(name, value).unwrap_err();
            assert!(err.contains(why), "{name}: {err}");
        }
    }

    #[test]
    fn a_refused_upgrade_says_what_to_check() {
        use tokio_tungstenite::tungstenite::Error;
        use tokio_tungstenite::tungstenite::http::Response;
        let answered = |status: u16| {
            Error::Http(Box::new(
                Response::builder().status(status).body(None).unwrap(),
            ))
        };
        let sent = Sent::from([("cf-access-client-id".to_owned(), "id".to_owned())]);
        let none = format!("{:#}", refused(answered(403), &NONE));
        assert!(
            none.contains("403 Forbidden") && none.contains("set them for this address"),
            "{none}"
        );
        let wrong = format!("{:#}", refused(answered(302), &sent));
        assert!(
            wrong.contains("302")
                && wrong.contains("refused the headers set for this address (cf-access-client-id)"),
            "{wrong}"
        );
        // A tunnel that cannot reach the daemon is no matter of headers.
        let down = format!("{:#}", refused(answered(502), &sent));
        assert_eq!(
            down,
            "the tunnel answered 502 Bad Gateway to the WebSocket upgrade"
        );
    }
}
