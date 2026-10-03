//! A full turn from the app: a real daemon with the fake adapter, over TLS on localhost, and
//! the window wired to the client as `main` wires it. The session is opened from its row; a
//! prompt typed in the composer starts a turn whose reply streams, whose tool call waits on
//! the approval card's Allow, and which ends with the agent's answer; then the model is
//! switched from the composer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gtk::glib;
use herder_adapters::fake::FakeAdapter;
use herder_client_core::{Client, PairingUri};
use herder_daemon::Hub;
use herder_daemon::auth::{Auth, PAIRING_TTL};
use herder_daemon::login::Logins;
use herder_daemon::session::{AccountConfig, Accounts, Adapters, EventSink, SessionManager, Setup};
use herder_daemon::terminal::Terminals;
use herder_daemon::worktree::Worktrees;
use herder_daemon::ws::{Host, Server, Tls};
use herder_protocol::{AccountId, CommandBody, HostId, PermissionMode, Provider, TurnId};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::lists::SessionKey;
use crate::window::MainWindow;

const TIMEOUT: Duration = Duration::from_secs(30);

/// A git repository with one commit.
fn repo(dir: &Path) -> String {
    std::fs::create_dir_all(dir).expect("the repo dir");
    for args in [
        &["init", "--quiet", "--initial-branch=main"][..],
        &["commit", "--quiet", "--allow-empty", "-m", "init"],
    ] {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .status()
            .expect("git runs");
        assert!(status.success());
    }
    dir.to_str().expect("a UTF-8 dir").to_owned()
}

/// Starts a daemon on `dir` whose sessions run `script`; returns a pairing link.
async fn daemon(dir: &Path, script: PathBuf, shutdown: &CancellationToken) -> String {
    std::fs::create_dir_all(dir.join("tls")).expect("the tls dir");
    let tls = Tls::load_or_create(&dir.join("tls"), "box").expect("a certificate");
    let auth = Arc::new(Auth::open(dir).expect("the auth store"));
    let hub = Arc::new(Hub::default());
    let fake = Provider::Other("fake".into());
    let mut adapters = Adapters::new();
    adapters.register(fake.clone(), Arc::new(FakeAdapter::new(script)));
    let mut accounts = Accounts::new();
    accounts.insert(
        AccountId::new("account-1"),
        AccountConfig {
            provider: fake,
            label: "Account 1".into(),
            config_dir: Some(dir.join("account")),
        },
    );
    let turns = AtomicU64::new(0);
    let setup = Setup {
        store: herder_store::Store::open(dir.join("herder.db")).expect("the store"),
        adapters,
        accounts,
        sink: Arc::clone(&hub) as Arc<dyn EventSink>,
        turn_ids: Box::new(move || {
            TurnId::new(format!("turn-{}", turns.fetch_add(1, Ordering::SeqCst) + 1))
        }),
        worktrees: Worktrees::new(dir.join("worktrees")),
        attachments: dir.join("attachments"),
    };
    let sessions = SessionManager::open(setup, shutdown.clone())
        .await
        .expect("the sessions");
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("a port");
    let addr = listener.local_addr().expect("the address");
    let fingerprint = tls.fingerprint().to_owned();
    let terminals = Terminals::new(Arc::clone(&hub), PathBuf::from("/bin/sh"));
    let logins = Logins::new(HashMap::new(), dir.join("daemon.toml"), sessions.clone());
    let host = Host {
        id: HostId::new("host-1"),
        name: "box".into(),
    };
    let server = Server::new(
        tls,
        Arc::clone(&auth),
        hub,
        sessions,
        terminals,
        logins,
        host,
    );
    tokio::spawn(server.run(listener, shutdown.clone()));
    let code = auth.mint("alice", None, PAIRING_TTL).expect("a code").code;
    PairingUri {
        hosts: vec![addr.to_string()],
        fingerprint,
        code,
    }
    .to_string()
}

/// Runs the main loop until `done` holds.
fn until(what: &str, done: impl Fn() -> bool) {
    let context = glib::MainContext::default();
    let end = Instant::now() + TIMEOUT;
    while !done() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        while context.iteration(false) {}
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[gtk::test]
fn a_full_turn_from_the_app() {
    adw::init().expect("libadwaita initializes");
    let runtime = tokio::runtime::Runtime::new().expect("a tokio runtime");
    let _runtime = runtime.enter();
    let tmp = tempfile::tempdir().expect("a temp dir");
    let shutdown = CancellationToken::new();
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/turn.jsonl");
    let link = runtime.block_on(daemon(&tmp.path().join("box"), script, &shutdown));
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "herder-gtk-test".to_owned(),
    )
    .expect("the profile opens");
    let host_id = runtime
        .block_on(client.pair(link))
        .expect("pairing")
        .host_id;
    runtime
        .block_on(client.send(
            host_id.clone(),
            CommandBody::CreateSession {
                repo: Some(repo(&tmp.path().join("app"))),
                project_id: None,
                branch: Some("herder/health".to_owned()),
                account_id: Some(AccountId::new("account-1")),
                model: None,
                provider: None,
                permission_mode: Some(PermissionMode::Ask),
                max_children: None,
                failover_pin: None,
            },
        ))
        .expect("the session is created");

    let window = MainWindow::new(None);
    crate::wire(&window, &client);
    let view = window.session_view().clone();
    until("the session's row", || window.has_row("herder/health"));
    window.activate_row("herder/health");
    let key: SessionKey = view.key().expect("the session opens");
    assert_eq!(key.host_id, host_id);
    until("the composer", || {
        view.bottom_child().as_deref() == Some("composer")
    });

    view.submit("Run the tests.");
    until("the approval card", || {
        view.request_texts()
            .iter()
            .any(|text| text == "$ cargo test")
    });
    let texts = view.transcript_texts();
    assert!(texts.iter().any(|t| t == "Run the tests."), "{texts:?}");
    assert!(texts.iter().any(|t| t == "Running them now."), "{texts:?}");
    assert!(
        !texts.iter().any(|t| t == "QUEUED" || t == "SENDING"),
        "{texts:?}"
    );

    view.click("Allow");
    until("the turn's end", || {
        view.transcript_texts()
            .iter()
            .any(|text| text.starts_with("account-1 · "))
    });
    let texts = view.transcript_texts();
    for wanted in ["△ allowed Bash · by you", "All tests pass."] {
        assert!(texts.iter().any(|t| t == wanted), "{wanted} in {texts:?}");
    }
    assert_eq!(view.bottom_child().as_deref(), Some("composer"));

    view.pick_model("fake-large");
    until("the model switch", || {
        view.transcript_texts()
            .iter()
            .any(|text| text == "switched to fake-large")
    });
    assert_eq!(view.controls().1, "fake-large");
    shutdown.cancel();
}
