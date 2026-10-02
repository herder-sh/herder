//! Project discovery: which repositories this host holds, grouped into projects.
//!
//! Repositories come from three places: the repo of every session, a scan of the configured
//! roots ([`scan`]), and the paths `[[project]]` entries declare. Each repository's project is
//! its `origin` remote normalised by [`ProjectId::from_remote`], or a local project of this
//! host when it has none. `[[project]]` entries then override that ([`resolve`]): they name a
//! project, merge several remotes into one, and add or declare clones by path.
//!
//! [`Discovery::run`] scans at startup and every [`RESCAN_INTERVAL`], and looks at the session
//! repos again whenever a session is created ([`OnSessionsChanged`]). Each time the resolved
//! list differs from the last, it goes to every client through [`Hub::projects_changed`].

pub mod scan;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use herder_protocol::{
    AccountId, Event, HostId, Item, ItemId, Project, ProjectId, SessionHead, SessionId,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::hub::Hub;
use crate::session::{EventSink, SessionManager};

/// How often the roots are scanned again and every remote re-read.
pub const RESCAN_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// The `[projects]` table and the `[[project]]` entries, resolved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectsConfig {
    /// Directories scanned for repositories.
    pub roots: Vec<PathBuf>,
    /// Overrides, in file order.
    pub entries: Vec<ProjectEntry>,
}

/// One `[[project]]` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectEntry {
    /// Display name; the last segment of the project id when absent.
    pub name: Option<String>,
    /// Remotes, normalised, whose clones all belong to this project; the first gives its id.
    pub remotes: Vec<ProjectId>,
    /// Clones that belong to this project whatever their remote. Without `remotes`, the
    /// first of them that exists gives the project its id.
    pub paths: Vec<PathBuf>,
    /// Account new sessions of the project use when none is chosen.
    pub default_account: Option<AccountId>,
    /// Shell command run in each new worktree of the project.
    pub setup_command: Option<String>,
}

/// A repository on this host and its `origin` remote URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// Absolute path of the repository.
    pub path: PathBuf,
    /// `remote.origin.url`, when it has one.
    pub origin: Option<String>,
}

/// Groups `repos` into projects under the `entries` overrides, ordered by project id.
///
/// A repository belongs to the entry that lists its path, else to the entry whose remotes or
/// id match its own id, else to its own project. When two entries claim one remote, the first
/// wins. Repositories whose path is not UTF-8 are left out, as the protocol carries strings.
pub fn resolve(host: &HostId, repos: &[Repo], entries: &[ProjectEntry]) -> Vec<Project> {
    let own_id = |repo: &Repo| {
        repo.origin
            .as_deref()
            .and_then(ProjectId::from_remote)
            .unwrap_or_else(|| ProjectId::local(host, &repo.path.to_string_lossy()))
    };
    let by_path: HashMap<&Path, &Repo> = repos.iter().map(|r| (r.path.as_path(), r)).collect();
    let entry_ids: Vec<Option<ProjectId>> = entries
        .iter()
        .map(|entry| match entry.remotes.first() {
            Some(remote) => Some(remote.clone()),
            None => entry
                .paths
                .iter()
                .find_map(|path| by_path.get(path.as_path()))
                .map(|repo| own_id(repo)),
        })
        .collect();
    let mut claimed_paths = HashMap::new();
    let mut claimed_ids = HashMap::new();
    for (index, (entry, id)) in entries.iter().zip(&entry_ids).enumerate() {
        let Some(id) = id else { continue };
        for path in &entry.paths {
            claimed_paths.entry(path.as_path()).or_insert(index);
        }
        for remote in entry.remotes.iter().chain([id]) {
            claimed_ids.entry(remote.clone()).or_insert(index);
        }
    }

    let mut projects: BTreeMap<ProjectId, (Option<usize>, BTreeSet<String>)> = BTreeMap::new();
    for repo in repos {
        let Some(path) = repo.path.to_str() else {
            debug!("leaving out {}: its path is not UTF-8", repo.path.display());
            continue;
        };
        let own = own_id(repo);
        let entry = claimed_paths
            .get(repo.path.as_path())
            .or_else(|| claimed_ids.get(&own))
            .copied();
        let id = entry
            .and_then(|index| entry_ids[index].clone())
            .unwrap_or(own);
        let project = projects.entry(id).or_insert((entry, BTreeSet::new()));
        project.1.insert(path.to_owned());
    }

    projects
        .into_iter()
        .map(|(project_id, (entry, paths))| {
            let entry = entry.map(|index| &entries[index]);
            Project {
                name: entry
                    .and_then(|e| e.name.clone())
                    .unwrap_or_else(|| default_name(&project_id)),
                paths: paths.into_iter().collect(),
                default_account: entry.and_then(|e| e.default_account.clone()),
                setup_command: entry.and_then(|e| e.setup_command.clone()),
                project_id,
            }
        })
        .collect()
}

/// The last segment of a project id: the repository name of `github.com/org/repo`, the
/// directory name of a local `HOST:/home/dev/scratch`.
fn default_name(id: &ProjectId) -> String {
    let id = id.as_str();
    id.rsplit(['/', ':'])
        .find(|segment| !segment.is_empty())
        .unwrap_or(id)
        .to_owned()
}

/// Wraps the session manager's sink to wake [`Discovery`] when a session is created, as its
/// repo may be new.
pub struct OnSessionsChanged {
    /// Where every event goes on to.
    pub next: Arc<dyn EventSink>,
    /// Notified on every session list change.
    pub notify: Arc<Notify>,
}

impl EventSink for OnSessionsChanged {
    fn event(&self, event: &Event) {
        self.next.event(event);
    }

    fn snapshot(&self, session_id: &SessionId, item: &Item) {
        self.next.snapshot(session_id, item);
    }

    fn delta(&self, session_id: &SessionId, item_id: &ItemId, text: &str) {
        self.next.delta(session_id, item_id, text);
    }

    fn sessions_changed(&self, sessions: &[SessionHead]) {
        self.notify.notify_one();
        self.next.sessions_changed(sessions);
    }
}

/// Keeps the hub's project list current.
pub struct Discovery {
    /// This host, for the ids of repositories without a remote.
    pub host: HostId,
    /// Roots and overrides.
    pub config: ProjectsConfig,
    /// Where the list is published.
    pub hub: Arc<Hub>,
    /// Whose repos are discovered.
    pub sessions: SessionManager,
    /// Notified when a session is created; see [`OnSessionsChanged`].
    pub sessions_changed: Arc<Notify>,
}

impl Discovery {
    /// Publishes the project list at once, then again whenever it changes, until `shutdown`.
    pub async fn run(self, shutdown: CancellationToken) {
        let mut rescan = tokio::time::interval(RESCAN_INTERVAL);
        rescan.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Repositories found by the last scan, and every repository with its remote.
        let mut scanned = Vec::new();
        let mut repos: BTreeMap<PathBuf, Option<String>> = BTreeMap::new();
        let mut published = None;
        loop {
            let full = tokio::select! {
                () = shutdown.cancelled() => return,
                _ = rescan.tick() => true,
                () = self.sessions_changed.notified() => false,
            };
            let session_repos = match self.sessions.repos().await {
                Ok(paths) => paths,
                Err(err) => {
                    warn!("cannot list session repos for projects: {err:#}");
                    Vec::new()
                }
            };
            if full {
                let roots = self.config.roots.clone();
                scanned = tokio::task::spawn_blocking(move || scan::repos(&roots))
                    .await
                    .unwrap_or_else(|err| {
                        warn!("the project scan panicked: {err}");
                        Vec::new()
                    });
                repos.clear();
            }
            let wanted = self.wanted(&scanned, session_repos);
            repos.retain(|path, _| wanted.contains(path));
            let new: Vec<PathBuf> = wanted
                .into_iter()
                .filter(|path| !repos.contains_key(path))
                .collect();
            if new.is_empty() && !full {
                continue;
            }
            let origins = tokio::task::spawn_blocking(move || {
                new.into_iter()
                    .map(|path| {
                        let origin = scan::origin(&path);
                        (path, origin)
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
            repos.extend(origins);
            let list: Vec<Repo> = repos
                .iter()
                .map(|(path, origin)| Repo {
                    path: path.clone(),
                    origin: origin.clone(),
                })
                .collect();
            let projects = resolve(&self.host, &list, &self.config.entries);
            if published.as_ref() != Some(&projects) {
                debug!(projects = projects.len(), "project list changed");
                self.hub.projects_changed(projects.clone());
                published = Some(projects);
            }
        }
    }

    /// Every repository to resolve: those `scanned`, the session repos still there and the
    /// declared paths that are directories.
    fn wanted(&self, scanned: &[PathBuf], session_repos: Vec<PathBuf>) -> BTreeSet<PathBuf> {
        let declared = self.config.entries.iter().flat_map(|entry| &entry.paths);
        let mut wanted: BTreeSet<PathBuf> = scanned.iter().cloned().collect();
        wanted.extend(session_repos.into_iter().filter(|path| scan::is_repo(path)));
        wanted.extend(declared.filter(|path| path.is_dir()).cloned());
        wanted
    }
}
