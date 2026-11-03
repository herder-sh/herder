//! The CLIProxyAPI next to a vault: one pool of provider logins for the whole fleet, which
//! hosts reach through endpoint accounts.
//!
//! A vault whose `[vault]` table names `cliproxyapi`, the proxy's base URL on the vault's own
//! machine, keeps one client key per host there. When a host first replicates, the vault mints
//! a random key for it and keeps it as the secret [`KEY_SECRET`] in a set of its own,
//! `cliproxyapi-<host id>` ([`set_name`]), granted to that host for its own use: the host
//! fetches it with `FetchHostSecrets` when a session starts on an endpoint account naming it,
//! so no key is ever typed or kept on the host. Forgetting the host drops the set and the key.
//!
//! The vault owns the proxy's `access.api-keys`: every [`SYNC_EVERY`], when a host first
//! replicates, and when one is forgotten, it reads the list through the proxy's management API
//! and puts the hosts' keys there if the list differs, so a restarted proxy, or a key added by
//! hand, is set right. A proxy with no client key at all serves anyone, so with no host the
//! list holds one random key nobody has.
//!
//! The management API is CLIProxyAPI's `/v8/management`, with the secret in the proxy's
//! `management.secret-key`, which the vault reads from [`MANAGEMENT_KEY_ENV`]. The proxy takes
//! management requests from its own machine only (`management.allow-remote: false`); its
//! `MANAGEMENT_PASSWORD` variable would take them from anywhere, so it is never set.

use std::collections::BTreeSet;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use herder_protocol::{HostId, SecretValue, Timestamp, is_valid_secret_set_name};
use ring::rand::{SecureRandom, SystemRandom};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::blocking;
use super::secrets::SecretsKey;
use super::store::VaultStore;

/// The secret each host's client key is kept as, in its own set.
pub const KEY_SECRET: &str = "CLIPROXYAPI_KEY";

/// The environment variable the vault reads the proxy's management secret from.
pub const MANAGEMENT_KEY_ENV: &str = "HERDER_CLIPROXYAPI_MANAGEMENT_KEY";

/// How often the vault checks the proxy's client keys.
pub const SYNC_EVERY: Duration = Duration::from_secs(30);

/// What starts the name of a host's set; the host id follows.
const SET_PREFIX: &str = "cliproxyapi-";

/// How long one management request may take.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Most bytes of a management answer read.
const MAX_ANSWER_BYTES: u64 = 4 * 1024 * 1024;

/// The set `host`'s client key is kept in; `None` for a host id no set can be named after.
pub fn set_name(host: &HostId) -> Option<String> {
    let name = format!("{SET_PREFIX}{}", host.as_str().to_ascii_lowercase());
    is_valid_secret_set_name(&name).then_some(name)
}

/// A new random client key.
fn mint() -> Result<SecretValue> {
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("the system random number generator failed"))?;
    let key = format!("herder-{}", URL_SAFE_NO_PAD.encode(bytes));
    SecretValue::new(key).context("a minted key is a valid secret")
}

/// A CLIProxyAPI's management API.
#[derive(Clone)]
pub struct Management {
    /// `<base URL>/v8/management`.
    base: String,
    key: SecretValue,
    agent: ureq::Agent,
}

impl Management {
    /// The management API of the proxy at `url`, with secret `key`.
    pub fn new(url: &str, key: SecretValue) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            base: format!("{}/v8/management", url.trim_end_matches('/')),
            key,
            agent,
        }
    }

    /// The management API of the proxy at `url`, with the secret in [`MANAGEMENT_KEY_ENV`].
    pub fn from_env(url: &str) -> Result<Self> {
        let key = std::env::var(MANAGEMENT_KEY_ENV)
            .ok()
            .and_then(SecretValue::new)
            .with_context(|| {
                format!("{MANAGEMENT_KEY_ENV} must hold the CLIProxyAPI's management secret")
            })?;
        Ok(Self::new(url, key))
    }

    /// The proxy's client keys, `access.api-keys`.
    pub fn client_keys(&self) -> Result<Vec<String>> {
        match self.call("GET", "/config/access/api-keys", None)? {
            // The proxy answers `not_found` for a list it holds no entry for.
            None => Ok(Vec::new()),
            Some(keys) => serde_json::from_value(keys).context("the client keys are not a list"),
        }
    }

    /// Replaces the proxy's client keys with `keys`.
    pub fn set_client_keys(&self, keys: &[String]) -> Result<()> {
        let body = serde_json::to_string(keys)?;
        self.call("PUT", "/config/access/api-keys", Some(body))?
            .context("the proxy has no client keys to set")?;
        Ok(())
    }

    /// How many provider credentials the proxy holds.
    pub fn credentials(&self) -> Result<usize> {
        let answer = self
            .call("GET", "/credentials", None)?
            .context("the proxy lists no credentials")?;
        let files = answer["files"]
            .as_array()
            .context("the proxy's credentials have no `files` list")?;
        Ok(files.len())
    }

    /// Sends `method` to `path` with `body`; the JSON answer, `None` for a 404.
    fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<String>,
    ) -> Result<Option<serde_json::Value>> {
        let url = format!("{}{path}", self.base);
        let bearer = format!("Bearer {}", self.key.expose());
        let response = match (method, body) {
            ("GET", _) => self.agent.get(&url).header("Authorization", &bearer).call(),
            (_, body) => self
                .agent
                .put(&url)
                .header("Authorization", &bearer)
                .header("Content-Type", "application/json")
                .send(body.unwrap_or_default()),
        };
        let mut response = response.with_context(|| format!("cannot reach {url}"))?;
        let status = response.status().as_u16();
        let mut text = String::new();
        response
            .body_mut()
            .as_reader()
            .take(MAX_ANSWER_BYTES)
            .read_to_string(&mut text)
            .with_context(|| format!("cannot read {url}"))?;
        match status {
            200..=299 => serde_json::from_str(&text)
                .map(Some)
                .with_context(|| format!("{url} answered with something not JSON")),
            404 => Ok(None),
            401 | 403 => bail!(
                "{url} refused the management secret ({status}); set {MANAGEMENT_KEY_ENV} to the \
                 proxy's management.secret-key, and reach it from its own machine"
            ),
            _ => bail!("{url} answered {status}: {}", text.trim()),
        }
    }
}

/// Keeps one client key per host on the vault and in its CLIProxyAPI.
pub struct ClientKeys {
    management: Management,
    /// The one key the proxy holds when no host has one, so it never serves anyone without a
    /// key; random, and never handed out.
    placeholder: String,
    wake: Notify,
    /// Serializes syncs, so two never put lists over each other.
    syncing: tokio::sync::Mutex<()>,
}

impl ClientKeys {
    /// Keys kept in the proxy `management` answers for.
    pub fn new(management: Management) -> Result<Self> {
        Ok(Self {
            management,
            placeholder: mint()?.expose().to_owned(),
            wake: Notify::new(),
            syncing: tokio::sync::Mutex::new(()),
        })
    }

    /// Gives `host` its client key if it has none yet: mints one and keeps it in its set,
    /// granted to it for its own use. Whether one was minted.
    pub async fn ensure_host(
        &self,
        store: &Arc<Mutex<VaultStore>>,
        key: &SecretsKey,
        host: &HostId,
    ) -> Result<bool> {
        let Some(set) = set_name(host) else {
            warn!(host_id = %host, "no CLIProxyAPI key set can be named after this host id");
            return Ok(false);
        };
        let sealed = key.seal(&set, KEY_SECRET, &mint()?)?;
        let host = host.clone();
        let description = "This host's client key for the vault's CLIProxyAPI, minted by the \
                           vault; forgetting the host drops it";
        let minted = blocking(store, move |store| {
            let now = Timestamp::now();
            store.create_secret_set(&set, now)?;
            let minted = store.sealed_secret(&set, KEY_SECRET)?.is_none();
            if minted {
                store.put_secret(&set, KEY_SECRET, description, &sealed, now)?;
            }
            store.grant_secret_set(&set, &host, true)?;
            Ok(minted)
        })
        .await?;
        if minted {
            self.wake.notify_one();
        }
        Ok(minted)
    }

    /// Puts the client keys of the hosts the vault holds in the proxy, in place of whatever
    /// it holds, when that differs; drops the sets of hosts forgotten first. Whether the list
    /// changed.
    pub async fn sync(&self, store: &Arc<Mutex<VaultStore>>, key: &SecretsKey) -> Result<bool> {
        let _syncing = self.syncing.lock().await;
        let held = blocking(store, |store| {
            let hosts: BTreeSet<String> = store
                .hosts()?
                .into_iter()
                .filter_map(|host| set_name(&host.host_id))
                .collect();
            for set in store.secret_sets()? {
                if set.name.starts_with(SET_PREFIX) && !hosts.contains(&set.name) {
                    store.delete_secret(&set.name, KEY_SECRET)?;
                    store.delete_secret_set(&set.name)?;
                    info!(
                        set = set.name,
                        "CLIProxyAPI key of a forgotten host dropped"
                    );
                }
            }
            let mut held = Vec::new();
            for set in hosts {
                if let Some(sealed) = store.sealed_secret(&set, KEY_SECRET)? {
                    held.push((set, sealed));
                }
            }
            Ok(held)
        })
        .await?;
        let mut wanted: Vec<String> = held
            .iter()
            .filter_map(|(set, sealed)| {
                let value = key.open(set, KEY_SECRET, sealed);
                if value.is_none() {
                    warn!(set, "cannot open a host's CLIProxyAPI key");
                }
                value.map(|value| value.expose().to_owned())
            })
            .collect();
        if wanted.is_empty() {
            wanted.push(self.placeholder.clone());
        }
        wanted.sort();
        let management = self.management.clone();
        tokio::task::spawn_blocking(move || {
            let mut keys = management.client_keys()?;
            keys.sort();
            if keys == wanted {
                return Ok(false);
            }
            management.set_client_keys(&wanted)?;
            info!(keys = wanted.len(), "CLIProxyAPI client keys updated");
            Ok(true)
        })
        .await
        .context("the CLIProxyAPI sync panicked")?
    }

    /// Syncs now and every [`SYNC_EVERY`], and when a host gets a key, until `shutdown`.
    pub async fn run(
        self: Arc<Self>,
        store: Arc<Mutex<VaultStore>>,
        key: Arc<SecretsKey>,
        every: Duration,
        shutdown: CancellationToken,
    ) {
        loop {
            if let Err(err) = self.sync(&store, &key).await {
                warn!("cannot sync the CLIProxyAPI's client keys: {err:#}");
            }
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(every) => {}
                () = self.wake.notified() => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::Arc;

    use herder_protocol::DeviceId;

    use super::*;

    const SECRET: &str = "management-secret";

    /// A CLIProxyAPI's management API, on localhost, holding client keys only.
    struct FakeProxy {
        url: String,
        keys: Arc<Mutex<Vec<String>>>,
    }

    impl FakeProxy {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let keys = Arc::new(Mutex::new(Vec::<String>::new()));
            let held = Arc::clone(&keys);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let mut stream = stream.unwrap();
                    let (head, body) = read_request(&mut stream);
                    let path = "/v8/management/config/access/api-keys";
                    let (status, answer) = if !head.contains(&format!("Bearer {SECRET}")) {
                        ("401 Unauthorized", "{}".to_owned())
                    } else if head.starts_with(&format!("GET {path} ")) {
                        let keys = held.lock().unwrap();
                        ("200 OK", serde_json::to_string(&*keys).unwrap())
                    } else if head.starts_with(&format!("PUT {path} ")) {
                        *held.lock().unwrap() = serde_json::from_str(&body).unwrap();
                        ("200 OK", r#"{"status":"ok"}"#.to_owned())
                    } else {
                        ("404 Not Found", r#"{"error":"not_found"}"#.to_owned())
                    };
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    );
                }
            });
            Self { url, keys }
        }

        fn keys(&self) -> Vec<String> {
            self.keys.lock().unwrap().clone()
        }

        fn client_keys(&self) -> ClientKeys {
            let secret = SecretValue::new(SECRET).unwrap();
            ClientKeys::new(Management::new(&self.url, secret)).unwrap()
        }
    }

    /// A request's head and body, read off `stream`.
    fn read_request(stream: &mut std::net::TcpStream) -> (String, String) {
        let mut reader = std::io::BufReader::new(stream);
        let mut head = String::new();
        let mut length = 0;
        loop {
            let mut line = String::new();
            std::io::BufRead::read_line(&mut reader, &mut line).unwrap();
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
            if line == "\r\n" || line.is_empty() {
                break;
            }
            head.push_str(&line);
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        (head, String::from_utf8(body).unwrap())
    }

    fn vault() -> (tempfile::TempDir, Arc<Mutex<VaultStore>>, SecretsKey) {
        let dir = tempfile::tempdir().unwrap();
        let store = VaultStore::open(dir.path().join("vault.db")).unwrap();
        (dir, Arc::new(Mutex::new(store)), SecretsKey::new(&[7; 32]))
    }

    /// `host` replicating to `store`, as its first hello records it.
    fn replicates(store: &Arc<Mutex<VaultStore>>, host: &HostId) {
        let device = DeviceId::new(format!("device-{host}"));
        let mut store = store.lock().unwrap();
        assert!(store.bind(&device, host, "box", None, &[]).unwrap());
    }

    /// `host`'s client key, as the vault keeps it.
    fn kept(store: &Arc<Mutex<VaultStore>>, key: &SecretsKey, host: &HostId) -> Option<String> {
        let set = set_name(host).unwrap();
        let sealed = store
            .lock()
            .unwrap()
            .sealed_secret(&set, KEY_SECRET)
            .unwrap()?;
        Some(
            key.open(&set, KEY_SECRET, &sealed)
                .unwrap()
                .expose()
                .to_owned(),
        )
    }

    #[tokio::test]
    async fn a_host_gets_a_key_granted_for_its_own_use_in_the_proxy_until_it_is_forgotten() {
        let proxy = FakeProxy::start();
        let keys = proxy.client_keys();
        let (_dir, store, key) = vault();

        // No host yet: the proxy still holds a key, as one with none would serve anyone.
        assert!(keys.sync(&store, &key).await.unwrap());
        assert_eq!(proxy.keys(), vec![keys.placeholder.clone()]);

        let host = HostId::new("6f1c2d3e-4b5a-4c6d-8e9f-0a1b2c3d4e5f");
        replicates(&store, &host);
        assert!(keys.ensure_host(&store, &key, &host).await.unwrap());
        // Once: a host that connects again keeps its key.
        assert!(!keys.ensure_host(&store, &key, &host).await.unwrap());
        let minted = kept(&store, &key, &host).unwrap();
        assert!(
            minted.starts_with("herder-") && minted.len() > 40,
            "{minted}"
        );
        let set = set_name(&host).unwrap();
        let sets = store.lock().unwrap().secret_sets().unwrap();
        let granted = sets.iter().find(|s| s.name == set).unwrap();
        // For the host's own use, never its sessions'.
        assert_eq!(granted.host_use, vec![host.clone()]);
        assert!(granted.hosts.is_empty());

        assert!(keys.sync(&store, &key).await.unwrap());
        assert_eq!(proxy.keys(), vec![minted.clone()]);
        // In step: nothing to put.
        assert!(!keys.sync(&store, &key).await.unwrap());
        // A proxy that lost its keys, or got one by hand, is set right.
        proxy.keys.lock().unwrap().push("typed-by-hand".into());
        assert!(keys.sync(&store, &key).await.unwrap());
        assert_eq!(proxy.keys(), vec![minted.clone()]);

        // Forgotten: its set and its key go, from the vault and the proxy.
        store.lock().unwrap().forget(&host).unwrap();
        assert!(keys.sync(&store, &key).await.unwrap());
        assert_eq!(kept(&store, &key, &host), None);
        let sets = store.lock().unwrap().secret_sets().unwrap();
        assert!(sets.iter().all(|s| s.name != set));
        assert_eq!(proxy.keys(), vec![keys.placeholder.clone()]);
    }

    #[tokio::test]
    async fn a_proxy_that_refuses_the_management_secret_is_an_error() {
        let proxy = FakeProxy::start();
        let wrong = ClientKeys::new(Management::new(
            &proxy.url,
            SecretValue::new("wrong").unwrap(),
        ))
        .unwrap();
        let (_dir, store, key) = vault();
        let err = wrong.sync(&store, &key).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("refused the management secret"),
            "{err:#}"
        );
        assert!(proxy.keys().is_empty());
    }
}
