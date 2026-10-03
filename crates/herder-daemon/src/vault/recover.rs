//! Recovering a session from a dead host: `herder recover <session>` on another host.
//!
//! Recovery, not migration: it is offered only while the session's host is offline as the
//! vault sees it, or with `force` for a host the vault still shows online but that is gone
//! or cut off. The session keeps its id. This host reads the session's journal from the vault
//! as a client ([`client`]), and the session manager takes it over
//! ([`SessionManager::recover`]): a new worktree restored from the latest checkpoint on
//! `origin`, and the next prompt replayed on one of this host's accounts. The images its
//! prompts carried come from the vault too. Checkpoints the dead host could only bundle stay
//! on it: bundles are not uploaded to the vault.
//!
//! Once this host replicates the session, the vault takes its copy as the session and the old
//! host's as recovered ([`super::VaultStore`]): clients see the session on this host, and
//! the old host, when it is back, makes its copy read-only ([`release_recovered`]). There is
//! no move back.
//!
//! Each recovered session's origin is kept in `<data_dir>/recovered/<session>.json`, so this
//! host never takes the vault, still listing the session on its old host before this host
//! replicated it, for a sign that the session moved on.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use herder_client_core::auth::DeviceKey;
use herder_protocol::{AccountId, HostId, SessionId, SessionStatus, Timestamp};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::client;
use crate::config::VaultConfig;
use crate::data_dir::write_private;
use crate::session::{Recovered, SessionManager};
use crate::ws::Host;

/// Directory in the data dir holding each recovered session's [`Origin`].
const ORIGINS: &str = "recovered";

/// What `herder recover` asks for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// The session to recover.
    pub session_id: SessionId,
    /// The account to run it on; picked when absent.
    #[serde(default)]
    pub account_id: Option<AccountId>,
    /// Recover even though the vault shows the session's host online.
    #[serde(default)]
    pub force: bool,
}

/// A session recovered here, and where from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    /// The session as it is here now.
    pub session: Recovered,
    /// Where it came from.
    pub origin: Origin,
}

/// Where a recovered session came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    /// The host it ran on.
    pub host_id: HostId,
    /// That host's name.
    pub host_name: String,
    /// When it was recovered here.
    pub recovered_at: Timestamp,
}

/// Recovers sessions from the vault onto this host.
pub struct Recovery {
    /// The vault.
    pub vault: VaultConfig,
    /// The key this host is paired with the vault as.
    pub device: DeviceKey,
    /// This host.
    pub host: Host,
    /// Where recovered sessions go on.
    pub sessions: SessionManager,
    /// The data dir, which keeps each recovered session's origin.
    pub data_dir: PathBuf,
}

impl Recovery {
    /// Recovers `request.session_id` from the vault onto this host.
    pub async fn recover(&self, request: Request) -> Result<Outcome> {
        let session_id = &request.session_id;
        let view = client::read(&self.vault, &self.device, Some(session_id)).await?;
        let Some(head) = view.sessions.iter().find(|s| s.session_id == *session_id) else {
            bail!("the vault holds no session {session_id}");
        };
        let Some(origin) = head.host_id.clone() else {
            bail!("the vault does not say which host has {session_id}");
        };
        if origin == self.host.id {
            bail!("session {session_id} is on this host already");
        }
        let host = view.hosts.iter().find(|host| host.host_id == origin);
        let host_name = host.map_or_else(|| origin.to_string(), |host| host.host_name.clone());
        if host.is_some_and(|host| host.online) && !request.force {
            bail!(
                "{host_name} ({origin}) is online, so {session_id} is not recovered: drive it \
                 there. Pass --force only if that host is gone or cut off and the vault has \
                 not noticed yet"
            );
        }
        let Some(project_id) = head.project_id.clone() else {
            bail!("the vault does not say which project {session_id} works on");
        };
        let session = self
            .sessions
            .recover(view.events, view.images, project_id, request.account_id)
            .await
            .map_err(|error| anyhow::anyhow!(error.message))?;
        let origin = Origin {
            host_id: origin,
            host_name,
            recovered_at: Timestamp::now(),
        };
        if let Err(err) = save_origin(&self.data_dir, session_id, &origin) {
            warn!(%session_id, "cannot keep where the session came from: {err:#}");
        }
        info!(
            %session_id,
            from_host = %origin.host_id,
            force = request.force,
            "recovered from the vault"
        );
        Ok(Outcome { session, origin })
    }
}

/// Makes every session of this host read-only that the vault now shows on another host,
/// which recovered it while this one was gone. A session this host itself recovered is
/// never taken for moved on by the vault still showing it on its origin.
pub(super) async fn release_recovered(
    vault: &VaultConfig,
    device: &DeviceKey,
    host: &Host,
    sessions: &SessionManager,
    data_dir: &Path,
) -> Result<()> {
    let view = client::read(vault, device, None).await?;
    let local: HashMap<SessionId, SessionStatus> = sessions
        .sessions()
        .await?
        .into_iter()
        .map(|head| (head.session_id, head.status))
        .collect();
    for head in view.sessions {
        let (Some(holder), Some(status)) = (&head.host_id, local.get(&head.session_id)) else {
            continue;
        };
        if *holder == host.id || *status == SessionStatus::Moved {
            continue;
        }
        let origin = load_origin(data_dir, &head.session_id)?;
        if origin.is_some_and(|origin| origin.host_id == *holder) {
            continue;
        }
        info!(
            session_id = %head.session_id,
            to_host = %holder,
            "the session was recovered on another host; it is read-only here now"
        );
        sessions
            .moved_away(head.session_id.clone())
            .await
            .map_err(|error| anyhow::anyhow!(error.message))?;
    }
    Ok(())
}

fn save_origin(data_dir: &Path, session_id: &SessionId, origin: &Origin) -> Result<()> {
    let dir = data_dir.join(ORIGINS);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    write_private(
        &dir,
        &format!("{session_id}.json"),
        &serde_json::to_vec(origin)?,
    )
}

fn load_origin(data_dir: &Path, session_id: &SessionId) -> Result<Option<Origin>> {
    let path = data_dir.join(ORIGINS).join(format!("{session_id}.json"));
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes)
                .with_context(|| format!("reading {}", path.display()))?,
        )),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
    }
}
