//! `herder secret run` as a session's agent runs it: the real binary, started by a fake CLI
//! with exactly the environment herder gives the session, against a host daemon in this
//! process that replicates to a vault in this process over TLS on localhost. The agent learns
//! the names and the command from the `secrets` tool; the values reach only the program.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use herder_adapters::{
    Adapter, AdapterCommand, AdapterEvent, AdapterSession, Capabilities, StartFuture, StartRequest,
};
use herder_client_core::{Client, PairingUri};
use herder_daemon::Hub;
use herder_daemon::auth::{Auth, PAIRING_TTL};
use herder_daemon::config::{Retention, VaultConfig};
use herder_daemon::session::{AccountConfig, Accounts, Adapters, EventSink, SessionManager, Setup};
use herder_daemon::vault::{
    HostSecrets, LIVENESS_TIMEOUT, Replicator, SecretsKey, Server, VaultStore, WakeOnEvent,
};
use herder_daemon::worktree::Worktrees;
use herder_daemon::ws::{Host, Tls};
use herder_daemon::{mcp, session};
use herder_protocol::{
    AccountId, CommandBody, CommandResult, EventBody, HostId, Item, ItemBody, ItemId,
    PermissionMode, Provider, SecretValue, SessionId, UserId,
};
use herder_store::Store;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(30);

/// A CLI whose agent runs every prompt with `sh -c`, in the session's environment and folder,
/// and answers with the exit status and output.
struct Shell;

impl Adapter for Shell {
    fn start(&self, request: StartRequest) -> StartFuture {
        Box::pin(async move {
            let (commands, mut received) = mpsc::unbounded_channel();
            let (events, rx) = mpsc::channel(16);
            tokio::spawn(async move {
                while let Some(command) = received.recv().await {
                    let AdapterCommand::SendPrompt { turn_id, text, .. } = command else {
                        if matches!(command, AdapterCommand::Shutdown) {
                            break;
                        }
                        continue;
                    };
                    let started = AdapterEvent::TurnStarted {
                        turn_id: turn_id.clone(),
                    };
                    let _ = events.send(started).await;
                    let output = tokio::process::Command::new("sh")
                        .arg("-c")
                        .arg(&text)
                        .env_clear()
                        .envs(&request.env)
                        .current_dir(&request.cwd)
                        .output()
                        .await
                        .unwrap();
                    let text = format!(
                        "exit {}\n{}{}",
                        output.status.code().unwrap_or(-1),
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    );
                    let item = Item {
                        agent_message: None,
                        follow_up: None,
                        parent_call_id: None,
                        id: ItemId::new(format!("reply-{turn_id}")),
                        turn_id: turn_id.clone(),
                        body: ItemBody::AssistantMessage { text },
                    };
                    let _ = events.send(AdapterEvent::ItemCompleted { item }).await;
                    let completed = AdapterEvent::TurnCompleted {
                        turn_id,
                        usage: None,
                    };
                    let _ = events.send(completed).await;
                }
                let _ = events.send(AdapterEvent::Exited { error: None }).await;
            });
            Ok(AdapterSession {
                capabilities: Capabilities {
                    native_model_switch: true,
                    native_permission_mode_switch: true,
                    reports_usage: false,
                    native_resume: false,
                },
                commands,
                events: rx,
            })
        })
    }
}

fn alice() -> UserId {
    UserId::new("alice")
}

struct Vault {
    addr: std::net::SocketAddr,
    fingerprint: String,
    auth: Arc<Auth>,
}

async fn vault(dir: &Path, shutdown: CancellationToken) -> Vault {
    std::fs::create_dir_all(dir.join("tls")).unwrap();
    let tls = Tls::load_or_create(&dir.join("tls"), "vault").unwrap();
    let auth = Arc::new(Auth::open(dir).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let vault = Vault {
        addr: listener.local_addr().unwrap(),
        fingerprint: tls.fingerprint().to_owned(),
        auth: Arc::clone(&auth),
    };
    let server = Server::new(
        tls,
        auth,
        VaultStore::open(dir.join("vault.db")).unwrap(),
        Host {
            id: HostId::new("vault"),
            name: "vault".into(),
        },
        LIVENESS_TIMEOUT,
        Retention::default(),
        Some(SecretsKey::new(&[7; 32])),
    );
    tokio::spawn(server.run(vec![listener], shutdown));
    vault
}

/// A host daemon on `dir` running the [`Shell`] CLI, serving MCP with the herder binary under
/// test, and replicating to `vault`.
async fn host(dir: &Path, vault: &Vault, shutdown: CancellationToken) -> SessionManager {
    let fake = Provider::Other("fake".into());
    let mut adapters = Adapters::new();
    adapters.register(fake.clone(), Arc::new(Shell));
    let accounts = Accounts::from([(
        AccountId::new("work"),
        AccountConfig {
            provider: fake,
            label: "Work".into(),
            config_dir: Some(dir.join("account")),
            fallback: false,
        },
    )]);
    let changed = Arc::new(Notify::new());
    let sink = Arc::new(WakeOnEvent {
        next: Arc::new(Hub::default()) as Arc<dyn EventSink>,
        notify: Arc::clone(&changed),
    });
    let setup = Setup {
        store: Store::open(dir.join("herder.db")).unwrap(),
        adapters,
        accounts,
        sink,
        turn_ids: session::ulid_turn_ids(),
        worktrees: Worktrees::new(dir.join("worktrees")),
        attachments: dir.join("attachments"),
        chats: dir.join("chats"),
    };
    let sessions = SessionManager::open(setup, shutdown.clone()).await.unwrap();
    sessions
        .serve_mcp(mcp::Config {
            data_dir: dir.to_owned(),
            herder: env!("CARGO_BIN_EXE_herder").into(),
        })
        .unwrap();
    let secrets = Arc::new(HostSecrets::default());
    secrets.set_linked(true);
    sessions.use_secrets(Arc::clone(&secrets)).unwrap();
    let replicator = Replicator {
        vault: VaultConfig::new(
            vault.addr.to_string(),
            vault.fingerprint.clone(),
            Some(vault.auth.mint_host("devbox", PAIRING_TTL).unwrap().code),
        ),
        device: Replicator::device_key(dir).unwrap(),
        host: Host {
            id: HostId::new("devbox"),
            name: "devbox".into(),
        },
        sessions: sessions.clone(),
        changed,
        secrets,
    };
    tokio::spawn(replicator.run(shutdown));
    sessions
}

/// Prompts `session_id` with a shell command and returns what the agent answered.
async fn run(sessions: &SessionManager, session_id: &SessionId, command: &str) -> String {
    let replies = |events: Vec<herder_protocol::Event>| -> Vec<String> {
        events
            .into_iter()
            .filter_map(|event| match event.body {
                EventBody::ItemAdded { item } => match item.body {
                    ItemBody::AssistantMessage { text } => Some(text),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    };
    let before = replies(sessions.read_since(session_id, 0, 10_000).await.unwrap()).len();
    let prompt = CommandBody::SendPrompt {
        session_id: session_id.clone(),
        text: command.into(),
        images: Vec::new(),
        files: Vec::new(),
    };
    sessions.handle(alice(), prompt).await.unwrap();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let replies = replies(sessions.read_since(session_id, 0, 10_000).await.unwrap());
        if replies.len() > before {
            return replies[before].clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the turn never ended"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Calls the `secrets` tool as the session's agent, through the MCP shim its CLI runs.
async fn secrets_tool(data_dir: &Path, session_id: &SessionId) -> Value {
    let (mut input, shim_input) = tokio::io::duplex(1 << 16);
    let (shim_output, output) = tokio::io::duplex(1 << 16);
    let (dir, session) = (data_dir.to_owned(), session_id.clone());
    let shim =
        tokio::spawn(async move { mcp::shim(&dir, &session, shim_input, shim_output).await });
    let call = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "secrets", "arguments": {} },
    });
    input
        .write_all(format!("{call}\n").as_bytes())
        .await
        .unwrap();
    let mut line = String::new();
    tokio::time::timeout(TIMEOUT, BufReader::new(output).read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    drop(input);
    let _ = shim.await;
    let answer: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(answer["result"]["isError"], false, "{answer}");
    answer["result"]["structuredContent"].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_runs_a_command_with_secrets_it_never_sees() {
    let tmp = tempfile::tempdir().unwrap();
    let (vault_dir, host_dir) = (tmp.path().join("v"), tmp.path().join("h"));
    std::fs::create_dir_all(&host_dir).unwrap();
    let shutdown = CancellationToken::new();
    let vault = vault(&vault_dir, shutdown.clone()).await;
    let sessions = host(&host_dir, &vault, shutdown.clone()).await;

    // The owner keeps three secrets on the vault, two of them named alike, and grants their
    // sets to the host once it replicated there.
    let owner = Client::open(tmp.path().join("c").display().to_string(), "test".into()).unwrap();
    let link = PairingUri {
        hosts: vec![vault.addr.to_string()],
        fingerprint: vault.fingerprint.clone(),
        code: vault.auth.mint("alice", None, PAIRING_TTL).unwrap().code,
    };
    owner.pair(link.to_string()).await.unwrap();
    let created = CommandBody::CreateSession {
        repo: None,
        project_id: None,
        branch: None,
        account_id: Some(AccountId::new("work")),
        provider: None,
        model: None,
        permission_mode: Some(PermissionMode::FullAccess),
        failover_pin: None,
        chat: true,
    };
    let CommandResult::SessionCreated { session_id } =
        sessions.handle(alice(), created).await.unwrap()
    else {
        panic!("expected a session");
    };
    assert_eq!(run(&sessions, &session_id, "true").await, "exit 0\n");
    let send = async |command| owner.send(HostId::new("vault"), command).await.unwrap();
    for set in ["prod", "dev"] {
        send(CommandBody::CreateSecretSet { set: set.into() }).await;
    }
    for (set, name, value) in [
        ("prod", "X", "expected"),
        ("prod", "API_TOKEN", "prod-token"),
        ("dev", "API_TOKEN", "dev-token"),
    ] {
        send(CommandBody::PutSecret {
            set: set.into(),
            name: name.into(),
            description: format!("{name} for {set}"),
            value: SecretValue::new(value).unwrap(),
        })
        .await;
    }
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    for set in ["prod", "dev"] {
        loop {
            let grant = CommandBody::GrantSecretSet {
                set: set.into(),
                host_id: HostId::new("devbox"),
            };
            // The host is known once it has replicated.
            if owner.send(HostId::new("vault"), grant).await.is_ok() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the host never replicated"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    // The agent lists names, never values, and learns the command to run.
    let listed = secrets_tool(&host_dir, &session_id).await;
    let wire = listed.to_string();
    for value in ["expected", "prod-token", "dev-token"] {
        assert!(!wire.contains(value), "{wire}");
    }
    let refs: Vec<&str> = listed["secrets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|secret| secret["secret"].as_str().unwrap())
        .collect();
    // A name only one set has is named alone.
    assert_eq!(refs, ["dev/API_TOKEN", "prod/API_TOKEN", "X"]);
    let herder = listed["run"].as_str().unwrap().to_owned();
    assert!(herder.ends_with(" secret run"), "{herder}");

    let mut shown = listed.clone();
    shown["run"] = "herder secret run".into();
    let mut transcript = format!(
        "secrets tool, as the agent sees it:\n{}\n\n",
        serde_json::to_string_pretty(&shown).unwrap()
    );
    for command in [
        format!(
            "{herder} X -- sh -c 'test \"$X\" = expected && echo \"X matches, ${{#X}} characters\"'"
        ),
        format!("{herder} X -- sh -c 'test \"$X\" = unexpected'"),
        format!(
            "{herder} prod/API_TOKEN X -- sh -c 'echo \"API_TOKEN=${{API_TOKEN:+‹redacted›}} X=${{X:+‹redacted›}}\"'"
        ),
        format!("{herder} API_TOKEN -- true"),
        format!("{herder} NOPE -- true"),
        format!("{herder} prod/API_TOKEN dev/API_TOKEN -- true"),
    ] {
        let answer = run(&sessions, &session_id, &command).await;
        transcript.push_str(&format!(
            "$ {}\n{answer}\n",
            command.replace(&herder, "herder secret run")
        ));
    }
    // Values reach the program, never the agent.
    for value in ["expected\n", "prod-token", "dev-token"] {
        assert!(!transcript.contains(value), "{transcript}");
    }
    let answers: Vec<&str> = transcript.split("\n$ ").skip(1).collect();
    assert!(
        answers[0].contains("exit 0\nX matches, 8 characters"),
        "{}",
        answers[0]
    );
    assert!(answers[1].contains("exit 1\n"), "{}", answers[1]);
    assert!(
        answers[2].contains("exit 0\nAPI_TOKEN=‹redacted› X=‹redacted›"),
        "{}",
        answers[2]
    );
    assert!(
        answers[3].contains(
            "exit 1\nherder: API_TOKEN is in more than one set granted to this host; name one \
             of dev/API_TOKEN, prod/API_TOKEN"
        ),
        "{}",
        answers[3]
    );
    assert!(
        answers[4].contains("exit 1\nherder: no set granted to this host has NOPE"),
        "{}",
        answers[4]
    );
    assert!(
        answers[5].contains("exit 1\nherder: prod/API_TOKEN and dev/API_TOKEN would both be"),
        "{}",
        answers[5]
    );

    // Each value the vault handed out is audited for the session, without the value.
    let audit = send(CommandBody::ListSecretAudit { before: None }).await;
    let CommandResult::SecretAudit { entries } = &audit else {
        panic!("expected the audit: {audit:?}");
    };
    assert!(!entries.is_empty());
    assert!(entries.iter().all(|entry| entry.session_id == session_id));
    assert!(!serde_json::to_string(&audit).unwrap().contains("expected"));

    if let Some(out) = std::env::var_os("HERDER_SECRET_TRANSCRIPT") {
        std::fs::write(out, &transcript).unwrap();
    }
    shutdown.cancel();
}
