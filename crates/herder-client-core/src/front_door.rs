//! Signing in with a browser to a login proxy in front of a tunnel address: a front door,
//! such as Cloudflare Access with an identity provider.
//!
//! A person cannot type a service token into a login page, so the device does what
//! `cloudflared access login` does:
//!
//! 1. It asks the edge which Access application guards the address: a `HEAD` with
//!    `cf-access-metadata-request: true` answers with a JWT in `cf-access-metadata` that
//!    names its audience (`aud`) and hostname.
//! 2. It makes an X25519 key pair for this sign-in only, and has the person open
//!    `https://<host>/cdn-cgi/access/cli?...&token=<public key>` in a browser, where they sign
//!    in as for any Access application.
//! 3. Meanwhile it long-polls Cloudflare's transfer service at
//!    `login.cloudflareaccess.org/transfer/<public key>`, which hands over the application's
//!    token once the person signed in, sealed to the key (NaCl `crypto_box`), so only this
//!    device can open it.
//!
//! The token, a JWT, is then sent as `cf-access-token` on the address's upgrade, kept in the
//! device's profile with the address's other headers ([`crate::Client::set_address_headers`]).
//! Its `exp` says when the person signs in again; a proxy that turns the upgrade away to its
//! login says so sooner ([`SignInWanted`]).
//!
//! The metadata JWT is read without checking its signature: its audience only goes back to
//! the same host as a query parameter, and the token comes sealed to this device's key, and
//! goes only to that host.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD};
use crypto_box::aead::Aead;
use crypto_box::{PublicKey, SalsaBox, SecretKey};
use herder_protocol::Timestamp;
use http_body_util::{BodyExt, Empty};
use hyper::body::Bytes;
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::tunnel;

/// Cloudflare's transfer service, which hands a signed-in token to the device.
pub(crate) const TRANSFER: &str = "https://login.cloudflareaccess.org";

/// The header a Cloudflare Access application token is sent in.
pub(crate) const TOKEN_HEADER: &str = "cf-access-token";

/// How long the person gets to sign in in the browser.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(600);

/// How long one long poll of the transfer service may wait.
const POLL_TIMEOUT: Duration = Duration::from_secs(70);

/// How long any other request may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Most bytes of an answer read.
const MAX_ANSWER: usize = 64 * 1024;

/// A tunnel's upgrade turned away to its proxy's login: the person signs in with a browser.
#[derive(Debug, thiserror::Error)]
#[error("the login proxy in front of it wants you to sign in with a browser")]
pub(crate) struct SignInWanted;

/// Whether a refused upgrade, answered `status` with `location` after sending `sent`, is a
/// login proxy asking for a sign-in: a redirect to Cloudflare Access's login, or a refusal of
/// a signed-in token, which has expired or been revoked.
pub(crate) fn wants_sign_in(status: u16, location: Option<&str>, sent: &tunnel::Sent) -> bool {
    let to_login = location
        .and_then(|location| url::Url::parse(location).ok())
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| host.ends_with(".cloudflareaccess.com"));
    match status {
        300..=399 => to_login,
        401 | 403 => to_login || sent.contains_key(TOKEN_HEADER),
        _ => false,
    }
}

/// When the token `jwt` expires, from its `exp`; `None` for one that is not a JWT with one.
pub(crate) fn expires_at(jwt: &str) -> Option<Timestamp> {
    let exp = claims(jwt)?.get("exp")?.as_i64()?;
    Timestamp::from_second(exp).ok()
}

/// The claims of `jwt`, unchecked.
fn claims(jwt: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// A sign-in under way: the page to open, and what opens the token it ends with.
pub(crate) struct Begun {
    /// The page the person signs in on.
    pub(crate) url: String,
    /// Where the transfer service hands the token over.
    poll: String,
    key: SecretKey,
}

impl Begun {
    /// Waits for the person to sign in, and returns the application token.
    pub(crate) async fn token(&self) -> anyhow::Result<String> {
        let deadline = tokio::time::Instant::now() + SIGN_IN_TIMEOUT;
        loop {
            if tokio::time::Instant::now() >= deadline {
                bail!(
                    "nobody signed in within {} minutes",
                    SIGN_IN_TIMEOUT.as_secs() / 60
                );
            }
            let answer = get(&self.poll, &BTreeMap::new(), POLL_TIMEOUT).await;
            let (status, headers, body) = match answer {
                Ok(answer) => answer,
                // A long poll the network or the service dropped is asked again.
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };
            if status.is_server_error() {
                bail!(
                    "the Cloudflare Access transfer service answered {status}: {}",
                    String::from_utf8_lossy(&body).trim()
                );
            }
            if status != StatusCode::OK {
                // Not signed in yet.
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            let sender = headers
                .get("service-public-key")
                .context("the transfer service named no key")?;
            return self.open(&body, sender);
        }
    }

    /// The application token in `body`, sealed by `sender`.
    fn open(&self, body: &[u8], sender: &str) -> anyhow::Result<String> {
        let sealed = STANDARD
            .decode(String::from_utf8_lossy(body).trim())
            .context("the transfer service's answer is not base64")?;
        if sealed.len() < 24 {
            bail!("the transfer service's answer is too short");
        }
        let (nonce, sealed) = sealed.split_at(24);
        let sender: [u8; 32] = URL_SAFE
            .decode(sender.trim())
            .ok()
            .and_then(|key| key.try_into().ok())
            .context("the transfer service's key is not one")?;
        let opened = SalsaBox::new(&PublicKey::from(sender), &self.key)
            .decrypt(nonce.into(), sealed)
            .map_err(|_| anyhow!("the token the transfer service handed over does not open"))?;
        let answer: serde_json::Value =
            serde_json::from_slice(&opened).context("the handed over token is not JSON")?;
        answer["app_token"]
            .as_str()
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
            .context("the transfer service handed over no application token")
    }
}

/// Starts signing in to the Access application in front of `address`, a tunnel's `wss://`
/// or `ws://` URL, through the transfer service at `transfer` ([`TRANSFER`] but in tests).
pub(crate) async fn begin(address: &str, transfer: &str) -> anyhow::Result<Begun> {
    let url = url::Url::parse(address).context("the address")?;
    let scheme = match url.scheme() {
        "wss" => "https",
        "ws" => "http",
        _ => bail!("{address} is not a tunnel's URL: only those have a front door"),
    };
    let host = url.host_str().context("the address names no host")?;
    let origin = match url.port() {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    };

    let metadata = BTreeMap::from([("cf-access-metadata-request".to_owned(), "true".to_owned())]);
    let (_, headers, _) = request(
        Method::HEAD,
        &format!("{origin}/"),
        &metadata,
        REQUEST_TIMEOUT,
    )
    .await
    .with_context(|| format!("asking {host} which Cloudflare Access application it is"))?;
    let jwt = headers.get("cf-access-metadata").with_context(|| {
        format!(
            "no Cloudflare Access application guards {host}; if its proxy takes headers, set \
             them for this address instead"
        )
    })?;
    let claims = claims(jwt).context("Cloudflare Access's metadata is not a JWT")?;
    let aud = claims
        .get("aud")
        .and_then(serde_json::Value::as_str)
        .filter(|aud| !aud.is_empty())
        .context("Cloudflare Access named no application")?;
    let named = claims.get("hostname").and_then(serde_json::Value::as_str);
    if !named.is_some_and(|named| named.eq_ignore_ascii_case(host)) {
        bail!("Cloudflare Access answered for {named:?}, not {host}");
    }

    let key = SecretKey::generate(&mut crypto_box::aead::OsRng);
    let public = URL_SAFE.encode(key.public_key().as_bytes());
    let mut redirect = url::Url::parse(&origin)?;
    redirect
        .query_pairs_mut()
        .append_pair("aud", aud)
        .append_pair("token", &public);
    let mut page = url::Url::parse(&origin)?.join("/cdn-cgi/access/cli")?;
    page.query_pairs_mut()
        .append_pair("aud", aud)
        .append_pair("token", &public)
        .append_pair("redirect_url", redirect.as_str())
        .append_pair("send_org_token", "true")
        .append_pair("edge_token_transfer", "true");
    Ok(Begun {
        url: page.into(),
        poll: format!("{}/transfer/{public}", transfer.trim_end_matches('/')),
        key,
    })
}

/// The status, lowercase headers and body of a `GET` of `url` with `headers`.
async fn get(
    url: &str,
    headers: &BTreeMap<String, String>,
    timeout: Duration,
) -> anyhow::Result<(StatusCode, BTreeMap<String, String>, Bytes)> {
    request(Method::GET, url, headers, timeout).await
}

/// Sends `method` to `url` (`https://`, checked against the public web PKI, or `http://`)
/// with `headers`, following no redirect, within `timeout`.
async fn request(
    method: Method,
    url: &str,
    headers: &BTreeMap<String, String>,
    timeout: Duration,
) -> anyhow::Result<(StatusCode, BTreeMap<String, String>, Bytes)> {
    tokio::time::timeout(timeout, send(method, url, headers))
        .await
        .unwrap_or_else(|_| Err(anyhow!("no answer from {url} in {timeout:?}")))
}

async fn send(
    method: Method,
    url: &str,
    headers: &BTreeMap<String, String>,
) -> anyhow::Result<(StatusCode, BTreeMap<String, String>, Bytes)> {
    let parsed = url::Url::parse(url)?;
    let host = parsed.host_str().context("the URL names no host")?;
    let bare = host.trim_matches(['[', ']']);
    let port = parsed
        .port_or_known_default()
        .context("the URL names no port")?;
    let tcp = TcpStream::connect((bare, port))
        .await
        .with_context(|| format!("connecting to {host}"))?;
    let authority = match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };
    let target = match parsed.query() {
        Some(query) => format!("{}?{query}", parsed.path()),
        None => parsed.path().to_owned(),
    };
    let mut builder = Request::builder()
        .method(method)
        .uri(target)
        .header("host", authority)
        .header("user-agent", concat!("herder/", env!("CARGO_PKG_VERSION")));
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    let request = builder.body(Empty::<Bytes>::new())?;
    let response = if parsed.scheme() == "https" {
        let config = tunnel::web_pki().map_err(|err| anyhow!("TLS: {err}"))?;
        let name = ServerName::try_from(bare.to_owned()).context("the TLS server name")?;
        let tls = TlsConnector::from(Arc::clone(config))
            .connect(name, tcp)
            .await
            .with_context(|| format!("TLS with {host}"))?;
        exchange(TokioIo::new(tls), request).await?
    } else {
        exchange(TokioIo::new(tcp), request).await?
    };
    Ok(response)
}

async fn exchange<I>(
    io: I,
    request: Request<Empty<Bytes>>,
) -> anyhow::Result<(StatusCode, BTreeMap<String, String>, Bytes)>
where
    I: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
{
    let (mut sender, connection) = hyper::client::conn::http1::handshake(io).await?;
    tokio::spawn(connection);
    let response = sender.send_request(request).await?;
    let status = response.status();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned()))
        })
        .collect();
    let body = http_body_util::Limited::new(response.into_body(), MAX_ANSWER)
        .collect()
        .await
        .map_err(|err| anyhow!("reading the answer: {err}"))?
        .to_bytes();
    Ok((status, headers, body))
}

#[cfg(test)]
pub(crate) mod tests;
