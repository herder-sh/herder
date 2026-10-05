//! Forking a session: host A runs a session that is mid-turn, host B forks it from the vault,
//! images included, and goes on with the fork, while A's session stays as it is; A forks it
//! too, from its own journal. Hosts and vault run in process over TLS on localhost, each on its
//! own runtime.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use herder_adapters::fake::FakeAdapter;
use herder_adapters::{Adapter, StartFuture, StartRequest};
use herder_daemon::Hub;
use herder_daemon::auth::{Auth, PAIRING_TTL};
use herder_daemon::config::{Retention, VaultConfig};
use herder_daemon::session::fork::Forks;
use herder_daemon::session::{AccountConfig, Accounts, Adapters, EventSink, SessionManager, Setup};
use herder_daemon::vault::fork::FromVault;
use herder_daemon::vault::{LIVENESS_TIMEOUT, Replicator, Server, VaultStore, WakeOnEvent};
use herder_daemon::worktree::{Worktrees, checkpoint};
use herder_protocol::{
    AccountId, Bytes, CommandBody, CommandResult, ErrorCode, Event, EventBody, HistoryPart, HostId,
    Image, ItemBody, PermissionMode, Project, ProjectId, Provider, Relay, SessionId, SessionStatus,
    TurnId, UserId,
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
                let server = Server::new(
                    tls,
                    Arc::clone(&auth),
                    store,
                    host,
                    LIVENESS_TIMEOUT,
                    Retention::default(),
                );
                tokio::spawn(server.run(vec![listener], CancellationToken::new()));
                (addr, fingerprint, auth)
            })
            .await;
        // Images are backed up, so a recovered session has them.
        let config = VaultConfig {
            attachments: true,
            ..VaultConfig::new(addr.to_string(), fingerprint, None)
        };
        Self {
            _runtime: runtime,
            config,
            auth,
        }
    }

    /// Where a host replicates to, pairing as `user` with a fresh client code: forking
    /// another host's session reads the vault.
    fn config(&self, user: &str) -> VaultConfig {
        VaultConfig {
            pairing_code: Some(self.auth.mint(user, None, PAIRING_TTL).unwrap().code),
            ..self.config.clone()
        }
    }

    /// Where a host replicates to, pairing as the host-only device `host`.
    fn host_config(&self, host: &str) -> VaultConfig {
        VaultConfig {
            pairing_code: Some(self.auth.mint_host(host, PAIRING_TTL).unwrap().code),
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
    fn accepts_images(&self) -> bool {
        true
    }

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
/// replicator and forks.
struct HostDaemon {
    runtime: Runtime,
    sessions: SessionManager,
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
                        icon: None,
                        icon_uploaded: false,
                        icon_background: None,
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
                };
                tokio::spawn(replicator.run(shutdown));
                sessions
                    .fork_from(Forks {
                        host: me.id,
                        vault: Some(FromVault {
                            vault,
                            device: Replicator::device_key(&dir).unwrap(),
                        }),
                    })
                    .unwrap();
                sessions
            })
        };
        let sessions = started.await;
        Self {
            runtime,
            sessions,
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
        self.prompt_with(session_id, text, Vec::new()).await
    }

    async fn prompt_with(
        &self,
        session_id: &SessionId,
        text: &str,
        images: Vec<Image>,
    ) -> Result<CommandResult, herder_protocol::ErrorInfo> {
        self.handle(CommandBody::SendPrompt {
            session_id: session_id.clone(),
            text: text.into(),
            images,
        })
        .await
    }

    /// The bytes of every image `journal` names, as this host answers `get_attachment`.
    async fn images(&self, session_id: &SessionId, journal: &[Event]) -> Vec<Image> {
        let mut images = Vec::new();
        for event in journal {
            let EventBody::ItemAdded { item } = &event.body else {
                continue;
            };
            let ItemBody::UserMessage { attachments, .. } = &item.body else {
                continue;
            };
            for attachment in attachments {
                let fetched = self
                    .handle(CommandBody::GetAttachment {
                        session_id: session_id.clone(),
                        attachment_id: attachment.attachment_id.clone(),
                    })
                    .await;
                let Ok(CommandResult::Attachment { media_type, data }) = fetched else {
                    panic!("expected an image, got {fetched:?}");
                };
                images.push(Image { media_type, data });
            }
        }
        images
    }

    /// Forks a session onto this host as a client's `fork_session` does; returns the fork.
    async fn fork(&self, session_id: &SessionId) -> SessionId {
        let forked = self
            .handle(CommandBody::ForkSession {
                session_id: session_id.clone(),
                account_id: None,
                relay: None,
            })
            .await
            .unwrap();
        let CommandResult::SessionForked {
            session_id: fork,
            forked_from,
            ..
        } = forked
        else {
            panic!("expected a fork, got {forked:?}");
        };
        assert_eq!(forked_from, *session_id);
        assert_ne!(fork, *session_id);
        fork
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
        EventBody::SessionStatusChanged { status, .. } => Some(status),
        _ => None,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_forks_onto_another_host_from_the_vault_and_onto_its_own() {
    let tmp = tempfile::tempdir().unwrap();
    let (a_dir, b_dir, c_dir) = (
        tmp.path().join("a"),
        tmp.path().join("b"),
        tmp.path().join("c"),
    );
    clones(tmp.path(), &[&a_dir, &b_dir, &c_dir]);
    let vault = Vault::start(&tmp.path().join("vault")).await;

    // Host A: a first turn completes and is checkpointed to origin, a second is still running.
    let a = HostDaemon::start(
        &a_dir,
        "host-a",
        "a-account",
        "fork_a.jsonl",
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
    let image = Image {
        media_type: "image/png".into(),
        data: Bytes(b"\x89PNG\r\n\x1a\nscreenshot".to_vec()),
    };
    a.prompt_with(&session_id, "First.", vec![image.clone()])
        .await
        .unwrap();
    a.journal_until(&session_id, |body| {
        matches!(
            body,
            EventBody::SessionStatusChanged {
                retry_at: None,
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
        "fork_b.jsonl",
        vault.config("b"),
    )
    .await;
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
    // Host C, paired host-only, replicates but cannot read the vault to fork anything.
    let c = HostDaemon::start(
        &c_dir,
        "host-c",
        "c-account",
        "fork_b.jsonl",
        vault.host_config("host-c"),
    )
    .await;
    let refused = c
        .handle(CommandBody::ForkSession {
            session_id: session_id.clone(),
            account_id: None,
            relay: None,
        })
        .await
        .unwrap_err();
    assert!(
        refused.message.contains("paired as a host"),
        "{}",
        refused.message
    );
    c.runtime.kill().await;

    // A is up and mid-turn: B forks its session all the same.
    let forked = b
        .handle(CommandBody::ForkSession {
            session_id: session_id.clone(),
            account_id: None,
            relay: None,
        })
        .await
        .unwrap();
    let CommandResult::SessionForked {
        session_id: fork,
        account_id,
        forked_from,
        from_host_id,
    } = forked
    else {
        panic!("expected a fork, got {forked:?}");
    };
    assert_ne!(fork, session_id);
    assert_eq!(
        (account_id.as_str(), &forked_from, from_host_id.as_str()),
        ("b-account", &session_id, "host-a")
    );
    // The worktree is as A's checkpoint left it, minus what checkpoints never take, on the
    // fork's own branch.
    let b_worktree = b.sessions.worktree(&fork).await.unwrap();
    assert!(b_worktree.starts_with(&b_dir));
    assert_eq!(
        std::fs::read_to_string(b_worktree.join("notes.txt")).unwrap(),
        "half done\n"
    );
    assert!(!b_worktree.join(".env").exists());
    assert_eq!(git(&b_worktree, &["status", "--porcelain"]), "?? notes.txt");
    // Its journal is A's, under the fork's id, with B's paths and branch, then the fork from
    // A and the switch to B's account, both by the user who forked it, and the turn A had
    // open, failed.
    let b_journal = b.journal(&fork).await;
    assert!(b_journal.iter().all(|event| event.session_id == fork));
    let copied = &b_journal[..a_journal.len()];
    for (b_event, a_event) in copied.iter().zip(&a_journal).skip(1) {
        assert_eq!((b_event.seq, &b_event.body), (a_event.seq, &a_event.body));
    }
    let EventBody::SessionCreated {
        repo,
        worktree,
        branch,
        ..
    } = &copied[0].body
    else {
        panic!("the journal starts with {:?}", copied[0].body);
    };
    assert_eq!(&git(&b_worktree, &["branch", "--show-current"]), branch);
    assert_ne!(
        branch,
        &git(
            &a.sessions.worktree(&session_id).await.unwrap(),
            &["branch", "--show-current"]
        )
    );
    assert_eq!(repo, b.repo.to_str().unwrap());
    assert_eq!(worktree, b_worktree.to_str().unwrap());
    let tail = &b_journal[a_journal.len()..];
    assert!(
        matches!(&tail[0].body, EventBody::SessionForked { from_session, from_host }
            if *from_session == session_id && from_host.as_str() == "host-a"),
        "{:?}",
        tail[0].body
    );
    assert!(
        matches!(&tail[1].body, EventBody::AccountSwitched { account_id } if account_id.as_str() == "b-account")
    );
    assert_eq!((&tail[0].by, &tail[1].by), (&Some(alice()), &Some(alice())));
    assert!(
        matches!(&tail[2].body, EventBody::TurnFailed { turn_id, .. } if turn_id.as_str() == "turn-2")
    );
    assert_eq!(status(&b_journal), Some(SessionStatus::NeedsYou));
    // B answers for the image A's prompt carried.
    assert_eq!(
        b.images(&fork, &b_journal).await,
        std::slice::from_ref(&image)
    );

    // The fork goes on on B, its CLI seeded with the transcript.
    b.prompt(&fork, "Third.").await.unwrap();
    b.journal_until(&fork, |body| {
        matches!(body, EventBody::TurnCompleted { turn_id, .. } if turn_id.as_str() == "b-turn-1")
    })
    .await;
    let seeds = b.seeds.lock().unwrap().clone();
    assert_eq!(seeds.len(), 1);
    assert_eq!(seeds[0][..3], ["First.", "One.", "Second."]);

    // A's session is as it was: still its own, still in its turn, and the vault lists both.
    let a_now = a.journal(&session_id).await;
    assert_eq!(a_now[..a_journal.len()], a_journal[..]);
    assert_eq!(status(&a_now), Some(SessionStatus::Running));
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let fleet = VaultStore::open(&vault_db).unwrap().fleet().unwrap();
        let hosts: Vec<_> = fleet
            .iter()
            .map(|head| (head.session_id.clone(), head.host_id.clone()))
            .collect();
        if hosts.len() == 2 {
            assert!(hosts.contains(&(session_id.clone(), Some(HostId::new("host-a")))));
            assert!(hosts.contains(&(fork.clone(), Some(HostId::new("host-b")))));
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the vault never listed the fork: {hosts:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // A forks its own session from its own journal and checkpoint, then again: each fork is
    // a session of its own.
    let local = a.fork(&session_id).await;
    let again = a.fork(&session_id).await;
    assert_ne!(local, again);
    let a_fork = a.sessions.worktree(&local).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(a_fork.join("notes.txt")).unwrap(),
        "half done\n"
    );
    let local_journal = a.journal(&local).await;
    let marks: Vec<_> = local_journal
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::SessionForked {
                from_session,
                from_host,
            } => Some((from_session, from_host.as_str(), &event.by)),
            _ => None,
        })
        .collect();
    assert_eq!(marks, [(&session_id, "host-a", &Some(alice()))]);
    assert_eq!(status(&local_journal), Some(SessionStatus::NeedsYou));
    assert_eq!(a.images(&local, &local_journal).await, [image]);
    assert_eq!(
        status(&a.journal(&session_id).await),
        Some(SessionStatus::Running)
    );

    // A is gone: B forks its session from the vault alone.
    a.runtime.kill().await;
    let after_death = b.fork(&session_id).await;
    assert_ne!(after_death, fork);
    assert!(b.sessions.worktree(&after_death).await.is_ok());

    // A session neither the host nor its vault holds is not found.
    let missing = b
        .handle(CommandBody::ForkSession {
            session_id: SessionId::new("nope"),
            account_id: None,
            relay: None,
        })
        .await
        .unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound, "{missing:?}");
}

/// A vault nothing answers at: a host configured with it forks only what it holds or is
/// relayed.
fn dead_vault() -> VaultConfig {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    VaultConfig::new(address, "00".repeat(32), None)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relayed_history_forks_onto_another_host_without_the_vault() {
    let tmp = tempfile::tempdir().unwrap();
    let (a_dir, b_dir) = (tmp.path().join("a"), tmp.path().join("b"));
    clones(tmp.path(), &[&a_dir, &b_dir]);
    let a = HostDaemon::start(&a_dir, "host-a", "a-account", "fork_a.jsonl", dead_vault()).await;
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
    let image = Image {
        media_type: "image/png".into(),
        data: Bytes(b"\x89PNG\r\n\x1a\nscreenshot".to_vec()),
    };
    a.prompt_with(&session_id, "First.", vec![image.clone()])
        .await
        .unwrap();
    let a_journal = a
        .journal_until(&session_id, |body| {
            matches!(
                body,
                EventBody::SessionStatusChanged {
                    retry_at: None,
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
    let attachment_id = a_journal
        .iter()
        .find_map(|event| match &event.body {
            EventBody::ItemAdded { item } => match &item.body {
                ItemBody::UserMessage { attachments, .. } => {
                    attachments.first().map(|a| a.attachment_id.clone())
                }
                _ => None,
            },
            _ => None,
        })
        .unwrap();

    // Host B has a vault, which does not answer: a relayed history needs none.
    let b = HostDaemon::start(&b_dir, "host-b", "b-account", "fork_b.jsonl", dead_vault()).await;
    let relay = || {
        Some(Relay {
            host_id: HostId::new("host-a"),
            project_id: project(),
        })
    };
    let fork_relayed = |relay| CommandBody::ForkSession {
        session_id: session_id.clone(),
        account_id: None,
        relay,
    };
    let upload = |events: Vec<Event>| CommandBody::UploadHistory {
        session_id: session_id.clone(),
        part: HistoryPart::Events { events },
    };

    // Nothing uploaded: nothing to fork.
    let refused = b.handle(fork_relayed(relay())).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::BadRequest, "{refused:?}");
    // An upload starts at the first event.
    let refused = b.handle(upload(a_journal[1..].to_vec())).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::BadRequest, "{refused:?}");
    // A history with a gap, one with another session's event, and a task child's are refused,
    // and each fork takes its upload, so the next starts afresh.
    let mut gap = a_journal.clone();
    gap.remove(2);
    let mut mixed = a_journal.clone();
    mixed[3].session_id = SessionId::new("other");
    let mut child = a_journal.clone();
    if let EventBody::SessionCreated { parent, .. } = &mut child[0].body {
        *parent = Some(SessionId::new("primary"));
    }
    for (history, code) in [
        (gap, ErrorCode::BadRequest),
        (mixed, ErrorCode::BadRequest),
        (child, ErrorCode::Unsupported),
    ] {
        b.handle(upload(history)).await.unwrap();
        let refused = b.handle(fork_relayed(relay())).await.unwrap_err();
        assert_eq!(refused.code, code, "{refused:?}");
        let again = b.handle(fork_relayed(relay())).await.unwrap_err();
        assert!(again.message.contains("no history"), "{again:?}");
    }
    // A history claiming to be of B itself is not taken.
    b.handle(upload(a_journal.clone())).await.unwrap();
    let refused = b
        .handle(fork_relayed(Some(Relay {
            host_id: HostId::new("host-b"),
            project_id: project(),
        })))
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::NotFound, "{refused:?}");
    // Without the relay, B looks in its vault, which does not answer.
    b.handle(upload(a_journal.clone())).await.unwrap();
    let refused = b.handle(fork_relayed(None)).await.unwrap_err();
    assert!(refused.message.contains("vault"), "{refused:?}");

    // The whole history, in two parts and the image, forks onto B.
    let (first, rest) = a_journal.split_at(2);
    b.handle(upload(first.to_vec())).await.unwrap();
    b.handle(upload(rest.to_vec())).await.unwrap();
    b.handle(CommandBody::UploadHistory {
        session_id: session_id.clone(),
        part: HistoryPart::Image {
            attachment_id,
            image: image.clone(),
        },
    })
    .await
    .unwrap();
    let forked = b.handle(fork_relayed(relay())).await.unwrap();
    let CommandResult::SessionForked {
        session_id: fork,
        account_id,
        forked_from,
        from_host_id,
    } = forked
    else {
        panic!("expected a fork, got {forked:?}");
    };
    assert_eq!(
        (account_id.as_str(), &forked_from, from_host_id.as_str()),
        ("b-account", &session_id, "host-a")
    );
    let b_worktree = b.sessions.worktree(&fork).await.unwrap();
    assert!(b_worktree.starts_with(&b_dir));
    assert_eq!(
        std::fs::read_to_string(b_worktree.join("notes.txt")).unwrap(),
        "half done\n"
    );
    // The history is imported as it was, under the fork's id, then marked as forked by the
    // user who forked it.
    let b_journal = b.journal(&fork).await;
    for (b_event, a_event) in b_journal.iter().zip(&a_journal).skip(1) {
        assert_eq!(
            (b_event.seq, &b_event.by, &b_event.body),
            (a_event.seq, &a_event.by, &a_event.body)
        );
    }
    let tail = &b_journal[a_journal.len()..];
    assert!(
        matches!(&tail[0].body, EventBody::SessionForked { from_session, from_host }
            if *from_session == session_id && from_host.as_str() == "host-a"),
        "{:?}",
        tail[0].body
    );
    assert_eq!(tail[0].by, Some(alice()));
    assert_eq!(b.images(&fork, &b_journal).await, [image]);
    // The upload was taken.
    let again = b.handle(fork_relayed(relay())).await.unwrap_err();
    assert!(again.message.contains("no history"), "{again:?}");
}
