//! The WebSocket upgrade of a connection to the hub, read by the hub itself so that it can
//! route by the request's path and answer once it knows its answer.
//!
//! A client asking for a relay stream sends the stream's first frames right behind its
//! request, before the hub answered ([`herder_protocol::relay`]). The hub reads the request
//! and nothing after it but what came in the same reads, which it keeps, so those frames
//! wait unread, in that buffer and the socket, until the stream is open or refused. A
//! WebSocket library's server handshake would take them for junk after the request.

use anyhow::{Context, Result, bail};
use herder_protocol::RelayError;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};

/// Most bytes of a request's head: a herder client's is a few hundred.
const MAX_HEAD: usize = 8 * 1024;

/// Most headers a request may have.
const MAX_HEADERS: usize = 32;

/// An upgrade request read off `stream`, not answered yet.
pub(crate) struct Upgrade<S> {
    stream: S,
    path: String,
    accept: String,
    /// What came after the request in the same reads: the start of the stream.
    tail: Vec<u8>,
}

/// Reads an upgrade request off `stream`: a `GET` asking for a WebSocket.
pub(crate) async fn read<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S) -> Result<Upgrade<S>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0; 2048];
    loop {
        let read = stream
            .read(&mut chunk)
            .await
            .context("reading the upgrade")?;
        if read == 0 {
            bail!("the peer closed the connection before its upgrade request ended");
        }
        buf.extend_from_slice(&chunk[..read]);
        let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut request = httparse::Request::new(&mut headers);
        match request.parse(&buf).context("the upgrade request")? {
            httparse::Status::Partial if buf.len() < MAX_HEAD => continue,
            httparse::Status::Partial => bail!("an upgrade request over {MAX_HEAD} bytes"),
            httparse::Status::Complete(head) => {
                if request.method != Some("GET") {
                    bail!("an upgrade must be a GET");
                }
                let header = |name: &str| {
                    request
                        .headers
                        .iter()
                        .find(|header| header.name.eq_ignore_ascii_case(name))
                        .and_then(|header| std::str::from_utf8(header.value).ok())
                };
                if !header("upgrade").is_some_and(|value| value.eq_ignore_ascii_case("websocket")) {
                    bail!("the request asks for no WebSocket");
                }
                if header("sec-websocket-version") != Some("13") {
                    bail!("the request asks for a WebSocket version other than 13");
                }
                let key = header("sec-websocket-key").context("the request has no key")?;
                let accept = derive_accept_key(key.trim().as_bytes());
                let path = request.path.unwrap_or("/").to_owned();
                let tail = buf.split_off(head);
                return Ok(Upgrade {
                    stream,
                    path,
                    accept,
                    tail,
                });
            }
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> Upgrade<S> {
    /// The request's path, with any query.
    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    /// Answers `101`: the WebSocket, which reads the bytes that came behind the request
    /// first.
    pub(crate) async fn accept(mut self, config: WebSocketConfig) -> Result<WebSocketStream<S>> {
        let answer = format!(
            "HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
             Sec-WebSocket-Accept: {}\r\n\r\n",
            self.accept
        );
        self.stream.write_all(answer.as_bytes()).await?;
        self.stream.flush().await?;
        Ok(
            WebSocketStream::from_partially_read(
                self.stream,
                self.tail,
                Role::Server,
                Some(config),
            )
            .await,
        )
    }

    /// Answers with `status` and, with one, `error` as JSON, and closes the connection. The
    /// bytes behind the request are dropped unread.
    pub(crate) async fn refuse(mut self, status: u16, error: Option<&RelayError>) -> Result<()> {
        let body = match error {
            Some(error) => serde_json::to_string(error)?,
            None => String::new(),
        };
        let reason = match status {
            400 => "Bad Request",
            403 => "Forbidden",
            404 => "Not Found",
            429 => "Too Many Requests",
            503 => "Service Unavailable",
            _ => "Error",
        };
        let answer = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        self.stream.write_all(answer.as_bytes()).await?;
        self.stream.shutdown().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUEST: &[u8] = b"GET /machines/h1 HTTP/1.1\r\nHost: herder\r\nConnection: Upgrade\r\n\
        Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
        Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n";

    #[tokio::test]
    async fn the_bytes_behind_a_request_are_kept_for_the_websocket() {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let mut sent = REQUEST.to_vec();
        sent.extend_from_slice(b"first frames");
        client.write_all(&sent).await.unwrap();
        let upgrade = read(server).await.unwrap();
        assert_eq!(upgrade.path(), "/machines/h1");
        assert_eq!(upgrade.tail, b"first frames");
        // RFC 6455's example.
        assert_eq!(upgrade.accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[tokio::test]
    async fn a_request_split_over_reads_is_read_whole() {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let reading = tokio::spawn(read(server));
        for piece in REQUEST.chunks(7) {
            client.write_all(piece).await.unwrap();
            tokio::task::yield_now().await;
        }
        let upgrade = reading.await.unwrap().unwrap();
        assert_eq!(upgrade.path(), "/machines/h1");
        assert!(upgrade.tail.is_empty());
    }

    #[tokio::test]
    async fn a_request_that_is_no_upgrade_is_refused() {
        for request in [
            &b"POST / HTTP/1.1\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: a\r\n\r\n"[..],
            b"GET / HTTP/1.1\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: a\r\n\r\n",
            b"GET / HTTP/1.1\r\nUpgrade: websocket\r\nSec-WebSocket-Key: a\r\n\r\n",
            b"GET / HTTP/1.1\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\r\n",
        ] {
            let (mut client, server) = tokio::io::duplex(64 * 1024);
            client.write_all(request).await.unwrap();
            assert!(read(server).await.is_err());
        }
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let reading = tokio::spawn(read(server));
        let head = format!("GET / HTTP/1.1\r\nX: {}\r\n", "a".repeat(MAX_HEAD));
        let _ = client.write_all(head.as_bytes()).await;
        assert!(reading.await.unwrap().is_err());
    }
}
