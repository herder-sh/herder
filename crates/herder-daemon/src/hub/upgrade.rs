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

    /// The stream the request came on.
    pub(crate) fn stream(&self) -> &S {
        &self.stream
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
    use tokio::io::DuplexStream;

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

    /// Fuzz targets, run briefly by `cargo test`, at length with `PROPTEST_CASES`: the
    /// parser reads what any peer sends before it is authenticated.
    mod fuzz {
        use herder_protocol::RelayRoute;
        use proptest::prelude::*;

        use super::*;

        /// `sent` read off a stream it arrives on in pieces cut at `cuts`, after which the peer
        /// closes; with the upgrade, the rest of the stream.
        fn read_sent(sent: &[u8], cuts: &[usize]) -> Result<(Upgrade<DuplexStream>, Vec<u8>)> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            runtime.block_on(async {
                let (mut client, server) = tokio::io::duplex(sent.len() + 1);
                let sent = sent.to_vec();
                let mut cuts = cuts.to_vec();
                let writing = tokio::spawn(async move {
                    let mut rest = &sent[..];
                    // A request refused part way closes the stream under the rest.
                    for cut in cuts.drain(..) {
                        let (piece, after) = rest.split_at(cut.min(rest.len()));
                        if client.write_all(piece).await.is_err() {
                            return;
                        }
                        rest = after;
                        tokio::task::yield_now().await;
                    }
                    let _ = client.write_all(rest).await;
                });
                let read = read(server).await;
                writing.await.unwrap();
                let mut upgrade = read?;
                let mut rest = Vec::new();
                upgrade.stream.read_to_end(&mut rest).await.unwrap();
                Ok((upgrade, rest))
            })
        }

        /// A herder client's upgrade request for `path`, with `headers` beside its own.
        fn request(path: &str, headers: &[(String, String)]) -> Vec<u8> {
            let mut request = format!(
                "GET {path} HTTP/1.1\r\nHost: herder\r\nConnection: Upgrade\r\n\
                 Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
                 Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"
            );
            for (name, value) in headers {
                request.push_str(&format!("{name}: {value}\r\n"));
            }
            request.push_str("\r\n");
            request.into_bytes()
        }

        fn cuts() -> impl Strategy<Value = Vec<usize>> {
            prop::collection::vec(0usize..3000, 0..8)
        }

        proptest! {
            /// Any bytes: refused or read, never a panic, never more than a head's worth held,
            /// and what follows an accepted request neither lost nor reordered.
            #[test]
            fn fuzz_any_bytes(sent in prop::collection::vec(any::<u8>(), 0..3 * MAX_HEAD), cuts in cuts()) {
                if let Ok((upgrade, rest)) = read_sent(&sent, &cuts) {
                    let head = sent.len() - upgrade.tail.len() - rest.len();
                    prop_assert!(sent[..head].windows(4).any(|word| word == b"GET "));
                    prop_assert!(head <= MAX_HEAD + 2048, "a head of {head} bytes");
                    prop_assert_eq!([&sent[..head], &upgrade.tail[..], &rest[..]].concat(), sent);
                }
            }

            /// A herder client's request with bytes of it changed: refused or read as above.
            #[test]
            fn fuzz_a_request_changed(
                edits in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..6),
                tail in prop::collection::vec(any::<u8>(), 0..512),
                cuts in cuts(),
            ) {
                let mut sent = request("/machines/h1", &[]);
                for (at, byte) in edits {
                    let at = at.index(sent.len());
                    sent[at] = byte;
                }
                sent.extend_from_slice(&tail);
                if let Ok((upgrade, rest)) = read_sent(&sent, &cuts) {
                    let head = sent.len() - upgrade.tail.len() - rest.len();
                    prop_assert_eq!([&sent[..head], &upgrade.tail[..], &rest[..]].concat(), sent);
                }
            }

            /// Any route, any extra headers, any first frames behind the request: the route's
            /// path read back as sent, and the frames kept whole and in order.
            #[test]
            fn fuzz_a_route_and_its_first_frames(
                id in ".{1,40}",
                stream in any::<bool>(),
                headers in prop::collection::vec(("[A-Za-z][A-Za-z0-9-]{0,20}", "[ -~]{0,100}"), 0..16),
                tail in prop::collection::vec(any::<u8>(), 0..4096),
                cuts in cuts(),
            ) {
                let route = match stream {
                    true => RelayRoute::Stream(id),
                    false => RelayRoute::Machine(herder_protocol::HostId::new(id)),
                };
                let mut sent = request(&route.path(), &headers);
                let head = sent.len();
                sent.extend_from_slice(&tail);
                let (upgrade, rest) = read_sent(&sent, &cuts).unwrap();
                prop_assert_eq!(RelayRoute::parse(upgrade.path()), Some(route));
                prop_assert_eq!([&upgrade.tail[..], &rest[..]].concat(), &sent[head..]);
            }

            /// Any path: a route or none, never a panic; a route reads back as its own path.
            #[test]
            fn fuzz_any_path(path in "(/(machines|streams|proxy)(/[^?]{0,30})?)?\\PC{0,40}") {
                if let Some(route) = RelayRoute::parse(&path) {
                    prop_assert_eq!(RelayRoute::parse(&route.path()), Some(route));
                }
            }
        }
    }
}
