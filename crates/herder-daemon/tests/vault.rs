//! A host daemon replicating to a vault daemon, both in process over TLS on localhost: sessions
//! appear in the vault, a vault restarted mid-stream gets the rest, images included, a host that was offline
//! catches up when it is back, and a client paired with the vault sees every host's sessions,
//! read-only, and the hosts with their liveness. A host's device replicates and reads nothing.
//! A host fetches the secrets of the sets granted to it, and only those, each fetch audited,
//! and holds those granted for its own use, with no session, until they are revoked.
//! A host backs up and fetches secrets only as its `[vault]` switches say, each without the other.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use herder_client_core::PairingUri;
use herder_client_core::auth::client_config;
use herder_client_core::{Client, Error, Machine, PairResult, SessionSubscription};
use herder_daemon::Hub;
use herder_daemon::auth::{Auth, DeviceRole, PAIRING_TTL};
use herder_daemon::config::{Retention, VaultConfig};
use herder_daemon::session::{Accounts, Adapters, EventSink, SessionManager, Setup};
use herder_daemon::vault::{
    Admin, Held, HeldSecrets, HostSecrets, LIVENESS_TIMEOUT, Replicator, SecretsKey, Server,
    VaultStore, WakeOnEvent,
};
use herder_daemon::worktree::Worktrees;
use herder_daemon::ws::{Host, Tls};
use herder_protocol::{
    AccountId, Attachment, AttachmentId, ClientHello, ClientMessage, Command, CommandBody,
    CommandId, CommandResult, Cursor, ErrorCode, Event, EventBody, HostHello, HostId, HostMessage,
    HostSecretsState, Item, ItemBody, ItemId, JournalRecord, PROTOCOL_VERSION, PermissionMode,
    Provider, REPLICATION_VERSION, ReplicationErrorCode, SecretError, SecretErrorCode, SecretRef,
    SecretValue, ServerMessage, SessionId, SessionStatus, SessionSummary, Timestamp, TurnId,
    UserId, VaultMessage,
};
use herder_store::{NewEvent, Store};
use rustls::pki_types::ServerName;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(30);

/// The master key test vaults keep secrets under.
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

struct Vault {
    runtime: Runtime,
    addr: SocketAddr,
    fingerprint: String,
    auth: Arc<Auth>,
    admin: Admin,
}

impl Vault {
    /// Starts a vault on `dir`, listening on `port` (0 for any).
    async fn start(dir: &Path, port: u16) -> Self {
        Self::start_with(dir, port, LIVENESS_TIMEOUT, Some(SECRETS_KEY)).await
    }

    /// Starts a vault that takes a host silent for `liveness` for gone, and keeps secrets
    /// under `secrets_key`.
    async fn start_with(
        dir: &Path,
        port: u16,
        liveness: Duration,
        secrets_key: Option<[u8; 32]>,
    ) -> Self {
        let runtime = Runtime::new();
        let dir = dir.to_owned();
        std::fs::create_dir_all(dir.join("tls")).unwrap();
        let (started, ready) = tokio::sync::oneshot::channel();
        runtime.handle().spawn({
            async move {
                let tls = Tls::load_or_create(&dir.join("tls"), "vault").unwrap();
                let auth = Arc::new(Auth::open(&dir).unwrap());
                let store = VaultStore::open(dir.join("vault.db")).unwrap();
                let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
                let addr = listener.local_addr().unwrap();
                let fingerprint = tls.fingerprint().to_owned();
                let host = Host {
                    id: HostId::new("vault"),
                    name: "vault".into(),
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
                started
                    .send((addr, fingerprint, auth, server.admin()))
                    .ok()
                    .unwrap();
                server.run(vec![listener], CancellationToken::new()).await;
            }
        });
        let (addr, fingerprint, auth, admin) = ready.await.unwrap();
        Self {
            runtime,
            addr,
            fingerprint,
            auth,
            admin,
        }
    }

    /// Where a host replicates to, pairing as a host-only device with a fresh code, as
    /// `herder pair --host` mints it; images are not backed up.
    fn config(&self) -> VaultConfig {
        VaultConfig::new(
            self.addr.to_string(),
            self.fingerprint.clone(),
            Some(self.auth.mint_host("devbox", PAIRING_TTL).unwrap().code),
        )
    }

    /// A pairing link for a client of `user`, as `herder pair` on the vault prints it.
    fn pairing_link(&self, user: &str) -> String {
        PairingUri {
            hosts: vec![self.addr.to_string()],
            fingerprint: self.fingerprint.clone(),
            code: self.auth.mint(user, None, PAIRING_TTL).unwrap().code,
        }
        .to_string()
    }

    /// Every event the vault holds of `host-1`, by session; read from its database file.
    fn held(dir: &Path) -> BTreeMap<SessionId, Vec<JournalRecord>> {
        let store = VaultStore::open(dir.join("vault.db")).unwrap();
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
        VaultStore::open(dir.join("vault.db"))
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
    async fn start(dir: &Path, vault: VaultConfig) -> Self {
        Self::start_with(dir, vault, HostSecrets::default()).await
    }

    /// Starts a host whose secrets are `secrets`.
    async fn start_with(dir: &Path, vault: VaultConfig, secrets: HostSecrets) -> Self {
        let runtime = Runtime::new();
        let dir = dir.to_owned();
        let secrets = Arc::new(secrets);
        secrets.set_linked(vault.secrets);
        let host_secrets = Arc::clone(&secrets);
        let sessions = runtime
            .handle()
            .spawn(async move {
                let changed = Arc::new(Notify::new());
                let sink = Arc::new(WakeOnEvent {
                    next: Arc::new(Hub::default()) as Arc<dyn EventSink>,
                    notify: Arc::clone(&changed),
                });
                let setup = Setup {
                    store: Store::open(dir.join("herder.db")).unwrap(),
                    adapters: Adapters::new(),
                    accounts: Accounts::new(),
                    sink,
                    turn_ids: Box::new(|| TurnId::new("turn")),
                    worktrees: Worktrees::new(dir.join("worktrees")),
                    attachments: dir.join("attachments"),
                    chats: dir.join("chats"),
                };
                let shutdown = CancellationToken::new();
                let sessions = SessionManager::open(setup, shutdown.clone()).await.unwrap();
                let replicator = Replicator {
                    vault,
                    device: Replicator::device_key(&dir).unwrap(),
                    host: Host {
                        id: host_id(),
                        name: "devbox".into(),
                    },
                    sessions: sessions.clone(),
                    changed,
                    secrets: host_secrets,
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

/// Every image the events the vault holds name: the image the vault holds, if any, and the one
/// the host keeps.
fn images(host_dir: &Path, vault_dir: &Path) -> Vec<(Option<Vec<u8>>, Vec<u8>)> {
    let store = VaultStore::open(vault_dir.join("vault.db")).unwrap();
    let mut images = Vec::new();
    for (session_id, records) in Vault::held(vault_dir) {
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

/// Waits until the vault holds exactly the host's journal, and its fleet index the host's
/// latest seqs.
async fn caught_up(host_dir: &Path, vault_dir: &Path) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let host = journal(host_dir);
        let held = Vault::held(vault_dir);
        let heads: BTreeMap<_, _> = Vault::summaries(vault_dir)
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
            "the vault did not catch up: holds {:?} of {:?}",
            held.values().map(Vec::len).collect::<Vec<_>>(),
            host.values().map(Vec::len).collect::<Vec<_>>(),
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Every session's seqs held by the vault are gap-free from 1, so a resend never duplicated
/// or skipped one.
fn assert_gap_free(vault_dir: &Path) {
    for (session_id, records) in Vault::held(vault_dir) {
        let seqs: Vec<_> = records.iter().map(|r| r.seq).collect();
        assert!(
            seqs.iter().copied().eq(1..=seqs.len() as u64),
            "{session_id}: {seqs:?}"
        );
    }
}

/// The machine a link of one machine paired with.
fn one_machine(results: Vec<PairResult>) -> Machine {
    match <[PairResult; 1]>::try_from(results) {
        Ok([PairResult::Paired { machine }]) => machine,
        other => panic!("expected one paired machine: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_appear_in_the_vault_and_follow_live() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, vault_dir) = (tmp.path().join("host"), tmp.path().join("vault"));
    std::fs::create_dir_all(&host_dir).unwrap();
    // More events than one batch holds.
    seed(&host_dir, 3, 300);
    let vault = Vault::start(&vault_dir, 0).await;
    let host = HostDaemon::start(&host_dir, vault.config()).await;
    caught_up(&host_dir, &vault_dir).await;

    host.switch_model("s2", "live").await;
    caught_up(&host_dir, &vault_dir).await;
    let held = Vault::held(&vault_dir);
    let last = held[&SessionId::new("s2")].last().unwrap();
    assert_eq!(
        last.body.decode(),
        EventBody::ModelSwitched {
            model: "live".into()
        }
    );
    assert_eq!(last.by.as_ref().map(|by| by.as_str()), Some("alice"));
    let summaries = Vault::summaries(&vault_dir);
    assert_eq!(summaries.len(), 3);
    assert_eq!(summaries[0].branch.as_deref(), Some("herder/s1"));
    assert_eq!(summaries[0].project_id.as_str(), "host-1:/home/dev/herder");
    host.runtime.kill().await;
    vault.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_vault_restarted_mid_stream_gets_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, vault_dir) = (tmp.path().join("host"), tmp.path().join("vault"));
    std::fs::create_dir_all(&host_dir).unwrap();
    // More than the replicator's window of unacknowledged events, so the kill comes mid-stream,
    // with images all along.
    for _ in 0..10 {
        seed(&host_dir, 2, 100);
        seed_images(&host_dir, 2, 5);
    }
    let vault = Vault::start(&vault_dir, 0).await;
    let addr = vault.addr;
    let config = VaultConfig {
        attachments: true,
        ..vault.config()
    };
    let host = HostDaemon::start(&host_dir, config).await;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while Vault::held(&vault_dir).is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "nothing replicated");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    vault.runtime.kill().await;
    // Every image an event the vault holds names was durable before that event.
    for (held, kept) in images(&host_dir, &vault_dir) {
        assert_eq!(held, Some(kept));
    }
    // The host keeps working while the vault is down.
    host.switch_model("s1", "while-down").await;
    assert_gap_free(&vault_dir);

    let vault = Vault::start(&vault_dir, addr.port()).await;
    caught_up(&host_dir, &vault_dir).await;
    assert_gap_free(&vault_dir);
    let images = images(&host_dir, &vault_dir);
    assert_eq!(images.len(), 2 * 10 * 5);
    for (held, kept) in images {
        assert_eq!(held, Some(kept));
    }
    host.runtime.kill().await;
    vault.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_that_was_offline_catches_up_without_pairing_again() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, vault_dir) = (tmp.path().join("host"), tmp.path().join("vault"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 2, 10);
    let vault = Vault::start(&vault_dir, 0).await;
    let host = HostDaemon::start(&host_dir, vault.config()).await;
    caught_up(&host_dir, &vault_dir).await;
    host.runtime.kill().await;

    // Offline: the journal grows, including a new session, and the code is used up.
    seed(&host_dir, 3, 20);
    let config = VaultConfig {
        pairing_code: None,
        ..vault.config()
    };
    let host = HostDaemon::start(&host_dir, config).await;
    caught_up(&host_dir, &vault_dir).await;
    assert_gap_free(&vault_dir);
    assert_eq!(vault.auth.devices().len(), 1);
    host.runtime.kill().await;
    vault.runtime.kill().await;
}

/// The vault as the paired client sees it once `ready` holds, waiting for it.
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

/// The refusal of a prompt to `session` sent through the vault.
async fn refusal(client: &Client, vault: &HostId, session: &str) -> herder_protocol::ErrorInfo {
    let command = CommandBody::SendPrompt {
        session_id: SessionId::new(session),
        text: "hi".into(),
        images: Vec::new(),
        files: Vec::new(),
    };
    match client.send(vault.clone(), command).await {
        Err(Error::Rejected { info: error }) => error,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_of_the_vault_sees_host_sessions_read_only() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, vault_dir) = (tmp.path().join("host"), tmp.path().join("vault"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 2, 3);
    let vault = Vault::start(&vault_dir, 0).await;
    let host = HostDaemon::start(&host_dir, vault.config()).await;
    caught_up(&host_dir, &vault_dir).await;

    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    let paired = client
        .pair(vault.pairing_link("alice"))
        .await
        .map(one_machine)
        .unwrap();
    let vault_id = paired.host_id;
    assert_eq!(vault_id.as_str(), "vault");
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
        .subscribe_session(vault_id.clone(), SessionId::new("s1"))
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
        match client.send(vault_id.clone(), edit).await {
            Err(Error::Rejected { info }) => assert_eq!(info.code, ErrorCode::ReadOnly),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
    let error = refusal(&client, &vault_id, "s1").await;
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
        client.send(vault_id.clone(), archive).await,
        Err(Error::Rejected { info: error }) if error.code == ErrorCode::ReadOnly
    ));
    // A client of the vault shares it on, with the vault's own address.
    let Ok(CommandResult::DevicePairing {
        addresses,
        fingerprint,
        ..
    }) = client
        .send(
            vault_id.clone(),
            CommandBody::PairDevice {
                user: None,
                role: None,
            },
        )
        .await
    else {
        panic!("expected a device pairing");
    };
    assert_eq!(
        (addresses, fingerprint),
        (vec![vault.addr.to_string()], vault.fingerprint.clone())
    );
    let missing = refusal(&client, &vault_id, "nope").await;
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
        let error = refusal(&client, &vault_id, "s1").await;
        if error.message.contains("which is offline, last seen ") {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{}", error.message);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(client.machines()[0].sessions.len(), 2);
    let sub = client
        .subscribe_session(vault_id.clone(), SessionId::new("s2"))
        .unwrap();
    assert_eq!(seqs(&sub, 4).await, [1, 2, 3, 4]);
    vault.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_of_the_vault_sees_what_it_holds_of_each_host() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, vault_dir) = (tmp.path().join("host"), tmp.path().join("vault"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 2, 3);
    let vault = Vault::start(&vault_dir, 0).await;
    let host = HostDaemon::start(&host_dir, vault.config()).await;
    caught_up(&host_dir, &vault_dir).await;

    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    client
        .pair(vault.pairing_link("alice"))
        .await
        .map(one_machine)
        .unwrap();
    let machine = machine_when(&client, |m| m.vault.as_ref().is_some_and(|v| v.events == 8)).await;
    let status = machine.vault.unwrap();
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
    let machine = machine_when(&client, |m| m.vault.as_ref().is_some_and(|v| v.events == 9)).await;
    let status = machine.vault.unwrap();
    assert_eq!(status.hosts[0].events, 9);
    let lag = status.hosts[0].lag_ms.unwrap();
    assert!(lag < TIMEOUT.as_millis() as u64, "{lag}");

    // A disconnected vault's status is dropped: it is live only.
    vault.runtime.kill().await;
    machine_when(&client, |m| m.vault.is_none()).await;
    host.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_host_is_offline_after_the_liveness_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, vault_dir) = (tmp.path().join("host"), tmp.path().join("vault"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 1, 0);
    let liveness = Duration::from_millis(500);
    let vault = Vault::start_with(&vault_dir, 0, liveness, None).await;
    // A real host replicates the session, then stops for good.
    let host = HostDaemon::start(&host_dir, vault.config()).await;
    caught_up(&host_dir, &vault_dir).await;
    host.runtime.kill().await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    let vault_id = client
        .pair(vault.pairing_link("alice"))
        .await
        .map(one_machine)
        .unwrap()
        .host_id;

    // The host's device comes back but falls silent after its hello, as a host whose machine
    // hangs or whose network drops without closing the connection would.
    let mut ws = dial(&vault, &host_dir).await;
    let hello = HostMessage::Hello(HostHello {
        replication_version: REPLICATION_VERSION,
        host_id: host_id(),
        host_name: "devbox".into(),
        build: "test".into(),
        pairing_code: None,
        attachments_cap: None,
    });
    let text = serde_json::to_string(&hello).unwrap();
    ws.send(Message::text(text)).await.unwrap();
    let Some(Ok(Message::Text(reply))) = ws.next().await else {
        panic!("no hello from the vault");
    };
    assert!(matches!(
        serde_json::from_str(&reply).unwrap(),
        VaultMessage::Hello(_)
    ));
    assert!(
        refusal(&client, &vault_id, "s1")
            .await
            .message
            .ends_with("which is online")
    );

    machine_when(&client, |m| m.hosts.iter().any(|h| h.online)).await;

    // Silent from now on, without closing: offline once the timeout passes.
    machine_when(&client, |m| m.hosts.iter().all(|h| !h.online)).await;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let error = refusal(&client, &vault_id, "s1").await;
        if error.message.contains("which is offline") {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{}", error.message);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop(ws);
    vault.runtime.kill().await;
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<TcpStream>>;

/// A WebSocket to the vault with the device key of the host in `host_dir`.
async fn dial(vault: &Vault, host_dir: &Path) -> Ws {
    let device = Replicator::device_key(host_dir).unwrap();
    let config = client_config(&vault.fingerprint, &device).unwrap();
    let tcp = TcpStream::connect(vault.addr).await.unwrap();
    let tls = TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from("herder").unwrap(), tcp)
        .await
        .unwrap();
    let request = format!("wss://{}/", vault.addr)
        .into_client_request()
        .unwrap();
    tokio_tungstenite::client_async(request, tls)
        .await
        .unwrap()
        .0
}

/// Every text message the vault sends on `ws` until it closes the connection.
async fn until_closed(ws: &mut Ws) -> Vec<String> {
    let mut texts = Vec::new();
    loop {
        match tokio::time::timeout(TIMEOUT, ws.next()).await {
            Err(_) => panic!("the vault kept the connection open; sent {texts:?}"),
            Ok(Some(Ok(Message::Text(text)))) => texts.push(text.as_str().to_owned()),
            Ok(Some(Ok(Message::Close(_))) | None | Some(Err(_))) => return texts,
            Ok(Some(Ok(_))) => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_device_replicates_and_resumes_but_reads_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, vault_dir) = (tmp.path().join("host"), tmp.path().join("vault"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 2, 3);
    let vault = Vault::start(&vault_dir, 0).await;
    let host = HostDaemon::start(&host_dir, vault.config()).await;
    caught_up(&host_dir, &vault_dir).await;
    let devices = vault.auth.devices();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].0.role, DeviceRole::Host);
    host.runtime.kill().await;

    // Its key, stolen, speaks the client protocol, as `herder fork` reads the vault too:
    // every read is refused at the hello, before the vault sends a host list, a session list,
    // a journal or the secrets.
    let mut ws = dial(&vault, &host_dir).await;
    let hello = ClientMessage::Hello(ClientHello {
        protocol_version: PROTOCOL_VERSION,
        client: "thief".into(),
        resume: vec![Cursor {
            session_id: SessionId::new("s1"),
            after_seq: 0,
        }],
        pairing_code: None,
        sign_in: false,
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
        // The vault may close before the last ones arrive.
        let _ = ws
            .send(Message::text(serde_json::to_string(&message).unwrap()))
            .await;
    }
    let sent = until_closed(&mut ws).await;
    let [only] = sent.as_slice() else {
        panic!("the vault sent more than a refusal: {sent:?}");
    };
    let ServerMessage::Error { error } = serde_json::from_str(only).unwrap() else {
        panic!("expected a refusal: {only}");
    };
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert!(
        error
            .message
            .contains("only backs its sessions up to the vault"),
        "{}",
        error.message
    );

    // As a host it may still only be itself, and sees only its own cursors.
    let mut ws = dial(&vault, &host_dir).await;
    let hello = HostMessage::Hello(HostHello {
        replication_version: REPLICATION_VERSION,
        host_id: HostId::new("macbook"),
        host_name: "macbook".into(),
        build: "test".into(),
        pairing_code: None,
        attachments_cap: None,
    });
    ws.send(Message::text(serde_json::to_string(&hello).unwrap()))
        .await
        .unwrap();
    let sent = until_closed(&mut ws).await;
    assert!(
        matches!(
            serde_json::from_str(&sent[0]).unwrap(),
            VaultMessage::Error { error } if error.code == ReplicationErrorCode::Forbidden
        ),
        "{sent:?}"
    );

    // Its own host resumes from the vault's cursors.
    seed(&host_dir, 3, 5);
    let config = VaultConfig {
        pairing_code: None,
        ..vault.config()
    };
    let host = HostDaemon::start(&host_dir, config).await;
    caught_up(&host_dir, &vault_dir).await;
    assert_gap_free(&vault_dir);
    host.runtime.kill().await;

    // A client device reads everything, as before.
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    client
        .pair(vault.pairing_link("alice"))
        .await
        .map(one_machine)
        .unwrap();
    let machine = machine_when(&client, |m| m.sessions.len() == 3).await;
    assert_eq!(machine.hosts.len(), 1);
    vault.runtime.kill().await;
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

/// The next message the vault sends on `ws`.
async fn next_message(ws: &mut Ws) -> VaultMessage {
    loop {
        match tokio::time::timeout(TIMEOUT, ws.next()).await.unwrap() {
            Some(Ok(Message::Text(text))) => return serde_json::from_str(&text).unwrap(),
            Some(Ok(Message::Close(_))) | None => panic!("the vault closed the connection"),
            Some(Ok(_)) => {}
            Some(Err(err)) => panic!("{err}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn images_are_not_backed_up_unless_the_host_says_so_and_never_fail_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, vault_dir) = (tmp.path().join("host"), tmp.path().join("vault"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 2, 3);
    seed_images(&host_dir, 2, 2);
    let vault = Vault::start(&vault_dir, 0).await;
    let host = HostDaemon::start(&host_dir, vault.config()).await;
    caught_up(&host_dir, &vault_dir).await;
    // Off by default: the journals name four images, and the vault holds none.
    let images = images(&host_dir, &vault_dir);
    assert_eq!(images.len(), 4);
    assert!(images.iter().all(|(held, _)| held.is_none()));
    host.runtime.kill().await;
    // A client asking for one is told it was not backed up.
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    let vault_id = client
        .pair(vault.pairing_link("alice"))
        .await
        .map(one_machine)
        .unwrap()
        .host_id;
    let fetch = CommandBody::GetAttachment {
        session_id: SessionId::new("s1"),
        attachment_id: AttachmentId::new("img1x0"),
    };
    let Err(Error::Rejected { info }) = client.send(vault_id, fetch).await else {
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
        let mut ws = dial(&vault, &host_dir).await;
        let hello = HostMessage::Hello(HostHello {
            replication_version: REPLICATION_VERSION,
            host_id: host_id(),
            host_name: "devbox".into(),
            build: "test".into(),
            pairing_code: None,
            attachments_cap: cap,
        });
        ws.send(Message::text(serde_json::to_string(&hello).unwrap()))
            .await
            .unwrap();
        assert!(matches!(
            next_message(&mut ws).await,
            VaultMessage::Hello(_)
        ));
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
        let VaultMessage::Ack(cursor) = next_message(&mut ws).await else {
            panic!("expected an ack");
        };
        assert_eq!(cursor.session_id.as_str(), "s1");
        let held = VaultStore::open(vault_dir.join("vault.db"))
            .unwrap()
            .attachment(
                &host_id(),
                &SessionId::new("s1"),
                &AttachmentId::new(format!("sent{size}")),
            )
            .unwrap();
        assert_eq!(held.is_some(), kept, "cap {cap:?}, {size} bytes");
    }
    vault.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hosts_images_stay_within_its_cap_and_the_hosts_list_shows_usage() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, vault_dir) = (tmp.path().join("host"), tmp.path().join("vault"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 2, 1);
    // Ten images of 14 bytes each, the oldest first; room for three.
    seed_images(&host_dir, 2, 5);
    let vault = Vault::start(&vault_dir, 0).await;
    let config = VaultConfig {
        attachments: true,
        attachments_cap: 3 * 14,
        ..vault.config()
    };
    let host = HostDaemon::start(&host_dir, config).await;
    caught_up(&host_dir, &vault_dir).await;
    let held: Vec<bool> = images(&host_dir, &vault_dir)
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
    client
        .pair(vault.pairing_link("alice"))
        .await
        .map(one_machine)
        .unwrap();
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
    vault.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_forgotten_host_leaves_nothing_on_the_vault() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, vault_dir) = (tmp.path().join("host"), tmp.path().join("vault"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 2, 3);
    let vault = Vault::start(&vault_dir, 0).await;
    let host = HostDaemon::start(&host_dir, vault.config()).await;
    caught_up(&host_dir, &vault_dir).await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    client
        .pair(vault.pairing_link("alice"))
        .await
        .map(one_machine)
        .unwrap();
    machine_when(&client, |m| m.sessions.len() == 2).await;

    // Never while it is online.
    let refused = vault.admin.forget_host("devbox").await.unwrap_err();
    assert!(format!("{refused:#}").contains("online"), "{refused:#}");
    host.runtime.kill().await;
    machine_when(&client, |m| m.hosts.iter().all(|h| !h.online)).await;
    let missing = vault.admin.forget_host("laptop").await.unwrap_err();
    assert!(
        format!("{missing:#}").contains("no host laptop"),
        "{missing:#}"
    );

    let forgot = vault.admin.forget_host("devbox").await.unwrap();
    assert_eq!(
        (forgot.host_id, forgot.sessions, forgot.devices),
        (host_id(), 2, 1)
    );
    assert!(Vault::held(&vault_dir).is_empty());
    // Its device is unpaired; the client's stays.
    let devices = vault.auth.devices();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].0.role, DeviceRole::Client);
    machine_when(&client, |m| m.hosts.is_empty() && m.sessions.is_empty()).await;
    vault.runtime.kill().await;
}

/// Runs a secret command on the vault as the paired `client`.
async fn secret_command(client: &Client, command: CommandBody) -> Result<CommandResult, ErrorCode> {
    match client.send(HostId::new("vault"), command).await {
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
    let (host_dir, vault_dir) = (tmp.path().join("host"), tmp.path().join("vault"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 1, 1);
    let vault = Vault::start(&vault_dir, 0).await;
    let host = HostDaemon::start(&host_dir, vault.config()).await;
    caught_up(&host_dir, &vault_dir).await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    client
        .pair(vault.pairing_link("alice"))
        .await
        .map(one_machine)
        .unwrap();

    for set in ["prod", "dev", "other"] {
        let created = CommandBody::CreateSecretSet { set: set.into() };
        assert_eq!(
            secret_command(&client, created).await,
            Ok(CommandResult::Applied)
        );
    }
    let again = CommandBody::CreateSecretSet { set: "prod".into() };
    assert_eq!(
        secret_command(&client, again).await,
        Err(ErrorCode::Conflict)
    );
    let bad = CommandBody::CreateSecretSet { set: "Prod".into() };
    assert_eq!(
        secret_command(&client, bad).await,
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
        };
        assert_eq!(
            secret_command(&client, put).await,
            Ok(CommandResult::Applied)
        );
    }
    let nowhere = CommandBody::PutSecret {
        set: "nowhere".into(),
        name: "TOKEN".into(),
        description: "x".into(),
        value: SecretValue::new("x").unwrap(),
    };
    assert_eq!(
        secret_command(&client, nowhere).await,
        Err(ErrorCode::NotFound)
    );
    for set in ["prod", "dev"] {
        let grant = CommandBody::GrantSecretSet {
            set: set.into(),
            host_id: host_id(),
            host_use: false,
        };
        assert_eq!(
            secret_command(&client, grant).await,
            Ok(CommandResult::Applied)
        );
    }
    let stranger = CommandBody::GrantSecretSet {
        set: "other".into(),
        host_id: HostId::new("stranger"),
        host_use: false,
    };
    assert_eq!(
        secret_command(&client, stranger).await,
        Err(ErrorCode::NotFound)
    );

    // The owner sees every set and secret, never a value.
    let listed = secret_command(&client, CommandBody::ListSecrets)
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
    };
    assert_eq!(
        secret_command(&client, revoke).await,
        Ok(CommandResult::Applied)
    );
    let revoked = fetch(&host, "s3", &["prod/TOKEN"]).await.unwrap_err();
    assert_eq!(revoked.code, SecretErrorCode::NotGranted);
    assert_eq!(
        fetch(&host, "s3", &["TOKEN"]).await.unwrap(),
        [("dev/TOKEN".to_owned(), "dev-token-value".to_owned())]
    );

    // One audit row per value handed out, newest first, without the value.
    let audit = secret_command(&client, CommandBody::ListSecretAudit { before: None })
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
        secret_command(&client, older).await.unwrap()
    else {
        panic!("expected the audit");
    };
    assert_eq!(older.len(), 1);
    assert_eq!(older[0].name, "STRIPE_KEY");
    let wire = serde_json::to_string(&audit).unwrap();
    assert!(!wire.contains("-value"), "{wire}");

    // Sets go once they are empty; deleting a secret takes it out of the host's reach.
    let busy = CommandBody::DeleteSecretSet { set: "dev".into() };
    assert_eq!(
        secret_command(&client, busy).await,
        Err(ErrorCode::Conflict)
    );
    let delete = CommandBody::DeleteSecret {
        set: "dev".into(),
        name: "TOKEN".into(),
    };
    assert_eq!(
        secret_command(&client, delete).await,
        Ok(CommandResult::Applied)
    );
    let gone = fetch(&host, "s4", &["TOKEN"]).await.unwrap_err();
    assert_eq!(gone.code, SecretErrorCode::Unknown);
    let delete = CommandBody::DeleteSecretSet { set: "dev".into() };
    assert_eq!(
        secret_command(&client, delete).await,
        Ok(CommandResult::Applied)
    );
    host.runtime.kill().await;
    vault.runtime.kill().await;
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
async fn a_host_holds_secrets_for_its_own_use_until_revoked_or_its_vault_is_down() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, vault_dir) = (tmp.path().join("host"), tmp.path().join("vault"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 1, 1);
    let vault = Vault::start(&vault_dir, 0).await;
    let secrets = HostSecrets::with_ttl(Duration::from_millis(300));
    let host = HostDaemon::start_with(&host_dir, vault.config(), secrets).await;
    caught_up(&host_dir, &vault_dir).await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    client
        .pair(vault.pairing_link("alice"))
        .await
        .map(one_machine)
        .unwrap();
    for (set, name, value) in [
        ("bots", "SLACK_TOKEN", "slack-v1"),
        ("mail", "SMTP_PASSWORD", "smtp-value"),
    ] {
        let created = CommandBody::CreateSecretSet { set: set.into() };
        assert_eq!(
            secret_command(&client, created).await,
            Ok(CommandResult::Applied)
        );
        let put = CommandBody::PutSecret {
            set: set.into(),
            name: name.into(),
            description: format!("{name} of {set}"),
            value: SecretValue::new(value).unwrap(),
        };
        assert_eq!(
            secret_command(&client, put).await,
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
                          vault's owner to grant its set for host use"
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
        };
        assert_eq!(
            secret_command(&client, grant).await,
            Ok(CommandResult::Applied)
        );
    }
    let listed = secret_command(&client, CommandBody::ListSecrets)
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
    let audit = secret_command(&client, CommandBody::ListSecretAudit { before: None })
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
    };
    assert_eq!(
        secret_command(&client, put).await,
        Ok(CommandResult::Applied)
    );
    held_until(&slack, |held| holds(held, "slack-v2")).await;

    // The grant for host use is its own: revoking the sessions' one finds none, and revoking
    // it stops the reads.
    let sessions_grant = CommandBody::RevokeSecretSet {
        set: "bots".into(),
        host_id: host_id(),
        host_use: false,
    };
    assert_eq!(
        secret_command(&client, sessions_grant).await,
        Err(ErrorCode::NotFound)
    );
    let revoke = CommandBody::RevokeSecretSet {
        set: "bots".into(),
        host_id: host_id(),
        host_use: true,
    };
    assert_eq!(
        secret_command(&client, revoke).await,
        Ok(CommandResult::Applied)
    );
    let revoked = held_until(&slack, |held| matches!(held, Held::Denied(_))).await;
    let Held::Denied(error) = revoked else {
        unreachable!()
    };
    assert_eq!(error.code, SecretErrorCode::Unknown);
    assert!(holds(&mail.current(), "smtp-value"));

    // A purpose the vault cannot audit gets nothing.
    let bad = host
        .secrets
        .hold("Bot Team".into(), vec![secret("SMTP_PASSWORD")]);
    let refused = held_until(&bad, |held| matches!(held, Held::Denied(_))).await;
    let Held::Denied(error) = refused else {
        unreachable!()
    };
    assert!(error.message.contains("`Bot Team`"), "{}", error.message);

    // The vault down, the host is waiting for it.
    vault.runtime.kill().await;
    held_until(&mail, |held| *held == Held::WaitingForVault).await;
    assert_eq!(mail.current().state(), HostSecretsState::WaitingForVault);
    host.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_vault_without_its_key_keeps_working_but_hands_out_no_secrets() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, vault_dir) = (tmp.path().join("host"), tmp.path().join("vault"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 1, 1);
    let vault = Vault::start(&vault_dir, 0).await;
    let host = HostDaemon::start(&host_dir, vault.config()).await;
    caught_up(&host_dir, &vault_dir).await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    client
        .pair(vault.pairing_link("alice"))
        .await
        .map(one_machine)
        .unwrap();
    for command in [
        CommandBody::CreateSecretSet { set: "prod".into() },
        CommandBody::PutSecret {
            set: "prod".into(),
            name: "TOKEN".into(),
            description: "Token".into(),
            value: SecretValue::new("token-value").unwrap(),
        },
        CommandBody::GrantSecretSet {
            set: "prod".into(),
            host_id: host_id(),
            host_use: false,
        },
    ] {
        assert_eq!(
            secret_command(&client, command).await,
            Ok(CommandResult::Applied)
        );
    }
    assert_eq!(fetch(&host, "s1", &["TOKEN"]).await.unwrap().len(), 1);
    host.runtime.kill().await;
    let addr = vault.addr;
    vault.runtime.kill().await;

    // Without a key: hosts replicate as before, secret commands and fetches are refused.
    let vault = Vault::start_with(&vault_dir, addr.port(), LIVENESS_TIMEOUT, None).await;
    seed(&host_dir, 1, 3);
    let config = VaultConfig {
        pairing_code: None,
        ..vault.config()
    };
    let host = HostDaemon::start(&host_dir, config.clone()).await;
    caught_up(&host_dir, &vault_dir).await;
    let refused = match client
        .send(HostId::new("vault"), CommandBody::ListSecrets)
        .await
    {
        Err(Error::Rejected { info }) => info,
        other => panic!("expected a refusal: {other:?}"),
    };
    assert_eq!(refused.code, ErrorCode::Unsupported);
    assert!(
        refused
            .message
            .starts_with("this vault has no secrets key; set HERDER_VAULT_SECRETS_KEY"),
        "{}",
        refused.message
    );
    let error = fetch(&host, "s2", &["TOKEN"]).await.unwrap_err();
    assert!(
        error.message.starts_with("the vault has no secrets key")
            && error.message.contains("HERDER_VAULT_SECRETS_KEY"),
        "{}",
        error.message
    );
    host.runtime.kill().await;
    vault.runtime.kill().await;

    // With another key: what was stored under the old one does not open.
    let vault = Vault::start_with(&vault_dir, addr.port(), LIVENESS_TIMEOUT, Some([8; 32])).await;
    // New events, so catching up means the host is connected again.
    seed(&host_dir, 1, 5);
    let host = HostDaemon::start(&host_dir, config).await;
    caught_up(&host_dir, &vault_dir).await;
    let error = fetch(&host, "s3", &["TOKEN"]).await.unwrap_err();
    assert!(
        error.message.contains("cannot decrypt"),
        "{}",
        error.message
    );
    host.runtime.kill().await;
    vault.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_backs_up_and_fetches_secrets_only_as_its_switches_say() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, vault_dir) = (tmp.path().join("host"), tmp.path().join("vault"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 2, 3);
    let vault = Vault::start(&vault_dir, 0).await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "test".into(),
    )
    .unwrap();
    client
        .pair(vault.pairing_link("alice"))
        .await
        .map(one_machine)
        .unwrap();

    // Backup off: the host connects, so its sessions get secrets, but replicates nothing.
    let config = VaultConfig {
        backup: false,
        ..vault.config()
    };
    let host = HostDaemon::start(&host_dir, config).await;
    machine_when(&client, |m| {
        m.hosts.iter().any(|h| h.host_id == host_id() && h.online)
    })
    .await;
    for command in [
        CommandBody::CreateSecretSet { set: "prod".into() },
        CommandBody::PutSecret {
            set: "prod".into(),
            name: "TOKEN".into(),
            description: "Token".into(),
            value: SecretValue::new("token-value").unwrap(),
        },
        CommandBody::GrantSecretSet {
            set: "prod".into(),
            host_id: host_id(),
            host_use: false,
        },
    ] {
        assert_eq!(
            secret_command(&client, command).await,
            Ok(CommandResult::Applied)
        );
    }
    assert_eq!(
        fetch(&host, "s1", &["TOKEN"]).await.unwrap(),
        [("prod/TOKEN".to_owned(), "token-value".to_owned())]
    );
    // The fetch was answered on the connection after anything a backup would have sent.
    assert!(Vault::held(&vault_dir).is_empty());
    assert!(Vault::summaries(&vault_dir).is_empty());
    host.runtime.kill().await;

    // Secrets off: the host replicates everything, and its sessions get no secrets.
    let config = VaultConfig {
        pairing_code: None,
        secrets: false,
        ..vault.config()
    };
    let host = HostDaemon::start(&host_dir, config).await;
    caught_up(&host_dir, &vault_dir).await;
    let refused = fetch(&host, "s2", &["TOKEN"]).await.unwrap_err();
    assert_eq!(refused.code, SecretErrorCode::NoVault);
    host.runtime.kill().await;
    vault.runtime.kill().await;
}
