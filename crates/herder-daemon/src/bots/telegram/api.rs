//! Telegram's Bot API, over HTTPS: methods called with JSON, or with a file as a form.

use std::time::Duration;

use frankenstein::response::ErrorResponse;
use http_body_util::{BodyExt, Full, Limited};
use hyper::Request;
use hyper::body::Bytes;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// How long a call may take, long polls included.
const TIMEOUT: Duration = Duration::from_secs(90);

/// The most bytes of an answer read.
const ANSWER_BYTES: usize = 16 << 20;

/// How many times a call Telegram asks to retry later is tried.
const TRIES: usize = 3;

/// The longest wait for a retry Telegram asks for.
const MAX_RETRY_AFTER: u64 = 30;

/// A bot's Bot API.
pub struct Api {
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
    /// The bot's token, which every call's url holds.
    token: String,
}

/// Why a call failed.
#[derive(Debug)]
pub struct Failure {
    /// Telegram's error code, such as 401 for a refused token; `None` when Telegram did not
    /// answer.
    pub code: Option<u64>,
    /// What went wrong, as Telegram says it.
    pub description: String,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.description)
    }
}

/// A file sent with a call.
pub struct Upload<'a> {
    /// The parameter it is.
    pub field: &'a str,
    pub name: &'a str,
    pub media_type: &'a str,
    pub data: Vec<u8>,
}

impl Api {
    pub fn new(token: &str) -> Result<Self, String> {
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .map_err(|err| format!("cannot load the system's certificates: {err}"))?
            .https_only()
            .enable_http1()
            .build();
        Ok(Self {
            client: Client::builder(TokioExecutor::new()).build(https),
            token: token.to_owned(),
        })
    }

    /// Calls `method` with `params`.
    pub async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: &impl Serialize,
    ) -> Result<T, Failure> {
        let body = serde_json::to_vec(params).map_err(|err| Failure {
            code: None,
            description: format!("cannot build the request: {err}"),
        })?;
        self.send(method, "application/json", body).await
    }

    /// Calls `method` with `fields` and `upload`, as a form.
    pub async fn upload<T: DeserializeOwned>(
        &self,
        method: &str,
        fields: &[(&str, String)],
        upload: Upload<'_>,
    ) -> Result<T, Failure> {
        let boundary = format!("herder-{}", uuid::Uuid::now_v7().simple());
        let mut body = Vec::new();
        for (name, value) in fields {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n\
                     {value}\r\n"
                )
                .as_bytes(),
            );
        }
        let file_name: String = upload
            .name
            .chars()
            .map(|c| {
                if matches!(c, '"' | '\r' | '\n') {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"; \
                 filename=\"{file_name}\"\r\nContent-Type: {}\r\n\r\n",
                upload.field, upload.media_type
            )
            .as_bytes(),
        );
        body.extend_from_slice(&upload.data);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let content_type = format!("multipart/form-data; boundary={boundary}");
        self.send(method, &content_type, body).await
    }

    /// The file at `file_path`, as `getFile` names it, if it holds at most `limit` bytes.
    pub async fn download(&self, file_path: &str, limit: usize) -> Result<Vec<u8>, String> {
        let url = format!(
            "https://api.telegram.org/file/bot{}/{file_path}",
            self.token
        );
        let request = Request::get(url)
            .body(Full::default())
            .map_err(|err| format!("cannot build the request: {err}"))?;
        let response = tokio::time::timeout(TIMEOUT, self.client.request(request))
            .await
            .map_err(|_| "Telegram did not answer in time".to_owned())?
            .map_err(|err| format!("cannot reach Telegram: {err}"))?;
        if !response.status().is_success() {
            return Err(format!("Telegram answered {}", response.status()));
        }
        let body = Limited::new(response.into_body(), limit)
            .collect()
            .await
            .map_err(|err| format!("cannot download it: {err}"))?;
        Ok(body.to_bytes().to_vec())
    }

    async fn send<T: DeserializeOwned>(
        &self,
        method: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<T, Failure> {
        let body = Bytes::from(body);
        let mut tries = 0;
        loop {
            tries += 1;
            let answer = self.once(method, content_type, body.clone()).await?;
            if answer["ok"] == true {
                return serde_json::from_value(answer["result"].clone()).map_err(|err| Failure {
                    code: None,
                    description: format!("cannot read Telegram's answer to {method}: {err}"),
                });
            }
            let error: ErrorResponse = serde_json::from_value(answer).map_err(|err| Failure {
                code: None,
                description: format!("cannot read Telegram's error for {method}: {err}"),
            })?;
            let retry_after = error
                .parameters
                .as_ref()
                .and_then(|parameters| parameters.retry_after)
                .map(u64::from);
            match retry_after {
                Some(seconds) if tries < TRIES && seconds <= MAX_RETRY_AFTER => {
                    tokio::time::sleep(Duration::from_secs(seconds)).await;
                }
                _ => {
                    return Err(Failure {
                        code: Some(error.error_code),
                        description: error.description,
                    });
                }
            }
        }
    }

    /// Sends one request; resolves to Telegram's answer, whether it is ok or an error.
    async fn once(&self, method: &str, content_type: &str, body: Bytes) -> Result<Value, Failure> {
        let unreachable = |description: String| Failure {
            code: None,
            description,
        };
        let url = format!("https://api.telegram.org/bot{}/{method}", self.token);
        let request = Request::post(url)
            .header(hyper::header::CONTENT_TYPE, content_type)
            .body(Full::new(body))
            .map_err(|err| unreachable(format!("cannot build the request: {err}")))?;
        let response = tokio::time::timeout(TIMEOUT, self.client.request(request))
            .await
            .map_err(|_| unreachable("Telegram did not answer in time".into()))?
            .map_err(|err| unreachable(format!("cannot reach Telegram: {err}")))?;
        let status = response.status();
        let body = Limited::new(response.into_body(), ANSWER_BYTES)
            .collect()
            .await
            .map_err(|err| unreachable(format!("cannot read Telegram's answer: {err}")))?
            .to_bytes();
        serde_json::from_slice(&body)
            .map_err(|_| unreachable(format!("Telegram answered {status} to {method}")))
    }
}
