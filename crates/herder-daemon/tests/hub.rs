//! A host daemon replicating to a hub daemon, both in process over TLS on localhost: sessions
//! appear in the hub, a hub restarted mid-stream gets the rest, images included, a host that was offline
//! catches up when it is back, and a client paired with the hub sees every host's sessions,
//! read-only, and the hosts with their liveness. A host's device replicates and reads nothing.
//! A host fetches the secrets of the sets granted to it, and only those, each fetch audited,
//! and holds those granted for its own use, with no session, until they are revoked.
//! A host backs up and fetches secrets only as its `[hub]` switches say, each without the other.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use herder_client_core::auth::{DeviceKey, client_config};
use herder_client_core::{Client, Error, Machine, SessionSubscription};
use herder_daemon::Fanout;
use herder_daemon::auth::authenticator::{Authenticator, Join};
use herder_daemon::auth::{Auth, DeviceRole, PAIRING_TTL};
use herder_daemon::config::{HubConfig, Retention};
use herder_daemon::hub::{
    Admin, ClientKeys, Held, HeldSecrets, HostSecrets, HubStore, LIVENESS_TIMEOUT, Replicator,
    SecretsKey, Server, WakeOnEvent,
};
use herder_daemon::session::{Accounts, Adapters, EventSink, SessionManager, Setup};
use herder_daemon::worktree::Worktrees;
use herder_daemon::ws::{Host, Tls};
use herder_protocol::{
    AccountId, Attachment, AttachmentId, ClientHello, ClientMessage, Command, CommandBody,
    CommandId, CommandResult, Cursor, ErrorCode, Event, EventBody, HostHello, HostId, HostMessage,
    HostSecretsState, HubMessage, Item, ItemBody, ItemId, JournalRecord, PROTOCOL_VERSION,
    PermissionMode, Provider, REPLICATION_VERSION, RelayError, RelayErrorCode, RelayRoute,
    ReplicationErrorCode, SecretError, SecretErrorCode, SecretRef, SecretValue, ServerMessage,
    SessionId, SessionStatus, SessionSummary, Timestamp, TurnId, UserId,
};
use herder_store::{NewEvent, Store};
use rustls::pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(30);

/// The master key test hubs keep secrets under.
const SECRETS_KEY: [u8; 32] = [7; 32];

fn host_id() -> HostId {
    HostId::new("host-1")
}

/// A daemon task on its own runtime, so killing it drops every task at once, as a killed
/// process would.
struct Runtime(Option<tokio::runtime::Runtime>);

impl Runtime {
    fn new() -> Self {
        Self(Some(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap(),
        ))
    }

    fn handle(&self) -> &tokio::runtime::Runtime {
        self.0.as_ref().unwrap()
    }

    async fn kill(mut self) {
        let runtime = self.0.take().unwrap();
        tokio::task::spawn_blocking(move || runtime.shutdown_timeout(TIMEOUT))
            .await
            .unwrap();
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_background();
        }
    }
}

struct Hub {
    runtime: Runtime,
    addr: SocketAddr,
    /// Where the hub takes tunneled connections, as `cloudflared` delivers them.
    tunnel: SocketAddr,
    fingerprint: String,
    auth: Arc<Auth>,
    admin: Admin,
}

impl Hub {
    /// Starts a hub on `dir`, listening on `port` (0 for any).
    async fn start(dir: &Path, port: u16) -> Self {
        Self::start_with(dir, port, LIVENESS_TIMEOUT, Some(SECRETS_KEY), None).await
    }

    /// Starts a hub that takes a host silent for `liveness` for gone, and keeps secrets
    /// under `secrets_key`.
    async fn start_with(
        dir: &Path,
        port: u16,
        liveness: Duration,
        secrets_key: Option<[u8; 32]>,
        cliproxyapi: Option<ClientKeys>,
    ) -> Self {
        let runtime = Runtime::new();
        let dir = dir.to_owned();
        std::fs::create_dir_all(dir.join("tls")).unwrap();
        let (started, ready) = tokio::sync::oneshot::channel();
        runtime.handle().spawn({
            async move {
                let tls = Tls::load_or_create(&dir.join("tls"), "hub").unwrap();
                let auth = Arc::new(Auth::open(&dir).unwrap());
                let store = HubStore::open(dir.join("hub.db")).unwrap();
                let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
                let addr = listener.local_addr().unwrap();
                let tunnel_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let tunnel = tunnel_listener.local_addr().unwrap();
                let fingerprint = tls.fingerprint().to_owned();
                let host = Host {
                    id: HostId::new("hub"),
                    name: "hub".into(),
                };
                let server = Server::new(
                    tls,
                    Arc::clone(&auth),
                    store,
                    host,
                    liveness,
                    Retention::default(),
                    secrets_key.as_ref().map(SecretsKey::new),
                );
                if let Some(keys) = cliproxyapi {
                    server.use_cliproxyapi(keys).unwrap();
                }
                started
                    .send((addr, tunnel, fingerprint, auth, server.admin()))
                    .ok()
                    .unwrap();
                server
                    .run_tunneled(
                        vec![listener],
                        Some(tunnel_listener),
                        CancellationToken::new(),
                    )
                    .await;
            }
        });
        let (addr, tunnel, fingerprint, auth, admin) = ready.await.unwrap();
        Self {
            runtime,
            addr,
            tunnel,
            fingerprint,
            auth,
            admin,
        }
    }

    /// Where a host replicates to, pairing as a host-only device with a fresh code, as
    /// `herder pair --host` mints it; images are not backed up.
    fn config(&self) -> HubConfig {
        HubConfig::new(
            self.addr.to_string(),
            self.fingerprint.clone(),
            Some(self.auth.mint_host("devbox", PAIRING_TTL).unwrap().code),
        )
    }

    /// Signs `client` in as the hub's owner, the person `alice`, who claims it first.
    async fn join(&self, client: &Client) -> (Authenticator, Machine) {
        self.join_at(client, self.addr.to_string()).await
    }

    /// [`Hub::join`], reaching the hub at `address`.
    async fn join_at(&self, client: &Client, address: String) -> (Authenticator, Machine) {
        Join::open(&self.auth, herder_protocol::Role::Owner)
            .join(client, vec![address], &self.fingerprint, "alice")
            .await
    }

    /// Every event the hub holds of `host-1`, by session; read from its database file.
    fn held(dir: &Path) -> BTreeMap<SessionId, Vec<JournalRecord>> {
        let store = HubStore::open(dir.join("hub.db")).unwrap();
        store
            .cursors(&host_id())
            .unwrap()
            .into_iter()
            .map(|cursor| {
                let records = store
                    .records(&host_id(), &cursor.session_id, 0, usize::MAX)
                    .unwrap();
                (cursor.session_id, records)
            })
            .collect()
    }

    fn summaries(dir: &Path) -> Vec<SessionSummary> {
        HubStore::open(dir.join("hub.db"))
            .unwrap()
            .summaries(&host_id())
            .unwrap()
    }
}

/// A host daemon's sessions and its replicator.
struct HostDaemon {
    runtime: Runtime,
    sessions: SessionManager,
    secrets: Arc<HostSecrets>,
}

impl HostDaemon {
    async fn start(dir: &Path, hub: HubConfig) -> Self {
        Self::start_with(dir, hub, HostSecrets::default(), Adapters::new()).await
    }

    /// Starts a host whose secrets are `secrets`, running sessions on `adapters`.
    async fn start_with(
        dir: &Path,
        hub: HubConfig,
        secrets: HostSecrets,
        adapters: Adapters,
    ) -> Self {
        let runtime = Runtime::new();
        let dir = dir.to_owned();
        let secrets = Arc::new(secrets);
        secrets.set_linked(hub.secrets);
        let host_secrets = Arc::clone(&secrets);
        let sessions = runtime
            .handle()
            .spawn(async move {
                let changed = Arc::new(Notify::new());
                let sink = Arc::new(WakeOnEvent {
                    next: Arc::new(Fanout::default()) as Arc<dyn EventSink>,
                    notify: Arc::clone(&changed),
                });
                let setup = Setup {
                    store: Store::open(dir.join("herder.db")).unwrap(),
                    adapters,
                    accounts: Accounts::new(),
                    sink,
                    turn_ids: Box::new(|| TurnId::new("turn")),
                    worktrees: Worktrees::new(dir.join("worktrees")),
                    attachments: dir.join("attachments"),
                    chats: dir.join("chats"),
                };
                let shutdown = CancellationToken::new();
                let sessions = SessionManager::open(setup, shutdown.clone()).await.unwrap();
                sessions.use_secrets(Arc::clone(&host_secrets)).unwrap();
                sessions
                    .use_hub_logins(dir.join("hub-accounts"))
                    .await
                    .unwrap();
                let replicator = Replicator {
                    hub,
                    device: Replicator::device_key(&dir).unwrap(),
                    host: Host {
                        id: host_id(),
                        name: "devbox".into(),
                    },
                    sessions: sessions.clone(),
                    changed,
                    secrets: host_secrets,
                    relayed: Arc::default(),
                };
                tokio::spawn(replicator.run(shutdown));
                sessions
            })
            .await
            .unwrap();
        Self {
            runtime,
            sessions,
            secrets,
        }
    }

    /// Records a model switch in `session` through the session manager, as a client would.
    async fn switch_model(&self, session: &str, model: &str) {
        let sessions = self.sessions.clone();
        let command = CommandBody::SetModel {
            session_id: SessionId::new(session),
            model: model.to_owned(),
        };
        self.runtime
            .handle()
            .spawn(async move { sessions.handle(UserId::new("alice"), command).await })
            .await
            .unwrap()
            .unwrap();
    }
}

/// Appends to the host's journal directly: `sessions` sessions, each created and then given
/// `events` model switches.
fn seed(dir: &Path, sessions: usize, events: usize) {
    let mut store = Store::open(dir.join("herder.db")).unwrap();
    for s in 1..=sessions {
        let session_id = SessionId::new(format!("s{s}"));
        if store.latest_seq(&session_id).unwrap() == 0 {
            let body = EventBody::SessionCreated {
                repo: "/home/dev/herder".into(),
                worktree: format!("/home/dev/worktrees/s{s}"),
                branch: Some(format!("herder/s{s}")),
                provider: Provider::Claude,
                account_id: AccountId::new("main"),
                model: "m0".into(),
                permission_mode: PermissionMode::Ask,
                parent: None,
                parent_host: None,
                task: None,
                failover_pin: None,
                chat: false,
                origin: None,
            };
            append(&mut store, &session_id, body);
        }
        for e in 0..events {
            let body = EventBody::ModelSwitched {
                model: format!("m{e}"),
            };
            append(&mut store, &session_id, body);
        }
    }
}

/// Appends `prompts` prompts to each of the host's `sessions` sessions, each carrying one PNG
/// kept where the session manager keeps images.
fn seed_images(dir: &Path, sessions: usize, prompts: usize) {
    let mut store = Store::open(dir.join("herder.db")).unwrap();
    for s in 1..=sessions {
        let session_id = SessionId::new(format!("s{s}"));
        let images = dir.join("attachments").join(session_id.as_str());
        std::fs::create_dir_all(&images).unwrap();
        for p in 0..prompts {
            let id = format!("img{s}x{p}");
            let data = [PNG, id.as_bytes()].concat();
            std::fs::write(images.join(format!("{id}.png")), &data).unwrap();
            let item = Item {
                agent_message: None,
                follow_up: None,
                parent_call_id: None,
                id: ItemId::new(format!("item-{id}")),
                turn_id: TurnId::new("turn"),
                body: ItemBody::UserMessage {
                    text: "Look.".into(),
                    attachments: vec![Attachment {
                        attachment_id: AttachmentId::new(id),
                        media_type: "image/png".into(),
                        size: data.len() as u64,
                        name: None,
                    }],
                },
            };
            append(&mut store, &session_id, EventBody::ItemAdded { item });
        }
    }
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Every image the events the hub holds name: the image the hub holds, if any, and the one
/// the host keeps.
fn images(host_dir: &Path, hub_dir: &Path) -> Vec<(Option<Vec<u8>>, Vec<u8>)> {
    let store = HubStore::open(hub_dir.join("hub.db")).unwrap();
    let mut images = Vec::new();
    for (session_id, records) in Hub::held(hub_dir) {
        for record in records {
            let EventBody::ItemAdded { item } = record.body.decode() else {
                continue;
            };
            let ItemBody::UserMessage { attachments, .. } = item.body else {
                continue;
            };
            for attachment in attachments {
                let held = store
                    .attachment(&host_id(), &session_id, &attachment.attachment_id)
                    .unwrap();
                let kept = host_dir
                    .join("attachments")
                    .join(session_id.as_str())
                    .join(format!("{}.png", attachment.attachment_id));
                images.push((held.map(|image| image.data.0), std::fs::read(kept).unwrap()));
            }
        }
    }
    images
}

fn append(store: &mut Store, session_id: &SessionId, body: EventBody) {
    store
        .append(NewEvent {
            session_id: session_id.clone(),
            at: Timestamp::now(),
            by: None,
            body,
        })
        .unwrap();
}

/// Every event of every session in the host's journal.
fn journal(dir: &Path) -> BTreeMap<SessionId, Vec<JournalRecord>> {
    let store = Store::open(dir.join("herder.db")).unwrap();
    store
        .sessions()
        .unwrap()
        .into_iter()
        .map(|session| {
            let records = store
                .read_records_since(&session.session_id, 0, usize::MAX)
                .unwrap();
            (session.session_id, records)
        })
        .collect()
}

/// Waits until the hub holds exactly the host's journal, and its fleet index the host's
/// latest seqs.
async fn caught_up(host_dir: &Path, hub_dir: &Path) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let host = journal(host_dir);
        let held = Hub::held(hub_dir);
        let heads: BTreeMap<_, _> = Hub::summaries(hub_dir)
            .into_iter()
            .map(|summary| (summary.session_id, summary.head_seq))
            .collect();
        let host_heads: BTreeMap<_, _> = host
            .iter()
            .map(|(id, records)| (id.clone(), records.len() as u64))
            .collect();
        if held == host && heads == host_heads {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the hub did not catch up: holds {:?} of {:?}",
            held.values().map(Vec::len).collect::<Vec<_>>(),
            host.values().map(Vec::len).collect::<Vec<_>>(),
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Every session's seqs held by the hub are gap-free from 1, so a resend never duplicated
/// or skipped one.
fn assert_gap_free(hub_dir: &Path) {
    for (session_id, records) in Hub::held(hub_dir) {
        let seqs: Vec<_> = records.iter().map(|r| r.seq).collect();
        assert!(
            seqs.iter().copied().eq(1..=seqs.len() as u64),
            "{session_id}: {seqs:?}"
        );
    }
}

/// The machine a link of one machine paired with.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_appear_in_the_hub_and_follow_live() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    std::fs::create_dir_all(&host_dir).unwrap();
    // More events than one batch holds.
    seed(&host_dir, 3, 300);
    let hub = Hub::start(&hub_dir, 0).await;
    let host = HostDaemon::start(&host_dir, hub.config()).await;
    caught_up(&host_dir, &hub_dir).await;

    host.switch_model("s2", "live").await;
    caught_up(&host_dir, &hub_dir).await;
    let held = Hub::held(&hub_dir);
    let last = held[&SessionId::new("s2")].last().unwrap();
    assert_eq!(
        last.body.decode(),
        EventBody::ModelSwitched {
            model: "live".into()
        }
    );
    assert_eq!(last.by.as_ref().map(|by| by.as_str()), Some("alice"));
    let summaries = Hub::summaries(&hub_dir);
    assert_eq!(summaries.len(), 3);
    assert_eq!(summaries[0].branch.as_deref(), Some("herder/s1"));
    assert_eq!(summaries[0].project_id.as_str(), "host-1:/home/dev/herder");
    host.runtime.kill().await;
    hub.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hub_restarted_mid_stream_gets_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    std::fs::create_dir_all(&host_dir).unwrap();
    // More than the replicator's window of unacknowledged events, so the kill comes mid-stream,
    // with images all along.
    for _ in 0..10 {
        seed(&host_dir, 2, 100);
        seed_images(&host_dir, 2, 5);
    }
    let hub = Hub::start(&hub_dir, 0).await;
    let addr = hub.addr;
    let config = HubConfig {
        attachments: true,
        ..hub.config()
    };
    let host = HostDaemon::start(&host_dir, config).await;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while Hub::held(&hub_dir).is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "nothing replicated");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    hub.runtime.kill().await;
    // Every image an event the hub holds names was durable before that event.
    for (held, kept) in images(&host_dir, &hub_dir) {
        assert_eq!(held, Some(kept));
    }
    // The host keeps working while the hub is down.
    host.switch_model("s1", "while-down").await;
    assert_gap_free(&hub_dir);

    let hub = Hub::start(&hub_dir, addr.port()).await;
    caught_up(&host_dir, &hub_dir).await;
    assert_gap_free(&hub_dir);
    let images = images(&host_dir, &hub_dir);
    assert_eq!(images.len(), 2 * 10 * 5);
    for (held, kept) in images {
        assert_eq!(held, Some(kept));
    }
    host.runtime.kill().await;
    hub.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_that_was_offline_catches_up_without_pairing_again() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 2, 10);
    let hub = Hub::start(&hub_dir, 0).await;
    let host = HostDaemon::start(&host_dir, hub.config()).await;
    caught_up(&host_dir, &hub_dir).await;
    host.runtime.kill().await;

    // Offline: the journal grows, including a new session, and the code is used up.
    seed(&host_dir, 3, 20);
    let config = HubConfig {
        pairing_code: None,
        ..hub.config()
    };
    let host = HostDaemon::start(&host_dir, config).await;
    caught_up(&host_dir, &hub_dir).await;
    assert_gap_free(&hub_dir);
    assert_eq!(hub.auth.devices().len(), 1);
    host.runtime.kill().await;
    hub.runtime.kill().await;
}

/// The hub as the paired client sees it once `ready` holds, waiting for it.
async fn machine_when(client: &Client, ready: impl Fn(&Machine) -> bool) -> Machine {
    let changes = client.changes();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        if let Some(machine) = client.machines().into_iter().find(&ready) {
            return machine;
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            tokio::time::timeout(left, changes.next()).await.is_ok(),
            "the client never saw it: {:?}",
            client.machines()
        );
    }
}

/// The next `count` events the subscription delivers; an update may carry none.
async fn events(sub: &SessionSubscription, count: usize) -> Vec<Event> {
    let mut events = Vec::new();
    while events.len() < count {
        let update = tokio::time::timeout(TIMEOUT, sub.next())
            .await
            .unwrap()
            .unwrap();
        events.extend(update.events);
    }
    events
}

/// The seqs of the next `count` events the subscription delivers.
async fn seqs(sub: &SessionSubscription, count: usize) -> Vec<u64> {
    let events = events(sub, count).await;
    events.iter().map(|event| event.seq).collect()
}

/// The refusal of a prompt to `session` sent through the hub.
async fn refusal(client: &Client, hub: &HostId, session: &str) -> herder_protocol::ErrorInfo {
    let command = CommandBody::SendPrompt {
        session_id: SessionId::new(session),
        text: "hi".into(),
        images: Vec::new(),
        files: Vec::new(),
    };
    match client.send(hub.clone(), command).await {
        Err(Error::Rejected { info: error }) => error,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_of_the_hub_sees_host_sessions_read_only() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 2, 3);
    let hub = Hub::start(&hub_dir, 0).await;
    let host = HostDaemon::start(&host_dir, hub.config()).await;
    caught_up(&host_dir, &hub_dir).await;

    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    let (passkey, paired) = hub.join(&client).await;
    let hub_id = paired.host_id;
    assert_eq!(hub_id.as_str(), "hub");
    let machine = machine_when(&client, |m| m.sessions.len() == 2).await;
    assert_eq!(machine.hosts.len(), 1);
    let devbox = &machine.hosts[0];
    assert_eq!(
        (
            devbox.host_id.as_str(),
            devbox.host_name.as_str(),
            devbox.online
        ),
        ("host-1", "devbox", true)
    );
    let s1 = &machine.sessions[0];
    assert_eq!(s1.session_id.as_str(), "s1");
    assert_eq!(s1.host_id, Some(host_id()));
    assert_eq!(s1.head_seq, 4);
    assert_eq!(s1.status, SessionStatus::Idle);
    assert_eq!(s1.account_id.as_str(), "main");
    assert_eq!(
        s1.project_id.as_ref().map(|p| p.as_str()),
        Some("host-1:/home/dev/herder")
    );

    // The transcript replays, then follows the host live.
    let sub = client
        .subscribe_session(hub_id.clone(), SessionId::new("s1"))
        .unwrap();
    assert_eq!(seqs(&sub, 4).await, [1, 2, 3, 4]);
    host.switch_model("s1", "live").await;
    let live = events(&sub, 1).await;
    assert_eq!(live.len(), 1);
    assert_eq!(
        live[0].body,
        EventBody::ModelSwitched {
            model: "live".into()
        }
    );

    // Mutating commands are refused, naming the owning host; so are edits to its queue.
    let session = SessionId::new("s1");
    let prompt_id = herder_protocol::PromptId::new("p1");
    for edit in [
        CommandBody::RemoveQueued {
            session_id: session.clone(),
            prompt_id: prompt_id.clone(),
        },
        CommandBody::MoveQueued {
            session_id: session.clone(),
            prompt_id: prompt_id.clone(),
            before: None,
        },
        CommandBody::SendQueuedNow {
            session_id: session.clone(),
            prompt_id: prompt_id.clone(),
        },
        CommandBody::MergeQueued {
            session_id: session.clone(),
            prompt_ids: vec![prompt_id.clone(), herder_protocol::PromptId::new("p2")],
        },
    ] {
        match client.send(hub_id.clone(), edit).await {
            Err(Error::Rejected { info }) => assert_eq!(info.code, ErrorCode::ReadOnly),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
    let error = refusal(&client, &hub_id, "s1").await;
    assert_eq!(error.code, ErrorCode::ReadOnly);
    assert!(error.message.contains("read-only"), "{}", error.message);
    assert!(
        error.message.contains("devbox (host-1)"),
        "{}",
        error.message
    );
    assert!(
        error.message.ends_with("which is online"),
        "{}",
        error.message
    );
    let archive = CommandBody::ArchiveSession {
        session_id: SessionId::new("s2"),
    };
    assert!(matches!(
        client.send(hub_id.clone(), archive).await,
        Err(Error::Rejected { info: error }) if error.code == ErrorCode::ReadOnly
    ));
    // A client of the hub invites someone, with the hub's own address.
    let Ok(CommandResult::PasskeyChallenge { challenge, .. }) = client
        .send(hub_id.clone(), CommandBody::PasskeyChallenge)
        .await
    else {
        panic!("expected a passkey challenge");
    };
    let invite = CommandBody::Invite {
        role: herder_protocol::Role::Member,
        assertion: Box::new(passkey.assert(&challenge)),
    };
    let Ok(CommandResult::Invite {
        addresses,
        fingerprint,
        ..
    }) = client.send(hub_id.clone(), invite).await
    else {
        panic!("expected an invite");
    };
    assert_eq!(
        (addresses, fingerprint),
        (vec![hub.addr.to_string()], hub.fingerprint.clone())
    );
    let missing = refusal(&client, &hub_id, "nope").await;
    assert_eq!(missing.code, ErrorCode::NotFound);

    // The host stops: its sessions stay listed and readable, and are shown offline.
    host.runtime.kill().await;
    let machine = machine_when(&client, |m| m.hosts.iter().all(|h| !h.online)).await;
    assert_eq!(machine.hosts.len(), 1);
    assert!(
        machine
            .sessions
            .iter()
            .all(|s| s.host_id == Some(host_id()))
    );
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let error = refusal(&client, &hub_id, "s1").await;
        if error.message.contains("which is offline, last seen ") {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{}", error.message);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(client.machines()[0].sessions.len(), 2);
    let sub = client
        .subscribe_session(hub_id.clone(), SessionId::new("s2"))
        .unwrap();
    assert_eq!(seqs(&sub, 4).await, [1, 2, 3, 4]);
    hub.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_of_the_hub_sees_what_it_holds_of_each_host() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 2, 3);
    let hub = Hub::start(&hub_dir, 0).await;
    let host = HostDaemon::start(&host_dir, hub.config()).await;
    caught_up(&host_dir, &hub_dir).await;

    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    hub.join(&client).await;
    let machine = machine_when(&client, |m| m.hub.as_ref().is_some_and(|v| v.events == 8)).await;
    let status = machine.hub.unwrap();
    assert_eq!((status.sessions, status.events), (2, 8));
    assert!(status.storage_bytes > 0);
    assert_eq!(status.hosts.len(), 1);
    let devbox = &status.hosts[0];
    assert_eq!(devbox.host_id, host_id());
    assert_eq!((devbox.sessions, devbox.events), (2, 8));
    let newest = journal(&host_dir)
        .values()
        .flat_map(|records| records.iter().map(|record| record.at))
        .max();
    assert_eq!(devbox.last_event_at, newest);
    assert!(devbox.lag_ms.is_some());

    // It follows what the host replicates live.
    host.switch_model("s1", "live").await;
    let machine = machine_when(&client, |m| m.hub.as_ref().is_some_and(|v| v.events == 9)).await;
    let status = machine.hub.unwrap();
    assert_eq!(status.hosts[0].events, 9);
    let lag = status.hosts[0].lag_ms.unwrap();
    assert!(lag < TIMEOUT.as_millis() as u64, "{lag}");

    // A disconnected hub's status is dropped: it is live only.
    hub.runtime.kill().await;
    machine_when(&client, |m| m.hub.is_none()).await;
    host.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_host_is_offline_after_the_liveness_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 1, 0);
    let liveness = Duration::from_millis(500);
    let hub = Hub::start_with(&hub_dir, 0, liveness, None, None).await;
    // A real host replicates the session, then stops for good.
    let host = HostDaemon::start(&host_dir, hub.config()).await;
    caught_up(&host_dir, &hub_dir).await;
    host.runtime.kill().await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    let hub_id = hub.join(&client).await.1.host_id;

    // The host's device comes back but falls silent after its hello, as a host whose machine
    // hangs or whose network drops without closing the connection would.
    let mut ws = dial(&hub, &host_dir).await;
    let hello = HostMessage::Hello(HostHello {
        replication_version: REPLICATION_VERSION,
        host_id: host_id(),
        host_name: "devbox".into(),
        build: "test".into(),
        pairing_code: None,
        attachments_cap: None,
        relay: false,
    });
    let text = serde_json::to_string(&hello).unwrap();
    ws.send(Message::text(text)).await.unwrap();
    let Some(Ok(Message::Text(reply))) = ws.next().await else {
        panic!("no hello from the hub");
    };
    assert!(matches!(
        serde_json::from_str(&reply).unwrap(),
        HubMessage::Hello(_)
    ));
    assert!(
        refusal(&client, &hub_id, "s1")
            .await
            .message
            .ends_with("which is online")
    );

    machine_when(&client, |m| m.hosts.iter().any(|h| h.online)).await;

    // Silent from now on, without closing: offline once the timeout passes.
    machine_when(&client, |m| m.hosts.iter().all(|h| !h.online)).await;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let error = refusal(&client, &hub_id, "s1").await;
        if error.message.contains("which is offline") {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{}", error.message);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop(ws);
    hub.runtime.kill().await;
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<TcpStream>>;

/// A WebSocket to the hub with the device key of the host in `host_dir`.
async fn dial(hub: &Hub, host_dir: &Path) -> Ws {
    dial_as(hub, &Replicator::device_key(host_dir).unwrap()).await
}

/// A WebSocket to the hub with the key `device`.
async fn dial_as(hub: &Hub, device: &DeviceKey) -> Ws {
    let config = client_config(&hub.fingerprint, device).unwrap();
    let tcp = TcpStream::connect(hub.addr).await.unwrap();
    let tls = TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from("herder").unwrap(), tcp)
        .await
        .unwrap();
    let request = format!("wss://{}/", hub.addr)
        .into_client_request()
        .unwrap();
    tokio_tungstenite::client_async(request, tls)
        .await
        .unwrap()
        .0
}

/// Every text message the hub sends on `ws` until it closes the connection.
async fn until_closed(ws: &mut Ws) -> Vec<String> {
    let mut texts = Vec::new();
    loop {
        match tokio::time::timeout(TIMEOUT, ws.next()).await {
            Err(_) => panic!("the hub kept the connection open; sent {texts:?}"),
            Ok(Some(Ok(Message::Text(text)))) => texts.push(text.as_str().to_owned()),
            Ok(Some(Ok(Message::Close(_))) | None | Some(Err(_))) => return texts,
            Ok(Some(Ok(_))) => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_device_replicates_and_resumes_but_reads_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 2, 3);
    let hub = Hub::start(&hub_dir, 0).await;
    let host = HostDaemon::start(&host_dir, hub.config()).await;
    caught_up(&host_dir, &hub_dir).await;
    let devices = hub.auth.devices();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].0.role, DeviceRole::Host);
    host.runtime.kill().await;

    // Its key, stolen, speaks the client protocol, as `herder fork` reads the hub too:
    // every read is refused at the hello, before the hub sends a host list, a session list,
    // a journal or the secrets.
    let mut ws = dial(&hub, &host_dir).await;
    let hello = ClientMessage::Hello(ClientHello {
        protocol_version: PROTOCOL_VERSION,
        client: "thief".into(),
        resume: vec![Cursor {
            session_id: SessionId::new("s1"),
            after_seq: 0,
        }],
        pairing_code: None,
        sign_in: false,
        passkey: false,
    });
    let reads = [
        hello,
        ClientMessage::Subscribe(Cursor {
            session_id: SessionId::new("s2"),
            after_seq: 0,
        }),
        ClientMessage::Command(Command {
            id: CommandId::new("c1"),
            body: CommandBody::GetAttachment {
                session_id: SessionId::new("s1"),
                attachment_id: AttachmentId::new("a1"),
            },
        }),
        ClientMessage::Command(Command {
            id: CommandId::new("c2"),
            body: CommandBody::ListSecrets,
        }),
        ClientMessage::Sync {
            token: "read".into(),
        },
    ];
    for message in reads {
        // The hub may close before the last ones arrive.
        let _ = ws
            .send(Message::text(serde_json::to_string(&message).unwrap()))
            .await;
    }
    let sent = until_closed(&mut ws).await;
    let [only] = sent.as_slice() else {
        panic!("the hub sent more than a refusal: {sent:?}");
    };
    let ServerMessage::Error { error, .. } = serde_json::from_str(only).unwrap() else {
        panic!("expected a refusal: {only}");
    };
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert!(
        error
            .message
            .contains("only backs its sessions up to the hub"),
        "{}",
        error.message
    );

    // As a host it may still only be itself, and sees only its own cursors.
    let mut ws = dial(&hub, &host_dir).await;
    let hello = HostMessage::Hello(HostHello {
        replication_version: REPLICATION_VERSION,
        host_id: HostId::new("macbook"),
        host_name: "macbook".into(),
        build: "test".into(),
        pairing_code: None,
        attachments_cap: None,
        relay: false,
    });
    ws.send(Message::text(serde_json::to_string(&hello).unwrap()))
        .await
        .unwrap();
    let sent = until_closed(&mut ws).await;
    assert!(
        matches!(
            serde_json::from_str(&sent[0]).unwrap(),
            HubMessage::Error { error } if error.code == ReplicationErrorCode::Forbidden
        ),
        "{sent:?}"
    );

    // Its own host resumes from the hub's cursors.
    seed(&host_dir, 3, 5);
    let config = HubConfig {
        pairing_code: None,
        ..hub.config()
    };
    let host = HostDaemon::start(&host_dir, config).await;
    caught_up(&host_dir, &hub_dir).await;
    assert_gap_free(&hub_dir);
    host.runtime.kill().await;

    // A client device reads everything, as before.
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    hub.join(&client).await;
    let machine = machine_when(&client, |m| m.sessions.len() == 3).await;
    assert_eq!(machine.hosts.len(), 1);
    hub.runtime.kill().await;
}

/// The image of `session` with bytes `data`, as a host sends it.
fn image_message(session: &str, id: &str, data: &[u8]) -> HostMessage {
    HostMessage::Attachment(herder_protocol::AttachmentData {
        session_id: SessionId::new(session),
        attachment: Attachment {
            attachment_id: AttachmentId::new(id),
            media_type: "image/png".into(),
            size: data.len() as u64,
            name: None,
        },
        data: herder_protocol::Bytes(data.to_vec()),
    })
}

/// The next message the hub sends on `ws`.
async fn next_message(ws: &mut Ws) -> HubMessage {
    loop {
        match tokio::time::timeout(TIMEOUT, ws.next()).await.unwrap() {
            Some(Ok(Message::Text(text))) => return serde_json::from_str(&text).unwrap(),
            Some(Ok(Message::Close(_))) | None => panic!("the hub closed the connection"),
            Some(Ok(_)) => {}
            Some(Err(err)) => panic!("{err}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn images_are_not_backed_up_unless_the_host_says_so_and_never_fail_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 2, 3);
    seed_images(&host_dir, 2, 2);
    let hub = Hub::start(&hub_dir, 0).await;
    let host = HostDaemon::start(&host_dir, hub.config()).await;
    caught_up(&host_dir, &hub_dir).await;
    // Off by default: the journals name four images, and the hub holds none.
    let images = images(&host_dir, &hub_dir);
    assert_eq!(images.len(), 4);
    assert!(images.iter().all(|(held, _)| held.is_none()));
    host.runtime.kill().await;
    // A client asking for one is told it was not backed up.
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    let hub_id = hub.join(&client).await.1.host_id;
    let fetch = CommandBody::GetAttachment {
        session_id: SessionId::new("s1"),
        attachment_id: AttachmentId::new("img1x0"),
    };
    let Err(Error::Rejected { info }) = client.send(hub_id, fetch).await else {
        panic!("expected a refusal");
    };
    assert_eq!(info.code, ErrorCode::NotFound);
    assert!(
        info.message
            .starts_with(herder_protocol::IMAGE_NOT_BACKED_UP),
        "{}",
        info.message
    );

    // A host that sends images anyway, or one bigger than its cap, is not failed for it: the
    // image is dropped, and the batch after it acknowledged.
    for (cap, size, kept) in [
        (None, 20, false),
        (Some(64), 100, false),
        (Some(64), 20, true),
    ] {
        let mut ws = dial(&hub, &host_dir).await;
        let hello = HostMessage::Hello(HostHello {
            replication_version: REPLICATION_VERSION,
            host_id: host_id(),
            host_name: "devbox".into(),
            build: "test".into(),
            pairing_code: None,
            attachments_cap: cap,
            relay: false,
        });
        ws.send(Message::text(serde_json::to_string(&hello).unwrap()))
            .await
            .unwrap();
        assert!(matches!(next_message(&mut ws).await, HubMessage::Hello(_)));
        let mut data = PNG.to_vec();
        data.resize(size, b'x');
        let image = image_message("s1", &format!("sent{size}"), &data);
        ws.send(Message::text(serde_json::to_string(&image).unwrap()))
            .await
            .unwrap();
        let next = journal(&host_dir)[&SessionId::new("s1")].clone();
        let batch = HostMessage::Batch(herder_protocol::Batch {
            session_id: SessionId::new("s1"),
            events: next,
        });
        ws.send(Message::text(serde_json::to_string(&batch).unwrap()))
            .await
            .unwrap();
        let HubMessage::Ack(cursor) = next_message(&mut ws).await else {
            panic!("expected an ack");
        };
        assert_eq!(cursor.session_id.as_str(), "s1");
        let held = HubStore::open(hub_dir.join("hub.db"))
            .unwrap()
            .attachment(
                &host_id(),
                &SessionId::new("s1"),
                &AttachmentId::new(format!("sent{size}")),
            )
            .unwrap();
        assert_eq!(held.is_some(), kept, "cap {cap:?}, {size} bytes");
    }
    hub.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hosts_images_stay_within_its_cap_and_the_hosts_list_shows_usage() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 2, 1);
    // Ten images of 14 bytes each, the oldest first; room for three.
    seed_images(&host_dir, 2, 5);
    let hub = Hub::start(&hub_dir, 0).await;
    let config = HubConfig {
        attachments: true,
        attachments_cap: 3 * 14,
        ..hub.config()
    };
    let host = HostDaemon::start(&host_dir, config).await;
    caught_up(&host_dir, &hub_dir).await;
    let held: Vec<bool> = images(&host_dir, &hub_dir)
        .into_iter()
        .map(|(held, kept)| {
            assert_eq!(kept.len(), 14);
            held.is_some()
        })
        .collect();
    let mut newest = vec![false; 7];
    newest.extend([true; 3]);
    assert_eq!(held, newest);

    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    hub.join(&client).await;
    let machine = machine_when(&client, |m| {
        m.hosts
            .first()
            .and_then(|host| host.usage.as_ref())
            .is_some_and(|usage| usage.attachment_bytes == 3 * 14)
    })
    .await;
    assert_eq!(
        machine.hosts[0].usage,
        Some(herder_protocol::HostUsage {
            sessions: 2,
            attachment_bytes: 3 * 14,
            attachments_cap: Some(3 * 14),
        })
    );
    host.runtime.kill().await;
    hub.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_forgotten_host_leaves_nothing_on_the_hub() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 2, 3);
    let hub = Hub::start(&hub_dir, 0).await;
    let host = HostDaemon::start(&host_dir, hub.config()).await;
    caught_up(&host_dir, &hub_dir).await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    hub.join(&client).await;
    machine_when(&client, |m| m.sessions.len() == 2).await;

    // Never while it is online.
    let refused = hub.admin.forget_host("devbox").await.unwrap_err();
    assert!(format!("{refused:#}").contains("online"), "{refused:#}");
    host.runtime.kill().await;
    machine_when(&client, |m| m.hosts.iter().all(|h| !h.online)).await;
    let missing = hub.admin.forget_host("laptop").await.unwrap_err();
    assert!(
        format!("{missing:#}").contains("no host laptop"),
        "{missing:#}"
    );

    let forgot = hub.admin.forget_host("devbox").await.unwrap();
    assert_eq!(
        (forgot.host_id, forgot.sessions, forgot.devices),
        (host_id(), 2, 1)
    );
    assert!(Hub::held(&hub_dir).is_empty());
    // Its device is unpaired; the person's stays.
    assert!(hub.auth.devices().is_empty());
    assert_eq!(hub.auth.people()[0].devices.len(), 1);
    machine_when(&client, |m| m.hosts.is_empty() && m.sessions.is_empty()).await;
    hub.runtime.kill().await;
}

/// Runs a secret command on the hub as the signed-in `client`, with a fresh assertion of
/// its person's `passkey` where the command carries one.
async fn secret_command(
    client: &Client,
    passkey: &Authenticator,
    mut command: CommandBody,
) -> Result<CommandResult, ErrorCode> {
    let hub = HostId::new("hub");
    if let CommandBody::CreateSecretSet { assertion, .. }
    | CommandBody::DeleteSecretSet { assertion, .. }
    | CommandBody::PutSecret { assertion, .. }
    | CommandBody::DeleteSecret { assertion, .. }
    | CommandBody::GrantSecretSet { assertion, .. }
    | CommandBody::RevokeSecretSet { assertion, .. } = &mut command
    {
        let Ok(CommandResult::PasskeyChallenge { challenge, .. }) = client
            .send(hub.clone(), CommandBody::PasskeyChallenge)
            .await
        else {
            panic!("expected a passkey challenge");
        };
        *assertion = Some(Box::new(passkey.assert(&challenge)));
    }
    match client.send(hub, command).await {
        Ok(result) => Ok(result),
        Err(Error::Rejected { info }) => Err(info.code),
        Err(other) => panic!("the command failed: {other:?}"),
    }
}

fn secret(text: &str) -> SecretRef {
    text.parse().unwrap()
}

/// What each value fetched for `session` is, by set and name.
async fn fetch(
    host: &HostDaemon,
    session: &str,
    secrets: &[&str],
) -> Result<Vec<(String, String)>, SecretError> {
    let secrets: Vec<SecretRef> = secrets.iter().map(|text| secret(text)).collect();
    let fetched = host
        .secrets
        .fetch(&SessionId::new(session), &secrets)
        .await?;
    Ok(fetched
        .into_iter()
        .map(|value| {
            (
                format!("{}/{}", value.set, value.secret.name),
                value.value.expose().to_owned(),
            )
        })
        .collect())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_fetches_only_the_secrets_granted_to_it_and_each_fetch_is_audited() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 1, 1);
    let hub = Hub::start(&hub_dir, 0).await;
    let host = HostDaemon::start(&host_dir, hub.config()).await;
    caught_up(&host_dir, &hub_dir).await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    let (passkey, _) = hub.join(&client).await;

    for set in ["prod", "dev", "other"] {
        let created = CommandBody::CreateSecretSet {
            set: set.into(),
            assertion: None,
        };
        assert_eq!(
            secret_command(&client, &passkey, created).await,
            Ok(CommandResult::Applied)
        );
    }
    let again = CommandBody::CreateSecretSet {
        set: "prod".into(),
        assertion: None,
    };
    assert_eq!(
        secret_command(&client, &passkey, again).await,
        Err(ErrorCode::Conflict)
    );
    let bad = CommandBody::CreateSecretSet {
        set: "Prod".into(),
        assertion: None,
    };
    assert_eq!(
        secret_command(&client, &passkey, bad).await,
        Err(ErrorCode::BadRequest)
    );
    for (set, name, value) in [
        ("prod", "TOKEN", "prod-token-value"),
        ("dev", "TOKEN", "dev-token-value"),
        ("prod", "STRIPE_KEY", "stripe-key-value"),
        ("other", "HIDDEN", "hidden-value"),
    ] {
        let put = CommandBody::PutSecret {
            set: set.into(),
            name: name.into(),
            description: format!("{name} of {set}"),
            value: SecretValue::new(value).unwrap(),
            assertion: None,
        };
        assert_eq!(
            secret_command(&client, &passkey, put).await,
            Ok(CommandResult::Applied)
        );
    }
    let nowhere = CommandBody::PutSecret {
        set: "nowhere".into(),
        name: "TOKEN".into(),
        description: "x".into(),
        value: SecretValue::new("x").unwrap(),
        assertion: None,
    };
    assert_eq!(
        secret_command(&client, &passkey, nowhere).await,
        Err(ErrorCode::NotFound)
    );
    for set in ["prod", "dev"] {
        let grant = CommandBody::GrantSecretSet {
            set: set.into(),
            host_id: host_id(),
            host_use: false,
            assertion: None,
        };
        assert_eq!(
            secret_command(&client, &passkey, grant).await,
            Ok(CommandResult::Applied)
        );
    }
    let stranger = CommandBody::GrantSecretSet {
        set: "other".into(),
        host_id: HostId::new("stranger"),
        host_use: false,
        assertion: None,
    };
    assert_eq!(
        secret_command(&client, &passkey, stranger).await,
        Err(ErrorCode::NotFound)
    );

    // The owner sees every set and secret, never a value.
    let listed = secret_command(&client, &passkey, CommandBody::ListSecrets)
        .await
        .unwrap();
    let CommandResult::Secrets { sets, secrets } = &listed else {
        panic!("expected secrets: {listed:?}");
    };
    let granted: Vec<_> = sets
        .iter()
        .map(|set| (set.name.as_str(), set.hosts.len()))
        .collect();
    assert_eq!(granted, [("dev", 1), ("other", 0), ("prod", 1)]);
    assert_eq!(secrets.len(), 4);
    let wire = serde_json::to_string(&listed).unwrap();
    assert!(!wire.contains("-value"), "{wire}");

    // The host lists and fetches what its sets hold, and nothing else.
    let names: Vec<_> = host
        .secrets
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|secret| format!("{}/{}", secret.set, secret.name))
        .collect();
    assert_eq!(names, ["dev/TOKEN", "prod/STRIPE_KEY", "prod/TOKEN"]);
    assert_eq!(
        fetch(&host, "s1", &["STRIPE_KEY", "prod/TOKEN"])
            .await
            .unwrap(),
        [
            ("prod/STRIPE_KEY".to_owned(), "stripe-key-value".to_owned()),
            ("prod/TOKEN".to_owned(), "prod-token-value".to_owned())
        ]
    );
    let ambiguous = fetch(&host, "s1", &["TOKEN"]).await.unwrap_err();
    assert_eq!(ambiguous.code, SecretErrorCode::Ambiguous);
    assert_eq!(
        ambiguous.candidates,
        [secret("dev/TOKEN"), secret("prod/TOKEN")]
    );
    let not_granted = fetch(&host, "s1", &["other/HIDDEN"]).await.unwrap_err();
    assert_eq!(not_granted.code, SecretErrorCode::NotGranted);
    assert_eq!(not_granted.secret, Some(secret("other/HIDDEN")));
    for unknown in ["HIDDEN", "nowhere/TOKEN", "prod/NOPE"] {
        let error = fetch(&host, "s1", &[unknown]).await.unwrap_err();
        assert_eq!(error.code, SecretErrorCode::Unknown, "{unknown}");
    }
    // One secret refused refuses them all, and none is handed out.
    let mixed = fetch(&host, "s2", &["dev/TOKEN", "other/HIDDEN"]).await;
    assert_eq!(mixed.unwrap_err().code, SecretErrorCode::NotGranted);

    // A revoked set is out of reach for any session not holding its values yet.
    let revoke = CommandBody::RevokeSecretSet {
        set: "prod".into(),
        host_id: host_id(),
        host_use: false,
        assertion: None,
    };
    assert_eq!(
        secret_command(&client, &passkey, revoke).await,
        Ok(CommandResult::Applied)
    );
    let revoked = fetch(&host, "s3", &["prod/TOKEN"]).await.unwrap_err();
    assert_eq!(revoked.code, SecretErrorCode::NotGranted);
    assert_eq!(
        fetch(&host, "s3", &["TOKEN"]).await.unwrap(),
        [("dev/TOKEN".to_owned(), "dev-token-value".to_owned())]
    );

    // One audit row per value handed out, newest first, without the value.
    let audit = secret_command(
        &client,
        &passkey,
        CommandBody::ListSecretAudit { before: None },
    )
    .await
    .unwrap();
    let CommandResult::SecretAudit { entries } = &audit else {
        panic!("expected the audit: {audit:?}");
    };
    let rows: Vec<_> = entries
        .iter()
        .map(|entry| {
            (
                entry.host_id.as_str(),
                entry.session_id.as_ref().unwrap().as_str(),
                format!("{}/{}", entry.set, entry.name),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("host-1", "s3", "dev/TOKEN".to_owned()),
            ("host-1", "s1", "prod/TOKEN".to_owned()),
            ("host-1", "s1", "prod/STRIPE_KEY".to_owned()),
        ]
    );
    let older = CommandBody::ListSecretAudit {
        before: Some(entries[1].id),
    };
    let CommandResult::SecretAudit { entries: older } =
        secret_command(&client, &passkey, older).await.unwrap()
    else {
        panic!("expected the audit");
    };
    assert_eq!(older.len(), 1);
    assert_eq!(older[0].name, "STRIPE_KEY");
    let wire = serde_json::to_string(&audit).unwrap();
    assert!(!wire.contains("-value"), "{wire}");

    // Sets go once they are empty; deleting a secret takes it out of the host's reach.
    let busy = CommandBody::DeleteSecretSet {
        set: "dev".into(),
        assertion: None,
    };
    assert_eq!(
        secret_command(&client, &passkey, busy).await,
        Err(ErrorCode::Conflict)
    );
    let delete = CommandBody::DeleteSecret {
        set: "dev".into(),
        name: "TOKEN".into(),
        assertion: None,
    };
    assert_eq!(
        secret_command(&client, &passkey, delete).await,
        Ok(CommandResult::Applied)
    );
    let gone = fetch(&host, "s4", &["TOKEN"]).await.unwrap_err();
    assert_eq!(gone.code, SecretErrorCode::Unknown);
    let delete = CommandBody::DeleteSecretSet {
        set: "dev".into(),
        assertion: None,
    };
    assert_eq!(
        secret_command(&client, &passkey, delete).await,
        Ok(CommandResult::Applied)
    );
    host.runtime.kill().await;
    hub.runtime.kill().await;
}

/// Waits until what `held` holds is `wanted`.
async fn held_until(held: &HeldSecrets, wanted: impl Fn(&Held) -> bool) -> Held {
    let mut watch = held.watch();
    let reached = tokio::time::timeout(TIMEOUT, watch.wait_for(|held| wanted(held))).await;
    match reached {
        Ok(Ok(held)) => held.clone(),
        _ => panic!("still holding {:?}", held.current()),
    }
}

fn holds(held: &Held, value: &str) -> bool {
    matches!(held, Held::Ok(values) if values[0].value.expose() == value)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_holds_secrets_for_its_own_use_until_revoked_or_its_hub_is_down() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 1, 1);
    let hub = Hub::start(&hub_dir, 0).await;
    let secrets = HostSecrets::with_ttl(Duration::from_millis(300));
    let host = HostDaemon::start_with(&host_dir, hub.config(), secrets, Adapters::new()).await;
    caught_up(&host_dir, &hub_dir).await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    let (passkey, _) = hub.join(&client).await;
    for (set, name, value) in [
        ("bots", "SLACK_TOKEN", "slack-v1"),
        ("mail", "SMTP_PASSWORD", "smtp-value"),
    ] {
        let created = CommandBody::CreateSecretSet {
            set: set.into(),
            assertion: None,
        };
        assert_eq!(
            secret_command(&client, &passkey, created).await,
            Ok(CommandResult::Applied)
        );
        let put = CommandBody::PutSecret {
            set: set.into(),
            name: name.into(),
            description: format!("{name} of {set}"),
            value: SecretValue::new(value).unwrap(),
            assertion: None,
        };
        assert_eq!(
            secret_command(&client, &passkey, put).await,
            Ok(CommandResult::Applied)
        );
    }

    // Not granted for host use yet: denied.
    let slack = host
        .secrets
        .hold("bot:trip1-team".into(), vec![secret("SLACK_TOKEN")]);
    let denied = held_until(&slack, |held| matches!(held, Held::Denied(_))).await;
    assert_eq!(
        denied.state(),
        HostSecretsState::Denied {
            error: SecretError {
                code: SecretErrorCode::Unknown,
                message: "no set granted to this host for host use has SLACK_TOKEN; ask the \
                          hub's owner to grant its set for host use"
                    .into(),
                secret: Some(secret("SLACK_TOKEN")),
                candidates: Vec::new(),
            }
        }
    );
    for set in ["bots", "mail"] {
        let grant = CommandBody::GrantSecretSet {
            set: set.into(),
            host_id: host_id(),
            host_use: true,
            assertion: None,
        };
        assert_eq!(
            secret_command(&client, &passkey, grant).await,
            Ok(CommandResult::Applied)
        );
    }
    let listed = secret_command(&client, &passkey, CommandBody::ListSecrets)
        .await
        .unwrap();
    let CommandResult::Secrets { sets, .. } = &listed else {
        panic!("expected secrets");
    };
    let grants: Vec<_> = sets
        .iter()
        .map(|set| (set.name.as_str(), set.hosts.len(), set.host_use.clone()))
        .collect();
    assert_eq!(
        grants,
        [("bots", 0, vec![host_id()]), ("mail", 0, vec![host_id()])]
    );

    // Granted, the host reads them with no session, and asks again on its own.
    held_until(&slack, |held| holds(held, "slack-v1")).await;
    let mail = host
        .secrets
        .hold("mail:alerts".into(), vec![secret("SMTP_PASSWORD")]);
    held_until(&mail, |held| holds(held, "smtp-value")).await;
    assert_eq!(mail.current().state(), HostSecretsState::Ok);

    // Its sessions get none of them.
    let unknown = fetch(&host, "s1", &["SLACK_TOKEN"]).await.unwrap_err();
    assert_eq!(unknown.code, SecretErrorCode::Unknown);
    let not_granted = fetch(&host, "s1", &["bots/SLACK_TOKEN"]).await.unwrap_err();
    assert_eq!(not_granted.code, SecretErrorCode::NotGranted);
    assert!(host.secrets.list().await.unwrap().is_empty());

    // The audit shows the host's use, under its purpose, with no session.
    let audit = secret_command(
        &client,
        &passkey,
        CommandBody::ListSecretAudit { before: None },
    )
    .await
    .unwrap();
    let CommandResult::SecretAudit { entries } = &audit else {
        panic!("expected the audit");
    };
    let slack_reads = entries
        .iter()
        .filter(|entry| entry.purpose.as_deref() == Some("bot:trip1-team"))
        .inspect(|entry| {
            assert_eq!(entry.host_id, host_id());
            assert_eq!(entry.session_id, None);
            assert_eq!(
                (entry.set.as_str(), entry.name.as_str()),
                ("bots", "SLACK_TOKEN")
            );
        })
        .count();
    assert!(slack_reads >= 1, "no fetch for host use in the audit");
    let wire = serde_json::to_string(&audit).unwrap();
    assert!(!wire.contains("slack-v"), "{wire}");

    // A changed value is picked up.
    let put = CommandBody::PutSecret {
        set: "bots".into(),
        name: "SLACK_TOKEN".into(),
        description: "the bot's token".into(),
        value: SecretValue::new("slack-v2").unwrap(),
        assertion: None,
    };
    assert_eq!(
        secret_command(&client, &passkey, put).await,
        Ok(CommandResult::Applied)
    );
    held_until(&slack, |held| holds(held, "slack-v2")).await;

    // The grant for host use is its own: revoking the sessions' one finds none, and revoking
    // it stops the reads.
    let sessions_grant = CommandBody::RevokeSecretSet {
        set: "bots".into(),
        host_id: host_id(),
        host_use: false,
        assertion: None,
    };
    assert_eq!(
        secret_command(&client, &passkey, sessions_grant).await,
        Err(ErrorCode::NotFound)
    );
    let revoke = CommandBody::RevokeSecretSet {
        set: "bots".into(),
        host_id: host_id(),
        host_use: true,
        assertion: None,
    };
    assert_eq!(
        secret_command(&client, &passkey, revoke).await,
        Ok(CommandResult::Applied)
    );
    let revoked = held_until(&slack, |held| matches!(held, Held::Denied(_))).await;
    let Held::Denied(error) = revoked else {
        unreachable!()
    };
    assert_eq!(error.code, SecretErrorCode::Unknown);
    assert!(holds(&mail.current(), "smtp-value"));

    // A purpose the hub cannot audit gets nothing.
    let bad = host
        .secrets
        .hold("Bot Team".into(), vec![secret("SMTP_PASSWORD")]);
    let refused = held_until(&bad, |held| matches!(held, Held::Denied(_))).await;
    let Held::Denied(error) = refused else {
        unreachable!()
    };
    assert!(error.message.contains("`Bot Team`"), "{}", error.message);

    // The hub down, the host is waiting for it.
    hub.runtime.kill().await;
    held_until(&mail, |held| *held == Held::WaitingForHub).await;
    assert_eq!(mail.current().state(), HostSecretsState::WaitingForHub);
    host.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hub_without_its_key_keeps_working_but_hands_out_no_secrets() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 1, 1);
    let hub = Hub::start(&hub_dir, 0).await;
    let host = HostDaemon::start(&host_dir, hub.config()).await;
    caught_up(&host_dir, &hub_dir).await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    let (passkey, _) = hub.join(&client).await;
    for command in [
        CommandBody::CreateSecretSet {
            set: "prod".into(),
            assertion: None,
        },
        CommandBody::PutSecret {
            set: "prod".into(),
            name: "TOKEN".into(),
            description: "Token".into(),
            value: SecretValue::new("token-value").unwrap(),
            assertion: None,
        },
        CommandBody::GrantSecretSet {
            set: "prod".into(),
            host_id: host_id(),
            host_use: false,
            assertion: None,
        },
    ] {
        assert_eq!(
            secret_command(&client, &passkey, command).await,
            Ok(CommandResult::Applied)
        );
    }
    assert_eq!(fetch(&host, "s1", &["TOKEN"]).await.unwrap().len(), 1);
    host.runtime.kill().await;
    let addr = hub.addr;
    hub.runtime.kill().await;

    // Without a key: hosts replicate as before, secret commands and fetches are refused.
    let hub = Hub::start_with(&hub_dir, addr.port(), LIVENESS_TIMEOUT, None, None).await;
    seed(&host_dir, 1, 3);
    let config = HubConfig {
        pairing_code: None,
        ..hub.config()
    };
    let host = HostDaemon::start(&host_dir, config.clone()).await;
    caught_up(&host_dir, &hub_dir).await;
    let refused = match client
        .send(HostId::new("hub"), CommandBody::ListSecrets)
        .await
    {
        Err(Error::Rejected { info }) => info,
        other => panic!("expected a refusal: {other:?}"),
    };
    assert_eq!(refused.code, ErrorCode::Unsupported);
    assert!(
        refused
            .message
            .starts_with("this hub has no secrets key; set HERDER_HUB_SECRETS_KEY"),
        "{}",
        refused.message
    );
    let error = fetch(&host, "s2", &["TOKEN"]).await.unwrap_err();
    assert!(
        error.message.starts_with("the hub has no secrets key")
            && error.message.contains("HERDER_HUB_SECRETS_KEY"),
        "{}",
        error.message
    );
    host.runtime.kill().await;
    hub.runtime.kill().await;

    // With another key: what was stored under the old one does not open.
    let hub = Hub::start_with(&hub_dir, addr.port(), LIVENESS_TIMEOUT, Some([8; 32]), None).await;
    // New events, so catching up means the host is connected again.
    seed(&host_dir, 1, 5);
    let host = HostDaemon::start(&host_dir, config).await;
    caught_up(&host_dir, &hub_dir).await;
    let error = fetch(&host, "s3", &["TOKEN"]).await.unwrap_err();
    assert!(
        error.message.contains("cannot decrypt"),
        "{}",
        error.message
    );
    host.runtime.kill().await;
    hub.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_backs_up_and_fetches_secrets_only_as_its_switches_say() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 2, 3);
    let hub = Hub::start(&hub_dir, 0).await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    let (passkey, _) = hub.join(&client).await;

    // Backup off: the host connects, so its sessions get secrets, but replicates nothing.
    let config = HubConfig {
        backup: false,
        ..hub.config()
    };
    let host = HostDaemon::start(&host_dir, config).await;
    machine_when(&client, |m| {
        m.hosts.iter().any(|h| h.host_id == host_id() && h.online)
    })
    .await;
    for command in [
        CommandBody::CreateSecretSet {
            set: "prod".into(),
            assertion: None,
        },
        CommandBody::PutSecret {
            set: "prod".into(),
            name: "TOKEN".into(),
            description: "Token".into(),
            value: SecretValue::new("token-value").unwrap(),
            assertion: None,
        },
        CommandBody::GrantSecretSet {
            set: "prod".into(),
            host_id: host_id(),
            host_use: false,
            assertion: None,
        },
    ] {
        assert_eq!(
            secret_command(&client, &passkey, command).await,
            Ok(CommandResult::Applied)
        );
    }
    assert_eq!(
        fetch(&host, "s1", &["TOKEN"]).await.unwrap(),
        [("prod/TOKEN".to_owned(), "token-value".to_owned())]
    );
    // The fetch was answered on the connection after anything a backup would have sent.
    assert!(Hub::held(&hub_dir).is_empty());
    assert!(Hub::summaries(&hub_dir).is_empty());
    host.runtime.kill().await;

    // Secrets off: the host replicates everything, and its sessions get no secrets.
    let config = HubConfig {
        pairing_code: None,
        secrets: false,
        ..hub.config()
    };
    let host = HostDaemon::start(&host_dir, config).await;
    caught_up(&host_dir, &hub_dir).await;
    let refused = fetch(&host, "s2", &["TOKEN"]).await.unwrap_err();
    assert_eq!(refused.code, SecretErrorCode::NoHub);
    host.runtime.kill().await;
    hub.runtime.kill().await;
}

/// A CLIProxyAPI's management API on localhost, keeping the client keys the hub puts there.
fn fake_cliproxyapi() -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
    let (url, keys, _) = fake_cliproxyapi_recording();
    (url, keys)
}

/// The requests a fake CLIProxyAPI served outside its management API: each one's head and body.
type Requests = Arc<std::sync::Mutex<Vec<(String, String)>>>;

/// [`fake_cliproxyapi`], also recording every request outside the management API, head and
/// body, which it answers `{"served":true}`.
fn fake_cliproxyapi_recording() -> (String, Arc<std::sync::Mutex<Vec<String>>>, Requests) {
    use std::io::{BufRead, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let keys = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let held = Arc::clone(&keys);
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let (mut head, mut length) = (String::new(), 0);
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                head.push_str(&line);
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let path = "/v8/management/config/access/api-keys";
            let answer = if !head.contains("/v8/management/") {
                let body = String::from_utf8(body.clone()).unwrap();
                recorded.lock().unwrap().push((head.clone(), body));
                r#"{"served":true}"#.to_owned()
            } else if !head.contains("Bearer management-secret") {
                "{}".to_owned()
            } else if head.starts_with(&format!("PUT {path} ")) {
                *held.lock().unwrap() = serde_json::from_slice(&body).unwrap();
                r#"{"status":"ok"}"#.to_owned()
            } else if head.starts_with("GET /v8/management/credentials ") {
                r#"{"files":[{"name":"claude-ada@example.com.json","provider":"claude","email":"ada@example.com","status":"active"}]}"#.to_owned()
            } else {
                serde_json::to_string(&*held.lock().unwrap()).unwrap()
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{answer}",
                answer.len()
            );
        }
    });
    (url, keys, requests)
}

/// An adapter that records each start's environment, then plays `failover_retry.jsonl`.
struct RecordingEnv(Arc<std::sync::Mutex<Vec<BTreeMap<String, String>>>>);

impl herder_adapters::Adapter for RecordingEnv {
    fn start(&self, request: herder_adapters::StartRequest) -> herder_adapters::StartFuture {
        self.0.lock().unwrap().push(request.env.clone());
        let script =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/session/failover_retry.jsonl");
        herder_adapters::fake::FakeAdapter::new(script).start(request)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paired_host_runs_on_the_hubs_cliproxyapi_with_the_key_it_minted_until_forgotten() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    std::fs::create_dir_all(&host_dir).unwrap();
    let (url, proxy_keys) = fake_cliproxyapi();
    let management = herder_daemon::hub::cliproxyapi::Management::new(
        &url,
        SecretValue::new("management-secret").unwrap(),
    );
    let keys = ClientKeys::new(management, Some("https://cpa.example.com:8317".into())).unwrap();
    let hub = Hub::start_with(&hub_dir, 0, LIVENESS_TIMEOUT, Some(SECRETS_KEY), Some(keys)).await;
    let starts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut adapters = Adapters::new();
    adapters.register(
        Provider::Claude,
        Arc::new(RecordingEnv(Arc::clone(&starts))),
    );
    let host =
        HostDaemon::start_with(&host_dir, hub.config(), HostSecrets::default(), adapters).await;

    // Pairing gives the host a key: in the proxy, and to the host for its own use.
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let minted = loop {
        let held = proxy_keys.lock().unwrap().clone();
        if let [key] = held.as_slice()
            && key.starts_with("herder-")
            && let Ok(fetched) = host
                .secrets
                .fetch_for_host("endpoint:hub", &[secret("CLIPROXYAPI_KEY")])
                .await
        {
            assert_eq!(fetched[0].value.expose(), key);
            break key.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no key minted: {held:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    // The hub said where its proxy is, for a client to add an account on it.
    assert_eq!(
        host.secrets.cliproxyapi().as_deref(),
        Some("https://cpa.example.com:8317")
    );
    // And the host dials that address itself, as before the hub forwarded to any proxy.
    assert!(matches!(
        host.secrets.proxy_target(),
        Some(herder_daemon::hub::pin::Target::Url(url)) if url == "https://cpa.example.com:8317"
    ));
    // Its sessions never get it.
    let unknown = fetch(&host, "s1", &["CLIPROXYAPI_KEY"]).await.unwrap_err();
    assert_eq!(unknown.code, SecretErrorCode::Unknown);

    // An endpoint account naming the secret starts its CLI with the key, typed nowhere.
    let account = herder_daemon::session::AccountConfig {
        provider: Provider::Claude,
        label: "Hub".into(),
        config_dir: Some(host_dir.join("hub-account")),
        fallback: false,
        endpoint: Some(herder_daemon::session::Endpoint {
            url: "https://hub.example.com:8317".into(),
            key: herder_daemon::session::EndpointKey::Secret(secret("CLIPROXYAPI_KEY")),
        }),
    };
    assert!(host.sessions.add_account(AccountId::new("hub"), account));
    let repo = host_dir.join("app");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        &["init", "--quiet", "--initial-branch=main"][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@e",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .status()
            .unwrap();
        assert!(status.success());
    }
    let sessions = host.sessions.clone();
    let created = host
        .runtime
        .handle()
        .spawn(async move {
            let alice = UserId::new("alice");
            let create = CommandBody::CreateSession {
                repo: Some(repo.to_str().unwrap().to_owned()),
                project_id: None,
                branch: None,
                account_id: Some(AccountId::new("hub")),
                provider: None,
                model: None,
                permission_mode: Some(PermissionMode::Ask),
                failover_pin: None,
                chat: false,
            };
            let Ok(CommandResult::SessionCreated { session_id }) =
                sessions.handle(alice.clone(), create).await
            else {
                panic!("no session on the hub account");
            };
            let prompt = CommandBody::SendPrompt {
                session_id,
                text: "Refactor the parser.".into(),
                images: Vec::new(),
                files: Vec::new(),
            };
            sessions.handle(alice, prompt).await.unwrap();
        })
        .await;
    created.unwrap();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let env = loop {
        if let Some(env) = starts.lock().unwrap().first().cloned() {
            break env;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the CLI never started"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(
        env.get("ANTHROPIC_BASE_URL").map(String::as_str),
        Some("https://hub.example.com:8317")
    );
    assert_eq!(env.get("ANTHROPIC_AUTH_TOKEN"), Some(&minted));

    // Forgotten, the host's key leaves the proxy: it has no access any more.
    host.runtime.kill().await;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while let Err(err) = hub.admin.forget_host("devbox").await {
        assert!(tokio::time::Instant::now() < deadline, "{err:#}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let held = proxy_keys.lock().unwrap().clone();
    assert_eq!(held.len(), 1, "{held:?}");
    assert_ne!(held[0], minted);
    hub.runtime.kill().await;
}

/// A CLIProxyAPI on localhost holding three Claude logins: ada, used to 42%; carol, to 10%;
/// and bob, fallback-only. It keeps the client keys the hub puts there and the fields it
/// patches, and answers `/v1/messages`, recording each request's model and key.
struct FakeLogins {
    url: String,
    keys: Arc<std::sync::Mutex<Vec<String>>>,
    patched: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    messages: Arc<std::sync::Mutex<Vec<(String, String)>>>,
}

fn fake_cliproxyapi_logins() -> FakeLogins {
    use std::io::{BufRead, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let fake = FakeLogins {
        url,
        keys: Arc::default(),
        patched: Arc::default(),
        messages: Arc::default(),
    };
    let (keys, patched, messages) = (
        Arc::clone(&fake.keys),
        Arc::clone(&fake.patched),
        Arc::clone(&fake.messages),
    );
    let reset = (Timestamp::now().as_second() + 3600).to_string();
    let later = (Timestamp::now().as_second() + 7200).to_string();
    let credentials = serde_json::json!({ "files": [
        { "name": "claude-ada@example.com.json", "provider": "claude", "email": "ada@example.com",
          "status": "active", "note": "Work", "quota": { "signals": {
            "Anthropic-Ratelimit-Unified-5h-Utilization": "0.42",
            "Anthropic-Ratelimit-Unified-5h-Reset": reset } } },
        { "name": "claude-carol@example.com.json", "provider": "claude",
          "email": "carol@example.com", "status": "active", "quota": { "signals": {
            "Anthropic-Ratelimit-Unified-5h-Utilization": "0.10",
            "Anthropic-Ratelimit-Unified-5h-Reset": later } } },
        { "name": "claude-bob@example.com.json", "provider": "claude", "email": "bob@example.com",
          "status": "active", "priority": -1 },
    ]})
    .to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let (mut head, mut length) = (String::new(), 0);
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                head.push_str(&line);
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let managed = head.contains("Bearer management-secret");
            let answer = if head.starts_with("POST /v1/messages") {
                let key = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("authorization: bearer ")
                            .map(str::to_owned)
                    })
                    .unwrap_or_default();
                let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let model = request["model"].as_str().unwrap_or_default().to_owned();
                messages.lock().unwrap().push((model, key));
                r#"{"type":"message","content":[]}"#.to_owned()
            } else if !managed {
                "{}".to_owned()
            } else if head.starts_with("PUT /v8/management/config/access/api-keys ") {
                *keys.lock().unwrap() = serde_json::from_slice(&body).unwrap();
                r#"{"status":"ok"}"#.to_owned()
            } else if head.starts_with("GET /v8/management/credentials ") {
                credentials.clone()
            } else if head.starts_with("PATCH /v8/management/credentials/fields ") {
                patched
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(&body).unwrap());
                r#"{"status":"ok"}"#.to_owned()
            } else {
                serde_json::to_string(&*keys.lock().unwrap()).unwrap()
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{answer}",
                answer.len()
            );
        }
    });
    fake
}

/// An adapter that records each start, then plays the next of its scripts.
struct RecordingScripts {
    starts: Arc<std::sync::Mutex<Vec<herder_adapters::StartRequest>>>,
    scripts: std::sync::Mutex<Vec<std::path::PathBuf>>,
}

impl herder_adapters::Adapter for RecordingScripts {
    fn start(&self, request: herder_adapters::StartRequest) -> herder_adapters::StartFuture {
        self.starts.lock().unwrap().push(request.clone());
        let script = self.scripts.lock().unwrap().remove(0);
        herder_adapters::fake::FakeAdapter::new(script).start(request)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_hub_login_is_an_account_pinned_to_it_that_fails_over_to_the_next() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    std::fs::create_dir_all(&host_dir).unwrap();
    let proxy = fake_cliproxyapi_logins();
    let management = herder_daemon::hub::cliproxyapi::Management::new(
        &proxy.url,
        SecretValue::new("management-secret").unwrap(),
    );
    // Hosts reach the proxy where the hub does.
    let keys = ClientKeys::new(management, Some(proxy.url.clone())).unwrap();
    let hub = Hub::start_with(&hub_dir, 0, LIVENESS_TIMEOUT, Some(SECRETS_KEY), Some(keys)).await;
    // The first turn runs out of its limit; the retry completes.
    let limit = host_dir.join("limit.jsonl");
    std::fs::write(
        &limit,
        concat!(
            r#"{"expect": {"type": "send_prompt", "turn_id": "turn", "text": "Refactor the parser."}}"#, "\n",
            r#"{"emit": {"type": "turn_started", "turn_id": "turn"}}"#, "\n",
            r#"{"emit": {"type": "turn_failed", "turn_id": "turn", "error": {"class": "limit_reached", "message": "5-hour limit reached"}}}"#, "\n",
            r#"{"expect": {"type": "shutdown"}}"#, "\n",
        ),
    )
    .unwrap();
    let retry = host_dir.join("retry.jsonl");
    std::fs::write(
        &retry,
        concat!(
            r#"{"expect": {"type": "send_prompt", "turn_id": "turn", "text": "Refactor the parser."}}"#, "\n",
            r#"{"emit": {"type": "turn_started", "turn_id": "turn"}}"#, "\n",
            r#"{"emit": {"type": "turn_completed", "turn_id": "turn"}}"#, "\n",
        ),
    )
    .unwrap();
    let starts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut adapters = Adapters::new();
    adapters.register(
        Provider::Claude,
        Arc::new(RecordingScripts {
            starts: Arc::clone(&starts),
            scripts: std::sync::Mutex::new(vec![limit, retry]),
        }),
    );
    let host =
        HostDaemon::start_with(&host_dir, hub.config(), HostSecrets::default(), adapters).await;

    // Each login becomes an account, with the hub's label, fallback mark, email and usage.
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let accounts = loop {
        let accounts = host.sessions.accounts();
        if accounts.len() == 3 && !accounts[0].usage.is_empty() {
            break accounts;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the hub's logins did not become accounts with usage"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let ids: Vec<&str> = accounts.iter().map(|a| a.account_id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "vault-claude-ada-example.com",
            "vault-claude-bob-example.com",
            "vault-claude-carol-example.com"
        ]
    );
    let (ada, bob, carol) = (&accounts[0], &accounts[1], &accounts[2]);
    assert_eq!(ada.label, "Work");
    assert_eq!(
        ada.hub_login.as_deref(),
        Some("claude-ada@example.com.json")
    );
    assert_eq!(ada.email.as_deref(), Some("ada@example.com"));
    assert_eq!(ada.usage[0].window, "five_hour");
    assert!((ada.usage[0].used_percent - 42.0).abs() < 1e-9);
    assert_eq!(ada.key_secret, Some(secret("CLIPROXYAPI_KEY")));
    assert!(bob.fallback && !ada.fallback && !carol.fallback);
    assert_eq!(bob.label, "bob@example.com");
    // The hub pinned every login on the proxy.
    let pins: Vec<String> = proxy
        .patched
        .lock()
        .unwrap()
        .iter()
        .map(|patch| patch["prefix"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        pins,
        [
            "claude-ada-example.com",
            "claude-bob-example.com",
            "claude-carol-example.com"
        ]
    );
    // A request through ada's account runs on ada, with the model pinned and the host's key.
    let minted = loop {
        if let [key] = proxy.keys.lock().unwrap().as_slice()
            && key.starts_with("herder-")
        {
            break key.clone();
        }
        assert!(tokio::time::Instant::now() < deadline, "no key minted");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let url = ada.endpoint.clone().unwrap();
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    let key = minted.clone();
    tokio::task::spawn_blocking(move || {
        ureq::post(format!("{url}/v1/messages?beta=true"))
            .header("Authorization", format!("Bearer {key}"))
            .send(r#"{"model":"claude-opus-5-5","max_tokens":1,"messages":[]}"#)
            .unwrap();
    })
    .await
    .unwrap();
    assert_eq!(
        proxy.messages.lock().unwrap().as_slice(),
        [(
            "claude-ada-example.com/claude-opus-5-5".to_owned(),
            minted.to_ascii_lowercase()
        )]
    );

    // A session on ada that hits its limit moves to carol, the rotating login with room, not
    // to bob, the fallback.
    let repo = host_dir.join("app");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        &["init", "--quiet", "--initial-branch=main"][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@e",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .status()
            .unwrap();
        assert!(status.success());
    }
    let sessions = host.sessions.clone();
    host.runtime
        .handle()
        .spawn(async move {
            let alice = UserId::new("alice");
            let create = CommandBody::CreateSession {
                repo: Some(repo.to_str().unwrap().to_owned()),
                project_id: None,
                branch: None,
                account_id: Some(AccountId::new("vault-claude-ada-example.com")),
                provider: None,
                model: None,
                permission_mode: Some(PermissionMode::Ask),
                failover_pin: None,
                chat: false,
            };
            let Ok(CommandResult::SessionCreated { session_id }) =
                sessions.handle(alice.clone(), create).await
            else {
                panic!("no session on the hub login");
            };
            let prompt = CommandBody::SendPrompt {
                session_id,
                text: "Refactor the parser.".into(),
                images: Vec::new(),
                files: Vec::new(),
            };
            sessions.handle(alice, prompt).await.unwrap();
        })
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let bases = loop {
        let bases: Vec<String> = starts
            .lock()
            .unwrap()
            .iter()
            .map(|start| {
                start
                    .env
                    .get("ANTHROPIC_BASE_URL")
                    .cloned()
                    .unwrap_or_default()
            })
            .collect();
        if bases.len() == 2 {
            break bases;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no failover: {bases:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(bases[0].ends_with("/claude-ada-example.com"), "{bases:?}");
    assert!(bases[1].ends_with("/claude-carol-example.com"), "{bases:?}");
    host.runtime.kill().await;
    hub.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_owner_lists_the_logins_in_the_hubs_cliproxyapi() {
    let tmp = tempfile::tempdir().unwrap();
    let (url, _keys) = fake_cliproxyapi();
    let management = herder_daemon::hub::cliproxyapi::Management::new(
        &url,
        SecretValue::new("management-secret").unwrap(),
    );
    let hub = Hub::start_with(
        &tmp.path().join("hub"),
        0,
        LIVENESS_TIMEOUT,
        Some(SECRETS_KEY),
        Some(ClientKeys::new(management, None).unwrap()),
    )
    .await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    hub.join(&client).await;
    let listed = client
        .send(HostId::new("hub"), CommandBody::ListProviderLogins)
        .await;
    let Ok(CommandResult::ProviderLogins { logins }) = listed else {
        panic!("expected the proxy's logins: {listed:?}");
    };
    assert_eq!(
        logins,
        vec![herder_protocol::ProviderLogin {
            name: "claude-ada@example.com.json".into(),
            provider: Provider::Claude,
            label: Some("ada@example.com".into()),
            email: Some("ada@example.com".into()),
            fallback: false,
            usage: Vec::new(),
            pin: "claude-ada-example.com".into(),
            state: herder_protocol::ProviderLoginState::Active,
            until: None,
            message: None,
        }]
    );
    hub.runtime.kill().await;

    // A hub with no CLIProxyAPI says so.
    let bare = Hub::start(&tmp.path().join("bare"), 0).await;
    let client = Client::open(
        tmp.path().join("client2").display().to_string(),
        "test".into(),
    )
    .unwrap();
    bare.join(&client).await;
    let refused = match client
        .send(HostId::new("hub"), CommandBody::ListProviderLogins)
        .await
    {
        Err(Error::Rejected { info }) => info,
        other => panic!("expected a refusal: {other:?}"),
    };
    assert_eq!(refused.code, ErrorCode::Unsupported);
    assert!(
        refused.message.contains("runs no CLIProxyAPI"),
        "{refused:?}"
    );
    bare.runtime.kill().await;
}

/// Says `message` on `ws` as a JSON text frame.
async fn say(ws: &mut Ws, message: &impl serde::Serialize) {
    let text = serde_json::to_string(message).unwrap();
    ws.send(Message::text(text)).await.unwrap();
}

/// A host's replication connection to `hub` after the hellos: the host in `host_dir`,
/// which pairs the first time, saying whether it takes relayed connections.
async fn replicating(hub: &Hub, host_dir: &Path, relay: bool) -> Ws {
    std::fs::create_dir_all(host_dir).unwrap();
    let mut ws = dial(hub, host_dir).await;
    let hello = HostMessage::Hello(HostHello {
        replication_version: REPLICATION_VERSION,
        host_id: host_id(),
        host_name: "devbox".into(),
        build: "test".into(),
        pairing_code: Some(hub.auth.mint_host("devbox", PAIRING_TTL).unwrap().code),
        attachments_cap: None,
        relay,
    });
    say(&mut ws, &hello).await;
    assert!(matches!(next_message(&mut ws).await, HubMessage::Hello(_)));
    ws
}

/// A device paired with `hub` as a client, as a phone signed in to it is.
fn phone(hub: &Hub) -> DeviceKey {
    let device = DeviceKey::generate().unwrap();
    hub.auth.legacy_user(
        "phone",
        herder_protocol::Role::Member,
        &device.fingerprint(),
    );
    device
}

/// TLS to `hub` as `device`, with an upgrade request for `path` sent and `first` right
/// behind it, as one binary frame unless empty, before any answer: as a client asks for a
/// stream.
async fn ask(
    hub: &Hub,
    device: &DeviceKey,
    path: &str,
    first: &[u8],
) -> tokio_rustls::client::TlsStream<TcpStream> {
    let config = client_config(&hub.fingerprint, device).unwrap();
    let tcp = TcpStream::connect(hub.addr).await.unwrap();
    let mut tls = TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from("herder").unwrap(), tcp)
        .await
        .unwrap();
    let mut sent = format!(
        "GET {path} HTTP/1.1\r\nHost: herder\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
         Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
    )
    .into_bytes();
    if !first.is_empty() {
        // A masked binary frame, with a mask of zeros.
        assert!(first.len() < 126);
        sent.extend_from_slice(&[0x82, 0x80 | first.len() as u8, 0, 0, 0, 0]);
        sent.extend_from_slice(first);
    }
    tls.write_all(&sent).await.unwrap();
    tls.flush().await.unwrap();
    tls
}

/// The hub's answer to a request [`ask`] sent: the stream, or the code it refused with,
/// which the refusal's status agrees with.
async fn answer(mut tls: tokio_rustls::client::TlsStream<TcpStream>) -> Result<Ws, RelayErrorCode> {
    let mut head = Vec::new();
    let mut byte = [0; 1];
    while !head.ends_with(b"\r\n\r\n") {
        let read = tokio::time::timeout(TIMEOUT, tls.read(&mut byte))
            .await
            .expect("no answer from the hub")
            .unwrap();
        assert_eq!(read, 1, "the hub closed the connection without an answer");
        head.push(byte[0]);
    }
    let head = String::from_utf8(head).unwrap();
    let status: u16 = head[9..12].parse().unwrap();
    if status == 101 {
        assert!(head.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="));
        return Ok(WebSocketStream::from_raw_socket(tls, Role::Client, None).await);
    }
    let mut body = String::new();
    tls.read_to_string(&mut body).await.unwrap();
    let error: RelayError = serde_json::from_str(&body).unwrap();
    assert_eq!(error.code.status(), status, "{head}{body}");
    Err(error.code)
}

/// Opens a relay connection as `device` for `route`: the stream, or the code the hub
/// refused it with.
async fn relay(hub: &Hub, device: &DeviceKey, route: &RelayRoute) -> Result<Ws, RelayErrorCode> {
    answer(ask(hub, device, &route.path(), &[]).await).await
}

fn connect_to(host: &str) -> RelayRoute {
    RelayRoute::Machine(HostId::new(host))
}

/// The code the hub refused `route` from `device` with.
async fn relay_refusal(hub: &Hub, device: &DeviceKey, route: &RelayRoute) -> RelayErrorCode {
    match relay(hub, device, route).await {
        Ok(_) => panic!("the hub opened a stream for {route:?}"),
        Err(code) => code,
    }
}

/// Opens a stream from `device` to the host replicating on `replication`, which opens its
/// end as the host in `host_dir`: the device's end and the host's.
async fn stream(hub: &Hub, device: &DeviceKey, host_dir: &Path, replication: &mut Ws) -> (Ws, Ws) {
    let asked = ask(hub, device, &connect_to("host-1").path(), &[]).await;
    let HubMessage::RelayOpen { stream_id, .. } = next_message(replication).await else {
        panic!("the hub did not ask the host to open the stream");
    };
    let host_key = Replicator::device_key(host_dir).unwrap();
    let host = relay(hub, &host_key, &RelayRoute::Stream(stream_id))
        .await
        .unwrap();
    (answer(asked).await.unwrap(), host)
}

/// The next `len` bytes `ws` carries as binary frames.
async fn bytes(ws: &mut Ws, len: usize) -> Vec<u8> {
    let mut got = Vec::new();
    while got.len() < len {
        match tokio::time::timeout(TIMEOUT, ws.next()).await.unwrap() {
            Some(Ok(Message::Binary(data))) => got.extend_from_slice(&data),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            other => panic!("the stream ended after {} bytes: {other:?}", got.len()),
        }
    }
    got
}

/// Whether `ws` ends without carrying anything more.
async fn ended(ws: &mut Ws) -> bool {
    loop {
        match tokio::time::timeout(TIMEOUT, ws.next()).await.unwrap() {
            Some(Ok(Message::Close(_)) | Err(_)) | None => return true,
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(_)) => return false,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_hub_forwards_a_stream_between_a_device_and_a_host_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    let hub = Hub::start(&hub_dir, 0).await;
    let mut replication = replicating(&hub, &host_dir, true).await;
    let device = phone(&hub);

    // Clients are told the hub relays to the host while it is connected.
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    hub.join(&client).await;
    machine_when(&client, |m| m.hosts.iter().any(|h| h.relay)).await;

    let (mut near, mut far) = stream(&hub, &device, &host_dir, &mut replication).await;
    // Every byte arrives, in order, whatever the frames: small ones, and one far bigger
    // than a socket buffer.
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    let sent = [b"hello".to_vec(), Vec::new(), vec![0, 255, 7], big].concat();
    let sending = tokio::spawn({
        let sent = sent.clone();
        async move {
            for chunk in [&sent[..5], &sent[5..8], &sent[8..]] {
                near.send(Message::binary(chunk.to_vec())).await.unwrap();
            }
            near
        }
    });
    assert!(bytes(&mut far, sent.len()).await == sent, "bytes changed");
    let mut near = sending.await.unwrap();
    far.send(Message::binary(b"and back".to_vec()))
        .await
        .unwrap();
    assert_eq!(bytes(&mut near, 8).await, b"and back");

    // Either end closing ends the stream for the other.
    near.close(None).await.unwrap();
    assert!(ended(&mut far).await);
    let (mut near, mut far) = stream(&hub, &device, &host_dir, &mut replication).await;
    far.close(None).await.unwrap();
    assert!(ended(&mut near).await);
    // And so does a host that just goes away.
    let (mut near, far) = stream(&hub, &device, &host_dir, &mut replication).await;
    drop(far);
    assert!(ended(&mut near).await);

    // Without its replication connection the host is not relayed to any more.
    drop(replication);
    machine_when(&client, |m| m.hosts.iter().all(|h| !h.relay && !h.online)).await;
    hub.runtime.kill().await;
}

/// A device sends the first bytes of its stream right behind its request: the hub holds
/// them until the host opened its end, and they reach the host first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_bytes_a_device_sends_before_the_stream_opens_reach_the_host_first() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    let hub = Hub::start(&hub_dir, 0).await;
    let mut replication = replicating(&hub, &host_dir, true).await;
    let device = phone(&hub);

    let asked = ask(&hub, &device, &connect_to("host-1").path(), b"client hello").await;
    let HubMessage::RelayOpen { stream_id, .. } = next_message(&mut replication).await else {
        panic!("the hub did not ask the host to open the stream");
    };
    // The host takes its time: the bytes wait at the hub.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let host_key = Replicator::device_key(&host_dir).unwrap();
    let mut far = relay(&hub, &host_key, &RelayRoute::Stream(stream_id))
        .await
        .unwrap();
    let mut near = answer(asked).await.unwrap();
    assert_eq!(bytes(&mut far, 12).await, b"client hello");
    far.send(Message::binary(b"server hello".to_vec()))
        .await
        .unwrap();
    assert_eq!(bytes(&mut near, 12).await, b"server hello");

    // A host that never connects: the device is refused, its bytes dropped unread.
    drop(replication);
    let asked = ask(&hub, &device, &connect_to("host-1").path(), b"client hello").await;
    assert_eq!(answer(asked).await.err(), Some(RelayErrorCode::HostOffline));
    hub.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_hub_refuses_the_streams_it_may_not_forward() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    let hub = Hub::start(&hub_dir, 0).await;
    let device = phone(&hub);
    let host_key = {
        std::fs::create_dir_all(&host_dir).unwrap();
        Replicator::device_key(&host_dir).unwrap()
    };
    let to_host = connect_to("host-1");
    let refusal = |device, route| relay_refusal(&hub, device, route);

    // A host that takes no relayed connections is not one the hub forwards to.
    let replication = replicating(&hub, &host_dir, false).await;
    assert_eq!(
        refusal(&device, &to_host).await,
        RelayErrorCode::UnknownHost
    );
    let nowhere = connect_to("nope");
    assert_eq!(
        refusal(&device, &nowhere).await,
        RelayErrorCode::UnknownHost
    );
    drop(replication);

    let mut replication = replicating(&hub, &host_dir, true).await;
    // Only a device paired with the hub as a client gets a stream: not a stranger, and
    // not a host's device, which reads nothing here.
    let stranger = DeviceKey::generate().unwrap();
    assert_eq!(
        refusal(&stranger, &to_host).await,
        RelayErrorCode::Forbidden
    );
    assert_eq!(
        refusal(&host_key, &to_host).await,
        RelayErrorCode::Forbidden
    );
    // A path the hub does not know.
    let mut unknown = ask(&hub, &device, "/machines", &[]).await;
    let mut answered = String::new();
    unknown.read_to_string(&mut answered).await.unwrap();
    assert!(answered.starts_with("HTTP/1.1 404 "), "{answered}");
    // Only the host a stream waits for may open its end, and only a stream that waits.
    let accept = RelayRoute::Stream("no-such-stream".into());
    assert_eq!(
        refusal(&host_key, &accept).await,
        RelayErrorCode::BadRequest
    );
    assert_eq!(refusal(&device, &accept).await, RelayErrorCode::Forbidden);
    assert_eq!(
        refusal(&host_key, &RelayRoute::Proxy).await,
        RelayErrorCode::Forbidden
    );

    // A device gets no more streams than its share, whether or not the host opened them.
    let mut open = Vec::new();
    for _ in 0..32 {
        let ws = ask(&hub, &device, &to_host.path(), &[]).await;
        assert!(matches!(
            next_message(&mut replication).await,
            HubMessage::RelayOpen { .. }
        ));
        open.push(ws);
    }
    assert_eq!(refusal(&device, &to_host).await, RelayErrorCode::Busy);
    // One that ends makes room for another.
    drop(open.pop());
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while refusal(&device, &to_host).await == RelayErrorCode::Busy {
        assert!(tokio::time::Instant::now() < deadline, "no room was made");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop(open);

    // A host that is not connected is given a moment, then the device is told to try again.
    drop(replication);
    let started = std::time::Instant::now();
    assert_eq!(
        refusal(&device, &to_host).await,
        RelayErrorCode::HostOffline
    );
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "the host got no time"
    );
    hub.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_reaches_the_hub_through_its_tunnel_listener() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 1, 2);
    let hub = Hub::start(&hub_dir, 0).await;
    let host = HostDaemon::start(&host_dir, hub.config()).await;
    caught_up(&host_dir, &hub_dir).await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    // As `cloudflared` delivers a client that reached the hub's public hostname.
    let address = format!("ws://{}", hub.tunnel);
    let (_, machine) = hub.join_at(&client, address.clone()).await;
    let machine = machine_when(&client, |m| {
        m.host_id == machine.host_id && !m.sessions.is_empty()
    })
    .await;
    assert_eq!(machine.address, Some(address));
    assert_eq!(machine.hosts.len(), 1);
    host.runtime.kill().await;
    hub.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_replicates_through_the_hubs_tunnel() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 2, 5);
    let hub = Hub::start(&hub_dir, 0).await;
    // As a host away from the hub's network reaches it: at the tunnel's URL, where
    // `cloudflared` delivers its connection to the hub's tunnel listener.
    let config = HubConfig {
        address: format!("ws://{}", hub.tunnel),
        ..hub.config()
    };
    let host = HostDaemon::start(&host_dir, config).await;
    caught_up(&host_dir, &hub_dir).await;
    assert_gap_free(&hub_dir);
    host.runtime.kill().await;
    hub.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_reaches_the_hubs_cliproxyapi_through_the_hub() {
    use herder_daemon::hub::pin::{PinProxy, Target};
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    std::fs::create_dir_all(&host_dir).unwrap();
    let (url, _, requests) = fake_cliproxyapi_recording();
    let management = herder_daemon::hub::cliproxyapi::Management::new(
        &url,
        SecretValue::new("management-secret").unwrap(),
    );
    // A hub that names no address for its proxy forwards hosts to it itself.
    let keys = ClientKeys::new(management, None).unwrap();
    let hub = Hub::start_with(&hub_dir, 0, LIVENESS_TIMEOUT, Some(SECRETS_KEY), Some(keys)).await;
    let host = HostDaemon::start(&host_dir, hub.config()).await;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let upstream: herder_daemon::hub::pin::Upstream = loop {
        if let Some(Target::Hub(_)) = host.secrets.proxy_target() {
            let secrets = Arc::clone(&host.secrets);
            break Arc::new(move || secrets.proxy_target());
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no route to the proxy"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    // The hub told the host no address, and lists its logins all the same.
    assert_eq!(host.secrets.cliproxyapi(), None);
    let mut logins = host.secrets.provider_logins();
    tokio::time::timeout(TIMEOUT, logins.wait_for(|logins| !logins.is_empty()))
        .await
        .unwrap()
        .unwrap();

    // A CLI of the host, pointed at the host's loopback proxy as its hub logins' are,
    // reaches the hub's proxy: its request arrives as it would dialling the proxy, with
    // the login pinned and the client key it sent, and the answer comes back.
    let pin_proxy = PinProxy::start(upstream, CancellationToken::new())
        .await
        .unwrap();
    let call = |path: &'static str, body: &'static str| {
        let url = format!("{}{path}", pin_proxy.url("claude-ada"));
        tokio::task::spawn_blocking(move || {
            // A refusal is an answer here, not an error.
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build()
                .into();
            let mut response = agent
                .post(&url)
                .header("x-api-key", "herder-host-key")
                .header("Content-Type", "application/json")
                .send(body)
                .unwrap();
            let status = response.status().as_u16();
            (status, response.body_mut().read_to_string().unwrap())
        })
    };
    let answered = call(
        "/v1/messages?beta=true",
        r#"{"model":"opus","max_tokens":1}"#,
    );
    assert_eq!(
        answered.await.unwrap(),
        (200, r#"{"served":true}"#.to_owned())
    );
    // Each request has a stream of its own, so a second one works as the first.
    let answered = call("/v1/messages/count_tokens", r#"{"model":"opus"}"#);
    assert_eq!(answered.await.unwrap().0, 200);
    let seen = requests.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "{seen:?}");
    let (head, body) = &seen[0];
    assert!(head.starts_with("POST /v1/messages?beta=true "), "{head}");
    assert!(
        head.to_ascii_lowercase()
            .contains("x-api-key: herder-host-key"),
        "{head}"
    );
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(body["model"], "claude-ada/opus");
    assert!(seen[1].0.starts_with("POST /v1/messages/count_tokens "));
    // The proxy's management API stays the hub's alone: a request for it is refused by
    // the hub and never reaches the proxy.
    let refused = call("/v8/management/config/access/api-keys", "[]");
    assert_eq!(refused.await.unwrap().0, 403);
    assert_eq!(requests.lock().unwrap().len(), 2);

    // Only a host the hub forwards may come this way: not a client's device.
    let device = phone(&hub);
    assert_eq!(
        relay_refusal(&hub, &device, &RelayRoute::Proxy).await,
        RelayErrorCode::Forbidden
    );
    host.runtime.kill().await;
    hub.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hub_that_names_its_proxys_address_forwards_no_host_to_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, hub_dir) = (tmp.path().join("host"), tmp.path().join("hub"));
    let (url, ..) = fake_cliproxyapi_recording();
    let management = herder_daemon::hub::cliproxyapi::Management::new(
        &url,
        SecretValue::new("management-secret").unwrap(),
    );
    let keys = ClientKeys::new(management, Some("https://cpa.example.com:8317".into())).unwrap();
    let hub = Hub::start_with(&hub_dir, 0, LIVENESS_TIMEOUT, Some(SECRETS_KEY), Some(keys)).await;
    // The host is told the address, and not to come through the hub.
    std::fs::create_dir_all(&host_dir).unwrap();
    let mut ws = dial(&hub, &host_dir).await;
    let hello = HostMessage::Hello(HostHello {
        replication_version: REPLICATION_VERSION,
        host_id: host_id(),
        host_name: "devbox".into(),
        build: "test".into(),
        pairing_code: Some(hub.auth.mint_host("devbox", PAIRING_TTL).unwrap().code),
        attachments_cap: None,
        relay: true,
    });
    say(&mut ws, &hello).await;
    let HubMessage::Hello(hello) = next_message(&mut ws).await else {
        panic!("no hello from the hub");
    };
    assert_eq!(
        (hello.cliproxyapi.as_deref(), hello.cliproxyapi_relay),
        (Some("https://cpa.example.com:8317"), false)
    );
    let host_key = Replicator::device_key(&host_dir).unwrap();
    assert_eq!(
        relay_refusal(&hub, &host_key, &RelayRoute::Proxy).await,
        RelayErrorCode::Forbidden
    );
    hub.runtime.kill().await;
}
