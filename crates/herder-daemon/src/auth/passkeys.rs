//! Passkey ceremonies for [`PASSKEY_RP_ID`], verified offline by webauthn-rs against the public
//! keys the daemon stores.
//!
//! A key is kept, and travels, as WebAuthn defines it: the credential id and the COSE_Key from
//! the authenticator data ([`Key`]). webauthn-rs's own credential is rebuilt from it for each
//! verification, never stored.
//!
//! Every challenge is minted here, works once and expires after [`PASSKEY_CHALLENGE_SECS`]; an
//! assertion or registration names its challenge in its client data, which is how it finds its
//! ceremony. Challenges live in memory only, so a restart voids them.
//!
//! A claim, or a merge of a duplicate person, may sign several daemons' challenges at once
//! ([`Passkeys::verify_batch`]): what lets one Face ID claim, or merge on, every machine. The signed challenge is then the daemons' challenges, each
//! [`CHALLENGE_LEN`] bytes, one after the other. A daemon accepts it only when its own challenge
//! is among them, unexpired and unused, and uses it up; every other check is the same. Any other
//! assertion signs exactly one challenge of this daemon's.

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
use serde::{Deserialize, Serialize};
use serde_cbor_2::Value;
use tracing::debug;
use url::Url;
use uuid::Uuid;
use webauthn_rs::Webauthn;
use webauthn_rs::prelude::{
    AttestationFormat, AuthenticationResult, COSEKey, COSEKeyType, Credential,
    DiscoverableAuthentication, DiscoverableKey, ParsedAttestation, Passkey,
    PasskeyRegistration as Registering, PublicKeyCredential, RegisterPublicKeyCredential,
};
use webauthn_rs_proto::{RegisteredExtensions, UserVerificationPolicy};

/// How long a challenge stays valid.
const TTL: Duration = Duration::from_secs(PASSKEY_CHALLENGE_SECS);

/// The origin the platform's passkey API reports for an app associated with [`PASSKEY_RP_ID`].
const ORIGIN: &str = "https://herder.sh";

/// Backup eligible, in the authenticator data's flags.
const BACKUP_ELIGIBLE: u8 = 0x08;

/// Backed up, in the authenticator data's flags.
const BACKED_UP: u8 = 0x10;

/// The length of a challenge webauthn-rs mints, and of each in a batch.
const CHALLENGE_LEN: usize = 32;

/// Offset of the flags in the authenticator data, after the RP id hash.
const FLAGS_AT: usize = 32;

/// A passkey's public key as this daemon keeps it: WebAuthn's credential id and COSE_Key, and
/// the signature counter it last showed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    /// The credential id, base64url.
    pub credential_id: String,
    /// The COSE_Key, base64url.
    pub public_key: String,
    /// Its signature counter at its last use; synced passkeys always show 0.
    #[serde(default)]
    pub counter: u32,
}

impl Key {
    /// The key as the protocol carries it.
    pub fn wire(&self) -> PasskeyKey {
        PasskeyKey {
            credential_id: self.credential_id.clone(),
            public_key: self.public_key.clone(),
        }
    }

    /// The key `key` carries, once its id and COSE_Key decode.
    pub fn from_wire(key: &PasskeyKey) -> Result<Self, ErrorInfo> {
        let parsed = Self {
            credential_id: key.credential_id.clone(),
            public_key: key.public_key.clone(),
            counter: 0,
        };
        parsed.credential(false, false)?;
        Ok(parsed)
    }

    /// The key webauthn-rs registered.
    fn registered(passkey: Passkey) -> Result<Self, ErrorInfo> {
        let credential = Credential::from(passkey);
        Ok(Self {
            credential_id: URL_SAFE_NO_PAD.encode(&credential.cred_id),
            public_key: URL_SAFE_NO_PAD.encode(cose_encode(&credential.cred)?),
            counter: credential.counter,
        })
    }

    /// Notes a verified use; whether the counter moved.
    pub fn note(&mut self, result: &AuthenticationResult) -> bool {
        let used = URL_SAFE_NO_PAD.encode(result.cred_id()) == self.credential_id;
        if !used || result.counter() <= self.counter {
            return false;
        }
        self.counter = result.counter();
        true
    }

    /// webauthn-rs's credential for this key, as backed up as the assertion says: whether a
    /// passkey syncs is the authenticator's to say, and a key from another daemon does not
    /// carry it.
    fn credential(&self, backup_eligible: bool, backed_up: bool) -> Result<Credential, ErrorInfo> {
        let malformed = || bad_request("not a passkey's credential id and COSE public key");
        let cred_id = URL_SAFE_NO_PAD
            .decode(&self.credential_id)
            .map_err(|_| malformed())?;
        let bytes = URL_SAFE_NO_PAD
            .decode(&self.public_key)
            .map_err(|_| malformed())?;
        let value: Value = serde_cbor_2::from_slice(&bytes).map_err(|_| malformed())?;
        let cred = COSEKey::try_from(&value).map_err(|_| malformed())?;
        Ok(Credential {
            cred_id,
            cred,
            counter: self.counter,
            transports: None,
            user_verified: true,
            backup_eligible,
            backup_state: backed_up,
            registration_policy: UserVerificationPolicy::Required,
            extensions: RegisteredExtensions::none(),
            attestation: ParsedAttestation::default(),
            attestation_format: AttestationFormat::None,
        })
    }
}

/// `key` as a COSE_Key, the encoding the authenticator data carries.
fn cose_encode(key: &COSEKey) -> Result<Vec<u8>, ErrorInfo> {
    let int = |value: i64| Value::Integer(value.into());
    let alg = int(i64::from(key.type_ as i32));
    let entries = match &key.key {
        COSEKeyType::EC_EC2(ec2) => vec![
            (int(1), int(2)),
            (int(3), alg),
            (int(-1), int(ec2.curve.clone() as i64)),
            (int(-2), Value::Bytes(ec2.x.clone())),
            (int(-3), Value::Bytes(ec2.y.clone())),
        ],
        COSEKeyType::EC_OKP(okp) => vec![
            (int(1), int(1)),
            (int(3), alg),
            (int(-1), int(okp.curve.clone() as i64)),
            (int(-2), Value::Bytes(okp.x.clone())),
        ],
        COSEKeyType::RSA(rsa) => vec![
            (int(1), int(3)),
            (int(3), alg),
            (int(-1), Value::Bytes(rsa.n.clone())),
            (int(-2), Value::Bytes(rsa.e.to_vec())),
        ],
    };
    serde_cbor_2::to_vec(&Value::Map(entries.into_iter().collect())).map_err(internal)
}

/// The person a passkey identifies, from its user handle.
pub fn person_of(uuid: Uuid) -> PersonId {
    PersonId::new(uuid.hyphenated().to_string())
}

/// A verified assertion: who made it, and how to note it on their stored passkeys.
pub struct Asserted {
    /// The person whose passkey signed.
    pub person_id: PersonId,
    /// Feed to [`Key::note`] on each of their keys.
    pub result: AuthenticationResult,
}

/// Ceremonies in progress, by their challenge's base64url form, with when each expires.
type Open<S> = Mutex<HashMap<String, (S, Instant)>>;

/// The daemon's passkey ceremonies in progress.
pub struct Passkeys {
    webauthn: Webauthn,
    /// How long a challenge stays valid.
    ttl: Duration,
    /// Assertion challenges.
    assertions: Open<DiscoverableAuthentication>,
    /// Registration challenges, with the person each is for.
    registrations: Open<(Registering, Uuid)>,
}

impl Passkeys {
    /// Ceremonies for [`PASSKEY_RP_ID`].
    pub fn new() -> Result<Self> {
        Self::with_ttl(TTL)
    }

    /// Ceremonies whose challenges stay valid for `ttl`.
    pub fn with_ttl(ttl: Duration) -> Result<Self> {
        let origin = Url::parse(ORIGIN).context("parsing the passkey origin")?;
        let webauthn = webauthn_rs::WebauthnBuilder::new(PASSKEY_RP_ID, &origin)
            .and_then(|builder| builder.rp_name("herder").build())
            .context("setting up passkeys")?;
        Ok(Self {
            webauthn,
            ttl,
            assertions: Mutex::default(),
            registrations: Mutex::default(),
        })
    }

    fn expires_at(&self) -> Result<Timestamp, ErrorInfo> {
        Timestamp::now().checked_add(self.ttl).map_err(internal)
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
        open.insert(challenge.clone(), (state, now + self.ttl));
        Ok(PasskeyChallenge {
            rp_id: PASSKEY_RP_ID.to_owned(),
            challenge,
            expires_at: self.expires_at()?,
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
        open.insert(challenge.clone(), ((state, uuid), now + self.ttl));
        Ok(PasskeyCreation {
            rp_id: PASSKEY_RP_ID.to_owned(),
            challenge,
            person_id: person_of(uuid),
            user_handle: URL_SAFE_NO_PAD.encode(uuid.as_bytes()),
            expires_at: self.expires_at()?,
        })
    }

    /// Verifies `assertion` over one of this daemon's unexpired challenges, using it up,
    /// against the passkeys `keys` gives for the person it names.
    pub fn verify(
        &self,
        assertion: &PasskeyAssertion,
        keys: impl FnOnce(&PersonId) -> Vec<Key>,
    ) -> Result<Asserted, ErrorInfo> {
        self.verify_signed(assertion, keys, false)
    }

    /// [`Passkeys::verify`], for an assertion that may sign a batch of daemons' challenges,
    /// this daemon's among them: a claim, or a merge, on every machine at once.
    pub fn verify_batch(
        &self,
        assertion: &PasskeyAssertion,
        keys: impl FnOnce(&PersonId) -> Vec<Key>,
    ) -> Result<Asserted, ErrorInfo> {
        self.verify_signed(assertion, keys, true)
    }

    fn verify_signed(
        &self,
        assertion: &PasskeyAssertion,
        keys: impl FnOnce(&PersonId) -> Vec<Key>,
        batch: bool,
    ) -> Result<Asserted, ErrorInfo> {
        let credential: PublicKeyCredential = convert(assertion)?;
        let signed = challenge_of(&credential.response.client_data_json)?;
        let state = match batch {
            true => self.take_batch(&signed)?,
            false => take(&self.assertions, &signed)?,
        };
        let (uuid, _) = self
            .webauthn
            .identify_discoverable_authentication(&credential)
            .map_err(|_| forbidden("the passkey gave no person"))?;
        let person_id = person_of(uuid);
        let flags = credential
            .response
            .authenticator_data
            .get(FLAGS_AT)
            .copied()
            .unwrap_or_default();
        let (eligible, backed_up) = (flags & BACKUP_ELIGIBLE != 0, flags & BACKED_UP != 0);
        let keys = keys(&person_id)
            .iter()
            .map(|key| {
                let passkey = Passkey::from(key.credential(eligible, backed_up)?);
                Ok(DiscoverableKey::from(&passkey))
            })
            .collect::<Result<Vec<_>, ErrorInfo>>()?;
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

    /// The ceremony of this daemon's challenge within `signed`, used up, as the ceremony of all
    /// of `signed`: the challenges an assertion signed, of this daemon and of others.
    fn take_batch(&self, signed: &str) -> Result<DiscoverableAuthentication, ErrorInfo> {
        let bytes = URL_SAFE_NO_PAD
            .decode(signed)
            .map_err(|_| bad_request("the passkey's challenge is not base64url"))?;
        if bytes.len() == CHALLENGE_LEN {
            return take(&self.assertions, signed);
        }
        if bytes.is_empty() || bytes.len() % CHALLENGE_LEN != 0 {
            return Err(bad_request(
                "the passkey's challenge is no batch of challenges",
            ));
        }
        let ours = bytes
            .chunks(CHALLENGE_LEN)
            .map(|chunk| URL_SAFE_NO_PAD.encode(chunk))
            .find(|challenge| lock(&self.assertions).contains_key(challenge))
            .unwrap_or_default();
        let state = take(&self.assertions, &ours)?;
        // webauthn-rs checks the client data's challenge against its state's, which is ours
        // alone; the state is the same ceremony over all that was signed.
        let mut value = serde_json::to_value(state).map_err(internal)?;
        value["ast"]["challenge"] = signed.into();
        serde_json::from_value(value).map_err(internal)
    }

    /// Verifies `registration` from one of this daemon's unexpired creation challenges, using
    /// it up; returns the person it was created for and the new passkey.
    pub fn register(
        &self,
        registration: &PasskeyRegistration,
    ) -> Result<(PersonId, Key), ErrorInfo> {
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
        Ok((person_of(uuid), Key::registered(passkey)?))
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

#[cfg(test)]
mod tests {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use herder_protocol::{PasskeyKey, PersonId};
    use serde_cbor_2::Value;

    use super::{Key, Passkeys};
    use crate::auth::authenticator::Authenticator;

    #[test]
    fn a_registered_key_is_webauthns_credential_id_and_cose_key() {
        let passkeys = Passkeys::new().unwrap();
        let (passkey, registration) =
            Authenticator::register("tomas", &passkeys.creation().unwrap());
        let (_, key) = passkeys.register(&registration).unwrap();
        assert_eq!(key.credential_id, registration.id);
        let decode = |bytes: &[u8]| serde_cbor_2::from_slice::<Value>(bytes).unwrap();
        let ours = decode(&URL_SAFE_NO_PAD.decode(&key.public_key).unwrap());
        assert_eq!(ours, decode(&passkey.cose_key()));

        // The wire carries exactly that, and reads back as the same key.
        let wire = key.wire();
        assert_eq!(Key::from_wire(&wire).unwrap(), Key { counter: 0, ..key });
    }

    #[test]
    fn one_assertion_over_several_daemons_challenges_verifies_once_on_each() {
        let (here, there, elsewhere) = (
            Passkeys::new().unwrap(),
            Passkeys::new().unwrap(),
            Passkeys::new().unwrap(),
        );
        let (passkey, registration) = Authenticator::register("tomas", &here.creation().unwrap());
        let (person_id, key) = here.register(&registration).unwrap();
        let keys = |id: &PersonId| {
            assert_eq!(id, &person_id);
            vec![key.clone()]
        };
        let decode = |passkeys: &Passkeys| {
            URL_SAFE_NO_PAD
                .decode(passkeys.challenge().unwrap().challenge)
                .unwrap()
        };
        let signed = URL_SAFE_NO_PAD.encode([decode(&here), decode(&there)].concat());
        let assertion = passkey.assert_raw(&signed, "https://herder.sh");

        // Only a claim takes a batch.
        assert!(here.verify(&assertion, keys).is_err());
        let signed = URL_SAFE_NO_PAD.encode([decode(&here), decode(&there)].concat());
        let assertion = passkey.assert_raw(&signed, "https://herder.sh");
        assert_eq!(
            here.verify_batch(&assertion, keys).unwrap().person_id,
            person_id
        );
        assert_eq!(
            there.verify_batch(&assertion, keys).unwrap().person_id,
            person_id
        );
        // Each daemon's challenge works once, and a daemon whose challenge is not among them
        // refuses it.
        assert!(here.verify_batch(&assertion, keys).is_err());
        assert!(there.verify_batch(&assertion, keys).is_err());
        decode(&elsewhere);
        assert!(elsewhere.verify_batch(&assertion, keys).is_err());

        // A signature over another batch than the client data names is refused.
        let signed = URL_SAFE_NO_PAD.encode([decode(&here), decode(&there)].concat());
        let mut forged = passkey.assert_raw(&signed, "https://herder.sh");
        forged.response.signature = passkey
            .assert_raw(
                &URL_SAFE_NO_PAD.encode(decode(&elsewhere)),
                "https://herder.sh",
            )
            .response
            .signature;
        assert!(here.verify_batch(&forged, keys).is_err());
    }

    #[test]
    fn a_key_that_is_no_cose_key_is_refused() {
        let id = URL_SAFE_NO_PAD.encode([1; 16]);
        for public_key in ["not base64!", "AAAA", &URL_SAFE_NO_PAD.encode([0xa0])] {
            let key = PasskeyKey {
                credential_id: id.clone(),
                public_key: public_key.to_owned(),
            };
            assert!(Key::from_wire(&key).is_err(), "{public_key}");
        }
    }
}
