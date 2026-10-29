//! A host reading the vault's session list the way clients do, to see which of its sessions
//! another host took over.
//!
//! The host connects with its own device key, paired with the vault, and speaks the client
//! protocol: hello, then `sync`, which the vault answers once the lists are sent. Nothing is
//! subscribed. A host never reads other hosts' journals.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use herder_client_core::auth::{DeviceKey, client_config};
use herder_protocol::{ClientHello, ClientMessage, PROTOCOL_VERSION, ServerMessage, SessionHead};
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

/// How long reading the vault's session list may take.
const READ_TIMEOUT: Duration = Duration::from_secs(120);

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

/// Every session the vault holds, each on the host that has it now, read as `device`.
pub(super) async fn sessions(vault: &VaultConfig, device: &DeviceKey) -> Result<Vec<SessionHead>> {
    tokio::time::timeout(READ_TIMEOUT, read_sessions(vault, device))
        .await
        .map_err(|_| anyhow!("the vault did not answer in {READ_TIMEOUT:?}"))?
        .with_context(|| format!("reading the vault at {}", vault.address))
}

async fn read_sessions(vault: &VaultConfig, device: &DeviceKey) -> Result<Vec<SessionHead>> {
    let connector = TlsConnector::from(Arc::new(client_config(&vault.fingerprint, device)?));
    let mut ws = dial(vault, &connector).await?;
    let hello = ClientMessage::Hello(ClientHello {
        protocol_version: PROTOCOL_VERSION,
        client: BUILD.to_owned(),
        resume: Vec::new(),
        pairing_code: vault.pairing_code.clone(),
        sign_in: false,
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
    let mut sessions = Vec::new();
    loop {
        let text = match ws.next().await {
            Some(Ok(Message::Text(text))) => text,
            Some(Ok(Message::Close(_))) | None => bail!("the vault closed the connection"),
            Some(Ok(_)) => continue,
            Some(Err(err)) => return Err(err.into()),
        };
        match serde_json::from_str(&text)? {
            ServerMessage::Sessions { sessions: listed } => sessions = listed,
            ServerMessage::Error { error } => bail!("the vault refused: {}", error.message),
            ServerMessage::Synced { token: synced } if synced == token => break,
            _ => {}
        }
    }
    let _ = ws.close(None).await;
    Ok(sessions)
}
