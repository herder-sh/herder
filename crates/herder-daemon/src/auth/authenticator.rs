//! A software authenticator for tests, holding the recorded test passkeys of
//! `fixtures/passkeys.json`: it makes registrations and assertions over a daemon's live
//! challenges exactly as a platform authenticator encodes them, so tests never need a real one.
//!
//! Test support only: other crates' tests reach it through the `test-support` feature,
//! which only dev-dependencies turn on, so no release build has it. It panics on bad fixtures,
//! as a test should.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use herder_protocol::{
    AssertionResponse, PASSKEY_RP_ID, PasskeyAssertion, PasskeyChallenge, PasskeyCreation,
    PasskeyRegistration, RegistrationResponse,
};
use ring::digest::{SHA256, digest};
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair};

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
    /// The recorded passkey `name` (`tomas` or `sam`), for the person `user_handle` names.
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
