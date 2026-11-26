//! A host reading the vault's session list the way clients do, to see which of its sessions
//! another host took over.
//!
//! The host connects with its own device key, paired with the vault, and speaks the client
//! protocol: hello, then `sync`, which the vault answers once the lists are sent. Nothing is
//! subscribed. A host never reads other hosts' journals.

use std::net::SocketAddr;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use herder_client_core::auth::{DeviceKey, client_config};
use herder_protocol::{
    ClientHello, ClientMessage, PROTOCOL_VERSION, RELAY_VERSION, RelayAnswer, RelayHello,
    ServerMessage, SessionHead,
};
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use super::BUILD;
use crate::config::VaultConfig;
use crate::ws::tunnel::{self, Frames, Io};

/// A connection to the vault.
pub(super) type Ws = WebSocketStream<TlsStream<Box<dyn Io>>>;

/// Outer TLS, for a vault behind a `wss://` tunnel: the public web PKI, as a browser checks
/// it. The vault's own certificate is pinned inside.
static WEB_PKI: LazyLock<Result<Arc<rustls::ClientConfig>, String>> = LazyLock::new(|| {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let provider = rustls::crypto::ring::default_provider();
    rustls::ClientConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .map(|builder| Arc::new(builder.with_root_certificates(roots).with_no_client_auth()))
        .map_err(|err| err.to_string())
});

/// Time to open a relay connection to the vault and get its answer.
const RELAY_TIMEOUT: Duration = Duration::from_secs(10);

/// How long reading the vault's session list may take.
const READ_TIMEOUT: Duration = Duration::from_secs(120);

/// Opens a WebSocket to the vault over TLS, pinned through `connector`.
pub(super) async fn dial(vault: &VaultConfig, connector: &TlsConnector) -> Result<Ws> {
    Ok(dial_with(vault, connector, None).await?.0)
}

/// [`dial`] with the WebSocket configured as `config` says; also the address of the socket's
/// peer: the vault, or the edge of the tunnel to it.
async fn dial_with(
    vault: &VaultConfig,
    connector: &TlsConnector,
    config: Option<WebSocketConfig>,
) -> Result<(Ws, SocketAddr)> {
    let (stream, peer) = reach(&vault.address).await?;
    // The certificate is pinned by fingerprint, so the name is never checked.
    let name = ServerName::try_from("herder").context("the TLS server name")?;
    let tls = connector.connect(name, stream).await.context("TLS")?;
    // The vault reads nothing of the URL.
    let request = "wss://herder/".into_client_request()?;
    let (ws, _) = tokio_tungstenite::client_async_with_config(request, tls, config)
        .await
        .context("the WebSocket upgrade")?;
    Ok((ws, peer))
}

/// The byte stream to the vault at `address`: a TCP socket to `host:port`, or, for the URL of
/// a tunnel to the vault such as a Cloudflare Tunnel's public hostname, the frames of the
/// WebSocket the tunnel relays, as clients reach a daemon behind one (`docs/public-access.md`).
async fn reach(address: &str) -> Result<(Box<dyn Io>, SocketAddr)> {
    if !address.contains("://") {
        let tcp = TcpStream::connect(address).await.context("connecting")?;
        tcp.set_nodelay(true)?;
        let peer = tcp.peer_addr()?;
        return Ok((Box::new(tcp), peer));
    }
    let url = url::Url::parse(address).context("the vault's tunnel URL")?;
    let host = url.host_str().context("the tunnel URL names no host")?;
    let host = host.trim_matches(['[', ']']);
    let port = url
        .port_or_known_default()
        .context("the tunnel URL names no port")?;
    let tcp = TcpStream::connect((host, port))
        .await
        .context("connecting")?;
    tcp.set_nodelay(true)?;
    let peer = tcp.peer_addr()?;
    let outer: Box<dyn Io> = if url.scheme() == "wss" {
        let config = WEB_PKI.as_ref().map_err(|err| anyhow!("TLS: {err}"))?;
        let name = ServerName::try_from(host.to_owned()).context("the tunnel's server name")?;
        let tls = TlsConnector::from(Arc::clone(config))
            .connect(name, tcp)
            .await
            .context("the tunnel's TLS")?;
        Box::new(tls)
    } else {
        Box::new(tcp)
    };
    let request = address.into_client_request()?;
    let (ws, _) =
        tokio_tungstenite::client_async_with_config(request, outer, Some(tunnel::config()))
            .await
            .context("the tunnel's WebSocket upgrade")?;
    Ok((Box::new(Frames::new(ws)), peer))
}

/// Opens this host's end of the relay stream `stream_id`, which the vault asked for: the
/// stream a client's connection runs in, and the address of the socket's peer.
pub(super) async fn accept_relay(
    vault: &VaultConfig,
    connector: &TlsConnector,
    stream_id: &str,
) -> Result<(Box<dyn Io>, SocketAddr)> {
    let accept = RelayHello::Accept {
        relay_version: RELAY_VERSION,
        stream_id: stream_id.to_owned(),
    };
    open_relay(vault, connector, &accept).await
}

/// Opens a stream to the vault's CLIProxyAPI, for a host the vault forwards there: it
/// carries the HTTP a CLI speaks to the proxy.
pub(super) async fn open_proxy(
    vault: &VaultConfig,
    connector: &TlsConnector,
) -> Result<Box<dyn Io>> {
    let proxy = RelayHello::Proxy {
        relay_version: RELAY_VERSION,
    };
    let opening = open_relay(vault, connector, &proxy);
    let (stream, _) = tokio::time::timeout(RELAY_TIMEOUT, opening)
        .await
        .map_err(|_| anyhow!("no answer from the vault in {RELAY_TIMEOUT:?}"))??;
    Ok(stream)
}

/// A relay connection to the vault that says `hello`: the stream the vault opened for it,
/// and the address of the socket's peer.
async fn open_relay(
    vault: &VaultConfig,
    connector: &TlsConnector,
    hello: &RelayHello,
) -> Result<(Box<dyn Io>, SocketAddr)> {
    // Each frame goes out as written: what runs inside decides what to batch.
    let (mut ws, peer) = dial_with(vault, connector, Some(tunnel::config())).await?;
    ws.send(Message::text(serde_json::to_string(hello)?))
        .await?;
    loop {
        let text = match ws.next().await {
            Some(Ok(Message::Text(text))) => text,
            Some(Ok(Message::Close(_))) | None => bail!("the vault closed the connection"),
            Some(Ok(_)) => continue,
            Some(Err(err)) => return Err(err.into()),
        };
        return match serde_json::from_str(&text)? {
            RelayAnswer::Ready => Ok((Box::new(Frames::new(ws)), peer)),
            RelayAnswer::Refused { error } => Err(anyhow!("the vault refused: {}", error.message)),
            RelayAnswer::Unknown => Err(anyhow!("the vault's answer is not one this host knows")),
        };
    }
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
        passkey: false,
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
            ServerMessage::Error { error, .. } => bail!("the vault refused: {}", error.message),
            ServerMessage::Synced { token: synced } if synced == token => break,
            _ => {}
        }
    }
    let _ = ws.close(None).await;
    Ok(sessions)
}
