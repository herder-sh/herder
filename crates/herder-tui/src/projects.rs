//! Projects across machines: the session list grouped Project → sessions, each session
//! labelled by its machine, and the clones a new session of a project can start from.
//!
//! A session's project is the `project_id` its daemon lists it with; clones of one repository
//! on two machines share it, so their sessions land under one project. Until the daemon has
//! resolved it, a session counts as the local project of its repo on its machine, the id the
//! daemon gives a repository without a remote. The client core does not pass on the daemons'
//! project lists, so a project's name is the last segment of its id, and the clones known are
//! the repos of its sessions.

use std::collections::BTreeMap;

use herder_protocol::{HostId, ProjectId};

use crate::app::{App, Row};
use crate::session::SessionKey;

/// How the session list groups sessions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Grouping {
    /// Project → sessions, each labelled by its machine.
    #[default]
    Projects,
    /// Machine → sessions.
    Machines,
}

/// A clone of a project on a machine, to start a session from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectClone {
    /// The machine.
    pub host_id: HostId,
    /// Path of the repository there.
    pub repo: String,
}

/// The display name of a project: the last segment of its id, the repository name of
/// `github.com/org/repo` or the directory of a local `HOST:/home/dev/scratch`.
pub fn name(id: &ProjectId) -> &str {
    let id = id.as_str();
    id.rsplit(['/', ':'])
        .find(|segment| !segment.is_empty())
        .unwrap_or(id)
}

impl App {
    /// Switches the session list between projects and machines, keeping a selected session.
    pub fn toggle_grouping(&mut self) {
        self.grouping = match self.grouping {
            Grouping::Projects => Grouping::Machines,
            Grouping::Machines => Grouping::Projects,
        };
        if self
            .chosen
            .as_ref()
            .is_some_and(|row| row.session().is_none())
        {
            self.chosen = None;
        }
    }

    /// The project of `key`'s session: as its daemon lists it, else the local project of its
    /// repo; `None` until its repo is known.
    pub fn project_of(&self, key: &SessionKey) -> Option<ProjectId> {
        let machine = self.machines.iter().find(|m| m.host_id == key.host_id)?;
        let head = machine
            .sessions
            .iter()
            .find(|head| head.session_id == key.session_id)?;
        if let Some(project_id) = &head.project_id {
            return Some(project_id.clone());
        }
        let session = self.sessions.get(key)?;
        (!session.repo.is_empty()).then(|| ProjectId::local(&key.host_id, &session.repo))
    }

    /// The session list grouped by project, by name; sessions whose project is not known yet
    /// come last. Within a project, the sessions of every machine, newest first.
    pub(crate) fn project_rows(&self, fold: bool) -> Vec<Row> {
        let mut projects: BTreeMap<(bool, String, Option<ProjectId>), Vec<SessionKey>> =
            BTreeMap::new();
        for machine in &self.machines {
            for head in &machine.sessions {
                let key = SessionKey {
                    host_id: machine.host_id.clone(),
                    session_id: head.session_id.clone(),
                };
                let project = self.project_of(&key);
                let order = (
                    project.is_none(),
                    project
                        .as_ref()
                        .map(|p| name(p).to_owned())
                        .unwrap_or_default(),
                    project,
                );
                projects.entry(order).or_default().push(key);
            }
        }
        let mut rows = Vec::new();
        for ((_, _, project), mut keys) in projects {
            rows.push(Row::Project(project));
            // Session ids are ULIDs, so they sort oldest first across machines too.
            keys.sort_by(|a, b| (&a.session_id, &a.host_id).cmp(&(&b.session_id, &b.host_id)));
            rows.extend(self.forest(&keys, fold));
        }
        rows
    }

    /// The project new sessions start from: the selected project, or the selected session's,
    /// while the list groups by project.
    pub fn selected_project(&self) -> Option<ProjectId> {
        if self.grouping != Grouping::Projects {
            return None;
        }
        match self.selected()? {
            Row::Project(project) => project,
            Row::Session { key, .. } => self.project_of(&key),
            Row::Machine(_) => None,
        }
    }

    /// The machines with a clone of `project`, in pairing order, each with the repo of its
    /// newest session there.
    pub fn clones(&self, project: &ProjectId) -> Vec<ProjectClone> {
        let sessions = self.sessions_of(project);
        self.machines
            .iter()
            .filter_map(|machine| {
                let (_, repo) = sessions
                    .iter()
                    .filter(|(key, _)| key.host_id == machine.host_id)
                    .max_by_key(|(key, _)| &key.session_id)?;
                Some(ProjectClone {
                    host_id: machine.host_id.clone(),
                    repo: (*repo).to_owned(),
                })
            })
            .collect()
    }

    /// The clone of `project` used last: the one its newest session started from.
    pub fn last_used_clone(&self, project: &ProjectId) -> Option<ProjectClone> {
        let sessions = self.sessions_of(project);
        let (key, repo) = sessions.iter().max_by_key(|(key, _)| &key.session_id)?;
        Some(ProjectClone {
            host_id: key.host_id.clone(),
            repo: (*repo).to_owned(),
        })
    }

    /// Every listed session of `project` whose repo is known, with that repo.
    fn sessions_of(&self, project: &ProjectId) -> Vec<(&SessionKey, &str)> {
        self.sessions
            .iter()
            .filter(|(key, session)| {
                !session.repo.is_empty() && self.project_of(key).as_ref() == Some(project)
            })
            .map(|(key, session)| (key, session.repo.as_str()))
            .collect()
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod two_daemons;
