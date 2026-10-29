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
    Account, AccountId, AwsDeviceCode, AwsProfile, ClientHello, ClientMessage, Command,
    CommandBody, CommandId, CommandResult, Cursor, DaemonSettings, DeviceId, ErrorCode, ErrorInfo,
    Event, EventBody, HostId, Item, ItemBody, ItemId, LeftoverDecision, NewPerson,
    PROTOCOL_VERSION, PasskeyAssertion, PasskeyChallenge, PasskeyCreation, PasskeyKey,
    PermissionMode, PersonId, PersonPasskey, Provider, Role, Seq, ServerHello, ServerMessage,
    SessionHead, SessionId, SignIn, Terminal, TerminalPurpose, TurnId, UserId, UserInfo,
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

use super::{Backend, Host, Server, Tls};
use crate::auth::authenticator::{self, Authenticator};
use crate::auth::passkeys::Passkeys;
use crate::auth::{Auth, PAIRING_TTL};
use crate::aws::Aws;
use crate::hub::{self, DELTA_BACKLOG, Hub};
use crate::login::Logins;
use crate::mcp::Control;
use crate::session::EventSink;
use crate::settings::{self, Settings};
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
    /// The devices hosts replicate from, as a vault knows them.
    replicated: Arc<Mutex<Vec<DeviceId>>>,
}

impl Backend for TestBackend {
    fn accounts(&self) -> Vec<Account> {
        vec![Account {
            config_dir: None,
            account_id: AccountId::new("claude-main"),
            provider: Provider::Claude,
            label: "Main".into(),
            email: None,
            usage: Vec::new(),
            fallback: false,
            needs_login: None,
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
                parent_host: None,
                task: session.task,
                title: session.title,
                project_id: None,
                account_id: session.account_id,
                children_need_you: 0,
                queue: Vec::new(),
                threads: Vec::new(),
                chat: false,
                origin: None,
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
        _: &UserId,
        _: &CommandId,
        command: CommandBody,
    ) -> Result<CommandResult, ErrorInfo> {
        self.commands.fetch_add(1, Ordering::SeqCst);
        match command {
            CommandBody::SendPrompt { .. }
            | CommandBody::RemoveQueued { .. }
            | CommandBody::MoveQueued { .. }
            | CommandBody::SendQueuedNow { .. }
            | CommandBody::MergeQueued { .. } => Ok(CommandResult::Applied),
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

    async fn replicated(&self) -> anyhow::Result<Vec<DeviceId>> {
        Ok(self.replicated.lock().unwrap().clone())
    }
}

/// A running server, with the journal writer the session manager would own.
struct Daemon {
    addr: SocketAddr,
    /// A second address the server listens on, as a daemon given a list of them does.
    other: SocketAddr,
    fingerprint: String,
    auth: Arc<Auth>,
    /// A device paired as the owner.
    owner: DeviceKey,
    hub: Arc<Hub>,
    store: Arc<Mutex<Store>>,
    commands: Arc<AtomicUsize>,
    /// The devices hosts replicate from, when the daemon plays a vault.
    replicated: Arc<Mutex<Vec<DeviceId>>>,
    shutdown: CancellationToken,
    /// The daemon's config file, which the settings commands change.
    config: PathBuf,
    /// The commands the server runs for agents.
    control: Arc<dyn Control>,
    /// The host's AWS CLI setup: `bin`, its whole `PATH`, and `config`, its config file;
    /// both absent until a test writes them.
    aws: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Daemon {
    /// A daemon whose owner is the person `tomas`, signed in on the owner's device.
    async fn start() -> Arc<Self> {
        Self::start_with(Owner::Person).await
    }

    /// A daemon with no owner, nobody paired: as `herder service install` leaves it.
    async fn start_fresh() -> Arc<Self> {
        Self::start_with(Owner::None).await
    }

    /// A daemon from before people, unclaimed: its owner is the per-daemon user `owner`.
    async fn start_unclaimed() -> Arc<Self> {
        Self::start_with(Owner::User).await
    }

    async fn start_with(seeded: Owner) -> Arc<Self> {
        let tmp = tempfile::tempdir().unwrap();
        let tls = Tls::load_or_create(tmp.path(), "localhost").unwrap();
        let store = Arc::new(Mutex::new(
            Store::open(tmp.path().join("journal.sqlite3")).unwrap(),
        ));
        let commands = Arc::new(AtomicUsize::new(0));
        let replicated = Arc::new(Mutex::new(Vec::new()));
        let backend = TestBackend {
            store: Arc::clone(&store),
            commands: Arc::clone(&commands),
            worktree: tmp.path().to_owned(),
            replicated: Arc::clone(&replicated),
        };
        let hub = Arc::new(Hub::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let other_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let other = other_listener.local_addr().unwrap();
        let shutdown = CancellationToken::new();
        let host = Host {
            id: HostId::new("host"),
            name: "localhost".into(),
        };
        let fingerprint = tls.fingerprint().to_owned();
        let owner = DeviceKey::generate().unwrap();
        if seeded == Owner::User {
            std::fs::write(tmp.path().join("auth.json"), legacy_owner(&owner)).unwrap();
        }
        let auth = Arc::new(Auth::open(tmp.path()).unwrap());
        if seeded == Owner::Person {
            authenticator::enroll(&auth, "tomas", Role::Owner, &owner.fingerprint());
        }
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
        let config = tmp.path().join("daemon.toml");
        std::fs::write(&config, CONFIG).unwrap();
        let settings = Settings::new(
            &crate::Config::load_file(&config).unwrap(),
            None,
            shutdown.clone(),
        );
        server.manage_settings(Arc::new(settings)).unwrap();
        let bots = crate::bots::Bots::new(&crate::Config::load_file(&config).unwrap());
        server.manage_bots(Arc::new(bots)).unwrap();
        let aws = tmp.path().join("aws");
        let vars = [
            ("PATH", aws.join("bin")),
            ("HOME", tmp.path().to_owned()),
            ("AWS_CONFIG_FILE", aws.join("config")),
        ];
        let env = move |key: &str| {
            vars.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.clone().into_os_string())
        };
        server.manage_aws(Aws::from_env(env)).unwrap();
        let control = server.control();
        tokio::spawn(server.run(vec![listener, other_listener], shutdown.clone()));
        Arc::new(Self {
            addr,
            other,
            fingerprint,
            auth,
            owner,
            hub,
            store,
            commands,
            replicated,
            shutdown,
            config,
            control,
            aws,
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
                branch: Some("b".into()),
                provider: Provider::Claude,
                account_id: AccountId::new("a"),
                model: "m".into(),
                permission_mode: PermissionMode::Ask,
                parent: None,
                parent_host: None,
                task: None,
                failover_pin: None,
                chat: false,
                origin: None,
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

    /// A device signed in as the new person `name`, a member.
    fn member(&self, name: &str) -> DeviceKey {
        let device = DeviceKey::generate().unwrap();
        authenticator::enroll(&self.auth, name, Role::Member, &device.fingerprint());
        device
    }

    /// A client on `device`, sending `code` if given.
    async fn client_on(&self, device: &DeviceKey, code: Option<&str>) -> Client {
        Client::connect(self.addr, &self.fingerprint, device, code, None)
            .await
            .unwrap()
    }
}

/// Who owns a test daemon as it starts.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner {
    None,
    /// A person, by passkey.
    Person,
    /// A per-daemon user, as installs from before people have one.
    User,
}

/// The users file of an install from before people, whose owner `owner` paired `device`.
fn legacy_owner(device: &DeviceKey) -> String {
    serde_json::json!({
        "version": 0,
        "users": [{
            "user_id": "01J9A0OWNER00000000000000",
            "name": "owner",
            "role": "owner",
            "created_at": "2026-10-03T09:41:40.130129Z",
        }],
        "devices": [{
            "device_id": "01J9A0DOWNER0000000000000",
            "user_id": "01J9A0OWNER00000000000000",
            "client": "herder-ios/0.1.0",
            "fingerprint": device.fingerprint(),
            "paired_at": "2026-10-03T09:41:40.130129Z",
            "role": "client",
        }],
    })
    .to_string()
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

fn message(id: &str, text: &str) -> Item {
    Item {
        agent_message: None,
        follow_up: None,
        parent_call_id: None,
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
            sign_in: false,
            passkey: false,
        }))
        .await;
        let ServerMessage::Hello(hello) = self.recv().await else {
            panic!("expected a hello");
        };
        self.lists(&hello).await;
        hello
    }

    /// Signs in as a person: says hello with `sign_in`, answers the challenge with what
    /// `answer` makes of it, and reads the server hello and the lists that follow it.
    async fn sign_in(
        &mut self,
        answer: impl FnOnce(PasskeyChallenge, Option<PasskeyCreation>) -> SignIn,
    ) -> ServerHello {
        self.send(&ClientMessage::Hello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client: "test".into(),
            resume: Vec::new(),
            pairing_code: None,
            sign_in: true,
            passkey: true,
        }))
        .await;
        let ServerMessage::SignInChallenge {
            challenge,
            registration,
        } = self.recv().await
        else {
            panic!("expected a sign-in challenge");
        };
        self.send(&ClientMessage::SignIn(answer(challenge, registration)))
            .await;
        let ServerMessage::Hello(hello) = self.recv().await else {
            panic!("expected a hello");
        };
        self.lists(&hello).await;
        hello
    }

    /// Reads the lists that follow the server hello.
    async fn lists(&mut self, hello: &ServerHello) {
        assert!(matches!(self.recv().await, ServerMessage::Sessions { .. }));
        assert!(matches!(self.recv().await, ServerMessage::Accounts { .. }));
        assert!(matches!(self.recv().await, ServerMessage::Providers { .. }));
        if hello.role == Role::Owner {
            assert!(matches!(self.recv().await, ServerMessage::Terminals { .. }));
        }
    }

    async fn command(&mut self, id: &str, body: CommandBody) -> ServerMessage {
        self.send(&ClientMessage::Command(Command {
            id: CommandId::new(id),
            body,
        }))
        .await;
        self.recv().await
    }

    /// Sends a command and skips what the server broadcasts until its answer.
    async fn answer(&mut self, id: &str, body: CommandBody) -> ServerMessage {
        self.send(&ClientMessage::Command(Command {
            id: CommandId::new(id),
            body,
        }))
        .await;
        loop {
            let message = self.recv().await;
            if matches!(
                message,
                ServerMessage::CommandAccepted { .. } | ServerMessage::CommandRejected { .. }
            ) {
                return message;
            }
        }
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

/// An assertion over a challenge no daemon minted.
fn stale_assertion() -> PasskeyAssertion {
    let passkey = Authenticator::new("tomas", &[0; 16]);
    passkey.assert_raw("bm90LW1pbnRlZA", "https://herder.sh")
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
            sign_in: false,
            passkey: false,
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
            sign_in: false,
            passkey: false,
        }))
        .await;
    let ServerMessage::Error { error } = client.recv().await else {
        panic!("expected an error");
    };
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(client.try_recv().await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_command_this_build_cannot_read_is_refused_as_that_command() {
    let daemon = Daemon::start().await;
    let mut client = daemon.client().await;
    client.hello(Vec::new()).await;
    // As a client from another build may send it: a command this build does not know.
    let text = r#"{"type":"command","id":"c1","body":{"type":"no_such_command"}}"#;
    client.ws.send(Message::text(text)).await.unwrap();
    let ServerMessage::CommandRejected { command_id, error } = client.recv().await else {
        panic!("expected the command refused");
    };
    assert_eq!(command_id, CommandId::new("c1"));
    assert_eq!(error.code, ErrorCode::BadRequest);
    // A frame that is no command still gets a plain error.
    client.ws.send(Message::text("{}")).await.unwrap();
    assert!(matches!(
        client.recv().await,
        ServerMessage::Error { error } if error.code == ErrorCode::BadRequest
    ));
    assert_eq!(daemon.commands.load(Ordering::SeqCst), 0);
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
        files: Vec::new(),
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
        files: Vec::new(),
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
            sign_in: false,
            passkey: false,
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
    let daemon = Daemon::start_unclaimed().await;
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
    let daemon = Daemon::start_unclaimed().await;
    let code = daemon.auth.mint("bob", None, Duration::ZERO).unwrap().code;
    let device = DeviceKey::generate().unwrap();
    let error = refused(daemon.client_on(&device, Some(&code)).await).await;
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert!(error.message.contains("expired"), "{}", error.message);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_code_pairs_one_device_only() {
    let daemon = Daemon::start_unclaimed().await;
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

/// Asks for a `pair_device` code on `client`: the code, fingerprint and addresses.
async fn share(client: &mut Client, id: &str) -> (String, String, Vec<String>) {
    let ServerMessage::CommandAccepted {
        result:
            CommandResult::DevicePairing {
                code,
                fingerprint,
                addresses,
                ..
            },
        ..
    } = client.command(id, own_pairing()).await
    else {
        panic!("expected a device pairing");
    };
    (code, fingerprint, addresses)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_listen_address_serves_clients() {
    let daemon = Daemon::start().await;
    for addr in [daemon.other, daemon.addr] {
        let mut client = Client::connect(addr, &daemon.fingerprint, &daemon.owner, None, None)
            .await
            .unwrap();
        let hello = client.hello(Vec::new()).await;
        assert_eq!(hello.role, Role::Owner);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paired_device_pairs_another_as_its_own_user_and_role() {
    let daemon = Daemon::start_unclaimed().await;
    let member = DeviceKey::generate().unwrap();
    let code = daemon.auth.mint("bob", None, PAIRING_TTL).unwrap().code;
    let mut bob = daemon.client_on(&member, Some(&code)).await;
    let bob_hello = bob.hello(Vec::new()).await;
    let mut owner = daemon.client().await;
    let owner_hello = owner.hello(Vec::new()).await;

    for (client, sharer) in [(&mut owner, &owner_hello), (&mut bob, &bob_hello)] {
        let (code, fingerprint, addresses) = share(client, "share").await;
        assert_eq!(fingerprint, daemon.fingerprint);
        // The daemon's own addresses: bound to two, it advertises both, in order.
        assert_eq!(
            addresses,
            [daemon.addr.to_string(), daemon.other.to_string()]
        );
        let phone = DeviceKey::generate().unwrap();
        let mut paired = daemon.client_on(&phone, Some(&code)).await;
        let hello = paired.hello(Vec::new()).await;
        assert_eq!((&hello.user_id, hello.role), (&sharer.user_id, sharer.role));
        assert_ne!(hello.device_id, sharer.device_id);
        if hello.role == Role::Member {
            // A member's code pairs a member: still no terminals.
            let open = CommandBody::OpenTerminal {
                session_id: SessionId::new("s1"),
                cols: 80,
                rows: 24,
                assertion: None,
            };
            let ServerMessage::CommandRejected { error, .. } = paired.command("t", open).await
            else {
                panic!("expected a refusal");
            };
            assert_eq!(error.code, ErrorCode::Forbidden);
        }
        // The code works once.
        let other = DeviceKey::generate().unwrap();
        let error = refused(daemon.client_on(&other, Some(&code)).await).await;
        assert_eq!(error.code, ErrorCode::Forbidden);
    }
    // A resend of the same command is answered with the same code; a new one mints afresh.
    let (first, ..) = share(&mut owner, "again").await;
    assert_eq!(share(&mut owner, "again").await.0, first);
    assert_ne!(share(&mut owner, "fresh").await.0, first);
    assert_eq!(daemon.auth.devices().len(), 4);
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
    let device = daemon.member("bob");
    let mut member = daemon.client_on(&device, None).await;
    member.hello(Vec::new()).await;
    let open = |id: &str| {
        ClientMessage::Command(Command {
            id: CommandId::new(id),
            body: CommandBody::OpenTerminal {
                session_id: session.clone(),
                cols: 80,
                rows: 24,
                assertion: None,
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
                files: Vec::new(),
            },
        }))
        .await;
    assert!(matches!(
        member.recv().await,
        ServerMessage::CommandAccepted { .. }
    ));
    // And edit their queues, as whoever may prompt a session may.
    let prompt_id = herder_protocol::PromptId::new("p1");
    let edits = [
        CommandBody::RemoveQueued {
            session_id: session.clone(),
            prompt_id: prompt_id.clone(),
        },
        CommandBody::MoveQueued {
            session_id: session.clone(),
            prompt_id: prompt_id.clone(),
            before: None,
        },
        CommandBody::SendQueuedNow {
            session_id: session.clone(),
            prompt_id: prompt_id.clone(),
        },
        CommandBody::MergeQueued {
            session_id: session.clone(),
            prompt_ids: vec![prompt_id, herder_protocol::PromptId::new("p2")],
        },
    ];
    for (n, edit) in edits.into_iter().enumerate() {
        assert!(matches!(
            member.command(&format!("q{n}"), edit).await,
            ServerMessage::CommandAccepted { .. }
        ));
    }

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
        files: Vec::new(),
    };
    assert!(matches!(
        member.command("c4", prompt).await,
        ServerMessage::CommandAccepted { .. }
    ));
    assert_eq!(daemon.commands.load(Ordering::SeqCst), 6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn compose_down_is_for_owners_only() {
    let daemon = Daemon::start().await;
    let session = daemon.create_session("s1");
    let device = daemon.member("bob");
    let mut member = daemon.client_on(&device, None).await;
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
async fn only_owners_change_the_skill_library() {
    let daemon = Daemon::start().await;
    let device = daemon.member("bob");
    let mut member = daemon.client_on(&device, None).await;
    member.hello(Vec::new()).await;
    let commands = || {
        [
            CommandBody::SetSkillsRepo {
                url: "https://github.com/you/herder-skills".into(),
            },
            CommandBody::PutSkill {
                name: "deploy".into(),
                files: vec![herder_protocol::SkillFile {
                    path: "SKILL.md".into(),
                    data: herder_protocol::Bytes(b"---".to_vec()),
                    executable: false,
                }],
            },
            CommandBody::DeleteSkill {
                name: "deploy".into(),
            },
            CommandBody::ImportSkill {
                git_url: "https://github.com/you/skills".into(),
                path: None,
            },
            CommandBody::PullSkills,
            CommandBody::SetSkillEnabled {
                name: "deploy".into(),
                enabled: false,
            },
        ]
    };
    for (n, body) in commands().into_iter().enumerate() {
        let id = format!("m{n}");
        let ServerMessage::CommandRejected { error, .. } = member.command(&id, body).await else {
            panic!("expected a rejection");
        };
        assert_eq!(error.code, ErrorCode::Forbidden);
    }
    assert_eq!(daemon.commands.load(Ordering::SeqCst), 0);

    // Owners reach the backend.
    let mut owner = daemon.client().await;
    owner.hello(Vec::new()).await;
    for (n, body) in commands().into_iter().enumerate() {
        let id = format!("o{n}");
        let ServerMessage::CommandRejected { error, .. } = owner.command(&id, body).await else {
            panic!("expected the test backend's rejection");
        };
        assert_eq!(error.code, ErrorCode::Unsupported);
    }
    assert_eq!(daemon.commands.load(Ordering::SeqCst), 6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn browsing_folders_is_for_owners_only_and_never_remembered() {
    let daemon = Daemon::start().await;
    let device = daemon.member("bob");
    let mut member = daemon.client_on(&device, None).await;
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
            name: None,
            default_permission_mode: None,
            default_account: None,
            setup_command: None,
            icon_background: None,
            private_artifacts: false,
        },
        CommandBody::RemoveProject {
            project_id: herder_protocol::ProjectId::new("github.com/org/app"),
        },
        CommandBody::SetProjectIcon {
            project_id: herder_protocol::ProjectId::new("github.com/org/app"),
            icon: None,
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
    let device = daemon.member("bob");
    let mut member = daemon.client_on(&device, None).await;
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
async fn forking_a_session_is_for_owners_only() {
    let daemon = Daemon::start().await;
    let device = daemon.member("bob");
    let mut member = daemon.client_on(&device, None).await;
    member.hello(Vec::new()).await;
    let fork = || CommandBody::ForkSession {
        session_id: SessionId::new("s1"),
        account_id: None,
        relay: None,
    };
    let ServerMessage::CommandRejected { error, .. } = member.command("c1", fork()).await else {
        panic!("expected a rejection");
    };
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert!(error.message.contains("owners only"), "{}", error.message);
    // Nor may a member upload a history to fork, which could put words in others' mouths.
    let upload = CommandBody::UploadHistory {
        session_id: SessionId::new("s1"),
        part: herder_protocol::HistoryPart::Events { events: Vec::new() },
    };
    let ServerMessage::CommandRejected { error, .. } = member.command("c3", upload).await else {
        panic!("expected a rejection");
    };
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert_eq!(daemon.commands.load(Ordering::SeqCst), 0);

    let mut owner = daemon.client().await;
    owner.hello(Vec::new()).await;
    // The owner's reaches the backend.
    let ServerMessage::CommandRejected { error, .. } = owner.command("c2", fork()).await else {
        panic!("expected the test backend's rejection");
    };
    assert_eq!(error.code, ErrorCode::Unsupported);
    assert_eq!(daemon.commands.load(Ordering::SeqCst), 1);
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
        assertion: None,
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
            sign_in: false,
            passkey: false,
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
    assert!(matches!(
        client.recv().await,
        ServerMessage::Providers { .. }
    ));
    assert_eq!(
        client.recv().await,
        ServerMessage::Terminals {
            terminals: vec![listed]
        }
    );
    let attach = CommandBody::AttachTerminal {
        terminal_id: terminal_id.clone(),
        assertion: None,
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
    let attach = CommandBody::AttachTerminal {
        terminal_id,
        assertion: None,
    };
    assert!(matches!(
        client.command("c5", attach).await,
        ServerMessage::CommandRejected { error, .. } if error.code == ErrorCode::NotFound
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoking_a_device_disconnects_and_refuses_it() {
    let daemon = Daemon::start().await;
    let device = daemon.member("bob");
    let mut client = daemon.client_on(&device, None).await;
    let hello = client.hello(Vec::new()).await;
    assert!(daemon.auth.revoke(&hello.device_id).unwrap());
    assert!(
        client.try_recv().await.is_none(),
        "the connection stayed open"
    );
    let error = refused(daemon.client_on(&device, None).await).await;
    assert_eq!(error.code, ErrorCode::Forbidden);
}

/// A `pair_device` for the caller's own user and role.
fn own_pairing() -> CommandBody {
    CommandBody::PairDevice {
        user: None,
        role: None,
    }
}

/// The users a `list_users` on `client` answers with.
async fn users(client: &mut Client, id: &str) -> Vec<UserInfo> {
    let ServerMessage::CommandAccepted {
        result: CommandResult::Users { users },
        ..
    } = client.command(id, CommandBody::ListUsers).await
    else {
        panic!("expected the users");
    };
    users
}

/// The refusal of `body` on `client`.
async fn rejected(client: &mut Client, id: &str, body: CommandBody) -> ErrorInfo {
    let ServerMessage::CommandRejected { error, .. } = client.command(id, body).await else {
        panic!("expected a refusal");
    };
    error
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn everyone_lists_the_users_with_their_devices() {
    let daemon = Daemon::start_unclaimed().await;
    let mut owner = daemon.client().await;
    let owner_hello = owner.hello(Vec::new()).await;
    let code = daemon.auth.mint("bob", None, PAIRING_TTL).unwrap().code;
    let device = DeviceKey::generate().unwrap();
    let mut bob = daemon.client_on(&device, Some(&code)).await;
    let bob_hello = bob.hello(Vec::new()).await;

    let listed = users(&mut owner, "u1").await;
    assert_eq!(users(&mut bob, "u1").await, listed);
    let names: Vec<_> = listed.iter().map(|u| (u.name.as_str(), u.role)).collect();
    assert_eq!(names, [("owner", Role::Owner), ("bob", Role::Member)]);
    assert_eq!(listed[0].user_id, owner_hello.user_id);
    let bobs = &listed[1].devices;
    assert_eq!(bobs.len(), 1);
    assert_eq!(bobs[0].device_id, bob_hello.device_id);
    assert_eq!(bobs[0].client, "test");
    assert!(bobs[0].online && !bobs[0].host_only);
    assert!(bobs[0].last_seen.is_some());

    drop(bob);
    tokio::time::timeout(TIMEOUT, async {
        while users(&mut owner, "u2").await[1].devices[0].online {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the closed connection still shows online");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owner_changes_roles_but_never_leaves_no_owner() {
    let daemon = Daemon::start_unclaimed().await;
    let mut owner = daemon.client().await;
    let owner_hello = owner.hello(Vec::new()).await;

    let demote_self = CommandBody::SetUserRole {
        user_id: owner_hello.user_id.clone(),
        role: Role::Member,
    };
    let error = rejected(&mut owner, "r1", demote_self.clone()).await;
    assert_eq!(error.code, ErrorCode::Conflict);
    assert!(error.message.contains("last owner"), "{}", error.message);
    let nobody = CommandBody::SetUserRole {
        user_id: UserId::new("nobody"),
        role: Role::Owner,
    };
    assert_eq!(
        rejected(&mut owner, "r2", nobody).await.code,
        ErrorCode::NotFound
    );

    let code = daemon.auth.mint("bob", None, PAIRING_TTL).unwrap().code;
    let device = DeviceKey::generate().unwrap();
    let mut bob = daemon.client_on(&device, Some(&code)).await;
    let bob_hello = bob.hello(Vec::new()).await;
    assert_eq!(bob_hello.role, Role::Member);
    let promote = CommandBody::SetUserRole {
        user_id: bob_hello.user_id.clone(),
        role: Role::Owner,
    };
    // A member changes no one's role, their own included.
    assert_eq!(
        rejected(&mut bob, "r3", promote.clone()).await.code,
        ErrorCode::Forbidden
    );
    assert!(matches!(
        owner.command("r4", promote).await,
        ServerMessage::CommandAccepted {
            result: CommandResult::Applied,
            ..
        }
    ));
    // Bob's connection carried the member role, so it closes; it comes back an owner.
    assert!(bob.try_recv().await.is_none(), "the connection stayed open");
    let again = daemon
        .client_on(&device, None)
        .await
        .hello(Vec::new())
        .await;
    assert_eq!(again.role, Role::Owner);
    // With bob an owner, the first owner may step down, which closes their own connection.
    owner
        .send(&ClientMessage::Command(Command {
            id: CommandId::new("r5"),
            body: demote_self,
        }))
        .await;
    while owner.try_recv().await.is_some() {}
    assert_eq!(daemon.auth.role(&owner_hello.user_id), Some(Role::Member));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owner_revokes_a_device_and_it_is_disconnected() {
    let daemon = Daemon::start_unclaimed().await;
    let mut owner = daemon.client().await;
    owner.hello(Vec::new()).await;
    let code = daemon.auth.mint("bob", None, PAIRING_TTL).unwrap().code;
    let device = DeviceKey::generate().unwrap();
    let mut bob = daemon.client_on(&device, Some(&code)).await;
    let bob_hello = bob.hello(Vec::new()).await;
    let revoke = CommandBody::RevokeDevice {
        device_id: bob_hello.device_id.clone(),
        assertion: None,
    };

    assert_eq!(
        rejected(&mut bob, "v1", revoke.clone()).await.code,
        ErrorCode::Forbidden
    );
    assert!(matches!(
        owner.command("v2", revoke).await,
        ServerMessage::CommandAccepted {
            result: CommandResult::Applied,
            ..
        }
    ));
    assert!(bob.try_recv().await.is_none(), "the connection stayed open");
    let error = refused(daemon.client_on(&device, None).await).await;
    assert_eq!(error.code, ErrorCode::Forbidden);
    // Bob stays a user, with no devices.
    let listed = users(&mut owner, "v3").await;
    assert!(listed[1].devices.is_empty(), "{listed:?}");
    let gone = CommandBody::RevokeDevice {
        device_id: bob_hello.device_id,
        assertion: None,
    };
    assert_eq!(
        rejected(&mut owner, "v4", gone).await.code,
        ErrorCode::NotFound
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owner_mints_codes_for_others_and_a_member_only_for_themselves() {
    let daemon = Daemon::start_unclaimed().await;
    let mut owner = daemon.client().await;
    owner.hello(Vec::new()).await;
    let invite = |user: &str, role| CommandBody::PairDevice {
        user: Some(user.into()),
        role,
    };

    for (id, user, role) in [("p1", "carol", Role::Owner), ("p2", "dave", Role::Member)] {
        let ServerMessage::CommandAccepted {
            result: CommandResult::DevicePairing { code, .. },
            ..
        } = owner.command(id, invite(user, Some(role))).await
        else {
            panic!("expected a device pairing");
        };
        let device = DeviceKey::generate().unwrap();
        let hello = daemon
            .client_on(&device, Some(&code))
            .await
            .hello(Vec::new())
            .await;
        assert_eq!(hello.role, role);
        assert_eq!(daemon.auth.name(&hello.user_id).as_deref(), Some(user));
    }
    // An existing user keeps their role; a code cannot change it.
    let error = rejected(&mut owner, "p3", invite("dave", Some(Role::Owner))).await;
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(
        error.message.contains("already a member"),
        "{}",
        error.message
    );
    assert_eq!(
        rejected(&mut owner, "p4", invite(" ", None)).await.code,
        ErrorCode::BadRequest
    );

    let code = daemon.auth.mint("bob", None, PAIRING_TTL).unwrap().code;
    let device = DeviceKey::generate().unwrap();
    let mut bob = daemon.client_on(&device, Some(&code)).await;
    bob.hello(Vec::new()).await;
    for (id, body) in [
        ("m1", invite("eve", None)),
        ("m2", invite("bob", Some(Role::Member))),
        (
            "m3",
            CommandBody::PairDevice {
                user: None,
                role: Some(Role::Owner),
            },
        ),
    ] {
        assert_eq!(
            rejected(&mut bob, id, body).await.code,
            ErrorCode::Forbidden
        );
    }
    assert!(matches!(
        bob.command("m4", own_pairing()).await,
        ServerMessage::CommandAccepted {
            result: CommandResult::DevicePairing { .. },
            ..
        }
    ));
}

/// The config file every test daemon starts with.
const CONFIG: &str = "# mine\nlisten = \"0.0.0.0:7447\" # everywhere\n";

/// The settings a `settings` answer carries, and whether they need a restart.
fn settings_answer(message: ServerMessage) -> (DaemonSettings, bool) {
    match message {
        ServerMessage::CommandAccepted {
            result:
                CommandResult::Settings {
                    settings,
                    restart_required,
                    ..
                },
            ..
        } => (*settings, restart_required),
        other => panic!("not settings: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owner_changes_the_settings_which_apply_on_restart() {
    let daemon = Daemon::start().await;
    let mut client = daemon.client().await;
    client.hello(Vec::new()).await;
    let (settings, restart_required) =
        settings_answer(client.command("c1", CommandBody::GetSettings).await);
    assert!(!restart_required);
    assert!(!settings.failover.pin);

    let mut changed = settings.clone();
    changed.listen = vec!["127.0.0.1:7999".into(), "[::1]:7999".into()];
    changed.failover.pin = true;
    let set = CommandBody::SetSettings {
        settings: Box::new(changed.clone()),
    };
    let (settings, restart_required) = settings_answer(client.command("c2", set).await);
    assert_eq!(settings, changed);
    assert!(restart_required);
    assert_eq!(
        std::fs::read_to_string(&daemon.config).unwrap(),
        CONFIG.replace("\"0.0.0.0:7447\"", "[\"127.0.0.1:7999\", \"[::1]:7999\"]")
            + "\n[failover]\npin = true\n"
    );
    // Still not in effect for whoever asks next.
    let (_, restart_required) =
        settings_answer(client.command("c3", CommandBody::GetSettings).await);
    assert!(restart_required);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn settings_the_daemon_cannot_run_with_are_refused() {
    let daemon = Daemon::start().await;
    let mut client = daemon.client().await;
    client.hello(Vec::new()).await;
    let (settings, _) = settings_answer(client.command("c1", CommandBody::GetSettings).await);
    let mut cases = Vec::new();
    let mut new = settings.clone();
    // TEST-NET-3: never an address of this machine.
    new.listen = vec!["127.0.0.1:7447".into(), "203.0.113.9:7447".into()];
    cases.push(new);
    let mut new = settings.clone();
    new.listen = vec!["everywhere".into()];
    cases.push(new);
    let mut new = settings.clone();
    new.listen = Vec::new();
    cases.push(new);
    let mut new = settings.clone();
    new.listen = vec!["127.0.0.1:7447".into(), "127.0.0.1:7447".into()];
    cases.push(new);
    let mut new = settings.clone();
    new.resources.nice = 30;
    cases.push(new);
    let mut new = settings.clone();
    new.backup.attachments = true;
    cases.push(new);
    for (n, new) in cases.into_iter().enumerate() {
        let set = CommandBody::SetSettings {
            settings: Box::new(new),
        };
        assert!(matches!(
            client.command(&format!("c{}", n + 2), set).await,
            ServerMessage::CommandRejected { error, .. } if error.code == ErrorCode::BadRequest
        ));
    }
    assert_eq!(std::fs::read_to_string(&daemon.config).unwrap(), CONFIG);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_is_answered_before_the_daemon_stops() {
    let daemon = Daemon::start().await;
    let mut client = daemon.client().await;
    client.hello(Vec::new()).await;
    assert!(matches!(
        client.command("c1", CommandBody::RestartDaemon).await,
        ServerMessage::CommandAccepted {
            result: CommandResult::Applied,
            ..
        }
    ));
    tokio::time::timeout(Duration::from_secs(5), daemon.shutdown.cancelled())
        .await
        .unwrap();
    assert!(settings::restart_requested());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn updates_are_refused_for_members_and_where_no_service_restarts_the_daemon() {
    let daemon = Daemon::start().await;
    let update = CommandBody::UpdateDaemon {
        when: herder_protocol::UpdateWhen::Idle,
    };
    let device = daemon.member("bob");
    let mut member = daemon.client_on(&device, None).await;
    member.hello(Vec::new()).await;
    assert!(matches!(
        member.command("m1", update.clone()).await,
        ServerMessage::CommandRejected { error, .. } if error.code == ErrorCode::Forbidden
    ));

    // This daemon runs in a test, not as the service herder installed.
    let mut owner = daemon.client().await;
    owner.hello(Vec::new()).await;
    assert!(matches!(
        owner.command("c1", update).await,
        ServerMessage::CommandRejected { error, .. }
            if error.code == ErrorCode::Unsupported && error.message.contains("herder update")
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn owners_put_and_list_bots_and_members_are_refused() {
    let daemon = Daemon::start().await;
    let mut owner = daemon.client().await;
    owner.hello(Vec::new()).await;
    let bot = herder_protocol::Bot {
        bot_id: herder_protocol::BotId::new("tg"),
        kind: herder_protocol::BotKind::Telegram,
        name: "herder".into(),
        token: "TELEGRAM_TOKEN".parse().unwrap(),
        app_token: None,
        allow: Vec::new(),
        defaults: herder_protocol::BotDefaults::default(),
        channel_bindings: Vec::new(),
        instructions: None,
    };
    let put = CommandBody::PutBot {
        bot: Box::new(bot.clone()),
    };
    assert!(matches!(
        owner.command("c1", put.clone()).await,
        ServerMessage::CommandAccepted {
            result: CommandResult::Applied,
            ..
        }
    ));
    assert_eq!(
        owner.command("c2", CommandBody::ListBots).await,
        ServerMessage::CommandAccepted {
            command_id: CommandId::new("c2"),
            result: CommandResult::Bots {
                bots: vec![bot.clone()]
            },
        }
    );

    let device = daemon.member("bob");
    let mut member = daemon.client_on(&device, None).await;
    member.hello(Vec::new()).await;
    let remove = CommandBody::RemoveBot {
        bot_id: bot.bot_id.clone(),
    };
    for (n, command) in [
        CommandBody::ListBots,
        put,
        remove,
        CommandBody::GetBotStatus,
        CommandBody::GetBotMemory {
            bot_id: bot.bot_id.clone(),
        },
        CommandBody::SetBotFact {
            bot_id: bot.bot_id.clone(),
            fact_id: None,
            text: "Ada leads payments".into(),
        },
        CommandBody::ClearBotMemory {
            bot_id: bot.bot_id.clone(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        assert!(matches!(
            member.command(&format!("m{n}"), command).await,
            ServerMessage::CommandRejected { error, .. } if error.code == ErrorCode::Forbidden
        ));
    }
    assert_eq!(
        crate::Config::load_file(&daemon.config).unwrap().bots,
        [bot]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agents_act_for_a_bot_person_as_the_user_they_are_linked_to() {
    let daemon = Daemon::start().await;
    let mut owner_client = daemon.client().await;
    let owner = owner_client.hello(Vec::new()).await.user_id;
    let device = daemon.member("bob");
    let mut member_client = daemon.client_on(&device, None).await;
    let member = member_client.hello(Vec::new()).await.user_id;
    let person = |id: &str, name: &str, user_id: Option<&UserId>| herder_protocol::BotUser {
        external_user_id: id.into(),
        display_name: name.into(),
        dm: true,
        channels: true,
        approve: false,
        notify: false,
        owner: false,
        user_id: user_id.cloned(),
    };
    let mut bot = herder_protocol::Bot {
        bot_id: herder_protocol::BotId::new("tg"),
        kind: herder_protocol::BotKind::Telegram,
        name: "trash-can".into(),
        token: "TELEGRAM_TOKEN".parse().unwrap(),
        app_token: None,
        allow: vec![
            person("1", "Ada", Some(&owner)),
            person("2", "Bob", Some(&member)),
            person("3", "Kim", None),
        ],
        defaults: herder_protocol::BotDefaults::default(),
        channel_bindings: Vec::new(),
        instructions: None,
    };
    let put = |bot: &herder_protocol::Bot| CommandBody::PutBot {
        bot: Box::new(bot.clone()),
    };

    // Only owners link people, and only to users paired with the daemon.
    assert!(matches!(
        member_client.command("m1", put(&bot)).await,
        ServerMessage::CommandRejected { error, .. } if error.code == ErrorCode::Forbidden
    ));
    let mut ghost = bot.clone();
    ghost
        .allow
        .push(person("4", "Cy", Some(&UserId::new("ghost"))));
    assert!(matches!(
        owner_client.command("c1", put(&ghost)).await,
        ServerMessage::CommandRejected { error, .. }
            if error.code == ErrorCode::BadRequest && error.message.contains("Cy is linked to ghost")
    ));
    assert_eq!(crate::Config::load_file(&daemon.config).unwrap().bots, []);
    assert!(matches!(
        owner_client.command("c2", put(&bot)).await,
        ServerMessage::CommandAccepted {
            result: CommandResult::Applied,
            ..
        }
    ));

    // A turn Ada prompted acts as the owner she is linked to; Bob's as a member; Kim's as no one.
    let control = &daemon.control;
    let list_secrets = || CommandBody::ListSecrets;
    let via = |name: &str| UserId::new(format!("via trash-can, {name}"));
    // Past the owners-only check, a host with no vault keeps no secrets.
    let ada = control
        .command(SessionId::new("s1"), via("Ada"), list_secrets())
        .await
        .unwrap_err();
    assert_ne!(ada.code, ErrorCode::Forbidden, "{ada:?}");
    // But no agent handles a secret's value, an owner's turn's neither.
    let value = CommandBody::PutSecret {
        set: "prod".into(),
        name: "API_KEY".into(),
        description: "The API key".into(),
        value: herder_protocol::SecretValue::new("s3cret").unwrap(),
        assertion: None,
    };
    let refused = control
        .command(SessionId::new("s1"), via("Ada"), value)
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::Forbidden);
    assert!(refused.message.contains("store_secret"), "{refused:?}");
    let take = CommandBody::GetSecretToStore {
        session_id: SessionId::new("s1"),
        approval_id: herder_protocol::ApprovalId::new("secret-1"),
    };
    let refused = control
        .command(SessionId::new("s1"), via("Ada"), take)
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::Forbidden);
    assert!(matches!(
        control
            .command(SessionId::new("s1"), via("Ada"), CommandBody::GetSettings)
            .await,
        Ok(CommandResult::Settings { .. })
    ));
    let bob = control
        .command(SessionId::new("s1"), via("Bob"), list_secrets())
        .await
        .unwrap_err();
    assert_eq!(bob.code, ErrorCode::Forbidden);
    let kim = control
        .command(SessionId::new("s1"), via("Kim"), list_secrets())
        .await
        .unwrap_err();
    assert_eq!(kim.code, ErrorCode::Forbidden);
    assert_eq!(
        kim.message,
        "this turn was sent by Kim via trash-can, who isn't linked to a herder user; an owner \
         can link them in the bot's People"
    );
    let gone = control
        .command(
            SessionId::new("s1"),
            UserId::new("gone"),
            CommandBody::GetSettings,
        )
        .await
        .unwrap_err();
    assert_eq!(
        gone.message,
        "this turn was sent by gone, who isn't paired with localhost; prompt the session \
         yourself in a herder app and its agents act for you: \
         herder://session?host=host&session=s1"
    );

    // Two people of one name share a link only when both have it.
    bot.allow.push(person("5", "Ada", None));
    assert!(matches!(
        owner_client.command("c3", put(&bot)).await,
        ServerMessage::CommandAccepted { .. }
    ));
    let twin = control
        .command(SessionId::new("s1"), via("Ada"), CommandBody::GetSettings)
        .await
        .unwrap_err();
    assert_eq!(twin.code, ErrorCode::Forbidden);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agents_run_commands_as_their_user_within_its_role() {
    let daemon = Daemon::start().await;
    let owner = daemon.client().await.hello(Vec::new()).await.user_id;
    let device = daemon.member("bob");
    let member = daemon
        .client_on(&device, None)
        .await
        .hello(Vec::new())
        .await
        .user_id;
    let control = &daemon.control;
    let browse = || CommandBody::ListDirectory { path: "/".into() };

    assert!(matches!(
        control
            .command(SessionId::new("s1"), owner.clone(), browse())
            .await,
        Ok(CommandResult::Directory { .. })
    ));
    assert!(matches!(
        control
            .command(
                SessionId::new("s1"),
                owner.clone(),
                CommandBody::GetSettings
            )
            .await,
        Ok(CommandResult::Settings { .. })
    ));
    assert_eq!(daemon.commands.load(Ordering::SeqCst), 1);

    // A member's agent gets a member's rights only.
    for command in [browse(), CommandBody::GetSettings] {
        let error = control
            .command(SessionId::new("s1"), member.clone(), command)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Forbidden);
    }

    // No agent opens a terminal or mints a pairing code, whoever it acts for; nor does one
    // whose user is gone.
    let refused = [
        CommandBody::OpenTerminal {
            session_id: SessionId::new("s1"),
            cols: 80,
            rows: 24,
            assertion: None,
        },
        CommandBody::LogInAccount {
            account_id: AccountId::new("claude-main"),
            cols: 80,
            rows: 24,
        },
        CommandBody::ListAwsProfiles,
        CommandBody::AwsSsoLogin {
            profile: "prod".into(),
            cols: 80,
            rows: 24,
        },
        CommandBody::PairDevice {
            user: None,
            role: None,
        },
        CommandBody::ListUsers,
        CommandBody::SetUserRole {
            user_id: owner.clone(),
            role: Role::Member,
        },
        CommandBody::RevokeDevice {
            device_id: DeviceId::new("d1"),
            assertion: None,
        },
        // Nor does one see, invite, change or sign in people, even with an assertion.
        CommandBody::PasskeyChallenge,
        CommandBody::ListPeople,
        CommandBody::Invite {
            role: Role::Owner,
            assertion: Box::new(stale_assertion()),
        },
        CommandBody::SetPersonRole {
            person_id: PersonId::new("p1"),
            role: Role::Owner,
            person: None,
            assertion: Box::new(stale_assertion()),
        },
        CommandBody::RevokePerson {
            person_id: PersonId::new("p1"),
            assertion: Box::new(stale_assertion()),
        },
        CommandBody::ApproveLogin {
            code: "K7Q-4PD".into(),
            assertion: Box::new(stale_assertion()),
        },
        CommandBody::ClaimUsers {
            person: Box::new(NewPerson {
                name: "agent".into(),
                passkey: PersonPasskey::Existing {
                    person_id: PersonId::new("p1"),
                    key: PasskeyKey {
                        credential_id: "Y3JlZA".into(),
                        public_key: "pQ".into(),
                    },
                    assertion: stale_assertion(),
                },
            }),
            also: Vec::new(),
        },
        CommandBody::DecideLeftover {
            user_id: owner.clone(),
            decision: LeftoverDecision::ItsMe,
            assertion: Box::new(stale_assertion()),
        },
        CommandBody::PairVaultHost {
            host_name: "devbox".into(),
        },
        CommandBody::RemoveBot {
            bot_id: herder_protocol::BotId::new("trash-can"),
        },
        // A bot's memory is its owners', and its own sessions' through `remember` only.
        CommandBody::GetBotMemory {
            bot_id: herder_protocol::BotId::new("trash-can"),
        },
        CommandBody::SetBotFact {
            bot_id: herder_protocol::BotId::new("trash-can"),
            fact_id: None,
            text: "Ada leads payments".into(),
        },
        CommandBody::DeleteBotFact {
            bot_id: herder_protocol::BotId::new("trash-can"),
            fact_id: herder_protocol::FactId::new("01J9FACT"),
        },
        CommandBody::ClearBotMemory {
            bot_id: herder_protocol::BotId::new("trash-can"),
        },
        CommandBody::UpdateDaemon {
            when: herder_protocol::UpdateWhen::Now,
        },
    ];
    for command in refused {
        let error = control
            .command(SessionId::new("s1"), owner.clone(), command)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Forbidden);
    }
    let gone = control
        .command(SessionId::new("s1"), UserId::new("gone"), browse())
        .await
        .unwrap_err();
    assert_eq!(gone.code, ErrorCode::Forbidden);
    assert_eq!(daemon.commands.load(Ordering::SeqCst), 1);

    let overview = control.overview().await.unwrap();
    assert_eq!(
        overview
            .accounts
            .iter()
            .map(|account| account.account_id.as_str())
            .collect::<Vec<_>>(),
        ["claude-main"]
    );
}

/// A fake `aws` that prints the device-code prompt of `aws sso login --no-browser`, waits a
/// little and exits 0 for profile `prod`, else 1; and a config with `prod` and `broken`.
fn fake_aws(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(
        dir.join("config"),
        "[default]\nregion = eu-west-1\n\n[profile prod]\nsso_session = corp\n\n\
         [profile broken]\nsso_start_url = https://corp.awsapps.com/start\n\n\
         [sso-session corp]\nsso_start_url = https://corp.awsapps.com/start\n",
    )
    .unwrap();
    let script = bin.join("aws");
    std::fs::write(
        &script,
        r#"#!/bin/sh
[ "$1 $2 $3 $5 $6" = "sso login --profile --use-device-code --no-browser" ] || exit 64
printf 'Browser will not be automatically opened.\nPlease visit the following URL:\n\n'
printf 'https://device.sso.eu-west-1.amazonaws.com/\n\nThen enter the code:\n\nABCD-EFGH\n\n'
printf 'Alternatively, you may visit the following URL which will autofill the code upon loading:\n'
printf 'https://device.sso.eu-west-1.amazonaws.com/?user_code=ABCD-EFGH\n'
sleep 1
[ "$4" = prod ] || exit 1
echo "Successfully logged into Start URL: https://corp.awsapps.com/start"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Signs `profile` in through `client`, and returns the terminal lists it saw and the exit
/// code the terminal closed with.
async fn aws_sso_login(
    client: &mut Client,
    id: &str,
    profile: &str,
) -> (Vec<Vec<Terminal>>, Option<i32>) {
    let login = CommandBody::AwsSsoLogin {
        profile: profile.into(),
        cols: 80,
        rows: 24,
    };
    client
        .send(&ClientMessage::Command(Command {
            id: CommandId::new(id),
            body: login,
        }))
        .await;
    let (mut lists, mut opened, mut closed) = (Vec::new(), None, None);
    loop {
        match client.recv().await {
            ServerMessage::Terminals { terminals } => {
                let empty = terminals.is_empty();
                lists.push(terminals);
                if empty && closed.is_some() {
                    break;
                }
            }
            ServerMessage::CommandAccepted {
                result: CommandResult::TerminalOpened { terminal_id },
                ..
            } => opened = Some(terminal_id),
            ServerMessage::TerminalClosed {
                terminal_id,
                exit_code,
            } => {
                assert_eq!(Some(&terminal_id), opened.as_ref());
                closed = Some(exit_code);
            }
            ServerMessage::TerminalOutput { .. } => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    (lists, closed.flatten())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owner_signs_the_host_in_to_aws() {
    let daemon = Daemon::start().await;
    let mut owner = daemon.client().await;
    owner.hello(Vec::new()).await;

    // Without the AWS CLI there is nothing to sign in with, and no config, no profiles.
    let login = |profile: &str| CommandBody::AwsSsoLogin {
        profile: profile.into(),
        cols: 80,
        rows: 24,
    };
    assert!(matches!(
        owner.command("a1", login("prod")).await,
        ServerMessage::CommandRejected { error, .. } if error.code == ErrorCode::Unsupported
    ));
    assert_eq!(
        owner.command("a2", CommandBody::ListAwsProfiles).await,
        ServerMessage::CommandAccepted {
            command_id: CommandId::new("a2"),
            result: CommandResult::AwsProfiles {
                profiles: Vec::new()
            },
        }
    );

    fake_aws(&daemon.aws);
    let sso = |name: &str, sso: bool| AwsProfile {
        name: name.into(),
        sso,
    };
    assert_eq!(
        owner.command("a3", CommandBody::ListAwsProfiles).await,
        ServerMessage::CommandAccepted {
            command_id: CommandId::new("a3"),
            result: CommandResult::AwsProfiles {
                profiles: vec![
                    sso("default", false),
                    sso("prod", true),
                    sso("broken", true)
                ],
            },
        }
    );
    assert!(matches!(
        owner.command("a4", login("nope")).await,
        ServerMessage::CommandRejected { error, .. } if error.code == ErrorCode::BadRequest
    ));

    // A member sees neither the profiles nor a sign-in.
    let device = daemon.member("bob");
    let mut member = daemon.client_on(&device, None).await;
    member.hello(Vec::new()).await;
    for (id, body) in [("m1", CommandBody::ListAwsProfiles), ("m2", login("prod"))] {
        assert!(matches!(
            member.command(id, body).await,
            ServerMessage::CommandRejected { error, .. } if error.code == ErrorCode::Forbidden
        ));
    }

    // The sign-in's terminal gets the device code once the CLI prints it, and closes with
    // the CLI's exit code.
    let device = AwsDeviceCode {
        verification_url: "https://device.sso.eu-west-1.amazonaws.com/?user_code=ABCD-EFGH".into(),
        user_code: "ABCD-EFGH".into(),
    };
    let (lists, exit_code) = aws_sso_login(&mut owner, "a5", "prod").await;
    assert_eq!(exit_code, Some(0));
    let purposes: Vec<_> = lists
        .iter()
        .flatten()
        .map(|terminal| terminal.purpose.clone())
        .collect();
    assert_eq!(
        purposes,
        [
            TerminalPurpose::AwsLogin {
                profile: "prod".into(),
                device: None,
            },
            TerminalPurpose::AwsLogin {
                profile: "prod".into(),
                device: Some(device.clone()),
            },
        ]
    );
    let (lists, exit_code) = aws_sso_login(&mut owner, "a6", "broken").await;
    assert_eq!(exit_code, Some(1));
    assert!(lists.iter().flatten().any(|terminal| terminal.purpose
        == TerminalPurpose::AwsLogin {
            profile: "broken".into(),
            device: Some(device.clone()),
        }));

    // The member heard of none of it.
    member
        .send(&ClientMessage::Sync { token: "t".into() })
        .await;
    assert!(matches!(member.recv().await, ServerMessage::Synced { .. }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fresh_daemon_gets_its_first_owner_by_an_assertion_and_their_devices_need_no_code() {
    let daemon = Daemon::start_fresh().await;
    // Tomas created his passkey for another machine; his app carries its public key here.
    let elsewhere = Passkeys::new().unwrap();
    let (passkey, registration) = Authenticator::register("tomas", &elsewhere.creation().unwrap());
    let (person_id, key) = elsewhere.register(&registration).unwrap();
    let key = key.wire();

    // `herder service install` printed the code; the app shows the same one.
    let (code, _) = daemon.auth.open_claim().unwrap();
    let mac = DeviceKey::generate().unwrap();
    let owner = daemon
        .client_on(&mac, None)
        .await
        .sign_in(|challenge, registration| {
            assert!(
                registration.is_some(),
                "a claimable daemon offers a registration"
            );
            SignIn::ClaimOwnership {
                code: code.clone(),
                person: NewPerson {
                    name: "Tomas".into(),
                    passkey: PersonPasskey::Existing {
                        person_id: person_id.clone(),
                        key: key.clone(),
                        assertion: passkey.assert(&challenge),
                    },
                },
            }
        })
        .await;
    assert_eq!(owner.role, Role::Owner);
    assert_eq!(owner.person_id.as_ref(), Some(&person_id));
    assert_eq!(owner.user_id.as_str(), person_id.as_str());

    // His iPhone has the same passkey, and no code.
    let iphone = DeviceKey::generate().unwrap();
    let mut phone = daemon.client_on(&iphone, None).await;
    let signed_in = phone
        .sign_in(|challenge, registration| {
            assert!(registration.is_none(), "the claim window closed");
            SignIn::Passkey {
                assertion: passkey.assert(&challenge),
            }
        })
        .await;
    assert_eq!(signed_in.user_id, owner.user_id);
    assert_ne!(signed_in.device_id, owner.device_id);

    // A terminal with no passkey waits for the phone to approve it.
    let terminal_key = DeviceKey::generate().unwrap();
    let mut terminal = daemon.client_on(&terminal_key, None).await;
    terminal
        .send(&ClientMessage::Hello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client: "herder-tui/0.7.0".into(),
            resume: Vec::new(),
            pairing_code: None,
            sign_in: true,
            passkey: false,
        }))
        .await;
    assert!(matches!(
        terminal.recv().await,
        ServerMessage::SignInChallenge { .. }
    ));
    terminal
        .send(&ClientMessage::SignIn(SignIn::StartLogin))
        .await;
    let ServerMessage::LoginRequest { code, .. } = terminal.recv().await else {
        panic!("expected a login request");
    };
    let ServerMessage::CommandAccepted {
        result:
            CommandResult::PasskeyChallenge {
                challenge,
                registration: None,
            },
        ..
    } = phone
        .command("challenge", CommandBody::PasskeyChallenge)
        .await
    else {
        panic!("expected a passkey challenge");
    };
    let approve = CommandBody::ApproveLogin {
        code,
        assertion: Box::new(passkey.assert(&challenge)),
    };
    let ServerMessage::CommandAccepted {
        result: CommandResult::LoginApproved { device_id, client },
        ..
    } = phone.command("approve", approve).await
    else {
        panic!("expected the login approved");
    };
    assert_eq!(client, "herder-tui/0.7.0");
    let ServerMessage::Hello(hello) = terminal.recv().await else {
        panic!("expected a hello");
    };
    assert_eq!(
        (hello.user_id, hello.device_id),
        (owner.user_id.clone(), device_id)
    );

    // Each is known from then on, and listed under him.
    let again = daemon
        .client_on(&iphone, None)
        .await
        .hello(Vec::new())
        .await;
    assert_eq!(again.device_id, signed_in.device_id);
    let ServerMessage::CommandAccepted {
        result: CommandResult::People { people },
        ..
    } = phone.command("people", CommandBody::ListPeople).await
    else {
        panic!("expected the people");
    };
    assert_eq!(people.len(), 1);
    assert_eq!(people[0].devices.len(), 3);

    // Owner-only people commands need a fresh assertion of the caller's.
    let invite = CommandBody::Invite {
        role: Role::Member,
        assertion: Box::new(stale_assertion()),
    };
    let ServerMessage::CommandRejected { error, .. } = phone.command("invite", invite).await else {
        panic!("expected the invite refused");
    };
    assert_eq!(error.code, ErrorCode::Forbidden);
}

/// Moves `owner` to the person `0192f3a4-…`, holding no passkey here, with `also`.
fn claim(daemon: &Daemon, owner: &UserId, also: &[UserId]) {
    daemon
        .auth
        .claim_as(
            owner,
            &herder_protocol::PersonId::new("0192f3a4-5b6c-7d8e-9f00-112233445566"),
            "Tomas".into(),
            Vec::new(),
            also,
            &[],
        )
        .unwrap();
}

#[tokio::test]
async fn after_a_claim_the_owners_history_reads_as_the_person() {
    let daemon = Daemon::start_unclaimed().await;
    let owner = daemon.client().await.hello(Vec::new()).await.user_id;
    let bob = DeviceKey::generate().unwrap();
    let code = daemon.auth.mint("bob", None, PAIRING_TTL).unwrap().code;
    let bob = daemon
        .client_on(&bob, Some(&code))
        .await
        .hello(Vec::new())
        .await
        .user_id;
    // Sessions by both, journaled under their per-daemon ids.
    let session = daemon.create_session("s1");
    let by = |user: &UserId| {
        daemon
            .store
            .lock()
            .unwrap()
            .append(NewEvent {
                session_id: session.clone(),
                at: jiff::Timestamp::now(),
                by: Some(user.clone()),
                body: added("i1", "hi"),
            })
            .unwrap()
    };
    by(&owner);
    by(&bob);

    let person = UserId::new("0192f3a4-5b6c-7d8e-9f00-112233445566");
    claim(&daemon, &owner, &[]);
    let mut client = daemon.client().await;
    assert_eq!(client.hello(Vec::new()).await.user_id, person);
    client.subscribe(&session, 0).await;
    client.read_until(|view| view.last_seq() == 3).await;
    let authors: Vec<_> = client.view.events.iter().map(|e| e.by.clone()).collect();
    assert_eq!(authors, [None, Some(person.clone()), Some(bob)]);

    // Live events under the old id too.
    let event = by(&owner);
    daemon.hub.event(&event);
    client.read_until(|view| view.last_seq() == 4).await;
    assert_eq!(client.view.events[3].by, Some(person));
}

#[tokio::test]
async fn after_a_claim_agents_act_for_the_person_by_any_of_their_old_ids() {
    let daemon = Daemon::start_unclaimed().await;
    let mut client = daemon.client().await;
    let owner = client.hello(Vec::new()).await.user_id;
    // The person's id on the machine a forked session came from.
    let elsewhere = UserId::new("01JA0HOSTUSER");
    claim(&daemon, &owner, std::slice::from_ref(&elsewhere));

    // A bot person linked to the old id is still linked, as the person.
    let bot = herder_protocol::Bot {
        bot_id: herder_protocol::BotId::new("tg"),
        kind: herder_protocol::BotKind::Telegram,
        name: "trash-can".into(),
        token: "TELEGRAM_TOKEN".parse().unwrap(),
        app_token: None,
        allow: vec![herder_protocol::BotUser {
            external_user_id: "1".into(),
            display_name: "Ada".into(),
            dm: true,
            channels: true,
            approve: false,
            notify: false,
            owner: false,
            user_id: Some(owner.clone()),
        }],
        defaults: herder_protocol::BotDefaults::default(),
        channel_bindings: Vec::new(),
        instructions: None,
    };
    assert!(matches!(
        client
            .command("c1", CommandBody::PutBot { bot: Box::new(bot) })
            .await,
        ServerMessage::CommandAccepted { .. }
    ));
    let via_ada = UserId::new("via trash-can, Ada");
    for by in [owner, elsewhere, via_ada] {
        let settings = daemon
            .control
            .command(SessionId::new("s1"), by.clone(), CommandBody::GetSettings)
            .await;
        assert!(
            matches!(settings, Ok(CommandResult::Settings { .. })),
            "{by}: {settings:?}"
        );
    }
}

/// Appends an event `by` sent to `session` on `daemon`'s journal, as a host's would be, or
/// as the vault holds it once replicated.
fn journal(daemon: &Daemon, session: &SessionId, by: &UserId) -> Event {
    daemon
        .store
        .lock()
        .unwrap()
        .append(NewEvent {
            session_id: session.clone(),
            at: jiff::Timestamp::now(),
            by: Some(by.clone()),
            body: added("i1", "hi"),
        })
        .unwrap()
}

/// The authors of `session`'s events on `daemon`, as a newly connected client reads them.
async fn authors(daemon: &Daemon, session: &SessionId) -> Vec<Option<UserId>> {
    let mut client = daemon.client().await;
    client.hello(Vec::new()).await;
    client.subscribe(session, 0).await;
    let head = daemon
        .store
        .lock()
        .unwrap()
        .read_since(session, 0, 100)
        .unwrap()
        .len();
    client.read_until(|view| view.events.len() == head).await;
    client.view.events.into_iter().map(|e| e.by).collect()
}

async fn claimed(
    client: &mut Client,
    id: &str,
    person: NewPerson,
    also: Vec<UserId>,
) -> CommandResult {
    let body = CommandBody::ClaimUsers {
        person: Box::new(person),
        also,
    };
    match client.command(id, body).await {
        ServerMessage::CommandAccepted { result, .. } => result,
        other => panic!("the claim was refused: {other:?}"),
    }
}

async fn challenge(client: &mut Client, id: &str) -> (PasskeyChallenge, Option<PasskeyCreation>) {
    let ServerMessage::CommandAccepted {
        result:
            CommandResult::PasskeyChallenge {
                challenge,
                registration,
            },
        ..
    } = client.command(id, CommandBody::PasskeyChallenge).await
    else {
        panic!("expected a passkey challenge");
    };
    (challenge, registration)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_claim_makes_the_person_owner_of_a_host_and_its_vault_and_their_sessions_theirs() {
    // Today's fleet: a host and a vault, each with its own owner user for this device, and on
    // the vault a machine paired as a user, which replicates.
    let host = Daemon::start_unclaimed().await;
    let vault = Daemon::start_unclaimed().await;
    let mut on_host = host.client().await;
    let host_user = on_host.hello(Vec::new()).await.user_id;
    let mut on_vault = vault.client().await;
    let vault_user = on_vault.hello(Vec::new()).await.user_id;
    let machine = DeviceKey::generate().unwrap();
    let code = vault
        .auth
        .mint("trash-can-01", None, PAIRING_TTL)
        .unwrap()
        .code;
    let machine = vault
        .client_on(&machine, Some(&code))
        .await
        .hello(Vec::new())
        .await;
    vault.replicated.lock().unwrap().push(machine.device_id);
    // And on the host, a user from before with a CLI device.
    let cli = DeviceKey::generate().unwrap();
    let code = host.auth.mint("tomas", None, PAIRING_TTL).unwrap().code;
    let leftover = host
        .client_on(&cli, Some(&code))
        .await
        .hello(Vec::new())
        .await
        .user_id;

    // A session on the host by the owner, which the vault holds as replicated, and one on the
    // vault's own owner.
    let session = host.create_session("s1");
    journal(&host, &session, &host_user);
    let copy = vault.create_session("s1");
    journal(&vault, &copy, &host_user);
    journal(&vault, &copy, &vault_user);

    // The app creates the passkey on the host and claims it.
    let (_, creation) = challenge(&mut on_host, "c1").await;
    let creation = creation.expect("an owner without a passkey can create one");
    let (passkey, registration) = Authenticator::register("tomas", &creation);
    let person = NewPerson {
        name: "Tomas".into(),
        passkey: PersonPasskey::Register { registration },
    };
    let CommandResult::Claimed {
        person_id,
        hosts,
        leftovers,
    } = claimed(&mut on_host, "c2", person, Vec::new()).await
    else {
        panic!("expected the claim");
    };
    assert!(hosts.is_empty());
    let left: Vec<_> = leftovers.iter().map(|user| &user.user_id).collect();
    assert_eq!(left, [&leftover], "the CLI's user needs a decision");
    // It's me, with a fresh assertion.
    let (fresh, _) = challenge(&mut on_host, "d1").await;
    let decide = CommandBody::DecideLeftover {
        user_id: leftover.clone(),
        decision: LeftoverDecision::ItsMe,
        assertion: Box::new(passkey.assert(&fresh)),
    };
    assert!(matches!(
        on_host.command("d2", decide).await,
        ServerMessage::CommandAccepted {
            result: CommandResult::Applied,
            ..
        }
    ));
    assert!(host.auth.leftovers().is_empty());
    assert_eq!(host.auth.resolve(&leftover).as_str(), person_id.as_str());
    let ServerMessage::CommandAccepted {
        result: CommandResult::People { people },
        ..
    } = on_host.command("c3", CommandBody::ListPeople).await
    else {
        panic!("expected the people");
    };

    // The same passkey claims the vault, which learns the person's id on the host too.
    let (challenge, _) = challenge(&mut on_vault, "c4").await;
    let person = NewPerson {
        name: "Tomas".into(),
        passkey: PersonPasskey::Existing {
            person_id: person_id.clone(),
            key: people[0].passkeys[0].clone(),
            assertion: passkey.assert(&challenge),
        },
    };
    let CommandResult::Claimed {
        person_id: on_the_vault,
        hosts,
        leftovers,
    } = claimed(&mut on_vault, "c5", person, vec![host_user.clone()]).await
    else {
        panic!("expected the claim");
    };
    assert_eq!(on_the_vault, person_id);
    assert_eq!(hosts, ["trash-can-01"], "the machine is a host now");
    assert!(leftovers.is_empty());

    // The person owns both, and their devices sign in with no code.
    let me = UserId::new(person_id.as_str());
    for daemon in [&host, &vault] {
        assert_eq!(daemon.auth.role(&me), Some(Role::Owner));
        let phone = DeviceKey::generate().unwrap();
        let signed_in = daemon
            .client_on(&phone, None)
            .await
            .sign_in(|challenge, _| SignIn::Passkey {
                assertion: passkey.assert(&challenge),
            })
            .await;
        assert_eq!(signed_in.user_id, me);
    }
    // Old sessions show the person as author, on the host and as the vault holds them.
    assert_eq!(authors(&host, &session).await, [None, Some(me.clone())]);
    assert_eq!(
        authors(&vault, &copy).await,
        [None, Some(me.clone()), Some(me)]
    );
}

/// An assertion by `passkey` over a challenge `client` just asked for.
async fn fresh(client: &mut Client, passkey: &Authenticator) -> Box<PasskeyAssertion> {
    let ServerMessage::CommandAccepted {
        result: CommandResult::PasskeyChallenge { challenge, .. },
        ..
    } = client
        .answer(
            &ulid::Ulid::generate().to_string(),
            CommandBody::PasskeyChallenge,
        )
        .await
    else {
        panic!("expected a passkey challenge");
    };
    Box::new(passkey.assert(&challenge))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_invited_member_connects_a_role_change_applies_and_a_revoke_disconnects() {
    let daemon = Daemon::start_fresh().await;
    let (code, _) = daemon.auth.open_claim().unwrap();
    let mut tomas_passkey = None;
    let mut owner = daemon
        .client_on(&DeviceKey::generate().unwrap(), None)
        .await;
    owner
        .sign_in(|_, registration| {
            let (passkey, registration) =
                Authenticator::register("tomas", &registration.expect("a claim registers"));
            tomas_passkey = Some(passkey);
            SignIn::ClaimOwnership {
                code: code.clone(),
                person: NewPerson {
                    name: "Tomas".into(),
                    passkey: PersonPasskey::Register { registration },
                },
            }
        })
        .await;
    let tomas = tomas_passkey.unwrap();

    // Tomas invites Sam as a member; Sam registers his own passkey through the invite.
    let invite = CommandBody::Invite {
        role: Role::Member,
        assertion: fresh(&mut owner, &tomas).await,
    };
    let ServerMessage::CommandAccepted {
        result: CommandResult::Invite { invite, role, .. },
        ..
    } = owner.command("invite", invite).await
    else {
        panic!("expected an invite");
    };
    assert_eq!(role, Role::Member);
    let laptop = DeviceKey::generate().unwrap();
    let mut sam = daemon.client_on(&laptop, None).await;
    let joined = sam
        .sign_in(|_, registration| {
            let (_, registration) =
                Authenticator::register("sam", &registration.expect("an invite registers"));
            SignIn::AcceptInvite {
                invite: invite.clone(),
                person: NewPerson {
                    name: "Sam".into(),
                    passkey: PersonPasskey::Register { registration },
                },
            }
        })
        .await;
    assert_eq!(joined.role, Role::Member);
    let sam_id = joined.person_id.clone().expect("Sam is a person");
    // Its invite used, the daemon takes no more new passkeys.
    let mut late = daemon
        .client_on(&DeviceKey::generate().unwrap(), None)
        .await;
    late.send(&ClientMessage::Hello(ClientHello {
        protocol_version: PROTOCOL_VERSION,
        client: "test".into(),
        resume: Vec::new(),
        pairing_code: None,
        sign_in: true,
        passkey: false,
    }))
    .await;
    assert!(
        matches!(
            late.recv().await,
            ServerMessage::SignInChallenge {
                registration: None,
                ..
            }
        ),
        "an invite works once"
    );

    // Making Sam an owner closes his connection; he comes back as an owner.
    let promote = CommandBody::SetPersonRole {
        person_id: sam_id.clone(),
        role: Role::Owner,
        person: None,
        assertion: fresh(&mut owner, &tomas).await,
    };
    assert!(matches!(
        owner.command("promote", promote).await,
        ServerMessage::CommandAccepted {
            result: CommandResult::Applied,
            ..
        }
    ));
    assert!(sam.try_recv().await.is_none(), "the role change closed it");
    let mut sam = daemon.client_on(&laptop, None).await;
    let back = sam.hello(Vec::new()).await;
    assert_eq!(
        (back.role, back.device_id),
        (Role::Owner, joined.device_id.clone())
    );

    // Removing Sam disconnects him at once, and his device is refused from then on.
    let remove = CommandBody::RevokePerson {
        person_id: sam_id,
        assertion: fresh(&mut owner, &tomas).await,
    };
    assert!(matches!(
        owner.command("remove", remove).await,
        ServerMessage::CommandAccepted {
            result: CommandResult::Applied,
            ..
        }
    ));
    assert!(sam.try_recv().await.is_none(), "the revoke closed it");
    let again = daemon.client_on(&laptop, None).await;
    assert_eq!(refused(again).await.code, ErrorCode::Forbidden);
}

/// A fresh daemon claimed by Tomas from his Mac: its connection, and his passkey.
async fn owned_by_tomas(daemon: &Daemon) -> (Client, ServerHello, Authenticator) {
    owned_from(daemon, &DeviceKey::generate().unwrap()).await
}

/// A fresh daemon claimed by Tomas from `mac`: its connection, and his passkey.
async fn owned_from(daemon: &Daemon, mac: &DeviceKey) -> (Client, ServerHello, Authenticator) {
    let (code, _) = daemon.auth.open_claim().unwrap();
    let mut client = daemon.client_on(mac, None).await;
    let mut passkey = None;
    let hello = client
        .sign_in(|_, registration| {
            let (made, registration) = Authenticator::register("tomas", &registration.unwrap());
            passkey = Some(made);
            SignIn::ClaimOwnership {
                code,
                person: NewPerson {
                    name: "Tomas".into(),
                    passkey: PersonPasskey::Register { registration },
                },
            }
        })
        .await;
    (client, hello, passkey.unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_person_opens_a_terminal_and_changes_secrets_only_with_a_fresh_assertion() {
    let daemon = Daemon::start_fresh().await;
    let session = daemon.create_session("s1");
    let (mut mac, tomas, passkey) = owned_by_tomas(&daemon).await;
    let open = |assertion| CommandBody::OpenTerminal {
        session_id: session.clone(),
        cols: 80,
        rows: 24,
        assertion,
    };
    // The Mac signs assertions: without one it is told to send one.
    let refusals = [
        ("bare", None, ErrorCode::AssertionRequired),
        (
            "stale",
            Some(Box::new(stale_assertion())),
            ErrorCode::Forbidden,
        ),
    ];
    for (id, assertion, code) in refusals {
        let ServerMessage::CommandRejected { error, .. } = mac.answer(id, open(assertion)).await
        else {
            panic!("expected the terminal refused");
        };
        assert_eq!(error.code, code, "{}", error.message);
    }
    let assertion = Some(fresh(&mut mac, &passkey).await);
    let opened = mac.answer("fresh", open(assertion.clone())).await;
    let ServerMessage::CommandAccepted {
        result: CommandResult::TerminalOpened { .. },
        ..
    } = opened
    else {
        panic!("expected the terminal opened, got {opened:?}");
    };
    // An assertion works once.
    let ServerMessage::CommandRejected { error, .. } = mac.answer("again", open(assertion)).await
    else {
        panic!("expected a replayed assertion refused");
    };
    assert_eq!(error.code, ErrorCode::Forbidden);

    // Every change to secrets is the same; this daemon is no vault, so a fresh one gets as far
    // as that.
    let create = |assertion| CommandBody::CreateSecretSet {
        set: "prod".into(),
        assertion,
    };
    let ServerMessage::CommandRejected { error, .. } = mac.answer("set", create(None)).await else {
        panic!("expected the secret change refused");
    };
    assert_eq!(error.code, ErrorCode::AssertionRequired);
    let assertion = Some(fresh(&mut mac, &passkey).await);
    let ServerMessage::CommandRejected { error, .. } = mac.answer("set2", create(assertion)).await
    else {
        panic!("expected the secret change to reach the vault check");
    };
    assert_eq!(error.code, ErrorCode::Unsupported, "{}", error.message);

    // An agent acting for a person has no passkey, so it never gets that far.
    let refused = daemon
        .control
        .command(session, tomas.user_id, create(None))
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::Forbidden);
    assert!(refused.message.contains("Face ID"), "{}", refused.message);
}

/// Reads `client`'s messages until the one `pick` takes.
async fn until<T>(client: &mut Client, pick: impl Fn(ServerMessage) -> Option<T>) -> T {
    loop {
        if let Some(found) = pick(client.recv().await) {
            return found;
        }
    }
}

/// The answer to `command_id`, skipping everything else.
async fn answer_to(client: &mut Client, command_id: &str) -> ServerMessage {
    let id = CommandId::new(command_id);
    until(client, |message| match &message {
        ServerMessage::CommandAccepted { command_id, .. }
        | ServerMessage::CommandRejected { command_id, .. }
            if *command_id == id =>
        {
            Some(message)
        }
        _ => None,
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_reattaches_its_own_terminal_without_an_assertion_but_no_other_device_does() {
    let daemon = Daemon::start_fresh().await;
    let session = daemon.create_session("s1");
    let mac_key = DeviceKey::generate().unwrap();
    let (mut mac, _, passkey) = owned_from(&daemon, &mac_key).await;
    let open = CommandBody::OpenTerminal {
        session_id: session,
        cols: 80,
        rows: 24,
        assertion: Some(fresh(&mut mac, &passkey).await),
    };
    let ServerMessage::CommandAccepted {
        result: CommandResult::TerminalOpened { terminal_id },
        ..
    } = mac.answer("open", open).await
    else {
        panic!("expected the terminal opened");
    };
    let attach = |assertion| CommandBody::AttachTerminal {
        terminal_id: terminal_id.clone(),
        assertion,
    };

    // The Mac reconnects, and re-attaches with no Face ID.
    drop(mac);
    let mut again = daemon.client_on(&mac_key, None).await;
    again.hello(Vec::new()).await;
    let reattached = again.answer("reattach", attach(None)).await;
    assert!(
        matches!(reattached, ServerMessage::CommandAccepted { .. }),
        "{reattached:?}"
    );

    // His iPhone never had it: it needs one.
    let mut phone = daemon
        .client_on(&DeviceKey::generate().unwrap(), None)
        .await;
    phone
        .sign_in(|challenge, _| SignIn::Passkey {
            assertion: passkey.assert(&challenge),
        })
        .await;
    let ServerMessage::CommandRejected { error, .. } = phone.answer("bare", attach(None)).await
    else {
        panic!("expected the phone's attach refused");
    };
    assert_eq!(error.code, ErrorCode::AssertionRequired);
    let signed = attach(Some(fresh(&mut phone, &passkey).await));
    let attached = phone.answer("signed", signed).await;
    assert!(
        matches!(attached, ServerMessage::CommandAccepted { .. }),
        "{attached:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_terminal_without_a_passkey_asks_the_phone_and_waits_for_it() {
    let daemon = Daemon::start_fresh().await;
    let session = daemon.create_session("s1");
    let (mut mac, _, passkey) = owned_by_tomas(&daemon).await;

    // A terminal signs in with `herder login`, approved from the Mac.
    let mut tui = daemon
        .client_on(&DeviceKey::generate().unwrap(), None)
        .await;
    tui.send(&ClientMessage::Hello(ClientHello {
        protocol_version: PROTOCOL_VERSION,
        client: "herder-tui/0.17.0".into(),
        resume: Vec::new(),
        pairing_code: None,
        sign_in: true,
        passkey: false,
    }))
    .await;
    assert!(matches!(
        tui.recv().await,
        ServerMessage::SignInChallenge { .. }
    ));
    tui.send(&ClientMessage::SignIn(SignIn::StartLogin)).await;
    let ServerMessage::LoginRequest { code, .. } = tui.recv().await else {
        panic!("expected a login request");
    };
    let approve = CommandBody::ApproveLogin {
        code,
        assertion: fresh(&mut mac, &passkey).await,
    };
    assert!(matches!(
        mac.answer("login", approve).await,
        ServerMessage::CommandAccepted { .. }
    ));
    let ServerMessage::Hello(hello) = tui.recv().await else {
        panic!("expected a hello");
    };
    tui.lists(&hello).await;

    // It opens a terminal: the Mac is asked, approves with Face ID, and the terminal opens.
    tui.send(&ClientMessage::Command(Command {
        id: CommandId::new("open"),
        body: CommandBody::OpenTerminal {
            session_id: session,
            cols: 80,
            rows: 24,
            assertion: None,
        },
    }))
    .await;
    let request = until(&mut mac, |message| match message {
        ServerMessage::ApprovalRequest(request) => Some(request),
        _ => None,
    })
    .await;
    assert_eq!(
        (request.action.as_str(), request.client.as_str()),
        ("Open a terminal", "herder-tui/0.17.0")
    );
    let approve = CommandBody::ApproveAction {
        request_id: request.request_id.clone(),
        assertion: fresh(&mut mac, &passkey).await,
    };
    assert!(matches!(
        mac.answer("approve", approve).await,
        ServerMessage::CommandAccepted { .. }
    ));
    let opened = answer_to(&mut tui, "open").await;
    assert!(
        matches!(
            opened,
            ServerMessage::CommandAccepted {
                result: CommandResult::TerminalOpened { .. },
                ..
            }
        ),
        "{opened:?}"
    );
    // An approval works once.
    let again = CommandBody::RefuseAction {
        request_id: request.request_id,
    };
    let ServerMessage::CommandRejected { error, .. } = mac.answer("again", again).await else {
        panic!("expected the closed request refused");
    };
    assert_eq!(error.code, ErrorCode::NotFound);

    // Refused on the Mac, the secret change is refused on the terminal.
    tui.send(&ClientMessage::Command(Command {
        id: CommandId::new("set"),
        body: CommandBody::CreateSecretSet {
            set: "prod".into(),
            assertion: None,
        },
    }))
    .await;
    let request = until(&mut mac, |message| match message {
        ServerMessage::ApprovalRequest(request) => Some(request),
        _ => None,
    })
    .await;
    assert_eq!(request.action, "Create the secret set prod");
    let refuse = CommandBody::RefuseAction {
        request_id: request.request_id,
    };
    assert!(matches!(
        mac.answer("refuse", refuse).await,
        ServerMessage::CommandAccepted { .. }
    ));
    let ServerMessage::CommandRejected { error, .. } = answer_to(&mut tui, "set").await else {
        panic!("expected the refused change rejected");
    };
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert!(error.message.contains("refused"), "{}", error.message);

    // With no app of his connected, nobody can approve: refused at once.
    drop(mac);
    tokio::time::sleep(Duration::from_millis(200)).await;
    tui.send(&ClientMessage::Command(Command {
        id: CommandId::new("alone"),
        body: CommandBody::OpenTerminal {
            session_id: SessionId::new("s1"),
            cols: 80,
            rows: 24,
            assertion: None,
        },
    }))
    .await;
    let ServerMessage::CommandRejected { error, .. } = answer_to(&mut tui, "alone").await else {
        panic!("expected the lone terminal refused");
    };
    assert!(error.message.contains("herder app"), "{}", error.message);
}
