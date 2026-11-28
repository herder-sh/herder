//! Reaching a machine through a hub this device is signed in to, when it cannot dial the
//! machine itself: away from the machine's network, with no VPN and no public address of the
//! machine's own.
//!
//! The client opens a connection to the hub as it always does, pinned to the hub's
//! certificate and with this device's key for the hub, and asks for a stream to the machine
//! by its upgrade request's path ([`herder_protocol::relay`]). It does not wait for the
//! answer: its TLS to the machine starts right behind the request, and the hub holds those
//! bytes until the machine dialed in, so the upgrade to the hub costs no round trip of its
//! own. The hub forwards the stream's bytes to the machine, which
//! keeps a connection to the hub. Inside the stream the client then runs its ordinary
//! connection to the machine: TLS pinning the machine's certificate and presenting this
//! device's key for it, the WebSocket and the hello. The hub relays only ciphertext, and
//! the machine knows the device as on a direct connection.

use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::task::{Context, Poll, ready};

use anyhow::Context as _;
use herder_protocol::{HostId, RelayError, RelayRoute};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;

use crate::address;
use crate::auth::{DeviceKey, client_config};
use crate::supervisor::{self, Supervisor, Watched};
use crate::tunnel::{self, Frames};
use crate::wireguard::Tunnels;

/// Most bytes of the hub's answer to an upgrade, refusal included.
const MAX_ANSWER: usize = 16 * 1024;

/// How a machine's addresses are reached: where its `hub://` address leads (the machine it
/// is of, and the client's machines, the hub among them), and the headers its tunnels want.
#[derive(Clone, Default)]
pub(crate) struct Via {
    /// The machine to reach; `None` for one that is being signed in to, which no hub
    /// relays to yet.
    machine: Option<HostId>,
    machines: Weak<Mutex<Vec<Arc<Supervisor>>>>,
    /// What each tunnel address of the machine sends on its upgrade.
    headers: tunnel::Headers,
    /// The client's WireGuard tunnels, for `wg://` addresses.
    tunnels: Option<Arc<Tunnels>>,
    /// The machine's tunnel addresses whose login proxy turned the upgrade away to its login.
    front_doors: FrontDoors,
}

/// A machine's tunnel addresses whose login proxy last turned the upgrade away to its login,
/// so the person signs in with a browser ([`crate::front_door`]).
pub(crate) type FrontDoors = Arc<std::sync::Mutex<std::collections::BTreeSet<String>>>;

/// A relay stream through a hub: the frames of a connection to it, whose upgrade may not be
/// answered yet.
pub(crate) type Stream = Frames<Upgrading<TlsStream<Watched>>>;

impl Via {
    /// The routes of `machine`, one of `machines`, whose tunnels want `headers`, noting in
    /// `front_doors` which ones' login proxy wants a sign-in.
    pub(crate) fn new(
        machine: HostId,
        machines: Weak<Mutex<Vec<Arc<Supervisor>>>>,
        headers: tunnel::Headers,
        tunnels: Option<Arc<Tunnels>>,
        front_doors: FrontDoors,
    ) -> Self {
        Self {
            machine: Some(machine),
            machines,
            headers,
            tunnels,
            front_doors,
        }
    }

    /// Notes whether the login proxy in front of `address` turned its upgrade away to its
    /// login, from how reaching it went.
    pub(crate) fn note<T>(&self, address: &str, reached: &anyhow::Result<T>) {
        if !address::is_tunnel(address) {
            return;
        }
        let wanted = reached.as_ref().err().is_some_and(|err| {
            err.chain()
                .any(|cause| cause.is::<crate::front_door::SignInWanted>())
        });
        // Every write is a single insert or remove, so a poisoned set is consistent.
        let mut doors = self
            .front_doors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if wanted {
            doors.insert(address.to_owned());
        } else if reached.is_ok() {
            doors.remove(address);
        }
    }

    /// The client's WireGuard tunnels; none while signing in.
    pub(crate) fn tunnels(&self) -> Option<&Tunnels> {
        self.tunnels.as_deref()
    }

    /// The machine whose addresses these are; none while signing in.
    pub(crate) fn machine(&self) -> Option<&HostId> {
        self.machine.as_ref()
    }

    /// What `address` sends on its upgrade, a tunnel's.
    pub(crate) fn headers(&self, address: &str) -> &tunnel::Sent {
        self.headers.get(address).unwrap_or(&tunnel::NONE)
    }

    /// A stream to the machine through `hub`, one of the client's machines: the hub is
    /// reached at its own addresses, raced as any machine's are.
    pub(crate) async fn stream(&self, hub: &HostId) -> anyhow::Result<Stream> {
        let host_id = self
            .machine
            .clone()
            .context("a machine is first signed in to at an address of its own")?;
        let hub_machine = self
            .machines
            .upgrade()
            .and_then(|machines| {
                // Every update leaves the list whole, so a poisoned list is consistent.
                let machines = machines.lock().unwrap_or_else(PoisonError::into_inner);
                machines
                    .iter()
                    .find(|machine| machine.saved.host_id == *hub)
                    .cloned()
            })
            .with_context(|| format!("this device is not signed in to the hub {hub}"))?;
        let saved = hub_machine.saved();
        let addresses: Vec<String> = saved
            .routes
            .dialled(&saved.addresses)
            .into_iter()
            // The machine has a route of its own through the hub's Cloudflare relay.
            .filter(|address| address::hub(address).is_none() && address::relay(address).is_none())
            .collect();
        let device = DeviceKey::from_pem(&saved.device_key)?;
        let connector = TlsConnector::from(Arc::new(client_config(&saved.fingerprint, &device)?));
        // The hub's own routes: its tunnels' headers, and this client's WireGuard tunnels.
        let via = Self {
            machine: Some(hub.clone()),
            machines: Weak::new(),
            headers: saved.headers,
            tunnels: self.tunnels.clone(),
            // The hub's tunnels' sign-ins are noted on the hub, where the apps show them.
            front_doors: hub_machine.front_doors_set(),
        };
        let reaching = |address: String| {
            let (connector, via) = (connector.clone(), via.clone());
            async move { supervisor::dial(&address, &via, &connector).await }
        };
        let (mut tls, _) = supervisor::race(&addresses, reaching)
            .await
            .map_err(|failed| {
                anyhow::Error::new(failed).context(format!("the hub {}", saved.name))
            })?;
        let key = generate_key();
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: herder\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
             Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\n\r\n",
            RelayRoute::Machine(host_id).path()
        );
        tls.write_all(request.as_bytes())
            .await
            .context("asking the hub")?;
        tls.flush().await.context("asking the hub")?;
        let upgrading = Upgrading {
            stream: tls,
            accept: derive_accept_key(key.as_bytes()),
            hub: saved.name,
            answer: Vec::new(),
            open: false,
        };
        // Each frame goes out as written: the TLS inside decides what to batch.
        let ws =
            WebSocketStream::from_raw_socket(upgrading, Role::Client, Some(tunnel::config())).await;
        Ok(Frames::new(ws))
    }
}

impl Via {
    /// A stream to the machine through the Cloudflare relay `address` goes through, with the
    /// token this device holds from the hub whose relay it is: the relay pairs it with the
    /// machine's own connection to it, and forwards ciphertext only.
    pub(crate) async fn relay_stream(&self, address: &str) -> anyhow::Result<Frames> {
        let host_id = self
            .machine
            .clone()
            .context("a machine is first signed in to at an address of its own")?;
        let relay = address::relay(address).context("not a route through a relay")?;
        let grant = self
            .machines
            .upgrade()
            .and_then(|machines| {
                // Every update leaves the list whole, so a poisoned list is consistent.
                let machines = machines.lock().unwrap_or_else(PoisonError::into_inner);
                machines.iter().find_map(|machine| {
                    machine.relay_grant().filter(|grant| {
                        address::through_relay(&grant.url).as_deref() == Some(address)
                    })
                })
            })
            .context(
                "no hub this device is signed in to gave it a token for this relay; it gets \
                 one once connected to its hub",
            )?;
        if grant.expires_at <= herder_protocol::Timestamp::now() {
            anyhow::bail!(
                "this device's token for the relay expired; it renews on the next connection \
                 to the hub"
            );
        }
        let mut headers = tunnel::Sent::new();
        headers.insert(
            "authorization".to_owned(),
            format!("Bearer {}", grant.token),
        );
        tunnel::open(&format!("{relay}/machines/{host_id}"), &headers)
            .await
            .context("the Cloudflare relay")
    }
}

/// A connection to the hub whose upgrade request is sent and whose answer is read before
/// anything else: writes go out at once, behind the request, and the first read takes the
/// answer. A `101` opens the stream; a refusal fails the read with the hub's reason.
pub(crate) struct Upgrading<S> {
    stream: S,
    /// The `Sec-WebSocket-Accept` the answer must carry.
    accept: String,
    /// The hub's name, for a refusal.
    hub: String,
    /// The answer as read so far; once the stream is open, what came behind it, not read
    /// yet.
    answer: Vec<u8>,
    open: bool,
}

impl<S> Upgrading<S> {
    /// The length of the answer's head once it is a `101`; `None` until enough of the answer
    /// is read, or, at `eof`, the refusal or failure it is.
    fn answered(&self, eof: bool) -> io::Result<Option<usize>> {
        let mut headers = [httparse::EMPTY_HEADER; 32];
        let mut response = httparse::Response::new(&mut headers);
        let bad = |message: String| io::Error::new(io::ErrorKind::InvalidData, message);
        let head = match response.parse(&self.answer) {
            Ok(httparse::Status::Complete(head)) => head,
            Ok(httparse::Status::Partial) if !eof => return Ok(None),
            Ok(httparse::Status::Partial) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!(
                        "the hub {} closed the connection without an answer",
                        self.hub
                    ),
                ));
            }
            Err(err) => return Err(bad(format!("the hub's answer: {err}"))),
        };
        let header = |name: &str| {
            response
                .headers
                .iter()
                .find(|header| header.name.eq_ignore_ascii_case(name))
                .and_then(|header| std::str::from_utf8(header.value).ok())
        };
        let status = response.code.unwrap_or_default();
        if status == 101 {
            if header("sec-websocket-accept").map(str::trim) != Some(self.accept.as_str()) {
                return Err(bad(
                    "the hub's answer does not accept this upgrade".to_owned()
                ));
            }
            return Ok(Some(head));
        }
        let length = header("content-length")
            .and_then(|length| length.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let body = &self.answer[head..];
        if body.len() < length && !eof {
            return Ok(None);
        }
        let reason = serde_json::from_slice::<RelayError>(&body[..body.len().min(length)])
            .map(|error| error.message)
            .unwrap_or_else(|_| format!("{status} {}", response.reason.unwrap_or_default()));
        Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            format!("the hub {} refused: {reason}", self.hub),
        ))
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Upgrading<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        while !this.open {
            let eof = {
                let mut chunk = [0; 2048];
                let mut read = ReadBuf::new(&mut chunk);
                ready!(Pin::new(&mut this.stream).poll_read(cx, &mut read))?;
                this.answer.extend_from_slice(read.filled());
                read.filled().is_empty()
            };
            if let Some(head) = this.answered(eof)? {
                this.answer.drain(..head);
                this.open = true;
            } else if this.answer.len() > MAX_ANSWER {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("the hub's answer is over {MAX_ANSWER} bytes"),
                )));
            }
        }
        if this.answer.is_empty() {
            return Pin::new(&mut this.stream).poll_read(cx, buf);
        }
        let n = this.answer.len().min(buf.remaining());
        buf.put_slice(&this.answer[..n]);
        this.answer.drain(..n);
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Upgrading<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    fn asked(stream: tokio::io::DuplexStream) -> Upgrading<tokio::io::DuplexStream> {
        Upgrading {
            stream,
            accept: derive_accept_key(b"dGhlIHNhbXBsZSBub25jZQ=="),
            hub: "home".into(),
            answer: Vec::new(),
            open: false,
        }
    }

    #[tokio::test]
    async fn what_comes_behind_a_101_is_the_stream() {
        let (mut hub, client) = tokio::io::duplex(4096);
        let mut upgrading = asked(client);
        // Writes go out before any answer.
        upgrading.write_all(b"client hello").await.unwrap();
        let mut sent = [0; 12];
        hub.read_exact(&mut sent).await.unwrap();
        assert_eq!(&sent, b"client hello");
        let answer = "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
            Connection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\nserver";
        for piece in answer.as_bytes().chunks(5) {
            hub.write_all(piece).await.unwrap();
        }
        hub.write_all(b" hello").await.unwrap();
        drop(hub);
        let mut read = Vec::new();
        upgrading.read_to_end(&mut read).await.unwrap();
        assert_eq!(read, b"server hello");
    }

    #[tokio::test]
    async fn a_refusal_fails_the_read_with_the_hubs_reason() {
        let (mut hub, client) = tokio::io::duplex(4096);
        let mut upgrading = asked(client);
        let body = r#"{"code":"host_offline","message":"the host h is not connected"}"#;
        let answer = format!(
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        hub.write_all(answer.as_bytes()).await.unwrap();
        let err = upgrading.read(&mut [0; 16]).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::ConnectionRefused);
        assert_eq!(
            err.to_string(),
            "the hub home refused: the host h is not connected"
        );

        // Something in front of the hub refused, with no reason of the hub's.
        let (mut hub, client) = tokio::io::duplex(4096);
        let mut upgrading = asked(client);
        hub.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
            .await
            .unwrap();
        drop(hub);
        let err = upgrading.read(&mut [0; 16]).await.unwrap_err();
        assert_eq!(err.to_string(), "the hub home refused: 502 Bad Gateway");
    }

    #[tokio::test]
    async fn an_answer_for_another_upgrade_is_refused() {
        let (mut hub, client) = tokio::io::duplex(4096);
        let mut upgrading = asked(client);
        hub.write_all(b"HTTP/1.1 101 Switching Protocols\r\nSec-WebSocket-Accept: x\r\n\r\n")
            .await
            .unwrap();
        let err = upgrading.read(&mut [0; 16]).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
