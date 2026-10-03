//! The host side of replication: one background task streaming every session's journal to
//! the vault.
//!
//! It connects as a paired device, takes the vault's cursors from its hello, sends each
//! session's summary and the events after its cursor, then follows the journal as it grows.
//! It reads the journal on its own and learns of new events through [`WakeOnEvent`], which
//! only wakes it, so a slow or unreachable vault never holds up a session. When the vault is
//! unreachable it retries with a backoff of up to [`BACKOFF_CAP`]. Every image a batch's
//! prompts carried goes just ahead of the batch.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use herder_client_core::auth::{DeviceKey, client_config};
use herder_protocol::{
    Account, Attachment, AttachmentData, Batch, Cursor, Event, EventBody, HostHello, HostMessage,
    Item, ItemBody, ItemId, JournalRecord, MAX_BATCH_EVENTS, REPLICATION_VERSION, RejectReason,
    Seq, SessionHead, SessionId, SessionStatus, SessionSummary, VaultMessage,
};
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::BUILD;
use super::client::{self, Ws};
use crate::config::VaultConfig;
use crate::data_dir::write_private;
use crate::session::{EventSink, SessionManager};
use crate::ws::Host;

/// File in the data dir holding the key this host pairs with the vault as.
const DEVICE_FILE: &str = "vault-device.pem";

/// Time to connect and get the vault's hello.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// First wait before reconnecting; it doubles on every failed attempt up to [`BACKOFF_CAP`].
const BACKOFF_BASE: Duration = Duration::from_secs(1);

/// Longest wait between attempts to reach the vault.
pub const BACKOFF_CAP: Duration = Duration::from_secs(60);

/// How often an idle connection is pinged.
const KEEPALIVE: Duration = Duration::from_secs(20);

/// Silence after which the vault is taken for gone and the connection dropped.
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

/// Streams a host's sessions to its vault.
pub struct Replicator {
    /// Where the vault is and how it is pinned.
    pub vault: VaultConfig,
    /// The key this host pairs with the vault as; see [`Replicator::device_key`].
    pub device: DeviceKey,
    /// This host.
    pub host: Host,
    /// Whose journals are replicated.
    pub sessions: SessionManager,
    /// Notified when the journal grows; see [`WakeOnEvent`].
    pub changed: Arc<Notify>,
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
        let connector = match client_config(&self.vault.fingerprint, &self.device) {
            Ok(config) => TlsConnector::from(Arc::new(config)),
            Err(err) => return warn!("cannot replicate to the vault: {err:#}"),
        };
        let mut attempt = 0;
        loop {
            let mut connected = false;
            let result = tokio::select! {
                () = shutdown.cancelled() => return,
                result = self.connection(&connector, &mut connected) => result,
            };
            if connected {
                attempt = 0;
            }
            let wait = backoff(attempt);
            let err = result
                .err()
                .unwrap_or_else(|| anyhow!("the vault closed the connection"));
            if attempt == 0 {
                warn!(vault = %self.vault.address, "replication paused, retrying in {wait:?}: {err:#}");
            } else {
                debug!(vault = %self.vault.address, "cannot reach the vault, retrying in {wait:?}: {err:#}");
            }
            attempt += 1;
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(wait) => {}
            }
        }
    }

    /// Connects once and exchanges hellos, pairing with the vault's code if this host is not
    /// paired yet, then hangs up: whether the vault takes this host.
    pub(super) async fn check(&self) -> Result<()> {
        let config = client_config(&self.vault.fingerprint, &self.device)?;
        let connector = TlsConnector::from(Arc::new(config));
        let (mut ws, _) = tokio::time::timeout(CONNECT_TIMEOUT, self.connect(&connector))
            .await
            .map_err(|_| anyhow!("no answer in {CONNECT_TIMEOUT:?}"))??;
        let _ = ws.close(None).await;
        Ok(())
    }

    /// One connection, until it fails; sets `connected` once the vault said hello.
    async fn connection(&self, connector: &TlsConnector, connected: &mut bool) -> Result<()> {
        let (mut ws, acked) = tokio::time::timeout(CONNECT_TIMEOUT, self.connect(connector))
            .await
            .map_err(|_| anyhow!("no answer in {CONNECT_TIMEOUT:?}"))??;
        *connected = true;
        info!(vault = %self.vault.address, sessions = acked.len(), "replicating to the vault");
        // Sessions another host took over while this host was gone stop here before they
        // replicate again, retried on every keepalive until it worked; the vault drops this
        // connection when one is taken over later.
        let mut released = self.release_recovered().await;
        let mut link = Link::new(acked);
        let mut keepalive = tokio::time::interval_at(Instant::now() + KEEPALIVE, KEEPALIVE);
        let mut heard = Instant::now();
        loop {
            self.pump(&mut ws, &mut link).await?;
            tokio::select! {
                () = self.changed.notified() => {}
                _ = keepalive.tick() => {
                    if heard.elapsed() > SILENCE {
                        bail!("the vault stopped answering");
                    }
                    ws.send(Message::Ping(Default::default())).await?;
                    if !released {
                        released = self.release_recovered().await;
                    }
                }
                frame = ws.next() => {
                    heard = Instant::now();
                    match frame.context("the vault closed the connection")?? {
                        Message::Text(text) => link.receive(serde_json::from_str(&text)?)?,
                        Message::Close(_) => bail!("the vault closed the connection"),
                        _ => {}
                    }
                }
            }
        }
    }

    /// Makes this host's sessions that another host took over read-only; returns whether
    /// that worked.
    async fn release_recovered(&self) -> bool {
        let released = release_recovered(&self.vault, &self.device, &self.host, &self.sessions);
        match released.await {
            Ok(()) => true,
            Err(err) => {
                warn!("cannot check for sessions other hosts took over: {err:#}");
                false
            }
        }
    }

    /// Connects and exchanges hellos; returns the vault's cursors.
    async fn connect(&self, connector: &TlsConnector) -> Result<(Ws, Vec<Cursor>)> {
        let mut ws = client::dial(&self.vault, connector).await?;
        let hello = HostMessage::Hello(HostHello {
            replication_version: REPLICATION_VERSION,
            host_id: self.host.id.clone(),
            host_name: self.host.name.clone(),
            build: BUILD.to_owned(),
            pairing_code: self.vault.pairing_code.clone(),
        });
        ws.send(Message::text(serde_json::to_string(&hello)?))
            .await?;
        loop {
            let text = match ws.next().await {
                Some(Ok(Message::Text(text))) => text,
                Some(Ok(Message::Close(_))) | None => bail!("the vault closed the connection"),
                Some(Ok(_)) => continue,
                Some(Err(err)) => return Err(err.into()),
            };
            return match serde_json::from_str(&text)? {
                VaultMessage::Hello(hello) if hello.replication_version == REPLICATION_VERSION => {
                    Ok((ws, hello.acked))
                }
                VaultMessage::Hello(hello) => Err(anyhow!(
                    "the vault speaks replication {}, this host {REPLICATION_VERSION}",
                    hello.replication_version
                )),
                VaultMessage::Error { error } => {
                    Err(anyhow!("the vault refused: {}", error.message))
                }
                _ => Err(anyhow!("the vault did not say hello")),
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
                    "the vault holds {sent} events of this session, more than this host's {head}; \
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
                for attachment in attachments(&events) {
                    match self.sessions.image(&session_id, &attachment).await {
                        Ok(image) => {
                            let image = AttachmentData {
                                session_id: session_id.clone(),
                                attachment,
                                data: image.data,
                            };
                            send(ws, HostMessage::Attachment(image)).await?;
                        }
                        // The vault never gets an image this host lost; its journal still
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

/// What one connection knows; dropped with it, as the vault's hello is the only cursor.
struct Link {
    /// Last seq sent per session.
    sent: HashMap<SessionId, Seq>,
    /// Last seq the vault acknowledged per session.
    acked: HashMap<SessionId, Seq>,
    /// Sessions not replicated, as the vault holds a different history of them.
    stopped: HashSet<SessionId>,
    /// The summary last sent per session.
    published: HashMap<SessionId, SessionSummary>,
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
        }
    }

    fn receive(&mut self, message: VaultMessage) -> Result<()> {
        match message {
            VaultMessage::Ack(cursor) => {
                let acked = self.acked.entry(cursor.session_id).or_default();
                *acked = (*acked).max(cursor.after_seq);
            }
            VaultMessage::Rejected {
                cursor,
                reason: RejectReason::Gap,
            } => {
                self.acked
                    .insert(cursor.session_id.clone(), cursor.after_seq);
                self.sent.insert(cursor.session_id, cursor.after_seq);
            }
            VaultMessage::Rejected {
                cursor,
                reason: RejectReason::Conflict,
            } => {
                warn!(
                    session_id = %cursor.session_id,
                    "the vault holds a different history of this session; not replicating it \
                     until that is resolved"
                );
                self.stopped.insert(cursor.session_id);
            }
            VaultMessage::Error { error } => bail!("the vault refused: {}", error.message),
            VaultMessage::Hello(_) | VaultMessage::Unknown => {}
        }
        Ok(())
    }
}

/// Every image the prompts among `records` carried, in order.
fn attachments(records: &[JournalRecord]) -> Vec<Attachment> {
    records
        .iter()
        // Only a `user_message` item names images; skip decoding everything else.
        .filter(|record| record.body.event_type() == "item_added")
        .flat_map(|record| match record.body.decode() {
            EventBody::ItemAdded {
                item:
                    Item {
                        body: ItemBody::UserMessage { attachments, .. },
                        ..
                    },
            } => attachments,
            _ => Vec::new(),
        })
        .collect()
}

async fn send(ws: &mut Ws, message: HostMessage) -> Result<()> {
    let text = serde_json::to_string(&message)?;
    ws.send(Message::text(text))
        .await
        .context("writing to the vault")
}

/// Wait before retry `attempt` (0 for the first): doubling from [`BACKOFF_BASE`] up to
/// [`BACKOFF_CAP`].
fn backoff(attempt: u32) -> Duration {
    BACKOFF_BASE
        .saturating_mul(2u32.saturating_pow(attempt))
        .min(BACKOFF_CAP)
}

/// Makes every session of this host read-only that the vault now shows on another host,
/// which took it over under the same id while this one was gone.
async fn release_recovered(
    vault: &VaultConfig,
    device: &DeviceKey,
    host: &Host,
    sessions: &SessionManager,
) -> Result<()> {
    let view = client::read(vault, device, None).await?;
    let local: HashMap<SessionId, SessionStatus> = sessions
        .sessions()
        .await?
        .into_iter()
        .map(|head| (head.session_id, head.status))
        .collect();
    for head in view.sessions {
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
