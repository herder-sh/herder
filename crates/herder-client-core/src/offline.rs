//! The offline cache: what each machine last said, saved in `<config_dir>/cache/` so a client
//! opened without a connection shows it at once.
//!
//! One file per machine holds the role, the lists the daemon sent (sessions, fleet hosts,
//! projects, accounts) and the events of the [`RECENT`] listed sessions whose latest event is
//! the newest. A session's events are saved complete, from its first, because its subscription
//! resumes after the last one held: a trimmed log would hide the older ones for good.
//!
//! Live data always wins. A supervisor reads its file only when it starts, before it connects;
//! from then on the daemon's lists replace the cached ones and its events extend the cached
//! logs by seq. Each save writes what is held at that moment, under the supervisor's save lock,
//! so a slower save of older state never lands after a newer one.
//!
//! The files hold transcripts, so they are private like the profile. A missing or unreadable
//! file is an empty cache: it is only a cache.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use herder_protocol::SessionHead;
use herder_protocol::{Account, Event, FailoverSettings, FleetHost, HostId, Project, Role};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::profile::write_private;

/// How many sessions keep their events in the cache.
pub(crate) const RECENT: usize = 20;

const DIR: &str = "cache";

/// A machine as cached.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Cached {
    pub(crate) role: Option<Role>,
    pub(crate) sessions: Vec<SessionHead>,
    pub(crate) hosts: Vec<FleetHost>,
    pub(crate) projects: Vec<Project>,
    pub(crate) accounts: Vec<Account>,
    pub(crate) failover: FailoverSettings,
    #[serde(default)]
    pub(crate) providers: Vec<herder_protocol::ProviderStatus>,
    /// Each cached session's events, in seq order from its first.
    pub(crate) logs: Vec<Vec<Event>>,
}

/// The file name of `host_id`'s cache. The host id comes from the daemon, so it is hashed
/// rather than trusted as a path.
fn file(host_id: &HostId) -> String {
    let hash: String = Sha256::digest(host_id.as_str().as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{hash}.json")
}

fn dir(config_dir: &Path) -> PathBuf {
    config_dir.join(DIR)
}

/// What is cached for `host_id`; empty when nothing is, or the file is unreadable.
pub(crate) fn load(config_dir: &Path, host_id: &HostId) -> Cached {
    let path = dir(config_dir).join(file(host_id));
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Cached::default(),
        Err(err) => {
            warn!("ignoring the offline cache {}: {err}", path.display());
            return Cached::default();
        }
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|err| {
        warn!("ignoring the offline cache {}: {err}", path.display());
        Cached::default()
    })
}

/// Replaces what is cached for `host_id` with `cached`.
pub(crate) fn save(config_dir: &Path, host_id: &HostId, cached: &Cached) -> Result<(), String> {
    let json = serde_json::to_vec(cached).map_err(|err| format!("encoding: {err}"))?;
    write_private(&dir(config_dir), &file(host_id), &json)
}

/// Deletes what is cached for `host_id`.
pub(crate) fn remove(config_dir: &Path, host_id: &HostId) {
    let path = dir(config_dir).join(file(host_id));
    if let Err(err) = fs::remove_file(&path)
        && err.kind() != io::ErrorKind::NotFound
    {
        warn!("removing the offline cache {}: {err}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use herder_protocol::{AccountId, EventBody, SessionId, SessionStatus, Timestamp};

    use super::*;

    #[test]
    fn a_cache_survives_a_save_is_private_and_a_bad_one_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let host = HostId::new("../../machines");
        assert_eq!(load(tmp.path(), &host), Cached::default());
        let session_id = SessionId::new("s");
        let cached = Cached {
            role: Some(Role::Owner),
            sessions: vec![SessionHead {
                session_id: session_id.clone(),
                host_id: None,
                head_seq: 1,
                status: SessionStatus::Idle,
                parent: None,
                parent_host: None,
                task: None,
                title: None,
                project_id: None,
                account_id: AccountId::new("a"),
                children_need_you: 0,
                queue: Vec::new(),
                chat: false,
            }],
            logs: vec![vec![Event {
                session_id,
                seq: 1,
                at: Timestamp::UNIX_EPOCH,
                by: None,
                body: EventBody::SessionStatusChanged {
                    retry_at: None,
                    status: SessionStatus::Idle,
                },
            }]],
            ..Cached::default()
        };
        save(tmp.path(), &host, &cached).unwrap();
        assert_eq!(load(tmp.path(), &host), cached);

        // The daemon's host id never escapes the cache dir.
        let files: Vec<_> = fs::read_dir(tmp.path().join(DIR))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(files, [tmp.path().join(DIR).join(file(&host))]);
        let mode = fs::metadata(&files[0]).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        fs::write(&files[0], "not json").unwrap();
        assert_eq!(load(tmp.path(), &host), Cached::default());
        remove(tmp.path(), &host);
        remove(tmp.path(), &host);
        assert!(!files[0].exists());
    }
}
