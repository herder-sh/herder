//! Relay streams: the hub forwards bytes between a client and a host that both reach it,
//! so the client reaches a host it cannot dial ([`herder_protocol::relay`]).
//!
//! A client device asks for a stream to a host by its upgrade request's path, and may send
//! the stream's first frames behind it. The hub asks the host, on its replication
//! connection, to open its end, waits for that connection holding the client's frames
//! unread, answers both upgrades, then copies every binary frame's bytes from each to the
//! other until one of them closes. It reads nothing of them: inside,
//! the client and the host run the TLS of a direct connection, which the hub has no key
//! to.
//!
//! A host reaches the hub's CLIProxyAPI the same way, where the hub names no address for
//! it: on such a stream the hub serves the HTTP the host's CLIs speak, forwarding each
//! request to the proxy beside it, so the proxy is on no network but the hub's own.
//!
//! [`Relays`] holds what connects the three connections involved: which hosts take relayed
//! connections, on which channel their replication connection is told to open one, the
//! streams waiting for their host, and how many streams each device and host has open.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use herder_protocol::{DeviceId, HostId, RelayError, RelayErrorCode, RelayRoute};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::header::{CONNECTION, HOST, HeaderName};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::conn::Upgrade;
use super::{Shared, blocking, cliproxyapi};
use crate::auth::DeviceRole;
use crate::ws;
use crate::ws::Transport;
use crate::ws::tunnel::{Frames, Pass};

/// How long a host that is not connected gets to connect before its client is refused: after
/// a hub restart both reconnect, and the client is usually first.
const HOST_WAIT: Duration = Duration::from_secs(3);

/// How long a host gets to open its end of a stream.
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(10);

/// Streams one device may have open: a connection to each of its machines, with room for the
/// attempts that race while it reconnects.
pub(super) const STREAMS_PER_DEVICE: usize = 32;

/// Streams to one host: every device of a few people.
pub(super) const STREAMS_PER_HOST: usize = 64;

/// Streams one host may have open to the hub's CLIProxyAPI: one for each request its CLIs
/// have under way.
pub(super) const PROXY_STREAMS_PER_HOST: usize = 64;

/// The HTTP client the hub forwards hosts' requests to its CLIProxyAPI with.
type ProxyClient = Client<HttpsConnector<HttpConnector>, Incoming>;

/// The body of an answer to a host: the proxy's, or the hub's own refusal.
type ProxyBody = BoxBody<Bytes, hyper::Error>;

/// How often each end of a stream is pinged, and the silence after which it is taken for
/// gone. A peer behind a tunnel is not the socket's peer, so the kernel cannot tell.
const PING_EVERY: Duration = Duration::from_secs(30);
const SILENCE: Duration = Duration::from_secs(90);

/// The receive buffer of the hub's socket from a host on a relay stream: how far the hub
/// reads ahead of what it passed on to the client. Left to itself, the kernel grows it to
/// megabytes, and a catch-up the client's slower link cannot take yet waits in it, where the
/// host can put nothing ahead of it: an answer the client waits on queues behind all of it.
/// This little keeps the backlog on the host, which sends what the client waits on first.
/// It caps a stream at about this per round trip between the host and the hub (Linux doubles
/// it for its own use): some 2 MB/s at 30 ms.
const HOST_READ_AHEAD: usize = 64 * 1024;

/// Time to deliver an answer or a close frame before the socket is dropped.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

/// A stream a host is asked to open its end of.
#[derive(Debug)]
pub(super) struct Open {
    /// The stream, as the host's accept names it.
    pub(super) stream_id: String,
    /// The hub's id of the device that asked.
    pub(super) device_id: DeviceId,
}

/// The host's end of a stream: its connection, its upgrade not answered yet, and what
/// cancels it.
type End = (Upgrade, CancellationToken);

/// What the hub's connections share about relay streams.
pub(super) struct Relays {
    state: Mutex<State>,
    /// Notified when a host that takes relayed connections connects.
    host_connected: Notify,
    host_wait: Duration,
    accept_timeout: Duration,
    per_device: usize,
    per_host: usize,
    /// Made the first time a host asks for the CLIProxyAPI.
    proxy_client: OnceLock<ProxyClient>,
}

#[derive(Default)]
struct State {
    /// Each connected host that takes relayed connections: its registration, and where its
    /// replication connection takes the streams to open.
    hosts: HashMap<HostId, (u64, mpsc::UnboundedSender<Open>)>,
    registrations: u64,
    /// Streams whose host has not opened its end yet, by stream id.
    waiting: HashMap<String, (HostId, oneshot::Sender<End>)>,
    by_device: HashMap<DeviceId, usize>,
    by_host: HashMap<HostId, usize>,
    /// Streams to the CLIProxyAPI each host has open.
    proxied: HashMap<HostId, usize>,
}

impl Default for Relays {
    fn default() -> Self {
        Self::new(
            HOST_WAIT,
            ACCEPT_TIMEOUT,
            STREAMS_PER_DEVICE,
            STREAMS_PER_HOST,
        )
    }
}

impl Relays {
    pub(super) fn new(
        host_wait: Duration,
        accept_timeout: Duration,
        per_device: usize,
        per_host: usize,
    ) -> Self {
        Self {
            state: Mutex::default(),
            host_connected: Notify::new(),
            host_wait,
            accept_timeout,
            per_device,
            per_host,
            proxy_client: OnceLock::new(),
        }
    }

    /// The client requests are forwarded to the CLIProxyAPI with. It checks a proxy that
    /// serves TLS against the web's CAs, as the hub's management client does.
    fn proxy_client(&self) -> Result<ProxyClient> {
        if let Some(client) = self.proxy_client.get() {
            return Ok(client.clone());
        }
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let provider = rustls::crypto::ring::default_provider();
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth();
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_or_http()
            .enable_http1()
            .build();
        let client = Client::builder(TokioExecutor::new()).build(https);
        Ok(self.proxy_client.get_or_init(|| client).clone())
    }

    /// Room for one more stream from `host` to the CLIProxyAPI, until the slot is dropped.
    fn reserve_proxy(self: &Arc<Self>, host: &HostId) -> Option<ProxySlot> {
        let mut state = self.lock();
        let open = state.proxied.get(host).copied().unwrap_or(0);
        if open >= PROXY_STREAMS_PER_HOST {
            return None;
        }
        state.proxied.insert(host.clone(), open + 1);
        Some(ProxySlot {
            relays: Arc::clone(self),
            host: host.clone(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // Every update leaves the maps consistent, so a poisoned state is too.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Notes that `host`, which takes relayed connections, is connected: until the returned
    /// end is dropped, its streams to open come out of it.
    pub(super) fn host_connected(self: &Arc<Self>, host: &HostId) -> HostEnd {
        let (sender, opens) = mpsc::unbounded_channel();
        let registration = {
            let mut state = self.lock();
            state.registrations += 1;
            let registration = state.registrations;
            // A host that reconnects replaces its old connection's registration.
            state.hosts.insert(host.clone(), (registration, sender));
            registration
        };
        self.host_connected.notify_waiters();
        HostEnd {
            relays: Arc::clone(self),
            host: host.clone(),
            registration,
            opens,
        }
    }

    /// Whether `host` is connected and takes relayed connections.
    pub(super) fn takes(&self, host: &HostId) -> bool {
        self.lock().hosts.contains_key(host)
    }

    /// Room for one more stream from `device` to `host`, until the slot is dropped; `None`
    /// when either has as many as it may.
    fn reserve(self: &Arc<Self>, device: &DeviceId, host: &HostId) -> Option<Slot> {
        let mut state = self.lock();
        let from_device = state.by_device.get(device).copied().unwrap_or(0);
        let to_host = state.by_host.get(host).copied().unwrap_or(0);
        if from_device >= self.per_device || to_host >= self.per_host {
            return None;
        }
        state.by_device.insert(device.clone(), from_device + 1);
        state.by_host.insert(host.clone(), to_host + 1);
        Some(Slot {
            relays: Arc::clone(self),
            device: device.clone(),
            host: host.clone(),
        })
    }

    /// Asks `host` to open its end of a new stream for `device`, and waits for it: the
    /// host's end, or `None` when the host is not connected or did not open one in time.
    async fn open(self: &Arc<Self>, device: &DeviceId, host: &HostId) -> Option<End> {
        let deadline = Instant::now() + self.host_wait;
        let stream_id = ulid::Ulid::generate().to_string();
        let (sender, end) = oneshot::channel();
        let mut sender = Some(sender);
        // Removes the stream from those waiting however this ends.
        let _waiting = Waiting {
            relays: Arc::clone(self),
            stream_id: stream_id.clone(),
        };
        loop {
            // Before the check, so a host that connects right after it is not missed.
            let connected = self.host_connected.notified();
            tokio::pin!(connected);
            connected.as_mut().enable();
            {
                let mut state = self.lock();
                if let Some((_, opens)) = state.hosts.get(host) {
                    let open = Open {
                        stream_id: stream_id.clone(),
                        device_id: device.clone(),
                    };
                    // A closed channel is a host whose connection just ended: wait for the
                    // next one.
                    if opens.send(open).is_ok()
                        && let Some(sender) = sender.take()
                    {
                        state
                            .waiting
                            .insert(stream_id.clone(), (host.clone(), sender));
                        break;
                    }
                }
            }
            tokio::time::timeout_at(deadline, connected).await.ok()?;
        }
        tokio::time::timeout(self.accept_timeout, end)
            .await
            .ok()?
            .ok()
    }

    /// Where the end `host` opened for `stream_id` goes; `None` when no client waits for it
    /// from that host any more.
    fn accept(&self, stream_id: &str, host: &HostId) -> Option<oneshot::Sender<End>> {
        let mut state = self.lock();
        let (waited, _) = state.waiting.get(stream_id)?;
        if waited != host {
            return None;
        }
        state.waiting.remove(stream_id).map(|(_, sender)| sender)
    }
}

/// A connected host's registration with [`Relays`], for its replication connection.
pub(super) struct HostEnd {
    relays: Arc<Relays>,
    host: HostId,
    registration: u64,
    opens: mpsc::UnboundedReceiver<Open>,
}

impl HostEnd {
    /// The next stream the host is to open its end of.
    pub(super) async fn next(&mut self) -> Open {
        match self.opens.recv().await {
            Some(open) => open,
            // A newer connection of the host took the registration over.
            None => std::future::pending().await,
        }
    }
}

impl Drop for HostEnd {
    fn drop(&mut self) {
        let mut state = self.relays.lock();
        if state
            .hosts
            .get(&self.host)
            .is_some_and(|(registration, _)| *registration == self.registration)
        {
            state.hosts.remove(&self.host);
        }
    }
}

/// One open or opening stream, counted against its device and host until dropped.
struct Slot {
    relays: Arc<Relays>,
    device: DeviceId,
    host: HostId,
}

impl Drop for Slot {
    fn drop(&mut self) {
        fn release<K: std::hash::Hash + Eq>(counts: &mut HashMap<K, usize>, key: &K) {
            if let Some(count) = counts.get_mut(key) {
                *count -= 1;
                if *count == 0 {
                    counts.remove(key);
                }
            }
        }
        let mut state = self.relays.lock();
        release(&mut state.by_device, &self.device);
        release(&mut state.by_host, &self.host);
    }
}

/// One stream of a host to the CLIProxyAPI, counted until dropped.
struct ProxySlot {
    relays: Arc<Relays>,
    host: HostId,
}

impl Drop for ProxySlot {
    fn drop(&mut self) {
        let mut state = self.relays.lock();
        if let Some(open) = state.proxied.get_mut(&self.host) {
            *open -= 1;
            if *open == 0 {
                state.proxied.remove(&self.host);
            }
        }
    }
}

struct Waiting {
    relays: Arc<Relays>,
    stream_id: String,
}

impl Drop for Waiting {
    fn drop(&mut self) {
        self.relays.lock().waiting.remove(&self.stream_id);
    }
}

/// Serves a relay connection whose upgrade request names `route`, from the device with
/// certificate `device`; `pass` counts it until it authenticated.
pub(super) async fn serve(
    upgrade: Upgrade,
    route: RelayRoute,
    shared: &Shared,
    device: &str,
    pass: Pass,
    peer: SocketAddr,
    cancel: CancellationToken,
) {
    let result = match route {
        RelayRoute::Machine(host_id) => {
            connect(upgrade, shared, device, pass, &host_id, peer, cancel).await
        }
        RelayRoute::Stream(stream_id) => {
            accept(upgrade, shared, device, pass, &stream_id, peer, cancel).await
        }
        RelayRoute::Proxy => proxy(upgrade, shared, device, pass, peer, cancel).await,
    };
    if let Err(err) = result {
        info!(%peer, "relay connection failed: {err:#}");
    }
}

/// A client's end: checks that the hub forwards for it to `host`, has the host open its
/// end, then forwards until either end closes.
async fn connect(
    upgrade: Upgrade,
    shared: &Shared,
    device: &str,
    pass: Pass,
    host: &HostId,
    peer: SocketAddr,
    cancel: CancellationToken,
) -> Result<()> {
    // From the client's request until both ends are answered: what relaying adds to a
    // connection's start.
    let opening = tracing::info_span!("hub.relay", outcome = tracing::field::Empty);
    let identity =
        match shared
            .auth
            .authenticate(device, None, "relay", DeviceRole::Client, &cancel)
        {
            Ok(identity) => identity,
            Err(error) => {
                let error = refusal(RelayErrorCode::Forbidden, error.message);
                refuse(upgrade, peer, error).await;
                return Ok(());
            }
        };
    drop(pass);
    let device = identity.device_id;
    let relays = &shared.relays;
    let known = blocking(&shared.store, |store| store.hosts()).await?;
    let presence = shared.fleet.presence();
    let error = if !known.iter().any(|known| known.host_id == *host) {
        Some(refusal(
            RelayErrorCode::UnknownHost,
            format!("no host {host} replicates to this hub"),
        ))
    } else if presence.online(host) && !relays.takes(host) {
        Some(no_relay(host))
    } else {
        None
    };
    if let Some(error) = error {
        refuse(upgrade, peer, error).await;
        return Ok(());
    }
    let Some(_slot) = relays.reserve(&device, host) else {
        let message = format!(
            "too many relay streams are open: at most {} per device and {} per host",
            relays.per_device, relays.per_host
        );
        refuse(upgrade, peer, refusal(RelayErrorCode::Busy, message)).await;
        return Ok(());
    };
    let opened = tokio::select! {
        () = cancel.cancelled() => return Ok(()),
        opened = relays.open(&device, host) => opened,
    };
    let Some((host_end, host_cancel)) = opened else {
        let error = if presence.online(host) && !relays.takes(host) {
            no_relay(host)
        } else {
            refusal(
                RelayErrorCode::HostOffline,
                format!("the host {host} is not connected to this hub, or did not answer"),
            )
        };
        refuse(upgrade, peer, error).await;
        return Ok(());
    };
    let host_ws = host_end.accept(ws::ws_config()).await?;
    // The client's frames that waited behind its request go first.
    let ws = upgrade.accept(ws::ws_config()).await?;
    opening.record("outcome", "ok");
    drop(opening);
    info!(%peer, device_id = %device, host_id = %host, "relay stream opened");
    let started = Instant::now();
    let ended = tokio::select! {
        () = cancel.cancelled() => "the device was revoked, or the hub is shutting down".to_owned(),
        () = host_cancel.cancelled() => "the host was revoked".to_owned(),
        ended = splice(ws, host_ws, PING_EVERY, SILENCE) => ended,
    };
    info!(
        %peer,
        device_id = %device,
        host_id = %host,
        "relay stream closed after {:.1?}: {ended}",
        started.elapsed()
    );
    Ok(())
}

/// A host's end: hands the connection to the client waiting for `stream_id`, which forwards.
async fn accept(
    upgrade: Upgrade,
    shared: &Shared,
    device: &str,
    pass: Pass,
    stream_id: &str,
    peer: SocketAddr,
    cancel: CancellationToken,
) -> Result<()> {
    let identity = match shared
        .auth
        .authenticate(device, None, "relay", DeviceRole::Host, &cancel)
    {
        Ok(identity) => identity,
        Err(error) => {
            let error = refusal(RelayErrorCode::Forbidden, error.message);
            refuse(upgrade, peer, error).await;
            return Ok(());
        }
    };
    drop(pass);
    let device = identity.device_id;
    let host = blocking(&shared.store, move |store| store.host_of_device(&device)).await?;
    let Some(host) = host else {
        let message = "this device replicates to this hub as no host".to_owned();
        refuse(upgrade, peer, refusal(RelayErrorCode::Forbidden, message)).await;
        return Ok(());
    };
    let gone = || {
        refusal(
            RelayErrorCode::BadRequest,
            format!("no client waits for the stream {stream_id}"),
        )
    };
    // A tunneled host's bytes come through `cloudflared`, whose buffers the hub cannot set.
    if let Transport::Tcp(tcp) = upgrade.stream().get_ref().0.transport()
        && let Err(err) = socket2::SockRef::from(tcp).set_recv_buffer_size(HOST_READ_AHEAD)
    {
        debug!(%peer, "cannot limit the socket's receive buffer: {err}");
    }
    let Some(waiting) = shared.relays.accept(stream_id, &host) else {
        refuse(upgrade, peer, gone()).await;
        return Ok(());
    };
    // The client's task answers and forwards from here; it gave up if it is gone.
    if let Err((upgrade, _)) = waiting.send((upgrade, cancel)) {
        refuse(upgrade, peer, gone()).await;
    }
    Ok(())
}

/// A host's stream to the hub's CLIProxyAPI: serves the HTTP the host's CLIs speak on it,
/// forwarding each request to the proxy, until the host closes it.
async fn proxy(
    upgrade: Upgrade,
    shared: &Shared,
    device: &str,
    pass: Pass,
    peer: SocketAddr,
    cancel: CancellationToken,
) -> Result<()> {
    let identity = match shared
        .auth
        .authenticate(device, None, "relay", DeviceRole::Host, &cancel)
    {
        Ok(identity) => identity,
        Err(error) => {
            let error = refusal(RelayErrorCode::Forbidden, error.message);
            refuse(upgrade, peer, error).await;
            return Ok(());
        }
    };
    drop(pass);
    let device = identity.device_id;
    let host = blocking(&shared.store, move |store| store.host_of_device(&device)).await?;
    // Only a host the hub's hello told to come this way: one that holds a client key of a
    // proxy the hub names no address for.
    let forwarder = host.as_ref().and_then(|host| {
        let keys = shared.cliproxyapi.get()?;
        (keys.endpoint().is_none() && cliproxyapi::set_name(host).is_some())
            .then(|| keys.management().url().to_owned())
    });
    let (Some(host), Some(upstream)) = (host, forwarder) else {
        let message = "this hub forwards this device to no CLIProxyAPI".to_owned();
        refuse(upgrade, peer, refusal(RelayErrorCode::Forbidden, message)).await;
        return Ok(());
    };
    let Some(_slot) = shared.relays.reserve_proxy(&host) else {
        let message = format!(
            "too many streams to the CLIProxyAPI are open: at most {PROXY_STREAMS_PER_HOST} \
             per host"
        );
        refuse(upgrade, peer, refusal(RelayErrorCode::Busy, message)).await;
        return Ok(());
    };
    let client = match shared.relays.proxy_client() {
        Ok(client) => client,
        Err(err) => {
            let message = format!("the hub cannot reach its CLIProxyAPI: {err:#}");
            refuse(upgrade, peer, refusal(RelayErrorCode::Internal, message)).await;
            return Ok(());
        }
    };
    let ws = upgrade.accept(ws::ws_config()).await?;
    let service = service_fn(move |request| forward(client.clone(), upstream.clone(), request));
    let serving = http1::Builder::new().serve_connection(TokioIo::new(Frames::new(ws)), service);
    tokio::select! {
        () = cancel.cancelled() => {}
        served = serving => {
            if let Err(err) = served {
                debug!(%peer, host_id = %host, "a stream to the CLIProxyAPI failed: {err}");
            }
        }
    }
    Ok(())
}

/// Whether the hub forwards a host's request for `path`: anything but the proxy's
/// management API, which stays the hub's alone, as it is while hosts dial the proxy and
/// the proxy takes management requests from its own machine only. A path with an escape is
/// refused too, as it could spell the same.
fn forwardable(path: &str) -> bool {
    !path.contains('%') && !path.to_ascii_lowercase().contains("management")
}

/// Forwards `request` to the CLIProxyAPI at `upstream` as it is, the host's client key
/// included, and streams the answer back.
async fn forward(
    client: ProxyClient,
    upstream: String,
    request: Request<Incoming>,
) -> Result<Response<ProxyBody>, hyper_util::client::legacy::Error> {
    let (mut parts, body) = request.into_parts();
    if !forwardable(parts.uri.path()) {
        let refused = Full::new(Bytes::from_static(b"herder: not forwarded"))
            .map_err(|never| match never {})
            .boxed();
        let mut response = Response::new(refused);
        *response.status_mut() = StatusCode::FORBIDDEN;
        return Ok(response);
    }
    let path = parts.uri.path_and_query().map_or("/", |path| path.as_str());
    // A base URL and a path a client sent always make a URL; the client refuses one that
    // does not.
    parts.uri = format!("{upstream}{path}").parse().unwrap_or_default();
    for name in [HOST, CONNECTION] {
        parts.headers.remove(name);
    }
    parts.headers.remove(HeaderName::from_static("keep-alive"));
    match client.request(Request::from_parts(parts, body)).await {
        Ok(mut response) => {
            response.headers_mut().remove(CONNECTION);
            response
                .headers_mut()
                .remove(HeaderName::from_static("keep-alive"));
            Ok(response.map(BodyExt::boxed))
        }
        Err(err) => {
            warn!("cannot reach the CLIProxyAPI at {upstream}: {err}");
            Err(err)
        }
    }
}

fn refusal(code: RelayErrorCode, message: String) -> RelayError {
    RelayError { code, message }
}

fn no_relay(host: &HostId) -> RelayError {
    refusal(
        RelayErrorCode::UnknownHost,
        format!("the host {host} takes no relayed connections"),
    )
}

/// Tells the peer why it gets no stream, answering its upgrade, and closes the connection.
async fn refuse(upgrade: Upgrade, peer: SocketAddr, error: RelayError) {
    debug!(%peer, code = ?error.code, "relay refused: {}", error.message);
    let refused = upgrade.refuse(error.code.status(), Some(&error));
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, refused).await;
}

/// Copies binary frames between `a` and `b` until either closes, fails or is silent for
/// `silence`, pinging each every `ping`; closes both and returns why it ended.
pub(super) async fn splice<A, B>(a: A, b: B, ping: Duration, silence: Duration) -> String
where
    A: Stream<Item = tungstenite::Result<Message>> + Sink<Message, Error = tungstenite::Error>,
    B: Stream<Item = tungstenite::Result<Message>> + Sink<Message, Error = tungstenite::Error>,
{
    let (mut a_sink, a_stream) = a.split();
    let (mut b_sink, b_stream) = b.split();
    // Each direction on its own, so one that is held up never holds up the other.
    let ended = tokio::select! {
        ended = pump(a_stream, &mut b_sink, ping, silence) => ended,
        ended = pump(b_stream, &mut a_sink, ping, silence) => ended,
    };
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, async {
        let _ = a_sink.close().await;
        let _ = b_sink.close().await;
    })
    .await;
    match ended {
        Ok(()) => "one end closed it".to_owned(),
        Err(err) => format!("{err:#}"),
    }
}

/// Copies binary frames from `from` to `to` until `from` closes.
async fn pump(
    mut from: impl Stream<Item = tungstenite::Result<Message>> + Unpin,
    to: &mut (impl Sink<Message, Error = tungstenite::Error> + Unpin),
    ping: Duration,
    silence: Duration,
) -> Result<()> {
    let mut pings = tokio::time::interval_at(Instant::now() + ping, ping);
    let mut heard = Instant::now();
    loop {
        tokio::select! {
            frame = from.next() => {
                heard = Instant::now();
                match frame {
                    // The bytes go on as they came: nothing is decoded or copied.
                    Some(Ok(Message::Binary(data))) => to.send(Message::Binary(data)).await?,
                    // Pings are answered by the WebSocket itself; a pong is a sign of life.
                    Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
                    Some(Ok(Message::Text(_))) => bail!("a stream carries binary frames only"),
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Err(err)) => return Err(anyhow!(err)),
                }
            }
            _ = pings.tick() => {
                if heard.elapsed() > silence {
                    bail!("one end was silent for {silence:?}");
                }
                // To the other end, whose pong the other direction hears.
                to.send(Message::Ping(Default::default())).await?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relays(per_device: usize, per_host: usize) -> Arc<Relays> {
        Arc::new(Relays::new(
            Duration::from_millis(50),
            Duration::from_millis(50),
            per_device,
            per_host,
        ))
    }

    #[test]
    fn the_proxys_management_api_is_never_forwarded() {
        for path in [
            "/v1/messages",
            "/v1/messages/count_tokens",
            "/v1/responses",
            "/v1beta/models/gemini-3-pro:streamGenerateContent",
            "/",
        ] {
            assert!(forwardable(path), "{path}");
        }
        for path in [
            "/v8/management/config/access/api-keys",
            "/v8/Management/credentials",
            "//v8/management",
            "/v8/%6danagement/credentials",
            "/v1/../v8/management",
        ] {
            assert!(!forwardable(path), "{path}");
        }
    }

    #[test]
    fn streams_are_capped_per_device_and_per_host_and_a_dropped_slot_frees_its_place() {
        let relays = relays(2, 3);
        let (a, b, c) = (DeviceId::new("a"), DeviceId::new("b"), DeviceId::new("c"));
        let (h1, h2) = (HostId::new("h1"), HostId::new("h2"));
        let first = relays.reserve(&a, &h1).unwrap();
        let _second = relays.reserve(&a, &h2).unwrap();
        assert!(relays.reserve(&a, &h1).is_none(), "a third from one device");
        let _third = relays.reserve(&b, &h1).unwrap();
        let _fourth = relays.reserve(&c, &h1).unwrap();
        assert!(relays.reserve(&b, &h1).is_none(), "a fourth to one host");
        drop(first);
        relays.reserve(&b, &h1).unwrap();
        let state = relays.lock();
        assert_eq!(state.by_device.get(&b), Some(&1));
        assert_eq!(state.by_host.get(&h1), Some(&2));
    }

    #[tokio::test]
    async fn a_stream_waits_for_its_host_to_connect_and_only_that_host_may_open_it() {
        let relays = relays(8, 8);
        let (device, host) = (DeviceId::new("d"), HostId::new("h"));
        // No host: given up on after the wait, leaving nothing behind.
        assert!(relays.open(&device, &host).await.is_none());
        assert!(relays.lock().waiting.is_empty());

        let opening = tokio::spawn({
            let (relays, device, host) = (Arc::clone(&relays), device.clone(), host.clone());
            async move { relays.open(&device, &host).await.is_some() }
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        let mut end = relays.host_connected(&host);
        assert!(relays.takes(&host));
        let open = end.next().await;
        assert_eq!(open.device_id, device);
        assert!(
            relays
                .accept(&open.stream_id, &HostId::new("other"))
                .is_none()
        );
        assert!(relays.accept("another stream", &host).is_none());
        let waiting = relays.accept(&open.stream_id, &host).unwrap();
        assert!(
            relays.accept(&open.stream_id, &host).is_none(),
            "taken once"
        );
        // The host never sends its end: the client gives up, and the sender finds it gone.
        assert!(!opening.await.unwrap());
        assert!(waiting.is_closed());

        // A newer connection of the host takes over; the older one's end leaves it alone.
        let newer = relays.host_connected(&host);
        drop(end);
        assert!(relays.takes(&host));
        drop(newer);
        assert!(!relays.takes(&host));
    }
}
