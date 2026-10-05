//! End to end: a daemon runs a Claude session from a recorded fixture, a device pairs with it
//! through `herder pair`, and drives one turn with an approval over the WebSocket.
//!
//! The daemon is `herder_daemon::serve`, the same function `herder daemon` runs, in this
//! process: that is where the test hands it a Claude adapter whose transport replays
//! `fixtures/claude/approval.jsonl` instead of spawning `claude`. Everything past the transport
//! is production code: the Claude session, the session manager and journal, the hub, TLS, the
//! WebSocket server, pairing through the control socket and the real `herder pair` binary.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use herder_adapters::claude;
use herder_adapters::fixture::Fixture;
use herder_adapters::transport::Transport;
use herder_adapters::{Adapter, StartFuture, StartRequest};
use herder_client_core::PairingUri;
use herder_client_core::auth::{DeviceKey, client_config};
use herder_daemon::session::{AccountConfig, Accounts, Adapters};
use herder_protocol::{
    Account, AccountId, ApprovalDecision, ApprovalId, ClientHello, ClientMessage, Command,
    CommandBody, CommandId, CommandResult, Cursor, Event, EventBody, FailoverSettings, Item,
    ItemBody, ItemId, PROTOCOL_VERSION, PermissionMode, Provider, Role, ServerMessage,
    SessionStatus, UserId,
};
use herder_store::Store;
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(10);

/// The prompt `approval.jsonl` was recorded with; replay checks it byte for byte.
const PROMPT: &str = "Run the shell command `touch herder-ok.txt` with the Bash tool, then reply with the word done.";

/// The Claude adapter on a recording: each start replays the fixture as the `claude` process.
struct ReplayClaude {
    fixture: PathBuf,
    starts: Arc<Mutex<Vec<StartRequest>>>,
}

impl Adapter for ReplayClaude {
    fn start(&self, request: StartRequest) -> StartFuture {
        self.starts.lock().unwrap().push(request.clone());
        let fixture = Fixture::load(&self.fixture).unwrap();
        claude::start(Transport::replay(fixture), request)
    }
}

/// Runs git in `dir` with a fixed identity, panicking on failure.
fn git(dir: &Path, args: &[&str]) {
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
}

/// A repository with one commit on `main`.
fn repo(dir: &Path) -> PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README"), "hello\n").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

/// Waits until `path` exists.
async fn wait_for(path: &Path) {
    let deadline = Instant::now() + TIMEOUT;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Runs `herder pair` for `user` against the daemon of `config` and reads the link it prints.
async fn pair(config: &Path, user: &str) -> PairingUri {
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_herder"))
        .args(["pair", "--config"])
        .arg(config)
        .args(["--user", user])
        .env_remove("HERDER_CONFIG")
        .output()
        .await
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "herder pair: {stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
        .lines()
        .find(|line| line.starts_with("herder://pair?"))
        .unwrap_or_else(|| panic!("no pairing link in {stdout}"))
        .parse()
        .unwrap()
}

/// What the client knows: the durable events it holds, the items streaming right now, and the
/// streamed text of each item once its `item_added` completed it.
#[derive(Debug, Default)]
struct View {
    events: Vec<Event>,
    streaming: HashMap<ItemId, Item>,
    streamed: HashMap<ItemId, Item>,
    snapshots: usize,
}

impl View {
    fn apply(&mut self, message: &ServerMessage) {
        match message {
            ServerMessage::Event(event) => {
                if let EventBody::ItemAdded { item } = &event.body
                    && let Some(live) = self.streaming.remove(&item.id)
                {
                    self.streamed.insert(item.id.clone(), live);
                }
                let expected = self.events.last().map_or(1, |last| last.seq + 1);
                assert_eq!(event.seq, expected, "gap or duplicate");
                self.events.push(event.clone());
            }
            ServerMessage::Snapshot { item, .. } => {
                self.snapshots += 1;
                self.streaming.insert(item.id.clone(), item.clone());
            }
            ServerMessage::Delta { item_id, text, .. } => {
                let item = self
                    .streaming
                    .get_mut(item_id)
                    .expect("a delta for an item without a snapshot");
                match &mut item.body {
                    ItemBody::AssistantMessage { text: so_far }
                    | ItemBody::Reasoning { text: so_far } => so_far.push_str(text),
                    ItemBody::ToolResult { output, .. } => output.push_str(text),
                    other => panic!("a delta for {other:?}"),
                }
            }
            _ => {}
        }
    }

    fn status(&self) -> Option<SessionStatus> {
        self.events
            .iter()
            .rev()
            .find_map(|event| match &event.body {
                EventBody::SessionStatusChanged { status, .. } => Some(*status),
                _ => None,
            })
    }
}

/// A paired device's connection.
struct Client {
    ws: WebSocketStream<TlsStream<TcpStream>>,
    view: View,
    commands: u64,
}

impl Client {
    /// Connects to `addr` as `device`, pinning `fingerprint`.
    async fn connect(addr: &str, fingerprint: &str, device: &DeviceKey) -> std::io::Result<Self> {
        let config = client_config(fingerprint, device).unwrap();
        let addr: SocketAddr = addr.parse().unwrap();
        let tcp = TcpStream::connect(addr).await?;
        let tls = TlsConnector::from(Arc::new(config))
            .connect(ServerName::try_from("localhost").unwrap(), tcp)
            .await?;
        let request = format!("wss://localhost:{}/", addr.port())
            .into_client_request()
            .unwrap();
        let (ws, _) = tokio_tungstenite::client_async(request, tls)
            .await
            .map_err(std::io::Error::other)?;
        Ok(Self {
            ws,
            view: View::default(),
            commands: 0,
        })
    }

    async fn send(&mut self, message: &ClientMessage) {
        let text = serde_json::to_string(message).unwrap();
        self.ws.send(Message::text(text)).await.unwrap();
    }

    /// The next message, skipping resource figures, which arrive whenever they change.
    async fn recv(&mut self) -> ServerMessage {
        loop {
            let frame = tokio::time::timeout(TIMEOUT, self.ws.next())
                .await
                .expect("no message from the daemon")
                .expect("the daemon closed the connection")
                .unwrap();
            if let Message::Text(text) = frame {
                let message = serde_json::from_str(&text).unwrap();
                if background(&message) {
                    continue;
                }
                self.view.apply(&message);
                return message;
            }
        }
    }

    /// Reads until `done` holds for the view.
    async fn until(&mut self, done: impl Fn(&View) -> bool) {
        while !done(&self.view) {
            self.recv().await;
        }
    }

    /// Sends `body` as a command and returns what it produced.
    async fn command(&mut self, body: CommandBody) -> CommandResult {
        self.commands += 1;
        let id = CommandId::new(format!("c{}", self.commands));
        self.send(&ClientMessage::Command(Command {
            id: id.clone(),
            body,
        }))
        .await;
        loop {
            match self.recv().await {
                ServerMessage::CommandAccepted { command_id, result } if command_id == id => {
                    return result;
                }
                ServerMessage::CommandRejected { command_id, error } if command_id == id => {
                    panic!("command {id} rejected: {error:?}");
                }
                _ => {}
            }
        }
    }
}

/// One line per event, `by: what`, so a history reads at a glance; `alice` is shown by name.
fn describe(events: &[Event], alice: &UserId) -> Vec<String> {
    events
        .iter()
        .map(|event| {
            let what = match &event.body {
                EventBody::SessionCreated { .. } => "session_created".to_owned(),
                EventBody::SessionStatusChanged { status, .. } => format!("status {status:?}"),
                EventBody::TurnStarted { .. } => "turn_started".to_owned(),
                EventBody::TurnCompleted { .. } => "turn_completed".to_owned(),
                EventBody::ItemAdded { item } => match &item.body {
                    ItemBody::UserMessage { text, .. } => format!("user {text}"),
                    ItemBody::AssistantMessage { text } => format!("assistant {text}"),
                    ItemBody::ToolCall { name, .. } => format!("tool_call {name}"),
                    ItemBody::ToolResult { output, .. } => format!("tool_result {output}"),
                    other => format!("{other:?}"),
                },
                EventBody::ApprovalRequested { summary, .. } => {
                    format!("approval_requested {summary}")
                }
                EventBody::ApprovalResolved { decision, .. } => {
                    format!("approval_resolved {decision:?}")
                }
                EventBody::ModelSwitched { model } => format!("model_switched {model}"),
                other => format!("{other:?}"),
            };
            match &event.by {
                Some(user) if user == alice => format!("alice: {what}"),
                Some(user) => format!("{user}: {what}"),
                None => format!("-: {what}"),
            }
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paired_client_runs_a_claude_turn_with_an_approval() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(tmp.path());
    let data_dir = tmp.path().join("data");
    let config_path = tmp.path().join("daemon.toml");
    std::fs::write(
        &config_path,
        format!(
            "listen = \"127.0.0.1:0\"\ndata_dir = {:?}\n\n# Admission by the turn limit only, whatever the CI host's cores and load.\n[resources]\nmax_turns = 8\nmin_memory_available_mib = 0\nmax_memory_pressure = 100\nmax_load_percent = 10000\n",
            data_dir.to_str().unwrap()
        ),
    )
    .unwrap();
    let config = herder_daemon::Config::load(Some(&config_path)).unwrap();

    let starts = Arc::new(Mutex::new(Vec::new()));
    let mut adapters = Adapters::new();
    adapters.register(
        Provider::Claude,
        Arc::new(ReplayClaude {
            fixture: Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../herder-adapters/fixtures/claude/approval.jsonl"),
            starts: Arc::clone(&starts),
        }),
    );
    let account = AccountId::new("work");
    let account_dir = tmp.path().join("accounts/work");
    let accounts = Accounts::from([(
        account.clone(),
        AccountConfig {
            provider: Provider::Claude,
            label: "Work".into(),
            config_dir: Some(account_dir.clone()),
        },
    )]);
    let shutdown = CancellationToken::new();
    let daemon = tokio::spawn({
        let shutdown = shutdown.clone();
        async move {
            herder_daemon::serve(&config, adapters, Default::default(), accounts, shutdown).await
        }
    });
    wait_for(&data_dir.join("control.sock")).await;

    // Pair: `herder pair` mints a code over the control socket; the device presents it once,
    // pinning the fingerprint the link carries.
    let link = pair(&config_path, "alice").await;
    let host = &link.hosts[0];
    let device = DeviceKey::generate().unwrap();
    let wrong_pin = "0".repeat(64);
    assert!(
        Client::connect(host, &wrong_pin, &device).await.is_err(),
        "a daemon whose certificate is not the pinned one was trusted"
    );
    let mut client = Client::connect(host, &link.fingerprint, &device)
        .await
        .unwrap();
    client
        .send(&ClientMessage::Hello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client: "herder-e2e".into(),
            resume: Vec::new(),
            pairing_code: Some(link.code.clone()),
        }))
        .await;
    let ServerMessage::Hello(hello) = client.recv().await else {
        panic!("expected a hello");
    };
    // The daemon's first user is its owner; `by` names them by user id.
    assert_eq!(hello.role, Role::Owner);
    let alice = hello.user_id;
    assert!(matches!(
        client.recv().await,
        ServerMessage::Sessions { .. }
    ));
    let ServerMessage::Accounts { accounts, failover } = client.recv().await else {
        panic!("expected the accounts list");
    };
    assert_eq!(
        accounts,
        [Account {
            config_dir: Some(account_dir.to_string_lossy().into_owned()),
            account_id: account.clone(),
            provider: Provider::Claude,
            label: "Work".into(),
            usage: Vec::new(),
        }]
    );
    assert_eq!(failover, FailoverSettings::default());

    // Create a session on the repository and stream it.
    let CommandResult::SessionCreated { session_id } = client
        .command(CommandBody::CreateSession {
            repo: Some(repo.to_str().unwrap().to_owned()),
            project_id: None,
            branch: None,
            account_id: Some(account.clone()),
            provider: None,
            model: None,
            permission_mode: Some(PermissionMode::Ask),
            max_children: None,
            failover_pin: None,
        })
        .await
    else {
        panic!("expected a session");
    };
    client
        .send(&ClientMessage::Subscribe(Cursor {
            session_id: session_id.clone(),
            after_seq: 0,
        }))
        .await;
    client.until(|view| !view.events.is_empty()).await;

    // Prompt; the agent asks to run a command, and the client allows it.
    client
        .command(CommandBody::SendPrompt {
            session_id: session_id.clone(),
            text: PROMPT.into(),
            images: Vec::new(),
        })
        .await;
    client
        .until(|view| view.status() == Some(SessionStatus::NeedsYou))
        .await;
    let approval_id = approval_id(&client.view.events);
    client
        .command(CommandBody::AnswerApproval {
            session_id: session_id.clone(),
            approval_id,
            decision: ApprovalDecision::Allow,
        })
        .await;
    client
        .until(|view| {
            view.status() == Some(SessionStatus::Idle)
                && view
                    .events
                    .iter()
                    .any(|event| matches!(event.body, EventBody::TurnCompleted { .. }))
        })
        .await;

    assert_eq!(
        describe(&client.view.events, &alice),
        [
            "alice: session_created",
            "-: status Running",
            "alice: user Run the shell command `touch herder-ok.txt` with the Bash tool, then \
             reply with the word done.",
            "-: turn_started",
            "-: model_switched claude-haiku-4-5-20251001",
            "-: tool_call Bash",
            "-: approval_requested Bash: touch herder-ok.txt",
            "-: status NeedsYou",
            "alice: approval_resolved Allow",
            "-: status Running",
            "-: tool_result (Bash completed with no output)",
            "-: assistant done",
            "-: turn_completed",
            "-: status Idle",
        ]
    );

    // The reply streamed: a snapshot, then deltas, that the completed item agrees with.
    assert!(client.view.snapshots > 0, "nothing streamed");
    let (reply_id, reply) = assistant_reply(&client.view.events);
    let streamed = client
        .view
        .streamed
        .get(&reply_id)
        .expect("the reply never streamed");
    let ItemBody::AssistantMessage { text } = &streamed.body else {
        panic!("streamed {streamed:?}");
    };
    assert!(reply.starts_with(text.as_str()), "{text:?} vs {reply:?}");

    // The CLI ran in the session's own worktree on the account's config dir.
    let start = starts.lock().unwrap()[0].clone();
    assert_eq!(start.config_dir, Some(account_dir));
    assert!(
        start.cwd.starts_with(&data_dir) && start.cwd.join(".git").exists(),
        "{}",
        start.cwd.display()
    );

    shutdown.cancel();
    tokio::time::timeout(TIMEOUT, daemon)
        .await
        .expect("the daemon did not stop")
        .unwrap()
        .unwrap();

    // The client holds exactly the journal.
    let journal = Store::open(data_dir.join("db/herder.db"))
        .unwrap()
        .read_since(&session_id, 0, 1000)
        .unwrap();
    assert_eq!(journal, client.view.events);
}

fn approval_id(events: &[Event]) -> ApprovalId {
    events
        .iter()
        .find_map(|event| match &event.body {
            EventBody::ApprovalRequested { approval_id, .. } => Some(approval_id.clone()),
            _ => None,
        })
        .expect("no approval requested")
}

fn assistant_reply(events: &[Event]) -> (ItemId, String) {
    events
        .iter()
        .find_map(|event| match &event.body {
            EventBody::ItemAdded {
                item:
                    Item {
                        id,
                        body: ItemBody::AssistantMessage { text },
                        ..
                    },
            } => Some((id.clone(), text.clone())),
            _ => None,
        })
        .expect("no assistant reply")
}

/// Whether `message` is one the daemon sends whenever it has news, whatever the client does:
/// a host's or session's resource figures, or the skill library and a session's skills,
/// which follow every pull.
fn background(message: &ServerMessage) -> bool {
    matches!(
        message,
        ServerMessage::HostResources(_)
            | ServerMessage::SessionResources { .. }
            | ServerMessage::SkillsStatus(_)
            | ServerMessage::SessionSkills { .. }
    )
}
