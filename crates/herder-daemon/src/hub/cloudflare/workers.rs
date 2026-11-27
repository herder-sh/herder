//! The calls of Cloudflare's API the hub makes for its Durable Object relay: the account's
//! `workers.dev` subdomain, and the relay's script, uploaded with its Durable Object class and
//! secret, served on `workers.dev`, and deleted.

use anyhow::{Context, Result, bail};
use serde_json::json;

use super::api::Api;

/// The relay's script in the account, and the first label of its `workers.dev` hostname.
pub const SCRIPT: &str = "herder-relay";

/// The relay's source, as uploaded.
pub const WORKER: &str = include_str!("../../../../../deploy/cloudflare-relay/worker.js");

/// The Workers runtime behaviour the relay is written for; its tests use the same.
const COMPATIBILITY_DATE: &str = "2026-07-01";

/// The Durable Object migration that creates the relay's class, in SQLite storage (which the
/// free plan has); sent only to a script that has not run it.
const MIGRATION: &str = "v1";

impl Api {
    /// The `workers.dev` subdomain of `account`.
    pub fn workers_subdomain(&self, account: &str) -> Result<String> {
        self.call(
            "GET",
            &format!("/accounts/{account}/workers/subdomain"),
            None,
        )?
        .as_ref()
        .and_then(|result| result["subdomain"].as_str())
        .filter(|subdomain| !subdomain.is_empty())
        .map(str::to_owned)
        .context(
            "the Cloudflare account has no workers.dev subdomain yet: open Workers & Pages \
                 in the Cloudflare dashboard once to pick one",
        )
    }

    /// Whether the relay's script is in `account`, and if so, the migration it last ran.
    pub fn relay_script(&self, account: &str) -> Result<Option<String>> {
        let scripts = self
            .call("GET", &format!("/accounts/{account}/workers/scripts"), None)?
            .unwrap_or_default();
        Ok(scripts
            .as_array()
            .into_iter()
            .flatten()
            .find(|script| script["id"] == SCRIPT)
            .map(|script| script["migration_tag"].as_str().unwrap_or("").to_owned()))
    }

    /// Uploads the relay to `account` with `secret` as its `RELAY_SECRET`, creating its
    /// Durable Object class unless `migrated`, and serves it on `workers.dev`.
    pub fn upload_relay(&self, account: &str, secret: &str, migrated: bool) -> Result<()> {
        let mut metadata = json!({
            "main_module": "worker.js",
            "compatibility_date": COMPATIBILITY_DATE,
            "bindings": [
                { "type": "durable_object_namespace", "name": "RELAY", "class_name": "Relay" },
                { "type": "secret_text", "name": "RELAY_SECRET", "text": secret },
            ],
        });
        if !migrated {
            metadata["migrations"] =
                json!({ "new_tag": MIGRATION, "new_sqlite_classes": ["Relay"] });
        }
        // Neither part contains it.
        let boundary = "herder-relay-part-7c1f0d2e9a6b";
        let mut body = Vec::new();
        for (name, file, content_type, content) in [
            (
                "metadata",
                "",
                "application/json",
                metadata.to_string().into_bytes(),
            ),
            (
                "worker.js",
                "; filename=\"worker.js\"",
                "application/javascript+module",
                WORKER.as_bytes().to_vec(),
            ),
        ] {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"{file}\r\n\
                     Content-Type: {content_type}\r\n\r\n"
                )
                .as_bytes(),
            );
            body.extend_from_slice(&content);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        let script = format!("/accounts/{account}/workers/scripts/{SCRIPT}");
        let content_type = format!("multipart/form-data; boundary={boundary}");
        if self.send("PUT", &script, &content_type, &body)?.is_none() {
            bail!("Cloudflare has no Workers in account {account}");
        }
        let route = json!({ "enabled": true, "previews_enabled": false });
        self.call("POST", &format!("{script}/subdomain"), Some(route))?
            .context("Cloudflare lost the relay's script as it was uploaded")?;
        Ok(())
    }

    /// Deletes the relay's script from `account`, with its Durable Objects; gone already is
    /// fine.
    pub fn delete_relay(&self, account: &str) -> Result<()> {
        let script = format!("/accounts/{account}/workers/scripts/{SCRIPT}?force=true");
        self.call("DELETE", &script, None)?;
        Ok(())
    }
}

/// Whether a script that ran `migration` has the relay's Durable Object class.
pub fn migrated(migration: &str) -> bool {
    migration == MIGRATION
}
