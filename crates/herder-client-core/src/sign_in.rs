//! Signing this device in to a machine as a person, rather than pairing it with a code.
//!
//! The device connects under a new key with `sign_in` set, and the daemon, which does not know
//! that key, answers with a challenge. The device then signs in one of the ways
//! [`SignIn`] names: an app with the person's passkey ([`Client::sign_in`]), or a terminal,
//! which holds none, by waiting until an app approves the code it shows ([`Client::log_in`]).
//! Once the daemon says hello the machine is saved, as pairing saves it.

use std::time::Duration;

use futures_util::SinkExt;
use herder_protocol::{
    ClientHello, ClientMessage, ErrorCode, ErrorInfo, PROTOCOL_VERSION, PasskeyChallenge,
    PasskeyCreation, ServerMessage, SignIn, Timestamp,
};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

use crate::profile::SavedMachine;
use crate::supervisor::{self, Opened, Ws};
use crate::{Client, Error, Machine, unsaved};

/// Time the daemon gets to answer a sign-in that needs nobody's approval.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);

/// A machine that asked this device to sign in, with the connection that waits for it.
pub struct PendingSignIn {
    client: Client,
    challenge: PasskeyChallenge,
    registration: Option<PasskeyCreation>,
    /// Taken by the one [`PendingSignIn::finish`].
    connection: Mutex<Option<Connection>>,
}

/// The connection a sign-in goes on, and the machine it saves.
struct Connection {
    ws: Ws,
    saved: SavedMachine,
}

/// [`Client::sign_in`].
pub(crate) async fn start(
    client: &Client,
    hosts: &[String],
    fingerprint: &str,
) -> Result<PendingSignIn, Error> {
    let (saved, device) =
        unsaved(hosts, fingerprint).map_err(|message| Error::Local { message })?;
    let hello = ClientHello {
        protocol_version: PROTOCOL_VERSION,
        client: client.inner.client.clone(),
        resume: Vec::new(),
        pairing_code: None,
        sign_in: true,
        passkey: false,
    };
    let (ws, opened, _) = supervisor::open(&saved.addresses, &saved.fingerprint, &device, hello)
        .await
        .map_err(|message| Error::Unreachable { message })?;
    let Opened::SignIn {
        challenge,
        registration,
    } = opened
    else {
        // A key made just now cannot be known to the machine.
        return Err(Error::Local {
            message: "the machine knew a device key made just now".to_owned(),
        });
    };
    Ok(PendingSignIn {
        client: client.clone(),
        challenge,
        registration,
        connection: Mutex::new(Some(Connection { ws, saved })),
    })
}

/// [`Client::log_in`].
pub(crate) async fn log_in(
    client: &Client,
    hosts: &[String],
    fingerprint: &str,
    shown: impl FnOnce(&str, Timestamp) + Send,
) -> Result<Machine, Error> {
    let pending = start(client, hosts, fingerprint).await?;
    let Some(mut connection) = pending.connection.into_inner() else {
        return Err(Error::Closed);
    };
    connection.send(SignIn::StartLogin).await?;
    let (code, expires_at) = match connection.answer(ANSWER_TIMEOUT).await? {
        ServerMessage::LoginRequest { code, expires_at } => (code, expires_at),
        other => return Err(unexpected(other)),
    };
    shown(&code, expires_at);
    // The daemon says when the request ends; it answers by then.
    let left = expires_at.duration_since(Timestamp::now()).unsigned_abs();
    let answer = connection.answer(left + ANSWER_TIMEOUT).await?;
    connection.welcomed(client, answer)
}

impl PendingSignIn {
    /// What an existing passkey signs, for every way of signing in but a login.
    pub fn challenge(&self) -> PasskeyChallenge {
        self.challenge.clone()
    }

    /// What a new passkey is created from, to claim a machine or accept an invite with a new
    /// passkey; `None` while the machine takes neither.
    pub fn registration(&self) -> Option<PasskeyCreation> {
        self.registration.clone()
    }

    /// Signs in with `sign_in` and saves the machine once it accepts, replacing it if paired
    /// before. Fails with `Rejected` when the machine refuses, e.g. `forbidden` for an
    /// assertion it cannot verify or a wrong code. A login, which waits for an app's
    /// approval, goes through [`Client::log_in`] instead. A sign-in finishes once: calling
    /// this again fails with `Closed`.
    pub async fn finish(&self, sign_in: SignIn) -> Result<Machine, Error> {
        if matches!(sign_in, SignIn::StartLogin) {
            return Err(Error::Local {
                message: "a login waits for an app's approval; use log_in".to_owned(),
            });
        }
        let Some(mut connection) = self.connection.lock().await.take() else {
            return Err(Error::Closed);
        };
        connection.send(sign_in).await?;
        let answer = connection.answer(ANSWER_TIMEOUT).await?;
        connection.welcomed(&self.client, answer)
    }
}

impl Connection {
    async fn send(&mut self, sign_in: SignIn) -> Result<(), Error> {
        let text =
            serde_json::to_string(&ClientMessage::SignIn(sign_in)).map_err(|err| Error::Local {
                message: format!("encoding the sign-in: {err}"),
            })?;
        self.ws
            .send(Message::text(text))
            .await
            .map_err(|err| Error::Unreachable {
                message: format!("{err}"),
            })
    }

    /// The daemon's next message, within `wait`.
    async fn answer(&mut self, wait: Duration) -> Result<ServerMessage, Error> {
        match tokio::time::timeout(wait, supervisor::receive(&mut self.ws)).await {
            Ok(Ok(message)) => Ok(message),
            Ok(Err(err)) => Err(Error::Unreachable {
                message: format!("{err:#}"),
            }),
            Err(_) => Err(Error::Unreachable {
                message: format!("the machine did not answer within {wait:?}"),
            }),
        }
    }

    /// The machine as saved once `answer`, its hello, says this device is signed in.
    fn welcomed(self, client: &Client, answer: ServerMessage) -> Result<Machine, Error> {
        let ServerMessage::Hello(hello) = answer else {
            return Err(unexpected(answer));
        };
        // The supervisor opens its own connection; this one only signed in.
        let mut saved = self.saved;
        saved.host_id = hello.host_id;
        saved.name = hello.host_name;
        client
            .add(saved)
            .map_err(|(_, message)| Error::Local { message })
    }
}

/// The error for `answer`, which is not what the sign-in waited for.
fn unexpected(answer: ServerMessage) -> Error {
    let info = match answer {
        ServerMessage::Error { error } => error,
        _ => ErrorInfo {
            code: ErrorCode::Internal,
            message: "the machine answered the sign-in with something else".to_owned(),
        },
    };
    Error::Rejected { info }
}
