//! A fake of the part of Cloudflare's API the managed tunnel calls, on localhost: one account
//! with the zone `example.com`, and the tunnels and DNS records made in it. Anything but
//! [`TOKEN`] is refused.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

/// The API token the fake takes.
pub const TOKEN: &str = "cf-api-token";

/// The fake's account.
pub const ACCOUNT: &str = "acc1";

/// The fake's zone, `example.com`.
pub const ZONE: &str = "zone1";

/// What the fake holds.
#[derive(Debug, Default)]
pub struct Held {
    pub tunnels: BTreeMap<String, Value>,
    pub configs: BTreeMap<String, Value>,
    pub records: BTreeMap<String, Value>,
    /// Workers scripts by name: the multipart body of their last upload, the migration they
    /// last ran, and whether they are served on `workers.dev`.
    pub scripts: BTreeMap<String, Script>,
    /// Every request, as `METHOD /path`.
    pub calls: Vec<String>,
    next: u32,
}

impl Held {
    fn id(&mut self, kind: &str) -> String {
        self.next += 1;
        format!("{kind}{}", self.next)
    }

    /// The service the tunnel routing `hostname` sends it to, as an edge would find it: the
    /// DNS record's tunnel, then that tunnel's ingress rule for the hostname.
    pub fn route(&self, hostname: &str) -> Option<String> {
        let record = self.records.values().find(|r| r["name"] == hostname)?;
        let tunnel = record["content"]
            .as_str()?
            .strip_suffix(".cfargotunnel.com")?;
        let live = self.tunnels.get(tunnel)?["deleted_at"].is_null();
        let rules = self.configs.get(tunnel)?["config"]["ingress"]
            .as_array()?
            .clone();
        let rule = rules.iter().find(|rule| rule["hostname"] == hostname)?;
        live.then(|| rule["service"].as_str().map(str::to_owned))?
    }

    /// The tunnel token Cloudflare gives for tunnel `id`.
    pub fn token_of(id: &str) -> String {
        format!("token-of-{id}")
    }
}

/// A Workers script the fake holds.
#[derive(Debug, Default, Clone)]
pub struct Script {
    /// The body of its last upload.
    pub upload: String,
    /// The Durable Object migration it last ran, if any.
    pub migration: Option<String>,
    /// Whether it is served on `workers.dev`.
    pub workers_dev: bool,
}

/// The fake account's `workers.dev` subdomain.
pub const SUBDOMAIN: &str = "ada";

/// The fake API.
pub struct FakeCloudflare {
    pub url: String,
    pub held: Arc<Mutex<Held>>,
    /// While set, requests wait for their answer, as from a Cloudflare that is slow.
    stalled: Arc<AtomicBool>,
}

impl FakeCloudflare {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let held = Arc::new(Mutex::new(Held::default()));
        let stalled = Arc::new(AtomicBool::new(false));
        let (shared, stalls) = (Arc::clone(&held), Arc::clone(&stalled));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                // Something on a dev machine may connect and send no request; skip it.
                let Some((head, body)) = read_request(&mut stream) else {
                    continue;
                };
                while stalls.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(10));
                }
                let (status, answer) = answer(&shared, &head, &body);
                let text = answer.to_string();
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
            }
        });
        Self { url, held, stalled }
    }

    /// Whether requests wait for their answer from now on, until this is called with `false`.
    pub fn stall(&self, stalled: bool) {
        self.stalled.store(stalled, Ordering::SeqCst);
    }

    pub fn held(&self) -> std::sync::MutexGuard<'_, Held> {
        self.held.lock().unwrap()
    }
}

fn ok(result: Value) -> (&'static str, Value) {
    (
        "200 OK",
        json!({ "success": true, "errors": [], "result": result }),
    )
}

fn missing() -> (&'static str, Value) {
    (
        "404 Not Found",
        json!({ "success": false, "errors": [{ "code": 1, "message": "not found" }] }),
    )
}

fn answer(held: &Mutex<Held>, head: &str, body: &str) -> (&'static str, Value) {
    let mut words = head.split_whitespace();
    let method = words.next().unwrap_or_default().to_owned();
    let target = words.next().unwrap_or_default().to_owned();
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let param = |key: &str| {
        query
            .split('&')
            .find_map(|pair| pair.strip_prefix(&format!("{key}=")))
            .map(str::to_owned)
    };
    let mut held = held.lock().unwrap();
    held.calls.push(format!("{method} {path}"));
    if !head.contains(&format!("Bearer {TOKEN}")) {
        return (
            "403 Forbidden",
            json!({ "success": false, "errors": [{ "code": 9109, "message": "Invalid access token" }] }),
        );
    }
    let raw = body;
    let body: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match (method.as_str(), segments.as_slice()) {
        ("GET", ["zones"]) => match (param("name").as_deref(), param("account.id").as_deref()) {
            (Some("example.com"), Some(ACCOUNT)) => ok(json!([{
                "id": ZONE, "name": "example.com", "account": { "id": ACCOUNT },
            }])),
            _ => ok(json!([])),
        },
        ("GET", ["accounts", ACCOUNT, "cfd_tunnel"]) => {
            let name = param("name").unwrap_or_default();
            let found: Vec<Value> = held
                .tunnels
                .values()
                .filter(|t| t["name"] == name.as_str() && t["deleted_at"].is_null())
                .cloned()
                .collect();
            ok(json!(found))
        }
        ("POST", ["accounts", ACCOUNT, "cfd_tunnel"]) => {
            let id = held.id("tunnel");
            let tunnel = json!({ "id": id, "name": body["name"], "deleted_at": null });
            held.tunnels.insert(id, tunnel.clone());
            ok(tunnel)
        }
        ("GET", ["accounts", ACCOUNT, "cfd_tunnel", id]) => match held.tunnels.get(*id) {
            Some(tunnel) => ok(tunnel.clone()),
            None => missing(),
        },
        ("DELETE", ["accounts", ACCOUNT, "cfd_tunnel", id]) => match held.tunnels.get_mut(*id) {
            Some(tunnel) => {
                tunnel["deleted_at"] = json!("2026-11-27T00:00:00Z");
                ok(tunnel.clone())
            }
            None => missing(),
        },
        ("DELETE", ["accounts", ACCOUNT, "cfd_tunnel", id, "connections"]) => {
            if held.tunnels.contains_key(*id) {
                ok(json!(null))
            } else {
                missing()
            }
        }
        ("PUT", ["accounts", ACCOUNT, "cfd_tunnel", id, "configurations"]) => {
            if held.tunnels.contains_key(*id) {
                held.configs.insert((*id).to_owned(), body.clone());
                ok(body)
            } else {
                missing()
            }
        }
        ("GET", ["accounts", ACCOUNT, "cfd_tunnel", id, "token"]) => {
            if held.tunnels.contains_key(*id) {
                ok(json!(Held::token_of(id)))
            } else {
                missing()
            }
        }
        ("GET", ["zones", ZONE, "dns_records"]) => {
            let name = param("name").unwrap_or_default();
            let found: Vec<Value> = held
                .records
                .values()
                .filter(|r| r["name"] == name.as_str())
                .cloned()
                .collect();
            ok(json!(found))
        }
        ("POST", ["zones", ZONE, "dns_records"]) => {
            let id = held.id("record");
            let mut record = body;
            record["id"] = json!(id);
            held.records.insert(id, record.clone());
            ok(record)
        }
        ("PUT", ["zones", ZONE, "dns_records", id]) => {
            if held.records.contains_key(*id) {
                let mut record = body;
                record["id"] = json!(id);
                held.records.insert((*id).to_owned(), record.clone());
                ok(record)
            } else {
                missing()
            }
        }
        ("DELETE", ["zones", ZONE, "dns_records", id]) => match held.records.remove(*id) {
            Some(_) => ok(json!({ "id": id })),
            None => missing(),
        },
        ("GET", ["accounts", ACCOUNT, "workers", "subdomain"]) => {
            ok(json!({ "subdomain": SUBDOMAIN }))
        }
        ("GET", ["accounts", ACCOUNT, "workers", "scripts"]) => {
            let scripts: Vec<Value> = held
                .scripts
                .iter()
                .map(|(id, script)| json!({ "id": id, "migration_tag": script.migration }))
                .collect();
            ok(json!(scripts))
        }
        ("PUT", ["accounts", ACCOUNT, "workers", "scripts", name]) => {
            assert!(head.contains("multipart/form-data; boundary="), "{head}");
            let script = held.scripts.entry((*name).to_owned()).or_default();
            if raw.contains(r#""new_tag":"v1""#) {
                script.migration = Some("v1".to_owned());
            }
            if script.migration.is_none() {
                return (
                    "400 Bad Request",
                    json!({ "success": false, "errors": [{ "code": 10074, "message": "Cannot create binding for class Relay that is not exported by the script" }] }),
                );
            }
            script.upload = raw.to_owned();
            ok(json!({ "id": name }))
        }
        ("POST", ["accounts", ACCOUNT, "workers", "scripts", name, "subdomain"]) => {
            match held.scripts.get_mut(*name) {
                Some(script) => {
                    script.workers_dev = body["enabled"] == true;
                    ok(json!({ "enabled": script.workers_dev }))
                }
                None => missing(),
            }
        }
        ("DELETE", ["accounts", ACCOUNT, "workers", "scripts", name]) => {
            match held.scripts.remove(*name) {
                Some(_) => ok(json!(null)),
                None => missing(),
            }
        }
        _ => missing(),
    }
}

/// A request's head and body, read off `stream`; `None` for anything but a request.
fn read_request(stream: &mut TcpStream) -> Option<(String, String)> {
    let mut reader = BufReader::new(stream);
    let mut head = String::new();
    let mut length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().ok()?;
        }
        if line == "\r\n" || line.is_empty() {
            break;
        }
        head.push_str(&line);
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    if !head.contains("HTTP/1.1") {
        return None;
    }
    Some((head, String::from_utf8(body).ok()?))
}
