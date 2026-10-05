//! What the vault keeps, and for how long: archived sessions of online hosts go once they
//! had no event for the retention period, checked every [`PRUNE_EVERY`]; an owner forgets a
//! host that is gone, and everything it replicated, with `herder vault forget-host`
//! ([`Admin::forget_host`]).
//!
//! Nothing else is ever dropped on its own: an online host's live sessions stay while they
//! exist, and a dead host's sessions stay until it is forgotten, since they may be all that
//! is left of them.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use herder_protocol::{HostId, Timestamp};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::{Shared, blocking};

/// How often the vault looks for archived sessions past the retention period.
pub const PRUNE_EVERY: Duration = Duration::from_secs(60 * 60);

/// Prunes archived sessions every [`PRUNE_EVERY`], from the first tick on, until `shutdown`.
pub(super) async fn run(shared: Arc<Shared>, shutdown: CancellationToken) {
    let retention = Duration::from_secs(u64::from(shared.retention.archive_days) * 24 * 60 * 60);
    // Time goes by tokio's clock from the wall clock at start, so a paused clock moves it too.
    let (started, wall) = (Instant::now(), Timestamp::now());
    let mut every = tokio::time::interval(PRUNE_EVERY);
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            _ = every.tick() => {}
        }
        let before = wall
            .checked_add(started.elapsed())
            .and_then(|now| now.checked_sub(retention));
        let before = match before {
            Ok(before) => before,
            Err(err) => return warn!("cannot tell which archived sessions to drop: {err}"),
        };
        let online = shared.fleet.presence().online_hosts();
        if online.is_empty() {
            continue;
        }
        match blocking(&shared.store, move |store| store.prune(&online, before)).await {
            Ok(dropped) if dropped.is_empty() => {}
            Ok(dropped) => {
                for (host, session) in &dropped {
                    info!(host_id = %host, session_id = %session, "archived session dropped after the retention period");
                }
                shared.fleet.refresh().await;
                shared.fleet.refresh_hosts().await;
            }
            Err(err) => warn!("cannot drop archived sessions: {err:#}"),
        }
    }
}

/// What the vault's control socket does with what it holds, for its owner.
#[derive(Clone)]
pub struct Admin {
    pub(super) shared: Arc<Shared>,
}

/// A host the vault forgot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Forgot {
    /// The host.
    pub host_id: HostId,
    /// Its name.
    pub host_name: String,
    /// Its sessions that were held.
    pub sessions: u32,
    /// Its devices unpaired.
    pub devices: usize,
}

impl Admin {
    /// Drops the host named, or with the id, `name`, and everything it replicated, and
    /// unpairs its devices. Refused while it is online: stop it backing up first.
    pub async fn forget_host(&self, name: &str) -> Result<Forgot> {
        let shared = &self.shared;
        let hosts = blocking(&shared.store, |store| store.hosts()).await?;
        let found: Vec<_> = hosts
            .into_iter()
            .filter(|host| host.host_id.as_str() == name || host.host_name == name)
            .collect();
        let host = match found.as_slice() {
            [] => bail!("the vault holds no host {name}"),
            [host] => host.clone(),
            several => bail!(
                "several hosts are named {name}: {}; name one by its id",
                several
                    .iter()
                    .map(|host| host.host_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        if shared.fleet.presence().online(&host.host_id) {
            bail!(
                "{} is online; stop it backing up to the vault first, then forget it",
                host.host_name
            );
        }
        // Unpaired first, so it cannot replicate again while it is dropped.
        let devices = {
            let host_id = host.host_id.clone();
            blocking(&shared.store, move |store| store.devices_of(&host_id)).await?
        };
        let mut unpaired = 0;
        for device in &devices {
            if shared
                .auth
                .revoke(device)
                .context("unpairing the host's device")?
            {
                unpaired += 1;
            }
        }
        let sessions = {
            let host_id = host.host_id.clone();
            blocking(&shared.store, move |store| store.forget(&host_id)).await?
        };
        info!(host_id = %host.host_id, sessions, "host forgotten");
        shared.fleet.refresh().await;
        shared.fleet.refresh_hosts().await;
        Ok(Forgot {
            host_id: host.host_id,
            host_name: host.host_name,
            sessions,
            devices: unpaired,
        })
    }
}

#[cfg(test)]
mod tests {
    use herder_protocol::{
        Batch, JournalRecord, ProjectId, RawEventBody, SessionId, SessionStatus, SessionSummary,
    };

    use super::*;
    use crate::auth::Auth;
    use crate::config::Retention;
    use crate::vault::{LIVENESS_TIMEOUT, Server, VaultStore};
    use crate::ws::{Host, Tls};

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    /// Replicates session `id` of `host` into `store`, its latest event `ago`.
    fn replicate(
        store: &mut VaultStore,
        host: &str,
        id: &str,
        status: SessionStatus,
        ago: Duration,
    ) {
        let host = HostId::new(host);
        let at = Timestamp::now().checked_sub(ago).unwrap();
        let body = serde_json::json!({
            "type": "session_created", "repo": "/repo", "worktree": "/wt", "branch": "b",
            "provider": "claude", "account_id": "main", "model": "m", "permission_mode": "ask"
        });
        let batch = Batch {
            session_id: SessionId::new(id),
            events: vec![JournalRecord {
                seq: 1,
                at,
                by: None,
                body: RawEventBody::from_value(body).unwrap(),
            }],
        };
        store.append(&host, &batch).unwrap();
        let summary = SessionSummary {
            session_id: SessionId::new(id),
            project_id: ProjectId::new("github.com/org/repo"),
            repo: "/repo".into(),
            branch: "b".into(),
            status,
            prs: Vec::new(),
            parent: None,
            parent_host: None,
            task: None,
            title: None,
            head_seq: 1,
            updated_at: at,
        };
        store.put_summary(&host, &summary).unwrap();
    }

    fn listed(server: &Server) -> Vec<String> {
        let store = server.store();
        let store = store.lock().unwrap();
        let fleet = store.fleet().unwrap();
        fleet
            .iter()
            .map(|head| head.session_id.to_string())
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn archived_sessions_of_online_hosts_go_after_the_retention_period() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = VaultStore::open(tmp.path().join("vault.db")).unwrap();
        std::fs::create_dir_all(tmp.path().join("tls")).unwrap();
        // 12 hours short of 90 days.
        let almost = 90 * DAY - DAY / 2;
        replicate(
            &mut store,
            "live",
            "archived",
            SessionStatus::Archived,
            almost,
        );
        replicate(&mut store, "live", "idle", SessionStatus::Idle, 400 * DAY);
        replicate(
            &mut store,
            "dead",
            "ancient",
            SessionStatus::Archived,
            400 * DAY,
        );
        let server = Server::new(
            Tls::load_or_create(&tmp.path().join("tls"), "vault").unwrap(),
            Arc::new(Auth::open(tmp.path()).unwrap()),
            store,
            Host {
                id: HostId::new("vault"),
                name: "vault".into(),
            },
            LIVENESS_TIMEOUT,
            Retention { archive_days: 90 },
        );
        server
            .shared
            .fleet
            .presence()
            .connected(&HostId::new("live"));
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(run(Arc::clone(&server.shared), shutdown.clone()));

        // The first check, at once, finds nothing past 90 days on the online host.
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(listed(&server), ["ancient", "archived", "idle"]);

        // A day on, the archived session is past it. The online host's live session stays
        // however old, and so does everything of the host that is gone.
        tokio::time::sleep(DAY).await;
        assert_eq!(listed(&server), ["ancient", "idle"]);
        shutdown.cancel();
        task.await.unwrap();
    }
}
