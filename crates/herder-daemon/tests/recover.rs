//! Recovering a session from a dead host: host A runs a session and dies mid-turn, host B
//! recovers it from the vault and goes on with it, and A, back again, keeps its copy
//! read-only. Hosts and vault run in process over TLS on localhost, each on its own runtime so
//! killing one drops every task at once, as a killed process would.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use herder_adapters::fake::FakeAdapter;
use herder_adapters::{Adapter, StartFuture, StartRequest};
use herder_daemon::Hub;
use herder_daemon::auth::{Auth, PAIRING_TTL};
use herder_daemon::config::VaultConfig;
use herder_daemon::session::{AccountConfig, Accounts, Adapters, EventSink, SessionManager, Setup};
use herder_daemon::vault::recover::{Recovery, Request};
use herder_daemon::vault::{LIVENESS_TIMEOUT, Replicator, Server, VaultStore, WakeOnEvent};
use herder_daemon::worktree::{Worktrees, checkpoint};
use herder_protocol::{
    AccountId, CommandBody, CommandResult, ErrorCode, Event, EventBody, HostId, ItemBody,
    PermissionMode, Project, ProjectId, Provider, SessionId, SessionStatus, TurnId, UserId,
};
use herder_store::Store;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(30);

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

    /// Runs `task` on this runtime and waits for its result.
    async fn run<T: Send + 'static>(&self, task: impl Future<Output = T> + Send + 'static) -> T {
        self.0.as_ref().unwrap().spawn(task).await.unwrap()
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
    _runtime: Runtime,
    config: VaultConfig,
    auth: Arc<Auth>,
}

impl Vault {
    async fn start(dir: &Path) -> Self {
        let runtime = Runtime::new();
        let dir = dir.to_owned();
        let (addr, fingerprint, auth) = runtime
            .run(async move {
                std::fs::create_dir_all(dir.join("tls")).unwrap();
                let tls =
                    herder_daemon::ws::Tls::load_or_create(&dir.join("tls"), "vault").unwrap();
                let auth = Arc::new(Auth::open(&dir).unwrap());
                let store = VaultStore::open(dir.join("vault.db")).unwrap();
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                let fingerprint = tls.fingerprint().to_owned();
                let host = herder_daemon::ws::Host {
                    id: HostId::new("vault"),
                    name: "vault".into(),
                };
                let server = Server::new(tls, Arc::clone(&auth), store, host, LIVENESS_TIMEOUT);
                tokio::spawn(server.run(listener, CancellationToken::new()));
                (addr, fingerprint, auth)
            })
            .await;
        let config = VaultConfig {
            address: addr.to_string(),
            fingerprint,
            pairing_code: None,
        };
        Self {
            _runtime: runtime,
            config,
            auth,
        }
    }

    /// Where a host replicates to, pairing as `user` with a fresh code.
    fn config(&self, user: &str) -> VaultConfig {
        VaultConfig {
            pairing_code: Some(self.auth.mint(user, None, PAIRING_TTL).unwrap().code),
            ..self.config.clone()
        }
    }
}

fn fake() -> Provider {
    Provider::Other("fake".into())
}

fn project() -> ProjectId {
    ProjectId::new("github.com/org/app")
}

fn alice() -> UserId {
    UserId::new("alice")
}

/// Plays a fake script and keeps every start's seed.
struct Seeds {
    adapter: FakeAdapter,
    seeds: Arc<Mutex<Vec<Vec<String>>>>,
}

impl Adapter for Seeds {
    fn start(&self, request: StartRequest) -> StartFuture {
        let texts = request
            .seed
            .iter()
            .filter_map(|item| match &item.body {
                ItemBody::UserMessage { text, .. } | ItemBody::AssistantMessage { text } => {
                    Some(text.clone())
                }
                _ => None,
            })
            .collect();
        self.seeds.lock().unwrap().push(texts);
        self.adapter.start(request)
    }
}

/// A host daemon: the fake provider on one account, a clone of the project, checkpoints, a
/// replicator and recovery.
struct HostDaemon {
    runtime: Runtime,
    sessions: SessionManager,
    recovery: Arc<Recovery>,
    repo: PathBuf,
    seeds: Arc<Mutex<Vec<Vec<String>>>>,
}

impl HostDaemon {
    async fn start(
        dir: &Path,
        host: &str,
        account: &str,
        script: &str,
        vault: VaultConfig,
    ) -> Self {
        let runtime = Runtime::new();
        let (dir, host, account) = (dir.to_owned(), host.to_owned(), account.to_owned());
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/session")
            .join(script);
        let seeds = Arc::new(Mutex::new(Vec::new()));
        let repo = dir.join("app");
        let started = {
            let (seeds, repo) = (Arc::clone(&seeds), repo.clone());
            runtime.run(async move {
                let mut adapters = Adapters::new();
                let adapter = Seeds {
                    adapter: FakeAdapter::new(script),
                    seeds,
                };
                adapters.register(fake(), Arc::new(adapter));
                let mut accounts = Accounts::new();
                accounts.insert(
                    AccountId::new(account),
                    AccountConfig {
                        provider: fake(),
                        label: "Account".into(),
                        config_dir: Some(dir.join("account")),
                    },
                );
                let changed = Arc::new(Notify::new());
                let sink = Arc::new(WakeOnEvent {
                    next: Arc::new(Hub::default()) as Arc<dyn EventSink>,
                    notify: Arc::clone(&changed),
                });
                let prefix = if host == "host-a" { "turn" } else { "b-turn" };
                let turns = AtomicU64::new(0);
                let setup = Setup {
                    store: Store::open(dir.join("herder.db")).unwrap(),
                    adapters,
                    accounts,
                    sink,
                    turn_ids: Box::new(move || {
                        TurnId::new(format!(
                            "{prefix}-{}",
                            turns.fetch_add(1, Ordering::SeqCst) + 1
                        ))
                    }),
                    worktrees: Worktrees::new(dir.join("worktrees")),
                    attachments: dir.join("attachments"),
                };
                let shutdown = CancellationToken::new();
                let sessions = SessionManager::open(setup, shutdown.clone()).await.unwrap();
                sessions
                    .checkpoint_turns(checkpoint::Config {
                        dir: dir.join("checkpoints"),
                        keep: checkpoint::KEEP,
                        push_timeout: TIMEOUT,
                    })
                    .unwrap();
                sessions
                    .set_projects(&[Project {
                        project_id: project(),
                        name: "app".into(),
                        paths: vec![repo.to_str().unwrap().to_owned()],
                        default_permission_mode: None,
                        default_account: None,
                        setup_command: None,
                    }])
                    .await;
                let me = herder_daemon::ws::Host {
                    id: HostId::new(&host),
                    name: host.clone(),
                };
                let replicator = Replicator {
                    vault: vault.clone(),
                    device: Replicator::device_key(&dir).unwrap(),
                    host: me.clone(),
                    sessions: sessions.clone(),
                    changed,
                    data_dir: dir.clone(),
                };
                tokio::spawn(replicator.run(shutdown));
                let recovery = Arc::new(Recovery {
                    vault,
                    device: Replicator::device_key(&dir).unwrap(),
                    host: me,
                    sessions: sessions.clone(),
                    data_dir: dir,
                });
                (sessions, recovery)
            })
        };
        let (sessions, recovery) = started.await;
        Self {
            runtime,
            sessions,
            recovery,
            repo,
            seeds,
        }
    }

    async fn handle(
        &self,
        command: CommandBody,
    ) -> Result<CommandResult, herder_protocol::ErrorInfo> {
        let sessions = self.sessions.clone();
        self.runtime
            .run(async move { sessions.handle(alice(), command).await })
            .await
    }

    async fn prompt(
        &self,
        session_id: &SessionId,
        text: &str,
    ) -> Result<CommandResult, herder_protocol::ErrorInfo> {
        self.handle(CommandBody::SendPrompt {
            session_id: session_id.clone(),
            text: text.into(),
            images: Vec::new(),
        })
        .await
    }

    async fn recover(
        &self,
        request: Request,
    ) -> anyhow::Result<herder_daemon::vault::recover::Outcome> {
        let recovery = Arc::clone(&self.recovery);
        self.runtime
            .run(async move { recovery.recover(request).await })
            .await
    }

    async fn journal(&self, session_id: &SessionId) -> Vec<Event> {
        let (sessions, session_id) = (self.sessions.clone(), session_id.clone());
        self.runtime
            .run(async move { sessions.read_since(&session_id, 0, 10_000).await.unwrap() })
            .await
    }

    /// Waits until the session's journal has an event `done` accepts; returns the journal.
    async fn journal_until(
        &self,
        session_id: &SessionId,
        done: impl Fn(&EventBody) -> bool,
    ) -> Vec<Event> {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            let journal = self.journal(session_id).await;
            if journal.iter().any(|event| done(&event.body)) {
                return journal;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out; the journal is {:#?}",
                journal.iter().map(|e| &e.body).collect::<Vec<_>>()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// Runs git in `dir`, panicking on failure; returns trimmed stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A bare `origin` with one commit on `main`, cloned as `<host>/app` for each host.
fn clones(root: &Path, hosts: &[&Path]) {
    let origin = root.join("origin.git");
    let seed = root.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(
        root,
        &[
            "init",
            "--quiet",
            "--bare",
            "--initial-branch=main",
            "origin.git",
        ],
    );
    git(&seed, &["init", "--quiet", "--initial-branch=main"]);
    std::fs::write(seed.join("README"), "app\n").unwrap();
    git(&seed, &["add", "README"]);
    git(&seed, &["commit", "--quiet", "-m", "init"]);
    git(
        &seed,
        &["push", "--quiet", origin.to_str().unwrap(), "main"],
    );
    for host in hosts {
        std::fs::create_dir_all(host).unwrap();
        git(host, &["clone", "--quiet", origin.to_str().unwrap(), "app"]);
    }
}

fn status(journal: &[Event]) -> Option<SessionStatus> {
    journal.iter().rev().find_map(|event| match event.body {
        EventBody::SessionStatusChanged { status } => Some(status),
        _ => None,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_of_a_dead_host_goes_on_on_another_and_stays_read_only_on_the_first() {
    let tmp = tempfile::tempdir().unwrap();
    let (a_dir, b_dir) = (tmp.path().join("a"), tmp.path().join("b"));
    clones(tmp.path(), &[&a_dir, &b_dir]);
    let vault = Vault::start(&tmp.path().join("vault")).await;

    // Host A: a first turn completes and is checkpointed to origin, a second is cut short.
    let a = HostDaemon::start(
        &a_dir,
        "host-a",
        "a-account",
        "recover_a.jsonl",
        vault.config("a"),
    )
    .await;
    let created = a
        .handle(CommandBody::CreateSession {
            repo: None,
            project_id: Some(project()),
            branch: None,
            account_id: Some(AccountId::new("a-account")),
            provider: None,
            model: None,
            permission_mode: Some(PermissionMode::Ask),
            max_children: None,
            failover_pin: None,
        })
        .await
        .unwrap();
    let CommandResult::SessionCreated { session_id } = created else {
        panic!("expected a created session, got {created:?}");
    };
    let a_worktree = a.sessions.worktree(&session_id).await.unwrap();
    std::fs::write(a_worktree.join("notes.txt"), "half done\n").unwrap();
    std::fs::write(a_worktree.join(".env"), "TOKEN=secret\n").unwrap();
    a.prompt(&session_id, "First.").await.unwrap();
    a.journal_until(&session_id, |body| {
        matches!(
            body,
            EventBody::SessionStatusChanged {
                status: SessionStatus::Idle
            }
        )
    })
    .await;
    let checkpoint = format!("refs/herder/{session_id}/turn-1");
    let origin = tmp.path().join("origin.git");
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while !git(&origin, &["for-each-ref", "--format=%(refname)"]).contains(&checkpoint) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the checkpoint never reached origin"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    a.prompt(&session_id, "Second.").await.unwrap();
    let a_journal = a
        .journal_until(
            &session_id,
            |body| matches!(body, EventBody::ItemAdded { item } if item.id.as_str() == "item-2"),
        )
        .await;

    // Host B, paired with the same vault, waits for A's journal to reach it.
    let b = HostDaemon::start(
        &b_dir,
        "host-b",
        "b-account",
        "recover_b.jsonl",
        vault.config("b"),
    )
    .await;
    let request = Request {
        session_id: session_id.clone(),
        account_id: None,
        force: false,
    };
    let vault_db = tmp.path().join("vault/vault.db");
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let held = VaultStore::open(&vault_db)
            .unwrap()
            .records(&HostId::new("host-a"), &session_id, 0, usize::MAX)
            .unwrap();
        if held.len() == a_journal.len() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the vault never held A's journal"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // A is online: no recovery without force.
    let refused = b.recover(request.clone()).await.unwrap_err().to_string();
    assert!(refused.contains("host-a (host-a) is online"), "{refused}");

    // A dies mid-turn; B recovers the session once the vault shows A offline.
    a.runtime.kill().await;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let outcome = loop {
        match b.recover(request.clone()).await {
            Ok(outcome) => break outcome,
            Err(err) if err.to_string().contains("is online") => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "A never went offline"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(err) => panic!("recovery failed: {err:#}"),
        }
    };
    assert_eq!(outcome.session.session_id, session_id);
    assert_eq!(outcome.origin.host_id, HostId::new("host-a"));
    assert_eq!(outcome.session.account_id, AccountId::new("b-account"));
    assert_eq!(
        outcome.session.checkpoint.as_deref(),
        Some(checkpoint.as_str())
    );
    // The worktree is as A left it, minus what checkpoints never take.
    let b_worktree = PathBuf::from(&outcome.session.worktree);
    assert!(b_worktree.starts_with(&b_dir));
    assert_eq!(
        std::fs::read_to_string(b_worktree.join("notes.txt")).unwrap(),
        "half done\n"
    );
    assert!(!b_worktree.join(".env").exists());
    assert_eq!(git(&b_worktree, &["status", "--porcelain"]), "?? notes.txt");
    assert_eq!(
        git(&b_worktree, &["branch", "--show-current"]),
        outcome.session.branch
    );
    // Its journal is A's, at the same seqs, with B's paths, then the switch to B's account
    // and the turn A left open, failed.
    let b_journal = b.journal(&session_id).await;
    let a_held = &b_journal[..a_journal.len()];
    for (b_event, a_event) in a_held.iter().zip(&a_journal).skip(1) {
        assert_eq!(b_event, a_event);
    }
    let EventBody::SessionCreated { repo, worktree, .. } = &a_held[0].body else {
        panic!("the journal starts with {:?}", a_held[0].body);
    };
    assert_eq!(repo, b.repo.to_str().unwrap());
    assert_eq!(worktree, &outcome.session.worktree);
    let tail: Vec<_> = b_journal[a_journal.len()..]
        .iter()
        .map(|e| &e.body)
        .collect();
    assert!(
        matches!(tail[0], EventBody::AccountSwitched { account_id } if account_id.as_str() == "b-account")
    );
    assert!(
        matches!(tail[1], EventBody::TurnFailed { turn_id, .. } if turn_id.as_str() == "turn-2")
    );
    assert_eq!(status(&b_journal), Some(SessionStatus::NeedsYou));

    // It goes on on B, its CLI seeded with the transcript.
    b.prompt(&session_id, "Third.").await.unwrap();
    b.journal_until(&session_id, |body| {
        matches!(body, EventBody::TurnCompleted { turn_id } if turn_id.as_str() == "b-turn-1")
    })
    .await;
    // Settled once idle: nothing more is journaled.
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let b_journal = loop {
        let journal = b.journal(&session_id).await;
        if status(&journal) == Some(SessionStatus::Idle) {
            break journal;
        }
        assert!(tokio::time::Instant::now() < deadline, "B never settled");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let seeds = b.seeds.lock().unwrap().clone();
    assert_eq!(seeds.len(), 1);
    assert_eq!(seeds[0][..3], ["First.", "One.", "Second."]);

    // The vault shows it on B, which replicates it on from where A stopped.
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let store = VaultStore::open(&vault_db).unwrap();
        let fleet = store.fleet().unwrap();
        let head = fleet.iter().find(|head| head.session_id == session_id);
        if head.is_some_and(|head| {
            head.host_id == Some(HostId::new("host-b")) && head.head_seq == b_journal.len() as u64
        }) {
            assert_eq!(fleet.len(), 1);
            assert_eq!(
                store
                    .recovered_to(&HostId::new("host-a"), &session_id)
                    .unwrap(),
                Some(HostId::new("host-b"))
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the vault never showed the session on B: {fleet:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // A comes back: its copy turns read-only, and the session stays B's.
    let a = HostDaemon::start(
        &a_dir,
        "host-a",
        "a-account",
        "recover_a.jsonl",
        vault.config("a"),
    )
    .await;
    let a_journal = a
        .journal_until(&session_id, |body| {
            matches!(
                body,
                EventBody::SessionStatusChanged {
                    status: SessionStatus::Moved
                }
            )
        })
        .await;
    assert_eq!(status(&a_journal), Some(SessionStatus::Moved));
    let refused = a.prompt(&session_id, "Fourth.").await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::Conflict);
    assert!(
        refused.message.contains("recovered on another host"),
        "{}",
        refused.message
    );
    assert!(a.sessions.worktree(&session_id).await.is_err());
    // A's copy reaches the vault but is never shown.
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let store = VaultStore::open(&vault_db).unwrap();
        let held = store
            .records(&HostId::new("host-a"), &session_id, 0, usize::MAX)
            .unwrap();
        if held.len() == a_journal.len() {
            let fleet = store.fleet().unwrap();
            assert_eq!(fleet.len(), 1);
            assert_eq!(fleet[0].host_id, Some(HostId::new("host-b")));
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "A's copy never reached the vault"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // There is no move back.
    let back = a.recover(request.clone()).await.unwrap_err().to_string();
    assert!(back.contains("is online"), "{back}");
    let again = a
        .recover(Request {
            force: true,
            ..request
        })
        .await
        .unwrap_err()
        .to_string();
    assert!(again.contains("never moves back"), "{again}");
}
