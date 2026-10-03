//! A daemon for the bindings' tests and samples: in-process, on localhost, with one account
//! on the fake adapter replaying `fixtures/hello.jsonl`, and a git repository to create a
//! session on.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, ensure};
use herder_adapters::fake::FakeAdapter;
use herder_client_core::PairingUri;
use herder_daemon::auth::{Auth, PAIRING_TTL};
use herder_daemon::login::Logins;
use herder_daemon::session::{AccountConfig, Accounts, Adapters, SessionManager, Setup};
use herder_daemon::terminal::Terminals;
use herder_daemon::worktree::Worktrees;
use herder_daemon::ws::{Host, Server, Tls};
use herder_daemon::{Hub, session};
use herder_protocol::{AccountId, HostId, Provider, TurnId};
use herder_store::Store;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// The account sessions run on.
pub const ACCOUNT: &str = "fake";

/// A running daemon; [`FakeDaemon::stop`] stops it and removes everything it created.
pub struct FakeDaemon {
    /// A pairing link with a fresh code, for user `sample`, who becomes the owner.
    pub link: String,
    /// Absolute path of a git repository with one commit.
    pub repo: String,
    shutdown: CancellationToken,
    server: JoinHandle<()>,
    _tmp: TempDir,
}

impl FakeDaemon {
    /// Starts the daemon on the current tokio runtime.
    pub async fn start() -> Result<Self> {
        let tmp = tempfile::tempdir()?;
        let dir = tmp.path().join("daemon");
        let repo = repo(&tmp.path().join("app"))?;
        std::fs::create_dir_all(dir.join("tls"))?;

        let tls = Tls::load_or_create(&dir.join("tls"), "fake-host")?;
        let auth = Arc::new(Auth::open(&dir)?);
        let hub = Arc::new(Hub::default());
        let fake = Provider::Other("fake".into());
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/hello.jsonl");
        let mut adapters = Adapters::new();
        adapters.register(fake.clone(), Arc::new(FakeAdapter::new(script)));
        let mut accounts = Accounts::new();
        accounts.insert(
            AccountId::new(ACCOUNT),
            AccountConfig {
                provider: fake,
                label: "Fake".into(),
                config_dir: Some(dir.join("account")),
            },
        );
        let turns = AtomicU64::new(0);
        let setup = Setup {
            store: Store::open(dir.join("herder.db"))?,
            adapters,
            accounts,
            sink: Arc::clone(&hub) as Arc<dyn session::EventSink>,
            turn_ids: Box::new(move || {
                TurnId::new(format!("turn-{}", turns.fetch_add(1, Ordering::SeqCst) + 1))
            }),
            worktrees: Worktrees::new(dir.join("worktrees")),
            attachments: dir.join("attachments"),
        };
        let shutdown = CancellationToken::new();
        let sessions = SessionManager::open(setup, shutdown.clone()).await?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let link = PairingUri {
            hosts: vec![listener.local_addr()?.to_string()],
            fingerprint: tls.fingerprint().to_owned(),
            code: auth.mint("sample", None, PAIRING_TTL)?.code,
        };
        let terminals = Terminals::new(Arc::clone(&hub), PathBuf::from("/bin/sh"));
        let logins = Logins::new(HashMap::new(), dir.join("daemon.toml"), sessions.clone());
        let host = Host {
            id: HostId::new("fake-host"),
            name: "fake-host".into(),
        };
        let server = Server::new(tls, auth, hub, sessions, terminals, logins, host);
        let server = tokio::spawn(server.run(listener, shutdown.clone()));
        Ok(Self {
            link: link.to_string(),
            repo,
            shutdown,
            server,
            _tmp: tmp,
        })
    }

    /// Stops the daemon and waits until it has.
    pub async fn stop(self) -> Result<()> {
        self.shutdown.cancel();
        Ok(self.server.await?)
    }
}

/// A git repository with one commit, for the session to work on.
fn repo(dir: &Path) -> Result<String> {
    std::fs::create_dir(dir)?;
    for args in [
        &["init", "--quiet", "--initial-branch=main"][..],
        &["commit", "--quiet", "--allow-empty", "-m", "init"],
    ] {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=herder",
                "-c",
                "user.email=herder@example.com",
            ])
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .status()?;
        ensure!(status.success(), "git {args:?} failed");
    }
    dir.to_str()
        .map(str::to_owned)
        .context("the repository path is not UTF-8")
}
