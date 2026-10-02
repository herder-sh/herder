//! `herder session` as a script drives it: the real binary against a daemon in this process
//! whose sessions run the fake adapter on `fixtures/session.jsonl`, paired through
//! `herder connect`, every answer read as JSON.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use herder_adapters::fake::FakeAdapter;
use herder_client_core::auth::PairingUri;
use herder_daemon::auth::{Auth, PAIRING_TTL};
use herder_daemon::login::Logins;
use herder_daemon::session::{AccountConfig, Accounts, Adapters, SessionManager, Setup};
use herder_daemon::terminal::Terminals;
use herder_daemon::worktree::Worktrees;
use herder_daemon::ws::{Host, Server, Tls};
use herder_daemon::{Hub, session};
use herder_protocol::{AccountId, HostId, Provider, TurnId};
use herder_store::Store;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(30);

/// Starts a daemon on `dir` with one fake account, `work`, and returns a pairing link for it.
async fn daemon(dir: &Path, shutdown: CancellationToken) -> String {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/session.jsonl");
    std::fs::create_dir_all(dir.join("tls")).unwrap();
    let tls = Tls::load_or_create(&dir.join("tls"), "test-host").unwrap();
    let auth = Arc::new(Auth::open(dir).unwrap());
    let hub = Arc::new(Hub::default());
    let fake = Provider::Other("fake".into());
    let mut adapters = Adapters::new();
    adapters.register(fake.clone(), Arc::new(FakeAdapter::new(script)));
    let accounts = Accounts::from([(
        AccountId::new("work"),
        AccountConfig {
            provider: fake,
            label: "Work".into(),
            config_dir: Some(dir.join("account")),
            failover: false,
        },
    )]);
    let turns = AtomicU64::new(0);
    let setup = Setup {
        store: Store::open(dir.join("herder.db")).unwrap(),
        adapters,
        accounts,
        sink: Arc::clone(&hub) as Arc<dyn session::EventSink>,
        turn_ids: Box::new(move || {
            TurnId::new(format!("turn-{}", turns.fetch_add(1, Ordering::SeqCst) + 1))
        }),
        worktrees: Worktrees::new(dir.join("worktrees")),
    };
    let sessions = SessionManager::open(setup, shutdown.clone()).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let link = PairingUri {
        hosts: vec![listener.local_addr().unwrap().to_string()],
        fingerprint: tls.fingerprint().to_owned(),
        code: auth.mint("alice", None, PAIRING_TTL).unwrap().code,
    }
    .to_string();
    let host = Host {
        id: HostId::new("host-1"),
        name: "test-host".into(),
    };
    let terminals = Terminals::new(Arc::clone(&hub), PathBuf::from("/bin/sh"));
    let server = Server::new(tls, auth, hub, sessions, terminals, Logins::default(), host);
    tokio::spawn(server.run(listener, shutdown));
    link
}

/// A git repository with one commit.
fn repo(dir: &Path) -> String {
    std::fs::create_dir(dir).unwrap();
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
            .unwrap();
        assert!(status.success());
    }
    dir.to_str().unwrap().to_owned()
}

/// Runs the herder binary on the profile in `home` with `stdin` piped in.
async fn herder(home: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_herder"))
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(stdin.as_bytes()).await.unwrap();
    drop(input);
    tokio::time::timeout(TIMEOUT, child.wait_with_output())
        .await
        .unwrap_or_else(|_| panic!("herder {args:?} did not finish"))
        .unwrap()
}

/// Runs `herder session <args> --json`, expects exit `code` and parses what it printed.
async fn session(home: &Path, args: &[&str], stdin: &str, code: i32) -> Value {
    let args = [&["session"], args, &["--json"]].concat();
    let output = herder(home, &args, stdin).await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(code),
        "herder {args:?}: {stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(&stdout).unwrap_or_else(|err| panic!("{err}: {stdout:?}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_script_runs_a_session_from_new_to_archive() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let shutdown = CancellationToken::new();
    let link = daemon(&tmp.path().join("daemon"), shutdown.clone()).await;
    let repo = repo(&tmp.path().join("repo"));

    let paired = herder(&home, &["connect", &link], "").await;
    assert!(paired.status.success(), "{paired:?}");

    // New, with the prompt on stdin; the only machine and the only account are the defaults.
    let created = session(&home, &["new", "--repo", &repo], "Say hello.\n", 0).await;
    let id = created["session_id"].as_str().unwrap().to_owned();
    let done = session(&home, &["wait", &id], "", 0).await;
    assert_eq!(done["status"], "idle");
    assert_eq!(done["last_message"], "Hello, world.");
    assert_eq!(done["account_id"], "work");

    // Send; the turn is slow to start, so a short wait times out while it runs.
    let sent = session(&home, &["send", &id], "Run the tests.", 0).await;
    assert_eq!(sent["session_id"], id.as_str());
    let running = session(&home, &["wait", &id, "--timeout", "1"], "", 4).await;
    assert_eq!(running["status"], "running");
    assert_eq!(
        running["last_message"], "Hello, world.",
        "the new turn has not started"
    );

    // It stops on an approval, which a scripted answer allows.
    let asks = session(&home, &["wait", &id], "", 2).await;
    assert_eq!(asks["status"], "needs_you");
    assert_eq!(
        asks["approvals"],
        json!([{"approval_id": "approval-1", "summary": "Run cargo test", "routed_to": "user"}])
    );
    session(&home, &["send", &id, "--approve", "approval-1"], "", 0).await;
    let done = session(&home, &["wait", &id], "", 0).await;
    assert_eq!(done["last_message"], "All tests pass.");
    assert_eq!(done["approvals"], json!([]));

    // Status and list agree, with the session's branch and linked pull requests.
    let status = session(&home, &["status", &id, "--machine", "test-host"], "", 0).await;
    assert_eq!(status["repo"], repo.as_str());
    assert_eq!(status["model"], "fake-model-1");
    assert_eq!(status["prs"], json!([]));
    let list = session(&home, &["list"], "", 0).await;
    assert_eq!(list, json!([status]));

    session(&home, &["archive", &id], "", 0).await;
    let archived = session(&home, &["status", &id], "", 0).await;
    assert_eq!(archived["status"], "archived");

    // Failures exit 1 with a reason; a wait on an archived session is one.
    let unknown = herder(&home, &["session", "status", "nope"], "").await;
    assert_eq!(unknown.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("no session nope"));
    let wait = herder(&home, &["session", "wait", &id], "").await;
    assert_eq!(wait.status.code(), Some(1), "{wait:?}");
    let usage = herder(&home, &["session", "wait"], "").await;
    assert_eq!(usage.status.code(), Some(64));

    shutdown.cancel();
}
