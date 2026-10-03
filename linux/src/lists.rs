//! The session lists, as the TUI shows them, built from the client's machines and what each
//! session's subscription said: grouped by project, each session labelled by its machine, or
//! by machine, a vault's sessions under the host they run on. Children come under their
//! parent; a primary counts its children and how many of them need the user.
//!
//! A session's project is the `project_id` its daemon lists it with; until the daemon has
//! resolved it, the local project of its repo on its machine.

use std::collections::{BTreeMap, HashMap, HashSet};

use herder_client_core::{ConnectionState, Machine, SessionUpdate};
use herder_protocol::{
    EventBody, FleetHost, HostId, ProjectId, PullRequest, SessionHead, SessionId, SessionStatus,
    Timestamp,
};

/// A session of a machine; the key of everything per session.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionKey {
    /// The machine.
    pub host_id: HostId,
    /// The session.
    pub session_id: SessionId,
}

/// What a session's events say that the lists show and its list entry does not.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    /// Whether its first update arrived.
    pub loaded: bool,
    /// Repository path on the host.
    pub repo: String,
    /// Branch the session works on now.
    pub branch: String,
    /// Pull requests linked to the session now, in the order they were linked.
    pub prs: Vec<PullRequest>,
}

impl Summary {
    /// Folds in a subscription update; whether anything the lists show changed.
    pub fn apply(&mut self, update: &SessionUpdate) -> bool {
        let before = self.clone();
        self.loaded = true;
        for event in &update.events {
            match &event.body {
                EventBody::SessionCreated { repo, branch, .. } => {
                    self.repo.clone_from(repo);
                    self.branch.clone_from(branch);
                }
                EventBody::BranchCheckedOut { branch } => self.branch.clone_from(branch),
                EventBody::PrLinked { pr } | EventBody::PrUpdated { pr } => {
                    match self.prs.iter_mut().find(|known| known.number == pr.number) {
                        Some(known) => known.clone_from(pr),
                        None => self.prs.push(pr.clone()),
                    }
                }
                EventBody::PrUnlinked { number } => self.prs.retain(|pr| pr.number != *number),
                _ => {}
            }
        }
        *self != before
    }
}

/// How the session list groups sessions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Grouping {
    /// Project → sessions, each labelled by its machine.
    #[default]
    Projects,
    /// Machine → sessions; a vault's, host → sessions.
    Machines,
}

/// Whose sessions the list shows: what is selected in the sidebar.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    /// Every machine's.
    #[default]
    All,
    /// One machine's.
    Machine(HostId),
    /// The sessions a vault lists on one of its hosts.
    Host {
        /// The vault.
        vault: HostId,
        /// The host.
        host: HostId,
    },
}

/// A heading and the sessions under it.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub title: String,
    pub description: String,
    pub rows: Vec<SessionRow>,
}

/// One session in the list.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionRow {
    pub key: SessionKey,
    /// Nesting under its parent; 0 for a top-level session.
    pub depth: usize,
    pub title: String,
    pub status: SessionStatus,
    /// How many listed children it has.
    pub children: usize,
    /// How many of its children wait on the user; 0 when it has none listed.
    pub need_you: u32,
    /// Its pull requests, open ones first.
    pub prs: Vec<PullRequest>,
    /// Where it runs, while the list groups by project.
    pub place: Option<String>,
    /// For a `moved` copy, the host the session went to.
    pub moved_to: Option<String>,
}

/// Every listed session, for the subscriptions to follow.
pub fn keys(machines: &[Machine]) -> HashSet<SessionKey> {
    machines
        .iter()
        .flat_map(|machine| machine.sessions.iter().map(|head| key(machine, head)))
        .collect()
}

/// A connection's words.
pub fn connection(state: &ConnectionState) -> &str {
    match state {
        ConnectionState::Connected => "connected",
        ConnectionState::Connecting => "connecting",
        ConnectionState::Disconnected { error } => error,
    }
}

/// `count` sessions, in words.
pub fn sessions(count: usize) -> String {
    match count {
        1 => "1 session".to_owned(),
        count => format!("{count} sessions"),
    }
}

/// A vault's host's state: online, or offline with when the vault last heard from it.
pub fn host_state(host: &FleetHost) -> String {
    if host.online {
        return "online".to_owned();
    }
    let secs = Timestamp::now().duration_since(host.last_seen).as_secs();
    let minutes = secs.max(0) / 60;
    let (days, hours, minutes) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    let ago = if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    };
    format!("offline · {ago} ago")
}

fn key(machine: &Machine, head: &SessionHead) -> SessionKey {
    SessionKey {
        host_id: machine.host_id.clone(),
        session_id: head.session_id.clone(),
    }
}

/// The name of a project no machine lists: the last segment of its id.
fn project_id_name(id: &ProjectId) -> &str {
    let id = id.as_str();
    id.rsplit(['/', ':'])
        .find(|segment| !segment.is_empty())
        .unwrap_or(id)
}

/// The lists of `machines`, with what their sessions' subscriptions said.
pub struct Lists<'a> {
    pub machines: &'a [Machine],
    pub summaries: &'a HashMap<SessionKey, Summary>,
    /// For a narrow window: only each branch's last part.
    pub compact: bool,
}

impl Lists<'_> {
    /// The sessions `scope` covers, grouped by `grouping`.
    pub fn groups(&self, scope: &Scope, grouping: Grouping) -> Vec<Group> {
        match grouping {
            Grouping::Projects => self.by_project(scope),
            Grouping::Machines => self.by_machine(scope),
        }
    }

    fn by_project(&self, scope: &Scope) -> Vec<Group> {
        let mut projects: BTreeMap<(bool, String, Option<ProjectId>), Vec<SessionKey>> =
            BTreeMap::new();
        for machine in self.machines {
            for head in &machine.sessions {
                if !in_scope(machine, head, scope) {
                    continue;
                }
                let key = key(machine, head);
                let project = self.project_of(machine, head);
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
        projects
            .into_iter()
            .map(|((_, name, project), mut keys)| {
                let mut on: Vec<&str> = Vec::new();
                for machine in self.machines {
                    if keys.iter().any(|key| key.host_id == machine.host_id) {
                        on.push(&machine.name);
                    }
                }
                let mut description = sessions(keys.len());
                if !self.compact {
                    description = format!("{description} · {}", on.join(", "));
                }
                // Session ids are ULIDs, so they sort oldest first across machines too.
                keys.sort_by(|a, b| (&a.session_id, &a.host_id).cmp(&(&b.session_id, &b.host_id)));
                Group {
                    title: if project.is_some() {
                        name
                    } else {
                        "No project yet".to_owned()
                    },
                    description,
                    rows: self.forest(&keys, Grouping::Projects),
                }
            })
            .collect()
    }

    fn by_machine(&self, scope: &Scope) -> Vec<Group> {
        let mut groups = Vec::new();
        for machine in self.machines {
            let keys = |host: Option<&HostId>| -> Vec<SessionKey> {
                machine
                    .sessions
                    .iter()
                    .filter(|head| host.is_none() || head.host_id.as_ref() == host)
                    .map(|head| key(machine, head))
                    .collect()
            };
            let shown = match scope {
                Scope::All => true,
                Scope::Machine(host_id) | Scope::Host { vault: host_id, .. } => {
                    machine.host_id == *host_id
                }
            };
            if !shown {
                continue;
            }
            if machine.hosts.is_empty() {
                // The daemon lists sessions oldest first.
                let keys = keys(None);
                groups.push(Group {
                    title: machine.name.clone(),
                    description: format!(
                        "{} · {}",
                        connection(&machine.connection),
                        sessions(keys.len())
                    ),
                    rows: self.forest(&keys, Grouping::Machines),
                });
                continue;
            }
            // A vault: each host, then the sessions that run on it.
            for host in &machine.hosts {
                if matches!(scope, Scope::Host { host: only, .. } if *only != host.host_id) {
                    continue;
                }
                let keys = keys(Some(&host.host_id));
                groups.push(Group {
                    title: host.host_name.clone(),
                    description: format!(
                        "{} · {} · on {}",
                        host_state(host),
                        sessions(keys.len()),
                        machine.name
                    ),
                    rows: self.forest(&keys, Grouping::Machines),
                });
            }
        }
        groups
    }

    /// The rows of `keys`, given oldest first: newest first, each followed by its children
    /// among `keys`, oldest first.
    fn forest(&self, keys: &[SessionKey], grouping: Grouping) -> Vec<SessionRow> {
        let listed: HashSet<&SessionKey> = keys.iter().collect();
        let parent = |key: &SessionKey| {
            let parent = SessionKey {
                host_id: key.host_id.clone(),
                session_id: self.head(key)?.1.parent.clone()?,
            };
            (listed.contains(&parent) && parent != *key).then_some(parent)
        };
        let mut children: HashMap<SessionKey, Vec<&SessionKey>> = HashMap::new();
        let mut roots = Vec::new();
        for key in keys {
            match parent(key) {
                Some(parent) => children.entry(parent).or_default().push(key),
                None => roots.push(key),
            }
        }
        let mut rows = Vec::new();
        let mut stack: Vec<(&SessionKey, usize)> = roots.into_iter().map(|key| (key, 0)).collect();
        let mut seen = HashSet::new();
        while let Some((key, depth)) = stack.pop() {
            if !seen.insert(key) {
                continue;
            }
            rows.extend(self.row(key, depth, grouping));
            if let Some(kids) = children.get(key) {
                stack.extend(kids.iter().rev().map(|kid| (*kid, depth + 1)));
            }
        }
        rows
    }

    fn row(&self, key: &SessionKey, depth: usize, grouping: Grouping) -> Option<SessionRow> {
        let (machine, head) = self.head(key)?;
        let summary = self.summaries.get(key);
        let children = machine
            .sessions
            .iter()
            .filter(|child| {
                child.session_id != key.session_id && child.parent.as_ref() == Some(&key.session_id)
            })
            .count();
        Some(SessionRow {
            key: key.clone(),
            depth,
            title: self.title(head, summary, grouping),
            status: head.status,
            children,
            need_you: if children > 0 {
                head.children_need_you
            } else {
                0
            },
            prs: summary
                .map(|s| crate::prs::ordered(&s.prs))
                .unwrap_or_default(),
            place: (grouping == Grouping::Projects).then(|| place(machine, head)),
            moved_to: self.moved_to(key, head),
        })
    }

    /// A child's task; else, under a project's heading that names the repo already, its
    /// branch, and under a machine's, its repo's name and branch.
    fn title(&self, head: &SessionHead, summary: Option<&Summary>, grouping: Grouping) -> String {
        if let Some(task) = &head.task {
            return task.clone();
        }
        let Some(summary) = summary.filter(|s| s.loaded) else {
            return head.session_id.to_string();
        };
        let branch = match summary.branch.rsplit('/').next() {
            Some(last) if self.compact => last,
            _ => &summary.branch,
        };
        let repo = summary.repo.rsplit('/').find(|part| !part.is_empty());
        match (grouping, repo) {
            (Grouping::Machines, Some(repo)) => format!("{repo} · {branch}"),
            _ => branch.to_owned(),
        }
    }

    /// For a `moved` copy, the host or machine whose copy of the session is not moved.
    fn moved_to(&self, key: &SessionKey, head: &SessionHead) -> Option<String> {
        if head.status != SessionStatus::Moved {
            return None;
        }
        self.machines.iter().find_map(|machine| {
            let copy = machine.sessions.iter().find(|other| {
                other.session_id == key.session_id
                    && other.status != SessionStatus::Moved
                    && machine.host_id != key.host_id
            })?;
            Some(match fleet_host(machine, copy) {
                Some(host) => host.host_name.clone(),
                None => machine.name.clone(),
            })
        })
    }

    fn head(&self, key: &SessionKey) -> Option<(&Machine, &SessionHead)> {
        let machine = self.machines.iter().find(|m| m.host_id == key.host_id)?;
        let head = machine
            .sessions
            .iter()
            .find(|head| head.session_id == key.session_id)?;
        Some((machine, head))
    }

    /// As its daemon lists it, else the local project of its repo; `None` until its repo is
    /// known.
    fn project_of(&self, machine: &Machine, head: &SessionHead) -> Option<ProjectId> {
        if let Some(project_id) = &head.project_id {
            return Some(project_id.clone());
        }
        let summary = self.summaries.get(&key(machine, head))?;
        (!summary.repo.is_empty()).then(|| ProjectId::local(&machine.host_id, &summary.repo))
    }

    /// As the first machine that lists it names it, else from its id.
    fn project_name(&self, id: &ProjectId) -> String {
        self.machines
            .iter()
            .flat_map(|machine| &machine.projects)
            .find(|project| project.project_id == *id)
            .map_or_else(|| project_id_name(id).to_owned(), |p| p.name.clone())
    }
}

/// Whether `scope` covers `machine`'s session `head`.
fn in_scope(machine: &Machine, head: &SessionHead, scope: &Scope) -> bool {
    match scope {
        Scope::All => true,
        Scope::Machine(host_id) => machine.host_id == *host_id,
        Scope::Host { vault, host } => {
            machine.host_id == *vault && head.host_id.as_ref() == Some(host)
        }
    }
}

/// The vault host a session runs on.
fn fleet_host<'a>(machine: &'a Machine, head: &SessionHead) -> Option<&'a FleetHost> {
    let host = head.host_id.as_ref()?;
    machine.hosts.iter().find(|h| h.host_id == *host)
}

/// Where a session runs: its machine; for a vault's, the host, marked when offline.
fn place(machine: &Machine, head: &SessionHead) -> String {
    match fleet_host(machine, head) {
        Some(host) if !host.online => format!("{} · offline", host.host_name),
        Some(host) => host.host_name.clone(),
        None => machine.name.clone(),
    }
}

#[cfg(test)]
pub mod tests {
    use herder_protocol::{
        AccountId, CiStatus, Event, Mergeable, PermissionMode, PrState, Project, Provider,
        ReviewStatus, Role,
    };

    use super::*;

    pub fn head(id: &str, project: Option<&str>) -> SessionHead {
        SessionHead {
            session_id: SessionId::new(id),
            host_id: None,
            head_seq: 0,
            status: SessionStatus::Idle,
            parent: None,
            task: None,
            project_id: project.map(ProjectId::new),
            account_id: AccountId::new("claude-main"),
            children_need_you: 0,
        }
    }

    pub fn machine(host: &str, name: &str, sessions: Vec<SessionHead>) -> Machine {
        Machine {
            host_id: HostId::new(host),
            name: name.to_owned(),
            addresses: vec!["127.0.0.1:7447".to_owned()],
            fingerprint: "ab".repeat(32),
            connection: ConnectionState::Connected,
            quality: Default::default(),
            role: Some(Role::Owner),
            sessions,
            hosts: Vec::new(),
            projects: Vec::new(),
            accounts: Vec::new(),
            failover: Default::default(),
            terminals: Vec::new(),
            resources: None,
            session_usage: Default::default(),
        }
    }

    pub fn update(session: &str, bodies: Vec<EventBody>) -> SessionUpdate {
        SessionUpdate {
            events: bodies
                .into_iter()
                .zip(1..)
                .map(|(body, seq)| Event {
                    session_id: SessionId::new(session),
                    seq,
                    at: Timestamp::UNIX_EPOCH,
                    by: None,
                    body,
                })
                .collect(),
            streaming: Vec::new(),
        }
    }

    pub fn created(repo: &str, branch: &str) -> EventBody {
        EventBody::SessionCreated {
            repo: repo.to_owned(),
            worktree: format!("/srv/worktrees/{branch}"),
            branch: branch.to_owned(),
            provider: Provider::Claude,
            account_id: AccountId::new("claude-main"),
            model: "claude-opus".to_owned(),
            permission_mode: PermissionMode::Ask,
            parent: None,
            task: None,
            max_children: None,
            failover_pin: None,
        }
    }

    pub fn pr(number: u64, state: PrState, ci: CiStatus) -> PullRequest {
        PullRequest {
            number,
            url: format!("https://github.com/org/app/pull/{number}"),
            title: format!("PR {number}"),
            head_branch: None,
            state,
            ci,
            review: ReviewStatus::None,
            mergeable: Mergeable::Clean,
        }
    }

    pub fn key(host: &str, session: &str) -> SessionKey {
        SessionKey {
            host_id: HostId::new(host),
            session_id: SessionId::new(session),
        }
    }

    /// A task tree on `box` and a lone session on `nas`, both in `app`; and a vault with two
    /// hosts, one offline.
    pub fn fleet() -> (Vec<Machine>, HashMap<SessionKey, Summary>) {
        let child = |id: &str, task: &str, status| SessionHead {
            parent: Some(SessionId::new("s2")),
            task: Some(task.to_owned()),
            status,
            ..head(id, Some("github.com/org/app"))
        };
        let mut boxed = machine(
            "h1",
            "box",
            vec![
                SessionHead {
                    children_need_you: 1,
                    status: SessionStatus::Running,
                    ..head("s2", Some("github.com/org/app"))
                },
                child("s3", "write the tests", SessionStatus::NeedsYou),
                child("s4", "document it", SessionStatus::Error),
                // Not resolved to a project yet.
                head("s5", None),
            ],
        );
        boxed.projects = vec![Project {
            project_id: ProjectId::new("github.com/org/app"),
            name: "App".to_owned(),
            paths: vec!["/srv/app".to_owned()],
            default_permission_mode: None,
            default_account: None,
            setup_command: None,
            icon: None,
        }];
        let nas = machine("h2", "nas", vec![head("s1", Some("github.com/org/app"))]);
        let mut vault = machine(
            "v",
            "vault",
            vec![
                SessionHead {
                    host_id: Some(HostId::new("devbox")),
                    ..head("s6", Some("github.com/org/web"))
                },
                SessionHead {
                    host_id: Some(HostId::new("laptop")),
                    ..head("s7", Some("github.com/org/web"))
                },
            ],
        );
        vault.hosts = vec![
            FleetHost {
                host_id: HostId::new("devbox"),
                host_name: "devbox".to_owned(),
                online: true,
                last_seen: Timestamp::now(),
            },
            FleetHost {
                host_id: HostId::new("laptop"),
                host_name: "laptop".to_owned(),
                online: false,
                last_seen: Timestamp::now() - std::time::Duration::from_secs(2 * 3600 + 5 * 60),
            },
        ];
        let mut summaries = HashMap::new();
        for (host, id, repo, branch) in [
            ("h1", "s2", "/srv/app", "herder/api"),
            ("h1", "s3", "/srv/app", "herder/api-tests"),
            ("h1", "s4", "/srv/app", "herder/api-docs"),
            ("h1", "s5", "/srv/scratch", "herder/try"),
            ("h2", "s1", "/srv/app", "herder/fix-login"),
            ("v", "s6", "/srv/web", "herder/login"),
            ("v", "s7", "/srv/web", "herder/docs"),
        ] {
            let mut summary = Summary::default();
            summary.apply(&update(id, vec![created(repo, branch)]));
            summaries.insert(key(host, id), summary);
        }
        (vec![boxed, nas, vault], summaries)
    }

    fn titles(groups: &[Group]) -> Vec<(String, Vec<(usize, String)>)> {
        groups
            .iter()
            .map(|group| {
                let rows = group
                    .rows
                    .iter()
                    .map(|row| (row.depth, row.title.clone()))
                    .collect();
                (group.title.clone(), rows)
            })
            .collect()
    }

    fn owned(groups: &[(&str, &[(usize, &str)])]) -> Vec<(String, Vec<(usize, String)>)> {
        groups
            .iter()
            .map(|(title, rows)| {
                let rows = rows.iter().map(|(d, t)| (*d, (*t).to_owned())).collect();
                ((*title).to_owned(), rows)
            })
            .collect()
    }

    #[test]
    fn by_project_groups_every_machines_sessions_under_their_project() {
        let (machines, summaries) = fleet();
        let lists = Lists {
            machines: &machines,
            summaries: &summaries,
            compact: false,
        };
        let groups = lists.groups(&Scope::All, Grouping::Projects);
        assert_eq!(
            titles(&groups),
            owned(&[
                (
                    "App",
                    &[
                        (0, "herder/api"),
                        (1, "write the tests"),
                        (1, "document it"),
                        (0, "herder/fix-login"),
                    ],
                ),
                // Local: named after the repo's directory.
                ("scratch", &[(0, "herder/try")]),
                ("web", &[(0, "herder/docs"), (0, "herder/login")]),
            ])
        );
        assert_eq!(groups[0].description, "4 sessions · box, nas");
        let primary = &groups[0].rows[0];
        assert_eq!((primary.children, primary.need_you), (2, 1));
        assert_eq!(primary.status, SessionStatus::Running);
        assert_eq!(primary.place.as_deref(), Some("box"));
        assert_eq!(groups[0].rows[3].place.as_deref(), Some("nas"));
        assert_eq!(groups[0].rows[1].need_you, 0);
        assert_eq!(groups[2].rows[0].place.as_deref(), Some("laptop · offline"));

        let compact = Lists {
            compact: true,
            ..lists
        };
        let groups = compact.groups(&Scope::Machine(HostId::new("h2")), Grouping::Projects);
        assert_eq!(titles(&groups), owned(&[("App", &[(0, "fix-login")])]));
        assert_eq!(groups[0].description, "1 session");
    }

    #[test]
    fn by_machine_puts_a_vaults_sessions_under_their_host() {
        let (machines, summaries) = fleet();
        let lists = Lists {
            machines: &machines,
            summaries: &summaries,
            compact: false,
        };
        let groups = lists.groups(&Scope::All, Grouping::Machines);
        assert_eq!(
            titles(&groups),
            owned(&[
                (
                    "box",
                    &[
                        (0, "scratch · herder/try"),
                        (0, "app · herder/api"),
                        (1, "write the tests"),
                        (1, "document it"),
                    ],
                ),
                ("nas", &[(0, "app · herder/fix-login")]),
                ("devbox", &[(0, "web · herder/login")]),
                ("laptop", &[(0, "web · herder/docs")]),
            ])
        );
        assert_eq!(groups[0].description, "connected · 4 sessions");
        assert_eq!(groups[2].description, "online · 1 session · on vault");
        assert_eq!(
            groups[3].description,
            "offline · 2h 5m ago · 1 session · on vault"
        );
        assert_eq!(groups[0].rows[0].place, None);

        let host = Scope::Host {
            vault: HostId::new("v"),
            host: HostId::new("laptop"),
        };
        assert_eq!(
            titles(&lists.groups(&host, Grouping::Machines)),
            owned(&[("laptop", &[(0, "web · herder/docs")])])
        );
        assert_eq!(
            titles(&lists.groups(&host, Grouping::Projects)),
            owned(&[("web", &[(0, "herder/docs")])])
        );
    }

    #[test]
    fn a_summary_follows_branches_and_prs() {
        let mut summary = Summary::default();
        assert!(summary.apply(&update("s1", vec![created("/srv/app", "herder/a")])));
        assert!(!summary.apply(&update("s1", Vec::new())));
        assert!(summary.apply(&update(
            "s1",
            vec![
                EventBody::BranchCheckedOut {
                    branch: "herder/b".to_owned()
                },
                EventBody::PrLinked {
                    pr: pr(7, PrState::Merged, CiStatus::Passing)
                },
                EventBody::PrLinked {
                    pr: pr(9, PrState::Open, CiStatus::Pending)
                },
                EventBody::PrUpdated {
                    pr: pr(9, PrState::Open, CiStatus::Failing)
                },
            ]
        )));
        assert_eq!(summary.branch, "herder/b");
        assert_eq!(
            crate::prs::ordered(&summary.prs),
            [
                pr(9, PrState::Open, CiStatus::Failing),
                pr(7, PrState::Merged, CiStatus::Passing)
            ]
        );
        assert!(summary.apply(&update("s1", vec![EventBody::PrUnlinked { number: 7 }])));
        assert_eq!(summary.prs.len(), 1);
    }

    #[test]
    fn an_unloaded_session_shows_its_id_and_a_moved_one_where_it_went() {
        let mut from = machine(
            "h1",
            "box",
            vec![SessionHead {
                status: SessionStatus::Moved,
                ..head("s1", None)
            }],
        );
        from.connection = ConnectionState::Disconnected {
            error: "connection refused".to_owned(),
        };
        let to = machine("h2", "nas", vec![head("s1", None)]);
        let machines = [from, to];
        let summaries = HashMap::new();
        let lists = Lists {
            machines: &machines,
            summaries: &summaries,
            compact: false,
        };
        let groups = lists.groups(&Scope::All, Grouping::Machines);
        assert_eq!(groups[0].description, "connection refused · 1 session");
        assert_eq!(groups[0].rows[0].title, "s1");
        assert_eq!(groups[0].rows[0].moved_to.as_deref(), Some("nas"));
        assert_eq!(groups[1].rows[0].moved_to, None);
        assert_eq!(
            keys(&machines),
            HashSet::from([key("h1", "s1"), key("h2", "s1")])
        );
    }
}
