//! The server over real TLS sockets on localhost.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use herder_client_core::auth::{DeviceKey, client_config};
use herder_protocol::{
    Account, AccountId, ClientHello, ClientMessage, Command, CommandBody, CommandId, CommandResult,
    Cursor, ErrorCode, ErrorInfo, Event, EventBody, HostId, Item, ItemBody, ItemId,
    PROTOCOL_VERSION, PermissionMode, Provider, Role, Seq, ServerHello, ServerMessage, SessionHead,
    SessionId, Terminal, TerminalPurpose, TurnId,
};
use herder_store::{NewEvent, Store};
use rustls::pki_types::ServerName;
use tokio::net::{TcpSocket, TcpStream};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_util::sync::CancellationToken;

use super::{Backend, Host, Identity, Server, Tls};
use crate::auth::{Auth, PAIRING_TTL};
use crate::hub::{self, DELTA_BACKLOG, Hub};
use crate::login::Logins;
use crate::session::EventSink;
use crate::terminal::Terminals;

const TIMEOUT: Duration = Duration::from_secs(20);

/// Streamed by the throttled-client test: far more than the socket buffers between server and
/// client hold, so a client that stops reading pushes back on the server.
const CHUNK: usize = 8 * 1024;
const CHUNKS: usize = 1200;

/// Serves the journal the test writes to; accepts `send_prompt`, rejects everything else.
/// Every session's worktree is `worktree`.
struct TestBackend {
    store: Arc<Mutex<Store>>,
    commands: Arc<AtomicUsize>,
    worktree: PathBuf,
}

impl Backend for TestBackend {
    fn accounts(&self) -> Vec<Account> {
        vec![Account {
            account_id: AccountId::new("claude-main"),
            provider: Provider::Claude,
            label: "Main".into(),
            usage: Vec::new(),
        }]
    }

    fn refresh_usage(&self) {}

    async fn sessions(&self) -> anyhow::Result<Vec<SessionHead>> {
        let sessions = self.store.lock().unwrap().sessions()?;
        Ok(sessions
            .into_iter()
            .map(|session| SessionHead {
                session_id: session.session_id,
                host_id: None,
                head_seq: session.last_seq,
                status: session.status,
                parent: session.parent,
                task: session.task,
                title: session.title,
                project_id: None,
                account_id: session.account_id,
                children_need_you: 0,
            })
            .collect())
    }

    async fn read_since(
        &self,
        session_id: &SessionId,
        after_seq: Seq,
        limit: usize,
    ) -> anyhow::Result<Vec<Event>> {
        Ok(self
            .store
            .lock()
            .unwrap()
            .read_since(session_id, after_seq, limit)?)
    }

    async fn command(
        &self,
        _: &Identity,
        _: &CommandId,
        command: CommandBody,
    ) -> Result<CommandResult, ErrorInfo> {
        self.commands.fetch_add(1, Ordering::SeqCst);
        match command {
            CommandBody::SendPrompt { .. } => Ok(CommandResult::Applied),
            CommandBody::ListDirectory { path } => Ok(CommandResult::Directory {
                path,
                entries: Vec::new(),
            }),
            CommandBody::GetProjectIcon { .. } => Ok(CommandResult::ProjectIcon {
                icon: "ab".into(),
                media_type: "image/png".into(),
                data: herder_protocol::Bytes(b"png".to_vec()),
            }),
            _ => Err(ErrorInfo {
                code: ErrorCode::Unsupported,
                message: "test backend".into(),
            }),
        }
    }

    async fn worktree(&self, session_id: &SessionId) -> Result<PathBuf, ErrorInfo> {
        if self
            .store
            .lock()
            .unwrap()
            .session(session_id)
            .unwrap()
            .is_none()
        {
            return Err(ErrorInfo {
                code: ErrorCode::NotFound,
                message: "no such session".into(),
            });
        }
        Ok(self.worktree.clone())
    }
}

/// A running server, with the journal writer the session manager would own.
struct Daemon {
    addr: SocketAddr,
    fingerprint: String,
    auth: Arc<Auth>,
    /// A device paired as the owner.
    owner: DeviceKey,
    hub: Arc<Hub>,
    store: Arc<Mutex<Store>>,
    commands: Arc<AtomicUsize>,
    shutdown: CancellationToken,
    _tmp: tempfile::TempDir,
}

impl Daemon {
    async fn start() -> Arc<Self> {
        let tmp = tempfile::tempdir().unwrap();
        let tls = Tls::load_or_create(tmp.path(), "localhost").unwrap();
        let store = Arc::new(Mutex::new(
            Store::open(tmp.path().join("journal.sqlite3")).unwrap(),
        ));
        let commands = Arc::new(AtomicUsize::new(0));
        let backend = TestBackend {
            store: Arc::clone(&store),
            commands: Arc::clone(&commands),
            worktree: tmp.path().to_owned(),
        };
        let hub = Arc::new(Hub::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let shutdown = CancellationToken::new();
        let host = Host {
            id: HostId::new("host"),
            name: "localhost".into(),
        };
        let fingerprint = tls.fingerprint().to_owned();
        let auth = Arc::new(Auth::open(tmp.path()).unwrap());
        let owner = DeviceKey::generate().unwrap();
        let code = auth.mint("owner", None, PAIRING_TTL).unwrap().code;
        auth.authenticate(
            &owner.fingerprint(),
            Some(&code),
            "test",
            crate::auth::DeviceRole::Client,
            &CancellationToken::new(),
        )
        .unwrap();
        let terminals = Terminals::new(Arc::clone(&hub), PathBuf::from("/bin/sh"));
        let server = Server::new(
            tls,
            Arc::clone(&auth),
            Arc::clone(&hub),
            backend,
            terminals,
            Logins::default(),
            host,
        );
        tokio::spawn(server.run(listener, shutdown.clone()));
        Arc::new(Self {
            addr,
            fingerprint,
            auth,
            owner,
            hub,
            store,
            commands,
            shutdown,
            _tmp: tmp,
        })
    }

    /// Appends to the journal and publishes, as the session manager does.
    fn append(&self, session: &SessionId, body: EventBody) -> Event {
        let event = self
            .store
            .lock()
            .unwrap()
            .append(NewEvent {
                session_id: session.clone(),
                at: jiff::Timestamp::now(),
                by: None,
                body,
            })
            .unwrap();
        self.hub.event(&event);
        event
    }

    fn create_session(&self, id: &str) -> SessionId {
        let session = SessionId::new(id);
        self.append(
            &session,
            EventBody::SessionCreated {
                repo: "/repo".into(),
                worktree: "/repo/wt".into(),
                branch: "b".into(),
                provider: Provider::Claude,
                account_id: AccountId::new("a"),
                model: "m".into(),
                permission_mode: PermissionMode::Ask,
                parent: None,
                task: None,
                max_children: None,
                failover_pin: None,
            },
        );
        session
    }

    /// A client on the owner's device.
    async fn client(&self) -> Client {
        Client::connect(self.addr, &self.fingerprint, &self.owner, None, None)
            .await
            .unwrap()
    }

    /// A client on `device`, sending `code` if given.
    async fn client_on(&self, device: &DeviceKey, code: Option<&str>) -> Client {
        Client::connect(self.addr, &self.fingerprint, device, code, None)
            .await
            .unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

fn message(id: &str, text: &str) -> Item {
    Item {
        id: ItemId::new(id),
        turn_id: TurnId::new("t1"),
        body: ItemBody::AssistantMessage { text: text.into() },
    }
}

fn added(id: &str, text: &str) -> EventBody {
    EventBody::ItemAdded {
        item: message(id, text),
    }
}

/// What a client knows: the durable events it holds and the items streaming right now.
#[derive(Debug, Default)]
struct View {
    events: Vec<Event>,
    streaming: HashMap<ItemId, Item>,
    snapshots: usize,
    deltas: usize,
}

impl View {
    fn apply(&mut self, message: ServerMessage) {
        match message {
            ServerMessage::Event(event) => {
                if let EventBody::ItemAdded { item } = &event.body {
                    self.streaming.remove(&item.id);
                }
                if let Some(last) = self.events.last() {
                    assert_eq!(event.seq, last.seq + 1, "gap or duplicate");
                }
                self.events.push(event);
            }
            ServerMessage::Snapshot { item, .. } => {
                self.snapshots += 1;
                self.streaming.insert(item.id.clone(), item);
            }
            ServerMessage::Delta { item_id, text, .. } => {
                self.deltas += 1;
                let item = self
                    .streaming
                    .get_mut(&item_id)
                    .expect("a delta for an item without a snapshot");
                hub::append(&mut item.body, &text);
            }
            _ => {}
        }
    }

    fn last_seq(&self) -> Seq {
        self.events.last().map_or(0, |event| event.seq)
    }

    fn text(&self, id: &str) -> Option<&str> {
        match &self.streaming.get(&ItemId::new(id))?.body {
            ItemBody::AssistantMessage { text } => Some(text),
            _ => None,
        }
    }
}

struct Client {
    ws: WebSocketStream<TlsStream<TcpStream>>,
    view: View,
    /// Sent in the hello.
    pairing_code: Option<String>,
}

impl Client {
    /// Connects over TLS as `device`, pinning `fingerprint` and sending `code` if given;
    /// `recv_buffer` shrinks the socket's receive buffer so a client that stops reading pushes
    /// back on the server sooner. Fails when the TLS handshake does.
    async fn connect(
        addr: SocketAddr,
        fingerprint: &str,
        device: &DeviceKey,
        code: Option<&str>,
        recv_buffer: Option<u32>,
    ) -> std::io::Result<Self> {
        let config = client_config(fingerprint, device).unwrap();
        let socket = TcpSocket::new_v4().unwrap();
        if let Some(size) = recv_buffer {
            socket.set_recv_buffer_size(size).unwrap();
        }
        let tcp = socket.connect(addr).await.unwrap();
        let tls = TlsConnector::from(Arc::new(config))
            .connect(ServerName::try_from("localhost").unwrap(), tcp)
            .await?;
        let ws_config = WebSocketConfig::default()
            .max_message_size(None)
            .max_frame_size(None);
        let request = format!("wss://localhost:{}/", addr.port())
            .into_client_request()
            .unwrap();
        let (ws, _) = tokio_tungstenite::client_async_with_config(request, tls, Some(ws_config))
            .await
            .unwrap();
        Ok(Self {
            ws,
            view: View::default(),
            pairing_code: code.map(str::to_owned),
        })
    }

    async fn send(&mut self, message: &ClientMessage) {
        let text = serde_json::to_string(message).unwrap();
        self.ws.send(Message::text(text)).await.unwrap();
    }

    /// The next message, or `None` once the server closed the connection.
    async fn try_recv(&mut self) -> Option<ServerMessage> {
        loop {
            let frame = tokio::time::timeout(TIMEOUT, self.ws.next())
                .await
                .expect("no message from the server");
            match frame {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return None,
                Some(Ok(Message::Text(text))) => return Some(serde_json::from_str(&text).unwrap()),
                Some(Ok(_)) => {}
            }
        }
    }

    async fn recv(&mut self) -> ServerMessage {
        self.try_recv()
            .await
            .expect("the server closed the connection")
    }

    /// Says hello and reads the server hello and the lists that follow it.
    async fn hello(&mut self, resume: Vec<Cursor>) -> ServerHello {
        self.send(&ClientMessage::Hello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client: "test".into(),
            resume,
            pairing_code: self.pairing_code.clone(),
        }))
        .await;
        let ServerMessage::Hello(hello) = self.recv().await else {
            panic!("expected a hello");
        };
        assert!(matches!(self.recv().await, ServerMessage::Sessions { .. }));
        assert!(matches!(self.recv().await, ServerMessage::Accounts { .. }));
        if hello.role == Role::Owner {
            assert!(matches!(self.recv().await, ServerMessage::Terminals { .. }));
        }
        hello
    }

    async fn command(&mut self, id: &str, body: CommandBody) -> ServerMessage {
        self.send(&ClientMessage::Command(Command {
            id: CommandId::new(id),
            body,
        }))
        .await;
        self.recv().await
    }

    async fn subscribe(&mut self, session: &SessionId, after_seq: Seq) {
        self.send(&ClientMessage::Subscribe(Cursor {
            session_id: session.clone(),
            after_seq,
        }))
        .await;
    }

    /// Applies messages until `done` holds for the view.
    async fn read_until(&mut self, done: impl Fn(&View) -> bool) {
        while !done(&self.view) {
            let message = self.recv().await;
            self.view.apply(message);
        }
    }
}

fn cursor(session: &SessionId, after_seq: Seq) -> Cursor {
    Cursor {
        session_id: session.clone(),
        after_seq,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hello_is_answered_with_the_protocol_version_and_lists() {
    let daemon = Daemon::start().await;
    let session = daemon.create_session("s1");
    let mut client = daemon.client().await;
    client
        .send(&ClientMessage::Hello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client: "test".into(),
            resume: Vec::new(),
            pairing_code: None,
        }))
        .await;
    let ServerMessage::Hello(hello) = client.recv().await else {
        panic!("expected a hello");
    };
    assert_eq!(hello.protocol_version, PROTOCOL_VERSION);
    assert_eq!(hello.host_id, HostId::new("host"));
    let ServerMessage::Sessions { sessions } = client.recv().await else {
        panic!("expected the sessions list");
    };
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        (&sessions[0].session_id, sessions[0].head_seq),
        (&session, 1)
    );
    let ServerMessage::Accounts { accounts, .. } = client.recv().await else {
        panic!("expected the accounts list");
    };
    let ids: Vec<_> = accounts.iter().map(|a| a.account_id.as_str()).collect();
    assert_eq!(ids, ["claude-main"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wrong_protocol_version_is_refused() {
    let daemon = Daemon::start().await;
    let mut client = daemon.client().await;
    client
        .send(&ClientMessage::Hello(ClientHello {
            protocol_version: PROTOCOL_VERSION + 1,
            client: "test".into(),
            resume: Vec::new(),
            pairing_code: None,
        }))
        .await;
    let ServerMessage::Error { error } = client.recv().await else {
        panic!("expected an error");
    };
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(client.try_recv().await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn commands_are_answered_by_the_backend() {
    let daemon = Daemon::start().await;
    let session = daemon.create_session("s1");
    let mut client = daemon.client().await;
    client.hello(Vec::new()).await;
    let command = |id: &str, body| {
        ClientMessage::Command(Command {
            id: CommandId::new(id),
            body,
        })
    };
    let prompt = CommandBody::SendPrompt {
        session_id: session.clone(),
        text: "hi".into(),
        images: Vec::new(),
    };
    client.send(&command("c1", prompt)).await;
    assert_eq!(
        client.recv().await,
        ServerMessage::CommandAccepted {
            command_id: CommandId::new("c1"),
            result: CommandResult::Applied,
        }
    );
    // A resend is answered again without reaching the backend.
    let prompt = CommandBody::SendPrompt {
        session_id: session.clone(),
        text: "hi".into(),
        images: Vec::new(),
    };
    client.send(&command("c1", prompt)).await;
    assert!(matches!(
        client.recv().await,
        ServerMessage::CommandAccepted { .. }
    ));
    assert_eq!(daemon.commands.load(Ordering::SeqCst), 1);
    let interrupt = CommandBody::Interrupt {
        session_id: session.clone(),
    };
    client.send(&command("c2", interrupt)).await;
    assert!(matches!(
        client.recv().await,
        ServerMessage::CommandRejected { command_id, .. } if command_id == CommandId::new("c2")
    ));
    client.subscribe(&SessionId::new("nope"), 0).await;
    assert!(matches!(
        client.recv().await,
        ServerMessage::Error { error } if error.code == ErrorCode::NotFound
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_clients_see_identical_durable_streams() {
    let daemon = Daemon::start().await;
    let session = daemon.create_session("s1");
    for n in 0..300 {
        daemon.append(&session, added(&format!("old{n}"), "x"));
    }
    let mut early = daemon.client().await;
    early.hello(vec![cursor(&session, 0)]).await;

    // Events keep landing while the second client replays the journal.
    let publisher = tokio::spawn({
        let daemon = Arc::clone(&daemon);
        let session = session.clone();
        async move {
            for n in 0..300 {
                let id = format!("new{n}");
                daemon.hub.snapshot(&session, &message(&id, ""));
                EventSink::delta(&*daemon.hub, &session, &ItemId::new(&id), "partial");
                daemon.append(&session, added(&id, "final"));
                if n % 10 == 0 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(5)).await;
    let mut late = daemon.client().await;
    late.hello(Vec::new()).await;
    late.subscribe(&session, 0).await;
    publisher.await.unwrap();

    let head = 1 + 300 + 300;
    early.read_until(|view| view.last_seq() == head).await;
    late.read_until(|view| view.last_seq() == head).await;
    assert_eq!(early.view.events, late.view.events);
    let seqs: Vec<Seq> = early.view.events.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, (1..=head).collect::<Vec<_>>());
    assert!(early.view.streaming.is_empty() && late.view.streaming.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_throttled_client_never_builds_a_backlog_and_converges() {
    let daemon = Daemon::start().await;
    let session = daemon.create_session("s1");
    let mut fast = daemon.client().await;
    fast.hello(vec![cursor(&session, 0)]).await;
    let mut slow = Client::connect(
        daemon.addr,
        &daemon.fingerprint,
        &daemon.owner,
        None,
        Some(4096),
    )
    .await
    .unwrap();
    slow.hello(vec![cursor(&session, 0)]).await;
    fast.read_until(|view| view.last_seq() == 1).await;
    slow.read_until(|view| view.last_seq() == 1).await;

    let turn = daemon.append(
        &session,
        EventBody::TurnStarted {
            turn_id: TurnId::new("t1"),
        },
    );
    daemon.hub.snapshot(&session, &message("long", ""));
    let chunk = "y".repeat(CHUNK);
    let full_text = chunk.repeat(CHUNKS);
    let fast_reader = tokio::spawn({
        let full_text = full_text.clone();
        async move {
            fast.read_until(|view| view.text("long") == Some(&full_text))
                .await;
            fast
        }
    });

    // Far more text than the socket buffers hold, streamed while the slow client reads
    // nothing at all.
    for _ in 0..CHUNKS {
        EventSink::delta(&*daemon.hub, &session, &ItemId::new("long"), &chunk);
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let peak = daemon.hub.peak_backlog();

    // Now the slow client reads, slowly, and catches up with the item still in progress.
    let mut slow_messages = 0;
    while slow.view.text("long") != Some(&full_text) {
        let message = slow.recv().await;
        slow.view.apply(message);
        slow_messages += 1;
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let mut fast = fast_reader.await.unwrap();
    assert!(
        peak <= DELTA_BACKLOG + 2,
        "a client queued {peak} messages, over the delta backlog of {DELTA_BACKLOG}"
    );
    assert!(
        slow.view.snapshots >= 2,
        "the slow client never lost deltas: {} snapshots",
        slow.view.snapshots
    );
    assert!(
        slow_messages < 100,
        "{slow_messages} messages reached the slow client"
    );
    assert_eq!(slow.view.streaming, fast.view.streaming);

    // Both converge on the same durable stream once the item completes.
    daemon.append(&session, added("long", &full_text));
    daemon.hub.snapshot(&session, &message("tail", "the"));
    EventSink::delta(&*daemon.hub, &session, &ItemId::new("tail"), " end");
    fast.read_until(|view| view.text("tail") == Some("the end"))
        .await;
    slow.read_until(|view| view.text("tail") == Some("the end"))
        .await;
    assert_eq!(slow.view.events, fast.view.events);
    assert_eq!(slow.view.last_seq(), turn.seq + 1);
    assert_eq!(slow.view.streaming, fast.view.streaming);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_resumes_from_its_cursor_after_a_disconnect() {
    let daemon = Daemon::start().await;
    let session = daemon.create_session("s1");
    for n in 0..10 {
        daemon.append(&session, added(&format!("i{n}"), "x"));
    }
    let mut client = daemon.client().await;
    client.hello(vec![cursor(&session, 0)]).await;
    client.read_until(|view| view.last_seq() == 6).await;
    let held = std::mem::take(&mut client.view.events);
    drop(client);

    for n in 10..15 {
        daemon.append(&session, added(&format!("i{n}"), "x"));
    }
    let mut client = daemon.client().await;
    client.hello(vec![cursor(&session, 6)]).await;
    client.read_until(|view| view.last_seq() == 16).await;
    let seqs: Vec<Seq> = client.view.events.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, (7..=16).collect::<Vec<_>>());
    assert_eq!(held.last().unwrap().seq, 6);

    // Live events follow the replay with nothing repeated.
    daemon.append(&session, added("i15", "x"));
    client.read_until(|view| view.last_seq() == 17).await;
    assert_eq!(client.view.events.len(), 11);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sync_is_answered_after_the_replay_before_it() {
    let daemon = Daemon::start().await;
    let session = daemon.create_session("s1");
    for n in 0..300 {
        daemon.append(&session, added(&format!("i{n}"), "x"));
    }
    let mut client = daemon.client().await;
    client.hello(Vec::new()).await;
    client.subscribe(&session, 0).await;
    let token = "01J9SYNC".to_owned();
    client
        .send(&ClientMessage::Sync {
            token: token.clone(),
        })
        .await;
    loop {
        match client.recv().await {
            ServerMessage::Synced { token: synced } => {
                assert_eq!(synced, token);
                break;
            }
            message => client.view.apply(message),
        }
    }
    assert_eq!(client.view.last_seq(), 301);
}

/// Says hello and expects the daemon to refuse the device and close the connection.
async fn refused(mut client: Client) -> ErrorInfo {
    client
        .send(&ClientMessage::Hello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client: "test".into(),
            resume: Vec::new(),
            pairing_code: client.pairing_code.clone(),
        }))
        .await;
    let ServerMessage::Error { error } = client.recv().await else {
        panic!("expected the device to be refused");
    };
    assert!(
        client.try_recv().await.is_none(),
        "the connection stayed open"
    );
    error
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_pairs_with_a_code_and_is_known_afterwards() {
    let daemon = Daemon::start().await;
    let pairing = daemon.auth.mint("bob", None, PAIRING_TTL).unwrap();
    assert_eq!(pairing.role, Role::Member);
    let device = DeviceKey::generate().unwrap();
    let paired = daemon
        .client_on(&device, Some(&pairing.code.to_lowercase()))
        .await
        .hello(Vec::new())
        .await;
    assert_eq!(paired.role, Role::Member);

    // Later connections need no code: the device key is enough.
    let again = daemon
        .client_on(&device, None)
        .await
        .hello(Vec::new())
        .await;
    assert_eq!(
        (&again.user_id, &again.device_id, again.role),
        (&paired.user_id, &paired.device_id, Role::Member)
    );
    let owner = daemon.client().await.hello(Vec::new()).await;
    assert_eq!(owner.role, Role::Owner);
    assert_ne!(owner.user_id, paired.user_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unpaired_device_is_refused() {
    let daemon = Daemon::start().await;
    let stranger = DeviceKey::generate().unwrap();
    let error = refused(daemon.client_on(&stranger, None).await).await;
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert!(error.message.contains("not paired"), "{}", error.message);
    let error = refused(daemon.client_on(&stranger, Some("NOPE0-NOPE0")).await).await;
    assert_eq!(error.code, ErrorCode::Forbidden);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_expired_code_is_refused() {
    let daemon = Daemon::start().await;
    let code = daemon.auth.mint("bob", None, Duration::ZERO).unwrap().code;
    let device = DeviceKey::generate().unwrap();
    let error = refused(daemon.client_on(&device, Some(&code)).await).await;
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert!(error.message.contains("expired"), "{}", error.message);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_code_pairs_one_device_only() {
    let daemon = Daemon::start().await;
    let code = daemon.auth.mint("bob", None, PAIRING_TTL).unwrap().code;
    let first = DeviceKey::generate().unwrap();
    daemon
        .client_on(&first, Some(&code))
        .await
        .hello(Vec::new())
        .await;
    let second = DeviceKey::generate().unwrap();
    let error = refused(daemon.client_on(&second, Some(&code)).await).await;
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert_eq!(daemon.auth.devices().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pinned_client_rejects_a_changed_certificate() {
    let daemon = Daemon::start().await;
    let other = tempfile::tempdir().unwrap();
    let replaced = Tls::load_or_create(other.path(), "localhost").unwrap();
    let err = Client::connect(
        daemon.addr,
        replaced.fingerprint(),
        &daemon.owner,
        None,
        None,
    )
    .await
    .err()
    .expect("the handshake succeeded against an unpinned certificate");
    let err = err.to_string();
    assert!(
        err.contains(&format!("fingerprint is {}", daemon.fingerprint)),
        "{err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminals_are_for_owners_only() {
    let daemon = Daemon::start().await;
    let session = daemon.create_session("s1");
    let code = daemon.auth.mint("bob", None, PAIRING_TTL).unwrap().code;
    let device = DeviceKey::generate().unwrap();
    let mut member = daemon.client_on(&device, Some(&code)).await;
    member.hello(Vec::new()).await;
    let open = |id: &str| {
        ClientMessage::Command(Command {
            id: CommandId::new(id),
            body: CommandBody::OpenTerminal {
                session_id: session.clone(),
                cols: 80,
                rows: 24,
            },
        })
    };
    member.send(&open("c1")).await;
    let ServerMessage::CommandRejected { error, .. } = member.recv().await else {
        panic!("expected a rejection");
    };
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert_eq!(daemon.commands.load(Ordering::SeqCst), 0);
    // Members still drive sessions.
    member
        .send(&ClientMessage::Command(Command {
            id: CommandId::new("c2"),
            body: CommandBody::SendPrompt {
                session_id: session.clone(),
                text: "hi".into(),
                images: Vec::new(),
            },
        }))
        .await;
    assert!(matches!(
        member.recv().await,
        ServerMessage::CommandAccepted { .. }
    ));

    // The owner opens one; the member never hears of it.
    let mut owner = daemon.client().await;
    owner.hello(Vec::new()).await;
    owner.send(&open("c3")).await;
    assert!(
        matches!(owner.recv().await, ServerMessage::Terminals { terminals } if terminals.len() == 1)
    );
    assert!(matches!(
        owner.recv().await,
        ServerMessage::CommandAccepted {
            result: CommandResult::TerminalOpened { .. },
            ..
        }
    ));
    let prompt = CommandBody::SendPrompt {
        session_id: session.clone(),
        text: "hi".into(),
        images: Vec::new(),
    };
    assert!(matches!(
        member.command("c4", prompt).await,
        ServerMessage::CommandAccepted { .. }
    ));
    assert_eq!(daemon.commands.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn compose_down_is_for_owners_only() {
    let daemon = Daemon::start().await;
    let session = daemon.create_session("s1");
    let code = daemon.auth.mint("bob", None, PAIRING_TTL).unwrap().code;
    let device = DeviceKey::generate().unwrap();
    let mut member = daemon.client_on(&device, Some(&code)).await;
    member.hello(Vec::new()).await;
    let down = || CommandBody::ComposeDown {
        session_id: session.clone(),
        project: "app".into(),
    };
    let ServerMessage::CommandRejected { error, .. } = member.command("c1", down()).await else {
        panic!("expected a rejection");
    };
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert_eq!(daemon.commands.load(Ordering::SeqCst), 0);

    let mut owner = daemon.client().await;
    owner.hello(Vec::new()).await;
    // The owner's reaches the backend, which this one does not support.
    let ServerMessage::CommandRejected { error, .. } = owner.command("c2", down()).await else {
        panic!("expected the test backend's rejection");
    };
    assert_eq!(error.code, ErrorCode::Unsupported);
    assert_eq!(daemon.commands.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn browsing_folders_is_for_owners_only_and_never_remembered() {
    let daemon = Daemon::start().await;
    let code = daemon.auth.mint("bob", None, PAIRING_TTL).unwrap().code;
    let device = DeviceKey::generate().unwrap();
    let mut member = daemon.client_on(&device, Some(&code)).await;
    member.hello(Vec::new()).await;
    let list = || CommandBody::ListDirectory {
        path: "/srv".into(),
    };
    for body in [
        list(),
        CommandBody::AddProject {
            path: "/srv/app".into(),
        },
        CommandBody::SetProjectSettings {
            project_id: herder_protocol::ProjectId::new("github.com/org/app"),
            default_permission_mode: None,
            default_account: None,
            setup_command: None,
        },
        CommandBody::RemoveProject {
            project_id: herder_protocol::ProjectId::new("github.com/org/app"),
        },
    ] {
        let ServerMessage::CommandRejected { error, .. } = member.command("c1", body).await else {
            panic!("expected a rejection");
        };
        assert_eq!(error.code, ErrorCode::Forbidden);
    }
    assert_eq!(daemon.commands.load(Ordering::SeqCst), 0);

    let mut owner = daemon.client().await;
    owner.hello(Vec::new()).await;
    // A listing changes nothing: a resend under the same id lists again.
    for _ in 0..2 {
        let ServerMessage::CommandAccepted { result, .. } = owner.command("c2", list()).await
        else {
            panic!("expected a listing");
        };
        assert!(matches!(result, CommandResult::Directory { .. }));
    }
    assert_eq!(daemon.commands.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn members_fetch_project_icons_afresh_on_every_resend() {
    let daemon = Daemon::start().await;
    let code = daemon.auth.mint("bob", None, PAIRING_TTL).unwrap().code;
    let device = DeviceKey::generate().unwrap();
    let mut member = daemon.client_on(&device, Some(&code)).await;
    member.hello(Vec::new()).await;
    for _ in 0..2 {
        let body = CommandBody::GetProjectIcon {
            project_id: herder_protocol::ProjectId::new("github.com/org/app"),
        };
        let ServerMessage::CommandAccepted { result, .. } = member.command("c1", body).await else {
            panic!("expected the icon");
        };
        assert!(matches!(result, CommandResult::ProjectIcon { .. }));
    }
    assert_eq!(daemon.commands.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_output_survives_a_disconnect_in_the_scrollback() {
    let daemon = Daemon::start().await;
    let session = daemon.create_session("s1");
    let mut client = daemon.client().await;
    client.hello(Vec::new()).await;
    let open = CommandBody::OpenTerminal {
        session_id: session.clone(),
        cols: 80,
        rows: 24,
    };
    client
        .send(&ClientMessage::Command(Command {
            id: CommandId::new("c1"),
            body: open,
        }))
        .await;
    let ServerMessage::Terminals { terminals } = client.recv().await else {
        panic!("expected the terminal list");
    };
    let ServerMessage::CommandAccepted {
        result: CommandResult::TerminalOpened { terminal_id },
        ..
    } = client.recv().await
    else {
        panic!("expected the terminal to open");
    };
    let listed = Terminal {
        terminal_id: terminal_id.clone(),
        purpose: TerminalPurpose::Shell {
            session_id: session.clone(),
        },
    };
    assert_eq!(terminals, std::slice::from_ref(&listed));
    let input = |line: &str| CommandBody::TerminalInput {
        terminal_id: terminal_id.clone(),
        data: herder_protocol::Bytes(format!("{line}\n").into_bytes()),
    };
    client
        .send(&ClientMessage::Command(Command {
            id: CommandId::new("c2"),
            body: input("printf 'he%s\\n' llo"),
        }))
        .await;
    let mut seen = Vec::new();
    let mut accepted = false;
    while !accepted || !String::from_utf8_lossy(&seen).contains("hello") {
        match client.recv().await {
            ServerMessage::TerminalOutput { data, .. } => seen.extend(data.0),
            ServerMessage::CommandAccepted { .. } => accepted = true,
            other => panic!("unexpected {other:?}"),
        }
    }
    drop(client);

    // A new connection lists the terminal, and attaching replays what it wrote.
    let mut client = daemon.client().await;
    client
        .send(&ClientMessage::Hello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client: "test".into(),
            resume: Vec::new(),
            pairing_code: None,
        }))
        .await;
    assert!(matches!(client.recv().await, ServerMessage::Hello(_)));
    assert!(matches!(
        client.recv().await,
        ServerMessage::Sessions { .. }
    ));
    assert!(matches!(
        client.recv().await,
        ServerMessage::Accounts { .. }
    ));
    assert_eq!(
        client.recv().await,
        ServerMessage::Terminals {
            terminals: vec![listed]
        }
    );
    let attach = CommandBody::AttachTerminal {
        terminal_id: terminal_id.clone(),
    };
    let ServerMessage::TerminalOutput { data, .. } = client.command("c3", attach).await else {
        panic!("expected the scrollback");
    };
    assert!(String::from_utf8_lossy(&data.0).contains("hello"));
    assert!(matches!(
        client.recv().await,
        ServerMessage::CommandAccepted { .. }
    ));

    // Exiting the shell closes the terminal.
    client
        .send(&ClientMessage::Command(Command {
            id: CommandId::new("c4"),
            body: input("exit 3"),
        }))
        .await;
    // The input's acceptance may arrive before or after the shell exits.
    let (mut closed, mut listed, mut accepted) = (None, false, false);
    while !(listed && accepted) {
        match client.recv().await {
            ServerMessage::TerminalClosed {
                terminal_id: id,
                exit_code,
            } => closed = Some((id, exit_code)),
            ServerMessage::Terminals { terminals } => {
                assert!(terminals.is_empty() && closed.is_some(), "{terminals:?}");
                listed = true;
            }
            ServerMessage::CommandAccepted { .. } => accepted = true,
            ServerMessage::TerminalOutput { .. } => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(closed, Some((terminal_id.clone(), Some(3))));
    let attach = CommandBody::AttachTerminal { terminal_id };
    assert!(matches!(
        client.command("c5", attach).await,
        ServerMessage::CommandRejected { error, .. } if error.code == ErrorCode::NotFound
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoking_a_device_disconnects_and_refuses_it() {
    let daemon = Daemon::start().await;
    let code = daemon.auth.mint("bob", None, PAIRING_TTL).unwrap().code;
    let device = DeviceKey::generate().unwrap();
    let mut client = daemon.client_on(&device, Some(&code)).await;
    let hello = client.hello(Vec::new()).await;
    assert!(daemon.auth.revoke(&hello.device_id).unwrap());
    assert!(
        client.try_recv().await.is_none(),
        "the connection stayed open"
    );
    let error = refused(daemon.client_on(&device, None).await).await;
    assert_eq!(error.code, ErrorCode::Forbidden);
}
