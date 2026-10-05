//! Keeps every machine where this device's user is owner on one skill library.
//!
//! The library is the repository URL last set through this client, else the one the first
//! owner machine reports. A machine whose daemon reports another repository, or none, when a
//! connection comes up is sent `set_skills_repo`, at most once per connection; one whose
//! repository changes while connected, set from another device, changes the library instead.
//! A write accepted by one machine (`put_skill`, `delete_skill`, `import_skill`) marks every
//! other machine as behind, and each is sent `pull_skills` once it is connected and has sent
//! its status. Only daemons that send a `skills_status` take part; vaults do not.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use herder_protocol::{Command, CommandBody, HostId};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::new_command_id;
use crate::supervisor::Supervisor;

/// What the client knows of the library and of each machine's place in it.
#[derive(Default)]
pub(crate) struct Library {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// The repository every owner machine is to use.
    repo: Option<String>,
    /// Each machine's repository as its daemon last reported it, and on which connection.
    reported: HashMap<HostId, (u32, Option<String>)>,
    /// The connection each machine was last sent `set_skills_repo` on, and the URL.
    sent: HashMap<HostId, (u32, String)>,
    /// Machines that missed a write and are to pull.
    behind: HashSet<HostId>,
}

impl Library {
    /// `machine` accepted `set_skills_repo` with `url` on its connection `connection`: the
    /// library is `url` from now on.
    pub(crate) fn repo_set(&self, machine: &HostId, connection: u32, url: String) {
        let mut state = self.lock();
        state.sent.clear();
        state
            .sent
            .insert(machine.clone(), (connection, url.clone()));
        state.repo = Some(url);
    }

    /// `machine` accepted a write: every other one of `machines` is behind.
    pub(crate) fn wrote(&self, machine: &HostId, machines: impl IntoIterator<Item = HostId>) {
        let mut state = self.lock();
        state
            .behind
            .extend(machines.into_iter().filter(|other| other != machine));
    }

    /// Sends each of `machines` what it needs to be on the library.
    fn sync(&self, machines: &[Arc<Supervisor>]) {
        let reports: Vec<_> = machines
            .iter()
            .filter_map(|machine| Some((machine, machine.library()?)))
            .collect();
        let mut state = self.lock();
        for (machine, (connection, repo)) in &reports {
            let host_id = &machine.saved.host_id;
            let previous = state
                .reported
                .insert(host_id.clone(), (*connection, repo.clone()));
            let Some(repo) = repo else { continue };
            // Set from another device while connected, or the first library this client sees.
            let changed =
                matches!(&previous, Some((c, p)) if c == connection && p.as_ref() != Some(repo));
            let ours = state.repo.as_deref().is_some_and(|url| same(url, repo));
            if state.repo.is_none() || (changed && !ours) {
                debug!(%repo, "the skill library is now the one {host_id} uses");
                state.repo = Some(repo.clone());
                state.sent.clear();
            }
        }
        let Some(url) = state.repo.clone() else {
            return;
        };
        for (machine, (connection, repo)) in reports {
            let host_id = &machine.saved.host_id;
            if repo.as_deref().is_some_and(|repo| same(&url, repo)) {
                if state.behind.remove(host_id) {
                    send(machine, CommandBody::PullSkills);
                }
                continue;
            }
            let sending = (connection, url.clone());
            if state.sent.get(host_id) == Some(&sending) {
                continue;
            }
            state.sent.insert(host_id.clone(), sending);
            // A fresh clone holds every write.
            state.behind.remove(host_id);
            send(machine, CommandBody::SetSkillsRepo { url: url.clone() });
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // Every update is a few inserts and removals, so a poisoned state is consistent.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Syncs `library` across `machines` whenever they change, until `stop` or until `machines`
/// is gone.
pub(crate) async fn run(
    library: Arc<Library>,
    machines: Weak<Mutex<Vec<Arc<Supervisor>>>>,
    mut changed: watch::Receiver<u64>,
    stop: CancellationToken,
) {
    loop {
        {
            let Some(machines) = machines.upgrade() else {
                return;
            };
            let machines = machines
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            library.sync(&machines);
        }
        tokio::select! {
            () = stop.cancelled() => return,
            changed = changed.changed() => if changed.is_err() { return },
        }
    }
}

/// Sends `body` to `machine` in the background, logging a refusal.
fn send(machine: &Arc<Supervisor>, body: CommandBody) {
    let machine = Arc::clone(machine);
    tokio::spawn(async move {
        let command = Command {
            id: new_command_id(),
            body,
        };
        match machine.send(command).await {
            Ok(Ok(_)) | Err(_) => {}
            Ok(Err(error)) => {
                warn!(machine = %machine.saved.name, "syncing the skill library: {}", error.message);
            }
        }
    });
}

/// Whether the daemon's `reported` repository is `url`, which it shows without credentials.
fn same(url: &str, reported: &str) -> bool {
    url == reported || without_credentials(url) == reported
}

/// `url` without the user info of a `scheme://` URL, as the daemon reports it.
fn without_credentials(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let end = rest.find('/').unwrap_or(rest.len());
    match rest[..end].rsplit_once('@') {
        Some((_, host)) => format!("{scheme}://{host}{}", &rest[end..]),
        None => url.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_stripped_for_comparison() {
        let plain = "https://github.com/you/skills.git";
        assert_eq!(without_credentials(plain), plain);
        assert_eq!(
            without_credentials("https://x-access-token:secret@github.com/you/skills.git"),
            plain
        );
        assert_eq!(
            without_credentials("https://secret@github.com/you/skills.git"),
            plain
        );
        assert_eq!(
            without_credentials("ssh://git@github.com/you/skills.git"),
            "ssh://github.com/you/skills.git"
        );
        assert_eq!(
            without_credentials("git@github.com:you/skills.git"),
            "git@github.com:you/skills.git"
        );
        assert!(same("https://t@github.com/you/skills.git", plain));
        assert!(!same("https://github.com/you/other.git", plain));
    }
}
