//! A daemon for the bindings' tests and samples: in-process, on localhost, with one account
//! on the fake adapter replaying `fixtures/hello.jsonl`, another replaying `fixtures/hold.jsonl`,
//! a third replaying `fixtures/approval.jsonl`, a fourth replaying `fixtures/tools.jsonl`, and a
//! git repository to create a session on.
//! It admits [`MAX_TURNS`] turns at once on a host that always has room otherwise, and an
//! owner may change that limit.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, ensure};
use herder_adapters::fake::FakeAdapter;
use herder_client_core::PairingUri;
use herder_daemon::auth::{Auth, PAIRING_TTL};
use herder_daemon::login::Logins;
use herder_daemon::resources::{Admission, ReadHost, Reading, ResourcesConfig};
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

/// An account whose turn runs until it is interrupted, for prompts to queue behind it.
pub const HOLD_ACCOUNT: &str = "hold";

/// An account whose turn waits on an approval, for the apps to show a request.
pub const APPROVAL_ACCOUNT: &str = "approval";

/// An account whose turns run long tool calls, for the apps to lay out rows wider than a phone;
/// its label is as long as real ones get.
pub const TOOLS_ACCOUNT: &str = "tools";

/// Turns the daemon runs at once until an owner changes it.
pub const MAX_TURNS: u32 = 4;

/// A host with room for every turn: 8 cores, 16 GiB, half of it available.
struct Roomy;

impl ReadHost for Roomy {
    fn read(&self) -> Result<Reading> {
        Ok(Reading {
            memory_total: 16 << 30,
            memory_available: 8 << 30,
            load_1m: 1.0,
            cpu_percent: 12.0,
            pressure: None,
        })
    }
}

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
    /// Starts the daemon of host `name` on the current tokio runtime.
    pub async fn start(name: &str) -> Result<Self> {
        let tmp = tempfile::tempdir()?;
        let dir = tmp.path().join("daemon");
        let repo = repo(&tmp.path().join("app"))?;
        std::fs::create_dir_all(dir.join("tls"))?;

        let tls = Tls::load_or_create(&dir.join("tls"), name)?;
        let auth = Arc::new(Auth::open(&dir)?);
        let hub = Arc::new(Hub::default());
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        let mut adapters = Adapters::new();
        let mut accounts = Accounts::new();
        for (account, label, script) in [
            (ACCOUNT, "Fake", "hello.jsonl"),
            (HOLD_ACCOUNT, "Hold", "hold.jsonl"),
            (APPROVAL_ACCOUNT, "Approval", "approval.jsonl"),
            (TOOLS_ACCOUNT, "Claude Max · team workspace", "tools.jsonl"),
        ] {
            // Each account has a provider of its own, as a provider has one script.
            let provider = Provider::Other(account.into());
            adapters.register(
                provider.clone(),
                Arc::new(FakeAdapter::new(fixtures.join(script))),
            );
            accounts.insert(
                AccountId::new(account),
                AccountConfig {
                    provider,
                    label: label.into(),
                    config_dir: Some(dir.join(account)),
                },
            );
        }
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
        let resources = ResourcesConfig {
            max_turns: Some(MAX_TURNS),
            ..ResourcesConfig::default()
        };
        let admission = Arc::new(Admission::new(resources.budget(8), Box::new(Roomy)));
        sessions.admit_turns(Arc::clone(&admission), dir.join("daemon.toml"))?;
        tokio::spawn({
            let (hub, shutdown) = (Arc::clone(&hub), shutdown.clone());
            async move { admission.run(&hub, shutdown).await }
        });
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let link = PairingUri {
            hosts: vec![listener.local_addr()?.to_string()],
            fingerprint: tls.fingerprint().to_owned(),
            code: auth.mint("sample", None, PAIRING_TTL)?.code,
        };
        let terminals = Terminals::new(Arc::clone(&hub), PathBuf::from("/bin/sh"));
        let logins = Logins::new(HashMap::new(), dir.join("daemon.toml"), sessions.clone());
        let host = Host {
            id: HostId::new(name),
            name: name.into(),
        };
        sessions.fork_from(session::fork::Forks {
            host: host.id.clone(),
            vault: None,
        })?;
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
