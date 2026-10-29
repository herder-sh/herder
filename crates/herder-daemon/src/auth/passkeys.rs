//! Passkey ceremonies for [`PASSKEY_RP_ID`], verified offline by webauthn-rs against the public
//! keys the daemon stores.
//!
//! Every challenge is minted here, works once and expires after [`PASSKEY_CHALLENGE_SECS`]; an
//! assertion or registration names its challenge in its client data, which is how it finds its
//! ceremony. Challenges live in memory only, so a restart voids them.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use herder_protocol::{
    ErrorCode, ErrorInfo, PASSKEY_CHALLENGE_SECS, PASSKEY_RP_ID, PasskeyAssertion,
    PasskeyChallenge, PasskeyCreation, PasskeyKey, PasskeyRegistration, PersonId, Timestamp,
};
use serde::Deserialize;
use tracing::debug;
use url::Url;
use uuid::Uuid;
use webauthn_rs::Webauthn;
use webauthn_rs::prelude::{
    AuthenticationResult, DiscoverableAuthentication, DiscoverableKey, Passkey,
    PasskeyRegistration as Registering, PublicKeyCredential, RegisterPublicKeyCredential,
};

/// How long a challenge stays valid.
const TTL: Duration = Duration::from_secs(PASSKEY_CHALLENGE_SECS);

/// The origin the platform's passkey API reports for an app associated with [`PASSKEY_RP_ID`].
const ORIGIN: &str = "https://herder.sh";

/// What a person may be added with: a passkey's public key, as daemons keep it.
pub fn key(passkey: &Passkey) -> PasskeyKey {
    PasskeyKey {
        credential_id: URL_SAFE_NO_PAD.encode(passkey.cred_id()),
        // A passkey is plain data; encoding it to a JSON value cannot fail.
        public_key: serde_json::to_value(passkey).unwrap_or_default(),
    }
}

/// The passkey a [`PasskeyKey`] carries, if it is one and its id matches.
pub fn passkey(key: &PasskeyKey) -> Result<Passkey, ErrorInfo> {
    let passkey: Passkey = serde_json::from_value(key.public_key.clone())
        .map_err(|err| bad_request(&format!("not a passkey's public key: {err}")))?;
    if URL_SAFE_NO_PAD.encode(passkey.cred_id()) != key.credential_id {
        return Err(bad_request(
            "the passkey's credential id does not match its key",
        ));
    }
    Ok(passkey)
}

/// The person a passkey identifies, from its user handle.
pub fn person_of(uuid: Uuid) -> PersonId {
    PersonId::new(uuid.hyphenated().to_string())
}

/// A verified assertion: who made it, and how to note it on their stored passkeys.
pub struct Asserted {
    /// The person whose passkey signed.
    pub person_id: PersonId,
    /// Feed to [`Passkey::update_credential`] on each of their passkeys.
    pub result: AuthenticationResult,
}

/// Ceremonies in progress, by their challenge's base64url form, with when each expires.
type Open<S> = Mutex<HashMap<String, (S, Instant)>>;

/// The daemon's passkey ceremonies in progress.
pub struct Passkeys {
    webauthn: Webauthn,
    /// Assertion challenges.
    assertions: Open<DiscoverableAuthentication>,
    /// Registration challenges, with the person each is for.
    registrations: Open<(Registering, Uuid)>,
}

impl Passkeys {
    /// Ceremonies for [`PASSKEY_RP_ID`].
    pub fn new() -> Result<Self> {
        let origin = Url::parse(ORIGIN).context("parsing the passkey origin")?;
        let webauthn = webauthn_rs::WebauthnBuilder::new(PASSKEY_RP_ID, &origin)
            .and_then(|builder| builder.rp_name("herder").build())
            .context("setting up passkeys")?;
        Ok(Self {
            webauthn,
            assertions: Mutex::default(),
            registrations: Mutex::default(),
        })
    }

    /// A fresh challenge for any known passkey to sign.
    pub fn challenge(&self) -> Result<PasskeyChallenge, ErrorInfo> {
        let (request, state) = self
            .webauthn
            .start_discoverable_authentication()
            .map_err(internal)?;
        let challenge = URL_SAFE_NO_PAD.encode(&request.public_key.challenge);
        let now = Instant::now();
        let mut open = lock(&self.assertions);
        open.retain(|_, (_, expires)| *expires > now);
        open.insert(challenge.clone(), (state, now + TTL));
        Ok(PasskeyChallenge {
            rp_id: PASSKEY_RP_ID.to_owned(),
            challenge,
            expires_at: expires_at()?,
        })
    }

    /// A fresh challenge to create a passkey for a new person from.
    pub fn creation(&self) -> Result<PasskeyCreation, ErrorInfo> {
        let uuid = Uuid::now_v7();
        let (request, state) = self
            .webauthn
            .start_passkey_registration(uuid, "herder", "herder", None)
            .map_err(internal)?;
        let challenge = URL_SAFE_NO_PAD.encode(&request.public_key.challenge);
        let now = Instant::now();
        let mut open = lock(&self.registrations);
        open.retain(|_, (_, expires)| *expires > now);
        open.insert(challenge.clone(), ((state, uuid), now + TTL));
        Ok(PasskeyCreation {
            rp_id: PASSKEY_RP_ID.to_owned(),
            challenge,
            person_id: person_of(uuid),
            user_handle: URL_SAFE_NO_PAD.encode(uuid.as_bytes()),
            expires_at: expires_at()?,
        })
    }

    /// Verifies `assertion` over one of this daemon's unexpired challenges, using it up,
    /// against the passkeys `keys` gives for the person it names.
    pub fn verify(
        &self,
        assertion: &PasskeyAssertion,
        keys: impl FnOnce(&PersonId) -> Vec<Passkey>,
    ) -> Result<Asserted, ErrorInfo> {
        let credential: PublicKeyCredential = convert(assertion)?;
        let challenge = challenge_of(&credential.response.client_data_json)?;
        let state = take(&self.assertions, &challenge)?;
        let (uuid, _) = self
            .webauthn
            .identify_discoverable_authentication(&credential)
            .map_err(|_| forbidden("the passkey gave no person"))?;
        let person_id = person_of(uuid);
        let keys: Vec<DiscoverableKey> = keys(&person_id).iter().map(Into::into).collect();
        if keys.is_empty() {
            return Err(forbidden("this passkey's person is not on this daemon"));
        }
        let result = self
            .webauthn
            .finish_discoverable_authentication(&credential, state, &keys)
            .map_err(|err| {
                debug!("passkey assertion refused: {err}");
                forbidden("the passkey's signature does not verify")
            })?;
        Ok(Asserted { person_id, result })
    }

    /// Verifies `registration` from one of this daemon's unexpired creation challenges, using
    /// it up; returns the person it was created for and the new passkey.
    pub fn register(
        &self,
        registration: &PasskeyRegistration,
    ) -> Result<(PersonId, Passkey), ErrorInfo> {
        let credential: RegisterPublicKeyCredential = convert(registration)?;
        let challenge = challenge_of(&credential.response.client_data_json)?;
        let (state, uuid) = take(&self.registrations, &challenge)?;
        let passkey = self
            .webauthn
            .finish_passkey_registration(&credential, &state)
            .map_err(|err| {
                debug!("passkey registration refused: {err}");
                forbidden("the new passkey does not verify")
            })?;
        Ok((person_of(uuid), passkey))
    }
}

/// The challenge the client data of a ceremony names.
fn challenge_of(client_data_json: &[u8]) -> Result<String, ErrorInfo> {
    #[derive(Deserialize)]
    struct ClientData {
        challenge: String,
    }
    serde_json::from_slice::<ClientData>(client_data_json)
        .map(|data| data.challenge)
        .map_err(|_| bad_request("the passkey's client data names no challenge"))
}

/// The ceremony of `challenge`, used up; refused once expired or unknown.
fn take<S>(open: &Open<S>, challenge: &str) -> Result<S, ErrorInfo> {
    match lock(open).remove(challenge) {
        Some((state, expires)) if expires > Instant::now() => Ok(state),
        _ => Err(forbidden(
            "the passkey challenge is unknown, used or expired; ask for a new one",
        )),
    }
}

/// A protocol message as the webauthn-rs type with the same WebAuthn JSON.
fn convert<T: serde::de::DeserializeOwned>(
    message: &impl serde::Serialize,
) -> Result<T, ErrorInfo> {
    serde_json::to_value(message)
        .and_then(serde_json::from_value)
        .map_err(|err| bad_request(&format!("not a WebAuthn credential: {err}")))
}

fn expires_at() -> Result<Timestamp, ErrorInfo> {
    Timestamp::now().checked_add(TTL).map_err(internal)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Each update is a single insert or remove, so a poisoned map is still consistent.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn forbidden(message: &str) -> ErrorInfo {
    ErrorInfo {
        code: ErrorCode::Forbidden,
        message: message.to_owned(),
    }
}

fn bad_request(message: &str) -> ErrorInfo {
    ErrorInfo {
        code: ErrorCode::BadRequest,
        message: message.to_owned(),
    }
}

fn internal(err: impl std::fmt::Display) -> ErrorInfo {
    tracing::error!("passkeys: {err}");
    ErrorInfo {
        code: ErrorCode::Internal,
        message: "passkeys failed on this daemon".to_owned(),
    }
}
