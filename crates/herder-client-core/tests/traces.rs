//! One trace from the app through the client core and the network to the daemon and the turn
//! a prompt starts: a real daemon with the fake adapter, over TLS on localhost, in this
//! process, and Sentry's test transport in place of Sentry. Nothing here reaches the real
//! Sentry: the DSN names no host that answers, and the transport sends nothing anywhere.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use herder_adapters::fake::FakeAdapter;
use herder_client_core::Client;
use herder_daemon::auth::Auth;
use herder_daemon::auth::authenticator;
use herder_daemon::login::Logins;
use herder_daemon::session::{AccountConfig, Accounts, Adapters, SessionManager, Setup};
use herder_daemon::terminal::Terminals;
use herder_daemon::worktree::Worktrees;
use herder_daemon::ws::{Host, Server, Tls};
use herder_daemon::{Fanout, session};
use herder_protocol::{
    AccountId, CommandBody, CommandResult, EventBody, HostId, PermissionMode, Provider, TurnId,
};
use herder_store::Store;
use herder_telemetry::{Component, Endpoints, Telemetry};
use sentry::protocol::{Context, EnvelopeItem, Transaction};
use sentry::test::TestTransport;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const TIMEOUT: Duration = Duration::from_secs(20);

/// The app's span the prompt is sent in.
const APP_TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const APP_SPAN: &str = "00f067aa0ba902b7";

fn account() -> AccountId {
    AccountId::new("account-1")
}

/// Starts a daemon on `dir` whose sessions run `traced_turn.jsonl`; answers its address, its
/// fingerprint and its auth.
async fn daemon(dir: &Path) -> (String, String, Arc<Auth>, CancellationToken) {
    std::fs::create_dir_all(dir.join("tls")).unwrap();
    let tls = Tls::load_or_create(&dir.join("tls"), "test-host").unwrap();
    let auth = Arc::new(Auth::open(dir).unwrap());
    let fanout = Arc::new(Fanout::default());
    let fake = Provider::Other("fake".into());
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/traced_turn.jsonl");
    let mut adapters = Adapters::new();
    adapters.register(fake.clone(), Arc::new(FakeAdapter::new(script)));
    let mut accounts = Accounts::new();
    accounts.insert(
        account(),
        AccountConfig {
            provider: fake,
            label: "Account 1".into(),
            config_dir: Some(dir.join("account")),
            fallback: false,
            endpoint: None,
        },
    );
    let setup = Setup {
        store: Store::open(dir.join("herder.db")).unwrap(),
        adapters,
        accounts,
        sink: Arc::clone(&fanout) as Arc<dyn session::EventSink>,
        turn_ids: Box::new(|| TurnId::new("turn-1")),
        worktrees: Worktrees::new(dir.join("worktrees")),
        attachments: dir.join("attachments"),
        chats: dir.join("chats"),
    };
    let shutdown = CancellationToken::new();
    let sessions = SessionManager::open(setup, shutdown.clone()).await.unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let fingerprint = tls.fingerprint().to_owned();
    let host = Host {
        id: HostId::new("host-1"),
        name: "test-host-1".into(),
    };
    let terminals = Terminals::new(Arc::clone(&fanout), PathBuf::from("/bin/sh"));
    let logins = Logins::new(HashMap::new(), dir.join("daemon.toml"), sessions.clone());
    let server = Server::new(
        tls,
        Arc::clone(&auth),
        fanout,
        sessions,
        terminals,
        logins,
        host,
    );
    tokio::spawn(server.run(vec![listener], shutdown.clone()));
    (addr, fingerprint, auth, shutdown)
}

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

/// A transaction as the test transport got it: its name, command, trace, span and parent.
#[derive(Debug, Clone)]
struct Seen {
    name: String,
    command: Option<String>,
    trace_id: String,
    span_id: String,
    parent: Option<String>,
}

fn seen(transport: &TestTransport, all: &mut Vec<Seen>) {
    for envelope in transport.fetch_and_clear_envelopes() {
        for item in envelope.items() {
            if let EnvelopeItem::Transaction(transaction) = item {
                all.push(describe(transaction));
            }
        }
    }
}

fn describe(transaction: &Transaction<'_>) -> Seen {
    let Some(Context::Trace(trace)) = transaction.contexts.get("trace") else {
        panic!("a transaction without its trace: {transaction:?}");
    };
    Seen {
        name: transaction.name.clone().unwrap_or_default(),
        command: trace
            .data
            .get("command")
            .and_then(|command| command.as_str())
            .map(str::to_owned),
        trace_id: trace.trace_id.to_string(),
        span_id: trace.span_id.to_string(),
        parent: trace.parent_span_id.map(|id| id.to_string()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sent_prompt_is_traced_from_the_app_through_the_daemon_to_its_turn() {
    tracing_subscriber::registry()
        .with(herder_telemetry::layer())
        .init();
    let tmp = tempfile::tempdir().unwrap();
    let (addr, fingerprint, auth, shutdown) = daemon(&tmp.path().join("daemon")).await;
    let repo = repo(&tmp.path().join("repo"));
    let client = Client::open(
        tmp.path().join("client").to_str().unwrap().to_owned(),
        "herder-test/0".into(),
    )
    .unwrap();
    let machine = authenticator::sign_in(&auth, &client, vec![addr], &fingerprint, "alice")
        .await
        .unwrap();
    let host = machine.host_id;

    // Off: a command carries no trace, and nothing is sent.
    let transport = TestTransport::new();
    let created = client
        .send(
            host.clone(),
            CommandBody::CreateSession {
                repo: Some(repo),
                project_id: None,
                branch: None,
                account_id: Some(account()),
                provider: None,
                model: None,
                permission_mode: Some(PermissionMode::Ask),
                failover_pin: None,
                chat: false,
            },
        )
        .await
        .unwrap();
    let CommandResult::SessionCreated { session_id } = created else {
        panic!("expected a session, got {created:?}");
    };
    assert!(transport.fetch_and_clear_envelopes().is_empty());

    // On, as the app's switch turns it: the app sends the prompt in a span of its own.
    herder_telemetry::set_with(
        Some(Telemetry {
            install_id: "0b7f1c2e-4d4b-4e0e-9a51-3f6f1b2a9c10".into(),
            sentry_dsn: Some("https://public@sentry.invalid/1".into()),
            posthog_token: None,
            component: Component::App,
        }),
        Endpoints {
            sentry: Some(Arc::new(Arc::clone(&transport))),
            base_rate: 1.0,
            ..Endpoints::default()
        },
    );
    let subscription = client
        .subscribe_session(host.clone(), session_id.clone())
        .unwrap();
    client
        .send_traced(
            host.clone(),
            CommandBody::SendPrompt {
                session_id,
                text: "Hello.".into(),
                images: Vec::new(),
                files: Vec::new(),
            },
            format!("00-{APP_TRACE}-{APP_SPAN}-01"),
        )
        .await
        .unwrap();
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let update = subscription.next().await.expect("the session's updates");
            if update
                .events
                .iter()
                .any(|event| matches!(event.body, EventBody::TurnCompleted { .. }))
            {
                break;
            }
        }
    })
    .await
    .expect("the turn completes");

    // The client's span, the daemon's and the turn's start, each in the app's trace, each
    // under the one before.
    let mut all = Vec::new();
    let found = tokio::time::timeout(TIMEOUT, async {
        loop {
            seen(&transport, &mut all);
            let sent = |name: &str, command: &str| {
                all.iter()
                    .find(|seen| {
                        seen.name == name
                            && seen.command.as_deref() == Some(command)
                            && seen.trace_id == APP_TRACE
                    })
                    .cloned()
            };
            let client = all
                .iter()
                .find(|seen| seen.parent.as_deref() == Some(APP_SPAN))
                .cloned();
            let daemon = client.as_ref().and_then(|client| {
                all.iter()
                    .find(|seen| seen.parent.as_deref() == Some(client.span_id.as_str()))
                    .cloned()
            });
            let turn = all
                .iter()
                .find(|seen| seen.name == "turn.start" && seen.trace_id == APP_TRACE)
                .cloned();
            if let (Some(_), Some(client), Some(daemon), Some(turn)) =
                (sent("command", "send_prompt"), client, daemon, turn)
            {
                return (client, daemon, turn);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    let Ok((client_span, daemon_span, turn)) = found else {
        panic!("the trace is incomplete: {all:#?}");
    };
    assert_eq!(client_span.command.as_deref(), Some("send_prompt"));
    assert_eq!(daemon_span.command.as_deref(), Some("send_prompt"));
    assert_eq!(daemon_span.trace_id, APP_TRACE);
    assert_eq!(turn.parent.as_deref(), Some(daemon_span.span_id.as_str()));
    // Off again: nothing more is sent.
    herder_telemetry::set(None);
    client.send(host, CommandBody::ListPeople).await.ok();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(transport.fetch_and_clear_envelopes().is_empty());
    shutdown.cancel();
}
