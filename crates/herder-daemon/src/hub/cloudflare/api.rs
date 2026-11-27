//! The calls of Cloudflare's API (v4) the hub makes: zones, Cloudflare Tunnels and their
//! remotely managed config, and DNS records. Every call is blocking; the hub makes them on the
//! blocking pool.

use std::io::Read;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use herder_protocol::SecretValue;
use serde::Deserialize;
use serde_json::{Value, json};

/// Cloudflare's API.
pub const BASE: &str = "https://api.cloudflare.com/client/v4";

/// Longest a call may take.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Most bytes of an answer read.
const MAX_ANSWER_BYTES: u64 = 1 << 20;

/// What a tunnel's DNS record points at: `<tunnel id>.cfargotunnel.com`.
pub const TUNNEL_DOMAIN: &str = "cfargotunnel.com";

/// Cloudflare's API at `base`, called with an API token.
pub struct Api {
    base: String,
    token: SecretValue,
    agent: ureq::Agent,
}

/// A zone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Zone {
    pub id: String,
    pub name: String,
}

/// A DNS record.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct DnsRecord {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub content: String,
}

impl Api {
    /// The API at `base` (`BASE` but in tests), called with `token`.
    pub fn new(base: &str, token: SecretValue) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            base: base.trim_end_matches('/').to_owned(),
            token,
            agent,
        }
    }

    /// The zone of `account` that `hostname` is in: the longest of its parent names that is
    /// one.
    pub fn zone_for(&self, account: &str, hostname: &str) -> Result<Zone> {
        let labels: Vec<&str> = hostname.split('.').collect();
        for start in 0..labels.len().saturating_sub(1) {
            let name = labels[start..].join(".");
            let zones = self
                .call(
                    "GET",
                    &format!("/zones?name={name}&account.id={account}"),
                    None,
                )?
                .unwrap_or_default();
            if let Some(zone) = zones.as_array().and_then(|zones| zones.first()) {
                return Ok(Zone {
                    id: string(zone, "id")?,
                    name: string(zone, "name")?,
                });
            }
        }
        bail!(
            "{hostname} is in no zone of Cloudflare account {account} that the API token can \
             read: add the zone to the account, or give the token Zone Read on it"
        )
    }

    /// Whether the tunnel `id` is there and not deleted.
    pub fn tunnel_exists(&self, account: &str, id: &str) -> Result<bool> {
        Ok(self
            .call("GET", &format!("/accounts/{account}/cfd_tunnel/{id}"), None)?
            .is_some_and(|tunnel| tunnel["deleted_at"].is_null()))
    }

    /// The id of the tunnel named `name` that is not deleted, if there is one.
    pub fn tunnel_named(&self, account: &str, name: &str) -> Result<Option<String>> {
        let tunnels = self
            .call(
                "GET",
                &format!("/accounts/{account}/cfd_tunnel?name={name}&is_deleted=false"),
                None,
            )?
            .unwrap_or_default();
        match tunnels.as_array().and_then(|tunnels| tunnels.first()) {
            Some(tunnel) => Ok(Some(string(tunnel, "id")?)),
            None => Ok(None),
        }
    }

    /// Creates a tunnel named `name` whose config lives in Cloudflare, and returns its id.
    pub fn create_tunnel(&self, account: &str, name: &str) -> Result<String> {
        let body = json!({ "name": name, "config_src": "cloudflare" });
        let tunnel = self
            .call(
                "POST",
                &format!("/accounts/{account}/cfd_tunnel"),
                Some(body),
            )?
            .context("Cloudflare created no tunnel")?;
        string(&tunnel, "id")
    }

    /// Sets the tunnel's ingress: `hostname` to `service`, anything else 404.
    pub fn configure_tunnel(
        &self,
        account: &str,
        id: &str,
        hostname: &str,
        service: &str,
    ) -> Result<()> {
        let body = json!({ "config": { "ingress": [
            { "hostname": hostname, "service": service, "originRequest": {} },
            { "service": "http_status:404" },
        ] } });
        self.call(
            "PUT",
            &format!("/accounts/{account}/cfd_tunnel/{id}/configurations"),
            Some(body),
        )?
        .context("Cloudflare has no such tunnel to configure")?;
        Ok(())
    }

    /// The token `cloudflared tunnel run` runs the tunnel with.
    pub fn tunnel_token(&self, account: &str, id: &str) -> Result<SecretValue> {
        let token = self
            .call(
                "GET",
                &format!("/accounts/{account}/cfd_tunnel/{id}/token"),
                None,
            )?
            .context("Cloudflare has no such tunnel to run")?;
        token
            .as_str()
            .and_then(|token| SecretValue::new(token.to_owned()))
            .context("Cloudflare's tunnel token is not text")
    }

    /// Deletes the tunnel, once its connections are dropped; one that is gone already is
    /// fine.
    pub fn delete_tunnel(&self, account: &str, id: &str) -> Result<()> {
        let path = format!("/accounts/{account}/cfd_tunnel/{id}");
        self.call("DELETE", &format!("{path}/connections"), None)?;
        self.call("DELETE", &path, None)?;
        Ok(())
    }

    /// The DNS records named `name`.
    pub fn dns_records(&self, zone: &str, name: &str) -> Result<Vec<DnsRecord>> {
        let records = self
            .call(
                "GET",
                &format!("/zones/{zone}/dns_records?name={name}"),
                None,
            )?
            .unwrap_or_default();
        serde_json::from_value(records).context("Cloudflare's DNS records are not a list")
    }

    /// Points `name` at `target` with a proxied CNAME: a new record, or record `id`.
    pub fn put_cname(
        &self,
        zone: &str,
        id: Option<&str>,
        name: &str,
        target: &str,
    ) -> Result<String> {
        let body = json!({
            "type": "CNAME",
            "name": name,
            "content": target,
            "proxied": true,
            "ttl": 1,
            "comment": "herder hub tunnel",
        });
        let record = match id {
            Some(id) => self.call(
                "PUT",
                &format!("/zones/{zone}/dns_records/{id}"),
                Some(body),
            )?,
            None => self.call("POST", &format!("/zones/{zone}/dns_records"), Some(body))?,
        }
        .context("Cloudflare has no such DNS record to update")?;
        string(&record, "id")
    }

    /// Deletes DNS record `id`; one that is gone already is fine.
    pub fn delete_dns_record(&self, zone: &str, id: &str) -> Result<()> {
        self.call("DELETE", &format!("/zones/{zone}/dns_records/{id}"), None)?;
        Ok(())
    }

    /// Sends `method` to `path` with `body`: the answer's `result`, `None` for a 404.
    pub(super) fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<Option<Value>> {
        let body = body.map(|body| body.to_string()).unwrap_or_default();
        self.send(method, path, "application/json", body.as_bytes())
    }

    /// Sends `method` to `path` with `body` of `content_type`: the answer's `result`, `None`
    /// for a 404.
    pub(super) fn send(
        &self,
        method: &str,
        path: &str,
        content_type: &str,
        body: &[u8],
    ) -> Result<Option<Value>> {
        let url = format!("{}{path}", self.base);
        let bearer = format!("Bearer {}", self.token.expose());
        let response = match method {
            "GET" => self.agent.get(&url).header("Authorization", &bearer).call(),
            "DELETE" => self
                .agent
                .delete(&url)
                .header("Authorization", &bearer)
                .call(),
            "POST" => self
                .agent
                .post(&url)
                .header("Authorization", &bearer)
                .header("Content-Type", content_type)
                .send(body),
            _ => self
                .agent
                .put(&url)
                .header("Authorization", &bearer)
                .header("Content-Type", content_type)
                .send(body),
        };
        let mut response = response.with_context(|| format!("cannot reach Cloudflare at {url}"))?;
        let status = response.status().as_u16();
        let mut text = String::new();
        response
            .body_mut()
            .as_reader()
            .take(MAX_ANSWER_BYTES)
            .read_to_string(&mut text)
            .with_context(|| format!("cannot read Cloudflare's answer from {url}"))?;
        let answer: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        match status {
            200..=299 => Ok(Some(answer["result"].clone())),
            404 => Ok(None),
            401 | 403 => bail!(
                "Cloudflare refused the API token for {method} {path} ({status}): {}; it needs \
                 Cloudflare Tunnel Edit, Zone Read and DNS Edit for the tunnel, and Workers \
                 Scripts Edit and Account Settings Read for the relay",
                errors(&answer, &text)
            ),
            _ => bail!(
                "Cloudflare answered {method} {path} with {status}: {}",
                errors(&answer, &text)
            ),
        }
    }
}

/// The messages of Cloudflare's `errors`, else the answer's text.
fn errors(answer: &Value, text: &str) -> String {
    let messages: Vec<String> = answer["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|error| error["message"].as_str().map(str::to_owned))
        .collect();
    if messages.is_empty() {
        text.trim().chars().take(200).collect()
    } else {
        messages.join("; ")
    }
}

/// The string field `key` of `value`.
pub(super) fn string(value: &Value, key: &str) -> Result<String> {
    value[key]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("Cloudflare's answer has no {key}"))
}
