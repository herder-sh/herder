//! The loopback proxy a host runs its hub logins' CLIs through, which pins each request to
//! one login of the hub's CLIProxyAPI.
//!
//! The proxy runs a request for `<pin>/<model>` on the login with that pin only
//! ([`super::cliproxyapi`]). A CLI picks its model names itself, for its main model, its
//! subagents and its side calls, and the proxy takes no alias such as `opus`, so the pin goes on
//! the names the CLI sends: an account on a hub login points its CLI at
//! `http://127.0.0.1:<port>/<pin>` ([`PinProxy::url`]), and this proxy forwards each request
//! to the hub's CLIProxyAPI with the pin taken off the path and put before the `model` of
//! its JSON body. Everything else passes as it is, the client key the CLI sends included, and
//! the answer streams back unchanged. herder never sees a provider token: the CLIProxyAPI
//! holds them.
//!
//! A hub that names no address for its proxy forwards to it itself: each request then
//! goes on a relay stream to the hub ([`herder_protocol::relay`]), inside the host's
//! pinned connection to it, so a host reaches the proxy wherever it reaches its hub.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::header::{CONNECTION, CONTENT_LENGTH, HOST, HeaderName, HeaderValue, TRANSFER_ENCODING};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpListener;
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::client;
use crate::config::HubConfig;
use crate::ws::tunnel::Io;

/// Largest request body forwarded: a prompt with its images.
const MAX_REQUEST_BYTES: usize = 256 * 1024 * 1024;

/// Where the hub's CLIProxyAPI is now, as the hub last said; `None` while it has not.
pub type Upstream = Arc<dyn Fn() -> Option<Target> + Send + Sync>;

/// How a host reaches its hub's CLIProxyAPI.
#[derive(Clone)]
pub enum Target {
    /// At this base URL, which the host can dial.
    Url(String),
    /// Through the hub, which forwards to the proxy beside it: the proxy is on no network
    /// the host could reach it on.
    Hub(Through),
}

/// Opens streams to the hub's CLIProxyAPI through the hub.
#[derive(Clone)]
pub struct Through(Arc<Open>);

type Open = dyn Fn() -> BoxFuture<'static, Result<Box<dyn Io>>> + Send + Sync;

impl Through {
    /// Streams opened as `hub` is dialled, with `connector`'s pinned TLS and device key.
    pub(super) fn new(hub: HubConfig, connector: TlsConnector) -> Self {
        Self(Arc::new(move || {
            let (hub, connector) = (hub.clone(), connector.clone());
            Box::pin(async move { client::open_proxy(&hub, &connector).await })
        }))
    }

    /// One stream, for one request.
    async fn open(&self) -> Result<Box<dyn Io>> {
        (self.0)().await
    }
}

type Failed = Box<dyn std::error::Error + Send + Sync>;

type Body = BoxBody<Bytes, hyper::Error>;

/// A running pin proxy, until its shutdown.
pub struct PinProxy {
    addr: SocketAddr,
}

struct State {
    upstream: Upstream,
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
}

impl PinProxy {
    /// Listens on a free loopback port and forwards to `upstream` until `shutdown`.
    pub async fn start(upstream: Upstream, shutdown: CancellationToken) -> Result<Self> {
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .context("cannot load the system's certificates")?
            .https_or_http()
            .enable_http1()
            .build();
        let state = Arc::new(State {
            upstream,
            client: Client::builder(TokioExecutor::new()).build(https),
        });
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .context("cannot listen on a loopback port for the hub logins")?;
        let addr = listener.local_addr()?;
        tokio::spawn(async move {
            loop {
                let stream = tokio::select! {
                    () = shutdown.cancelled() => return,
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _)) => stream,
                        Err(err) => {
                            warn!("the hub logins' proxy cannot accept: {err}");
                            continue;
                        }
                    },
                };
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    let service = service_fn(move |request| forward(Arc::clone(&state), request));
                    if let Err(err) = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await
                    {
                        debug!("a hub login's connection failed: {err}");
                    }
                });
            }
        });
        Ok(Self { addr })
    }

    /// The base URL a CLI runs on the login pinned `pin` with.
    pub fn url(&self, pin: &str) -> String {
        format!("http://{}/{pin}", self.addr)
    }
}

/// Forwards `request`, for `/<pin>/<path>`, to the CLIProxyAPI's `/<path>` with its model
/// pinned. When the proxy cannot be reached, the connection drops with no answer, as it would
/// have reaching the proxy directly, so the CLI fails the turn as unreachable.
async fn forward(state: Arc<State>, request: Request<Incoming>) -> Result<Response<Body>, Failed> {
    let (mut parts, body) = request.into_parts();
    let path = parts.uri.path().trim_start_matches('/');
    let (pin, rest) = path.split_once('/').unwrap_or((path, ""));
    if pin.is_empty() {
        return Ok(refuse(StatusCode::NOT_FOUND, "no hub login in the path"));
    }
    let Some(upstream) = (state.upstream)() else {
        return Ok(refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "the hub has not said where its CLIProxyAPI is",
        ));
    };
    let query = parts
        .uri
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let (target, through) = match &upstream {
        Target::Url(url) => (format!("{}/{rest}{query}", url.trim_end_matches('/')), None),
        // The hub puts its proxy's address before the path.
        Target::Hub(through) => (format!("/{rest}{query}"), Some(through)),
    };
    let Ok(uri) = target.parse() else {
        return Ok(refuse(
            StatusCode::BAD_GATEWAY,
            "the hub's CLIProxyAPI address is not a URL",
        ));
    };
    let body = match Limited::new(body, MAX_REQUEST_BYTES).collect().await {
        Ok(body) => pinned(body.to_bytes(), pin),
        Err(_) => {
            return Ok(refuse(
                StatusCode::PAYLOAD_TOO_LARGE,
                "the request is too large",
            ));
        }
    };
    parts.uri = uri;
    for name in [HOST, CONTENT_LENGTH, CONNECTION, TRANSFER_ENCODING] {
        parts.headers.remove(name);
    }
    parts.headers.remove(HeaderName::from_static("keep-alive"));
    let request = Request::from_parts(parts, Full::new(body));
    let response = match through {
        None => state.client.request(request).await.map_err(Failed::from),
        Some(through) => through_hub(through, request).await,
    };
    match response {
        Ok(response) => {
            let (mut parts, body) = response.into_parts();
            parts.headers.remove(CONNECTION);
            parts.headers.remove(HeaderName::from_static("keep-alive"));
            Ok(Response::from_parts(parts, body.boxed()))
        }
        Err(err) => {
            let at = match &upstream {
                Target::Url(url) => url.as_str(),
                Target::Hub(_) => "through the hub",
            };
            warn!("cannot reach the hub's CLIProxyAPI {at}: {err}");
            Err(err)
        }
    }
}

/// Sends `request` on a stream of its own through the hub, which lasts until its answer
/// is read to the end.
async fn through_hub(
    through: &Through,
    mut request: Request<Full<Bytes>>,
) -> Result<Response<Incoming>, Failed> {
    let stream = through.open().await?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            debug!("a stream to the hub's CLIProxyAPI failed: {err}");
        }
    });
    // HTTP/1.1 wants a host; the hub names the proxy's own in its place.
    request
        .headers_mut()
        .insert(HOST, HeaderValue::from_static("cliproxyapi"));
    Ok(sender.send_request(request).await?)
}

/// `body` with `<pin>/` before its JSON `model`, when it has one that has no pin yet; else as
/// it is.
fn pinned(body: Bytes, pin: &str) -> Bytes {
    let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return body;
    };
    let Some(model) = json.get("model").and_then(serde_json::Value::as_str) else {
        return body;
    };
    if model.contains('/') {
        return body;
    }
    json["model"] = format!("{pin}/{model}").into();
    serde_json::to_vec(&json).map_or(body, Bytes::from)
}

/// An error answer, shaped as the Anthropic API's are, which the CLI shows.
fn refuse(status: StatusCode, message: &str) -> Response<Body> {
    let body = serde_json::json!({
        "type": "error",
        "error": { "type": "api_error", "message": format!("herder: {message}") },
    });
    let mut response = Response::new(
        Full::new(Bytes::from(body.to_string()))
            .map_err(|never| match never {})
            .boxed(),
    );
    *response.status_mut() = status;
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pin_goes_before_a_bare_model_only() {
        let pin = |body: &str| {
            String::from_utf8(pinned(Bytes::from(body.to_owned()), "claude-ada").to_vec()).unwrap()
        };
        let body: serde_json::Value =
            serde_json::from_str(&pin(r#"{"model":"claude-opus-5-5","max_tokens":1}"#)).unwrap();
        assert_eq!(body["model"], "claude-ada/claude-opus-5-5");
        assert_eq!(body["max_tokens"], 1);
        assert_eq!(
            pin(r#"{"model":"other/claude-opus-5-5"}"#),
            r#"{"model":"other/claude-opus-5-5"}"#
        );
        assert_eq!(pin(r#"{"messages":[]}"#), r#"{"messages":[]}"#);
        assert_eq!(pin("not json"), "not json");
    }
}
