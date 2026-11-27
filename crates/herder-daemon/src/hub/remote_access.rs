//! How the hub is reached from outside, as its owners set it with `set_remote_access`
//! (`docs/remote-access.md`): the settings, kept in `<data_dir>/remote-access.json`, and what
//! the hub runs for them. Today that is the managed Cloudflare Tunnel
//! ([`cloudflare::ManagedTunnel`]); herder's WireGuard and the Durable Object relay are
//! refused until they come.
//!
//! The settings name the Cloudflare API token by the hub secret holding it
//! (`CloudflareSettings::api_token`); the hub reads its value from its secrets, and it never
//! goes back to a client.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{Context, Result};
use herder_protocol::{
    CloudflareSettings, ErrorCode, ErrorInfo, RemoteAccess, RemoteAccessSettings, SecretRef,
    SecretValue, Timestamp,
};

use super::cloudflare::{self, ManagedTunnel, Wanted};

/// The hub secret set `herder hub tunnel` puts the Cloudflare API token in.
pub const TOKEN_SET: &str = "cloudflare";

/// The secret `herder hub tunnel` puts the Cloudflare API token as.
pub const TOKEN_NAME: &str = "CLOUDFLARE_API_TOKEN";
use super::{HubStore, SecretsKey};

/// The hub's remote access.
pub struct RemoteAccessHub {
    /// `<data_dir>/remote-access.json`.
    path: PathBuf,
    /// The settings in effect; locked across a change, so changes apply one at a time.
    settings: tokio::sync::Mutex<RemoteAccessSettings>,
    store: Arc<Mutex<HubStore>>,
    secrets_key: Option<Arc<SecretsKey>>,
    tunnel: Arc<ManagedTunnel>,
}

impl RemoteAccessHub {
    /// The remote access of the hub with `data_dir`, reading the API token from the secrets in
    /// `store` under `secrets_key`, and running `tunnel`.
    pub fn open(
        data_dir: &Path,
        store: Arc<Mutex<HubStore>>,
        secrets_key: Option<Arc<SecretsKey>>,
        tunnel: Arc<ManagedTunnel>,
    ) -> Result<Arc<Self>> {
        let path = data_dir.join("remote-access.json");
        let settings = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("reading {}", path.display()))?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                RemoteAccessSettings::default()
            }
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Arc::new(Self {
            path,
            settings: tokio::sync::Mutex::new(settings),
            store,
            secrets_key,
            tunnel,
        }))
    }

    /// Tells the tunnel what the settings want, as the hub starts.
    pub async fn start(&self) {
        let settings = self.settings.lock().await.clone();
        let wanted = self.wanted(settings.cloudflare.as_ref()).await;
        self.tunnel.want(wanted, None);
    }

    /// Answers `get_remote_access`: the settings for an `owner` only.
    pub async fn get(&self, owner: bool) -> RemoteAccess {
        let settings = self.settings.lock().await.clone();
        RemoteAccess {
            settings: owner.then_some(settings),
            wireguard: None,
            relay: None,
            tunnel: self.tunnel.status().await,
        }
    }

    /// Answers `set_remote_access`: keeps `new` and applies it, the tunnel in the background.
    pub async fn set(&self, mut new: RemoteAccessSettings) -> Result<RemoteAccess, ErrorInfo> {
        if new.wireguard.is_some() {
            return Err(error(
                ErrorCode::Unsupported,
                "this hub's version of herder has no WireGuard endpoint yet".into(),
            ));
        }
        if let Some(cloudflare) = &mut new.cloudflare {
            if cloudflare.relay {
                return Err(error(
                    ErrorCode::Unsupported,
                    "this hub's version of herder deploys no Durable Object relay yet".into(),
                ));
            }
            if cloudflare.account_id.is_empty()
                || !cloudflare
                    .account_id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric())
            {
                return Err(error(
                    ErrorCode::BadRequest,
                    format!(
                        "{:?} is not a Cloudflare account id: copy it from the account's \
                         overview in the dashboard",
                        cloudflare.account_id
                    ),
                ));
            }
            if let Some(hostname) = &cloudflare.tunnel_hostname {
                cloudflare.tunnel_hostname = Some(
                    cloudflare::hostname(hostname)
                        .map_err(|err| error(ErrorCode::BadRequest, format!("{err:#}")))?,
                );
            }
            // Checked now, so an owner hears of a token that is not there at once.
            self.token(&cloudflare.api_token)
                .await
                .map_err(|why| error(ErrorCode::BadRequest, why))?;
        }
        let mut settings = self.settings.lock().await;
        let removal_token = match &settings.cloudflare {
            Some(old) => self.token(&old.api_token).await.ok(),
            None => None,
        };
        let json = serde_json::to_vec_pretty(&new).map_err(|err| internal(&err.into()))?;
        write(&self.path, &json).map_err(|err| internal(&err))?;
        *settings = new.clone();
        drop(settings);
        let wanted = self.wanted(new.cloudflare.as_ref()).await;
        self.tunnel.want(wanted, removal_token);
        Ok(self.get(true).await)
    }

    /// Sets the managed tunnel to `hostname`, or takes it away with none, for `herder hub
    /// tunnel`: in `account_id`, or the account set before, with `api_token` put as the hub
    /// secret [`TOKEN_SET`]/[`TOKEN_NAME`], or the token named before. The settings' other
    /// parts stay.
    pub async fn set_tunnel(
        &self,
        account_id: Option<String>,
        hostname: Option<String>,
        api_token: Option<SecretValue>,
    ) -> Result<RemoteAccess, ErrorInfo> {
        let mut settings = self.settings.lock().await.clone();
        let old = settings.cloudflare.take();
        if hostname.is_none() && old.as_ref().is_none_or(|old| old.tunnel_hostname.is_none()) {
            return Err(error(
                ErrorCode::NotFound,
                "this hub has no managed tunnel".into(),
            ));
        }
        let api_token = match api_token {
            Some(token) => {
                self.put_token(token).await?;
                SecretRef {
                    set: Some(TOKEN_SET.to_owned()),
                    name: TOKEN_NAME.to_owned(),
                }
            }
            None => old
                .as_ref()
                .map(|old| old.api_token.clone())
                .ok_or_else(|| {
                    error(ErrorCode::BadRequest, "give a Cloudflare API token".into())
                })?,
        };
        let account_id = account_id
            .or_else(|| old.as_ref().map(|old| old.account_id.clone()))
            .ok_or_else(|| {
                error(
                    ErrorCode::BadRequest,
                    "give the Cloudflare account id".into(),
                )
            })?;
        settings.cloudflare = Some(CloudflareSettings {
            account_id,
            api_token,
            relay: old.as_ref().is_some_and(|old| old.relay),
            tunnel_hostname: hostname,
        });
        self.set(settings).await
    }

    /// Puts `token` as the hub secret [`TOKEN_SET`]/[`TOKEN_NAME`], making the set if need be.
    async fn put_token(&self, token: SecretValue) -> Result<(), ErrorInfo> {
        let Some(key) = self.secrets_key.clone() else {
            return Err(error(
                ErrorCode::Unsupported,
                format!(
                    "this hub keeps no secrets, so it cannot keep the API token: set {} (see \
                     deploy/README.md)",
                    super::SECRETS_KEY_ENV
                ),
            ));
        };
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || {
            let sealed = key.seal(TOKEN_SET, TOKEN_NAME, &token)?;
            let now = Timestamp::now();
            let mut store = store.lock().unwrap_or_else(PoisonError::into_inner);
            store.create_secret_set(TOKEN_SET, now)?;
            store.put_secret(
                TOKEN_SET,
                TOKEN_NAME,
                "Cloudflare API token for the hub's managed tunnel",
                &sealed,
                now,
            )?;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .map_err(|err| internal(&err.into()))?
        .map_err(|err| internal(&err))
    }

    /// The tunnel `cloudflare` wants, or why the hub cannot tell.
    async fn wanted(
        &self,
        cloudflare: Option<&CloudflareSettings>,
    ) -> Result<Option<Wanted>, String> {
        let Some(cloudflare) = cloudflare else {
            return Ok(None);
        };
        let Some(hostname) = &cloudflare.tunnel_hostname else {
            return Ok(None);
        };
        Ok(Some(Wanted {
            account_id: cloudflare.account_id.clone(),
            api_token: self.token(&cloudflare.api_token).await?,
            hostname: hostname.clone(),
        }))
    }

    /// The value of the hub secret `secret`.
    async fn token(&self, secret: &SecretRef) -> Result<SecretValue, String> {
        let Some(key) = self.secrets_key.clone() else {
            return Err(format!(
                "this hub keeps no secrets, so it cannot read {secret}: set {} (see \
                 deploy/README.md)",
                super::SECRETS_KEY_ENV
            ));
        };
        let store = Arc::clone(&self.store);
        let secret = secret.clone();
        tokio::task::spawn_blocking(move || {
            // Every write is one transaction, so a poisoned store is consistent.
            let store = store.lock().unwrap_or_else(PoisonError::into_inner);
            let set = match &secret.set {
                Some(set) => set.clone(),
                None => {
                    let sets: Vec<String> = store
                        .secrets()
                        .map_err(|err| format!("reading the hub's secrets: {err}"))?
                        .into_iter()
                        .filter(|info| info.name == secret.name)
                        .map(|info| info.set)
                        .collect();
                    match sets.as_slice() {
                        [set] => set.clone(),
                        [] => return Err(format!("the hub has no secret {secret}")),
                        _ => {
                            return Err(format!(
                                "more than one set on the hub has a secret {secret}; name its \
                                 set, as set/{secret}"
                            ));
                        }
                    }
                }
            };
            let sealed = store
                .sealed_secret(&set, &secret.name)
                .map_err(|err| format!("reading the hub's secrets: {err}"))?
                .ok_or_else(|| format!("the hub has no secret {secret}"))?;
            key.open(&set, &secret.name, &sealed)
                .ok_or_else(|| format!("the hub cannot decrypt its secret {secret}"))
        })
        .await
        .map_err(|err| format!("reading the hub's secrets: {err}"))?
    }
}

/// Writes `contents` to `path`, readable by this user only.
fn write(path: &Path, contents: &[u8]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    crate::config::write_atomically(path, contents)
        .and_then(|()| std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)))
        .with_context(|| format!("writing {}", path.display()))
}

fn error(code: ErrorCode, message: String) -> ErrorInfo {
    ErrorInfo { code, message }
}

fn internal(err: &anyhow::Error) -> ErrorInfo {
    error(ErrorCode::Internal, format!("{err:#}"))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use herder_protocol::RouteState;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::hub::cloudflare::fake::{ACCOUNT, FakeCloudflare, TOKEN};
    use crate::settings::PublicAddresses;

    fn no_cloudflared() -> anyhow::Result<Vec<u8>> {
        anyhow::bail!("no cloudflared in this test")
    }

    async fn hub(
        dir: &Path,
        fake: &FakeCloudflare,
        key: Option<SecretsKey>,
    ) -> (Arc<RemoteAccessHub>, CancellationToken) {
        let store = Arc::new(Mutex::new(HubStore::open(dir.join("hub.db")).unwrap()));
        let tunnel =
            ManagedTunnel::open(dir, &fake.url, Some("127.0.0.1:7448".parse().unwrap())).unwrap();
        let shutdown = CancellationToken::new();
        tunnel
            .start(
                Arc::new(PublicAddresses::default()),
                no_cloudflared,
                shutdown.clone(),
            )
            .await;
        let hub = RemoteAccessHub::open(dir, store, key.map(Arc::new), tunnel).unwrap();
        hub.start().await;
        (hub, shutdown)
    }

    fn tunnel(hostname: &str, secret: &str) -> RemoteAccessSettings {
        RemoteAccessSettings {
            wireguard: None,
            cloudflare: Some(CloudflareSettings {
                account_id: ACCOUNT.into(),
                api_token: secret.parse().unwrap(),
                relay: false,
                tunnel_hostname: Some(hostname.into()),
            }),
        }
    }

    #[tokio::test]
    async fn an_owner_sets_the_tunnel_with_a_hub_secret_and_only_owners_see_the_settings() {
        let fake = FakeCloudflare::start();
        let dir = tempfile::tempdir().unwrap();
        let (hub, shutdown) = hub(dir.path(), &fake, Some(SecretsKey::new(&[1; 32]))).await;
        assert_eq!(hub.get(true).await.tunnel, None);

        // The token must be a hub secret first.
        let err = hub
            .set(tunnel("hub.example.com", "cloudflare/CF"))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert!(
            err.message.contains("no secret cloudflare/CF"),
            "{}",
            err.message
        );

        hub.put_token(SecretValue::new(TOKEN).unwrap())
            .await
            .unwrap();
        let access = hub
            .set(tunnel("HTTPS://Hub.Example.com/", TOKEN_NAME))
            .await
            .unwrap();
        let settings = access.settings.unwrap();
        assert_eq!(
            settings
                .cloudflare
                .as_ref()
                .unwrap()
                .tunnel_hostname
                .as_deref(),
            Some("hub.example.com")
        );
        assert_eq!(access.tunnel.unwrap().url, "wss://hub.example.com");
        for _ in 0..400 {
            if fake.held().route("hub.example.com").is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(fake.held().route("hub.example.com").is_some());

        // A member sees the tunnel, not the settings.
        let member = hub.get(false).await;
        assert_eq!(member.settings, None);
        assert!(member.tunnel.is_some());

        // Kept across a restart, and never with the token's value.
        let saved = std::fs::read_to_string(dir.path().join("remote-access.json")).unwrap();
        assert!(saved.contains("hub.example.com") && !saved.contains(TOKEN));
        shutdown.cancel();
        let (again, shutdown) = hub_again(dir.path(), &fake).await;
        assert_eq!(again.get(true).await.settings, Some(settings));
        shutdown.cancel();
    }

    async fn hub_again(
        dir: &Path,
        fake: &FakeCloudflare,
    ) -> (Arc<RemoteAccessHub>, CancellationToken) {
        hub(dir, fake, Some(SecretsKey::new(&[1; 32]))).await
    }

    #[tokio::test]
    async fn what_is_not_here_yet_or_not_valid_is_refused() {
        let fake = FakeCloudflare::start();
        let dir = tempfile::tempdir().unwrap();
        let (hub, shutdown) = hub(dir.path(), &fake, Some(SecretsKey::new(&[1; 32]))).await;
        hub.put_token(SecretValue::new(TOKEN).unwrap())
            .await
            .unwrap();

        let mut relay = tunnel("hub.example.com", TOKEN_NAME);
        relay.cloudflare.as_mut().unwrap().relay = true;
        assert_eq!(
            hub.set(relay).await.unwrap_err().code,
            ErrorCode::Unsupported
        );

        let mut account = tunnel("hub.example.com", TOKEN_NAME);
        account.cloudflare.as_mut().unwrap().account_id = "not an id".into();
        assert_eq!(
            hub.set(account).await.unwrap_err().code,
            ErrorCode::BadRequest
        );

        let err = hub.set(tunnel("hub", TOKEN_NAME)).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert!(!dir.path().join("remote-access.json").exists());
        shutdown.cancel();
    }

    #[tokio::test]
    async fn herder_hub_tunnel_puts_the_token_and_takes_the_tunnel_away() {
        let fake = FakeCloudflare::start();
        let dir = tempfile::tempdir().unwrap();
        let (hub, shutdown) = hub(dir.path(), &fake, Some(SecretsKey::new(&[1; 32]))).await;
        let err = hub
            .set_tunnel(None, Some("hub.example.com".into()), None)
            .await
            .unwrap_err();
        assert!(err.message.contains("API token"), "{}", err.message);

        hub.set_tunnel(
            Some(ACCOUNT.into()),
            Some("hub.example.com".into()),
            SecretValue::new(TOKEN),
        )
        .await
        .unwrap();
        let mut up = None;
        for _ in 0..400 {
            up = hub.get(true).await.tunnel;
            if up
                .as_ref()
                .is_some_and(|t| t.state != RouteState::SettingUp)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        // No cloudflared can run here, so it is set up in Cloudflare but not up.
        assert!(fake.held().route("hub.example.com").is_some(), "{up:?}");

        hub.set_tunnel(None, None, None).await.unwrap();
        for _ in 0..400 {
            if fake.held().records.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(fake.held().records.is_empty());
        let cloudflare = hub.get(true).await.settings.unwrap().cloudflare.unwrap();
        assert_eq!(cloudflare.tunnel_hostname, None);
        assert_eq!(
            cloudflare.api_token.to_string(),
            "cloudflare/CLOUDFLARE_API_TOKEN"
        );
        assert_eq!(
            hub.set_tunnel(None, None, None).await.unwrap_err().code,
            ErrorCode::NotFound
        );
        shutdown.cancel();
    }

    #[tokio::test]
    async fn a_hub_without_a_secrets_key_cannot_hold_the_token() {
        let fake = FakeCloudflare::start();
        let dir = tempfile::tempdir().unwrap();
        let (hub, shutdown) = hub(dir.path(), &fake, None).await;
        let err = hub
            .set_tunnel(
                Some(ACCOUNT.into()),
                Some("hub.example.com".into()),
                SecretValue::new(TOKEN),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("keeps no secrets"), "{}", err.message);
        shutdown.cancel();
    }
}
