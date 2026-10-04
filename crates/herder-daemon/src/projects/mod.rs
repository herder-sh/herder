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
//!
//! Owners change the `[[project]]` entries from a client: `add_project` declares a repository,
//! `set_project_settings` replaces a project's settings and `remove_project` drops a project:
//! its clones leave the entries and go into `[projects] exclude`, which discovery leaves out
//! wherever it finds them, until `add_project` declares one again. Each rewrites the daemon's
//! config file and takes effect at once ([`Overrides`]); discovery then rescans and publishes
//! the list. Removing never touches the repositories themselves.
//!
//! Each project's `icon` is the image an owner uploaded with `set_project_icon`, kept in the
//! daemon's data dir, else an image file found in its first clone ([`icon`]); discovery reads
//! it on every full scan, and `get_project_icon` reads it again when asked. Setting or
//! clearing an upload rescans and publishes the list at once, as changing the entries does.

pub mod icon;
pub mod scan;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use anyhow::Context;
use herder_protocol::{
    Account, AccountId, Event, HostId, Item, ItemId, PermissionMode, Project, ProjectId,
    SessionHead, SessionId,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::hub::Hub;
use crate::session::{EventSink, SessionManager};

/// How often the roots are scanned again and every remote re-read.
pub const RESCAN_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// How long a project's setup command may run in a new worktree unless configured otherwise.
pub const SETUP_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// The `[projects]` table and the `[[project]]` entries, resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectsConfig {
    /// Directories scanned for repositories.
    pub roots: Vec<PathBuf>,
    /// How long a setup command may run before it is killed and the setup fails.
    pub setup_timeout: Duration,
    /// Repositories left out wherever they are found: the clones of removed projects.
    pub exclude: Vec<PathBuf>,
    /// Overrides, in file order.
    pub entries: Vec<ProjectEntry>,
}

impl Default for ProjectsConfig {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            setup_timeout: SETUP_TIMEOUT,
            exclude: Vec::new(),
            entries: Vec::new(),
        }
    }
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
    /// Permission mode new sessions of the project start in when none is chosen.
    pub default_permission_mode: Option<PermissionMode>,
    /// Shell command run in each new worktree of the project.
    pub setup_command: Option<String>,
    /// The project's icon, relative to each clone; tried before the files [`icon`] looks for.
    pub icon: Option<PathBuf>,
}

/// The `[projects]` table and `[[project]]` entries as they are now: what the daemon started
/// with, then each change a client made, which is also written to the config file. Shared by
/// discovery and the session manager.
#[derive(Debug)]
pub struct Overrides {
    /// The daemon's config file.
    file: PathBuf,
    /// Where uploaded icons are kept ([`icon::upload`]).
    icons: PathBuf,
    config: RwLock<ProjectsConfig>,
    changed: Notify,
}

impl Overrides {
    /// Starts from `config`, as loaded from the config file `file`, where changes go, with
    /// uploaded icons kept in the folder `icons`.
    pub fn new(file: PathBuf, icons: PathBuf, config: ProjectsConfig) -> Self {
        Self {
            file,
            icons,
            config: RwLock::new(config),
            changed: Notify::new(),
        }
    }

    /// The table and entries now.
    pub fn config(&self) -> ProjectsConfig {
        self.config
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Where uploaded icons are kept.
    pub fn icons(&self) -> &Path {
        &self.icons
    }

    /// Resolves once a client changed the entries or an uploaded icon since the last call
    /// returned.
    pub async fn changed(&self) {
        self.changed.notified().await;
    }

    /// Declares the repository at `repo`, a normalised absolute path, as a project on `host`:
    /// adds it to the paths of the entry its remote already belongs to, else to a new entry;
    /// nothing when an entry lists it already. Returns its project. Blocks on the file system.
    pub fn add(&self, host: &HostId, repo: &Path) -> anyhow::Result<ProjectId> {
        let current = crate::config::read_projects(&self.file)?;
        let entries = &current.entries;
        let declared = entries
            .iter()
            .any(|entry| entry.paths.iter().any(|p| p == repo));
        let config = if declared {
            current
        } else {
            let own = scan::origin(repo)
                .as_deref()
                .and_then(ProjectId::from_remote);
            let entry = own.and_then(|own| {
                entries
                    .iter()
                    .position(|entry| entry.remotes.contains(&own))
            });
            crate::config::add_project(&self.file, entry, repo)?
        };
        let project = of_repo(host, repo, &config.entries)
            .with_context(|| format!("{} cannot be a project", repo.display()))?;
        self.replace(config);
        Ok(project.project_id)
    }

    /// Replaces the settings of `project`, as discovery last listed it, in the entry that
    /// shapes it, or a new entry declaring its first clone. Blocks on the file system.
    pub fn set(
        &self,
        project: &Project,
        settings: &crate::config::ProjectSettings,
    ) -> anyhow::Result<()> {
        let current = crate::config::read_projects(&self.file)?;
        let clone = project
            .paths
            .first()
            .with_context(|| format!("project {} has no clone here", project.project_id))?;
        let entry = entry_of(&current.entries, project);
        let config =
            crate::config::set_project_settings(&self.file, entry, Path::new(clone), settings)?;
        self.replace(config);
        Ok(())
    }

    /// Removes `project`, as discovery last listed it, from this host's projects: its clones
    /// leave every entry and are excluded from discovery ([`crate::config::remove_project`]).
    /// Nothing on disk but the config file changes. Blocks on the file system.
    pub fn remove(&self, project: &Project) -> anyhow::Result<()> {
        let clones: Vec<PathBuf> = project.paths.iter().map(PathBuf::from).collect();
        let config = crate::config::remove_project(&self.file, &clones)?;
        self.replace(config);
        Ok(())
    }

    /// Keeps `icon`, a media type and the bytes of an image of it, as `project`'s uploaded
    /// icon, or deletes the upload when `None` ([`icon::upload`]). Blocks on the file system.
    pub fn set_icon(&self, project: &ProjectId, icon: Option<(&str, &[u8])>) -> anyhow::Result<()> {
        icon::upload(&self.icons, project, icon)?;
        self.changed.notify_one();
        Ok(())
    }

    fn replace(&self, config: ProjectsConfig) {
        *self.config.write().unwrap_or_else(PoisonError::into_inner) = config;
        self.changed.notify_one();
    }
}

/// The index of the entry in `entries` that shapes `project`, as discovery listed it: the one
/// whose first remote is its id, else the first that lists one of its clones.
fn entry_of(entries: &[ProjectEntry], project: &Project) -> Option<usize> {
    entries.iter().position(|entry| {
        entry.remotes.first() == Some(&project.project_id)
            || entry.paths.iter().any(|path| {
                project
                    .paths
                    .iter()
                    .any(|clone| Path::new(clone) == path.as_path())
            })
    })
}

/// The icon of `project`, as discovery listed it: the one uploaded into `icons`
/// ([`icon::uploaded`]), else the one in its first clone that exists, trying first the `icon`
/// of the entry in `entries` that shapes it ([`icon::find`]). Blocks on the file system.
pub fn icon(project: &Project, entries: &[ProjectEntry], icons: &Path) -> Option<icon::Icon> {
    if let Some(uploaded) = icon::uploaded(icons, &project.project_id) {
        return Some(uploaded);
    }
    let explicit = entry_of(entries, project).and_then(|index| entries[index].icon.as_deref());
    let clone = project
        .paths
        .iter()
        .map(Path::new)
        .find(|path| path.is_dir())?;
    icon::find(clone, explicit)
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
                default_permission_mode: entry.and_then(|e| e.default_permission_mode),
                setup_command: entry.and_then(|e| e.setup_command.clone()),
                icon: None,
                icon_uploaded: false,
                project_id,
            }
        })
        .collect()
}

/// The project of the repository at `repo` under the `entries` overrides, reading its remote;
/// blocks on the file system.
pub fn of_repo(host: &HostId, repo: &Path, entries: &[ProjectEntry]) -> Option<Project> {
    let repo = Repo {
        path: repo.to_owned(),
        origin: scan::origin(repo),
    };
    resolve(host, &[repo], entries).pop()
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

    fn accounts_changed(&self, accounts: &[Account]) {
        self.next.accounts_changed(accounts);
    }
}

/// Keeps the hub's project list current.
pub struct Discovery {
    /// This host, for the ids of repositories without a remote.
    pub host: HostId,
    /// Roots and overrides.
    pub config: Arc<Overrides>,
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
                // New roots or entries: scan everything again.
                () = self.config.changed() => true,
            };
            let config = self.config.config();
            let session_repos = match self.sessions.repos().await {
                Ok(paths) => paths,
                Err(err) => {
                    warn!("cannot list session repos for projects: {err:#}");
                    Vec::new()
                }
            };
            if full {
                let roots = config.roots.clone();
                scanned = tokio::task::spawn_blocking(move || scan::repos(&roots))
                    .await
                    .unwrap_or_else(|err| {
                        warn!("the project scan panicked: {err}");
                        Vec::new()
                    });
                repos.clear();
            }
            let wanted = wanted(&config, &scanned, session_repos);
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
            let projects = with_icons(
                resolve(&self.host, &list, &config.entries),
                config,
                self.config.icons().to_owned(),
            )
            .await;
            if published.as_ref() != Some(&projects) {
                debug!(projects = projects.len(), "project list changed");
                self.hub.projects_changed(projects.clone());
                self.sessions.set_projects(&projects).await;
                published = Some(projects);
            }
        }
    }
}

/// `projects` with the hash of each one's icon under `config` and the uploads in `icons`.
async fn with_icons(
    mut projects: Vec<Project>,
    config: ProjectsConfig,
    icons: PathBuf,
) -> Vec<Project> {
    let listed = projects.clone();
    let icons = tokio::task::spawn_blocking(move || {
        listed
            .iter()
            .map(|project| {
                icon(project, &config.entries, &icons).map(|icon| (icon.hash, icon.uploaded))
            })
            .collect()
    })
    .await
    .unwrap_or_else(|err| {
        warn!("looking for project icons panicked: {err}");
        Vec::new()
    });
    for (project, icon) in projects.iter_mut().zip(icons) {
        project.icon_uploaded = icon.as_ref().is_some_and(|(_, uploaded)| *uploaded);
        project.icon = icon.map(|(hash, _)| hash);
    }
    projects
}

/// Every repository to resolve: those `scanned`, the session repos still there and the paths
/// `config` declares that are directories, but none `config` excludes.
fn wanted(
    config: &ProjectsConfig,
    scanned: &[PathBuf],
    session_repos: Vec<PathBuf>,
) -> BTreeSet<PathBuf> {
    let declared = config.entries.iter().flat_map(|entry| &entry.paths);
    let mut wanted: BTreeSet<PathBuf> = scanned.iter().cloned().collect();
    wanted.extend(session_repos.into_iter().filter(|path| scan::is_repo(path)));
    wanted.extend(declared.filter(|path| path.is_dir()).cloned());
    wanted.retain(|path| !config.exclude.contains(path));
    wanted
}
