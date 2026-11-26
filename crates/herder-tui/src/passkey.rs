//! The terminal's own passkey, for someone with no herder app: a P-256 key for herder.sh that
//! `herder login` creates when it claims a machine, kept in the client profile as
//! `passkey.json`, and that the TUI signs with wherever an app signs with Face ID.
//!
//! The private key is sealed under a passphrase: Argon2id stretches it into a key, and
//! ChaCha20-Poly1305 seals the PKCS#8 key under that. A copy of the file is useless without
//! the passphrase, and a wrong one fails the seal's check rather than giving a wrong key. A
//! passphrase works on every Linux machine, a server over SSH included, where a desktop
//! keyring (Secret Service) does not run; and a keyring would hand the key to any program of
//! the user's session without asking them.
//!
//! The passphrase is asked for every signature and the key is dropped after it, so each
//! assertion says what its flags claim: the user was present and verified just now. Like the
//! Mac's Secure Enclave passkey it is neither backup eligible nor backed up: it lives in this
//! file only.

use std::future::Future;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use herder_client_core::{Assertion, Client, Error, Machine};
use herder_protocol::{
    AssertionResponse, CommandBody, CommandResult, HostId, NewPerson, PASSKEY_RP_ID,
    PasskeyAssertion, PasskeyChallenge, PasskeyCreation, PasskeyKey, PasskeyRegistration, PersonId,
    PersonPasskey, RegistrationResponse, Role, SignIn,
};
use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey};
use ring::digest::{SHA256, digest};
use ring::rand::{SystemRandom, generate};
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair as _};
use serde::{Deserialize, Serialize};

/// The file in the client profile.
const FILE: &str = "passkey.json";

/// The origin the daemon expects assertions to be made for.
const ORIGIN: &str = "https://herder.sh";

/// User present and user verified, in the authenticator data's flags: the passphrase was
/// typed for this signature. Not backup eligible: the key is in one file.
const FLAGS: u8 = 0x01 | 0x04;

/// Attested credential data follows.
const ATTESTED: u8 = 0x40;

/// Argon2id's memory, in KiB, and passes: RFC 9106's choice for where 2 GiB is too much.
const KDF_MEMORY_KIB: u32 = 64 * 1024;
const KDF_PASSES: u32 = 3;

/// The shortest passphrase a new passkey takes.
pub const MIN_PASSPHRASE: usize = 8;

/// The terminal's passkey as its file keeps it: who it names, its public key, and the
/// private key sealed under the passphrase.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalPasskey {
    /// The person it names, on every machine it is added to.
    person_id: PersonId,
    /// How that person is called.
    name: String,
    /// The credential id, base64url.
    credential_id: String,
    /// The public key, a COSE_Key, base64url.
    public_key: String,
    /// Argon2id's salt, base64url.
    salt: String,
    /// The seal's nonce, base64url.
    nonce: String,
    /// The PKCS#8 private key, sealed, base64url.
    sealed: String,
}

/// The passkey with its private key, for as long as one signature takes.
pub struct Signer {
    credential_id: String,
    user_handle: String,
    key: EcdsaKeyPair,
}

impl LocalPasskey {
    /// The passkey kept in the profile `config_dir`; `None` when this terminal has none.
    pub fn load(config_dir: &Path) -> Result<Option<Self>> {
        let path = config_dir.join(FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        serde_json::from_str(&text)
            .map(Some)
            .with_context(|| format!("reading {}", path.display()))
    }

    /// Makes a passkey for the person `creation` names, called `name`, sealed under
    /// `passphrase`, with its registration for the daemon that asked. Nothing is saved yet.
    pub fn create(
        creation: &PasskeyCreation,
        name: &str,
        passphrase: &str,
    ) -> Result<(Self, PasskeyRegistration)> {
        if passphrase.chars().count() < MIN_PASSPHRASE {
            bail!("a passphrase has at least {MIN_PASSPHRASE} characters");
        }
        let random = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &random)
            .map_err(|_| anyhow!("cannot generate a key"))?;
        let key = key_pair(pkcs8.as_ref())?;
        let cose = cose_key(&key);
        // The public key's hash, so the id stays with the key.
        let hash = digest(&SHA256, key.public_key().as_ref());
        let raw_id = &hash.as_ref()[..16];
        let credential_id = URL_SAFE_NO_PAD.encode(raw_id);

        // Both fresh from the system for every passkey.
        let unread = |_| anyhow!("cannot read random bytes");
        let salt: [u8; 16] = generate(&random).map_err(unread)?.expose();
        let nonce: [u8; 12] = generate(&random).map_err(unread)?.expose();
        let mut sealed = pkcs8.as_ref().to_vec();
        sealing_key(passphrase, &salt)?
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(credential_id.as_bytes()),
                &mut sealed,
            )
            .map_err(|_| anyhow!("cannot seal the key"))?;

        let mut auth_data = auth_data(FLAGS | ATTESTED);
        auth_data.extend([0; 16]); // No AAGUID: no password manager holds it.
        // 16 bytes, so its length fits.
        auth_data.extend([0, 16]);
        auth_data.extend(raw_id);
        auth_data.extend(&cose);
        let mut attestation = head(5, 3);
        attestation.extend(text("fmt"));
        attestation.extend(text("none"));
        attestation.extend(text("attStmt"));
        attestation.extend(head(5, 0));
        attestation.extend(text("authData"));
        attestation.extend(bytes(&auth_data));
        let registration = PasskeyRegistration {
            id: credential_id.clone(),
            raw_id: credential_id.clone(),
            response: RegistrationResponse {
                client_data_json: URL_SAFE_NO_PAD
                    .encode(client_data("webauthn.create", &creation.challenge)?),
                attestation_object: URL_SAFE_NO_PAD.encode(attestation),
            },
            kind: "public-key".to_owned(),
        };
        let passkey = Self {
            person_id: creation.person_id.clone(),
            name: name.to_owned(),
            credential_id,
            public_key: URL_SAFE_NO_PAD.encode(cose),
            salt: URL_SAFE_NO_PAD.encode(salt),
            nonce: URL_SAFE_NO_PAD.encode(nonce),
            sealed: URL_SAFE_NO_PAD.encode(sealed),
        };
        Ok((passkey, registration))
    }

    /// Keeps the passkey in the profile `config_dir`, readable by the user only. Refuses to
    /// replace one: a machine that knows the person by the old key would lock them out.
    pub fn save(&self, config_dir: &Path) -> Result<PathBuf> {
        std::fs::create_dir_all(config_dir)
            .with_context(|| format!("creating {}", config_dir.display()))?;
        let path = config_dir.join(FILE);
        let text = serde_json::to_string_pretty(self).context("encoding the passkey")?;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .and_then(|mut file| {
                file.write_all(text.as_bytes())?;
                file.sync_all()
            })
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }

    /// Opens the private key with `passphrase`, to sign once. Stretching the passphrase takes
    /// a moment: call it off the async runtime's threads.
    pub fn unlock(&self, passphrase: &str) -> Result<Signer> {
        let salt = decode(&self.salt)?;
        let nonce: [u8; 12] = decode(&self.nonce)?
            .try_into()
            .map_err(|_| anyhow!("the passkey file is damaged"))?;
        let mut sealed = decode(&self.sealed)?;
        let pkcs8 = sealing_key(passphrase, &salt)?
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(self.credential_id.as_bytes()),
                &mut sealed,
            )
            .map_err(|_| anyhow!("wrong passphrase"))?;
        let handle = uuid_bytes(self.person_id.as_str())
            .ok_or_else(|| anyhow!("the passkey file is damaged"))?;
        Ok(Signer {
            credential_id: self.credential_id.clone(),
            user_handle: URL_SAFE_NO_PAD.encode(handle),
            key: key_pair(pkcs8)?,
        })
    }

    /// The person the passkey names.
    pub fn person_id(&self) -> &PersonId {
        &self.person_id
    }

    /// How that person is called.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The public key, as `list_people` lists it: what introduces the person to a machine
    /// that does not know them yet.
    pub fn key(&self) -> PasskeyKey {
        PasskeyKey {
            credential_id: self.credential_id.clone(),
            public_key: self.public_key.clone(),
            aaguid: None,
            added_at: None,
        }
    }
}

impl Signer {
    /// Signs `challenge`, as a platform authenticator encodes it. Refused when the challenge
    /// allows only other passkeys.
    pub fn assert(&self, challenge: &PasskeyChallenge) -> Result<PasskeyAssertion> {
        if !challenge.allow_credentials.is_empty()
            && !challenge.allow_credentials.contains(&self.credential_id)
        {
            bail!("the machine asks for another passkey than this terminal's");
        }
        let client_data = client_data("webauthn.get", &challenge.challenge)?;
        let auth_data = auth_data(FLAGS);
        let mut signed = auth_data.clone();
        signed.extend(digest(&SHA256, &client_data).as_ref());
        let signature = self
            .key
            .sign(&SystemRandom::new(), &signed)
            .map_err(|_| anyhow!("cannot sign"))?;
        Ok(PasskeyAssertion {
            id: self.credential_id.clone(),
            raw_id: self.credential_id.clone(),
            response: AssertionResponse {
                client_data_json: URL_SAFE_NO_PAD.encode(client_data),
                authenticator_data: URL_SAFE_NO_PAD.encode(auth_data),
                signature: URL_SAFE_NO_PAD.encode(signature.as_ref()),
                user_handle: Some(self.user_handle.clone()),
            },
            kind: "public-key".to_owned(),
        })
    }
}

/// The key that seals the private key: `passphrase` stretched with Argon2id over `salt`.
fn sealing_key(passphrase: &str, salt: &[u8]) -> Result<LessSafeKey> {
    let params = argon2::Params::new(KDF_MEMORY_KIB, KDF_PASSES, 1, Some(32))
        .map_err(|err| anyhow!("argon2: {err}"))?;
    let mut key = [0u8; 32];
    argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|err| anyhow!("the passkey file is damaged: {err}"))?;
    let key = UnboundKey::new(&CHACHA20_POLY1305, &key).map_err(|_| anyhow!("cannot seal"))?;
    Ok(LessSafeKey::new(key))
}

fn key_pair(pkcs8: &[u8]) -> Result<EcdsaKeyPair> {
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8, &SystemRandom::new())
        .map_err(|_| anyhow!("the passkey file is damaged"))
}

fn decode(field: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(field)
        .map_err(|_| anyhow!("the passkey file is damaged"))
}

/// The 16 bytes of a hyphenated UUID, as a person id is one: the WebAuthn user handle.
fn uuid_bytes(id: &str) -> Option<Vec<u8>> {
    let hex: Vec<u8> = id.bytes().filter(|byte| *byte != b'-').collect();
    if hex.len() != 32 {
        return None;
    }
    hex.chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

fn client_data(kind: &str, challenge: &str) -> Result<Vec<u8>> {
    serde_json::to_vec(&serde_json::json!({
        "type": kind,
        "challenge": challenge,
        "origin": ORIGIN,
        "crossOrigin": false,
    }))
    .context("encoding the client data")
}

/// The RP id hash, `flags` and a zero signature counter.
fn auth_data(flags: u8) -> Vec<u8> {
    let mut data = digest(&SHA256, PASSKEY_RP_ID.as_bytes()).as_ref().to_vec();
    data.push(flags);
    data.extend([0; 4]);
    data
}

/// The public key as a COSE_Key: EC2, ES256, P-256, x, y.
fn cose_key(key: &EcdsaKeyPair) -> Vec<u8> {
    // An uncompressed point: 0x04, x, y.
    let point = key.public_key().as_ref();
    let mut cose = head(5, 5);
    cose.extend([0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21]);
    cose.extend(bytes(&point[1..33]));
    cose.push(0x22);
    cose.extend(bytes(&point[33..65]));
    cose
}

/// A CBOR head of `major` type with argument `len`, which is under 65536 for all a passkey
/// encodes.
fn head(major: u8, len: usize) -> Vec<u8> {
    match len {
        0..24 => vec![major << 5 | len as u8],
        24..256 => vec![major << 5 | 24, len as u8],
        _ => vec![major << 5 | 25, (len >> 8) as u8, len as u8],
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

/// Asks the user for the passphrase, once: `None` when they cancel. The argument says what
/// it is for.
pub type Ask =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync>;

/// The terminal's passkey as a client's authenticator: every signature asks the user for
/// the passphrase, as an app asks for Face ID.
#[derive(Clone)]
pub struct Prompted {
    passkey: LocalPasskey,
    ask: Ask,
}

impl Prompted {
    /// `passkey`, asking for its passphrase with `ask`.
    pub fn new(passkey: LocalPasskey, ask: Ask) -> Self {
        Self { passkey, ask }
    }

    /// The passkey.
    pub fn passkey(&self) -> &LocalPasskey {
        &self.passkey
    }

    /// Asks for the passphrase and signs `challenge`, which is `what` it approves.
    pub async fn sign(
        &self,
        what: String,
        challenge: PasskeyChallenge,
    ) -> Result<PasskeyAssertion> {
        let Some(passphrase) = (self.ask)(what).await else {
            bail!("cancelled");
        };
        let passkey = self.passkey.clone();
        // Argon2id is slow on purpose.
        tokio::task::spawn_blocking(move || passkey.unlock(&passphrase)?.assert(&challenge))
            .await
            .context("unlocking the passkey")?
    }

    /// Signs this device in to a machine that knows the passkey's person, as an app signs in
    /// with Face ID.
    pub async fn sign_in(
        &self,
        client: &Client,
        hosts: Vec<String>,
        fingerprint: String,
    ) -> Result<Machine> {
        let pending = client.sign_in(hosts, fingerprint, Vec::new()).await?;
        let what = "sign this terminal in".to_owned();
        let assertion = self.sign(what, pending.challenge()).await?;
        Ok(pending.finish(SignIn::Passkey { assertion }).await?)
    }

    /// Signs a fresh challenge of `host_id`, for a command that carries its assertion.
    async fn fresh(
        &self,
        client: &Client,
        host_id: &HostId,
        what: String,
    ) -> Result<PasskeyAssertion> {
        let challenge = match client
            .send(host_id.clone(), CommandBody::PasskeyChallenge)
            .await?
        {
            CommandResult::PasskeyChallenge { challenge, .. } => challenge,
            other => bail!("the machine answered a passkey challenge with {other:?}"),
        };
        self.sign(what, challenge).await
    }
}

impl herder_client_core::Authenticator for Prompted {
    fn assert(&self, _host_id: HostId, challenge: PasskeyChallenge) -> Assertion {
        let prompted = self.clone();
        Box::pin(async move {
            prompted
                .sign("approve".to_owned(), challenge)
                .await
                .map_err(|err| Error::Local {
                    message: format!("{err:#}"),
                })
        })
    }
}

/// How a new person gets on a machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Way {
    /// As its first owner, with the code `herder claim` shows.
    Claim(String),
    /// With an invite an owner minted.
    Invite(String),
}

/// What signing in takes from the user: the passphrase of the passkey this terminal has, or
/// the name and passphrase for the one it makes.
pub enum Secret<'a> {
    /// This terminal's passkey, and its passphrase.
    Existing(&'a LocalPasskey, &'a str),
    /// No passkey yet: one is made for `name` under `passphrase`, and saved in `config_dir`.
    New {
        /// How the person is called.
        name: &'a str,
        /// What seals the new key.
        passphrase: &'a str,
        /// The client profile.
        config_dir: &'a Path,
    },
}

/// Puts this terminal's person on the machine at `hosts` with certificate `fingerprint`,
/// `way`: with the passkey the terminal has, or one made now. The device is theirs from then
/// on. A new passkey is saved before the machine hears of it, so a claim never leaves a
/// machine owned by a key that was lost.
pub async fn join(
    client: &Client,
    hosts: Vec<String>,
    fingerprint: String,
    way: Way,
    secret: Secret<'_>,
) -> Result<Machine> {
    let pending = client.sign_in(hosts, fingerprint, Vec::new()).await?;
    let passkey = match secret {
        Secret::Existing(passkey, passphrase) => {
            let (locked, passphrase) = (passkey.clone(), passphrase.to_owned());
            let challenge = pending.challenge();
            let assertion =
                tokio::task::spawn_blocking(move || locked.unlock(&passphrase)?.assert(&challenge))
                    .await
                    .context("unlocking the passkey")??;
            NewPerson {
                name: passkey.name.clone(),
                passkey: PersonPasskey::Existing {
                    person_id: passkey.person_id.clone(),
                    key: passkey.key(),
                    assertion,
                },
            }
        }
        Secret::New {
            name,
            passphrase,
            config_dir,
        } => {
            let Some(creation) = pending.registration() else {
                bail!(match way {
                    Way::Claim(_) =>
                        "the machine is not waiting for an owner; run `herder claim` on it",
                    Way::Invite(_) => "the machine takes no invite",
                });
            };
            let (passkey, registration) = LocalPasskey::create(&creation, name, passphrase)?;
            passkey.save(config_dir)?;
            NewPerson {
                name: name.to_owned(),
                passkey: PersonPasskey::Register { registration },
            }
        }
    };
    let sign_in = match way {
        Way::Claim(code) => SignIn::ClaimOwnership {
            code,
            person: passkey,
        },
        Way::Invite(invite) => SignIn::AcceptInvite {
            invite,
            person: passkey,
        },
    };
    Ok(pending.finish(sign_in).await?)
}

/// Signs this device in to a machine that knows the passkey's person, as an app signs in
/// with Face ID.
pub async fn sign_in(
    client: &Client,
    hosts: Vec<String>,
    fingerprint: String,
    passkey: &LocalPasskey,
    passphrase: &str,
) -> Result<Machine> {
    let pending = client.sign_in(hosts, fingerprint, Vec::new()).await?;
    let (locked, passphrase) = (passkey.clone(), passphrase.to_owned());
    let challenge = pending.challenge();
    let assertion =
        tokio::task::spawn_blocking(move || locked.unlock(&passphrase)?.assert(&challenge))
            .await
            .context("unlocking the passkey")??;
    Ok(pending.finish(SignIn::Passkey { assertion }).await?)
}

/// What the person does with their passkey beyond their own commands: what an app's People
/// and approval screens do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersonOp {
    /// Approve the `herder login` that shows this code: that terminal signs in as the person.
    ApproveLogin(String),
    /// Approve what a device of the person without a passkey asks to do.
    ApproveAction(String),
    /// Refuse it.
    RefuseAction(String),
    /// Mint an invite for someone to join with this role.
    Invite(Role),
    /// List the machine's people.
    People,
    /// Give the person called this a role.
    SetRole(String, Role),
    /// Remove the person called this, with their devices.
    Remove(String),
}

/// Does `op` on the machine `host_id` and says what happened, for the status line; an
/// invite's `herder://invite` link comes second, to copy.
pub async fn person_op(
    client: &Client,
    passkey: &Prompted,
    host_id: &HostId,
    op: PersonOp,
) -> Result<(String, Option<String>)> {
    let send = |body| client.send(host_id.clone(), body);
    let applied = |text: String| Ok((text, None));
    match op {
        PersonOp::ApproveLogin(code) => {
            let assertion = passkey
                .fresh(
                    client,
                    host_id,
                    format!("sign in the terminal showing {code}"),
                )
                .await?;
            send(CommandBody::ApproveLogin {
                code,
                assertion: Box::new(assertion),
            })
            .await?;
            applied("signed that terminal in as you".to_owned())
        }
        PersonOp::ApproveAction(request_id) => {
            // The client asks the authenticator, which is `passkey`.
            client.approve_action(host_id.clone(), request_id).await?;
            applied("approved".to_owned())
        }
        PersonOp::RefuseAction(request_id) => {
            client.refuse_action(host_id.clone(), request_id).await?;
            applied("refused".to_owned())
        }
        PersonOp::Invite(role) => {
            let what = format!("invite {}", article(role));
            let assertion = passkey.fresh(client, host_id, what).await?;
            let result = send(CommandBody::Invite {
                role,
                assertion: Box::new(assertion),
            })
            .await?;
            let CommandResult::Invite {
                invite,
                fingerprint,
                addresses,
                ..
            } = result
            else {
                bail!("the machine answered the invite with {result:?}");
            };
            let name = client
                .machines()
                .into_iter()
                .find(|machine| machine.host_id == *host_id)
                .map(|machine| machine.name);
            let link = InviteLink {
                by: Some(passkey.passkey.name.clone()),
                role,
                machines: vec![InviteMachine {
                    hosts: addresses,
                    fingerprint,
                    invite,
                    name,
                }],
            }
            .to_string();
            let said = format!(
                "copied the invite link for {}, to open in the app or with `herder login`",
                article(role)
            );
            Ok((said, Some(link)))
        }
        PersonOp::People => {
            let people = people(client, host_id).await?;
            let listed: Vec<String> = people
                .iter()
                .map(|person| format!("{} ({})", person.name, role_name(person.role)))
                .collect();
            applied(listed.join(", "))
        }
        PersonOp::SetRole(name, role) => {
            let person_id = person_called(client, host_id, &name).await?;
            let what = format!("make {name} {}", article(role));
            let assertion = passkey.fresh(client, host_id, what).await?;
            send(CommandBody::SetPersonRole {
                person_id,
                role,
                person: None,
                assertion: Box::new(assertion),
            })
            .await?;
            applied(format!("{name} is {} now", article(role)))
        }
        PersonOp::Remove(name) => {
            let person_id = person_called(client, host_id, &name).await?;
            let assertion = passkey
                .fresh(client, host_id, format!("remove {name}"))
                .await?;
            send(CommandBody::RevokePerson {
                person_id,
                assertion: Box::new(assertion),
            })
            .await?;
            applied(format!("removed {name} and their devices"))
        }
    }
}

async fn people(client: &Client, host_id: &HostId) -> Result<Vec<herder_protocol::PersonInfo>> {
    match client
        .send(host_id.clone(), CommandBody::ListPeople)
        .await?
    {
        CommandResult::People { people, .. } => Ok(people),
        other => bail!("the machine answered the people list with {other:?}"),
    }
}

/// The one person of the machine called `name`, whatever its case.
async fn person_called(client: &Client, host_id: &HostId, name: &str) -> Result<PersonId> {
    let people = people(client, host_id).await?;
    let mut found = people
        .iter()
        .filter(|person| person.name.eq_ignore_ascii_case(name));
    match (found.next(), found.next()) {
        (Some(person), None) => Ok(person.person_id.clone()),
        (Some(_), Some(_)) => bail!("more than one person is called {name}"),
        _ => {
            let names: Vec<&str> = people.iter().map(|person| person.name.as_str()).collect();
            bail!("nobody is called {name}; there is {}", names.join(", "))
        }
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Owner => "owner",
        Role::Member => "member",
    }
}

fn article(role: Role) -> &'static str {
    match role {
        Role::Owner => "an owner",
        Role::Member => "a member",
    }
}

/// A `herder://invite` link, as the apps make and open it: who invites, as what, and each
/// machine's own one-time invite with where it is and how to recognise it.
///
/// `herder://invite?by=Tomas&role=member&host=…&fp=…&invite=…&name=devbox&host=…`: each
/// machine is its `host`s, then its `fp`, `invite` and optional `name`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InviteLink {
    /// Who invites, when the link says.
    pub by: Option<String>,
    /// The role the link shows; each machine decides with its invite.
    pub role: Role,
    /// The machines, at least one.
    pub machines: Vec<InviteMachine>,
}

/// One machine of an [`InviteLink`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InviteMachine {
    /// Where it listens, as `host:port`.
    pub hosts: Vec<String>,
    /// Its certificate's SHA-256, hex.
    pub fingerprint: String,
    /// Its one-time invite token.
    pub invite: String,
    /// What the inviter calls it.
    pub name: Option<String>,
}

impl std::fmt::Display for InviteLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        if let Some(by) = &self.by {
            query.append_pair("by", by);
        }
        query.append_pair("role", role_name(self.role));
        for machine in &self.machines {
            for host in &machine.hosts {
                query.append_pair("host", host);
            }
            query.append_pair("fp", &machine.fingerprint);
            query.append_pair("invite", &machine.invite);
            if let Some(name) = &machine.name {
                query.append_pair("name", name);
            }
        }
        write!(f, "herder://invite?{}", query.finish())
    }
}

impl std::str::FromStr for InviteLink {
    type Err = anyhow::Error;

    fn from_str(link: &str) -> Result<Self> {
        let query = link
            .trim()
            .strip_prefix("herder://invite?")
            .ok_or_else(|| anyhow!("not a herder://invite link"))?;
        let mut parsed = Self {
            by: None,
            role: Role::Member,
            machines: Vec::new(),
        };
        let mut machine = InviteMachine::default();
        let close = |machine: &mut InviteMachine, machines: &mut Vec<InviteMachine>| {
            let machine = std::mem::take(machine);
            if machine.hosts.is_empty()
                || machine.fingerprint.is_empty()
                || machine.invite.is_empty()
            {
                bail!("the invite link is missing a machine's address, fingerprint or invite");
            }
            machines.push(machine);
            Ok(())
        };
        for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
            let value = value.into_owned();
            match &*key {
                "host" => {
                    // A host after a fingerprint starts the next machine.
                    if !machine.fingerprint.is_empty() {
                        close(&mut machine, &mut parsed.machines)?;
                    }
                    machine.hosts.push(value);
                }
                "fp" => machine.fingerprint = value,
                "invite" => machine.invite = value,
                "name" => machine.name = Some(value),
                "by" => parsed.by = Some(value),
                "role" if value == "owner" => parsed.role = Role::Owner,
                _ => {}
            }
        }
        close(&mut machine, &mut parsed.machines)?;
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creation() -> PasskeyCreation {
        PasskeyCreation {
            rp_id: PASSKEY_RP_ID.to_owned(),
            challenge: "Y2hhbGxlbmdl".to_owned(),
            person_id: PersonId::new("0191f3a0-1c2d-7e3f-8a4b-5c6d7e8f9a0b"),
            user_handle: String::new(),
            expires_at: herder_protocol::Timestamp::now(),
        }
    }

    fn challenge() -> PasskeyChallenge {
        PasskeyChallenge {
            rp_id: PASSKEY_RP_ID.to_owned(),
            challenge: "Y2hhbGxlbmdl".to_owned(),
            expires_at: herder_protocol::Timestamp::now(),
            allow_credentials: Vec::new(),
        }
    }

    #[test]
    fn the_saved_file_signs_with_its_passphrase_only() {
        let dir = tempfile::tempdir().unwrap();
        let (passkey, _) = LocalPasskey::create(&creation(), "alice", "correct horse").unwrap();
        let path = passkey.save(dir.path()).unwrap();
        let mode = std::os::unix::fs::MetadataExt::mode(&std::fs::metadata(&path).unwrap());
        assert_eq!(mode & 0o777, 0o600);

        let loaded = LocalPasskey::load(dir.path()).unwrap().unwrap();
        assert_eq!(loaded, passkey);
        let assertion = loaded
            .unlock("correct horse")
            .unwrap()
            .assert(&challenge())
            .unwrap();
        assert_eq!(assertion.id, passkey.key().credential_id);
        // User present and verified, and neither backup flag.
        let auth_data = decode(&assertion.response.authenticator_data).unwrap();
        assert_eq!(auth_data[32], 0x05);

        for wrong in ["correct horsf", "", "Correct horse"] {
            let refused = loaded
                .unlock(wrong)
                .err()
                .expect("a wrong passphrase opened it");
            assert_eq!(refused.to_string(), "wrong passphrase");
        }
    }

    #[test]
    fn the_file_holds_no_key_in_the_clear() {
        let dir = tempfile::tempdir().unwrap();
        let (passkey, _) = LocalPasskey::create(&creation(), "alice", "correct horse").unwrap();
        passkey.save(dir.path()).unwrap();
        let key = passkey.unlock("correct horse").unwrap().key;

        // Nothing in the file parses as a private key, and the sealed bytes are not one.
        let sealed = decode(&passkey.sealed).unwrap();
        assert!(key_pair(&sealed).is_err());
        assert!(key_pair(&sealed[..sealed.len() - 16]).is_err());
        // The public point is there, as every machine has it; its private scalar is not.
        let text = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(text.contains(&passkey.key().public_key));
        assert_eq!(cose_key(&key), decode(&passkey.public_key).unwrap());

        // A file changed by a byte opens with no passphrase.
        let mut damaged = passkey.clone();
        let mut bytes = sealed;
        bytes[10] ^= 1;
        damaged.sealed = URL_SAFE_NO_PAD.encode(bytes);
        assert!(damaged.unlock("correct horse").is_err());
    }

    #[test]
    fn every_passkey_is_sealed_with_its_own_salt_and_nonce() {
        let (first, _) = LocalPasskey::create(&creation(), "alice", "correct horse").unwrap();
        let (second, _) = LocalPasskey::create(&creation(), "alice", "correct horse").unwrap();
        assert_ne!(first.salt, second.salt);
        assert_ne!(first.nonce, second.nonce);
        for field in [&first.salt, &first.nonce] {
            assert!(decode(field).unwrap().iter().any(|byte| *byte != 0));
        }
        assert_eq!(decode(&first.salt).unwrap().len(), 16);
        assert_eq!(decode(&first.nonce).unwrap().len(), 12);
    }

    #[test]
    fn a_passkey_is_never_replaced_and_takes_no_short_passphrase() {
        let dir = tempfile::tempdir().unwrap();
        let (first, _) = LocalPasskey::create(&creation(), "alice", "correct horse").unwrap();
        first.save(dir.path()).unwrap();
        let (second, _) = LocalPasskey::create(&creation(), "alice", "correct horse").unwrap();
        assert!(second.save(dir.path()).is_err());
        assert_eq!(LocalPasskey::load(dir.path()).unwrap().unwrap(), first);
        assert!(LocalPasskey::create(&creation(), "alice", "short").is_err());
    }

    #[test]
    fn a_challenge_for_other_passkeys_is_not_signed() {
        let (passkey, _) = LocalPasskey::create(&creation(), "alice", "correct horse").unwrap();
        let signer = passkey.unlock("correct horse").unwrap();
        let mut challenge = challenge();
        challenge.allow_credentials = vec!["b3RoZXI".to_owned()];
        assert!(signer.assert(&challenge).is_err());
        challenge
            .allow_credentials
            .push(passkey.key().credential_id);
        assert!(signer.assert(&challenge).is_ok());
    }

    #[test]
    fn an_invite_link_reads_back_as_the_apps_write_it() {
        let link = InviteLink {
            by: Some("Tomas A.".into()),
            role: Role::Owner,
            machines: vec![
                InviteMachine {
                    hosts: vec!["10.0.0.2:7447".into(), "[fd00::1]:7447".into()],
                    fingerprint: "ab".repeat(32),
                    invite: "tok-1".into(),
                    name: Some("dev box".into()),
                },
                InviteMachine {
                    hosts: vec!["10.0.0.3:7447".into()],
                    fingerprint: "cd".repeat(32),
                    invite: "tok-2".into(),
                    name: None,
                },
            ],
        };
        let text = link.to_string();
        assert!(
            text.starts_with("herder://invite?by=Tomas+A.&role=owner&host=10.0.0.2%3A7447&host="),
            "{text}"
        );
        assert_eq!(text.parse::<InviteLink>().unwrap(), link);
        // A link the apps make carries `for` too, and a member's says so or nothing.
        let member: InviteLink = format!(
            "herder://invite?for=Lina&host=a:1&fp={}&invite=t",
            "ab".repeat(32)
        )
        .parse()
        .unwrap();
        assert_eq!(member.role, Role::Member);
        assert!(
            "herder://invite?host=a:1&invite=t"
                .parse::<InviteLink>()
                .is_err()
        );
        assert!(
            "herder://own?host=a:1&fp=ab&code=c"
                .parse::<InviteLink>()
                .is_err()
        );
    }
}
