//! People: who uses a daemon, identified by WebAuthn passkeys for [`PASSKEY_RP_ID`].
//!
//! A person is one identity across every daemon that knows them. A daemon keeps each person's
//! passkey public keys and their role on it; the private keys never leave the person's devices.
//! A device the daemon does not know signs in by setting [`crate::ClientHello::sign_in`]: the
//! daemon answers `sign_in_challenge`, and the device answers with a [`SignIn`]. Owner-only
//! people commands, opening or attaching a terminal and every change to secrets carry an
//! assertion over a fresh `passkey_challenge` when the caller is a person.
//!
//! Passkey messages follow WebAuthn's JSON encoding (`PublicKeyCredential.toJSON()`): camelCase
//! names, and binary values as base64url without padding.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{PersonId, Role, Timestamp, UserDevice};

/// The WebAuthn relying party every herder passkey belongs to.
pub const PASSKEY_RP_ID: &str = "herder.sh";

/// Seconds an assertion over a `passkey_challenge` stays acceptable.
pub const PASSKEY_CHALLENGE_SECS: u64 = 60;

/// How a device the daemon does not know signs in, answering `sign_in_challenge`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum SignIn {
    /// As a person the daemon knows, with an assertion over the challenge by one of their
    /// passkeys. The device is listed under that person from then on.
    Passkey {
        /// The assertion.
        assertion: PasskeyAssertion,
    },
    /// Become the first owner of a daemon that has none, with the code `herder install`
    /// printed; the window closes after ten minutes or once it has an owner. Refused with
    /// `forbidden` for a wrong or expired code.
    ClaimOwnership {
        /// The code, as the terminal shows it; case and separators do not matter.
        code: String,
        /// Who claims it.
        person: NewPerson,
    },
    /// Join with an invite an owner minted, as the role it names.
    AcceptInvite {
        /// The invite token.
        invite: String,
        /// Who joins.
        person: NewPerson,
    },
    /// Sign in a device that holds no passkey, such as a terminal: the daemon answers
    /// `login_request` with a code, and the device waits, for up to ten minutes, until an app
    /// of the person approves it with `approve_login`.
    StartLogin,
}

/// A person introduced to a daemon by a sign-in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NewPerson {
    /// How others see them; 1 to 64 printable characters. A person the daemon already knows
    /// keeps the name it has.
    pub name: String,
    /// Their passkey.
    pub passkey: PersonPasskey,
}

/// The passkey a [`NewPerson`] proves they hold.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PersonPasskey {
    /// A passkey created just now from the challenge's `registration`; the person is the one
    /// it names.
    Register {
        /// The new credential.
        registration: PasskeyRegistration,
    },
    /// A passkey the person created before, as another daemon lists it, with an assertion by
    /// it over the challenge.
    Existing {
        /// The person.
        person_id: PersonId,
        /// Its public key, as `list_people` gave it.
        key: PasskeyKey,
        /// The assertion.
        assertion: PasskeyAssertion,
    },
}

/// What a passkey signs: the arguments of the platform's assertion request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PasskeyChallenge {
    /// Relying party; always [`PASSKEY_RP_ID`].
    pub rp_id: String,
    /// The challenge, base64url.
    pub challenge: String,
    /// When the daemon stops accepting an assertion over it.
    pub expires_at: Timestamp,
}

/// What a new passkey is created from: the arguments of the platform's registration request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PasskeyCreation {
    /// Relying party; always [`PASSKEY_RP_ID`].
    pub rp_id: String,
    /// The challenge, base64url.
    pub challenge: String,
    /// The person the passkey will identify, picked by the daemon.
    pub person_id: PersonId,
    /// The WebAuthn user handle for `person_id`, base64url: the UUID's 16 bytes.
    pub user_handle: String,
    /// When the daemon stops accepting a registration from it.
    pub expires_at: Timestamp,
}

/// A WebAuthn assertion, as `PublicKeyCredential.toJSON()` encodes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyAssertion {
    /// The credential id, base64url.
    pub id: String,
    /// The same, base64url.
    pub raw_id: String,
    /// What the authenticator signed.
    pub response: AssertionResponse,
    /// Always `public-key`.
    #[serde(rename = "type")]
    pub kind: String,
}

/// The authenticator's part of a [`PasskeyAssertion`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssertionResponse {
    /// The client data, base64url.
    #[serde(rename = "clientDataJSON")]
    pub client_data_json: String,
    /// The authenticator data, base64url.
    pub authenticator_data: String,
    /// The signature, base64url.
    pub signature: String,
    /// The user handle, base64url; a passkey always gives it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_handle: Option<String>,
}

/// A new WebAuthn credential, as `PublicKeyCredential.toJSON()` encodes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyRegistration {
    /// The credential id, base64url.
    pub id: String,
    /// The same, base64url.
    pub raw_id: String,
    /// What the authenticator made.
    pub response: RegistrationResponse,
    /// Always `public-key`.
    #[serde(rename = "type")]
    pub kind: String,
}

/// The authenticator's part of a [`PasskeyRegistration`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationResponse {
    /// The client data, base64url.
    #[serde(rename = "clientDataJSON")]
    pub client_data_json: String,
    /// The attestation object, base64url.
    pub attestation_object: String,
}

/// A passkey's public key, as WebAuthn defines it, to introduce its person to another daemon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PasskeyKey {
    /// The credential id, base64url.
    pub credential_id: String,
    /// The credential public key, a COSE_Key as the authenticator data carries it, base64url.
    pub public_key: String,
}

/// A person to add to a daemon that does not know them yet, as another daemon lists them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PersonCard {
    /// How others see them.
    pub name: String,
    /// Their passkeys' public keys; at least one.
    pub passkeys: Vec<PasskeyKey>,
}

/// A person on a daemon, answering `list_people`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PersonInfo {
    /// The person.
    pub person_id: PersonId,
    /// How others see them.
    pub name: String,
    /// Their role on this daemon.
    pub role: Role,
    /// Their passkeys' public keys.
    pub passkeys: Vec<PasskeyKey>,
    /// Their devices on this daemon, oldest first.
    pub devices: Vec<UserDevice>,
}

/// What becomes of a user `claimed` left over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LeftoverDecision {
    /// They are the caller: their history reads as the caller's person.
    ItsMe,
    /// They are removed, with their devices.
    Remove,
}
