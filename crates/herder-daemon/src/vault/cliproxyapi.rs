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
//!
//! The vault's owners manage the proxy's provider logins from their apps through the same API
//! ([`command`]): `/credentials` lists and deletes them, and `/oauth/auth-url` starts a login,
//! which waits five minutes for the address the browser was sent to, posted to
//! `/oauth/callback`; `/oauth/status` then says when the proxy holds the login. The tokens
//! stay in the proxy: the answers read here carry none.

use std::collections::BTreeSet;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use herder_protocol::{
    CommandBody, CommandResult, ErrorCode, ErrorInfo, HostId, Provider, ProviderLogin,
    ProviderLoginState, SecretValue, Timestamp, is_valid_secret_set_name,
};
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

/// How long `complete_provider_login` waits for the proxy to trade the code for a login.
const COMPLETE_WAIT: Duration = Duration::from_secs(60);

/// How often the proxy is asked whether a login it is completing is done.
const COMPLETE_POLL: Duration = Duration::from_millis(500);

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

    /// The provider logins the proxy holds, ordered by provider, then label.
    pub fn logins(&self) -> Result<Vec<ProviderLogin>> {
        let answer = self
            .call("GET", "/credentials", None)?
            .context("the proxy lists no credentials")?;
        let files = answer["files"]
            .as_array()
            .context("the proxy's credentials have no `files` list")?;
        let now = Timestamp::now();
        let mut logins: Vec<ProviderLogin> = files.iter().filter_map(|f| login(f, now)).collect();
        logins.sort_by(|a, b| {
            (a.provider.as_str(), &a.label, &a.name).cmp(&(b.provider.as_str(), &b.label, &b.name))
        });
        Ok(logins)
    }

    /// Sends `method` to `path` with `body`; the JSON answer, `None` for a 404.
    fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<String>,
    ) -> Result<Option<serde_json::Value>> {
        let (url, status, answer, text) = self.request(method, path, body)?;
        match status {
            200..=299 => answer
                .map(Some)
                .with_context(|| format!("{url} answered with something not JSON")),
            404 => Ok(None),
            _ => Err(refusal(&url, status, &text)),
        }
    }

    /// Sends `method` to `path` with `body`: the URL, the status, the answer as JSON if it is
    /// JSON, and its text. Fails only when the proxy cannot be reached or refuses the secret.
    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<String>,
    ) -> Result<(String, u16, Option<serde_json::Value>, String)> {
        let url = format!("{}{path}", self.base);
        let bearer = format!("Bearer {}", self.key.expose());
        let response = match method {
            "GET" => self.agent.get(&url).header("Authorization", &bearer).call(),
            "DELETE" => self
                .agent
                .delete(&url)
                .header("Authorization", &bearer)
                .call(),
            "POST" => self
                .agent
                .post(&url)
                .header("Authorization", &bearer)
                .header("Content-Type", "application/json")
                .send(body.unwrap_or_default()),
            _ => self
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
        if matches!(status, 401 | 403) {
            bail!(
                "{url} refused the management secret ({status}); set {MANAGEMENT_KEY_ENV} to the \
                 proxy's management.secret-key, and reach it from its own machine"
            );
        }
        let answer = serde_json::from_str(&text).ok();
        Ok((url, status, answer, text))
    }
}

/// The error for an answer of `status` the vault did not expect.
fn refusal(url: &str, status: u16, text: &str) -> anyhow::Error {
    anyhow::anyhow!("{url} answered {status}: {}", text.trim())
}

/// A provider login, from one of the proxy's credential entries; `None` for one with no name.
fn login(entry: &serde_json::Value, now: Timestamp) -> Option<ProviderLogin> {
    let text = |key: &str| {
        entry[key]
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let yes = |key: &str| entry[key].as_bool() == Some(true);
    let name = text("name")?;
    let provider = Provider::from(text("provider").or_else(|| text("type"))?);
    let until = text("next_retry_after")
        .and_then(|at| at.parse::<Timestamp>().ok())
        .filter(|at| *at > now);
    let state = if yes("disabled") || text("status").as_deref() == Some("disabled") {
        ProviderLoginState::Disabled
    } else if yes("requires_sign_in") {
        ProviderLoginState::NeedsSignIn
    } else if (yes("unavailable") || yes("quota_blocked")) && until.is_some() {
        ProviderLoginState::CoolingDown
    } else if yes("unavailable") || text("status").as_deref() == Some("error") {
        ProviderLoginState::Error
    } else {
        ProviderLoginState::Active
    };
    Some(ProviderLogin {
        name,
        provider,
        label: text("email").or_else(|| text("label")),
        until: until.filter(|_| state == ProviderLoginState::CoolingDown),
        message: text("status_message"),
        state,
    })
}

/// Answers an owner's command on the proxy's provider logins, through `management`; blocks.
pub fn command(management: &Management, command: CommandBody) -> Result<CommandResult, ErrorInfo> {
    let internal = |err: anyhow::Error| failed(ErrorCode::Internal, format!("{err:#}"));
    match command {
        CommandBody::ListProviderLogins => Ok(CommandResult::ProviderLogins {
            logins: management.logins().map_err(internal)?,
        }),
        CommandBody::StartProviderLogin { provider } => {
            if !matches!(provider, Provider::Claude | Provider::Codex) {
                return Err(failed(
                    ErrorCode::BadRequest,
                    format!(
                        "the vault's CLIProxyAPI signs in to Claude and Codex here, not {}",
                        provider.as_str()
                    ),
                ));
            }
            let path = format!("/oauth/auth-url?provider={}", provider.as_str());
            let answer = management
                .call("GET", &path, None)
                .map_err(internal)?
                .ok_or_else(|| internal(anyhow::anyhow!("the proxy cannot start logins")))?;
            match (answer["state"].as_str(), answer["url"].as_str()) {
                (Some(state), Some(url)) => Ok(CommandResult::ProviderLoginStarted {
                    login_id: state.to_owned(),
                    url: url.to_owned(),
                }),
                _ => Err(internal(anyhow::anyhow!(
                    "the proxy started a login with no URL: {answer}"
                ))),
            }
        }
        CommandBody::CompleteProviderLogin {
            login_id,
            redirect_url,
        } => complete(management, &login_id, &redirect_url),
        CommandBody::CancelProviderLogin { login_id } => {
            let path = format!("/oauth/session?state={}", query(&login_id));
            let answer = management
                .call("DELETE", &path, None)
                .map_err(internal)?
                .unwrap_or_default();
            if answer["cancelled"].as_bool() != Some(true) {
                return Err(not_waiting());
            }
            Ok(CommandResult::Applied)
        }
        CommandBody::RemoveProviderLogin { name } => {
            let path = format!("/credentials?name={}", query(&name));
            match management.call("DELETE", &path, None).map_err(internal)? {
                Some(_) => Ok(CommandResult::Applied),
                None => Err(failed(
                    ErrorCode::NotFound,
                    format!("the vault's CLIProxyAPI holds no login {name}"),
                )),
            }
        }
        _ => Err(internal(anyhow::anyhow!("not a provider login command"))),
    }
}

/// Hands the proxy the address the browser was sent to, and waits until it holds the login.
fn complete(
    management: &Management,
    login_id: &str,
    redirect_url: &str,
) -> Result<CommandResult, ErrorInfo> {
    let internal = |err: anyhow::Error| failed(ErrorCode::Internal, format!("{err:#}"));
    let address = url::Url::parse(redirect_url.trim()).map_err(|_| {
        failed(
            ErrorCode::BadRequest,
            "paste the whole address the browser ended on, starting with http://localhost".into(),
        )
    })?;
    let has = |key: &str| {
        address
            .query_pairs()
            .any(|(k, v)| k == key && !v.is_empty())
    };
    if !has("code") && !has("error") {
        return Err(failed(
            ErrorCode::BadRequest,
            "that address has no code: paste the one the browser ended on once you signed in"
                .into(),
        ));
    }
    let body = serde_json::json!({ "state": login_id, "redirect_url": address.as_str() });
    let (url, status, answer, text) = management
        .request("POST", "/oauth/callback", Some(body.to_string()))
        .map_err(internal)?;
    match status {
        200..=299 => {}
        404 | 409 => return Err(not_waiting()),
        400 => {
            let why = answer
                .as_ref()
                .and_then(|a| a["error"].as_str())
                .unwrap_or("bad request")
                .to_owned();
            return Err(failed(
                ErrorCode::BadRequest,
                format!("the vault's CLIProxyAPI refused that address: {why}"),
            ));
        }
        _ => return Err(internal(refusal(&url, status, &text))),
    }
    let deadline = std::time::Instant::now() + COMPLETE_WAIT;
    let path = format!("/oauth/status?state={}", query(login_id));
    loop {
        let (_, _, answer, _) = management.request("GET", &path, None).map_err(internal)?;
        let answer = answer.unwrap_or_default();
        match answer["status"].as_str() {
            Some("ok") => {
                return Ok(CommandResult::ProviderLogins {
                    logins: management.logins().map_err(internal)?,
                });
            }
            Some("error") => {
                let why = answer["error"].as_str().unwrap_or("the login failed");
                if why == "unknown or expired state" {
                    return Err(not_waiting());
                }
                return Err(failed(
                    ErrorCode::Conflict,
                    format!("the provider did not sign this login in: {why}"),
                ));
            }
            _ if std::time::Instant::now() >= deadline => {
                return Err(failed(
                    ErrorCode::Internal,
                    "the vault's CLIProxyAPI took the code but has not finished the login; \
                     reload the list in a moment"
                        .into(),
                ));
            }
            _ => std::thread::sleep(COMPLETE_POLL),
        }
    }
}

/// `value`, escaped for a query string.
fn query(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn not_waiting() -> ErrorInfo {
    failed(
        ErrorCode::NotFound,
        "this login is no longer waiting: it was cancelled, finished, or took longer than five \
         minutes; start it again"
            .into(),
    )
}

fn failed(code: ErrorCode, message: String) -> ErrorInfo {
    ErrorInfo { code, message }
}

/// Keeps one client key per host on the vault and in its CLIProxyAPI.
pub struct ClientKeys {
    management: Management,
    /// Where hosts reach the proxy, told to each host that holds a key; `None` when the
    /// vault's config does not say.
    endpoint: Option<String>,
    /// The one key the proxy holds when no host has one, so it never serves anyone without a
    /// key; random, and never handed out.
    placeholder: String,
    wake: Notify,
    /// Serializes syncs, so two never put lists over each other.
    syncing: tokio::sync::Mutex<()>,
}

impl ClientKeys {
    /// Keys kept in the proxy `management` answers for, which hosts reach at `endpoint`.
    pub fn new(management: Management, endpoint: Option<String>) -> Result<Self> {
        Ok(Self {
            management,
            endpoint,
            placeholder: mint()?.expose().to_owned(),
            wake: Notify::new(),
            syncing: tokio::sync::Mutex::new(()),
        })
    }

    /// Where hosts reach the proxy, if the vault's config says.
    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }

    /// The proxy's management API.
    pub fn management(&self) -> &Management {
        &self.management
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

    /// A CLIProxyAPI's management API, on localhost: its client keys, its credentials, and
    /// the logins it waits on, by state. A callback with the code `good` signs in.
    struct FakeProxy {
        url: String,
        keys: Arc<Mutex<Vec<String>>>,
        credentials: Arc<Mutex<Vec<serde_json::Value>>>,
        logins: Arc<Mutex<std::collections::BTreeMap<String, serde_json::Value>>>,
    }

    /// `path`'s query parameter `key`.
    fn param(path: &str, key: &str) -> Option<String> {
        let url = url::Url::parse(&format!("http://fake{path}")).ok()?;
        url.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    }

    impl FakeProxy {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let keys = Arc::new(Mutex::new(Vec::<String>::new()));
            let credentials = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
            let logins = Arc::new(Mutex::new(std::collections::BTreeMap::new()));
            let held = Arc::clone(&keys);
            let creds = Arc::clone(&credentials);
            let waiting = Arc::clone(&logins);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let mut stream = stream.unwrap();
                    let (head, body) = read_request(&mut stream);
                    let mut words = head.split_whitespace();
                    let method = words.next().unwrap_or_default().to_owned();
                    let target = words.next().unwrap_or_default().to_owned();
                    let path = target
                        .strip_prefix("/v8/management")
                        .unwrap_or_default()
                        .to_owned();
                    let route = path.split('?').next().unwrap_or_default();
                    let ok = |answer: serde_json::Value| ("200 OK", answer.to_string());
                    let missing = |why: &str| {
                        let answer = serde_json::json!({ "status": "error", "error": why });
                        ("404 Not Found", answer.to_string())
                    };
                    let (status, answer) = if !head.contains(&format!("Bearer {SECRET}")) {
                        ("401 Unauthorized", "{}".to_owned())
                    } else {
                        match (method.as_str(), route) {
                            ("GET", "/config/access/api-keys") => {
                                ok(serde_json::json!(*held.lock().unwrap()))
                            }
                            ("PUT", "/config/access/api-keys") => {
                                *held.lock().unwrap() = serde_json::from_str(&body).unwrap();
                                ok(serde_json::json!({ "status": "ok" }))
                            }
                            ("GET", "/credentials") => {
                                ok(serde_json::json!({ "files": *creds.lock().unwrap() }))
                            }
                            ("DELETE", "/credentials") => {
                                let name = param(&path, "name").unwrap_or_default();
                                let mut creds = creds.lock().unwrap();
                                let before = creds.len();
                                creds.retain(|c| c["name"] != name.as_str());
                                if creds.len() < before {
                                    ok(serde_json::json!({ "status": "ok" }))
                                } else {
                                    missing("auth file not found")
                                }
                            }
                            ("GET", "/oauth/auth-url") => {
                                let provider = param(&path, "provider").unwrap_or_default();
                                let mut waiting = waiting.lock().unwrap();
                                let state = format!("state{}", waiting.len() + 1);
                                waiting.insert(
                                    state.clone(),
                                    serde_json::json!({ "status": "wait", "provider": provider }),
                                );
                                ok(serde_json::json!({
                                    "status": "ok",
                                    "state": state,
                                    "url": format!("https://{provider}.example/oauth?state={state}"),
                                }))
                            }
                            ("POST", "/oauth/callback") => {
                                let body: serde_json::Value = serde_json::from_str(&body).unwrap();
                                let state = body["state"].as_str().unwrap_or_default();
                                let redirect = body["redirect_url"].as_str().unwrap_or_default();
                                let code = url::Url::parse(redirect)
                                    .ok()
                                    .and_then(|u| param(&format!("/?{}", u.query()?), "code"));
                                let mut waiting = waiting.lock().unwrap();
                                match waiting.get_mut(state) {
                                    Some(login) if login["status"] == "wait" => {
                                        if code.as_deref() == Some("good") {
                                            let provider = login["provider"].clone();
                                            *login = serde_json::json!({ "status": "ok" });
                                            creds.lock().unwrap().push(serde_json::json!({
                                                "name": format!("{}-ada@example.com.json", provider.as_str().unwrap()),
                                                "provider": provider,
                                                "email": "ada@example.com",
                                                "status": "active",
                                                // Never relayed.
                                                "id_token": { "email": "ada@example.com" },
                                            }));
                                        } else {
                                            *login = serde_json::json!({
                                                "status": "error",
                                                "error": "Bad request",
                                            });
                                        }
                                        ok(serde_json::json!({ "status": "ok" }))
                                    }
                                    Some(_) => (
                                        "409 Conflict",
                                        r#"{"status":"error","error":"oauth flow is not pending"}"#
                                            .to_owned(),
                                    ),
                                    None => missing("unknown or expired state"),
                                }
                            }
                            ("GET", "/oauth/status") => {
                                let state = param(&path, "state").unwrap_or_default();
                                match waiting.lock().unwrap().get(&state) {
                                    Some(login) => ok(login.clone()),
                                    None => ok(serde_json::json!({
                                        "status": "error",
                                        "error": "unknown or expired state",
                                    })),
                                }
                            }
                            ("DELETE", "/oauth/session") => {
                                let state = param(&path, "state").unwrap_or_default();
                                let cancelled = waiting.lock().unwrap().remove(&state).is_some();
                                ok(serde_json::json!({ "status": "ok", "cancelled": cancelled }))
                            }
                            _ => ("404 Not Found", r#"{"error":"not_found"}"#.to_owned()),
                        }
                    };
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    );
                }
            });
            Self {
                url,
                keys,
                credentials,
                logins,
            }
        }

        fn management(&self) -> Management {
            Management::new(&self.url, SecretValue::new(SECRET).unwrap())
        }

        fn keys(&self) -> Vec<String> {
            self.keys.lock().unwrap().clone()
        }

        fn client_keys(&self) -> ClientKeys {
            let secret = SecretValue::new(SECRET).unwrap();
            ClientKeys::new(Management::new(&self.url, secret), None).unwrap()
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
        let wrong = ClientKeys::new(
            Management::new(&proxy.url, SecretValue::new("wrong").unwrap()),
            None,
        )
        .unwrap();
        let (_dir, store, key) = vault();
        let err = wrong.sync(&store, &key).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("refused the management secret"),
            "{err:#}"
        );
        assert!(proxy.keys().is_empty());
    }

    /// Runs `body` on the proxy's logins, as the vault does for an owner.
    async fn run(proxy: &FakeProxy, body: CommandBody) -> Result<CommandResult, ErrorInfo> {
        let management = proxy.management();
        tokio::task::spawn_blocking(move || command(&management, body))
            .await
            .unwrap()
    }

    fn listed(result: Result<CommandResult, ErrorInfo>) -> Vec<ProviderLogin> {
        match result {
            Ok(CommandResult::ProviderLogins { logins }) => logins,
            other => panic!("expected logins: {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_owner_adds_a_login_with_the_address_the_browser_was_sent_to_and_removes_it() {
        let proxy = FakeProxy::start();
        assert!(listed(run(&proxy, CommandBody::ListProviderLogins).await).is_empty());

        let Ok(CommandResult::ProviderLoginStarted { login_id, url }) = run(
            &proxy,
            CommandBody::StartProviderLogin {
                provider: Provider::Claude,
            },
        )
        .await
        else {
            panic!("the login did not start");
        };
        assert_eq!(
            url,
            format!("https://claude.example/oauth?state={login_id}")
        );

        // An address with no code is the wrong one, and the login keeps waiting.
        let wrong = run(
            &proxy,
            CommandBody::CompleteProviderLogin {
                login_id: login_id.clone(),
                redirect_url: "http://localhost:54545/callback".into(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(wrong.code, ErrorCode::BadRequest);
        let garbage = run(
            &proxy,
            CommandBody::CompleteProviderLogin {
                login_id: login_id.clone(),
                redirect_url: "not an address".into(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(garbage.code, ErrorCode::BadRequest);

        let redirect_url = format!("http://localhost:54545/callback?code=good&state={login_id}");
        let logins = listed(
            run(
                &proxy,
                CommandBody::CompleteProviderLogin {
                    login_id: login_id.clone(),
                    redirect_url: redirect_url.clone(),
                },
            )
            .await,
        );
        assert_eq!(
            logins,
            vec![ProviderLogin {
                name: "claude-ada@example.com.json".into(),
                provider: Provider::Claude,
                label: Some("ada@example.com".into()),
                state: ProviderLoginState::Active,
                until: None,
                message: None,
            }]
        );
        // Done: it no longer waits.
        let again = run(
            &proxy,
            CommandBody::CompleteProviderLogin {
                login_id,
                redirect_url,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(again.code, ErrorCode::NotFound);

        let name = logins[0].name.clone();
        let removed = run(
            &proxy,
            CommandBody::RemoveProviderLogin { name: name.clone() },
        )
        .await;
        assert!(matches!(removed, Ok(CommandResult::Applied)), "{removed:?}");
        assert!(proxy.credentials.lock().unwrap().is_empty());
        let gone = run(&proxy, CommandBody::RemoveProviderLogin { name })
            .await
            .unwrap_err();
        assert_eq!(gone.code, ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn a_login_the_provider_refuses_or_the_owner_cancels_is_not_kept() {
        let proxy = FakeProxy::start();
        let start = || CommandBody::StartProviderLogin {
            provider: Provider::Codex,
        };
        let Ok(CommandResult::ProviderLoginStarted { login_id, .. }) = run(&proxy, start()).await
        else {
            panic!("the login did not start");
        };
        let refused = run(
            &proxy,
            CommandBody::CompleteProviderLogin {
                login_id,
                redirect_url: "http://localhost:1455/auth/callback?error=access_denied".into(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(refused.code, ErrorCode::Conflict);
        assert!(refused.message.contains("Bad request"), "{refused:?}");

        let Ok(CommandResult::ProviderLoginStarted { login_id, .. }) = run(&proxy, start()).await
        else {
            panic!("the login did not start");
        };
        let cancel = || CommandBody::CancelProviderLogin {
            login_id: login_id.clone(),
        };
        assert!(matches!(
            run(&proxy, cancel()).await,
            Ok(CommandResult::Applied)
        ));
        assert!(!proxy.logins.lock().unwrap().contains_key(&login_id));
        let twice = run(&proxy, cancel()).await.unwrap_err();
        assert_eq!(twice.code, ErrorCode::NotFound);
        let late = run(
            &proxy,
            CommandBody::CompleteProviderLogin {
                login_id: login_id.clone(),
                redirect_url: format!(
                    "http://localhost:1455/auth/callback?code=good&state={login_id}"
                ),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(late.code, ErrorCode::NotFound);
        assert!(proxy.credentials.lock().unwrap().is_empty());

        // Only the providers the proxy signs in to from here.
        let gemini = run(
            &proxy,
            CommandBody::StartProviderLogin {
                provider: Provider::Gemini,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(gemini.code, ErrorCode::BadRequest);
    }

    #[test]
    fn a_login_says_whether_the_proxy_can_use_it() {
        let now: Timestamp = "2026-11-03T12:00:00Z".parse().unwrap();
        let state = |entry: serde_json::Value| login(&entry, now).unwrap();
        let cooling = state(serde_json::json!({
            "name": "codex-a.json", "provider": "codex", "label": "a", "status": "error",
            "unavailable": true, "next_retry_after": "2026-11-03T13:00:00Z",
            "status_message": "quota exhausted",
        }));
        assert_eq!(cooling.state, ProviderLoginState::CoolingDown);
        assert_eq!(cooling.until, Some("2026-11-03T13:00:00Z".parse().unwrap()));
        assert_eq!(cooling.label.as_deref(), Some("a"));
        assert_eq!(cooling.message.as_deref(), Some("quota exhausted"));
        // A cooldown that is over is not one.
        let over = state(serde_json::json!({
            "name": "codex-a.json", "provider": "codex", "status": "active",
            "next_retry_after": "2026-11-03T11:00:00Z",
        }));
        assert_eq!((over.state, over.until), (ProviderLoginState::Active, None));
        let signed_out = state(serde_json::json!({
            "name": "claude-b.json", "provider": "claude", "requires_sign_in": true,
            "status": "error", "status_message": "unauthorized (refresh token invalid)",
        }));
        assert_eq!(signed_out.state, ProviderLoginState::NeedsSignIn);
        let failing = state(serde_json::json!({
            "name": "claude-c.json", "type": "claude", "status": "error", "unavailable": true,
        }));
        assert_eq!(failing.state, ProviderLoginState::Error);
        assert_eq!(failing.provider, Provider::Claude);
        let off = state(serde_json::json!({
            "name": "claude-d.json", "provider": "claude", "disabled": true,
            "status": "disabled",
        }));
        assert_eq!(off.state, ProviderLoginState::Disabled);
        assert!(login(&serde_json::json!({ "provider": "claude" }), now).is_none());
    }
}
