//! The fleet view the vault gives clients: every replicated session of every host, read-only.
//!
//! Clients pair with the vault and connect to it as to a daemon, on the port hosts replicate
//! to. They get the session list, and replay and follow each session's journal as its host
//! replicates it, the host list with each host's liveness, which the vault tells from its
//! replication connection ([`Presence`]), and the vault's status: what it holds of each host
//! and how far behind it is, at most every [`STATUS_INTERVAL`]. `get_attachment` answers from
//! the images hosts replicated; every other command on a session is refused as `read_only`: a
//! session is driven on its host, which the error names.
//!
//! Owners link hosts to the vault from a client: `pair_vault_host` mints a host-only code, as
//! `herder pair --host` does, for the client to hand the host with `link_vault`, and
//! `revoke_vault_host` unpairs the devices a host replicates from once it stops backing up.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use herder_protocol::{
    Account, AttachmentId, Batch, CommandBody, CommandId, CommandResult, ErrorCode, ErrorInfo,
    Event, EventBody, FleetHost, HostId, HostUsage, IMAGE_NOT_BACKED_UP, Seq, SessionHead,
    SessionId, Timestamp, VaultVolume,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::VaultStore;
use super::blocking;
use crate::auth::{Auth, PAIRING_TTL};
use crate::hub::Hub;
use crate::session::EventSink;
use crate::ws::{Backend, Identity};

/// The vault's sessions as the client server sees them.
#[derive(Clone)]
pub(crate) struct Fleet {
    store: Arc<Mutex<VaultStore>>,
    hub: Arc<Hub>,
    /// Pairs hosts and revokes them, for owners.
    auth: Arc<Auth>,
    presence: Arc<Presence>,
    /// The session list clients got last.
    listed: Arc<tokio::sync::Mutex<Vec<SessionHead>>>,
    /// Held while the host list is read and sent, so an older one never follows a newer one.
    sending_hosts: Arc<tokio::sync::Mutex<()>>,
    /// Woken when what the vault holds changed, for [`Fleet::publish_status`].
    status_changed: Arc<Notify>,
}

/// How often at most clients get the vault's status.
pub(crate) const STATUS_INTERVAL: Duration = Duration::from_secs(2);

impl Fleet {
    pub(crate) fn new(store: Arc<Mutex<VaultStore>>, hub: Arc<Hub>, auth: Arc<Auth>) -> Self {
        Self {
            store,
            hub,
            auth,
            presence: Arc::default(),
            listed: Arc::default(),
            sending_hosts: Arc::default(),
            status_changed: Arc::default(),
        }
    }

    pub(crate) fn presence(&self) -> &Presence {
        &self.presence
    }

    /// Sends the events of `batch`, just stored, to the session's subscribers; each client
    /// skips the ones it has.
    pub(crate) fn publish(&self, batch: &Batch) {
        for record in &batch.events {
            let event = record.to_event(batch.session_id.clone());
            // Bodies from a newer build cannot be sent, as on replay.
            if !matches!(event.body, EventBody::Unknown) {
                self.hub.event(&event);
            }
        }
    }

    /// Sends clients the vault's status whenever what it holds changed, at most every
    /// [`STATUS_INTERVAL`], until `shutdown`; the first one at once.
    pub(crate) async fn publish_status(&self, shutdown: CancellationToken) {
        self.status_changed.notify_one();
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = self.status_changed.notified() => {}
            }
            match blocking(&self.store, |store| store.status()).await {
                Ok(mut status) => {
                    for host in &mut status.hosts {
                        host.lag_ms = self.presence.lag_ms(&host.host_id);
                    }
                    self.hub.vault_status(status);
                }
                Err(err) => warn!("cannot read the vault's status: {err:#}"),
            }
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(STATUS_INTERVAL) => {}
            }
        }
    }

    /// Sends clients the session list if anything in it but the head seqs changed, as a
    /// daemon does; the vault's status follows.
    pub(crate) async fn refresh(&self) {
        self.status_changed.notify_one();
        // Held while reading, so a slower refresh never sends an older list after a newer one.
        let mut listed = self.listed.lock().await;
        let heads = match self.heads().await {
            Ok(heads) => heads,
            Err(err) => return warn!("cannot list the fleet: {err:#}"),
        };
        let headless = |heads: &[SessionHead]| -> Vec<SessionHead> {
            heads
                .iter()
                .map(|head| SessionHead {
                    head_seq: 0,
                    ..head.clone()
                })
                .collect()
        };
        if headless(&listed) != headless(&heads) {
            self.hub.sessions_changed(&heads);
            *listed = heads;
        }
    }

    /// Sends clients the host list with each host's liveness and usage now; the vault's status
    /// follows.
    pub(crate) async fn refresh_hosts(&self) {
        self.status_changed.notify_one();
        let _sending = self.sending_hosts.lock().await;
        let hosts = match blocking(&self.store, |store| store.hosts()).await {
            Ok(hosts) => hosts,
            Err(err) => return warn!("cannot list the hosts: {err:#}"),
        };
        let hosts = hosts
            .into_iter()
            .map(|host| FleetHost {
                online: self.presence.online(&host.host_id),
                last_seen: self
                    .presence
                    .last_heard(&host.host_id)
                    .unwrap_or(host.seen_at),
                usage: Some(HostUsage {
                    sessions: host.sessions,
                    attachment_bytes: host.attachment_bytes,
                    attachments_cap: host.attachments_cap,
                }),
                host_id: host.host_id,
                host_name: host.host_name,
            })
            .collect();
        self.hub.hosts_changed(hosts);
    }

    /// The bytes of the image `attachment_id` of `session_id`, from the copy of the host that
    /// has the session now.
    async fn attachment(
        &self,
        session_id: SessionId,
        attachment_id: AttachmentId,
    ) -> Result<CommandResult, ErrorInfo> {
        let found = {
            let (session_id, attachment_id) = (session_id.clone(), attachment_id.clone());
            blocking(&self.store, move |store| {
                let Some(host) = store.host_of(&session_id)? else {
                    return Ok(None);
                };
                store.attachment(&host, &session_id, &attachment_id)
            })
            .await
        };
        match found {
            Ok(Some(image)) => Ok(CommandResult::Attachment {
                media_type: image.media_type,
                data: image.data,
            }),
            Ok(None) => Err(error(
                ErrorCode::NotFound,
                format!(
                    "{IMAGE_NOT_BACKED_UP}: the vault holds no image {attachment_id} of \
                     session {session_id}"
                ),
            )),
            Err(err) => {
                warn!("cannot read an image: {err:#}");
                Err(error(
                    ErrorCode::Internal,
                    "cannot read the vault database".into(),
                ))
            }
        }
    }

    /// A one-time code that pairs the host named `host_name` to replicate here and only that.
    fn pair_host(&self, host_name: &str) -> Result<CommandResult, ErrorInfo> {
        let pairing = self
            .auth
            .mint_host(host_name, PAIRING_TTL)
            .map_err(|err| error(ErrorCode::BadRequest, format!("{err:#}")))?;
        Ok(CommandResult::HostPairing {
            code: pairing.code,
            expires_at: pairing.expires_at,
        })
    }

    /// Unpairs every device that replicates as `host_id`, closing its connections.
    async fn revoke_host(&self, host_id: HostId) -> Result<CommandResult, ErrorInfo> {
        let devices = {
            let host_id = host_id.clone();
            blocking(&self.store, move |store| store.devices_of(&host_id)).await
        };
        let devices = devices.map_err(|err| {
            warn!("cannot look up a host's devices: {err:#}");
            error(ErrorCode::Internal, "cannot read the vault database".into())
        })?;
        let mut revoked = 0;
        for device in &devices {
            match self.auth.revoke(device) {
                Ok(true) => revoked += 1,
                Ok(false) => {}
                Err(err) => {
                    warn!(device_id = %device, "cannot revoke a host's device: {err:#}");
                    return Err(error(
                        ErrorCode::Internal,
                        "cannot save the paired devices".into(),
                    ));
                }
            }
        }
        if revoked == 0 {
            return Err(error(
                ErrorCode::NotFound,
                format!("no paired device replicates as host {host_id}"),
            ));
        }
        Ok(CommandResult::Applied)
    }

    async fn heads(&self) -> anyhow::Result<Vec<SessionHead>> {
        blocking(&self.store, |store| store.fleet()).await
    }

    /// The refusal of a command on `session_id`, naming the host it belongs to.
    async fn read_only(&self, session_id: &SessionId) -> ErrorInfo {
        let found = {
            let session_id = session_id.clone();
            blocking(&self.store, move |store| {
                let Some(host) = store.host_of(&session_id)? else {
                    return Ok(None);
                };
                let record = store.hosts()?.into_iter().find(|h| h.host_id == host);
                Ok(Some((host, record)))
            })
            .await
        };
        let (host, record) = match found {
            Ok(Some(found)) => found,
            Ok(None) => {
                return error(
                    ErrorCode::NotFound,
                    format!("session {session_id} does not exist"),
                );
            }
            Err(err) => {
                warn!("cannot look up a session's host: {err:#}");
                return error(ErrorCode::Internal, "cannot read the vault database".into());
            }
        };
        let name = record.as_ref().map_or(host.as_str(), |r| &r.host_name);
        let liveness = if self.presence.online(&host) {
            "online".to_owned()
        } else {
            match self
                .presence
                .last_heard(&host)
                .or(record.as_ref().map(|r| r.seen_at))
            {
                Some(seen) => format!("offline, last seen {seen}"),
                None => "offline".to_owned(),
            }
        };
        error(
            ErrorCode::ReadOnly,
            format!(
                "session {session_id} is read-only on the vault; drive it on its host \
                 {name} ({host}), which is {liveness}"
            ),
        )
    }
}

impl Backend for Fleet {
    async fn sessions(&self) -> anyhow::Result<Vec<SessionHead>> {
        self.heads().await
    }

    fn accounts(&self) -> Vec<Account> {
        Vec::new()
    }

    fn refresh_usage(&self) {}

    async fn read_since(
        &self,
        session_id: &SessionId,
        after_seq: Seq,
        limit: usize,
    ) -> anyhow::Result<Vec<Event>> {
        let session_id = session_id.clone();
        blocking(&self.store, move |store| {
            let Some(host) = store.host_of(&session_id)? else {
                return Ok(Vec::new());
            };
            let records = store.records(&host, &session_id, after_seq, limit)?;
            Ok(records
                .iter()
                .map(|record| record.to_event(session_id.clone()))
                .collect())
        })
        .await
    }

    async fn command(
        &self,
        _: &Identity,
        _: &CommandId,
        command: CommandBody,
    ) -> Result<CommandResult, ErrorInfo> {
        match command {
            CommandBody::GetAttachment {
                session_id,
                attachment_id,
            } => return self.attachment(session_id, attachment_id).await,
            CommandBody::GetVaultLink => {
                let volume = blocking(&self.store, |store| {
                    Ok(store.path().parent().and_then(volume))
                });
                return Ok(CommandResult::VaultLink {
                    is_vault: true,
                    vault: None,
                    volume: volume.await.unwrap_or_default(),
                });
            }
            CommandBody::PairVaultHost { host_name } => return self.pair_host(&host_name),
            CommandBody::RevokeVaultHost { host_id } => return self.revoke_host(host_id).await,
            CommandBody::LinkVault { .. } | CommandBody::UnlinkVault => {
                return Err(error(
                    ErrorCode::Unsupported,
                    "a vault backs up nowhere; link a host to it instead".into(),
                ));
            }
            _ => {}
        }
        match target(&command) {
            Some(session_id) => Err(self.read_only(session_id).await),
            None => Err(error(
                ErrorCode::Unsupported,
                "the vault runs no sessions; do this on a host".into(),
            )),
        }
    }

    async fn worktree(&self, session_id: &SessionId) -> Result<PathBuf, ErrorInfo> {
        Err(self.read_only(session_id).await)
    }
}

/// The session a command acts on, if it acts on one.
fn target(command: &CommandBody) -> Option<&SessionId> {
    match command {
        CommandBody::ArchiveSession { session_id, .. }
        | CommandBody::UnarchiveSession { session_id }
        | CommandBody::SendPrompt { session_id, .. }
        | CommandBody::GetAttachment { session_id, .. }
        | CommandBody::Interrupt { session_id }
        | CommandBody::RemoveQueued { session_id, .. }
        | CommandBody::MoveQueued { session_id, .. }
        | CommandBody::SendQueuedNow { session_id, .. }
        | CommandBody::MergeQueued { session_id, .. }
        | CommandBody::SetModel { session_id, .. }
        | CommandBody::SetPermissionMode { session_id, .. }
        | CommandBody::AnswerApproval { session_id, .. }
        | CommandBody::AnswerQuestion { session_id, .. }
        | CommandBody::SwitchAccount { session_id, .. }
        | CommandBody::SwitchProvider { session_id, .. }
        | CommandBody::RenameSession { session_id, .. }
        | CommandBody::RetitleSession { session_id }
        | CommandBody::LinkPr { session_id, .. }
        | CommandBody::UnlinkPr { session_id, .. }
        | CommandBody::ComposeDown { session_id, .. }
        | CommandBody::OpenTerminal { session_id, .. } => Some(session_id),
        CommandBody::CreateSession { .. }
        | CommandBody::ForkSession { .. }
        | CommandBody::UploadHistory { .. }
        | CommandBody::ListDirectory { .. }
        | CommandBody::AddProject { .. }
        | CommandBody::SetProjectSettings { .. }
        | CommandBody::RemoveProject { .. }
        | CommandBody::SetProjectIcon { .. }
        | CommandBody::GetProjectIcon { .. }
        | CommandBody::GetVaultLink
        | CommandBody::GetUsageSummary { .. }
        | CommandBody::LinkVault { .. }
        | CommandBody::UnlinkVault
        | CommandBody::PairVaultHost { .. }
        | CommandBody::RevokeVaultHost { .. }
        | CommandBody::PairDevice
        | CommandBody::SetAccountSettings { .. }
        | CommandBody::SetResourceLimits { .. }
        | CommandBody::GetSettings
        | CommandBody::SetSettings { .. }
        | CommandBody::RestartDaemon
        | CommandBody::SetSkillsRepo { .. }
        | CommandBody::PutSkill { .. }
        | CommandBody::DeleteSkill { .. }
        | CommandBody::ImportSkill { .. }
        | CommandBody::PullSkills
        | CommandBody::SetSkillEnabled { .. }
        | CommandBody::AddAccount { .. }
        | CommandBody::InstallProvider { .. }
        | CommandBody::LogInAccount { .. }
        | CommandBody::AttachTerminal { .. }
        | CommandBody::DetachTerminal { .. }
        | CommandBody::ResizeTerminal { .. }
        | CommandBody::TerminalInput { .. } => None,
    }
}

/// The size and use of the volume `dir` is on; `None` when it cannot be read.
fn volume(dir: &Path) -> Option<VaultVolume> {
    let stat = match nix::sys::statvfs::statvfs(dir) {
        Ok(stat) => stat,
        Err(err) => {
            warn!(dir = %dir.display(), "cannot read the vault's disk usage: {err}");
            return None;
        }
    };
    // Each is a `u64` on Linux under a libc alias, so the casts widen nothing.
    let block = stat.fragment_size() as u64;
    let total = (stat.blocks() as u64).saturating_mul(block);
    let free = (stat.blocks_free() as u64).saturating_mul(block);
    Some(VaultVolume {
        total_bytes: total,
        used_bytes: total.saturating_sub(free),
    })
}

fn error(code: ErrorCode, message: String) -> ErrorInfo {
    ErrorInfo { code, message }
}

/// Which hosts have a replication connection open, and when the others were last heard from
/// since the vault started.
#[derive(Debug, Default)]
pub(crate) struct Presence(Mutex<HashMap<HostId, Seen>>);

#[derive(Debug)]
struct Seen {
    /// Open connections; a reconnecting host briefly has two.
    connections: usize,
    /// When any of them last received a frame.
    at: Timestamp,
    /// The age of the newest event of the host's latest stored batch, when it was stored.
    lag: Option<Duration>,
}

impl Presence {
    pub(crate) fn connected(&self, host: &HostId) {
        let mut hosts = self.lock();
        let seen = hosts.entry(host.clone()).or_insert(Seen {
            connections: 0,
            at: Timestamp::now(),
            lag: None,
        });
        seen.connections += 1;
        seen.at = Timestamp::now();
    }

    pub(crate) fn heard(&self, host: &HostId) {
        if let Some(seen) = self.lock().get_mut(host) {
            seen.at = Timestamp::now();
        }
    }

    /// Records that a batch of `host`'s whose newest event happened `at` was stored now.
    pub(crate) fn stored(&self, host: &HostId, at: Timestamp) {
        if let Some(seen) = self.lock().get_mut(host) {
            // A host clock ahead of the vault's makes no lag.
            let lag = Timestamp::now().duration_since(at);
            seen.lag = Some(lag.try_into().unwrap_or(Duration::ZERO));
        }
    }

    /// The lag of `host`'s latest stored batch ([`Presence::stored`]), in milliseconds.
    pub(crate) fn lag_ms(&self, host: &HostId) -> Option<u64> {
        let lag = self.lock().get(host)?.lag?;
        Some(u64::try_from(lag.as_millis()).unwrap_or(u64::MAX))
    }

    /// Ends one of `host`'s connections; returns when it was last heard from.
    pub(crate) fn disconnected(&self, host: &HostId) -> Timestamp {
        let mut hosts = self.lock();
        match hosts.get_mut(host) {
            Some(seen) => {
                seen.connections = seen.connections.saturating_sub(1);
                seen.at
            }
            None => Timestamp::now(),
        }
    }

    /// Whether `host` has a replication connection open.
    pub(crate) fn online(&self, host: &HostId) -> bool {
        self.lock()
            .get(host)
            .is_some_and(|seen| seen.connections > 0)
    }

    /// Every host with a replication connection open.
    pub(crate) fn online_hosts(&self) -> Vec<HostId> {
        self.lock()
            .iter()
            .filter(|(_, seen)| seen.connections > 0)
            .map(|(host, _)| host.clone())
            .collect()
    }

    /// When `host` was last heard from, if since the vault started.
    pub(crate) fn last_heard(&self, host: &HostId) -> Option<Timestamp> {
        self.lock().get(host).map(|seen| seen.at)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<HostId, Seen>> {
        // Every update is a single field write, so a poisoned map is consistent.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
