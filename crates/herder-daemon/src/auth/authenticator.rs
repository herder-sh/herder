//! A software authenticator for tests, holding the recorded test passkeys of
//! `fixtures/passkeys.json`: it makes registrations and assertions over a daemon's live
//! challenges exactly as a platform authenticator encodes them, so tests never need a real one.
//!
//! Test support only: other crates' tests reach it through the `test-authenticator` feature,
//! which only dev-dependencies turn on, so no release build has it. It panics on bad fixtures,
//! as a test should.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use herder_protocol::{
    AssertionResponse, NewPerson, PASSKEY_RP_ID, PasskeyAssertion, PasskeyChallenge,
    PasskeyCreation, PasskeyRegistration, PersonPasskey, RegistrationResponse, Role, SignIn,
};
use ring::digest::{SHA256, digest};
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair};

use herder_client_core::{Client, Machine};
use tokio_util::sync::CancellationToken;

use super::Auth;
use crate::ws::Identity;

const FIXTURES: &str = include_str!("fixtures/passkeys.json");

/// User present, user verified, backup eligible and backed up: a synced passkey.
const FLAGS: u8 = 0x01 | 0x04 | 0x08 | 0x10;

/// Attested credential data follows.
const ATTESTED: u8 = 0x40;

/// One recorded passkey.
pub struct Authenticator {
    credential_id: Vec<u8>,
    key: EcdsaKeyPair,
    /// The person the passkey was created for, once registered.
    pub user_handle: Vec<u8>,
}

impl Authenticator {
    /// The recorded passkey `name` (`tomas`, `sam`, `alice`, `bob`, `carol` or `guest`), for
    /// the person `user_handle` names.
    pub fn new(name: &str, user_handle: &[u8]) -> Self {
        let fixtures: serde_json::Value = serde_json::from_str(FIXTURES).unwrap();
        let fixture = &fixtures[name];
        let decode = |field: &str| {
            URL_SAFE_NO_PAD
                .decode(fixture[field].as_str().unwrap())
                .unwrap()
        };
        let key = EcdsaKeyPair::from_pkcs8(
            &ECDSA_P256_SHA256_ASN1_SIGNING,
            &decode("pkcs8"),
            &SystemRandom::new(),
        )
        .unwrap();
        Self {
            credential_id: decode("credential_id"),
            key,
            user_handle: user_handle.to_vec(),
        }
    }

    /// Creates the passkey from `creation`, for the person it names.
    pub fn register(name: &str, creation: &PasskeyCreation) -> (Self, PasskeyRegistration) {
        let handle = URL_SAFE_NO_PAD.decode(&creation.user_handle).unwrap();
        let authenticator = Self::new(name, &handle);
        let client_data = client_data("webauthn.create", &creation.challenge);
        let mut auth_data = authenticator.auth_data(FLAGS | ATTESTED);
        auth_data.extend([0; 16]); // No AAGUID.
        let id_len = u16::try_from(authenticator.credential_id.len()).unwrap();
        auth_data.extend(id_len.to_be_bytes());
        auth_data.extend(&authenticator.credential_id);
        auth_data.extend(authenticator.cose_key());
        let mut attestation = head(5, 3);
        attestation.extend(text("fmt"));
        attestation.extend(text("none"));
        attestation.extend(text("attStmt"));
        attestation.extend(head(5, 0));
        attestation.extend(text("authData"));
        attestation.extend(bytes(&auth_data));
        let id = URL_SAFE_NO_PAD.encode(&authenticator.credential_id);
        let registration = PasskeyRegistration {
            id: id.clone(),
            raw_id: id,
            response: RegistrationResponse {
                client_data_json: URL_SAFE_NO_PAD.encode(client_data),
                attestation_object: URL_SAFE_NO_PAD.encode(attestation),
            },
            kind: "public-key".to_owned(),
        };
        (authenticator, registration)
    }

    /// Signs the device with certificate `fingerprint` in to `auth` as this passkey's person,
    /// with a fresh assertion.
    pub fn sign_in(&self, auth: &Auth, fingerprint: &str) -> Identity {
        let assertion = self.assert(&auth.challenge().unwrap());
        auth.sign_in(
            fingerprint,
            "test",
            SignIn::Passkey { assertion },
            &CancellationToken::new(),
        )
        .unwrap()
    }

    /// Signs `challenge`.
    pub fn assert(&self, challenge: &PasskeyChallenge) -> PasskeyAssertion {
        self.assert_raw(&challenge.challenge, "https://herder.sh")
    }

    /// Signs the base64url `challenge` as if for `origin`.
    pub fn assert_raw(&self, challenge: &str, origin: &str) -> PasskeyAssertion {
        let client_data = serde_json::to_vec(&serde_json::json!({
            "type": "webauthn.get",
            "challenge": challenge,
            "origin": origin,
            "crossOrigin": false,
        }))
        .unwrap();
        let auth_data = self.auth_data(FLAGS);
        let mut signed = auth_data.clone();
        signed.extend(digest(&SHA256, &client_data).as_ref());
        let signature = self.key.sign(&SystemRandom::new(), &signed).unwrap();
        let id = URL_SAFE_NO_PAD.encode(&self.credential_id);
        PasskeyAssertion {
            id: id.clone(),
            raw_id: id,
            response: AssertionResponse {
                client_data_json: URL_SAFE_NO_PAD.encode(client_data),
                authenticator_data: URL_SAFE_NO_PAD.encode(auth_data),
                signature: URL_SAFE_NO_PAD.encode(signature.as_ref()),
                user_handle: Some(URL_SAFE_NO_PAD.encode(&self.user_handle)),
            },
            kind: "public-key".to_owned(),
        }
    }

    /// The RP id hash, `flags` and a zero signature counter, as synced passkeys keep it.
    fn auth_data(&self, flags: u8) -> Vec<u8> {
        let mut data = digest(&SHA256, PASSKEY_RP_ID.as_bytes()).as_ref().to_vec();
        data.push(flags);
        data.extend([0; 4]);
        data
    }

    /// The public key as a COSE_Key: EC2, ES256, P-256, x, y.
    fn cose_key(&self) -> Vec<u8> {
        // An uncompressed point: 0x04, x, y.
        let point = self.key.public_key().as_ref();
        let mut key = head(5, 5);
        key.extend([0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21]);
        key.extend(bytes(&point[1..33]));
        key.push(0x22);
        key.extend(bytes(&point[33..65]));
        key
    }
}

/// A way in for a new person: the first owner's claim of a daemon with no owner, or an invite
/// to one that has an owner.
pub struct Join {
    code: String,
    claim: bool,
}

impl Join {
    /// Opens the way in to `auth` for a person with `role`. Open it before the device asks for
    /// its sign-in challenge, so the challenge offers a passkey to create.
    pub fn open(auth: &Auth, role: Role) -> Self {
        if role == Role::Owner && auth.owner().is_none() {
            return Self::claim(auth.open_claim().unwrap().0);
        }
        Self::invite(auth.invite(role).unwrap().0)
    }

    /// The claim of a daemon with no owner, by the code `herder service install` showed.
    pub fn claim(code: String) -> Self {
        Self { code, claim: true }
    }

    /// An invite, by its code.
    pub fn invite(code: String) -> Self {
        Self { code, claim: false }
    }

    /// Signs `client` in to the daemon at `hosts` with certificate `fingerprint` as the new
    /// person `name`, with the recorded passkey `name`: what a test does where a device paired
    /// with a code.
    pub async fn join(
        self,
        client: &Client,
        hosts: Vec<String>,
        fingerprint: &str,
        name: &str,
    ) -> (Authenticator, Machine) {
        let pending = client.sign_in(hosts, fingerprint.to_owned()).await.unwrap();
        let (passkey, sign_in) = self.sign_in(name, &pending.registration().unwrap());
        (passkey, pending.finish(sign_in).await.unwrap())
    }

    /// How the device signs in: creating the recorded passkey `name` from `creation`, for the
    /// person `name`.
    pub fn sign_in(self, name: &str, creation: &PasskeyCreation) -> (Authenticator, SignIn) {
        let (passkey, registration) = Authenticator::register(name, creation);
        let person = NewPerson {
            name: name.to_owned(),
            passkey: PersonPasskey::Register { registration },
        };
        let sign_in = match self.claim {
            true => SignIn::ClaimOwnership {
                code: self.code,
                person,
            },
            false => SignIn::AcceptInvite {
                invite: self.code,
                person,
            },
        };
        (passkey, sign_in)
    }
}

/// Signs the device with certificate `fingerprint` in to `auth` as the new person `name`, with
/// `role` and the recorded passkey `name`: how a test puts someone on a daemon without a client.
pub fn enroll(auth: &Auth, name: &str, role: Role, fingerprint: &str) -> (Authenticator, Identity) {
    let join = Join::open(auth, role);
    let (_, creation) = auth.sign_in_challenge().unwrap();
    let (passkey, sign_in) = join.sign_in(name, &creation.unwrap());
    let identity = auth
        .sign_in(fingerprint, "test", sign_in, &CancellationToken::new())
        .unwrap();
    (passkey, identity)
}

fn client_data(kind: &str, challenge: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "type": kind,
        "challenge": challenge,
        "origin": "https://herder.sh",
        "crossOrigin": false,
    }))
    .unwrap()
}

/// A CBOR head of `major` type with argument `len`.
fn head(major: u8, len: usize) -> Vec<u8> {
    match u8::try_from(len) {
        Ok(len) if len < 24 => vec![major << 5 | len],
        Ok(len) => vec![major << 5 | 24, len],
        Err(_) => {
            let mut head = vec![major << 5 | 25];
            head.extend(u16::try_from(len).unwrap().to_be_bytes());
            head
        }
    }
}

fn bytes(value: &[u8]) -> Vec<u8> {
    let mut encoded = head(2, value.len());
    encoded.extend(value);
    encoded
}

fn text(value: &str) -> Vec<u8> {
    let mut encoded = head(3, value.len());
    encoded.extend(value.as_bytes());
    encoded
}
