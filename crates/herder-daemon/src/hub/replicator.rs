//! The host side of replication: one background task streaming every session's journal to
//! the hub.
//!
//! It connects as a paired device, takes the hub's cursors from its hello, sends each
//! session's summary and the events after its cursor, then follows the journal as it grows.
//! It reads the journal on its own and learns of new events through [`WakeOnEvent`], which
//! only wakes it, so a slow or unreachable hub never holds up a session. When the hub is
//! unreachable it retries with a backoff of up to [`BACKOFF_CAP`]. When the host backs up
//! images (`[hub] attachments`), every image a batch's prompts carried goes just ahead of
//! the batch; otherwise none does. While connected, it carries the host's secret requests
//! too ([`HostSecrets`]), and, unless `[hub] relay` is off, opens this host's end of every
//! relay stream the hub asks for, so clients reach the host through the hub
//! (`docs/hub-relay.md`).
//!
//! The `[hub]` table's switches say which of the two it does: with `backup` off it sends the
//! hub nothing of the journal, and with `secrets` off it carries no secret requests.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use herder_client_core::auth::{DeviceKey, client_config};
use herder_protocol::{
    Account, Attachment, AttachmentData, Batch, Cursor, DeviceId, Event, FetchedSecret, HostHello,
    HostMessage, HostSecretFetch, HubMessage, Item, ItemId, JournalRecord, MAX_BATCH_EVENTS,
    REPLICATION_VERSION, RejectReason, RelayGrant, SecretError, SecretFetch, SecretInfo, Seq,
    SessionHead, SessionId, SessionStatus, SessionSummary,
};
use tokio::sync::{Notify, oneshot};
use tokio::time::Instant;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::client::{self, Ws};
use super::host_secrets::{Ask, HostSecrets};
use super::pin;
use super::store::SecretUse;
use super::{BUILD, cloudflare_relay};
use crate::config::HubConfig;
use crate::data_dir::write_private;
use crate::session::{EventSink, SessionManager};
use crate::ws::{Host, Relayed};

/// File in the data dir holding the key this host pairs with the hub as.
const DEVICE_FILE: &str = "hub-device.pem";

/// Time to connect and get the hub's hello.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// First wait before reconnecting; it doubles on every failed attempt up to [`BACKOFF_CAP`].
const BACKOFF_BASE: Duration = Duration::from_secs(1);

/// Longest wait between attempts to reach the hub.
pub const BACKOFF_CAP: Duration = Duration::from_secs(60);

/// How often an idle connection is pinged.
const KEEPALIVE: Duration = Duration::from_secs(20);

/// Silence after which the hub is taken for gone and the connection dropped.
const SILENCE: Duration = Duration::from_secs(60);

/// Events of one session sent but not yet acknowledged, at most.
const WINDOW: Seq = 4 * MAX_BATCH_EVENTS as Seq;

/// Passes everything on to `next`, and wakes the replicator on every durable event.
pub struct WakeOnEvent {
    /// Where everything goes on to.
    pub next: Arc<dyn EventSink>,
    /// Notified on every event.
    pub notify: Arc<Notify>,
}

impl EventSink for WakeOnEvent {
    fn event(&self, event: &Event) {
        self.next.event(event);
        self.notify.notify_one();
    }

    fn snapshot(&self, session_id: &SessionId, item: &Item) {
        self.next.snapshot(session_id, item);
    }

    fn delta(&self, session_id: &SessionId, item_id: &ItemId, text: &str) {
        self.next.delta(session_id, item_id, text);
    }

    fn sessions_changed(&self, sessions: &[SessionHead]) {
        // A project resolved by discovery changes summaries without an event.
        self.notify.notify_one();
        self.next.sessions_changed(sessions);
    }

    fn accounts_changed(&self, accounts: &[Account]) {
        self.next.accounts_changed(accounts);
    }
}

/// Streams a host's sessions to its hub.
pub struct Replicator {
    /// Where the hub is and how it is pinned.
    pub hub: HubConfig,
    /// The key this host pairs with the hub as; see [`Replicator::device_key`].
    pub device: DeviceKey,
    /// This host.
    pub host: Host,
    /// Whose journals are replicated.
    pub sessions: SessionManager,
    /// Notified when the journal grows; see [`WakeOnEvent`].
    pub changed: Arc<Notify>,
    /// The host's secret requests, sent while connected.
    pub secrets: Arc<HostSecrets>,
    /// Serves the clients the hub relays to this host, once the daemon's server is up;
    /// until then, and for a host that runs none, the hub's requests go unanswered.
    pub relayed: Arc<OnceLock<Relayed>>,
}

impl Replicator {
    /// The device key in `data_dir`, created the first time.
    pub fn device_key(data_dir: &Path) -> Result<DeviceKey> {
        let path = data_dir.join(DEVICE_FILE);
        match std::fs::read_to_string(&path) {
            Ok(pem) => {
                DeviceKey::from_pem(&pem).with_context(|| format!("reading {}", path.display()))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let key = DeviceKey::generate()?;
                write_private(data_dir, DEVICE_FILE, key.to_pem().as_bytes())?;
                Ok(key)
            }
            Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Replicates until `shutdown`, reconnecting whenever the connection fails.
    pub async fn run(self, shutdown: CancellationToken) {
        let connector = match client_config(&self.hub.fingerprint, &self.device) {
            Ok(config) => TlsConnector::from(Arc::new(config)),
            Err(err) => return warn!("cannot replicate to the hub: {err:#}"),
        };
        // The hub's token for its Cloudflare relay, kept across connections to the hub: the
        // relay reaches this host while the hub is down too.
        let grants = tokio::sync::watch::Sender::new(None);
        if self.hub.relay {
            tokio::spawn(cloudflare_relay::machine(
                self.host.id.clone(),
                grants.subscribe(),
                self.serve_relayed(),
                shutdown.child_token(),
            ));
        }
        let mut attempt = 0;
        loop {
            let mut connected = false;
            let result = tokio::select! {
                () = shutdown.cancelled() => return,
                result = self.connection(&connector, &mut connected, &grants) => result,
            };
            if connected {
                attempt = 0;
            }
            let wait = backoff(attempt);
            let err = result
                .err()
                .unwrap_or_else(|| anyhow!("the hub closed the connection"));
            if attempt == 0 {
                warn!(hub = %self.hub.address, "replication paused, retrying in {wait:?}: {err:#}");
            } else {
                debug!(hub = %self.hub.address, "cannot reach the hub, retrying in {wait:?}: {err:#}");
            }
            attempt += 1;
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(wait) => {}
            }
        }
    }

    /// Connects once and exchanges hellos, pairing with the hub's code if this host is not
    /// paired yet, then hangs up: whether the hub takes this host.
    pub(super) async fn check(&self) -> Result<()> {
        let config = client_config(&self.hub.fingerprint, &self.device)?;
        let connector = TlsConnector::from(Arc::new(config));
        let (mut ws, _, _) = tokio::time::timeout(CONNECT_TIMEOUT, self.connect(&connector))
            .await
            .map_err(|_| anyhow!("no answer in {CONNECT_TIMEOUT:?}"))??;
        let _ = ws.close(None).await;
        Ok(())
    }

    /// One connection, until it fails; sets `connected` once the hub said hello, and
    /// `grants` to each token the hub gives for its Cloudflare relay.
    async fn connection(
        &self,
        connector: &TlsConnector,
        connected: &mut bool,
        grants: &tokio::sync::watch::Sender<Option<RelayGrant>>,
    ) -> Result<()> {
        let (mut ws, acked, grant) = tokio::time::timeout(CONNECT_TIMEOUT, self.connect(connector))
            .await
            .map_err(|_| anyhow!("no answer in {CONNECT_TIMEOUT:?}"))??;
        *connected = true;
        grants.send_replace(grant);
        info!(hub = %self.hub.address, sessions = acked.len(), "replicating to the hub");
        // Sessions another host took over while this host was gone stop here before they
        // replicate again, retried on every keepalive until it worked; the hub drops this
        // connection when one is taken over later.
        let mut released = !self.hub.backup || self.release_recovered().await;
        let mut link = Link::new(acked);
        // Without secrets, no request ever comes: the sender drops with this statement.
        let (_attached, mut asks) = if self.hub.secrets {
            let (attached, asks) = self.secrets.attach();
            (Some(attached), asks)
        } else {
            (None, tokio::sync::mpsc::unbounded_channel().1)
        };
        let mut keepalive = tokio::time::interval_at(Instant::now() + KEEPALIVE, KEEPALIVE);
        let mut heard = Instant::now();
        loop {
            if self.hub.backup {
                self.pump(&mut ws, &mut link).await?;
            }
            tokio::select! {
                () = self.changed.notified() => {}
                Some(ask) = asks.recv() => send(&mut ws, link.ask(ask)).await?,
                _ = keepalive.tick() => {
                    if heard.elapsed() > SILENCE {
                        bail!("the hub stopped answering");
                    }
                    ws.send(Message::Ping(Default::default())).await?;
                    if !released {
                        released = self.release_recovered().await;
                    }
                }
                frame = ws.next() => {
                    heard = Instant::now();
                    match frame.context("the hub closed the connection")?? {
                        Message::Text(text) => match serde_json::from_str(&text)? {
                            HubMessage::ProviderLogins { logins } => {
                                self.secrets.set_provider_logins(logins);
                            }
                            HubMessage::RelayOpen { stream_id, device_id } => {
                                self.open_relay(connector, stream_id, device_id);
                            }
                            HubMessage::RelayGrant { grant } => {
                                grants.send_replace(grant);
                            }
                            message => link.receive(message)?,
                        },
                        Message::Close(_) => bail!("the hub closed the connection"),
                        _ => {}
                    }
                }
            }
        }
    }

    /// Opens this host's end of the relay stream `stream_id` on a connection of its own, and
    /// serves the client the hub knows as `device` in it. Not awaited: the stream outlives
    /// this replication connection, and opening it must not hold replication up.
    fn open_relay(&self, connector: &TlsConnector, stream_id: String, device: DeviceId) {
        let Some(relayed) = self.relayed.get().filter(|_| self.hub.relay).cloned() else {
            return debug!(%stream_id, "not opening a relay stream: this host serves none");
        };
        let (hub, connector) = (self.hub.clone(), connector.clone());
        tokio::spawn(async move {
            let accepting = client::accept_relay(&hub, &connector, &stream_id);
            match tokio::time::timeout(CONNECT_TIMEOUT, accepting).await {
                Ok(Ok((stream, peer))) => relayed.serve(stream, peer, device),
                Ok(Err(err)) => info!(%stream_id, "cannot open a relay stream: {err:#}"),
                Err(_) => info!(%stream_id, "cannot open a relay stream in {CONNECT_TIMEOUT:?}"),
            }
        });
    }

    /// Serves each client that reaches this host through the hub's Cloudflare relay as one the
    /// hub relays, once the daemon's server is up.
    fn serve_relayed(&self) -> Arc<dyn Fn(cloudflare_relay::Opened) + Send + Sync> {
        let relayed = Arc::clone(&self.relayed);
        Arc::new(move |opened| match relayed.get() {
            Some(relayed) => {
                relayed.serve(opened.stream, opened.peer, DeviceId::new(opened.device));
            }
            None => debug!("not serving a relayed client: this host serves none yet"),
        })
    }

    /// Makes this host's sessions that another host took over read-only; returns whether
    /// that worked.
    async fn release_recovered(&self) -> bool {
        let released = release_recovered(&self.hub, &self.device, &self.host, &self.sessions);
        match released.await {
            Ok(()) => true,
            Err(err) => {
                warn!("cannot check for sessions other hosts took over: {err:#}");
                false
            }
        }
    }

    /// Connects and exchanges hellos; returns the hub's cursors, and its token for its
    /// Cloudflare relay.
    async fn connect(
        &self,
        connector: &TlsConnector,
    ) -> Result<(Ws, Vec<Cursor>, Option<RelayGrant>)> {
        let mut ws = client::dial(&self.hub, connector).await?;
        let hello = HostMessage::Hello(HostHello {
            replication_version: REPLICATION_VERSION,
            host_id: self.host.id.clone(),
            host_name: self.host.name.clone(),
            build: BUILD.to_owned(),
            pairing_code: self.hub.pairing_code.clone(),
            attachments_cap: self.hub.images_cap(),
            relay: self.hub.relay,
            cloudflare_relay: self.hub.relay,
        });
        ws.send(Message::text(serde_json::to_string(&hello)?))
            .await?;
        loop {
            let text = match ws.next().await {
                Some(Ok(Message::Text(text))) => text,
                Some(Ok(Message::Close(_))) | None => bail!("the hub closed the connection"),
                Some(Ok(_)) => continue,
                Some(Err(err)) => return Err(err.into()),
            };
            return match serde_json::from_str(&text)? {
                HubMessage::Hello(hello) if hello.replication_version == REPLICATION_VERSION => {
                    let through = hello
                        .cliproxyapi_relay
                        .then(|| pin::Through::new(self.hub.clone(), connector.clone()));
                    self.secrets.set_cliproxyapi(hello.cliproxyapi, through);
                    Ok((ws, hello.acked, hello.relay_grant))
                }
                HubMessage::Hello(hello) => Err(anyhow!(
                    "the hub speaks replication {}, this host {REPLICATION_VERSION}",
                    hello.replication_version
                )),
                HubMessage::Error { error } => Err(anyhow!("the hub refused: {}", error.message)),
                _ => Err(anyhow!("the hub did not say hello")),
            };
        }
    }

    /// Sends every summary that changed, and every event not sent yet, within the window.
    async fn pump(&self, ws: &mut Ws, link: &mut Link) -> Result<()> {
        for summary in self.sessions.summaries(&self.host.id).await? {
            let session_id = summary.session_id.clone();
            let head = summary.head_seq;
            if link.published.get(&session_id) != Some(&summary) {
                link.published.insert(session_id.clone(), summary.clone());
                send(ws, HostMessage::Session(summary)).await?;
            }
            if link.stopped.contains(&session_id) {
                continue;
            }
            let mut sent = link.sent.get(&session_id).copied().unwrap_or(0);
            if sent > head {
                warn!(
                    %session_id,
                    "the hub holds {sent} events of this session, more than this host's {head}; \
                     not replicating it until that is resolved"
                );
                link.stopped.insert(session_id);
                continue;
            }
            let acked = link.acked.get(&session_id).copied().unwrap_or(0);
            while sent < head && sent.saturating_sub(acked) < WINDOW {
                let events = self
                    .sessions
                    .read_records_since(&session_id, sent, MAX_BATCH_EVENTS)
                    .await?;
                let Some(last) = events.last().map(|event| event.seq) else {
                    break;
                };
                let images = if self.hub.attachments {
                    attachments(&events)
                } else {
                    Vec::new()
                };
                for attachment in images {
                    match self
                        .sessions
                        .attachment_data(&session_id, &attachment)
                        .await
                    {
                        Ok(data) => {
                            let image = AttachmentData {
                                session_id: session_id.clone(),
                                attachment,
                                data,
                            };
                            send(ws, HostMessage::Attachment(image)).await?;
                        }
                        // The hub never gets an image this host lost; its journal still
                        // names it.
                        Err(error) => warn!(
                            %session_id,
                            attachment_id = %attachment.attachment_id,
                            "an image is not replicated: {}",
                            error.message
                        ),
                    }
                }
                let batch = Batch {
                    session_id: session_id.clone(),
                    events,
                };
                send(ws, HostMessage::Batch(batch)).await?;
                sent = last;
            }
            link.sent.insert(session_id, sent);
        }
        Ok(())
    }
}

/// What one connection knows; dropped with it, as the hub's hello is the only cursor.
struct Link {
    /// Last seq sent per session.
    sent: HashMap<SessionId, Seq>,
    /// Last seq the hub acknowledged per session.
    acked: HashMap<SessionId, Seq>,
    /// Sessions not replicated, as the hub holds a different history of them.
    stopped: HashSet<SessionId>,
    /// The summary last sent per session.
    published: HashMap<SessionId, SessionSummary>,
    /// Secret requests sent and not answered yet, by request id.
    pending: HashMap<u64, Pending>,
    /// The id of the last secret request sent.
    requested: u64,
}

/// Who waits for the answer to a secret request.
enum Pending {
    List(oneshot::Sender<Vec<SecretInfo>>),
    Fetch(oneshot::Sender<Result<Vec<FetchedSecret>, SecretError>>),
}

impl Link {
    fn new(acked: Vec<Cursor>) -> Self {
        let acked: HashMap<_, _> = acked
            .into_iter()
            .map(|cursor| (cursor.session_id, cursor.after_seq))
            .collect();
        Self {
            sent: acked.clone(),
            acked,
            stopped: HashSet::new(),
            published: HashMap::new(),
            pending: HashMap::new(),
            requested: 0,
        }
    }

    /// The message asking the hub for `ask`, whose answer goes where `ask` says.
    fn ask(&mut self, ask: Ask) -> HostMessage {
        self.requested += 1;
        let request_id = self.requested;
        match ask {
            Ask::List(answer) => {
                self.pending.insert(request_id, Pending::List(answer));
                HostMessage::ListSecrets { request_id }
            }
            Ask::Fetch {
                used,
                secrets,
                answer,
            } => {
                self.pending.insert(request_id, Pending::Fetch(answer));
                match used {
                    SecretUse::Session(session_id) => HostMessage::FetchSecrets(SecretFetch {
                        request_id,
                        session_id,
                        secrets,
                    }),
                    SecretUse::Host(purpose) => HostMessage::FetchHostSecrets(HostSecretFetch {
                        request_id,
                        purpose,
                        secrets,
                    }),
                }
            }
        }
    }

    fn receive(&mut self, message: HubMessage) -> Result<()> {
        match message {
            HubMessage::Ack(cursor) => {
                let acked = self.acked.entry(cursor.session_id).or_default();
                *acked = (*acked).max(cursor.after_seq);
            }
            HubMessage::Rejected {
                cursor,
                reason: RejectReason::Gap,
            } => {
                self.acked
                    .insert(cursor.session_id.clone(), cursor.after_seq);
                self.sent.insert(cursor.session_id, cursor.after_seq);
            }
            HubMessage::Rejected {
                cursor,
                reason: RejectReason::Conflict,
            } => {
                warn!(
                    session_id = %cursor.session_id,
                    "the hub holds a different history of this session; not replicating it \
                     until that is resolved"
                );
                self.stopped.insert(cursor.session_id);
            }
            HubMessage::Error { error } => bail!("the hub refused: {}", error.message),
            // A requester that gave up gets nothing.
            HubMessage::SecretList {
                request_id,
                secrets,
            } => {
                if let Some(Pending::List(answer)) = self.pending.remove(&request_id) {
                    let _ = answer.send(secrets);
                }
            }
            HubMessage::SecretValues {
                request_id,
                secrets,
            } => {
                if let Some(Pending::Fetch(answer)) = self.pending.remove(&request_id) {
                    let _ = answer.send(Ok(secrets));
                }
            }
            HubMessage::SecretsRefused { request_id, error } => {
                if let Some(Pending::Fetch(answer)) = self.pending.remove(&request_id) {
                    let _ = answer.send(Err(error));
                }
            }
            // Taken by the connection's loop, for the host's secrets and its relayed clients.
            HubMessage::ProviderLogins { .. }
            | HubMessage::RelayOpen { .. }
            | HubMessage::RelayGrant { .. }
            | HubMessage::Hello(_)
            | HubMessage::Unknown => {}
        }
        Ok(())
    }
}

/// Every image and file `records` name, in order: the prompts' and the published artifacts.
fn attachments(records: &[JournalRecord]) -> Vec<Attachment> {
    records
        .iter()
        // Only these events name attachments; skip decoding everything else.
        .filter(|record| {
            matches!(
                record.body.event_type(),
                "item_added" | "artifact_published"
            )
        })
        .flat_map(|record| record.body.decode().attachments().to_vec())
        .collect()
}

async fn send(ws: &mut Ws, message: HostMessage) -> Result<()> {
    let text = serde_json::to_string(&message)?;
    ws.send(Message::text(text))
        .await
        .context("writing to the hub")
}

/// Wait before retry `attempt` (0 for the first): doubling from [`BACKOFF_BASE`] up to
/// [`BACKOFF_CAP`].
fn backoff(attempt: u32) -> Duration {
    BACKOFF_BASE
        .saturating_mul(2u32.saturating_pow(attempt))
        .min(BACKOFF_CAP)
}

/// Makes every session of this host read-only that the hub now shows on another host,
/// which took it over under the same id while this one was gone.
async fn release_recovered(
    hub: &HubConfig,
    device: &DeviceKey,
    host: &Host,
    sessions: &SessionManager,
) -> Result<()> {
    let held = client::sessions(hub, device).await?;
    let local: HashMap<SessionId, SessionStatus> = sessions
        .sessions()
        .await?
        .into_iter()
        .map(|head| (head.session_id, head.status))
        .collect();
    for head in held {
        let (Some(holder), Some(status)) = (&head.host_id, local.get(&head.session_id)) else {
            continue;
        };
        if *holder == host.id || *status == SessionStatus::Moved {
            continue;
        }
        info!(
            session_id = %head.session_id,
            to_host = %holder,
            "another host took the session over; it is read-only here now"
        );
        sessions
            .moved_away(head.session_id.clone())
            .await
            .map_err(|error| anyhow::anyhow!(error.message))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let waits: Vec<_> = (0..8).map(|attempt| backoff(attempt).as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(backoff(u32::MAX), BACKOFF_CAP);
    }
}
