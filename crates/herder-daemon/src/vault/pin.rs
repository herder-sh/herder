//! The loopback proxy a host runs its vault logins' CLIs through, which pins each request to
//! one login of the vault's CLIProxyAPI.
//!
//! The proxy runs a request for `<pin>/<model>` on the login with that pin only
//! ([`super::cliproxyapi`]). A CLI picks its model names itself, for its main model, its
//! subagents and its side calls, and the proxy takes no alias such as `opus`, so the pin goes on
//! the names the CLI sends: an account on a vault login points its CLI at
//! `http://127.0.0.1:<port>/<pin>` ([`PinProxy::url`]), and this proxy forwards each request
//! to the vault's CLIProxyAPI with the pin taken off the path and put before the `model` of
//! its JSON body. Everything else passes as it is, the client key the CLI sends included, and
//! the answer streams back unchanged. herder never sees a provider token: the CLIProxyAPI
//! holds them.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::header::{CONNECTION, CONTENT_LENGTH, HOST, HeaderName, TRANSFER_ENCODING};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Largest request body forwarded: a prompt with its images.
const MAX_REQUEST_BYTES: usize = 256 * 1024 * 1024;

/// Where the vault's CLIProxyAPI is now, as the vault last said; `None` while it has not.
pub type Upstream = Arc<dyn Fn() -> Option<String> + Send + Sync>;

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
            .context("cannot listen on a loopback port for the vault logins")?;
        let addr = listener.local_addr()?;
        tokio::spawn(async move {
            loop {
                let stream = tokio::select! {
                    () = shutdown.cancelled() => return,
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _)) => stream,
                        Err(err) => {
                            warn!("the vault logins' proxy cannot accept: {err}");
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
                        debug!("a vault login's connection failed: {err}");
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
async fn forward(
    state: Arc<State>,
    request: Request<Incoming>,
) -> Result<Response<Body>, hyper_util::client::legacy::Error> {
    let (mut parts, body) = request.into_parts();
    let path = parts.uri.path().trim_start_matches('/');
    let (pin, rest) = path.split_once('/').unwrap_or((path, ""));
    if pin.is_empty() {
        return Ok(refuse(StatusCode::NOT_FOUND, "no vault login in the path"));
    }
    let Some(upstream) = (state.upstream)() else {
        return Ok(refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "the vault has not said where its CLIProxyAPI is",
        ));
    };
    let query = parts
        .uri
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let target = format!("{}/{rest}{query}", upstream.trim_end_matches('/'));
    let Ok(uri) = target.parse() else {
        return Ok(refuse(
            StatusCode::BAD_GATEWAY,
            "the vault's CLIProxyAPI address is not a URL",
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
    match state.client.request(request).await {
        Ok(response) => {
            let (mut parts, body) = response.into_parts();
            parts.headers.remove(CONNECTION);
            parts.headers.remove(HeaderName::from_static("keep-alive"));
            Ok(Response::from_parts(parts, body.boxed()))
        }
        Err(err) => {
            warn!("cannot reach the vault's CLIProxyAPI at {upstream}: {err}");
            Err(err)
        }
    }
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
