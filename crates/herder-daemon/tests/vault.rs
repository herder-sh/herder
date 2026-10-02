//! A host daemon replicating to a vault daemon, both in process over TLS on localhost: sessions
//! appear in the vault, a vault restarted mid-stream gets the rest, and a host that was offline
//! catches up when it is back.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use herder_daemon::Hub;
use herder_daemon::auth::{Auth, PAIRING_TTL};
use herder_daemon::config::VaultConfig;
use herder_daemon::session::{Accounts, Adapters, EventSink, SessionManager, Setup};
use herder_daemon::vault::{Replicator, Server, VaultStore, WakeOnEvent};
use herder_daemon::worktree::Worktrees;
use herder_daemon::ws::{Host, Tls};
use herder_protocol::{
    AccountId, CommandBody, EventBody, HostId, JournalRecord, PermissionMode, Provider, SessionId,
    SessionSummary, Timestamp, TurnId, UserId,
};
use herder_store::{NewEvent, Store};
use tokio::net::TcpListener;
use tokio::sync::Notify;
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
                let server = Server::new(tls, Arc::clone(&auth), store);
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
