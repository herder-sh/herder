//! The hub's managed Cloudflare Tunnel: herder creates a tunnel and a hostname for the hub in
//! the owner's Cloudflare account, and runs `cloudflared` for it itself, instead of a
//! `cloudflared` configured by hand (`docs/public-access.md`).
//!
//! An owner asks for it with `CloudflareSettings::tunnel_hostname`. The hub then, in the zone
//! of the account the hostname is in:
//!
//! - creates a tunnel named `herder-<hostname>` whose config lives in Cloudflare, or reuses
//!   it, and routes the hostname to the hub's tunnel listener (`tunnel` in its config);
//! - points the hostname at the tunnel with a proxied CNAME;
//! - runs the pinned `cloudflared` with the tunnel's token ([`super::cloudflared`]), and
//!   advertises `wss://<hostname>` among its public addresses while it does.
//!
//! What it made, with the tunnel's token, is kept in `<data_dir>/cloudflare/tunnel.json`,
//! readable by the hub's user only; the API token stays a hub secret. Neither goes to a
//! client. At each start the hub runs `cloudflared` at once and checks the tunnel with
//! Cloudflare, making again whatever was deleted; it tries again every few minutes while
//! that fails. Clearing the hostname deletes the tunnel and its DNS record.

use std::fs::DirBuilder;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use herder_protocol::{RouteState, RouteStatus, SecretValue};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::api::{Api, TUNNEL_DOMAIN};
use super::cloudflared::{self, Download, Run, Running, Status};
use crate::settings::PublicAddresses;

/// How long the hub waits before trying again a set-up that failed.
const RETRY: Duration = Duration::from_secs(300);

/// The tunnel an owner asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wanted {
    /// The Cloudflare account.
    pub account_id: String,
    /// The API token, from the hub secret the settings name.
    pub api_token: SecretValue,
    /// The hostname, as [`hostname`] gives it.
    pub hostname: String,
}

/// What the hub was last told to make of the tunnel.
#[derive(Clone, Debug)]
struct Desired {
    /// The tunnel, none, or why the hub cannot tell (its API token is missing).
    wanted: Result<Option<Wanted>, String>,
    /// The API token to delete the tunnel with, when none is wanted.
    removal_token: Option<SecretValue>,
}

/// Where the hub is in making the tunnel match what it was told.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Phase {
    /// Done, or not told anything yet.
    #[default]
    Idle,
    SettingUp,
    Failed(String),
}

/// What the hub keeps of its managed tunnel, in `tunnel.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    hostname: String,
    account_id: String,
    zone_id: String,
    tunnel_id: String,
    dns_record_id: String,
    tunnel_token: SecretValue,
}

/// The hub's managed tunnel.
pub struct ManagedTunnel {
    /// `<data_dir>/cloudflare`: the state and the `cloudflared` binary.
    dir: PathBuf,
    /// Cloudflare's API.
    api: String,
    /// Where `cloudflared` delivers connections: the hub's tunnel listener, if it has one.
    ingress: Option<String>,
    /// What is set up.
    state: Mutex<Option<State>>,
    /// What the hub was last told; `None` until it is.
    desired: watch::Sender<Option<Desired>>,
    phase: std::sync::Mutex<Phase>,
    /// What `cloudflared` runs.
    run: watch::Sender<Option<Run>>,
    /// What `cloudflared` is doing.
    running: Status,
}

impl ManagedTunnel {
    /// The managed tunnel of the hub with `data_dir`, whose tunnel listener is at `tunnel`,
    /// set up through the API at `api` ([`super::API`] but in tests).
    pub fn open(data_dir: &Path, api: &str, tunnel: Option<SocketAddr>) -> Result<Arc<Self>> {
        let dir = data_dir.join("cloudflare");
        let state = load(&dir)?;
        Ok(Arc::new(Self {
            dir,
            api: api.to_owned(),
            ingress: tunnel.map(ingress),
            state: Mutex::new(state),
            desired: watch::Sender::new(None),
            phase: std::sync::Mutex::default(),
            run: watch::Sender::new(None),
            running: Status::default(),
        }))
    }

    /// Runs `cloudflared`, got with `download`, for what is set up, until `shutdown`,
    /// advertising the hostname among `public`'s; and makes the tunnel match what
    /// [`Self::want`] says from then on.
    pub async fn start(
        self: &Arc<Self>,
        public: Arc<PublicAddresses>,
        download: Download,
        shutdown: CancellationToken,
    ) {
        tokio::spawn(cloudflared::supervise(
            self.dir.clone(),
            download,
            self.run.subscribe(),
            public,
            Arc::clone(&self.running),
            shutdown.clone(),
        ));
        if self.ingress.is_some()
            && let Some(state) = self.state.lock().await.as_ref()
        {
            // At once, on what was set up, while the hub checks it with Cloudflare.
            self.run.send_replace(Some(run(state)));
        }
        tokio::spawn(Arc::clone(self).reconcile(self.desired.subscribe(), shutdown));
    }

    /// Makes the tunnel `wanted`, or deletes it with `removal_token` when none is; or, given
    /// why the hub cannot tell what is wanted, keeps what runs and says so.
    pub fn want(&self, wanted: Result<Option<Wanted>, String>, removal_token: Option<SecretValue>) {
        self.set_phase(Phase::SettingUp);
        self.desired.send_replace(Some(Desired {
            wanted,
            removal_token,
        }));
    }

    /// The tunnel as `RemoteAccess::tunnel` shows it: `None` when there is none, wanted or
    /// set up; its URL empty when the hub cannot read the API token it was set up with.
    pub async fn status(&self) -> Option<RouteStatus> {
        // A tunnel is wanted, but its API token is missing: its hostname may not be known.
        let (hostname, unreadable) = {
            let desired = self.desired.borrow();
            match desired.as_ref().map(|desired| &desired.wanted) {
                Some(Ok(Some(wanted))) => (Some(wanted.hostname.clone()), false),
                Some(Err(_)) => (None, true),
                _ => (None, false),
            }
        };
        let state = self.state.lock().await.clone();
        let url = match hostname.or_else(|| state.as_ref().map(|state| state.hostname.clone())) {
            Some(hostname) => format!("wss://{hostname}"),
            None if unreadable => String::new(),
            None => return None,
        };
        let phase = self
            .phase
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let running = self
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let (state, error) = match phase {
            Phase::SettingUp => (RouteState::SettingUp, None),
            Phase::Failed(why) => (RouteState::Failed, Some(why)),
            Phase::Idle if state.is_none() => (RouteState::SettingUp, None),
            Phase::Idle => match running {
                Running::Running => (RouteState::Up, None),
                Running::Starting => (RouteState::SettingUp, None),
                Running::Failed(why) => (RouteState::Failed, Some(why)),
                Running::Stopped => (
                    RouteState::Failed,
                    Some(
                        "the managed tunnel needs the hub's tunnel listener: set `tunnel` in \
                         its config, such as tunnel = \"127.0.0.1:7449\", and restart it"
                            .to_owned(),
                    ),
                ),
            },
        };
        Some(RouteStatus { url, state, error })
    }

    fn set_phase(&self, phase: Phase) {
        *self.phase.lock().unwrap_or_else(PoisonError::into_inner) = phase;
    }

    /// Makes the tunnel match each thing the hub is told, trying again every [`RETRY`] while
    /// that fails, until `shutdown`.
    async fn reconcile(
        self: Arc<Self>,
        mut desired: watch::Receiver<Option<Desired>>,
        shutdown: CancellationToken,
    ) {
        loop {
            let now = desired.borrow_and_update().clone();
            let retry = match now {
                None => None,
                Some(now) => {
                    self.set_phase(Phase::SettingUp);
                    match self.apply(now).await {
                        Ok(()) => {
                            self.set_phase(Phase::Idle);
                            None
                        }
                        Err(err) => {
                            warn!("the managed tunnel is not set up: {err:#}");
                            self.set_phase(Phase::Failed(format!("{err:#}")));
                            Some(RETRY)
                        }
                    }
                }
            };
            tokio::select! {
                () = shutdown.cancelled() => return,
                changed = desired.changed() => if changed.is_err() { return },
                () = async {
                    match retry {
                        Some(retry) => tokio::time::sleep(retry).await,
                        None => std::future::pending().await,
                    }
                } => {}
            }
        }
    }

    /// Makes Cloudflare and `cloudflared` match `desired`.
    async fn apply(&self, desired: Desired) -> Result<()> {
        // Not held across the calls to Cloudflare, which may take seconds: the status the
        // hub's clients ask for reads it meanwhile. Only this task, one apply at a time,
        // changes it.
        let previous = self.state.lock().await.clone();
        let base = self.api.clone();
        let Some(wanted) = desired.wanted.map_err(anyhow::Error::msg)? else {
            let Some(old) = previous else {
                return Ok(());
            };
            let token = desired.removal_token.context(
                "the hub secret holding the Cloudflare API token is gone, so the tunnel cannot \
                 be deleted; put it back, or delete the tunnel in Cloudflare",
            )?;
            self.run.send_replace(None);
            tokio::task::spawn_blocking(move || teardown(&Api::new(&base, token), &old))
                .await
                .context("the tunnel removal panicked")??;
            let path = state_path(&self.dir);
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(err).with_context(|| format!("removing {}", path.display()));
                }
            }
            *self.state.lock().await = None;
            info!("the managed tunnel is deleted");
            return Ok(());
        };
        let service = self.ingress.clone().context(
            "the managed tunnel delivers to the hub's tunnel listener: set `tunnel` in its \
             config, such as tunnel = \"127.0.0.1:7449\", and restart the hub",
        )?;
        let new = tokio::task::spawn_blocking(move || {
            let api = Api::new(&base, wanted.api_token.clone());
            let mut previous = previous;
            if let Some(old) = previous.take_if(|old| {
                old.hostname != wanted.hostname || old.account_id != wanted.account_id
            }) {
                teardown(&api, &old)?;
            }
            ensure(&api, &wanted, &service, previous.as_ref())
        })
        .await
        .context("the tunnel set-up panicked")??;
        save(&self.dir, &new)?;
        info!(
            hostname = new.hostname,
            tunnel_id = new.tunnel_id,
            "the managed tunnel is set up"
        );
        self.run.send_if_modified(|running| {
            let wanted = Some(run(&new));
            let changed = *running != wanted;
            *running = wanted;
            changed
        });
        *self.state.lock().await = Some(new);
        Ok(())
    }
}

/// What `cloudflared` runs for `state`.
fn run(state: &State) -> Run {
    Run {
        token: state.tunnel_token.clone(),
        hostname: state.hostname.clone(),
    }
}

/// Where `cloudflared`, next to the hub, reaches its tunnel listener bound to `tunnel`.
fn ingress(tunnel: SocketAddr) -> String {
    let ip = match tunnel.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    format!("http://{}", SocketAddr::new(ip, tunnel.port()))
}

/// `text` as a tunnel's hostname: lowercase, without a scheme or a trailing `/`, of two
/// labels or more.
pub fn hostname(text: &str) -> Result<String> {
    let text = text.trim().to_ascii_lowercase();
    let name = ["https://", "wss://"]
        .iter()
        .find_map(|scheme| text.strip_prefix(scheme))
        .unwrap_or(&text)
        .trim_end_matches('/');
    let labels: Vec<&str> = name.split('.').collect();
    let valid = name.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    if !valid {
        bail!("{text:?} is not a hostname such as hub.example.com");
    }
    Ok(name.to_owned())
}

/// Makes Cloudflare match `wanted`, the tunnel routed to `service`, reusing what `previous`
/// set up where it is still there.
fn ensure(api: &Api, wanted: &Wanted, service: &str, previous: Option<&State>) -> Result<State> {
    let Wanted {
        account_id,
        hostname,
        ..
    } = wanted;
    let account = account_id.as_str();
    let zone = api.zone_for(account, hostname)?;

    let name = format!("herder-{hostname}");
    let kept = match previous {
        Some(previous) if api.tunnel_exists(account, &previous.tunnel_id)? => {
            Some(previous.tunnel_id.clone())
        }
        _ => api.tunnel_named(account, &name)?,
    };
    let tunnel_id = match kept {
        Some(id) => id,
        None => {
            let id = api.create_tunnel(account, &name)?;
            info!(tunnel = name, id, "created a Cloudflare Tunnel");
            id
        }
    };
    api.configure_tunnel(account, &tunnel_id, hostname, service)?;

    let target = format!("{tunnel_id}.{TUNNEL_DOMAIN}");
    let records = api.dns_records(&zone.id, hostname)?;
    let dns_record_id = if let Some(record) = records
        .iter()
        .find(|record| record.kind == "CNAME" && record.content == target)
    {
        record.id.clone()
    } else if let Some(record) = records.iter().find(|record| {
        record.kind == "CNAME" && record.content.ends_with(&format!(".{TUNNEL_DOMAIN}"))
    }) {
        info!(
            hostname,
            from = record.content,
            "pointing the hostname from another tunnel to the hub's"
        );
        api.put_cname(&zone.id, Some(&record.id), hostname, &target)?
    } else if let Some(record) = records.first() {
        bail!(
            "{hostname} already has a {} record that is not a tunnel's; delete it in Cloudflare, \
             or pick another hostname",
            record.kind
        );
    } else {
        api.put_cname(&zone.id, None, hostname, &target)?
    };

    let tunnel_token = api.tunnel_token(account, &tunnel_id)?;
    Ok(State {
        hostname: hostname.clone(),
        account_id: account_id.clone(),
        zone_id: zone.id,
        tunnel_id,
        dns_record_id,
        tunnel_token,
    })
}

/// Deletes what `state` set up; what is gone already is fine.
fn teardown(api: &Api, state: &State) -> Result<()> {
    api.delete_dns_record(&state.zone_id, &state.dns_record_id)?;
    api.delete_tunnel(&state.account_id, &state.tunnel_id)?;
    info!(
        hostname = state.hostname,
        tunnel_id = state.tunnel_id,
        "deleted the managed tunnel's DNS record and tunnel"
    );
    Ok(())
}

fn state_path(dir: &Path) -> PathBuf {
    dir.join("tunnel.json")
}

fn load(dir: &Path) -> Result<Option<State>> {
    let path = state_path(dir);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .with_context(|| format!("reading {}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
    }
}

/// Writes `state` to `dir`, which only this user can read.
fn save(dir: &Path, state: &State) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    let path = state_path(dir);
    let json = serde_json::to_vec_pretty(state)?;
    crate::config::write_atomically(&path, &json)
        .and_then(|()| std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)))
        .with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
#[path = "tunnel_tests.rs"]
pub(crate) mod tests;
