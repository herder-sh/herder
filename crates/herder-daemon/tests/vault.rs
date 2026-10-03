//! A host daemon replicating to a vault daemon, both in process over TLS on localhost: sessions
//! appear in the vault, a vault restarted mid-stream gets the rest, a host that was offline
//! catches up when it is back, and a client paired with the vault sees every host's sessions,
//! read-only, and the hosts with their liveness.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use herder_client_core::PairingUri;
use herder_client_core::auth::client_config;
use herder_client_core::{Client, Error, Machine, SessionSubscription};
use herder_daemon::Hub;
use herder_daemon::auth::{Auth, PAIRING_TTL};
use herder_daemon::config::VaultConfig;
use herder_daemon::session::{Accounts, Adapters, EventSink, SessionManager, Setup};
use herder_daemon::vault::{LIVENESS_TIMEOUT, Replicator, Server, VaultStore, WakeOnEvent};
use herder_daemon::worktree::Worktrees;
use herder_daemon::ws::{Host, Tls};
use herder_protocol::{
    AccountId, CommandBody, ErrorCode, Event, EventBody, HostHello, HostId, HostMessage,
    JournalRecord, PermissionMode, Provider, REPLICATION_VERSION, SessionId, SessionStatus,
    SessionSummary, Timestamp, TurnId, UserId, VaultMessage,
};
use herder_store::{NewEvent, Store};
use rustls::pki_types::ServerName;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(30);

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
}

impl Vault {
    /// Starts a vault on `dir`, listening on `port` (0 for any).
    async fn start(dir: &Path, port: u16) -> Self {
        Self::start_with(dir, port, LIVENESS_TIMEOUT).await
    }

    /// Starts a vault that takes a host silent for `liveness` for gone.
    async fn start_with(dir: &Path, port: u16, liveness: Duration) -> Self {
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
                let server = Server::new(tls, Arc::clone(&auth), store, host, liveness);
                started.send((addr, fingerprint, auth)).ok().unwrap();
                server.run(listener, CancellationToken::new()).await;
            }
        });
        let (addr, fingerprint, auth) = ready.await.unwrap();
        Self {
            runtime,
            addr,
            fingerprint,
            auth,
        }
    }

    /// Where a host replicates to, pairing with a fresh code.
    fn config(&self) -> VaultConfig {
        VaultConfig {
            address: self.addr.to_string(),
            fingerprint: self.fingerprint.clone(),
            pairing_code: Some(self.auth.mint("host-1", None, PAIRING_TTL).unwrap().code),
        }
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
}

impl HostDaemon {
    async fn start(dir: &Path, vault: VaultConfig) -> Self {
        let runtime = Runtime::new();
        let dir = dir.to_owned();
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
                    data_dir: dir.clone(),
                };
                tokio::spawn(replicator.run(shutdown));
                sessions
            })
            .await
            .unwrap();
        Self { runtime, sessions }
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
                branch: format!("herder/s{s}"),
                provider: Provider::Claude,
                account_id: AccountId::new("main"),
                model: "m0".into(),
                permission_mode: PermissionMode::Ask,
                parent: None,
                task: None,
                max_children: None,
                failover_pin: None,
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
    assert_eq!(summaries[0].branch, "herder/s1");
    assert_eq!(summaries[0].project_id.as_str(), "host-1:/home/dev/herder");
    host.runtime.kill().await;
    vault.runtime.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_vault_restarted_mid_stream_gets_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, vault_dir) = (tmp.path().join("host"), tmp.path().join("vault"));
    std::fs::create_dir_all(&host_dir).unwrap();
    // More than the replicator's window of unacknowledged events, so the kill comes mid-stream.
    seed(&host_dir, 2, 1000);
    let vault = Vault::start(&vault_dir, 0).await;
    let addr = vault.addr;
    let host = HostDaemon::start(&host_dir, vault.config()).await;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while Vault::held(&vault_dir).is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "nothing replicated");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    vault.runtime.kill().await;
    // The host keeps working while the vault is down.
    host.switch_model("s1", "while-down").await;
    assert_gap_free(&vault_dir);

    let vault = Vault::start(&vault_dir, addr.port()).await;
    caught_up(&host_dir, &vault_dir).await;
    assert_gap_free(&vault_dir);
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
    let paired = client.pair(vault.pairing_link("alice")).await.unwrap();
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

    // Mutating commands are refused, naming the owning host.
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
        force: true,
    };
    assert!(matches!(
        client.send(vault_id.clone(), archive).await,
        Err(Error::Rejected { info: error }) if error.code == ErrorCode::ReadOnly
    ));
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
async fn a_silent_host_is_offline_after_the_liveness_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    let (host_dir, vault_dir) = (tmp.path().join("host"), tmp.path().join("vault"));
    std::fs::create_dir_all(&host_dir).unwrap();
    seed(&host_dir, 1, 0);
    let liveness = Duration::from_millis(500);
    let vault = Vault::start_with(&vault_dir, 0, liveness).await;
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
        .unwrap()
        .host_id;

    // The host's device comes back but falls silent after its hello, as a host whose machine
    // hangs or whose network drops without closing the connection would.
    let device = Replicator::device_key(&host_dir).unwrap();
    let config = client_config(&vault.fingerprint, &device).unwrap();
    let tcp = tokio::net::TcpStream::connect(vault.addr).await.unwrap();
    let tls = TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from("herder").unwrap(), tcp)
        .await
        .unwrap();
    let request = format!("wss://{}/", vault.addr)
        .into_client_request()
        .unwrap();
    let (mut ws, _) = tokio_tungstenite::client_async(request, tls).await.unwrap();
    let hello = HostMessage::Hello(HostHello {
        replication_version: REPLICATION_VERSION,
        host_id: host_id(),
        host_name: "devbox".into(),
        build: "test".into(),
        pairing_code: None,
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
