//! Handing a session off between machines: three real daemons, with the fake adapter, over
//! TLS on localhost, each with a clone of one repository, and no vault that answers. The client
//! relays a session's history from its machine to another, whether that one has a vault or
//! not; once the session's machine is offline, the destination's vault is the only way.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use herder_adapters::fake::FakeAdapter;
use herder_adapters::{Adapter, StartFuture, StartRequest};
use herder_client_core::{Client, ConnectionState, Error, PairResult};
use herder_daemon::Hub;
use herder_daemon::auth::{Auth, PAIRING_TTL};
use herder_daemon::config::VaultConfig;
use herder_daemon::login::Logins;
use herder_daemon::projects::{Discovery, OnSessionsChanged, Overrides, ProjectsConfig};
use herder_daemon::session::{AccountConfig, Accounts, Adapters, EventSink, SessionManager, Setup};
use herder_daemon::terminal::Terminals;
use herder_daemon::vault::{Link, LinkSetup};
use herder_daemon::worktree::{Worktrees, checkpoint};
use herder_daemon::ws::{Host, Server, Tls};
use herder_protocol::{
    AccountId, Bytes, CommandBody, CommandResult, ErrorCode, Event, EventBody, HostId, Image,
    ItemBody, PermissionMode, ProjectId, Provider, SessionId, SessionStatus, TurnId,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(30);

fn account() -> AccountId {
    AccountId::new("account-1")
}

fn project() -> ProjectId {
    ProjectId::new("github.com/acme/app")
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

/// A bare origin with one commit, cloned at `<dir>/app` for each of `dirs`. Each clone's
/// origin is `acme/app` on GitHub, which git rewrites to the bare one, so checkpoints reach it.
fn clones(root: &Path, dirs: &[&Path]) -> PathBuf {
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
    let github = "git@github.com:acme/app.git";
    for dir in dirs {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["clone", "--quiet", origin.to_str().unwrap(), "app"]);
        let app = dir.join("app");
        git(&app, &["remote", "set-url", "origin", github]);
        let rewrite = format!("url.{}.insteadOf", origin.display());
        git(&app, &["config", &rewrite, github]);
    }
    origin
}

/// The fake adapter, taking images.
struct Seeing(FakeAdapter);

impl Adapter for Seeing {
    fn accepts_images(&self) -> bool {
        true
    }

    fn start(&self, request: StartRequest) -> StartFuture {
        self.0.start(request)
    }
}

/// A running daemon.
struct Daemon {
    link: String,
    repo: String,
    shutdown: CancellationToken,
}

/// Starts a daemon on `<dir>/app`, with project discovery, checkpoints and `vault` as its
/// `[vault]`; pairs as its owner with the returned link.
async fn daemon(dir: &Path, id: &str, vault: Option<VaultConfig>) -> Daemon {
    let shutdown = CancellationToken::new();
    std::fs::create_dir_all(dir.join("tls")).unwrap();
    let tls = Tls::load_or_create(&dir.join("tls"), id).unwrap();
    let auth = Arc::new(Auth::open(dir).unwrap());
    let hub = Arc::new(Hub::default());
    let fake = Provider::Other("fake".into());
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../herder-daemon/fixtures/session/fork_a.jsonl");
    let mut adapters = Adapters::new();
    adapters.register(fake.clone(), Arc::new(Seeing(FakeAdapter::new(script))));
    let mut accounts = Accounts::new();
    accounts.insert(
        account(),
        AccountConfig {
            provider: fake,
            label: "Account 1".into(),
            config_dir: Some(dir.join("account")),
            fallback: false,
        },
    );
    let sessions_changed = Arc::new(tokio::sync::Notify::new());
    let setup = Setup {
        store: herder_store::Store::open(dir.join("herder.db")).unwrap(),
        adapters,
        accounts,
        sink: Arc::new(OnSessionsChanged {
            next: Arc::clone(&hub) as Arc<dyn EventSink>,
            notify: Arc::clone(&sessions_changed),
        }),
        turn_ids: {
            let turns = AtomicU64::new(0);
            Box::new(move || {
                TurnId::new(format!("turn-{}", turns.fetch_add(1, Ordering::SeqCst) + 1))
            })
        },
        worktrees: Worktrees::new(dir.join("worktrees")),
        attachments: dir.join("attachments"),
    };
    let sessions = SessionManager::open(setup, shutdown.clone()).await.unwrap();
    sessions
        .checkpoint_turns(checkpoint::Config {
            dir: dir.join("checkpoints"),
            keep: checkpoint::KEEP,
            push_timeout: TIMEOUT,
        })
        .unwrap();
    let host = Host {
        id: HostId::new(id),
        name: id.into(),
    };
    tokio::spawn(
        Discovery {
            host: host.id.clone(),
            config: Arc::new(Overrides::new(
                dir.join("daemon.toml"),
                dir.join("project-icons"),
                ProjectsConfig::default(),
            )),
            hub: Arc::clone(&hub),
            sessions: sessions.clone(),
            sessions_changed,
            data_dir: dir.join("data"),
        }
        .run(shutdown.clone()),
    );
    let link = Link::start(
        LinkSetup {
            config_file: dir.join("daemon.toml"),
            host: host.clone(),
            sessions: sessions.clone(),
            changed: Arc::new(tokio::sync::Notify::new()),
            data_dir: dir.to_owned(),
            shutdown: shutdown.clone(),
        },
        vault,
    )
    .unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let fingerprint = tls.fingerprint().to_owned();
    let terminals = Terminals::new(Arc::clone(&hub), PathBuf::from("/bin/sh"));
    let logins = Logins::new(HashMap::new(), dir.join("daemon.toml"), sessions.clone());
    let server = Server::new(
        tls,
        Arc::clone(&auth),
        hub,
        sessions,
        terminals,
        logins,
        host,
    );
    server.link_vault(Arc::new(link)).unwrap();
    tokio::spawn(server.run(vec![listener], shutdown.clone()));
    let code = auth.mint("alice", None, PAIRING_TTL).unwrap().code;
    let link = herder_client_core::PairingUri {
        hosts: vec![addr.to_string()],
        fingerprint,
        code,
    }
    .to_string();
    Daemon {
        link,
        repo: dir.join("app").to_str().unwrap().to_owned(),
        shutdown,
    }
}

/// A vault nothing answers at.
fn dead_vault() -> VaultConfig {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    VaultConfig::new(address, "00".repeat(32), None)
}

async fn create(client: &Client, host: &str, repo: &str) -> SessionId {
    let command = CommandBody::CreateSession {
        repo: Some(repo.to_owned()),
        project_id: None,
        branch: None,
        account_id: Some(account()),
        provider: None,
        model: None,
        permission_mode: Some(PermissionMode::Ask),
        failover_pin: None,
    };
    let created = client.send(HostId::new(host), command).await.unwrap();
    let CommandResult::SessionCreated { session_id } = created else {
        panic!("expected a session, got {created:?}");
    };
    session_id
}

/// Waits until `done` holds of the client's machines.
async fn until(client: &Client, what: &str, done: impl Fn(&[herder_client_core::Machine]) -> bool) {
    let changes = client.changes();
    tokio::time::timeout(TIMEOUT, async {
        while !done(&client.machines()) {
            assert!(changes.next().await);
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting until {what}"));
}

/// The whole journal of a session, as the client streams it.
async fn journal(client: &Client, host: &str, session_id: &SessionId) -> Vec<Event> {
    let host = HostId::new(host);
    let subscription = client
        .subscribe_session(host.clone(), session_id.clone())
        .unwrap();
    client.synced(host).await.unwrap();
    subscription.next().await.unwrap().events
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_hands_off_to_another_machine_through_the_client() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = ["a", "b", "c"].map(|name| tmp.path().join(name));
    let origin = clones(tmp.path(), &[&dirs[0], &dirs[1], &dirs[2]]);
    // A runs the session, B has no vault, and C's vault does not answer.
    let a = daemon(&dirs[0], "host-a", None).await;
    let b = daemon(&dirs[1], "host-b", None).await;
    let c = daemon(&dirs[2], "host-c", Some(dead_vault())).await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "herder-tui/test".into(),
    )
    .unwrap();
    for daemon in [&a, &b, &c] {
        let paired = client.pair(daemon.link.clone()).await.unwrap();
        assert!(
            matches!(&paired[..], [PairResult::Paired { .. }]),
            "{paired:?}"
        );
    }
    let session_id = create(&client, "host-a", &a.repo).await;
    // B and C know their clones of the project by a session of their own.
    create(&client, "host-b", &b.repo).await;
    create(&client, "host-c", &c.repo).await;
    until(&client, "every machine resolved its project", |machines| {
        machines.iter().all(|m| {
            !m.sessions.is_empty() && m.sessions.iter().all(|s| s.project_id == Some(project()))
        })
    })
    .await;

    // A turn with an image, checkpointed to origin.
    let image = Image {
        media_type: "image/png".into(),
        data: Bytes(b"\x89PNG\r\n\x1a\nscreenshot".to_vec()),
    };
    client
        .send(
            HostId::new("host-a"),
            CommandBody::SendPrompt {
                session_id: session_id.clone(),
                text: "First.".into(),
                images: vec![image.clone()],
                files: Vec::new(),
            },
        )
        .await
        .unwrap();
    until(&client, "the turn ended", |machines| {
        machines[0].sessions.iter().any(|s| {
            s.session_id == session_id && s.head_seq > 3 && s.status == SessionStatus::Idle
        })
    })
    .await;
    let checkpoint = format!("refs/herder/{session_id}/turn-");
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while !git(&origin, &["for-each-ref", "--format=%(refname)"]).contains(&checkpoint) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no checkpoint on origin"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let a_journal = journal(&client, "host-a", &session_id).await;

    // A is up: B, without a vault, and C, with one, both take the history relayed from A.
    for destination in ["host-b", "host-c"] {
        let forked = client
            .fork_session(
                HostId::new("host-a"),
                session_id.clone(),
                HostId::new(destination),
                None,
            )
            .await
            .unwrap();
        let CommandResult::SessionForked {
            session_id: fork,
            forked_from,
            from_host_id,
            ..
        } = forked
        else {
            panic!("expected a fork, got {forked:?}");
        };
        assert_eq!(
            (&forked_from, from_host_id.as_str()),
            (&session_id, "host-a")
        );
        let copy = journal(&client, destination, &fork).await;
        for (copied, original) in copy.iter().zip(&a_journal).skip(1) {
            assert_eq!((copied.seq, &copied.body), (original.seq, &original.body));
        }
        assert!(copy[a_journal.len()..].iter().any(|event| matches!(
            &event.body,
            EventBody::SessionForked { from_session, from_host }
                if *from_session == session_id && from_host.as_str() == "host-a"
        )));
        // The image came along.
        let attachment_id = copy
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
        let fetched = client
            .send(
                HostId::new(destination),
                CommandBody::GetAttachment {
                    session_id: fork.clone(),
                    attachment_id,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            fetched,
            CommandResult::Attachment {
                media_type: image.media_type.clone(),
                data: image.data.clone(),
            }
        );
    }

    // A session forks onto its own machine from its own journal.
    let local = client
        .fork_session(
            HostId::new("host-a"),
            session_id.clone(),
            HostId::new("host-a"),
            None,
        )
        .await
        .unwrap();
    assert!(
        matches!(local, CommandResult::SessionForked { .. }),
        "{local:?}"
    );

    // A goes offline: B has no vault to find the session in, and C asks its vault.
    a.shutdown.cancel();
    until(&client, "host-a is offline", |machines| {
        !matches!(machines[0].connection, ConnectionState::Connected)
    })
    .await;
    let refused = client
        .fork_session(
            HostId::new("host-a"),
            session_id.clone(),
            HostId::new("host-b"),
            None,
        )
        .await
        .unwrap_err();
    let Error::Rejected { info } = refused else {
        panic!("expected a refusal, got {refused:?}");
    };
    assert_eq!(info.code, ErrorCode::NotFound);
    assert_eq!(
        info.message,
        "host-a is offline and host-b has no vault to find the session in"
    );
    let refused = client
        .fork_session(
            HostId::new("host-a"),
            session_id.clone(),
            HostId::new("host-c"),
            None,
        )
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("vault"), "{refused}");
    b.shutdown.cancel();
    c.shutdown.cancel();
}
