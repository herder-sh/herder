//! A host reading its vault the way clients do: the fleet, and a session's journal.
//!
//! The host connects with its own device key, paired with the vault, and speaks the client
//! protocol: hello with the session to replay as its one cursor, then `sync`, which the vault
//! answers once the lists and the replay are sent. Nothing is subscribed beyond that.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use herder_client_core::auth::{DeviceKey, client_config};
use herder_protocol::{
    ClientHello, ClientMessage, Cursor, Event, FleetHost, PROTOCOL_VERSION, ServerMessage,
    SessionHead, SessionId,
};
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use super::BUILD;
use crate::config::VaultConfig;

/// A connection to the vault.
pub(super) type Ws = WebSocketStream<TlsStream<TcpStream>>;

/// How long reading the vault may take, a long journal's replay included.
const READ_TIMEOUT: Duration = Duration::from_secs(120);

/// What the vault showed.
#[derive(Debug, Default)]
pub(super) struct View {
    /// Every session it holds, each on the host that has it now.
    pub(super) sessions: Vec<SessionHead>,
    /// Every host, with its liveness.
    pub(super) hosts: Vec<FleetHost>,
    /// The journal asked for, oldest first.
    pub(super) events: Vec<Event>,
}

/// Opens a WebSocket to the vault over TLS, pinned through `connector`.
pub(super) async fn dial(vault: &VaultConfig, connector: &TlsConnector) -> Result<Ws> {
    let address = &vault.address;
    let tcp = TcpStream::connect(address).await.context("connecting")?;
    tcp.set_nodelay(true)?;
    // The certificate is pinned by fingerprint, so the name is never checked.
    let name = ServerName::try_from("herder").context("the TLS server name")?;
    let tls = connector.connect(name, tcp).await.context("TLS")?;
    let request = format!("wss://{address}/").into_client_request()?;
    let (ws, _) = tokio_tungstenite::client_async(request, tls)
        .await
        .context("the WebSocket upgrade")?;
    Ok(ws)
}

/// Reads the fleet from the vault as `device`, and the whole journal of `journal` if given.
pub(super) async fn read(
    vault: &VaultConfig,
    device: &DeviceKey,
    journal: Option<&SessionId>,
) -> Result<View> {
    tokio::time::timeout(READ_TIMEOUT, read_view(vault, device, journal))
        .await
        .map_err(|_| anyhow!("the vault did not answer in {READ_TIMEOUT:?}"))?
        .with_context(|| format!("reading the vault at {}", vault.address))
}

async fn read_view(
    vault: &VaultConfig,
    device: &DeviceKey,
    journal: Option<&SessionId>,
) -> Result<View> {
    let connector = TlsConnector::from(Arc::new(client_config(&vault.fingerprint, device)?));
    let mut ws = dial(vault, &connector).await?;
    let hello = ClientMessage::Hello(ClientHello {
        protocol_version: PROTOCOL_VERSION,
        client: BUILD.to_owned(),
        resume: journal
            .map(|session_id| Cursor {
                session_id: session_id.clone(),
                after_seq: 0,
            })
            .into_iter()
            .collect(),
        pairing_code: vault.pairing_code.clone(),
    });
    let token = "read".to_owned();
    for message in [
        hello,
        ClientMessage::Sync {
            token: token.clone(),
        },
    ] {
        ws.send(Message::text(serde_json::to_string(&message)?))
            .await?;
    }
    let mut view = View::default();
    loop {
        let text = match ws.next().await {
            Some(Ok(Message::Text(text))) => text,
            Some(Ok(Message::Close(_))) | None => bail!("the vault closed the connection"),
            Some(Ok(_)) => continue,
            Some(Err(err)) => return Err(err.into()),
        };
        match serde_json::from_str(&text)? {
            ServerMessage::Sessions { sessions } => view.sessions = sessions,
            ServerMessage::Hosts { hosts } => view.hosts = hosts,
            ServerMessage::Event(event) if Some(&event.session_id) == journal => {
                view.events.push(event);
            }
            ServerMessage::Error { error } => bail!("the vault refused: {}", error.message),
            ServerMessage::Synced { token: synced } if synced == token => break,
            _ => {}
        }
    }
    let _ = ws.close(None).await;
    Ok(view)
}
