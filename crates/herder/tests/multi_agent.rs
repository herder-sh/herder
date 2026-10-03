//! End to end, multi-agent: a primary session spawns two children through herder's task
//! tools. One child's question goes to the primary, which answers it; the other asks to run
//! a deny-listed command, which goes to the user, who answers it over the WebSocket. The
//! primary then gets both reports, once each.
//!
//! The daemon is `herder_daemon::serve` in this process, on a fake agent registered as the
//! Claude provider, so the command approval policy applies as it does to Claude's `Bash`. The
//! primary's tool calls go through the real `herder mcp` binary, started with the arguments the
//! daemon handed the primary's CLI. Everything else is production code: sessions, routing,
//! the journal, pairing, TLS and the WebSocket server.

use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use herder_adapters::{
    Adapter, AdapterCommand, AdapterEvent, AdapterSession, Capabilities, McpServer, StartFuture,
    StartRequest,
};
use herder_client_core::PairingUri;
use herder_client_core::auth::{DeviceKey, client_config};
use herder_daemon::session::{AccountConfig, Accounts, Adapters};
use herder_protocol::{
    AccountId, Answer, Answerer, ApprovalDecision, ApprovalId, ApprovalOutcome, ClientHello,
    ClientMessage, Command, CommandBody, CommandId, CommandResult, Cursor, EscalationReason, Event,
    EventBody, Item, ItemBody, ItemId, PROTOCOL_VERSION, PermissionMode, Provider, QuestionId,
    Route, ServerMessage, SessionId, SessionStatus, TurnId, UserId,
};
use herder_store::Store;
use rustls::pki_types::ServerName;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::mpsc;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(10);

/// The MCP server the daemon gave each session's CLI, by session.
type Servers = Arc<Mutex<HashMap<SessionId, McpServer>>>;

/// An agent driven by its prompt. It replies `Done: <prompt>`, except that on `Ask.` it asks
/// a question with choices A and B, and on `Run <command>.` it asks to run the command with
/// `Bash`; either blocks the turn until it is answered.
struct Scripted {
    servers: Servers,
}

impl Adapter for Scripted {
    fn start(&self, request: StartRequest) -> StartFuture {
        let mcp = request.mcp.expect("the daemon serves the task tools");
        let session = mcp
            .args
            .iter()
            .skip_while(|arg| *arg != "--session")
            .nth(1)
            .expect("the server names its session");
        self.servers
            .lock()
            .unwrap()
            .insert(SessionId::new(session.as_str()), mcp.clone());
        Box::pin(async move {
            let (commands, received) = mpsc::unbounded_channel();
            let (events, rx) = mpsc::channel(64);
            tokio::spawn(agent(received, events));
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

async fn agent(
    mut received: mpsc::UnboundedReceiver<AdapterCommand>,
    events: mpsc::Sender<AdapterEvent>,
) {
    let reply = |turn_id: TurnId, text: String| {
        let item = Item {
            id: ItemId::new(format!("reply-{turn_id}")),
            turn_id: turn_id.clone(),
            body: ItemBody::AssistantMessage { text },
        };
        [
            AdapterEvent::ItemCompleted { item },
            AdapterEvent::TurnCompleted { turn_id },
        ]
    };
    // The turn blocked on a request.
    let mut blocked = None;
    while let Some(command) = received.recv().await {
        let mut out = Vec::new();
        match command {
            AdapterCommand::SendPrompt { turn_id, text, .. } => {
                out.push(AdapterEvent::TurnStarted {
                    turn_id: turn_id.clone(),
                });
                if text == "Ask." {
                    out.push(AdapterEvent::QuestionAsked {
                        question_id: QuestionId::new("question-1"),
                        turn_id: turn_id.clone(),
                        text: "A or B?".into(),
                        choices: vec!["A".into(), "B".into()],
                    });
                    blocked = Some(turn_id);
                } else if let Some(command) = text.strip_prefix("Run ") {
                    let call = ItemId::new("call-1");
                    let command = command.trim_end_matches('.');
                    out.push(AdapterEvent::ItemCompleted {
                        item: Item {
                            id: call.clone(),
                            turn_id: turn_id.clone(),
                            body: ItemBody::ToolCall {
                                name: "Bash".into(),
                                input: json!({ "command": command }),
                            },
                        },
                    });
                    out.push(AdapterEvent::ApprovalRequested {
                        approval_id: ApprovalId::new("approval-1"),
                        turn_id: turn_id.clone(),
                        tool_call_id: call,
                        summary: format!("Bash: {command}"),
                    });
                    blocked = Some(turn_id);
                } else {
                    out.extend(reply(turn_id, format!("Done: {text}")));
                }
            }
            AdapterCommand::AnswerQuestion { answer, .. } => {
                if let Some(turn_id) = blocked.take() {
                    let answer = serde_json::to_string(&answer).unwrap();
                    out.extend(reply(turn_id, format!("Answered: {answer}")));
                }
            }
            AdapterCommand::AnswerApproval { decision, .. } => {
                if let Some(turn_id) = blocked.take() {
                    out.extend(reply(turn_id, format!("Decided: {decision:?}")));
                }
            }
            AdapterCommand::Shutdown => break,
            _ => {}
        }
        for event in out {
            let _ = events.send(event).await;
        }
    }
    let _ = events.send(AdapterEvent::Exited { error: None }).await;
}

/// The primary's side of the task tools: `herder mcp`, run as its CLI would run it.
struct Tools {
    process: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_id: u64,
}

impl Tools {
    async fn start(server: &McpServer) -> Self {
        let mut process = tokio::process::Command::new(env!("CARGO_BIN_EXE_herder"))
            .args(&server.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut tools = Self {
            input: process.stdin.take().unwrap(),
            output: BufReader::new(process.stdout.take().unwrap()),
            process,
            next_id: 0,
        };
        let init = json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "multi-agent-test", "version": "0" },
        });
        tools.request("initialize", init).await;
        tools
            .write(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await;
        tools
    }

    async fn write(&mut self, message: Value) {
        let mut line = message.to_string();
        line.push('\n');
        self.input.write_all(line.as_bytes()).await.unwrap();
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.write(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await;
        let mut line = String::new();
        tokio::time::timeout(TIMEOUT, self.output.read_line(&mut line))
            .await
            .expect("no answer from herder mcp")
            .unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], id, "{response}");
        response["result"].clone()
    }

    /// A successful tool call's `structuredContent`.
    async fn call(&mut self, name: &str, arguments: Value) -> Value {
        let params = json!({ "name": name, "arguments": arguments });
        let result = self.request("tools/call", params).await;
        assert_ne!(result["isError"], true, "{name}: {result}");
        result["structuredContent"].clone()
    }

    async fn close(mut self) {
        drop(self.input);
        let status = tokio::time::timeout(TIMEOUT, self.process.wait())
            .await
            .expect("herder mcp did not exit")
            .unwrap();
        assert!(status.success(), "herder mcp: {status}");
    }
}

/// A paired device's connection, holding the events of every session it subscribed to.
struct Client {
    ws: WebSocketStream<TlsStream<TcpStream>>,
    events: HashMap<SessionId, Vec<Event>>,
    commands: u64,
}

impl Client {
    async fn connect(addr: &str, fingerprint: &str, device: &DeviceKey) -> Self {
        let config = client_config(fingerprint, device).unwrap();
        let addr: SocketAddr = addr.parse().unwrap();
        let tcp = TcpStream::connect(addr).await.unwrap();
        let tls = TlsConnector::from(Arc::new(config))
            .connect(ServerName::try_from("localhost").unwrap(), tcp)
            .await
            .unwrap();
        let request = format!("wss://localhost:{}/", addr.port())
            .into_client_request()
            .unwrap();
        let (ws, _) = tokio_tungstenite::client_async(request, tls).await.unwrap();
        Self {
            ws,
            events: HashMap::new(),
            commands: 0,
        }
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
                let message: ServerMessage = serde_json::from_str(&text).unwrap();
                if resources(&message) {
                    continue;
                }
                if let ServerMessage::Event(event) = &message {
                    self.events
                        .entry(event.session_id.clone())
                        .or_default()
                        .push(event.clone());
                }
                return message;
            }
        }
    }

    /// Reads until `session_id`'s events hold one `matching`.
    async fn until(&mut self, session_id: &SessionId, matching: fn(&EventBody) -> bool) {
        while !self
            .events
            .get(session_id)
            .is_some_and(|events| events.iter().any(|event| matching(&event.body)))
        {
            self.recv().await;
        }
    }

    async fn subscribe(&mut self, session_id: &SessionId) {
        let cursor = Cursor {
            session_id: session_id.clone(),
            after_seq: 0,
        };
        self.send(&ClientMessage::Subscribe(cursor)).await;
    }

    async fn command(&mut self, body: CommandBody) -> CommandResult {
        self.commands += 1;
        let id = CommandId::new(format!("c{}", self.commands));
        let command = Command {
            id: id.clone(),
            body,
        };
        self.send(&ClientMessage::Command(command)).await;
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
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
    repo
}

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
    assert!(output.status.success(), "herder pair: {stdout}");
    stdout
        .lines()
        .find(|line| line.starts_with("herder://pair?"))
        .unwrap_or_else(|| panic!("no pairing link in {stdout}"))
        .parse()
        .unwrap()
}

/// The single event of `journal` that `matching` picks out.
fn only<T>(journal: &[Event], matching: impl Fn(&Event) -> Option<T>) -> T {
    let mut found: Vec<T> = journal.iter().filter_map(matching).collect();
    assert_eq!(found.len(), 1, "{journal:#?}");
    found.remove(0)
}

fn ever_needed_you(journal: &[Event]) -> bool {
    journal.iter().any(|event| {
        matches!(
            event.body,
            EventBody::SessionStatusChanged {
                retry_at: None,
                status: SessionStatus::NeedsYou
            }
        )
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_primary_answers_one_child_and_the_user_answers_the_other() {
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
    let servers = Servers::default();
    let mut adapters = Adapters::new();
    adapters.register(
        Provider::Claude,
        Arc::new(Scripted {
            servers: Arc::clone(&servers),
        }),
    );
    let account = AccountId::new("work");
    let accounts = Accounts::from([(
        account.clone(),
        AccountConfig {
            provider: Provider::Claude,
            label: "Work".into(),
            config_dir: None,
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

    // Alice pairs and owns the daemon.
    let link = pair(&config_path, "alice").await;
    let device = DeviceKey::generate().unwrap();
    let mut client = Client::connect(&link.hosts[0], &link.fingerprint, &device).await;
    client
        .send(&ClientMessage::Hello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client: "herder-multi-agent".into(),
            resume: Vec::new(),
            pairing_code: Some(link.code.clone()),
        }))
        .await;
    let ServerMessage::Hello(hello) = client.recv().await else {
        panic!("expected a hello");
    };
    let alice: UserId = hello.user_id;

    // She starts the primary; its first turn starts its CLI, which gets the task tools.
    let CommandResult::SessionCreated {
        session_id: primary,
    } = client
        .command(CommandBody::CreateSession {
            repo: Some(repo.to_str().unwrap().to_owned()),
            project_id: None,
            branch: None,
            account_id: Some(account),
            provider: None,
            model: None,
            permission_mode: Some(PermissionMode::AutoEdit),
            max_children: None,
            failover_pin: None,
        })
        .await
    else {
        panic!("expected a session");
    };
    client.subscribe(&primary).await;
    client
        .command(CommandBody::SendPrompt {
            session_id: primary.clone(),
            text: "Plan.".into(),
            images: Vec::new(),
        })
        .await;
    client
        .until(&primary, |body| {
            matches!(body, EventBody::TurnCompleted { .. })
        })
        .await;
    // The server's command is this test binary, which is what `serve` runs in; its arguments
    // are what `herder mcp` takes.
    let server = servers.lock().unwrap()[&primary].clone();
    let mut tools = Tools::start(&server).await;

    // The primary spawns a child that asks a question and one that wants to run `sudo`.
    let asks = tools
        .call("spawn", json!({ "task": "Ask", "prompt": "Ask." }))
        .await;
    let asks = SessionId::new(asks["child"].as_str().unwrap());
    let sudo = tools
        .call("spawn", json!({ "task": "Sudo", "prompt": "Run sudo ls." }))
        .await;
    let sudo = SessionId::new(sudo["child"].as_str().unwrap());

    // The question reaches the primary, and the primary answers it. The command is never the
    // primary's: the first thing it waits for is the question.
    let event = tools.call("wait_for", json!({ "timeout_secs": 10 })).await;
    let question_id = "question-1";
    assert_eq!(
        event,
        json!({
            "kind": "request",
            "child": asks.as_str(),
            "request": {
                "kind": "question",
                "question_id": question_id,
                "text": "A or B?",
                "choices": ["A", "B"],
            },
        })
    );
    let choose = json!({ "child": asks.as_str(), "question_id": question_id, "choice": 1 });
    assert_eq!(tools.call("answer", choose).await, json!({}));

    // The command waits on Alice, who sees it on her client and allows it.
    client.subscribe(&sudo).await;
    client
        .until(&sudo, |body| {
            matches!(
                body,
                EventBody::SessionStatusChanged {
                    retry_at: None,
                    status: SessionStatus::NeedsYou
                }
            )
        })
        .await;
    let CommandResult::Applied = client
        .command(CommandBody::AnswerApproval {
            session_id: sudo.clone(),
            approval_id: ApprovalId::new("approval-1"),
            decision: ApprovalDecision::Allow,
        })
        .await
    else {
        panic!("expected the answer applied");
    };

    // Both children report, each once; then no child works.
    let mut reports = BTreeSet::new();
    for _ in 0..2 {
        let event = tools.call("wait_for", json!({ "timeout_secs": 10 })).await;
        assert_eq!(event["kind"], "report", "{event}");
        assert_eq!(event["status"], "idle", "{event}");
        reports.insert((
            SessionId::new(event["child"].as_str().unwrap()),
            event["summary"].as_str().unwrap().to_owned(),
        ));
    }
    assert_eq!(
        reports,
        BTreeSet::from([
            (
                asks.clone(),
                r#"Answered: {"type":"choice","index":1}"#.to_owned()
            ),
            (sudo.clone(), "Decided: Allow".to_owned()),
        ])
    );
    let event = tools.call("wait_for", json!({ "timeout_secs": 10 })).await;
    assert_eq!(event, json!({ "kind": "idle" }));
    tools.close().await;

    shutdown.cancel();
    tokio::time::timeout(TIMEOUT, daemon)
        .await
        .expect("the daemon did not stop")
        .unwrap()
        .unwrap();

    // The journals record who each request was put to and who answered it.
    let store = Store::open(data_dir.join("db/herder.db")).unwrap();
    let journal = |session_id: &SessionId| store.read_since(session_id, 0, 1000).unwrap();

    let primary_journal = journal(&primary);
    let spawned = primary_journal
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::ChildSpawned {
                child_session_id, ..
            } => Some(child_session_id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(spawned, [asks.clone(), sudo.clone()]);
    let reported = primary_journal
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::ChildReported {
                child_session_id, ..
            } => Some(child_session_id.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(reported, BTreeSet::from([asks.clone(), sudo.clone()]));

    let asks_journal = journal(&asks);
    let asked = only(&asks_journal, |event| match &event.body {
        EventBody::QuestionAsked {
            routed_to, reason, ..
        } => Some((*routed_to, *reason)),
        _ => None,
    });
    assert_eq!(asked, (Route::Primary, None));
    let answered = only(&asks_journal, |event| match &event.body {
        EventBody::QuestionAnswered {
            answer,
            answered_by,
            ..
        } => Some((event.by.clone(), answer.clone(), answered_by.clone())),
        _ => None,
    });
    assert_eq!(
        answered,
        (
            None,
            Answer::Choice { index: 1 },
            Answerer::Primary {
                session_id: primary.clone()
            }
        )
    );
    assert!(!ever_needed_you(&asks_journal), "{asks_journal:#?}");

    let sudo_journal = journal(&sudo);
    let requested = only(&sudo_journal, |event| match &event.body {
        EventBody::ApprovalRequested {
            summary,
            routed_to,
            reason,
            ..
        } => Some((summary.clone(), *routed_to, *reason)),
        _ => None,
    });
    assert_eq!(
        requested,
        (
            "Bash: sudo ls".to_owned(),
            Route::User,
            Some(EscalationReason::ExceedsAuthority)
        )
    );
    let resolved = only(&sudo_journal, |event| match &event.body {
        EventBody::ApprovalResolved {
            decision,
            answered_by,
            ..
        } => Some((event.by.clone(), *decision, answered_by.clone())),
        _ => None,
    });
    assert_eq!(
        resolved,
        (Some(alice), ApprovalOutcome::Allow, Answerer::User)
    );
    assert!(ever_needed_you(&sudo_journal));
    // What Alice's client saw of the child is the journal as recorded.
    assert!(sudo_journal.starts_with(&client.events[&sudo]));
}

/// Whether `message` is a host's or session's resource figures.
fn resources(message: &ServerMessage) -> bool {
    matches!(
        message,
        ServerMessage::HostResources(_) | ServerMessage::SessionResources { .. }
    )
}
