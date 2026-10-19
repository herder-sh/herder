//! Projects across machines: the session list grouped Project → sessions, each session
//! labelled by its machine, the chats under a heading of their own, and the clones a new
//! session of a project can start from.
//!
//! The grouping is the Mac app's (`apple/HerderKit/Sources/HerderKit/Lists.swift`). A
//! session's project is the `project_id` its daemon lists it with; clones of one repository on
//! two machines share it, so their sessions land under one project. Until the daemon has
//! resolved it, the session waits under "no project yet". A chat, about no project, is listed
//! under "chats". A project's name and clones are what the daemons list for it: the name an
//! owner gave it on the lowest-id machine that has one, the same whatever order the machines
//! paired in, and every clone on every machine, so a clone with no session yet can start one.

use std::collections::BTreeMap;

use herder_protocol::{HostId, PermissionMode, Project, ProjectId};

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

/// The name of a project no machine lists: the last segment of its id, the repository name
/// of `github.com/org/repo` or the directory of a local `HOST:/home/dev/scratch`.
pub fn name(id: &ProjectId) -> &str {
    let id = id.as_str();
    id.rsplit(['/', ':'])
        .find(|segment| !segment.is_empty())
        .unwrap_or(id)
}

impl App {
    /// The display name of a project: one an owner gave it on the lowest-id machine that has
    /// one, else from its id ([`name`]).
    pub fn project_name(&self, id: &ProjectId) -> String {
        let fallback = name(id);
        let mut listed: Vec<(&HostId, &Project)> = self
            .machines
            .iter()
            .flat_map(|machine| {
                machine
                    .projects
                    .iter()
                    .map(move |project| (&machine.host_id, project))
            })
            .filter(|(_, project)| project.project_id == *id && project.name != fallback)
            .collect();
        listed.sort_by_key(|(host_id, _)| *host_id);
        listed
            .first()
            .map_or(fallback, |(_, project)| project.name.as_str())
            .to_owned()
    }

    /// Index in `host_id`'s accounts of the account `project` starts sessions on there; the
    /// first when it names none.
    pub fn default_account(&self, host_id: &HostId, project: Option<&ProjectId>) -> usize {
        let Some(machine) = self.machines.iter().find(|m| m.host_id == *host_id) else {
            return 0;
        };
        machine
            .projects
            .iter()
            .find(|p| Some(&p.project_id) == project)
            .and_then(|p| p.default_account.as_ref())
            .and_then(|id| machine.accounts.iter().position(|a| a.account_id == *id))
            .unwrap_or(0)
    }

    /// The permission mode `project` starts sessions in on `host_id`; `ask` when it names
    /// none.
    pub fn default_mode(&self, host_id: &HostId, project: Option<&ProjectId>) -> PermissionMode {
        self.machines
            .iter()
            .find(|m| m.host_id == *host_id)
            .and_then(|machine| {
                machine
                    .projects
                    .iter()
                    .find(|p| Some(&p.project_id) == project)
            })
            .and_then(|p| p.default_permission_mode)
            .unwrap_or(PermissionMode::Ask)
    }

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

    /// The project of `key`'s session, as its daemon lists it; `None` until the daemon has
    /// resolved it, and for a chat.
    pub fn project_of(&self, key: &SessionKey) -> Option<ProjectId> {
        self.head(key)?.project_id.clone()
    }

    /// Whether `key`'s session is a chat, about no project.
    pub(crate) fn is_chat(&self, key: &SessionKey) -> bool {
        self.head(key).is_some_and(|head| head.chat)
    }

    /// The session list grouped as the Mac app groups it: the chats first, then each project
    /// by name, the sessions whose project is not known yet last. Within each, the sessions of
    /// every machine, newest first. An archived session with no project is left out, as one of
    /// a repository gone from its machine.
    pub(crate) fn project_rows(&self, fold: bool) -> Vec<Row> {
        let mut chats = Vec::new();
        let mut projects: BTreeMap<(bool, String, Option<ProjectId>), Vec<SessionKey>> =
            BTreeMap::new();
        let hidden = self.hidden();
        for machine in &self.machines {
            for head in &machine.sessions {
                let key = SessionKey {
                    host_id: machine.host_id.clone(),
                    session_id: head.session_id.clone(),
                };
                if self.shadowed(&key) || hidden.contains(&key) {
                    continue;
                }
                if head.chat {
                    chats.push(key);
                    continue;
                }
                let project = head.project_id.clone();
                if project.is_none() && self.archived(&key) {
                    continue;
                }
                let order = (
                    project.is_none(),
                    project
                        .as_ref()
                        .map(|p| self.project_name(p))
                        .unwrap_or_default(),
                    project,
                );
                projects.entry(order).or_default().push(key);
            }
        }
        // Session ids are ULIDs, so they sort oldest first across machines too.
        let oldest_first = |a: &SessionKey, b: &SessionKey| {
            (&a.session_id, &a.host_id).cmp(&(&b.session_id, &b.host_id))
        };
        let mut rows = Vec::new();
        if !chats.is_empty() {
            rows.push(Row::Chats);
            chats.sort_by(oldest_first);
            rows.extend(self.forest(&chats, fold));
        }
        for ((_, _, project), mut keys) in projects {
            rows.push(Row::Project(project));
            keys.sort_by(oldest_first);
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
            Row::Machine(_) | Row::Host { .. } | Row::Chats => None,
        }
    }

    /// Every clone of `project`, by machine in pairing order: on each, the repo of its newest
    /// session there first, then the other clones its daemon lists.
    pub fn clones(&self, project: &ProjectId) -> Vec<ProjectClone> {
        let sessions = self.sessions_of(project);
        let mut clones = Vec::new();
        for machine in &self.machines {
            let newest = sessions
                .iter()
                .filter(|(key, _)| key.host_id == machine.host_id)
                .max_by_key(|(key, _)| &key.session_id)
                .map(|(_, repo)| *repo);
            let listed = machine
                .projects
                .iter()
                .filter(|p| p.project_id == *project)
                .flat_map(|p| p.paths.iter().map(String::as_str));
            let mut repos: Vec<&str> = newest.into_iter().collect();
            for repo in listed {
                if !repos.contains(&repo) {
                    repos.push(repo);
                }
            }
            clones.extend(repos.into_iter().map(|repo| ProjectClone {
                host_id: machine.host_id.clone(),
                repo: repo.to_owned(),
            }));
        }
        clones
    }

    /// The clone of `project` used last: the one its newest session started from; else its
    /// first clone.
    pub fn last_used_clone(&self, project: &ProjectId) -> Option<ProjectClone> {
        let sessions = self.sessions_of(project);
        match sessions.iter().max_by_key(|(key, _)| &key.session_id) {
            Some((key, repo)) => Some(ProjectClone {
                host_id: key.host_id.clone(),
                repo: (*repo).to_owned(),
            }),
            None => self.clones(project).into_iter().next(),
        }
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
