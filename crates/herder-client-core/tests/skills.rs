//! Keeping the skill library in sync across machines: several fake daemons over TLS on
//! localhost that speak just enough of the protocol to hold a skill library each, all cloned
//! from one shared origin. The client sets the repository on every machine where its user is
//! owner, has the others pull after a write, catches up a machine that was offline, sets the
//! library on a machine paired later, and follows a repository set from another device.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use herder_client_core::{Client, ConnectionState, PairResult, PairingUri};
use herder_daemon::ws::Tls;
use herder_protocol::{
    ClientMessage, CommandBody, CommandResult, DeviceId, ErrorCode, ErrorInfo, HostId,
    PROTOCOL_VERSION, Role, ServerHello, ServerMessage, SkillsStatus, UserId,
};
use rustls::client::danger::HandshakeSignatureValid;
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, ServerConfig, SignatureScheme};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(20);

/// The library's repository, as set; the daemons report it without the token.
const URL: &str = "https://token@github.com/you/herder-skills.git";
const SHOWN: &str = "https://github.com/you/herder-skills.git";

/// The library's origin: its latest commit, which every write moves.
#[derive(Default)]
struct Origin {
    commits: Mutex<u32>,
}

impl Origin {
    fn head(&self) -> String {
        format!("c{}", self.commits.lock().unwrap())
    }

    fn commit(&self) -> String {
        *self.commits.lock().unwrap() += 1;
        self.head()
    }
}

/// What a fake daemon's library is at, and every skill command it was sent.
#[derive(Default)]
struct Library {
    repo: Option<String>,
    head: Option<String>,
    commands: Vec<CommandBody>,
}

/// A daemon that holds a skill library and answers its commands.
struct Fake {
    id: String,
    role: Role,
    addr: SocketAddr,
    tls: TlsAcceptor,
    fingerprint: String,
    origin: Arc<Origin>,
    library: Arc<Mutex<Library>>,
    /// Bumped whenever the library changes; each connection then sends its status.
    changed: Arc<watch::Sender<u64>>,
    online: Mutex<CancellationToken>,
}

impl Fake {
    /// A daemon for host `id` where the client's user is `role`, with `repo` cloned already;
    /// online.
    async fn start(
        dir: &Path,
        id: &str,
        role: Role,
        origin: &Arc<Origin>,
        repo: Option<&str>,
    ) -> Arc<Self> {
        let (tls, fingerprint) = acceptor(&dir.join(id));
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let fake = Arc::new(Self {
            id: id.into(),
            role,
            addr: listener.local_addr().unwrap(),
            tls,
            fingerprint,
            origin: Arc::clone(origin),
            library: Arc::new(Mutex::new(Library {
                repo: repo.map(str::to_owned),
                head: repo.map(|_| "elsewhere".into()),
                commands: Vec::new(),
            })),
            changed: Arc::new(watch::Sender::new(0)),
            online: Mutex::new(CancellationToken::new()),
        });
        fake.serve(listener);
        fake
    }

    fn link(&self) -> String {
        PairingUri {
            hosts: vec![self.addr.to_string()],
            fingerprint: self.fingerprint.clone(),
            code: "code".into(),
        }
        .to_string()
    }

    /// Drops every connection and stops listening.
    fn go_offline(&self) {
        self.online.lock().unwrap().cancel();
    }

    /// Listens again, on the same address.
    async fn go_online(self: &Arc<Self>) {
        *self.online.lock().unwrap() = CancellationToken::new();
        let listener = TcpListener::bind(self.addr).await.unwrap();
        self.serve(listener);
    }

    /// The repository set from another device, cloned fresh.
    fn set_repo_elsewhere(&self, repo: &str) {
        let mut library = self.library.lock().unwrap();
        library.repo = Some(repo.into());
        library.head = Some(self.origin.head());
        drop(library);
        self.changed.send_modify(|v| *v += 1);
    }

    fn head(&self) -> Option<String> {
        self.library.lock().unwrap().head.clone()
    }

    fn repo(&self) -> Option<String> {
        self.library.lock().unwrap().repo.clone()
    }

    fn commands(&self) -> Vec<CommandBody> {
        self.library.lock().unwrap().commands.clone()
    }

    fn count(&self, wanted: impl Fn(&CommandBody) -> bool) -> usize {
        self.commands().iter().filter(|body| wanted(body)).count()
    }

    fn status(&self) -> SkillsStatus {
        let library = self.library.lock().unwrap();
        SkillsStatus {
            repo: library.repo.as_deref().map(shown),
            head: library.head.clone(),
            last_pull: None,
            pull_error: None,
            skills: Vec::new(),
            reload: Vec::new(),
        }
    }

    fn serve(self: &Arc<Self>, listener: TcpListener) {
        let online = self.online.lock().unwrap().clone();
        let fake = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                let tcp = tokio::select! {
                    () = online.cancelled() => return,
                    accepted = listener.accept() => accepted.unwrap().0,
                };
                let (fake, online) = (Arc::clone(&fake), online.clone());
                tokio::spawn(async move {
                    tokio::select! {
                        () = online.cancelled() => {}
                        () = fake.connection(tcp) => {}
                    }
                });
            }
        });
    }

    async fn connection(&self, tcp: tokio::net::TcpStream) {
        let Ok(tls) = self.tls.accept(tcp).await else {
            return;
        };
        let Ok(mut ws) = tokio_tungstenite::accept_async(tls).await else {
            return;
        };
        let send = |message: &ServerMessage| Message::text(serde_json::to_string(message).unwrap());
        // The hello; any code pairs.
        let Some(Ok(Message::Text(_))) = ws.next().await else {
            return;
        };
        let hello = ServerMessage::Hello(ServerHello {
            protocol_version: PROTOCOL_VERSION,
            host_id: HostId::new(&self.id),
            host_name: self.id.clone(),
            user_id: UserId::new("alice"),
            device_id: DeviceId::new("device"),
            role: self.role,
        });
        let mut changed = self.changed.subscribe();
        changed.mark_changed();
        if ws.send(send(&hello)).await.is_err() {
            return;
        }
        loop {
            let frame = tokio::select! {
                _ = changed.changed() => {
                    let status = ServerMessage::SkillsStatus(self.status());
                    if ws.send(send(&status)).await.is_err() {
                        return;
                    }
                    continue;
                }
                frame = ws.next() => frame,
            };
            let text = match frame {
                Some(Ok(Message::Text(text))) => text,
                Some(Ok(_)) => continue,
                _ => return,
            };
            let reply = match serde_json::from_str(&text).unwrap() {
                ClientMessage::Command(command) => match self.apply(command.body) {
                    Ok(result) => ServerMessage::CommandAccepted {
                        command_id: command.id,
                        result,
                    },
                    Err(error) => ServerMessage::CommandRejected {
                        command_id: command.id,
                        error,
                    },
                },
                ClientMessage::Sync { token } => ServerMessage::Synced { token },
                _ => continue,
            };
            if ws.send(send(&reply)).await.is_err() {
                return;
            }
        }
    }

    /// A command, as P11.6's daemon applies it.
    fn apply(&self, body: CommandBody) -> Result<CommandResult, ErrorInfo> {
        let mut library = self.library.lock().unwrap();
        library.commands.push(body.clone());
        let refuse = |code, message: &str| ErrorInfo {
            code,
            message: message.into(),
        };
        if self.role != Role::Owner {
            return Err(refuse(ErrorCode::Forbidden, "owners only"));
        }
        match body {
            CommandBody::SetSkillsRepo { url } => {
                library.repo = Some(url);
                library.head = Some(self.origin.head());
            }
            CommandBody::PutSkill { .. }
            | CommandBody::DeleteSkill { .. }
            | CommandBody::ImportSkill { .. } => {
                if library.repo.is_none() {
                    return Err(refuse(ErrorCode::NotFound, "no library"));
                }
                library.head = Some(self.origin.commit());
            }
            CommandBody::PullSkills => {
                if library.repo.is_none() {
                    return Err(refuse(ErrorCode::NotFound, "no library"));
                }
                library.head = Some(self.origin.head());
            }
            _ => return Err(refuse(ErrorCode::Unsupported, "not a skill command")),
        }
        drop(library);
        self.changed.send_modify(|v| *v += 1);
        Ok(CommandResult::Applied)
    }
}

/// `url` as a daemon reports it: without the user info of an `https` URL.
fn shown(url: &str) -> String {
    match url
        .strip_prefix("https://")
        .and_then(|rest| rest.split_once('@'))
    {
        Some((_, rest)) => format!("https://{rest}"),
        None => url.to_owned(),
    }
}

/// A TLS acceptor with a certificate made in `dir`, taking any client certificate, and the
/// certificate's fingerprint.
fn acceptor(dir: &Path) -> (TlsAcceptor, String) {
    std::fs::create_dir_all(dir).unwrap();
    let fingerprint = Tls::load_or_create(dir, "fake")
        .unwrap()
        .fingerprint()
        .to_owned();
    let cert = CertificateDer::from_pem_file(dir.join("cert.pem")).unwrap();
    let key = PrivateKeyDer::from_pem_file(dir.join("key.pem")).unwrap();
    let provider = rustls::crypto::ring::default_provider();
    let devices = Arc::new(AnyDevice(provider.signature_verification_algorithms));
    let config = ServerConfig::builder_with_provider(Arc::new(provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_client_cert_verifier(devices)
        .with_single_cert(vec![cert], key)
        .unwrap();
    (TlsAcceptor::from(Arc::new(config)), fingerprint)
}

/// Takes any client certificate whose key signed the handshake.
#[derive(Debug)]
struct AnyDevice(WebPkiSupportedAlgorithms);

impl ClientCertVerifier for AnyDevice {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_schemes()
    }
}

async fn pair(client: &Client, fake: &Fake) {
    let results = client.pair(fake.link()).await.unwrap();
    assert!(
        matches!(results.as_slice(), [PairResult::Paired { .. }]),
        "{results:?}"
    );
}

/// Waits until `done` holds, checking whenever the client's machines change and at least
/// every 50 ms, as the fakes change on their own.
async fn wait(client: &Client, what: &str, done: impl Fn() -> bool) {
    let changes = client.changes();
    tokio::time::timeout(TIMEOUT, async {
        while !done() {
            let _ = tokio::time::timeout(Duration::from_millis(50), changes.next()).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what} did not happen in time"));
}

fn connected(client: &Client, fake: &Fake) -> bool {
    client.machines().iter().any(|machine| {
        machine.host_id.as_str() == fake.id
            && machine.connection == ConnectionState::Connected
            && machine.skills == Some(fake.status())
    })
}

fn is_set(body: &CommandBody) -> bool {
    matches!(body, CommandBody::SetSkillsRepo { .. })
}

fn is_pull(body: &CommandBody) -> bool {
    matches!(body, CommandBody::PullSkills)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_skill_library_stays_the_same_on_every_owned_machine() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = Arc::new(Origin::default());
    let a = Fake::start(tmp.path(), "a", Role::Owner, &origin, None).await;
    let b = Fake::start(tmp.path(), "b", Role::Owner, &origin, None).await;
    let c = Fake::start(tmp.path(), "c", Role::Owner, &origin, None).await;
    let member = Fake::start(tmp.path(), "member", Role::Member, &origin, None).await;
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "herder-test/0".into(),
    )
    .unwrap();
    for fake in [&a, &b, &c, &member] {
        pair(&client, fake).await;
        wait(&client, "connecting", || connected(&client, fake)).await;
    }

    // Setting the repository on one machine sets it on every other owned one.
    let set = CommandBody::SetSkillsRepo { url: URL.into() };
    client.send(HostId::new("a"), set).await.unwrap();
    for fake in [&b, &c] {
        wait(&client, "setting the repository", || {
            fake.repo().as_deref() == Some(URL)
        })
        .await;
        assert_eq!(fake.head(), Some(origin.head()));
        assert_eq!(fake.status().repo.as_deref(), Some(SHOWN));
    }

    // A write through one machine has every other one pull.
    let put = CommandBody::PutSkill {
        name: "deploy".into(),
        files: Vec::new(),
    };
    client.send(HostId::new("a"), put.clone()).await.unwrap();
    for fake in [&b, &c] {
        wait(&client, "a pull after a write", || {
            fake.head() == Some(origin.head())
        })
        .await;
    }
    assert_eq!(
        a.count(is_pull),
        0,
        "the machine written through pulls nothing"
    );

    // A machine that was offline for a write pulls once it is back.
    b.go_offline();
    wait(&client, "b going offline", || {
        client
            .machines()
            .iter()
            .any(|m| m.host_id.as_str() == "b" && m.connection != ConnectionState::Connected)
    })
    .await;
    let delete = CommandBody::DeleteSkill {
        name: "deploy".into(),
    };
    client.send(HostId::new("c"), delete).await.unwrap();
    wait(&client, "a pull on a", || a.head() == Some(origin.head())).await;
    assert_ne!(b.head(), Some(origin.head()));
    let pulls = b.count(is_pull);
    b.go_online().await;
    client.wake();
    wait(&client, "b catching up", || b.head() == Some(origin.head())).await;
    assert_eq!(b.count(is_pull), pulls + 1);

    // A machine paired later gets the library, whether it had none or another one.
    let fresh = Fake::start(tmp.path(), "fresh", Role::Owner, &origin, None).await;
    let other = Some("https://github.com/you/old-skills.git");
    let moved = Fake::start(tmp.path(), "moved", Role::Owner, &origin, other).await;
    for fake in [&fresh, &moved] {
        pair(&client, fake).await;
        wait(&client, "a new machine cloning the library", || {
            fake.repo().as_deref() == Some(URL) && fake.head() == Some(origin.head())
        })
        .await;
    }

    // A repository set from another device becomes the library everywhere.
    let elsewhere = "git@github.com:you/team-skills.git";
    a.set_repo_elsewhere(elsewhere);
    for fake in [&b, &c, &fresh, &moved] {
        wait(&client, "following a repository set elsewhere", || {
            fake.repo().as_deref() == Some(elsewhere)
        })
        .await;
    }

    // Each machine was set exactly as often as it needed; a member never hears of it.
    for fake in [&a, &b, &c, &fresh, &moved] {
        wait(&client, "settling", || connected(&client, fake)).await;
    }
    assert_eq!(a.count(is_set), 1);
    for fake in [&b, &c, &fresh, &moved] {
        assert_eq!(fake.count(is_set), 2, "{}: {:?}", fake.id, fake.commands());
    }
    assert!(member.commands().is_empty(), "{:?}", member.commands());
}
