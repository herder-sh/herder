//! GitHub, as the PR tracker reads it: conditional REST `GET`s and GraphQL queries through the
//! machine's `gh`, and the mapping from GitHub's JSON to [`PullRequest`].
//!
//! herder never holds a GitHub token: [`GhCli`] runs `gh api`, which uses whatever login `gh`
//! has on this machine.
//!
//! Review threads are only in GraphQL, which has no conditional requests; see
//! [`REVIEW_THREADS`].

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use herder_protocol::{CiStatus, Mergeable, PrState, PullRequest, ReviewStatus};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

/// How long one `gh api` call may take.
const GH_TIMEOUT: Duration = Duration::from_secs(30);

/// What a conditional `GET` returned.
#[derive(Clone, Debug, PartialEq)]
pub enum Fetched {
    /// `200`: the resource, with the ETag to send next time.
    Modified {
        /// The response's `ETag`, if it sent one.
        etag: Option<String>,
        /// The JSON body.
        body: serde_json::Value,
    },
    /// `304`: unchanged since the ETag sent.
    NotModified,
    /// `404`: no such resource, or no access to it.
    NotFound,
}

/// Future returned by [`GitHub::get`].
pub type GetFuture<'a> = Pin<Box<dyn Future<Output = Result<Fetched>> + Send + 'a>>;

/// Future returned by [`GitHub::graphql`].
pub type GraphqlFuture<'a> = Pin<Box<dyn Future<Output = Result<serde_json::Value>> + Send + 'a>>;

/// The GraphQL query for a pull request's review threads, with the variables `owner`, `name`
/// and `number`.
pub const REVIEW_THREADS: &str = "query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) { reviewThreads(first: 100) { nodes { isResolved } } }
  }
}";

/// Read access to GitHub's REST and GraphQL APIs.
pub trait GitHub: Send + Sync + 'static {
    /// `GET`s `path`, such as `repos/acme/app/pulls/1`, on the API of `host`, such as
    /// `github.com`; with `etag`, only if the resource changed since.
    fn get<'a>(&'a self, host: &'a str, path: &'a str, etag: Option<&'a str>) -> GetFuture<'a>;

    /// Runs the GraphQL `query` with `variables`, a JSON object of strings and numbers, on the
    /// API of `host`; returns the response's `data`.
    fn graphql<'a>(
        &'a self,
        host: &'a str,
        query: &'a str,
        variables: &'a serde_json::Value,
    ) -> GraphqlFuture<'a>;
}

/// [`GitHub`] through the `gh` CLI and its login on this machine.
#[derive(Clone, Copy, Debug, Default)]
pub struct GhCli;

impl GitHub for GhCli {
    fn get<'a>(&'a self, host: &'a str, path: &'a str, etag: Option<&'a str>) -> GetFuture<'a> {
        Box::pin(async move {
            let mut command = Command::new("gh");
            command
                .args(["api", "--hostname", host, "--include"])
                .args(["-H", "Accept: application/vnd.github+json"]);
            if let Some(etag) = etag {
                command.arg("-H").arg(format!("If-None-Match: {etag}"));
            }
            command
                .arg(path)
                .env("GH_NO_UPDATE_NOTIFIER", "1")
                .env("GH_PROMPT_DISABLED", "1")
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true);
            let output = tokio::time::timeout(GH_TIMEOUT, command.output())
                .await
                .map_err(|_| anyhow!("gh api {path} timed out"))?
                .context("running gh")?;
            // `gh` exits non-zero on 304 and 404 too; the status line is what counts.
            let response = String::from_utf8_lossy(&output.stdout);
            parse_response(&response).with_context(|| {
                format!(
                    "gh api {path}: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )
            })
        })
    }

    fn graphql<'a>(
        &'a self,
        host: &'a str,
        query: &'a str,
        variables: &'a serde_json::Value,
    ) -> GraphqlFuture<'a> {
        Box::pin(async move {
            let mut command = Command::new("gh");
            command
                .args(["api", "graphql", "--hostname", host, "-f"])
                .arg(format!("query={query}"));
            for (name, value) in variables.as_object().into_iter().flatten() {
                // `-f` passes a string, `-F` a number as a number.
                match value {
                    serde_json::Value::String(value) => {
                        command.arg("-f").arg(format!("{name}={value}"))
                    }
                    value => command.arg("-F").arg(format!("{name}={value}")),
                };
            }
            command
                .env("GH_NO_UPDATE_NOTIFIER", "1")
                .env("GH_PROMPT_DISABLED", "1")
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true);
            let output = tokio::time::timeout(GH_TIMEOUT, command.output())
                .await
                .map_err(|_| anyhow!("gh api graphql timed out"))?
                .context("running gh")?;
            parse_graphql(&output.stdout).with_context(|| {
                format!(
                    "gh api graphql: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )
            })
        })
    }
}

/// The `data` of a GraphQL response; an error when it reports any.
pub(crate) fn parse_graphql(body: &[u8]) -> Result<serde_json::Value> {
    let mut response: serde_json::Value =
        serde_json::from_slice(body).context("decoding the response")?;
    if let Some(errors) = response.get("errors").and_then(|errors| errors.as_array())
        && let Some(first) = errors.first()
    {
        bail!(
            "{}",
            first
                .get("message")
                .and_then(|message| message.as_str())
                .unwrap_or("GraphQL error")
        );
    }
    match response.get_mut("data") {
        Some(data) => Ok(data.take()),
        None => bail!("no data in the response"),
    }
}

/// How many of a pull request's review threads are unresolved, from the `data` of
/// [`REVIEW_THREADS`]; `None` when the pull request is not there.
pub(crate) fn unresolved_threads(data: &serde_json::Value) -> Option<u32> {
    let nodes = data
        .pointer("/repository/pullRequest/reviewThreads/nodes")?
        .as_array()?;
    let unresolved = nodes
        .iter()
        .filter(|thread| thread.get("isResolved") == Some(&serde_json::Value::Bool(false)))
        .count();
    Some(u32::try_from(unresolved).unwrap_or(u32::MAX))
}

/// Parses `gh api --include` output: a status line, headers, a blank line, the body.
pub(crate) fn parse_response(response: &str) -> Result<Fetched> {
    let mut lines = response.split('\n').map(|line| line.trim_end_matches('\r'));
    let status_line = lines.next().unwrap_or_default();
    let status: u16 = status_line
        .strip_prefix("HTTP/")
        .and_then(|rest| rest.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| anyhow!("no HTTP response"))?;
    let mut etag = None;
    for line in lines.by_ref() {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("etag")
        {
            etag = Some(value.trim().to_owned());
        }
    }
    match status {
        200 => {
            let body: Vec<&str> = lines.collect();
            let body = serde_json::from_str(&body.join("\n")).context("decoding the body")?;
            Ok(Fetched::Modified { etag, body })
        }
        304 => Ok(Fetched::NotModified),
        404 => Ok(Fetched::NotFound),
        _ => bail!("{status_line}"),
    }
}

/// A repository on a GitHub host.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GhRepo {
    /// API host, such as `github.com`.
    pub host: String,
    /// Owning user or organization.
    pub owner: String,
    /// Repository name.
    pub name: String,
}

impl GhRepo {
    /// The repository a git remote URL points at: scp-like (`git@host:owner/name.git`) or a
    /// URL (`https://`, `ssh://`, `git://`). `None` for anything else, such as a local path.
    pub fn from_url(url: &str) -> Option<Self> {
        let (authority, path) = match url.split_once("://") {
            Some((scheme, rest)) => {
                if !matches!(scheme, "https" | "http" | "ssh" | "git" | "git+ssh") {
                    return None;
                }
                rest.split_once('/')?
            }
            // scp-like: a colon before any slash.
            None => {
                let (host, path) = url.split_once(':')?;
                if host.contains('/') {
                    return None;
                }
                (host, path)
            }
        };
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        let host = host.split_once(':').map_or(host, |(host, _)| host);
        // GitHub's SSH-over-443 endpoint serves the same repositories.
        let host = if host == "ssh.github.com" {
            "github.com"
        } else {
            host
        };
        let path = path.trim_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        let (owner, name) = path.split_once('/')?;
        if host.is_empty() || owner.is_empty() || name.is_empty() || name.contains('/') {
            return None;
        }
        Some(Self {
            host: host.to_lowercase(),
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }

    /// `owner/name`.
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// A pull request as `GET repos/{o}/{r}/pulls/{n}` and the list endpoints return it.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ApiPull {
    pub(crate) number: u64,
    pub(crate) html_url: String,
    pub(crate) title: String,
    pub(crate) state: String,
    #[serde(default)]
    pub(crate) draft: bool,
    #[serde(default)]
    pub(crate) merged_at: Option<String>,
    #[serde(default)]
    pub(crate) mergeable: Option<bool>,
    pub(crate) created_at: jiff::Timestamp,
    pub(crate) head: ApiHead,
    #[serde(default)]
    pub(crate) requested_reviewers: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) requested_teams: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ApiHead {
    pub(crate) sha: String,
    /// The branch, for a pull request whose head repository still exists.
    #[serde(default, rename = "ref")]
    pub(crate) branch: Option<String>,
}

/// One entry of `GET repos/{o}/{r}/pulls/{n}/commits`.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ApiCommit {
    pub(crate) commit: ApiCommitBody,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ApiCommitBody {
    pub(crate) message: String,
}

/// `GET repos/{o}/{r}/commits/{sha}/check-runs`.
#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct ApiCheckRuns {
    #[serde(default)]
    pub(crate) check_runs: Vec<ApiCheckRun>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ApiCheckRun {
    #[serde(default)]
    pub(crate) name: String,
    pub(crate) status: String,
    #[serde(default)]
    pub(crate) conclusion: Option<String>,
}

/// `GET repos/{o}/{r}/commits/{sha}/status`: the commit statuses of the older status API.
#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct ApiStatus {
    pub(crate) state: String,
    #[serde(default)]
    pub(crate) total_count: u64,
    #[serde(default)]
    pub(crate) statuses: Vec<ApiCommitStatus>,
}

/// One status of [`ApiStatus`]: the latest of its context.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ApiCommitStatus {
    pub(crate) context: String,
    pub(crate) state: String,
}

/// One entry of `GET repos/{o}/{r}/pulls/{n}/reviews`.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ApiReview {
    #[serde(default)]
    pub(crate) user: Option<ApiUser>,
    pub(crate) state: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ApiUser {
    pub(crate) login: String,
}

/// The pull request herder tracks, from GitHub's pull request, its head commit's checks and
/// statuses, its reviews, and how many of its review threads are unresolved.
pub(crate) fn pull_request(
    pull: &ApiPull,
    runs: &ApiCheckRuns,
    status: &ApiStatus,
    reviews: &[ApiReview],
    unresolved_threads: Option<u32>,
) -> PullRequest {
    PullRequest {
        number: pull.number,
        url: pull.html_url.clone(),
        title: pull.title.clone(),
        head_branch: pull.head.branch.clone(),
        head_sha: Some(pull.head.sha.clone()),
        unresolved_threads,
        state: state(pull),
        ci: ci(runs, status),
        review: review(pull, reviews),
        mergeable: match pull.mergeable {
            Some(true) => Mergeable::Clean,
            Some(false) => Mergeable::Conflicting,
            None => Mergeable::Unknown,
        },
    }
}

pub(crate) fn state(pull: &ApiPull) -> PrState {
    if pull.merged_at.is_some() {
        PrState::Merged
    } else if pull.state == "closed" {
        PrState::Closed
    } else if pull.draft {
        PrState::Draft
    } else {
        PrState::Open
    }
}

/// The names of the check runs and status contexts that failed, as [`ci`] counts them.
pub(crate) fn failing_checks(runs: &ApiCheckRuns, status: &ApiStatus) -> Vec<String> {
    let runs = runs.check_runs.iter().filter(|run| {
        run.status == "completed"
            && !matches!(
                run.conclusion.as_deref(),
                Some("success" | "neutral" | "skipped")
            )
    });
    let statuses = status
        .statuses
        .iter()
        .filter(|status| !matches!(status.state.as_str(), "success" | "pending"));
    let mut names: Vec<String> = runs.map(|run| run.name.clone()).collect();
    names.extend(statuses.map(|status| status.context.clone()));
    names.retain(|name| !name.is_empty());
    names.dedup();
    names
}

/// Failing if any check run or status failed, else pending if any is still running, else
/// passing if there are any, else none.
fn ci(runs: &ApiCheckRuns, status: &ApiStatus) -> CiStatus {
    let mut failing = false;
    let mut pending = false;
    for run in &runs.check_runs {
        if run.status != "completed" {
            pending = true;
            continue;
        }
        match run.conclusion.as_deref() {
            Some("success" | "neutral" | "skipped") => {}
            // failure, cancelled, timed_out, action_required, startup_failure, stale
            _ => failing = true,
        }
    }
    if status.total_count > 0 {
        match status.state.as_str() {
            "success" => {}
            "pending" => pending = true,
            _ => failing = true,
        }
    }
    if failing {
        CiStatus::Failing
    } else if pending {
        CiStatus::Pending
    } else if runs.check_runs.is_empty() && status.total_count == 0 {
        CiStatus::None
    } else {
        CiStatus::Passing
    }
}

/// Each reviewer's latest verdict decides: changes requested beats approved; with neither,
/// a pending review request means a review is required.
fn review(pull: &ApiPull, reviews: &[ApiReview]) -> ReviewStatus {
    // Oldest first; a later verdict, or a dismissal, replaces a reviewer's earlier one.
    let mut latest: Vec<(&str, &str)> = Vec::new();
    for review in reviews {
        let Some(user) = &review.user else { continue };
        if !matches!(
            review.state.as_str(),
            "APPROVED" | "CHANGES_REQUESTED" | "DISMISSED"
        ) {
            continue;
        }
        latest.retain(|(login, _)| *login != user.login);
        latest.push((&user.login, &review.state));
    }
    if latest.iter().any(|(_, s)| *s == "CHANGES_REQUESTED") {
        ReviewStatus::ChangesRequested
    } else if latest.iter().any(|(_, s)| *s == "APPROVED") {
        ReviewStatus::Approved
    } else if !pull.requested_reviewers.is_empty() || !pull.requested_teams.is_empty() {
        ReviewStatus::Required
    } else {
        ReviewStatus::None
    }
}

/// Percent-encodes a query parameter value; `:` and `/` are kept, as GitHub's `head` filter
/// and branch names use them.
pub(crate) fn encode_query(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~:/".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn repo(owner: &str, name: &str) -> Option<GhRepo> {
        Some(GhRepo {
            host: "github.com".into(),
            owner: owner.into(),
            name: name.into(),
        })
    }

    #[test]
    fn remote_urls_name_their_repository() {
        for url in [
            "git@github.com:acme/app.git",
            "git@github.com:acme/app",
            "https://github.com/acme/app.git",
            "https://github.com/acme/app/",
            "https://user:token@github.com/acme/app.git",
            "ssh://git@github.com/acme/app.git",
            "ssh://git@ssh.github.com:443/acme/app.git",
            "git://github.com/acme/app.git",
        ] {
            assert_eq!(GhRepo::from_url(url), repo("acme", "app"), "{url}");
        }
        assert_eq!(
            GhRepo::from_url("git@ghe.example.com:Team/Tool.git").map(|r| r.host),
            Some("ghe.example.com".into())
        );
        for url in [
            "/srv/git/app.git",
            "file:///srv/git/app.git",
            "./app",
            "https://github.com/acme",
        ] {
            assert_eq!(GhRepo::from_url(url), None, "{url}");
        }
    }

    #[test]
    fn gh_include_output_parses_by_status() {
        let ok = "HTTP/2.0 200 OK\r\nEtag: W/\"abc\"\r\nX-Other: 1\r\n\r\n{\"number\":1}";
        assert_eq!(
            parse_response(ok).unwrap(),
            Fetched::Modified {
                etag: Some("W/\"abc\"".into()),
                body: json!({"number": 1})
            }
        );
        let not_modified = "HTTP/2.0 304 Not Modified\r\nEtag: W/\"abc\"\r\n\r\n";
        assert_eq!(parse_response(not_modified).unwrap(), Fetched::NotModified);
        let missing = "HTTP/2.0 404 Not Found\r\n\r\n{\"message\":\"Not Found\"}";
        assert_eq!(parse_response(missing).unwrap(), Fetched::NotFound);
        let limited = "HTTP/2.0 403 Forbidden\r\n\r\n{}";
        assert!(parse_response(limited).is_err());
        assert!(parse_response("").is_err());
    }

    fn pull(value: serde_json::Value) -> ApiPull {
        let mut base = json!({
            "number": 7,
            "html_url": "https://github.com/acme/app/pull/7",
            "title": "Fix it",
            "state": "open",
            "draft": false,
            "merged_at": null,
            "mergeable": true,
            "created_at": "2026-10-01T00:00:00Z",
            "head": {"sha": "abc", "ref": "feature"},
            "requested_reviewers": [],
            "requested_teams": []
        });
        for (key, value) in value.as_object().unwrap() {
            base[key] = value.clone();
        }
        serde_json::from_value(base).unwrap()
    }

    fn runs(runs: &[(&str, Option<&str>)]) -> ApiCheckRuns {
        ApiCheckRuns {
            check_runs: runs
                .iter()
                .map(|(status, conclusion)| ApiCheckRun {
                    name: format!("{status}-{}", conclusion.unwrap_or("none")),
                    status: (*status).into(),
                    conclusion: conclusion.map(Into::into),
                })
                .collect(),
        }
    }

    fn reviews(reviews: &[(&str, &str)]) -> Vec<ApiReview> {
        reviews
            .iter()
            .map(|(login, state)| ApiReview {
                user: Some(ApiUser {
                    login: (*login).into(),
                }),
                state: (*state).into(),
            })
            .collect()
    }

    #[test]
    fn state_maps_merged_closed_draft_and_open() {
        assert_eq!(state(&pull(json!({}))), PrState::Open);
        assert_eq!(state(&pull(json!({"draft": true}))), PrState::Draft);
        assert_eq!(state(&pull(json!({"state": "closed"}))), PrState::Closed);
        let merged = json!({"state": "closed", "merged_at": "2026-10-02T00:00:00Z"});
        assert_eq!(state(&pull(merged)), PrState::Merged);
    }

    #[test]
    fn ci_rolls_up_check_runs_and_statuses() {
        let none = ApiStatus::default();
        assert_eq!(ci(&runs(&[]), &none), CiStatus::None);
        let passing = runs(&[
            ("completed", Some("success")),
            ("completed", Some("skipped")),
        ]);
        assert_eq!(ci(&passing, &none), CiStatus::Passing);
        let pending = runs(&[("completed", Some("success")), ("in_progress", None)]);
        assert_eq!(ci(&pending, &none), CiStatus::Pending);
        let failing = runs(&[("completed", Some("failure")), ("queued", None)]);
        assert_eq!(ci(&failing, &none), CiStatus::Failing);
        let status = |state: &str| ApiStatus {
            state: state.into(),
            total_count: 1,
            statuses: Vec::new(),
        };
        assert_eq!(ci(&runs(&[]), &status("success")), CiStatus::Passing);
        assert_eq!(ci(&passing, &status("pending")), CiStatus::Pending);
        assert_eq!(ci(&passing, &status("error")), CiStatus::Failing);
        // No statuses at all reports "pending" with a zero count.
        let empty = ApiStatus {
            state: "pending".into(),
            total_count: 0,
            statuses: Vec::new(),
        };
        assert_eq!(ci(&passing, &empty), CiStatus::Passing);
    }

    #[test]
    fn failing_checks_are_named() {
        let runs = runs(&[
            ("completed", Some("success")),
            ("completed", Some("failure")),
            ("completed", Some("timed_out")),
            ("in_progress", None),
        ]);
        let status = ApiStatus {
            state: "failure".into(),
            total_count: 2,
            statuses: vec![
                ApiCommitStatus {
                    context: "lint".into(),
                    state: "error".into(),
                },
                ApiCommitStatus {
                    context: "deploy".into(),
                    state: "success".into(),
                },
            ],
        };
        assert_eq!(
            failing_checks(&runs, &status),
            ["completed-failure", "completed-timed_out", "lint"]
        );
    }

    #[test]
    fn review_threads_count_the_unresolved_ones() {
        let recorded = include_bytes!("../../tests/fixtures/github/review_threads.json");
        let data = parse_graphql(recorded).unwrap();
        assert_eq!(unresolved_threads(&data), Some(2));
        let missing = br#"{"data":{"repository":{"pullRequest":null}}}"#;
        assert_eq!(unresolved_threads(&parse_graphql(missing).unwrap()), None);
        let failed = br#"{"data":null,"errors":[{"type":"NOT_FOUND","message":"Could not resolve to a Repository"}]}"#;
        let err = parse_graphql(failed).unwrap_err();
        assert!(err.to_string().contains("Could not resolve"), "{err}");
    }

    #[test]
    fn review_takes_each_reviewers_latest_verdict() {
        let open = pull(json!({}));
        assert_eq!(review(&open, &[]), ReviewStatus::None);
        let requested = pull(json!({"requested_reviewers": [{"login": "bob"}]}));
        assert_eq!(review(&requested, &[]), ReviewStatus::Required);
        let approved = reviews(&[("bob", "COMMENTED"), ("bob", "APPROVED")]);
        assert_eq!(review(&requested, &approved), ReviewStatus::Approved);
        let changes = reviews(&[("bob", "APPROVED"), ("eve", "CHANGES_REQUESTED")]);
        assert_eq!(review(&open, &changes), ReviewStatus::ChangesRequested);
        let resolved = reviews(&[("eve", "CHANGES_REQUESTED"), ("eve", "APPROVED")]);
        assert_eq!(review(&open, &resolved), ReviewStatus::Approved);
        let dismissed = reviews(&[("eve", "CHANGES_REQUESTED"), ("eve", "DISMISSED")]);
        assert_eq!(review(&open, &dismissed), ReviewStatus::None);
    }

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(encode_query("acme:herder/ab12"), "acme:herder/ab12");
        assert_eq!(encode_query("a b&c#d+é"), "a%20b%26c%23d%2B%C3%A9");
    }
}
