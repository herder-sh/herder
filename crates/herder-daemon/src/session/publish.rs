//! `publish`: an agent shows an artifact of its work, a screenshot, recording, page, log or
//! any other file, in its thread, with a public link that opens anywhere.
//!
//! The daemon reads the file, or takes the page, and keeps it with the session's attachments
//! ([`super::attachments`]), so clients render it from `get_attachment` whatever the link does.
//! Unless the session's project keeps artifacts private, a [`Publisher`] uploads it first for
//! its public link; a failed upload fails the call, keeping nothing. The session's journal
//! records `artifact_published` with the file and the link: clients render that event, for
//! every provider alike, and the vault and forks copy the file as they do a prompt's.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use herder_protocol::{
    Attachment, AttachmentId, Bytes, EventBody, FILE_MEDIA_TYPE, MAX_FILE_BYTES, SessionId,
    Timestamp,
};
use herder_tasktools::{ErrorCode, PublishInput, PublishOutput, ToolError};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{SessionManager, attachments, projects};

/// Most bytes an artifact may have: as many as a prompt's file, so a client fetches it in one
/// message.
pub const MAX_ARTIFACT_BYTES: usize = MAX_FILE_BYTES;

/// A public link to an uploaded artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// Opens the artifact for anyone who has it.
    pub url: String,
    /// When the link stops working; `None` when it never does.
    pub expires_at: Option<Timestamp>,
}

/// What [`Publisher::upload`] returns: the link, or why there is none, for the agent.
pub type UploadFuture = Pin<Box<dyn Future<Output = Result<Link, String>> + Send>>;

/// Where artifacts are uploaded for their public links.
pub trait Publisher: Send + Sync {
    /// Uploads the file `name`, of media type `media_type`, holding `data`.
    fn upload(&self, name: String, media_type: &'static str, data: Bytes) -> UploadFuture;
}

/// [krowk](https://krowk.com), without an account: each artifact's link lasts a day.
pub struct Krowk {
    api: String,
}

impl Krowk {
    /// krowk's API.
    pub fn new() -> Self {
        Self::at("https://api.krowk.com")
    }

    fn at(api: &str) -> Self {
        Self {
            api: api.trim_end_matches('/').to_owned(),
        }
    }
}

impl Default for Krowk {
    fn default() -> Self {
        Self::new()
    }
}

impl Publisher for Krowk {
    fn upload(&self, name: String, media_type: &'static str, data: Bytes) -> UploadFuture {
        let api = self.api.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || krowk_upload(&api, &name, media_type, &data.0))
                .await
                .map_err(|err| format!("the upload stopped: {err}"))?
        })
    }
}

/// krowk's answer to declaring an artifact.
#[derive(Deserialize)]
struct Declared {
    slug: String,
    url: String,
    expires_at: Option<Timestamp>,
    upload: Target,
    claim_token: Option<String>,
}

/// Where and how to send an artifact's bytes.
#[derive(Deserialize)]
struct Target {
    url: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
}

/// Declares the artifact, puts its bytes where krowk says, then finalizes it: krowk's upload,
/// blocking.
fn krowk_upload(api: &str, name: &str, media_type: &str, data: &[u8]) -> Result<Link, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(300)))
        .http_status_as_error(false)
        .build()
        .into();
    let checksum: String = Sha256::digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let declare = serde_json::json!({
        "filename": name,
        "content_type": media_type,
        "byte_size": data.len(),
        "checksum": checksum,
    });
    let declared: Declared = serde_json::from_str(&answer(
        agent
            .post(format!("{api}/v1/artifacts"))
            .header("Content-Type", "application/json")
            .send(declare.to_string()),
    )?)
    .map_err(|err| format!("krowk answered the upload unexpectedly: {err}"))?;
    let mut put = agent.put(&declared.upload.url);
    for (header, value) in &declared.upload.headers {
        put = put.header(header, value);
    }
    answer(put.send(data))?;
    let finalize = serde_json::json!({ "claim_token": declared.claim_token });
    answer(
        agent
            .put(format!("{api}/v1/artifacts/{}/finalization", declared.slug))
            .header("Content-Type", "application/json")
            .send(finalize.to_string()),
    )?;
    Ok(Link {
        url: declared.url,
        expires_at: declared.expires_at,
    })
}

/// The body of a successful response; otherwise what went wrong, with krowk's own message.
fn answer(
    response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<String, String> {
    let mut response = response.map_err(|err| format!("cannot reach krowk: {err}"))?;
    let status = response.status();
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|err| format!("cannot read krowk's answer: {err}"))?;
    if status.is_success() {
        return Ok(body);
    }
    let message = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|json| json["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or(body);
    Err(format!("krowk refused the upload ({status}): {message}"))
}

/// The media type of a file named `name`, by its extension; clients pick how to render an
/// artifact the same way.
pub fn media_type(name: &str) -> &'static str {
    let extension = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("mp4" | "m4v") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        Some("html" | "htm") => "text/html",
        Some("md" | "markdown") => "text/markdown",
        Some("json") => "application/json",
        Some("csv") => "text/csv",
        Some("diff" | "patch") => "text/x-diff",
        Some("txt" | "log") => "text/plain",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// The file name an HTML page titled `title` is kept under.
fn page_name(title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    let slug: String = slug.chars().take(60).collect();
    if slug.is_empty() {
        "page.html".to_owned()
    } else {
        format!("{slug}.html")
    }
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::new(ErrorCode::InvalidArguments, message)
}

fn internal(message: impl Into<String>) -> ToolError {
    ToolError::new(ErrorCode::Internal, message)
}

impl SessionManager {
    /// `publish` for `caller`.
    pub(super) async fn publish(
        &self,
        caller: SessionId,
        input: PublishInput,
    ) -> Result<PublishOutput, ToolError> {
        let title = input.title.trim();
        if title.is_empty() {
            return Err(invalid(
                "`title` is empty; pass a short label for the artifact",
            ));
        }
        let session = self
            .inner
            .journal
            .session(caller.clone())
            .await
            .map_err(|err| internal(format!("{err:#}")))?
            .ok_or_else(|| internal(format!("the calling session {caller} does not exist")))?;
        let (name, data) = match (input.path, input.html) {
            (Some(path), None) => read(&Path::new(&session.worktree).join(path)).await?,
            (None, Some(html)) if html.trim().is_empty() => {
                return Err(invalid("`html` is empty; pass a complete HTML document"));
            }
            (None, Some(html)) => (page_name(title), html.into_bytes()),
            _ => return Err(invalid("pass exactly one of `path` and `html`")),
        };
        if data.len() > MAX_ARTIFACT_BYTES {
            return Err(invalid(format!(
                "the artifact is {} bytes, over the 10 MiB limit ({MAX_ARTIFACT_BYTES} bytes); \
                 shorten, downscale or compress it",
                data.len()
            )));
        }
        let data = Bytes(data);
        let link = match self.inner.publisher.get() {
            Some(publisher) if !self.inner.private_artifacts(Path::new(&session.repo)).await => {
                let link = publisher.upload(name.clone(), media_type(&name), data.clone());
                Some(link.await.map_err(|message| {
                    ToolError::new(
                        ErrorCode::UploadFailed,
                        format!("{message}; nothing was published, retry later"),
                    )
                })?)
            }
            _ => None,
        };
        let attachment = Attachment {
            attachment_id: AttachmentId::new(ulid::Ulid::new().to_string()),
            media_type: FILE_MEDIA_TYPE.to_owned(),
            size: data.0.len() as u64,
            name: Some(name),
        };
        attachments::keep(
            &self.inner.attachments,
            &caller,
            vec![(attachment.clone(), data)],
        )
        .await
        .map_err(|err| internal(err.message))?;
        let (url, expires_at) = link.map_or((None, None), |link| (Some(link.url), link.expires_at));
        let body = EventBody::ArtifactPublished {
            title: title.to_owned(),
            attachment,
            url: url.clone(),
            expires_at,
        };
        self.inner
            .journal
            .record(caller, None, body)
            .await
            .map_err(|err| internal(format!("{err:#}")))?;
        Ok(PublishOutput { url, expires_at })
    }
}

/// The name and bytes of the file at `path`, within [`MAX_ARTIFACT_BYTES`].
async fn read(path: &Path) -> Result<(String, Vec<u8>), ToolError> {
    let shown = path.display();
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|err| invalid(format!("cannot read {shown}: {err}")))?;
    if !metadata.is_file() {
        return Err(invalid(format!("{shown} is not a file")));
    }
    if metadata.len() > MAX_ARTIFACT_BYTES as u64 {
        return Err(invalid(format!(
            "{shown} is {} bytes, over the 10 MiB limit ({MAX_ARTIFACT_BYTES} bytes); shorten, \
             downscale or compress it",
            metadata.len()
        )));
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| invalid(format!("{shown} names no file")))?;
    let data = tokio::fs::read(path)
        .await
        .map_err(|err| invalid(format!("cannot read {shown}: {err}")))?;
    Ok((name, data))
}

impl super::Inner {
    /// Whether `repo`'s project keeps its artifacts private.
    async fn private_artifacts(&self, repo: &Path) -> bool {
        let Some((host, overrides)) = self.projects.get().cloned() else {
            return false;
        };
        let entries = overrides.config().entries;
        let repo = repo.to_owned();
        tokio::task::spawn_blocking(move || projects::of_repo(&host, &repo, &entries))
            .await
            .ok()
            .flatten()
            .is_some_and(|project| project.private_artifacts)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    use super::*;

    #[test]
    fn media_types_follow_the_extension() {
        assert_eq!(media_type("shot.PNG"), "image/png");
        assert_eq!(media_type("flow.mov"), "video/quicktime");
        assert_eq!(media_type("report.html"), "text/html");
        assert_eq!(media_type("test.log"), "text/plain");
        assert_eq!(media_type("Makefile"), "application/octet-stream");
    }

    #[test]
    fn pages_are_named_after_their_title() {
        assert_eq!(
            page_name("Request latency, by endpoint"),
            "request-latency-by-endpoint.html"
        );
        assert_eq!(page_name("  ✓  "), "page.html");
        assert_eq!(page_name(&"a".repeat(100)).len(), 65);
    }

    /// One request a fake krowk got: its request line, headers and body.
    struct Request {
        line: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    /// A fake krowk: answers each request with the next of the answers `answers` gives for
    /// its address, a status and a body; returns its address and, once done, the requests.
    fn fake_krowk(
        answers: impl FnOnce(&str) -> Vec<(u16, String)>,
    ) -> (String, std::thread::JoinHandle<Vec<Request>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let answers = answers(&base);
        let served = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, body) in answers {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut headers = Vec::new();
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    let header = header.trim_end();
                    if header.is_empty() {
                        break;
                    }
                    let (name, value) = header.split_once(':').unwrap();
                    headers.push((name.to_ascii_lowercase(), value.trim().to_owned()));
                }
                let length = headers
                    .iter()
                    .find(|(name, _)| name == "content-length")
                    .map_or(0, |(_, value)| value.parse().unwrap());
                let mut content = vec![0; length];
                reader.read_exact(&mut content).unwrap();
                requests.push(Request {
                    line: line.trim_end().to_owned(),
                    headers,
                    body: content,
                });
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            requests
        });
        (base, served)
    }

    fn header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
        request
            .headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
    }

    #[tokio::test]
    async fn krowk_declares_puts_and_finalizes_an_artifact() {
        let (base, served) = fake_krowk(|base| {
            let declared = serde_json::json!({
                "slug": "art_1",
                "state": "pending",
                "url": "https://krowk.com/a/art_1",
                "expires_at": "2026-10-11T17:57:01.233Z",
                "upload": {
                    "method": "PUT",
                    "url": format!("{base}/bucket/shot.png?sig=1"),
                    "headers": { "Content-Type": "image/png", "x-amz-checksum-sha256": "abc=" }
                },
                "claim_token": "krowk_claim_1"
            });
            vec![
                (201, declared.to_string()),
                (200, String::new()),
                (200, r#"{"slug":"art_1","state":"ready"}"#.to_owned()),
            ]
        });
        let png = Bytes(b"\x89PNG\r\n\x1a\nrest".to_vec());
        let link = Krowk::at(&base)
            .upload("shot.png".into(), "image/png", png.clone())
            .await
            .unwrap();
        assert_eq!(
            link,
            Link {
                url: "https://krowk.com/a/art_1".into(),
                expires_at: Some("2026-10-11T17:57:01.233Z".parse().unwrap()),
            }
        );
        let requests = served.join().unwrap();
        assert_eq!(requests[0].line, "POST /v1/artifacts HTTP/1.1");
        let declare: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let checksum: String = Sha256::digest(&png.0)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(
            declare,
            serde_json::json!({
                "filename": "shot.png",
                "content_type": "image/png",
                "byte_size": 12,
                "checksum": checksum,
            })
        );
        assert_eq!(requests[1].line, "PUT /bucket/shot.png?sig=1 HTTP/1.1");
        assert_eq!(header(&requests[1], "x-amz-checksum-sha256"), Some("abc="));
        assert_eq!(header(&requests[1], "content-type"), Some("image/png"));
        assert_eq!(requests[1].body, png.0);
        assert_eq!(
            requests[2].line,
            "PUT /v1/artifacts/art_1/finalization HTTP/1.1"
        );
        let finalize: serde_json::Value = serde_json::from_slice(&requests[2].body).unwrap();
        assert_eq!(
            finalize,
            serde_json::json!({ "claim_token": "krowk_claim_1" })
        );
    }

    #[tokio::test]
    async fn a_refused_upload_says_why() {
        let (base, served) = fake_krowk(|_| {
            vec![(
                429,
                r#"{"error":{"code":"rate_limited","message":"Too many uploads"}}"#.to_owned(),
            )]
        });
        let error = Krowk::at(&base)
            .upload("log.txt".into(), "text/plain", Bytes(b"ok".to_vec()))
            .await
            .unwrap_err();
        assert!(
            error.contains("429") && error.contains("Too many uploads"),
            "{error}"
        );
        served.join().unwrap();
    }
}
