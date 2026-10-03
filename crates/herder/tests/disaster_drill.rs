//! Disaster drill: host A loses its disk, and its session goes on on host B from the vault.
//!
//! Everything runs on this machine in scratch dirs, as separate processes: a vault (`herder
//! daemon` in vault mode), hosts A and B, a bare git `origin` both hosts clone. A runs a few
//! turns that change code, each checkpointed to `origin`, and is SIGKILLed in the middle of the
//! next one. `herder recover` on B takes the session over; B's worktree must be A's last
//! checkpoint exactly, and the session goes on there. A, started again on its old data dir,
//! keeps its copy read-only.
//!
//! The vault and `herder recover` / `herder pair` are the herder binary. A host is this test
//! binary run again as a child ([`drill_host`]): `herder_daemon::serve`, the function `herder
//! daemon` runs, with a scripted agent ([`Agent`]) in place of a vendor CLI, so no provider is
//! ever called. Run the drill on its own with `cargo test -p herder --test disaster_drill`.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use herder_adapters::{
    Adapter, AdapterCommand, AdapterEvent, AdapterSession, Capabilities, StartFuture, StartRequest,
};
use herder_client_core::PairingUri;
use herder_client_core::auth::{DeviceKey, client_config};
use herder_daemon::session::{AccountConfig, Accounts, Adapters};
use herder_daemon::vault::VaultStore;
use herder_protocol::{
    AccountId, ClientHello, ClientMessage, Command, CommandBody, CommandId, CommandResult, Cursor,
    ErrorCode, ErrorInfo, Event, EventBody, HostId, Item, ItemBody, ItemId, PROTOCOL_VERSION,
    PermissionMode, ProjectId, Provider, ServerMessage, SessionId, SessionStatus,
};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(30);

/// Set in a child of this binary to the config of the host it runs as.
const HOST_ENV: &str = "HERDER_DRILL_HOST";

const PROJECT: &str = "github.com/org/app";

fn fake() -> Provider {
    Provider::Other("fake".into())
}

/// The scripted agent a drill host runs instead of a vendor CLI. Each line of a prompt is an
/// edit to the worktree, `path=content` or `-path` to delete; a last line `hang` leaves the
/// turn open forever, as a CLI working when its host dies. Every start's seed is appended to
/// `seeds` as one JSON line of its message texts.
struct Agent {
    seeds: PathBuf,
}

impl Adapter for Agent {
    fn start(&self, request: StartRequest) -> StartFuture {
        let texts: Vec<_> = request
            .seed
            .iter()
            .filter_map(|item| match &item.body {
                ItemBody::UserMessage { text } | ItemBody::AssistantMessage { text } => {
                    Some(text.clone())
                }
                _ => None,
            })
            .collect();
        let mut seeds = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.seeds)
            .unwrap();
        writeln!(seeds, "{}", serde_json::to_string(&texts).unwrap()).unwrap();
        let cwd = request.cwd;
        Box::pin(async move {
            let (commands, mut command_rx) = mpsc::unbounded_channel();
            let (events, event_rx) = mpsc::channel(64);
            tokio::spawn(async move {
                while let Some(command) = command_rx.recv().await {
                    let AdapterCommand::SendPrompt { turn_id, text } = command else {
                        if command == AdapterCommand::Shutdown {
                            break;
                        }
                        continue;
                    };
                    let mut hang = false;
                    for line in text.lines() {
                        if line == "hang" {
                            hang = true;
                        } else if let Some(path) = line.strip_prefix('-') {
                            std::fs::remove_file(cwd.join(path)).unwrap();
                        } else {
                            let (path, content) = line.split_once('=').unwrap();
                            let path = cwd.join(path);
                            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                            std::fs::write(path, format!("{content}\n")).unwrap();
                        }
                    }
                    let reply = Item {
                        id: ItemId::new(format!("reply-{turn_id}")),
                        turn_id: turn_id.clone(),
                        body: ItemBody::AssistantMessage {
                            text: format!("Edited: {}", text.replace('\n', "; ")),
                        },
                    };
                    let mut out = vec![
                        AdapterEvent::TurnStarted {
                            turn_id: turn_id.clone(),
                        },
                        AdapterEvent::ItemCompleted { item: reply },
                    ];
                    if !hang {
                        out.push(AdapterEvent::TurnCompleted { turn_id });
                    }
                    for event in out {
                        if events.send(event).await.is_err() {
                            return;
                        }
                    }
                }
                let _ = events.send(AdapterEvent::Exited { error: None }).await;
            });
            Ok(AdapterSession {
                capabilities: Capabilities::default(),
                commands,
                events: event_rx,
            })
        })
    }
}

/// A drill host, when run as a child with [`HOST_ENV`] set; a no-op in a normal test run.
/// Its one account, `<host>-account`, is on the scripted agent; it runs until killed.
#[test]
fn drill_host() {
    let Ok(config_path) = std::env::var(HOST_ENV) else {
        return;
    };
    let config_path = PathBuf::from(config_path);
    let config = herder_daemon::Config::load(Some(&config_path)).unwrap();
    herder_daemon::logging::init(&config.log).unwrap();
    let dir = config_path.parent().unwrap();
    let host = dir.file_name().unwrap().to_str().unwrap();
    let mut adapters = Adapters::new();
    adapters.register(
        fake(),
        Arc::new(Agent {
            seeds: dir.join("seeds.jsonl"),
        }),
    );
    let accounts = Accounts::from([(
        AccountId::new(format!("{host}-account")),
        AccountConfig {
            provider: fake(),
            label: host.to_owned(),
            config_dir: Some(dir.join("account")),
            failover: false,
        },
    )]);
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(herder_daemon::serve(
            &config,
            adapters,
            Default::default(),
            accounts,
            CancellationToken::new(),
        ))
        .unwrap();
}

/// A process of the drill, SIGKILLed when dropped. Its stderr goes to `log`, printed when the
/// drill fails.
struct Process {
    child: Option<Child>,
    log: PathBuf,
}

impl Process {
    fn spawn(mut command: std::process::Command, log: PathBuf) -> Self {
        let stderr = std::fs::File::create(&log).unwrap();
        let child = command
            .env_remove("HERDER_CONFIG")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .unwrap();
        Self {
            child: Some(child),
            log,
        }
    }

    /// The vault: `herder daemon` on `config`, whose `mode` is `vault`.
    fn vault(config: &Path) -> Self {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_herder"));
        command.args(["daemon", "--config"]).arg(config);
        Self::spawn(command, config.with_file_name("daemon.log"))
    }

    /// A host on `config` ([`drill_host`]).
    fn host(config: &Path) -> Self {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["drill_host", "--exact", "--nocapture", "--test-threads=1"])
            .env(HOST_ENV, config);
        Self::spawn(command, config.with_file_name("daemon.log"))
    }

    /// Kills the process as a lost disk or power would: no shutdown, nothing flushed.
    fn sigkill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let pid = Pid::from_raw(i32::try_from(child.id()).unwrap());
            kill(pid, Signal::SIGKILL).unwrap();
            child.wait().unwrap();
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.sigkill();
        if std::thread::panicking() {
            let log = std::fs::read_to_string(&self.log).unwrap_or_default();
            eprintln!("--- {} ---\n{log}", self.log.display());
        }
    }
}

/// A free port on localhost, so a host comes back on the same address after a restart.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Admission by the turn limit only, whatever the CI host's cores and load.
const RESOURCES: &str = "[resources]\nmax_turns = 8\nmin_memory_available_mib = 0\nmax_memory_pressure = 100\nmax_load_percent = 10000\n";

/// Writes `<dir>/daemon.toml` for a daemon on `port` with its data dir in `dir`, plus `extra`.
fn write_config(dir: &Path, port: u16, extra: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("daemon.toml");
    std::fs::write(
        &path,
        format!(
            "listen = \"127.0.0.1:{port}\"\ndata_dir = {:?}\n\n{extra}",
            dir.join("data").to_str().unwrap()
        ),
    )
    .unwrap();
    path
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
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Runs the herder binary with `args`; returns whether it succeeded and its stdout and stderr.
async fn herder(args: &[&str]) -> (bool, String) {
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_herder"))
        .args(args)
        .env_remove("HERDER_CONFIG")
        .output()
        .await
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), text)
}

/// Mints a pairing code for `user` on the daemon of `config` with `herder pair`.
async fn pair(config: &Path, user: &str) -> PairingUri {
    let (ok, output) =
        herder(&["pair", "--config", config.to_str().unwrap(), "--user", user]).await;
    assert!(ok, "herder pair: {output}");
    output
        .lines()
        .find(|line| line.starts_with("herder://pair?"))
        .unwrap_or_else(|| panic!("no pairing link in {output}"))
        .parse()
        .unwrap()
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

/// A bare `origin.git` with one commit on `main`, cloned as `<host>/app` for each host.
fn clones(root: &Path, hosts: &[&Path]) {
    let origin = root.join("origin.git");
    let seed = root.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(root, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
    git(&seed, &["init", "-q", "-b", "main"]);
    std::fs::write(seed.join("README"), "app\n").unwrap();
    git(&seed, &["add", "README"]);
    git(&seed, &["commit", "-q", "-m", "init"]);
    git(&seed, &["push", "-q", origin.to_str().unwrap(), "main"]);
    for host in hosts {
        std::fs::create_dir_all(host).unwrap();
        git(host, &["clone", "-q", origin.to_str().unwrap(), "app"]);
    }
}

/// The session's checkpoint refs on `origin`, oldest first.
fn checkpoints(origin: &Path, session_id: &SessionId) -> Vec<String> {
    let refs = git(
        origin,
        &[
            "for-each-ref",
            "--sort=refname",
            "--format=%(refname)",
            &format!("refs/herder/{session_id}/"),
        ],
    );
    refs.lines().map(str::to_owned).collect()
}

/// Every file of `tree` on `origin`, path to content.
fn tree_files(origin: &Path, tree: &str) -> BTreeMap<String, String> {
    git(origin, &["ls-tree", "-r", "--name-only", tree])
        .lines()
        .map(|path| {
            let content = git(origin, &["cat-file", "blob", &format!("{tree}:{path}")]);
            (path.to_owned(), content)
        })
        .collect()
}

/// Every file of a worktree a checkpoint would take, path to content.
fn worktree_files(worktree: &Path) -> BTreeMap<String, String> {
    git(
        worktree,
        &["ls-files", "--cached", "--others", "--exclude-standard"],
    )
    .lines()
    .filter(|path| worktree.join(path).exists())
    .map(|path| {
        let content = std::fs::read_to_string(worktree.join(path)).unwrap();
        (path.to_owned(), content.trim_end().to_owned())
    })
    .collect()
}

/// A paired device's connection to a daemon, holding the events of the session it follows.
struct Client {
    ws: WebSocketStream<TlsStream<TcpStream>>,
    events: Vec<Event>,
}

impl Client {
    /// Connects to `addr` as `device`, pinning `fingerprint`, and says hello, presenting
    /// `pairing_code` on the first connection.
    async fn connect(
        addr: &str,
        fingerprint: &str,
        device: &DeviceKey,
        pairing_code: Option<String>,
    ) -> Self {
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
        let mut client = Self {
            ws,
            events: Vec::new(),
        };
        client
            .send(&ClientMessage::Hello(ClientHello {
                protocol_version: PROTOCOL_VERSION,
                client: "herder-drill".into(),
                resume: Vec::new(),
                pairing_code,
            }))
            .await;
        let hello = client.recv().await;
        assert!(
            matches!(hello, ServerMessage::Hello(_)),
            "expected a hello, got {hello:?}"
        );
        client
    }

    async fn send(&mut self, message: &ClientMessage) {
        let text = serde_json::to_string(message).unwrap();
        self.ws.send(Message::text(text)).await.unwrap();
    }

    async fn recv(&mut self) -> ServerMessage {
        loop {
            let frame = tokio::time::timeout(TIMEOUT, self.ws.next())
                .await
                .expect("no message from the daemon")
                .expect("the daemon closed the connection")
                .unwrap();
            if let Message::Text(text) = frame {
                let message: ServerMessage = serde_json::from_str(&text).unwrap();
                if let ServerMessage::Event(event) = &message {
                    self.events.push(event.clone());
                }
                return message;
            }
        }
    }

    /// Sends `body` as a command and returns its result or why it was rejected. Command ids
    /// are unique across connections: a daemon answers a user's resent id with its first
    /// result.
    async fn command(&mut self, body: CommandBody) -> Result<CommandResult, ErrorInfo> {
        static COMMANDS: AtomicU64 = AtomicU64::new(0);
        let id = CommandId::new(format!("c{}", COMMANDS.fetch_add(1, Ordering::Relaxed)));
        self.send(&ClientMessage::Command(Command {
            id: id.clone(),
            body,
        }))
        .await;
        loop {
            match self.recv().await {
                ServerMessage::CommandAccepted { command_id, result } if command_id == id => {
                    return Ok(result);
                }
                ServerMessage::CommandRejected { command_id, error } if command_id == id => {
                    return Err(error);
                }
                _ => {}
            }
        }
    }

    async fn subscribe(&mut self, session_id: &SessionId) {
        self.send(&ClientMessage::Subscribe(Cursor {
            session_id: session_id.clone(),
            after_seq: 0,
        }))
        .await;
    }

    async fn prompt(&mut self, session_id: &SessionId, text: &str) -> Result<(), ErrorInfo> {
        self.command(CommandBody::SendPrompt {
            session_id: session_id.clone(),
            text: text.into(),
        })
        .await
        .map(drop)
    }

    /// Reads until an event `done` accepts has arrived.
    async fn until(&mut self, done: impl Fn(&EventBody) -> bool) {
        while !self.events.iter().any(|event| done(&event.body)) {
            self.recv().await;
        }
    }

    /// Reads until the session is idle with `turns` turns completed.
    async fn until_idle_after(&mut self, turns: usize) {
        while !(self.status() == Some(SessionStatus::Idle) && self.completed() == turns) {
            self.recv().await;
        }
    }

    fn completed(&self) -> usize {
        self.events
            .iter()
            .filter(|event| matches!(event.body, EventBody::TurnCompleted { .. }))
            .count()
    }

    fn status(&self) -> Option<SessionStatus> {
        self.events.iter().rev().find_map(|event| match event.body {
            EventBody::SessionStatusChanged { status } => Some(status),
            _ => None,
        })
    }
}

/// The turns A completes, each checkpointed, then the one it dies in.
const A_TURNS: [&str; 3] = [
    "src/main.rs=fn main() {}\nnotes.txt=first draft",
    "src/main.rs=fn main() { app::run() }\n-README",
    "src/lib.rs=pub fn run() {}\nnotes.txt=second draft",
];
const A_LAST: &str = "src/lib.rs=pub fn run() { half written\nhang";
const B_TURN: &str = "src/lib.rs=pub fn run() { println!(\"recovered\") }";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dead_hosts_session_goes_on_on_another_host_from_the_vault() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let (a_dir, b_dir) = (root.join("a"), root.join("b"));
    clones(root, &[&a_dir, &b_dir]);
    let origin = root.join("origin.git");

    // The vault, and a pairing code on it for each host.
    let vault_config = write_config(&root.join("vault"), free_port(), "mode = \"vault\"\n");
    let _vault = Process::vault(&vault_config);
    wait_for(&root.join("vault/data/control.sock")).await;
    let host_config = async |dir: &Path, port: u16| {
        let link = pair(&vault_config, dir.file_name().unwrap().to_str().unwrap()).await;
        let app = dir.join("app");
        write_config(
            dir,
            port,
            &format!(
                "{RESOURCES}\n[[project]]\nremotes = [\"https://{PROJECT}\"]\npaths = [{:?}]\n\n[vault]\naddress = {:?}\nfingerprint = {:?}\npairing_code = {:?}\n",
                app.to_str().unwrap(),
                link.hosts[0],
                link.fingerprint,
                link.code,
            ),
        )
    };
    let a_config = host_config(&a_dir, free_port()).await;
    let b_config = host_config(&b_dir, free_port()).await;

    // Host A runs a session whose turns change code; each is checkpointed to origin.
    let mut a = Process::host(&a_config);
    wait_for(&a_dir.join("data/control.sock")).await;
    let a_link = pair(&a_config, "alice").await;
    let device = DeviceKey::generate().unwrap();
    let mut client = Client::connect(
        &a_link.hosts[0],
        &a_link.fingerprint,
        &device,
        Some(a_link.code.clone()),
    )
    .await;
    let created = client
        .command(CommandBody::CreateSession {
            repo: None,
            project_id: Some(ProjectId::new(PROJECT)),
            branch: None,
            account_id: Some(AccountId::new("a-account")),
            model: None,
            permission_mode: PermissionMode::Ask,
            max_children: None,
            failover_pin: None,
        })
        .await
        .unwrap();
    let CommandResult::SessionCreated { session_id } = created else {
        panic!("expected a session, got {created:?}");
    };
    client.subscribe(&session_id).await;
    for (turn, prompt) in A_TURNS.iter().enumerate() {
        client.prompt(&session_id, prompt).await.unwrap();
        client.until_idle_after(turn + 1).await;
        let deadline = Instant::now() + TIMEOUT;
        while checkpoints(&origin, &session_id).len() < turn + 1 {
            assert!(
                Instant::now() < deadline,
                "turn {} was never checkpointed to origin",
                turn + 1
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    let last_checkpoint = checkpoints(&origin, &session_id).pop().unwrap();
    let expected = tree_files(&origin, &last_checkpoint);
    assert_eq!(
        expected,
        BTreeMap::from(
            [
                ("notes.txt", "second draft"),
                ("src/lib.rs", "pub fn run() {}"),
                ("src/main.rs", "fn main() { app::run() }"),
            ]
            .map(|(path, content)| (path.to_owned(), content.to_owned()))
        )
    );

    // The next turn edits a file and is still running when A dies, before checkpointing.
    client.prompt(&session_id, A_LAST).await.unwrap();
    client
        .until(|body| {
            matches!(body, EventBody::ItemAdded { item }
                if matches!(&item.body, ItemBody::AssistantMessage { text } if text.contains("half written")))
        })
        .await;
    let a_events = client.events.len();
    drop(client);
    // What the vault holds is all B will know: wait until it has A's whole journal.
    let a_host = HostId::new(
        std::fs::read_to_string(a_dir.join("data/host-id"))
            .unwrap()
            .trim(),
    );
    let vault_db = root.join("vault/data/db/vault.db");
    let deadline = Instant::now() + TIMEOUT;
    while VaultStore::open(&vault_db)
        .unwrap()
        .records(&a_host, &session_id, 0, usize::MAX)
        .unwrap()
        .len()
        < a_events
    {
        assert!(
            Instant::now() < deadline,
            "the vault never held A's journal"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Until A dies, B may not take its session over.
    let mut b = Process::host(&b_config);
    wait_for(&b_dir.join("data/control.sock")).await;
    let recover = [
        "recover",
        "--config",
        b_config.to_str().unwrap(),
        session_id.as_str(),
    ];
    let (ok, output) = herder(&recover).await;
    assert!(!ok && output.contains("is online"), "{output}");

    // A's disk is gone with it: B must work from the vault and origin alone.
    a.sigkill();
    let a_disk = a_dir.join("data/worktrees");
    let lost = a_dir.join("lost-worktrees");
    std::fs::rename(&a_disk, &lost).unwrap();

    // B recovers it as soon as the vault sees A gone.
    let deadline = Instant::now() + TIMEOUT;
    let output = loop {
        let (ok, output) = herder(&recover).await;
        if ok {
            break output;
        }
        assert!(
            output.contains("is online") && Instant::now() < deadline,
            "herder recover: {output}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(
        output.contains(&format!("files restored from {last_checkpoint}")),
        "{output}"
    );
    assert!(output.contains("on account b-account"), "{output}");
    let b_worktree = output
        .lines()
        .find_map(|line| line.strip_prefix("worktree "))
        .and_then(|line| line.split(" on ").next())
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("no worktree in {output}"));
    assert!(b_worktree.starts_with(&b_dir), "{}", b_worktree.display());

    // B's worktree is exactly A's last checkpoint: the edit of the turn A died in is gone.
    assert_eq!(worktree_files(&b_worktree), expected);

    // The session goes on on B, its agent seeded with the transcript from A.
    let b_link = pair(&b_config, "alice").await;
    let mut client = Client::connect(
        &b_link.hosts[0],
        &b_link.fingerprint,
        &device,
        Some(b_link.code.clone()),
    )
    .await;
    client.subscribe(&session_id).await;
    client
        .until(|body| matches!(body, EventBody::AccountSwitched { .. }))
        .await;
    assert!(client.events.len() > a_events);
    client.prompt(&session_id, B_TURN).await.unwrap();
    client.until_idle_after(A_TURNS.len() + 1).await;
    assert_eq!(
        std::fs::read_to_string(b_worktree.join("src/lib.rs")).unwrap(),
        "pub fn run() { println!(\"recovered\") }\n"
    );
    let seeds = std::fs::read_to_string(b_dir.join("seeds.jsonl")).unwrap();
    let seeds: Vec<Vec<String>> = seeds
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(seeds.len(), 1, "{seeds:?}");
    let prompts: Vec<_> = seeds[0].iter().step_by(2).map(String::as_str).collect();
    assert_eq!(prompts, [A_TURNS[0], A_TURNS[1], A_TURNS[2], A_LAST]);
    // ... and B checkpoints its turns to origin as A did.
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let latest = checkpoints(&origin, &session_id).pop().unwrap();
        if latest != last_checkpoint {
            assert_eq!(worktree_files(&b_worktree), tree_files(&origin, &latest));
            break;
        }
        assert!(Instant::now() < deadline, "B never checkpointed its turn");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop(client);

    // A comes back on its old data dir: its copy turns read-only.
    std::fs::rename(&lost, &a_disk).unwrap();
    let _a = Process::host(&a_config);
    let deadline = Instant::now() + TIMEOUT;
    let mut client = loop {
        if TcpStream::connect(&a_link.hosts[0]).await.is_ok() {
            break Client::connect(&a_link.hosts[0], &a_link.fingerprint, &device, None).await;
        }
        assert!(Instant::now() < deadline, "A never came back");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    client.subscribe(&session_id).await;
    client
        .until(|body| {
            matches!(
                body,
                EventBody::SessionStatusChanged {
                    status: SessionStatus::Moved
                }
            )
        })
        .await;
    let refused = client
        .prompt(&session_id, "src/lib.rs=fn from_a() {}")
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::Conflict, "{refused:?}");
    assert!(
        refused.message.contains("recovered on another host"),
        "{}",
        refused.message
    );
    drop(client);
    b.sigkill();
}
