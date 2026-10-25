//! Secrets on the vault: owners manage them from clients, and hosts fetch the values of the
//! sets granted to them over their replication connection, for their sessions or, from the
//! sets granted for host use, for their own.
//!
//! Values are encrypted at rest with ChaCha20-Poly1305 under the vault's master key
//! ([`SecretsKey`]), which the vault reads from [`KEY_ENV`] at start: a random nonce per value,
//! and the secret's set and name as associated data, so a value moved to another row in the
//! database no longer opens. Without a key the vault keeps working, and refuses every secret
//! command with how to set one.
//!
//! A host's fetch resolves every secret it names before any value goes out
//! ([`VaultStore::resolve_secrets`]), and records one audit row per value handed out, without
//! the value. Nothing here logs a value.

use std::fmt;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use herder_protocol::{
    CommandBody, CommandResult, ErrorCode, ErrorInfo, HostId, MAX_SECRET_DESCRIPTION_CHARS,
    SecretError, SecretErrorCode, SecretRef, SecretValue, Timestamp, VaultMessage,
    is_valid_secret_name, is_valid_secret_purpose, is_valid_secret_set_name,
};
use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use tracing::{info, warn};

use super::store::{Grant, Sealed, SecretUse, SetDeleted};
use super::{VaultStore, blocking};

/// The environment variable the vault reads its master key from: 32 bytes, base64.
pub const KEY_ENV: &str = "HERDER_VAULT_SECRETS_KEY";

/// Bytes of a master key.
const KEY_LEN: usize = 32;

/// The vault's master key, which encrypts every secret's value. Its `Debug` shows nothing.
pub struct SecretsKey {
    key: LessSafeKey,
    random: SystemRandom,
}

impl fmt::Debug for SecretsKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretsKey(‹redacted›)")
    }
}

impl SecretsKey {
    /// The key in [`KEY_ENV`]; `None` when it is unset or empty.
    pub fn from_env() -> Result<Option<Self>> {
        match std::env::var(KEY_ENV) {
            Ok(text) if text.trim().is_empty() => Ok(None),
            Ok(text) => Self::from_base64(&text)
                .with_context(|| format!("reading the secrets key in {KEY_ENV}"))
                .map(Some),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => bail!("{KEY_ENV} is not base64"),
        }
    }

    /// A key from its base64 form, as `openssl rand -base64 32` prints it.
    pub fn from_base64(text: &str) -> Result<Self> {
        let bytes = STANDARD
            .decode(text.trim())
            .context("the key is not base64")?;
        let bytes: [u8; KEY_LEN] = bytes.try_into().map_err(|bytes: Vec<u8>| {
            anyhow::anyhow!("the key is {} bytes, not {KEY_LEN}", bytes.len())
        })?;
        Ok(Self::new(&bytes))
    }

    /// A new random key in its base64 form, for [`KEY_ENV`].
    pub fn generate_base64() -> Result<String> {
        let mut bytes = [0u8; KEY_LEN];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| anyhow::anyhow!("the system random number generator failed"))?;
        Ok(STANDARD.encode(bytes))
    }

    /// A key of these bytes.
    pub fn new(bytes: &[u8; KEY_LEN]) -> Self {
        // Only a key of the wrong length fails, and ChaCha20-Poly1305 takes exactly 32 bytes.
        let key = UnboundKey::new(&CHACHA20_POLY1305, bytes).expect("a 32-byte key");
        Self {
            key: LessSafeKey::new(key),
            random: SystemRandom::new(),
        }
    }

    /// Encrypts `value` as the secret `name` of `set`, under a fresh random nonce.
    pub fn seal(&self, set: &str, name: &str, value: &SecretValue) -> Result<Sealed> {
        let mut nonce = [0u8; NONCE_LEN];
        self.random
            .fill(&mut nonce)
            .map_err(|_| anyhow::anyhow!("the system random number generator failed"))?;
        let mut ciphertext = value.expose().as_bytes().to_vec();
        self.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(associated(set, name)),
                &mut ciphertext,
            )
            .map_err(|_| anyhow::anyhow!("encrypting the secret failed"))?;
        Ok(Sealed {
            nonce: nonce.to_vec(),
            ciphertext,
        })
    }

    /// The value of the secret `name` of `set`; `None` unless `sealed` was sealed under this
    /// key as that secret.
    pub fn open(&self, set: &str, name: &str, sealed: &Sealed) -> Option<SecretValue> {
        let nonce = Nonce::try_assume_unique_for_key(&sealed.nonce).ok()?;
        let mut buffer = sealed.ciphertext.clone();
        let plain = self
            .key
            .open_in_place(nonce, Aad::from(associated(set, name)), &mut buffer)
            .ok()?;
        SecretValue::new(String::from_utf8(plain.to_vec()).ok()?)
    }
}

/// What binds a value to its secret: `set/NAME`, which no other secret shares, since set
/// names hold no `/`.
fn associated(set: &str, name: &str) -> Vec<u8> {
    format!("{set}/{name}").into_bytes()
}

/// Runs an owner's secret command on the vault's `store`.
pub(super) async fn command(
    store: &Arc<Mutex<VaultStore>>,
    key: Option<&Arc<SecretsKey>>,
    command: CommandBody,
) -> Result<CommandResult, ErrorInfo> {
    let Some(key) = key else {
        return Err(error(
            ErrorCode::Unsupported,
            format!(
                "this vault has no secrets key; set {KEY_ENV} to 32 random bytes in base64 \
                 (`openssl rand -base64 32`) and restart the vault (see deploy/README.md)"
            ),
        ));
    };
    let now = Timestamp::now();
    match command {
        CommandBody::CreateSecretSet { set } => {
            if !is_valid_secret_set_name(&set) {
                return Err(bad_set(&set));
            }
            let created = {
                let set = set.clone();
                run(store, move |store| store.create_secret_set(&set, now)).await?
            };
            if !created {
                return Err(error(
                    ErrorCode::Conflict,
                    format!("a secret set named {set} exists already"),
                ));
            }
            info!(set, "secret set created");
        }
        CommandBody::DeleteSecretSet { set } => {
            let deleted = {
                let set = set.clone();
                run(store, move |store| store.delete_secret_set(&set)).await?
            };
            match deleted {
                SetDeleted::Deleted => info!(set, "secret set deleted"),
                SetDeleted::NotFound => return Err(no_set(&set)),
                SetDeleted::NotEmpty => {
                    return Err(error(
                        ErrorCode::Conflict,
                        format!("the set {set} still holds secrets; delete them first"),
                    ));
                }
            }
        }
        CommandBody::PutSecret {
            set,
            name,
            description,
            value,
        } => {
            if !is_valid_secret_name(&name) {
                return Err(bad_name(&name));
            }
            if description.contains(['\n', '\r'])
                || description.chars().count() > MAX_SECRET_DESCRIPTION_CHARS
            {
                return Err(error(
                    ErrorCode::BadRequest,
                    format!(
                        "a secret's description is one line of at most \
                         {MAX_SECRET_DESCRIPTION_CHARS} characters"
                    ),
                ));
            }
            // The wire form is a plain string, so its limits are checked here.
            let Some(value) = SecretValue::new(value.expose()) else {
                return Err(error(
                    ErrorCode::BadRequest,
                    format!(
                        "a secret's value is 1 to {} bytes without a NUL",
                        herder_protocol::MAX_SECRET_VALUE_BYTES
                    ),
                ));
            };
            let sealed = key
                .seal(&set, &name, &value)
                .map_err(|err| internal(&err))?;
            let put = {
                let (set, name) = (set.clone(), name.clone());
                run(store, move |store| {
                    store.put_secret(&set, &name, &description, &sealed, now)
                })
                .await?
            };
            if !put {
                return Err(no_set(&set));
            }
            info!(set, name, "secret stored");
        }
        CommandBody::DeleteSecret { set, name } => {
            let deleted = {
                let (set, name) = (set.clone(), name.clone());
                run(store, move |store| store.delete_secret(&set, &name)).await?
            };
            if !deleted {
                return Err(error(
                    ErrorCode::NotFound,
                    format!("the set {set} has no secret {name}"),
                ));
            }
            info!(set, name, "secret deleted");
        }
        CommandBody::GrantSecretSet {
            set,
            host_id,
            host_use,
        } => {
            let granted = {
                let (set, host_id) = (set.clone(), host_id.clone());
                run(store, move |store| {
                    store.grant_secret_set(&set, &host_id, host_use)
                })
                .await?
            };
            match granted {
                Grant::Granted => info!(set, %host_id, host_use, "secret set granted"),
                Grant::NoSet => return Err(no_set(&set)),
                Grant::NoHost => {
                    return Err(error(
                        ErrorCode::NotFound,
                        format!(
                            "no host {host_id} backs up to this vault; link that host to it first"
                        ),
                    ));
                }
            }
        }
        CommandBody::RevokeSecretSet {
            set,
            host_id,
            host_use,
        } => {
            let revoked = {
                let (set, host_id) = (set.clone(), host_id.clone());
                run(store, move |store| {
                    store.revoke_secret_set(&set, &host_id, host_use)
                })
                .await?
            };
            if !revoked {
                let way = if host_use { " for host use" } else { "" };
                return Err(error(
                    ErrorCode::NotFound,
                    format!("the set {set} is not granted to the host {host_id}{way}"),
                ));
            }
            info!(set, %host_id, host_use, "secret set revoked");
        }
        CommandBody::ListSecrets => {
            return run(store, |store| {
                Ok(CommandResult::Secrets {
                    sets: store.secret_sets()?,
                    secrets: store.secrets()?,
                })
            })
            .await;
        }
        CommandBody::ListSecretAudit { before } => {
            return run(store, move |store| {
                Ok(CommandResult::SecretAudit {
                    entries: store.secret_audit(before)?,
                })
            })
            .await;
        }
        _ => {
            return Err(error(
                ErrorCode::BadRequest,
                "not a secret command".to_owned(),
            ));
        }
    }
    Ok(CommandResult::Applied)
}

/// Answers a host's `list_secrets`: the secrets in the sets granted to it, without values.
pub(super) async fn list(
    store: &Arc<Mutex<VaultStore>>,
    host: &HostId,
    request_id: u64,
) -> Result<VaultMessage> {
    let host = host.clone();
    let secrets = blocking(store, move |store| store.granted_secrets(&host)).await?;
    Ok(VaultMessage::SecretList {
        request_id,
        secrets,
    })
}

/// Answers a host's `fetch_secrets` or `fetch_host_secrets`, request `request_id`: every value
/// it asked for, or why none goes out.
pub(super) async fn fetch(
    store: &Arc<Mutex<VaultStore>>,
    key: Option<&Arc<SecretsKey>>,
    host: &HostId,
    request_id: u64,
    used: SecretUse,
    secrets: Vec<SecretRef>,
) -> Result<VaultMessage> {
    Ok(match fetch_values(store, key, host, used, secrets).await? {
        Ok(secrets) => VaultMessage::SecretValues {
            request_id,
            secrets,
        },
        Err(error) => VaultMessage::SecretsRefused { request_id, error },
    })
}

async fn fetch_values(
    store: &Arc<Mutex<VaultStore>>,
    key: Option<&Arc<SecretsKey>>,
    host: &HostId,
    used: SecretUse,
    secrets: Vec<SecretRef>,
) -> Result<Result<Vec<herder_protocol::FetchedSecret>, SecretError>> {
    let Some(key) = key.cloned() else {
        return Ok(Err(refused(
            SecretErrorCode::NoVault,
            format!(
                "the vault has no secrets key, so it hands out no secrets; its owner sets \
                 {KEY_ENV} (see deploy/README.md)"
            ),
        )));
    };
    if secrets.is_empty() {
        return Ok(Err(refused(
            SecretErrorCode::Unknown,
            "name at least one secret to fetch".to_owned(),
        )));
    }
    if let SecretUse::Host(purpose) = &used
        && !is_valid_secret_purpose(purpose)
    {
        return Ok(Err(refused(
            SecretErrorCode::Unknown,
            format!(
                "`{purpose}` cannot say what secrets are for: lowercase letters, digits, `-`, \
                 `_`, `.` and `:`, at most {} bytes",
                herder_protocol::MAX_SECRET_NAME_BYTES
            ),
        )));
    }
    let owner = host.clone();
    let fetched = blocking(store, move |store| {
        let resolved = match store.resolve_secrets(&owner, used.host_use(), &secrets)? {
            Ok(resolved) => resolved,
            Err(error) => return Ok(Err(error)),
        };
        let mut values = Vec::with_capacity(resolved.len());
        for secret in resolved {
            let Some(value) = key.open(&secret.set, &secret.secret.name, &secret.sealed) else {
                return Ok(Err(SecretError {
                    code: SecretErrorCode::Unknown,
                    message: format!(
                        "the vault cannot decrypt {}/{}: it was stored under another secrets key than \
                         the one in HERDER_VAULT_SECRETS_KEY; restore that key or store it again",
                        secret.set, secret.secret.name
                    ),
                    secret: Some(secret.secret),
                    candidates: Vec::new(),
                }));
            };
            values.push(herder_protocol::FetchedSecret {
                secret: secret.secret,
                set: secret.set,
                value,
            });
        }
        let audited: Vec<(String, String)> = values
            .iter()
            .map(|value| (value.set.clone(), value.secret.name.clone()))
            .collect();
        store.audit_secrets(&owner, &used, &audited, Timestamp::now())?;
        Ok(Ok((values, used)))
    })
    .await?;
    Ok(match fetched {
        Ok((values, used)) => {
            let names: Vec<String> = values
                .iter()
                .map(|value| format!("{}/{}", value.set, value.secret.name))
                .collect();
            match used {
                SecretUse::Session(session_id) => {
                    info!(host_id = %host, %session_id, ?names, "secrets handed out");
                }
                SecretUse::Host(purpose) => {
                    info!(host_id = %host, purpose, ?names, "secrets handed out for host use");
                }
            }
            Ok(values)
        }
        Err(error) => {
            warn!(host_id = %host, code = ?error.code, "secrets refused: {}", error.message);
            Err(error)
        }
    })
}

/// Runs `call` on the store, failing as an internal error.
async fn run<T: Send + 'static>(
    store: &Arc<Mutex<VaultStore>>,
    call: impl FnOnce(&mut VaultStore) -> super::store::Result<T> + Send + 'static,
) -> Result<T, ErrorInfo> {
    blocking(store, call).await.map_err(|err| internal(&err))
}

fn refused(code: SecretErrorCode, message: String) -> SecretError {
    SecretError {
        code,
        message,
        secret: None,
        candidates: Vec::new(),
    }
}

fn error(code: ErrorCode, message: String) -> ErrorInfo {
    ErrorInfo { code, message }
}

fn internal(err: &anyhow::Error) -> ErrorInfo {
    warn!("a secret command failed: {err:#}");
    error(ErrorCode::Internal, format!("{err:#}"))
}

fn no_set(set: &str) -> ErrorInfo {
    error(ErrorCode::NotFound, format!("there is no secret set {set}"))
}

fn bad_set(set: &str) -> ErrorInfo {
    error(
        ErrorCode::BadRequest,
        format!(
            "`{set}` cannot name a set: lowercase letters, digits and single hyphens, at most \
             {} bytes",
            herder_protocol::MAX_SECRET_NAME_BYTES
        ),
    )
}

fn bad_name(name: &str) -> ErrorInfo {
    error(
        ErrorCode::BadRequest,
        format!(
            "`{name}` cannot name a secret: uppercase letters, digits and underscores, not \
             starting with a digit, at most {} bytes",
            herder_protocol::MAX_SECRET_NAME_BYTES
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> SecretsKey {
        SecretsKey::new(&[byte; KEY_LEN])
    }

    fn value(text: &str) -> SecretValue {
        SecretValue::new(text).unwrap()
    }

    #[test]
    fn a_value_round_trips_and_is_not_kept_in_plaintext() {
        let key = key(7);
        let sealed = key
            .seal("prod", "STRIPE_KEY", &value("sk_live_123"))
            .unwrap();
        assert_eq!(sealed.nonce.len(), NONCE_LEN);
        assert!(
            !sealed
                .ciphertext
                .windows(b"sk_live_123".len())
                .any(|window| window == b"sk_live_123")
        );
        let opened = key.open("prod", "STRIPE_KEY", &sealed).unwrap();
        assert_eq!(opened.expose(), "sk_live_123");
        // Every value gets its own nonce.
        let again = key
            .seal("prod", "STRIPE_KEY", &value("sk_live_123"))
            .unwrap();
        assert_ne!(again.nonce, sealed.nonce);
        assert_ne!(again.ciphertext, sealed.ciphertext);
    }

    #[test]
    fn another_key_opens_nothing() {
        let sealed = key(1).seal("prod", "TOKEN", &value("v")).unwrap();
        assert!(key(2).open("prod", "TOKEN", &sealed).is_none());
    }

    #[test]
    fn a_value_opens_only_as_the_secret_it_was_sealed_as() {
        let key = key(3);
        let sealed = key.seal("prod", "TOKEN", &value("v")).unwrap();
        assert!(key.open("dev", "TOKEN", &sealed).is_none());
        assert!(key.open("prod", "OTHER", &sealed).is_none());
        let mut tampered = key.seal("prod", "TOKEN", &value("v")).unwrap();
        tampered.ciphertext[0] ^= 1;
        assert!(key.open("prod", "TOKEN", &tampered).is_none());
    }

    #[test]
    fn a_generated_key_reads_back_and_is_new_each_time() {
        let text = SecretsKey::generate_base64().unwrap();
        let key = SecretsKey::from_base64(&text).unwrap();
        let sealed = key.seal("prod", "TOKEN", &value("v")).unwrap();
        assert_eq!(key.open("prod", "TOKEN", &sealed).unwrap().expose(), "v");
        assert_ne!(SecretsKey::generate_base64().unwrap(), text);
    }

    #[test]
    fn keys_come_in_base64_of_32_bytes_and_never_show() {
        let text = STANDARD.encode([9u8; KEY_LEN]);
        let key = SecretsKey::from_base64(&format!("{text}\n")).unwrap();
        assert_eq!(format!("{key:?}"), "SecretsKey(‹redacted›)");
        let short = SecretsKey::from_base64(&STANDARD.encode([9u8; 16])).unwrap_err();
        assert!(
            format!("{short:#}").contains("16 bytes, not 32"),
            "{short:#}"
        );
        assert!(SecretsKey::from_base64("not base64!").is_err());
    }
}
