//! The hub's relay Worker: a Cloudflare Worker and Durable Object in its owner's own
//! account (`deploy/relay-worker/`), through which devices reach a hub that has no public
//! address, as through a tunnel (`docs/relay-worker.md`).
//!
//! The hub deploys it with an API token its owner gives once, keeps that token and what it
//! deployed in `<data_dir>/relay-worker.json` (this user only), and redeploys it when its
//! bundled source changes. It never sends either to a client.
//!
//! While deployed, the hub keeps one WebSocket open to the Worker, authenticated with the
//! hub's key, and tells it the certificate fingerprints of every device paired with it,
//! whenever they change: the Worker lets in only those. For each device that connects the
//! Worker says `open`; the hub opens a second WebSocket for that stream and serves its
//! frames as it serves its tunnel listener's: TLS with the device certificate, the upgrade,
//! the hello. The Worker forwards ciphertext only.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::{Mutex as AsyncMutex, watch};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::cloudflare::{self, Deployment};
use super::{Shared, client, conn};
use crate::auth::control::RelayWorkerRequest;
use crate::ws::tunnel::Frames;

/// The file in the data dir that keeps the deployment.
const FILE: &str = "relay-worker.json";

/// How often the hub checks whether its paired devices changed.
const DEVICES_EVERY: Duration = Duration::from_secs(5);

/// How often the hub pings the Worker, and the silence after which it takes the connection
/// for gone. Cloudflare answers pings without waking the Durable Object.
const PING_EVERY: Duration = Duration::from_secs(30);
const SILENCE: Duration = Duration::from_secs(90);

/// Time to open a WebSocket to the Worker.
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);

/// Bounds of the wait before connecting to the Worker again.
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// The hub's relay Worker: where it is deployed, if it is, and the API that deploys it.
pub(super) struct RelayWorker {
    /// The hub's data dir, which keeps [`FILE`].
    dir: PathBuf,
    api: String,
    deployment: watch::Sender<Option<Deployment>>,
    /// Deploys and removals, one at a time.
    changing: AsyncMutex<()>,
}

impl RelayWorker {
    /// The relay Worker of the hub with data dir `data_dir`, deployed through the Cloudflare
    /// API at `api`.
    pub(super) fn open(data_dir: &Path, api: &str) -> Result<Self> {
        let path = data_dir.join(FILE);
        let deployment = match std::fs::read_to_string(&path) {
            Ok(text) => Some(
                serde_json::from_str(&text)
                    .with_context(|| format!("reading {}", path.display()))?,
            ),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self {
            dir: data_dir.to_owned(),
            api: api.to_owned(),
            deployment: watch::Sender::new(deployment),
            changing: AsyncMutex::new(()),
        })
    }

    /// The Worker's URL, if it is deployed.
    pub(super) fn url(&self) -> Option<String> {
        self.deployment.borrow().as_ref().map(|d| d.url.clone())
    }

    /// Deploys the Worker with `token`, or the token it was deployed with, to `account` or
    /// the token's one account; its URL.
    pub(super) async fn deploy(
        &self,
        token: Option<String>,
        account: Option<String>,
    ) -> Result<String> {
        let _changing = self.changing.lock().await;
        let current = self.deployment.borrow().clone();
        let token = match (token, &current) {
            (Some(token), _) => token,
            (None, Some(current)) => current.api_token.clone(),
            (None, None) => bail!("give a Cloudflare API token to deploy the relay Worker"),
        };
        let api = self.api.clone();
        let deployed = tokio::task::spawn_blocking(move || {
            cloudflare::deploy(&api, &token, account.as_deref(), current.as_ref())
        })
        .await
        .context("the deploy task panicked")??;
        self.save(Some(&deployed))?;
        let url = deployed.url.clone();
        self.deployment.send_replace(Some(deployed));
        Ok(url)
    }

    /// Removes the Worker from its account and forgets it; whether one was deployed.
    pub(super) async fn remove(&self) -> Result<bool> {
        let _changing = self.changing.lock().await;
        let Some(current) = self.deployment.borrow().clone() else {
            return Ok(false);
        };
        let api = self.api.clone();
        tokio::task::spawn_blocking(move || cloudflare::remove(&api, &current))
            .await
            .context("the remove task panicked")??;
        self.save(None)?;
        self.deployment.send_replace(None);
        Ok(true)
    }

    /// Deploys the bundled Worker again if the deployed one is older.
    pub(super) async fn update(&self) {
        let stale = self
            .deployment
            .borrow()
            .as_ref()
            .is_some_and(|d| d.worker != cloudflare::worker_hash());
        if !stale {
            return;
        }
        match self.deploy(None, None).await {
            Ok(url) => info!(%url, "relay Worker updated"),
            Err(err) => warn!("cannot update the relay Worker: {err:#}"),
        }
    }

    fn save(&self, deployment: Option<&Deployment>) -> Result<()> {
        let Some(deployment) = deployment else {
            return match std::fs::remove_file(self.dir.join(FILE)) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err.into()),
                _ => Ok(()),
            };
        };
        crate::data_dir::write_private(&self.dir, FILE, &serde_json::to_vec_pretty(deployment)?)
    }

    /// Uses `deployment` as deployed, without asking Cloudflare: for a test against a local
    /// Worker runtime.
    #[cfg(test)]
    pub(super) fn set(&self, deployment: Deployment) {
        self.deployment.send_replace(Some(deployment));
    }
}

/// What the Worker tells the hub on its connection.
#[derive(Deserialize)]
struct Open {
    /// A device's stream, waiting for the hub's end.
    open: String,
    /// The device's address, as Cloudflare names it.
    client: Option<String>,
}

/// Keeps the hub connected to its relay Worker while it is deployed, until `cancel`.
pub(super) async fn run(shared: Arc<Shared>, worker: Arc<RelayWorker>, cancel: CancellationToken) {
    worker.update().await;
    let mut deployments = worker.deployment.subscribe();
    let mut backoff = MIN_BACKOFF;
    loop {
        let current = deployments.borrow_and_update().clone();
        let Some(deployment) = current else {
            tokio::select! {
                () = cancel.cancelled() => return,
                changed = deployments.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    continue;
                }
            }
        };
        let started = Instant::now();
        let result = tokio::select! {
            () = cancel.cancelled() => return,
            _ = deployments.changed() => {
                backoff = MIN_BACKOFF;
                continue;
            }
            result = serve(&shared, &deployment, &cancel) => result,
        };
        if let Err(err) = result {
            warn!(url = %deployment.url, "relay Worker connection failed: {err:#}");
        }
        if started.elapsed() > MAX_BACKOFF {
            backoff = MIN_BACKOFF;
        }
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(backoff) => {}
            _ = deployments.changed() => {}
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// The hub's connection to the Worker: tells it the paired devices, and opens the hub's end
/// of each stream it announces, until it closes or falls silent.
async fn serve(
    shared: &Arc<Shared>,
    deployment: &Deployment,
    cancel: &CancellationToken,
) -> Result<()> {
    let (mut control, _) = dial(deployment, "/hub").await?;
    info!(url = %deployment.url, "connected to the relay Worker");
    let mut told: Option<Vec<String>> = None;
    let mut check = tokio::time::interval(DEVICES_EVERY);
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.reset();
    let mut heard = Instant::now();
    loop {
        tokio::select! {
            _ = check.tick() => {
                let devices = paired(shared);
                if told.as_ref() != Some(&devices) {
                    let text = serde_json::json!({ "devices": devices }).to_string();
                    control.send(Message::text(text)).await?;
                    told = Some(devices);
                }
            }
            _ = ping.tick() => {
                if heard.elapsed() > SILENCE {
                    bail!("the relay Worker fell silent");
                }
                control.send(Message::Ping(Default::default())).await?;
            }
            message = control.next() => {
                let message = message.context("the relay Worker closed the connection")??;
                heard = Instant::now();
                match message {
                    Message::Text(text) => match serde_json::from_str::<Open>(&text) {
                        Ok(open) => {
                            tokio::spawn(accept(
                                Arc::clone(shared),
                                deployment.clone(),
                                open,
                                cancel.child_token(),
                            ));
                        }
                        Err(err) => debug!("ignored a message from the relay Worker: {err}"),
                    },
                    Message::Close(_) => bail!("the relay Worker closed the connection"),
                    _ => {}
                }
            }
        }
    }
}

/// The certificate fingerprints of every device paired with the hub, sorted.
fn paired(shared: &Shared) -> Vec<String> {
    let mut devices: Vec<String> = shared
        .auth
        .devices()
        .into_iter()
        .map(|(device, _)| device.fingerprint)
        .collect();
    devices.sort();
    devices.dedup();
    devices
}

/// Opens the hub's end of the stream `open` announced and serves the device on it.
async fn accept(
    shared: Arc<Shared>,
    deployment: Deployment,
    open: Open,
    cancel: CancellationToken,
) {
    if open.open.len() != 32 || !open.open.bytes().all(|b| b.is_ascii_hexdigit()) {
        return debug!("ignored a relay stream with a malformed id");
    }
    let Some(pass) = shared.gates.tunnel.enter(open.client.as_deref()) else {
        return warn!(
            client = open.client.as_deref().unwrap_or("unknown"),
            "refused a relayed connection: too many are not signed in yet"
        );
    };
    let (ws, peer) = match dial(&deployment, &format!("/accept/{}", open.open)).await {
        Ok(opened) => opened,
        Err(err) => return info!("cannot open a relay stream: {err:#}"),
    };
    conn::run_relayed(Box::new(Frames::new(ws)), pass, peer, shared, cancel).await;
}

/// A WebSocket to `path` on the Worker, as the hub.
async fn dial(deployment: &Deployment, path: &str) -> Result<(client::Outer, SocketAddr)> {
    let url = format!(
        "{}{path}",
        deployment
            .url
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1)
    );
    let bearer = format!("Bearer {}", deployment.hub_key);
    tokio::time::timeout(
        DIAL_TIMEOUT,
        client::outer(&url, &[("authorization".to_owned(), bearer)]),
    )
    .await
    .map_err(|_| anyhow!("the relay Worker did not answer in {DIAL_TIMEOUT:?}"))?
}

impl super::Admin {
    /// Does what `request` asks of the hub's relay Worker; its URL after, while deployed.
    pub async fn relay_worker(&self, request: RelayWorkerRequest) -> Result<Option<String>> {
        let worker = self
            .shared
            .relay_worker
            .get()
            .context("this hub looks after no relay Worker")?;
        match request {
            RelayWorkerRequest::Deploy { token, account } => {
                let url = worker.deploy(token, account).await?;
                info!(%url, "relay Worker deployed");
                Ok(Some(url))
            }
            RelayWorkerRequest::Remove => {
                if worker.remove().await? {
                    info!("relay Worker removed");
                }
                Ok(None)
            }
            RelayWorkerRequest::Status => Ok(worker.url()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};

    use herder_client_core::auth::{DeviceKey, client_config};
    use herder_protocol::{HostHello, HostId, HostMessage, HubMessage, REPLICATION_VERSION};
    use ring::signature::Ed25519KeyPair;
    use rustls::pki_types::ServerName;
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
    use tokio_rustls::TlsConnector;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    use super::*;
    use crate::auth::{Auth, DeviceRole, PAIRING_TTL};
    use crate::config::Retention;
    use crate::hub::{HubStore, LIVENESS_TIMEOUT, Server};
    use crate::ws::{Host, Tls};

    const TIMEOUT: Duration = Duration::from_secs(30);

    /// The headers that let the device with key `pem` (its certificate and Ed25519 key, as
    /// [`herder_client_core::auth::DeviceKey::to_pem`] saves them) through the relay Worker at
    /// `host` (`name[:port]`, as the upgrade's `Host` names it), at unix time `now`.
    fn device_headers(pem: &str, host: &str, now: u64) -> Result<Vec<(String, String)>> {
        use base64::Engine as _;
        let cert =
            CertificateDer::from_pem_slice(pem.as_bytes()).context("the device certificate")?;
        let key = PrivatePkcs8KeyDer::from_pem_slice(pem.as_bytes()).context("the device key")?;
        let key = Ed25519KeyPair::from_pkcs8_maybe_unchecked(key.secret_pkcs8_der())
            .map_err(|err| anyhow!("the device key is not Ed25519: {err}"))?;
        let signature = key.sign(format!("herder-relay\n{host}\n{now}").as_bytes());
        let b64 = base64::engine::general_purpose::STANDARD;
        Ok(vec![
            ("x-herder-device".to_owned(), b64.encode(cert.as_ref())),
            ("x-herder-time".to_owned(), now.to_string()),
            (
                "x-herder-signature".to_owned(),
                b64.encode(signature.as_ref()),
            ),
        ])
    }

    /// The relay Worker in a local workerd (miniflare), with `hub_key`; killed on drop.
    pub(crate) struct LocalWorker {
        child: Child,
        /// Where it listens, `http://127.0.0.1:<port>`.
        pub(crate) url: String,
    }

    impl LocalWorker {
        /// Starts it, or `None` where `npm ci` did not run in `deploy/relay-worker`; in CI
        /// that fails the test instead.
        pub(crate) fn start(hub_key: &str) -> Option<Self> {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/relay-worker");
            // workerd loads modules from below its working directory only.
            let dir = dir.canonicalize().unwrap();
            if !dir.join("node_modules/miniflare").exists() {
                assert!(
                    std::env::var_os("CI").is_none(),
                    "run `npm ci` in deploy/relay-worker: CI runs the relay Worker's tests"
                );
                eprintln!("skipped: no miniflare; run `npm ci` in deploy/relay-worker");
                return None;
            }
            let mut child = Command::new("node")
                .current_dir(&dir)
                .arg("serve.mjs")
                .arg(hub_key)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .expect("node runs");
            let mut line = String::new();
            BufReader::new(child.stdout.take().unwrap())
                .read_line(&mut line)
                .unwrap();
            let url = line.trim().trim_end_matches('/').to_owned();
            assert!(url.starts_with("http://"), "miniflare said {line:?}");
            Some(Self { child, url })
        }
    }

    impl Drop for LocalWorker {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// Opens a connection to the hub through the Worker at `url` as `device`, signed as
    /// `signer`, and says a host's hello: the hub's answer.
    async fn hello_through(
        url: &str,
        hub_fingerprint: &str,
        device: &DeviceKey,
        signer: &DeviceKey,
    ) -> Result<HubMessage> {
        let host = url.trim_start_matches("http://");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        let headers = device_headers(signer.to_pem(), host, now)?;
        let (ws, _) = client::outer(&url.replacen("http://", "ws://", 1), &headers).await?;
        let config = client_config(hub_fingerprint, device)?;
        let tls = TlsConnector::from(Arc::new(config))
            .connect(ServerName::try_from("herder")?, Frames::new(ws))
            .await?;
        let request = "wss://herder/".into_client_request()?;
        let (mut inner, _) = tokio_tungstenite::client_async(request, tls).await?;
        let hello = HostMessage::Hello(HostHello {
            replication_version: REPLICATION_VERSION,
            host_id: HostId::new("host-1"),
            host_name: "devbox".into(),
            build: "test".into(),
            pairing_code: None,
            attachments_cap: None,
            relay: false,
        });
        inner
            .send(Message::text(serde_json::to_string(&hello)?))
            .await?;
        loop {
            match inner.next().await.context("the hub closed")?? {
                Message::Text(text) => return Ok(serde_json::from_str(&text)?),
                Message::Close(_) => bail!("the hub closed"),
                _ => {}
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_paired_device_reaches_the_hub_through_the_worker_and_no_one_else_does() {
        let hub_key = "e2e-hub-key";
        let Some(worker) = LocalWorker::start(hub_key) else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("tls")).unwrap();
        let tls = Tls::load_or_create(&dir.path().join("tls"), "hub").unwrap();
        let fingerprint = tls.fingerprint().to_owned();
        let auth = Arc::new(Auth::open(dir.path()).unwrap());
        let server = Server::new(
            tls,
            Arc::clone(&auth),
            HubStore::open(dir.path().join("hub.db")).unwrap(),
            Host {
                id: HostId::new("hub"),
                name: "hub".into(),
            },
            LIVENESS_TIMEOUT,
            Retention::default(),
            None,
        );
        // Deployed already, at the local Worker: the hub asks Cloudflare nothing.
        server
            .use_relay_worker(dir.path(), "http://127.0.0.1:9")
            .unwrap();
        server.shared.relay_worker.get().unwrap().set(Deployment {
            api_token: "unused".into(),
            account_id: "acc1".into(),
            url: worker.url.clone(),
            hub_key: hub_key.into(),
            worker: cloudflare::worker_hash(),
        });
        // Paired as a host, as `herder pair --host` and a first hello would.
        let device = DeviceKey::generate().unwrap();
        let code = auth.mint_host("devbox", PAIRING_TTL).unwrap().code;
        let cancel = CancellationToken::new();
        let identity = auth
            .authenticate(
                &device.fingerprint(),
                Some(&code),
                "test",
                DeviceRole::Host,
                &cancel,
            )
            .unwrap();
        let shutdown = CancellationToken::new();
        tokio::spawn(server.run(Vec::new(), shutdown.clone()));

        // The hub connects and tells the Worker its devices within a few seconds.
        let deadline = Instant::now() + TIMEOUT;
        let answer = loop {
            match hello_through(&worker.url, &fingerprint, &device, &device).await {
                Ok(answer) => break answer,
                Err(err) if Instant::now() < deadline => {
                    debug!("not yet: {err:#}");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Err(err) => panic!("never reached the hub through the Worker: {err:#}"),
            }
        };
        assert!(matches!(answer, HubMessage::Hello(_)), "{answer:?}");

        // A device the hub has not paired is refused by the Worker, before the hub.
        let stranger = DeviceKey::generate().unwrap();
        let refused = hello_through(&worker.url, &fingerprint, &stranger, &stranger)
            .await
            .unwrap_err();
        assert!(format!("{refused:#}").contains("401"), "{refused:#}");
        // Nor may it borrow a paired device's certificate.
        let borrowed = hello_through(&worker.url, &fingerprint, &device, &stranger).await;
        assert!(borrowed.is_err());

        // Unpaired, the device is refused within the time the hub takes to tell the Worker.
        auth.revoke(&identity.device_id).unwrap();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match hello_through(&worker.url, &fingerprint, &device, &device).await {
                Err(err) if format!("{err:#}").contains("401") => break,
                _ if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                other => panic!("a revoked device still got through: {other:?}"),
            }
        }
        shutdown.cancel();
    }

    #[tokio::test]
    async fn the_hub_keeps_what_it_deployed_to_itself_updates_it_and_forgets_it_once_removed() {
        use std::os::unix::fs::PermissionsExt as _;

        use super::super::cloudflare::tests::{FakeApi, TOKEN};

        let api = FakeApi::one_account();
        let dir = tempfile::tempdir().unwrap();
        let worker = RelayWorker::open(dir.path(), &api.url).unwrap();
        assert_eq!(worker.url(), None);
        let refused = worker.deploy(None, None).await.unwrap_err();
        assert!(format!("{refused:#}").contains("API token"), "{refused:#}");

        let url = worker.deploy(Some(TOKEN.into()), None).await.unwrap();
        assert_eq!(url, "https://herder-relay.ada.workers.dev");
        let file = dir.path().join(FILE);
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the token is the hub user's only");
        let kept: Deployment = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(kept.api_token, TOKEN);

        // A hub started again finds it, and deploys a newer bundled Worker with the kept token.
        let stale = Deployment {
            worker: "older".into(),
            ..kept.clone()
        };
        std::fs::write(&file, serde_json::to_vec(&stale).unwrap()).unwrap();
        let worker = RelayWorker::open(dir.path(), &api.url).unwrap();
        assert_eq!(worker.url().as_deref(), Some(url.as_str()));
        let uploads = || api.calls().iter().filter(|(m, _)| m == "PUT").count();
        let before = uploads();
        worker.update().await;
        assert_eq!(uploads(), before + 1);
        let updated: Deployment = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(updated, kept, "the same relay and key, the current Worker");
        // Current already: nothing to do.
        worker.update().await;
        assert_eq!(uploads(), before + 1);

        assert!(worker.remove().await.unwrap());
        assert_eq!(worker.url(), None);
        assert!(!file.exists());
        assert!(!worker.remove().await.unwrap());
    }
}
