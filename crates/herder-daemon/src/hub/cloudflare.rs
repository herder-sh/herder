//! Deploys herder's relay Worker (`deploy/relay-worker/worker.js`) to its owner's own
//! Cloudflare account through the Cloudflare API, updates it, and removes it.
//!
//! The Worker runs at `https://herder-relay.<account subdomain>.workers.dev`: no domain
//! needed. Its one Durable Object class keeps its state in SQLite storage, which Cloudflare's
//! free plan includes. The hub's key is a secret binding of the script, so it is set again
//! with each upload, and only the hub and Cloudflare know it.
//!
//! Every call is blocking; the hub runs them on its blocking pool.

use std::io::Read;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The Worker's source, as deployed.
pub(super) const WORKER: &str = include_str!("../../../../deploy/relay-worker/worker.js");

/// The script's name in the account, and the first label of its workers.dev hostname.
pub(super) const SCRIPT: &str = "herder-relay";

/// Cloudflare's API.
pub(super) const API: &str = "https://api.cloudflare.com/client/v4";

/// The Workers runtime behaviour the Worker is written for; the same in its tests.
const COMPATIBILITY_DATE: &str = "2026-07-01";

/// The Durable Object migration that creates the relay's class; sent only to a script that
/// has not run it.
const MIGRATION: &str = "v1";

const TIMEOUT: Duration = Duration::from_secs(30);

/// Longest answer read from the API.
const MAX_ANSWER_BYTES: u64 = 1 << 20;

/// The relay as deployed: where it is, and what the hub needs to use and look after it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deployment {
    /// The Cloudflare API token it was deployed with, kept to update and remove it.
    pub api_token: String,
    /// The account it runs in.
    pub account_id: String,
    /// `https://herder-relay.<subdomain>.workers.dev`.
    pub url: String,
    /// The key the hub authenticates to it with.
    pub hub_key: String,
    /// SHA-256 of the Worker source deployed, as hex: a hub whose bundled source differs
    /// deploys it again.
    pub worker: String,
}

/// SHA-256 of the bundled Worker source, as hex.
pub(super) fn worker_hash() -> String {
    hex(&Sha256::digest(WORKER.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The Cloudflare API at `base`, as the holder of `token`.
pub(super) struct Api {
    base: String,
    token: String,
    agent: ureq::Agent,
}

/// Cloudflare's answer envelope.
#[derive(Deserialize)]
struct Answer {
    success: bool,
    #[serde(default)]
    errors: Vec<ApiError>,
    #[serde(default)]
    result: serde_json::Value,
}

#[derive(Deserialize)]
struct ApiError {
    code: i64,
    message: String,
}

impl Api {
    pub(super) fn new(base: &str, token: &str) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            base: base.trim_end_matches('/').to_owned(),
            token: token.trim().to_owned(),
            agent,
        }
    }

    /// Sends `method` to `path` with `body` of `content_type`; the answer's `result`, or
    /// `None` for a 404.
    fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<(String, Vec<u8>)>,
    ) -> Result<Option<serde_json::Value>> {
        let url = format!("{}{path}", self.base);
        let bearer = format!("Bearer {}", self.token);
        let response = match (method, body) {
            ("GET", _) => self.agent.get(&url).header("Authorization", &bearer).call(),
            ("DELETE", _) => self
                .agent
                .delete(&url)
                .header("Authorization", &bearer)
                .call(),
            (method, body) => {
                let (content_type, body) = body.unwrap_or_default();
                let request = if method == "PUT" {
                    self.agent.put(&url)
                } else {
                    self.agent.post(&url)
                };
                request
                    .header("Authorization", &bearer)
                    .header("Content-Type", &content_type)
                    .send(&body[..])
            }
        };
        let mut response = response.with_context(|| format!("cannot reach {url}"))?;
        let status = response.status().as_u16();
        let mut text = String::new();
        response
            .body_mut()
            .as_reader()
            .take(MAX_ANSWER_BYTES)
            .read_to_string(&mut text)
            .with_context(|| format!("cannot read {url}"))?;
        if status == 404 {
            return Ok(None);
        }
        let answer: Answer = serde_json::from_str(&text)
            .map_err(|_| anyhow!("Cloudflare answered {status} to {method} {path}"))?;
        if !answer.success || !(200..300).contains(&status) {
            let errors: Vec<String> = answer
                .errors
                .iter()
                .map(|error| format!("{} ({})", error.message, error.code))
                .collect();
            if matches!(status, 401 | 403) {
                bail!(
                    "Cloudflare refused the API token: {}. It needs the account permissions \
                     Workers Scripts: Edit and Account Settings: Read",
                    errors.join("; ")
                );
            }
            bail!("Cloudflare refused {method} {path}: {}", errors.join("; "));
        }
        Ok(Some(answer.result))
    }

    fn get(&self, path: &str) -> Result<serde_json::Value> {
        self.call("GET", path, None)?
            .with_context(|| format!("Cloudflare has no {path}"))
    }

    /// The one account the token reaches, or `account` if given.
    fn account(&self, account: Option<&str>) -> Result<String> {
        if let Some(account) = account {
            return Ok(account.to_owned());
        }
        let accounts = self.get("/accounts?per_page=50")?;
        let ids: Vec<(&str, &str)> = accounts
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|account| Some((account["id"].as_str()?, account["name"].as_str()?)))
            .collect();
        match ids.as_slice() {
            [(id, _)] => Ok((*id).to_owned()),
            [] => bail!("the API token reaches no Cloudflare account"),
            several => bail!(
                "the API token reaches several Cloudflare accounts; name one with --account: {}",
                several
                    .iter()
                    .map(|(id, name)| format!("{id} ({name})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    /// The account's workers.dev subdomain.
    fn subdomain(&self, account: &str) -> Result<String> {
        let result = self.call(
            "GET",
            &format!("/accounts/{account}/workers/subdomain"),
            None,
        )?;
        result
            .as_ref()
            .and_then(|result| result["subdomain"].as_str())
            .filter(|subdomain| !subdomain.is_empty())
            .map(str::to_owned)
            .context(
                "the Cloudflare account has no workers.dev subdomain yet; open Workers & Pages \
                 in the Cloudflare dashboard once to pick one, then deploy again",
            )
    }

    /// Whether the script exists and has run the relay's migration.
    fn migrated(&self, account: &str) -> Result<bool> {
        let scripts = self.get(&format!("/accounts/{account}/workers/scripts"))?;
        Ok(scripts
            .as_array()
            .into_iter()
            .flatten()
            .any(|script| script["id"] == SCRIPT && script["migration_tag"] == MIGRATION))
    }

    /// Uploads the Worker with `hub_key`, and the migration unless it ran.
    fn upload(&self, account: &str, hub_key: &str, migrate: bool) -> Result<()> {
        let mut metadata = serde_json::json!({
            "main_module": "worker.js",
            "compatibility_date": COMPATIBILITY_DATE,
            "bindings": [
                { "type": "durable_object_namespace", "name": "RELAY", "class_name": "Relay" },
                { "type": "secret_text", "name": "HUB_KEY", "text": hub_key },
            ],
        });
        if migrate {
            metadata["migrations"] =
                serde_json::json!({ "new_tag": MIGRATION, "new_sqlite_classes": ["Relay"] });
        }
        let boundary = format!("herder-{}", hex(&rand_bytes::<12>()?));
        let mut body = Vec::new();
        for (name, file, content_type, content) in [
            (
                "metadata",
                None,
                "application/json",
                metadata.to_string().into_bytes(),
            ),
            (
                "worker.js",
                Some("worker.js"),
                "application/javascript+module",
                WORKER.as_bytes().to_vec(),
            ),
        ] {
            body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            let filename = file
                .map(|f| format!("; filename=\"{f}\""))
                .unwrap_or_default();
            body.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"{name}\"{filename}\r\n\
                     Content-Type: {content_type}\r\n\r\n"
                )
                .as_bytes(),
            );
            body.extend_from_slice(&content);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        let content_type = format!("multipart/form-data; boundary={boundary}");
        self.call(
            "PUT",
            &format!("/accounts/{account}/workers/scripts/{SCRIPT}"),
            Some((content_type, body)),
        )?
        .context("Cloudflare has no Workers API for the account")?;
        Ok(())
    }

    /// Serves the script at its workers.dev hostname.
    fn enable_workers_dev(&self, account: &str) -> Result<()> {
        let body = serde_json::json!({ "enabled": true, "previews_enabled": false });
        self.call(
            "POST",
            &format!("/accounts/{account}/workers/scripts/{SCRIPT}/subdomain"),
            Some(("application/json".to_owned(), body.to_string().into_bytes())),
        )?
        .context("Cloudflare has no such script")?;
        Ok(())
    }

    /// Deletes the script, with its Durable Object's storage; gone already is fine.
    fn delete(&self, account: &str) -> Result<()> {
        self.call(
            "DELETE",
            &format!("/accounts/{account}/workers/scripts/{SCRIPT}?force=true"),
            None,
        )?;
        Ok(())
    }
}

fn rand_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
        .map_err(|_| anyhow!("no randomness"))?;
    Ok(bytes)
}

/// Deploys the bundled Worker with `token`, to `account` or the token's one account: a new
/// relay with a new hub key, or, given `current` in the same account, the same relay updated
/// and its key kept.
pub(super) fn deploy(
    base: &str,
    token: &str,
    account: Option<&str>,
    current: Option<&Deployment>,
) -> Result<Deployment> {
    let api = Api::new(base, token);
    let account = api.account(account.or(current.map(|c| c.account_id.as_str())))?;
    let subdomain = api.subdomain(&account)?;
    let hub_key = match current {
        Some(current) if current.account_id == account => current.hub_key.clone(),
        _ => hex(&rand_bytes::<32>()?),
    };
    let migrate = !api.migrated(&account)?;
    api.upload(&account, &hub_key, migrate)?;
    api.enable_workers_dev(&account)?;
    Ok(Deployment {
        api_token: api.token,
        account_id: account,
        url: format!("https://{SCRIPT}.{subdomain}.workers.dev"),
        hub_key,
        worker: worker_hash(),
    })
}

/// Removes the relay `deployment` from its account.
pub(super) fn remove(base: &str, deployment: &Deployment) -> Result<()> {
    Api::new(base, &deployment.api_token).delete(&deployment.account_id)
}

#[cfg(test)]
pub(super) mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;

    pub(in crate::hub) const TOKEN: &str = "cf-token";

    /// What the fake API was asked: method, path, body.
    type Calls = Arc<Mutex<Vec<(String, String, String)>>>;

    /// Cloudflare's API on localhost, for one account with subdomain `ada`; the script, once
    /// uploaded, with the migration tag it last ran.
    pub(in crate::hub) struct FakeApi {
        pub(in crate::hub) url: String,
        calls: Calls,
        script: Arc<Mutex<Option<String>>>,
    }

    impl FakeApi {
        fn start(accounts: serde_json::Value) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let calls = Calls::default();
            let script = Arc::new(Mutex::new(None::<String>));
            let (seen, tag) = (Arc::clone(&calls), Arc::clone(&script));
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let mut stream = stream.unwrap();
                    let (method, path, head, body) = read(&mut stream);
                    seen.lock()
                        .unwrap()
                        .push((method.clone(), path.clone(), body.clone()));
                    let ok = |result: serde_json::Value| {
                        let answer =
                            serde_json::json!({ "success": true, "errors": [], "result": result });
                        ("200 OK", answer.to_string())
                    };
                    let (status, answer) = if !head.contains(&format!("Bearer {TOKEN}")) {
                        let answer = serde_json::json!({
                            "success": false,
                            "errors": [{ "code": 10000, "message": "Authentication error" }],
                        });
                        ("403 Forbidden", answer.to_string())
                    } else {
                        let script = format!("/accounts/acc1/workers/scripts/{SCRIPT}");
                        match (method.as_str(), path.as_str()) {
                            ("GET", "/accounts?per_page=50") => ok(accounts.clone()),
                            ("GET", "/accounts/acc1/workers/subdomain") => {
                                ok(serde_json::json!({ "subdomain": "ada" }))
                            }
                            ("GET", "/accounts/acc1/workers/scripts") => {
                                let scripts = match &*tag.lock().unwrap() {
                                    Some(migration) => serde_json::json!([
                                        { "id": SCRIPT, "migration_tag": migration },
                                    ]),
                                    None => serde_json::json!([]),
                                };
                                ok(scripts)
                            }
                            ("PUT", p) if p == script => {
                                let mut tag = tag.lock().unwrap();
                                if body.contains("\"new_tag\":\"v1\"") {
                                    *tag = Some("v1".to_owned());
                                } else if tag.is_none() {
                                    panic!("a new script was uploaded without its migration");
                                }
                                ok(serde_json::json!({ "id": SCRIPT }))
                            }
                            ("POST", p) if p == format!("{script}/subdomain") => {
                                ok(serde_json::json!({ "enabled": true }))
                            }
                            ("DELETE", p) if p == format!("{script}?force=true") => {
                                *tag.lock().unwrap() = None;
                                ok(serde_json::json!(null))
                            }
                            _ => ("404 Not Found", "{}".to_owned()),
                        }
                    };
                    write!(
                        stream,
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .unwrap();
                }
            });
            Self { url, calls, script }
        }

        pub(in crate::hub) fn one_account() -> Self {
            Self::start(serde_json::json!([{ "id": "acc1", "name": "Ada" }]))
        }

        pub(in crate::hub) fn calls(&self) -> Vec<(String, String)> {
            let calls = self.calls.lock().unwrap();
            calls
                .iter()
                .map(|(method, path, _)| (method.clone(), path.clone()))
                .collect()
        }

        /// The body of the last upload.
        fn uploaded(&self) -> String {
            let calls = self.calls.lock().unwrap();
            calls
                .iter()
                .rev()
                .find(|(method, _, _)| method == "PUT")
                .map(|(_, _, body)| body.clone())
                .unwrap()
        }
    }

    /// A request: method, path, head, body.
    fn read(stream: &mut std::net::TcpStream) -> (String, String, String, String) {
        let mut reader = BufReader::new(stream);
        let mut head = String::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
            head.push_str(&line);
        }
        let length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0);
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let mut words = head.split_whitespace();
        let method = words.next().unwrap().to_owned();
        let path = words.next().unwrap().to_owned();
        (
            method,
            path,
            head,
            String::from_utf8_lossy(&body).into_owned(),
        )
    }

    fn calls(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(m, p)| ((*m).to_owned(), (*p).to_owned()))
            .collect()
    }

    #[test]
    fn deploys_updates_and_removes_the_worker() {
        let api = FakeApi::one_account();
        let deployed = deploy(&api.url, TOKEN, None, None).unwrap();
        assert_eq!(deployed.account_id, "acc1");
        assert_eq!(deployed.url, "https://herder-relay.ada.workers.dev");
        assert_eq!(deployed.hub_key.len(), 64);
        assert_eq!(deployed.worker, worker_hash());
        assert_eq!(deployed.api_token, TOKEN);
        let script = "/accounts/acc1/workers/scripts/herder-relay";
        assert_eq!(
            api.calls(),
            calls(&[
                ("GET", "/accounts?per_page=50"),
                ("GET", "/accounts/acc1/workers/subdomain"),
                ("GET", "/accounts/acc1/workers/scripts"),
                ("PUT", script),
                ("POST", &format!("{script}/subdomain")),
            ])
        );
        let upload = api.uploaded();
        assert!(
            upload.contains(r#""new_sqlite_classes":["Relay"]"#),
            "{upload}"
        );
        assert!(upload.contains(&format!(r#""text":"{}""#, deployed.hub_key)));
        assert!(upload.contains("application/javascript+module"));
        assert!(upload.contains("export class Relay extends DurableObject"));

        // An update keeps the key and the account, and runs no migration again.
        api.calls.lock().unwrap().clear();
        let updated = deploy(&api.url, TOKEN, None, Some(&deployed)).unwrap();
        assert_eq!(updated, deployed);
        assert!(
            !api.uploaded().contains("new_tag"),
            "the migration ran once"
        );
        assert_eq!(api.calls()[0].1, "/accounts/acc1/workers/subdomain");

        remove(&api.url, &deployed).unwrap();
        assert_eq!(
            api.calls().last().unwrap(),
            &("DELETE".to_owned(), format!("{script}?force=true"))
        );
        assert!(api.script.lock().unwrap().is_none());
        // Removing what is gone already is fine.
        remove(&api.url, &deployed).unwrap();
    }

    #[test]
    fn a_refused_token_and_an_ambiguous_account_say_what_to_do() {
        let api = FakeApi::one_account();
        let err = deploy(&api.url, "wrong", None, None).unwrap_err();
        assert!(
            format!("{err:#}").contains("Workers Scripts: Edit"),
            "{err:#}"
        );

        let api = FakeApi::start(serde_json::json!([
            { "id": "acc1", "name": "Ada" },
            { "id": "acc2", "name": "Work" },
        ]));
        let err = deploy(&api.url, TOKEN, None, None).unwrap_err();
        assert!(format!("{err:#}").contains("acc2 (Work)"), "{err:#}");
        // Naming the account picks it.
        let deployed = deploy(&api.url, TOKEN, Some("acc1"), None).unwrap();
        assert_eq!(deployed.account_id, "acc1");
    }
}
