//! What the window lists, built from plain [`Machine`] values and each session as its
//! subscription built it ([`Session`]), as the Mac app's `Lists` builds them: the Board, the
//! chats and each project's tasks, by day of last activity, each task tree's children under
//! it and the archived ones last; the requests waiting on the user; and what the sidebar
//! counts.

use std::collections::{HashMap, HashSet};

use herder_client_core::{ConnectionState, Machine};
use herder_protocol::{
    ApprovalId, HostId, ItemBody, ProjectId, QuestionId, SessionHead, SessionId, SessionStatus,
    Timestamp,
};

use crate::session::{Entry, Session, first_line};

/// A session on a machine.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SessionKey {
    pub host_id: HostId,
    pub session_id: SessionId,
}

/// What the list pane shows: a section of the sidebar.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Section {
    /// Every task across all machines.
    #[default]
    Board,
    /// The sessions about no project.
    Chats,
    /// One project's tasks, on every machine that has it.
    Project(ProjectId),
    /// The machines, with their state.
    Machines,
}

/// A project as the sidebar lists it, merged across machines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectEntry {
    pub project_id: ProjectId,
    pub name: String,
    /// Tasks waiting for the user, and running.
    pub waiting: usize,
    pub running: usize,
}

/// A request waiting on the user: an approval or a question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub key: SessionKey,
    /// The session's title, and the machine it runs on.
    pub title: String,
    pub machine: String,
    pub kind: RequestKind,
    pub since: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestKind {
    Approval {
        id: ApprovalId,
        summary: String,
    },
    Question {
        id: QuestionId,
        text: String,
        choices: Vec<String>,
        multi_select: bool,
    },
}

/// A task's row: a session and the agents of its task tree under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub key: SessionKey,
    pub title: String,
    pub status: SessionStatus,
    /// Whether a request of its waits on the user.
    pub waiting: bool,
    /// What it is doing, or last did.
    pub activity: String,
    pub project: Option<String>,
    pub branch: String,
    pub machine: String,
    /// Its last event, for its age and its day.
    pub at: Option<Timestamp>,
    pub children: Vec<Task>,
}

impl Task {
    /// How the task tree stands, for the agents line: whether one of them waits, runs.
    pub fn children_state(&self) -> Option<&'static str> {
        if self.children.is_empty() {
            return None;
        }
        Some(if self.children.iter().any(|child| child.waiting) {
            "Needs you"
        } else if self
            .children
            .iter()
            .any(|child| child.status == SessionStatus::Running)
        {
            "Running"
        } else {
            "Done"
        })
    }
}

/// The list pane's content.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct View {
    pub requests: Vec<Request>,
    /// Sections by day, newest first: a heading and its tasks.
    pub days: Vec<(String, Vec<Task>)>,
    pub archived: Vec<Task>,
}

/// The listed sessions, without the copies a hub keeps of a machine this device is paired
/// with too: following both would show every such session twice.
pub fn keys(machines: &[Machine]) -> HashSet<SessionKey> {
    let direct: HashSet<(&HostId, &SessionId)> = machines
        .iter()
        .filter(|machine| machine.hosts.is_empty())
        .flat_map(|machine| {
            machine
                .sessions
                .iter()
                .map(move |head| (&machine.host_id, &head.session_id))
        })
        .collect();
    machines
        .iter()
        .flat_map(|machine| {
            machine.sessions.iter().filter_map(|head| {
                let shadowed = head
                    .host_id
                    .as_ref()
                    .is_some_and(|host| direct.contains(&(host, &head.session_id)));
                (!shadowed).then(|| key(machine, head))
            })
        })
        .collect()
}

fn key(machine: &Machine, head: &SessionHead) -> SessionKey {
    SessionKey {
        host_id: machine.host_id.clone(),
        session_id: head.session_id.clone(),
    }
}

/// A connection's state in words.
pub fn connection(state: &ConnectionState) -> &str {
    match state {
        ConnectionState::Connected => "Connected",
        ConnectionState::Connecting => "Connecting…",
        ConnectionState::Disconnected { .. } => "Offline",
    }
}

/// The machines' state in a few words, as the Board's header says it.
pub fn fleet_status(machines: &[Machine]) -> String {
    let connected = machines
        .iter()
        .filter(|machine| machine.connection == ConnectionState::Connected)
        .count();
    match machines.len() {
        0 => "No machines yet".to_owned(),
        1 if connected == 1 => "Connected".to_owned(),
        total if connected == total => format!("All {total} machines connected"),
        total => format!("{connected} of {total} machines connected"),
    }
}

/// How long ago `at` was, as a row says it: `now`, `4m`, `3h`, `2d`, `5w`.
pub fn age(at: Timestamp, now: Timestamp) -> String {
    let seconds = now.as_second().saturating_sub(at.as_second()).max(0);
    match seconds {
        0..60 => "now".to_owned(),
        60..3600 => format!("{}m", seconds / 60),
        3600..86_400 => format!("{}h", seconds / 3600),
        86_400..604_800 => format!("{}d", seconds / 86_400),
        _ => format!("{}w", seconds / 604_800),
    }
}

/// The lists over the machines and what each session's events built.
pub struct Lists<'a> {
    pub machines: &'a [Machine],
    pub sessions: &'a HashMap<SessionKey, Session>,
}

impl Lists<'_> {
    /// Every project with a clone on a machine, merged by id, with its live counts; by name.
    pub fn projects(&self) -> Vec<ProjectEntry> {
        let mut projects: Vec<ProjectEntry> = Vec::new();
        for machine in self.machines {
            for project in &machine.projects {
                if !projects.iter().any(|p| p.project_id == project.project_id) {
                    projects.push(ProjectEntry {
                        project_id: project.project_id.clone(),
                        name: project.name.clone(),
                        waiting: 0,
                        running: 0,
                    });
                }
            }
        }
        for (machine, head) in self.heads() {
            let Some(project) = self.project_of(machine, head) else {
                continue;
            };
            let Some(entry) = projects.iter_mut().find(|p| p.project_id == project) else {
                continue;
            };
            if self.waiting(&key(machine, head), head) {
                entry.waiting += 1;
            } else if head.status == SessionStatus::Running {
                entry.running += 1;
            }
        }
        projects.sort_by_key(|project| project.name.to_lowercase());
        projects
    }

    /// The requests waiting on the user in `section`, oldest first.
    pub fn requests(&self, section: &Section) -> Vec<Request> {
        let mut requests = Vec::new();
        for (machine, head) in self.heads() {
            if !self.in_section(machine, head, section) {
                continue;
            }
            let key = key(machine, head);
            let Some(session) = self.sessions.get(&key) else {
                continue;
            };
            let title = self.title(head, Some(session));
            let machine = self.place(machine, head);
            for approval in session.approvals.iter().filter(|a| a.to_user()) {
                requests.push(Request {
                    key: key.clone(),
                    title: title.clone(),
                    machine: machine.clone(),
                    kind: RequestKind::Approval {
                        id: approval.id.clone(),
                        summary: approval.summary.clone(),
                    },
                    since: approval.since,
                });
            }
            for question in session.questions.iter().filter(|q| q.to_user()) {
                requests.push(Request {
                    key: key.clone(),
                    title: title.clone(),
                    machine: machine.clone(),
                    kind: RequestKind::Question {
                        id: question.id.clone(),
                        text: question.text.clone(),
                        choices: question.choices.clone(),
                        multi_select: question.multi_select,
                    },
                    since: question.since,
                });
            }
        }
        requests.sort_by_key(|request| request.since);
        requests
    }

    /// The tasks of `section` whose title, branch, project or machine has `query`, by day of
    /// last activity in the local time zone, and the archived ones apart.
    pub fn view(&self, section: &Section, query: &str, day: impl Fn(Timestamp) -> String) -> View {
        let query = query.trim().to_lowercase();
        let listed = keys(self.machines);
        let mut tasks: Vec<Task> = Vec::new();
        for (machine, head) in self.heads() {
            let key = key(machine, head);
            if !listed.contains(&key) || !self.in_section(machine, head, section) {
                continue;
            }
            // A child shows under its parent when its parent is listed.
            if self.parent(machine, head).is_some() {
                continue;
            }
            tasks.push(self.task(machine, head));
        }
        tasks.retain(|task| query.is_empty() || matches(task, &query));
        tasks.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.title.cmp(&b.title)));
        let (archived, live): (Vec<Task>, Vec<Task>) = tasks
            .into_iter()
            .partition(|task| task.status == SessionStatus::Archived);
        let mut days: Vec<(String, Vec<Task>)> = Vec::new();
        for task in live {
            let heading = task.at.map(&day).unwrap_or_else(|| "Earlier".to_owned());
            match days.last_mut() {
                Some((last, tasks)) if *last == heading => tasks.push(task),
                _ => days.push((heading, vec![task])),
            }
        }
        let mut requests = self.requests(section);
        requests.retain(|request| {
            query.is_empty()
                || [&request.title, &request.machine]
                    .iter()
                    .any(|text| text.to_lowercase().contains(&query))
        });
        View {
            requests,
            days,
            archived,
        }
    }

    /// How many requests wait on the user, and how many tasks run, across every machine.
    pub fn counts(&self, section: &Section) -> (usize, usize) {
        let mut running = 0;
        for (machine, head) in self.heads() {
            if self.in_section(machine, head, section) && head.status == SessionStatus::Running {
                running += 1;
            }
        }
        (self.requests(section).len(), running)
    }

    /// Every listed session head, with its machine.
    fn heads(&self) -> impl Iterator<Item = (&Machine, &SessionHead)> {
        let listed = keys(self.machines);
        self.machines.iter().flat_map(move |machine| {
            let listed = listed.clone();
            machine
                .sessions
                .iter()
                .filter(move |head| listed.contains(&key(machine, head)))
                .map(move |head| (machine, head))
        })
    }

    fn task(&self, machine: &Machine, head: &SessionHead) -> Task {
        let key = key(machine, head);
        let session = self.sessions.get(&key);
        let mut children: Vec<Task> = machine
            .sessions
            .iter()
            .filter(|child| {
                child.parent.as_ref() == Some(&head.session_id)
                    && child
                        .parent_host
                        .as_ref()
                        .is_none_or(|host| *host == machine.host_id)
            })
            .map(|child| self.task(machine, child))
            .collect();
        children.sort_by(|a, b| a.key.session_id.cmp(&b.key.session_id));
        let project = self
            .project_of(machine, head)
            .map(|id| self.project_name(&id));
        let waiting = self.waiting(&key, head);
        Task {
            title: self.title(head, session),
            status: head.status,
            waiting,
            activity: session.map(activity).unwrap_or_default(),
            project,
            branch: session.map(|s| s.branch.clone()).unwrap_or_default(),
            machine: self.place(machine, head),
            at: session.and_then(|session| session.last_at),
            children,
            key,
        }
    }

    /// The listed parent of a child session, on the same machine.
    fn parent(&self, machine: &Machine, head: &SessionHead) -> Option<SessionId> {
        let parent = head.parent.as_ref()?;
        if head
            .parent_host
            .as_ref()
            .is_some_and(|host| *host != machine.host_id)
        {
            return None;
        }
        machine
            .sessions
            .iter()
            .any(|other| other.session_id == *parent)
            .then(|| parent.clone())
    }

    fn waiting(&self, key: &SessionKey, head: &SessionHead) -> bool {
        head.status == SessionStatus::NeedsYou
            || self.sessions.get(key).is_some_and(|session| {
                session.approvals.iter().any(|a| a.to_user())
                    || session.questions.iter().any(|q| q.to_user())
            })
    }

    fn in_section(&self, machine: &Machine, head: &SessionHead, section: &Section) -> bool {
        match section {
            Section::Board => !head.chat,
            Section::Chats => head.chat,
            Section::Project(id) => self.project_of(machine, head).as_ref() == Some(id),
            Section::Machines => false,
        }
    }

    /// A session's title: its own, else its task's, else its id's tail.
    fn title(&self, head: &SessionHead, session: Option<&Session>) -> String {
        head.title
            .clone()
            .or_else(|| head.task.clone())
            .or_else(|| session.and_then(|s| s.task.clone()))
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| {
                let id = head.session_id.as_str();
                format!("Session {}", &id[id.len().saturating_sub(6)..])
            })
    }

    /// The machine a session runs on: a hub's sessions run on one of its hosts.
    fn place(&self, machine: &Machine, head: &SessionHead) -> String {
        head.host_id
            .as_ref()
            .and_then(|host| machine.hosts.iter().find(|h| h.host_id == *host))
            .map_or_else(|| machine.name.clone(), |host| host.host_name.clone())
    }

    fn project_of(&self, machine: &Machine, head: &SessionHead) -> Option<ProjectId> {
        if let Some(id) = &head.project_id {
            return Some(id.clone());
        }
        // A child works in its parent's project.
        let parent = head.parent.as_ref()?;
        let parent = machine.sessions.iter().find(|h| h.session_id == *parent)?;
        parent.project_id.clone()
    }

    fn project_name(&self, id: &ProjectId) -> String {
        self.machines
            .iter()
            .flat_map(|machine| &machine.projects)
            .find(|project| project.project_id == *id)
            .map_or_else(|| id.to_string(), |project| project.name.clone())
    }
}

fn matches(task: &Task, query: &str) -> bool {
    [
        task.title.as_str(),
        task.branch.as_str(),
        task.machine.as_str(),
        task.project.as_deref().unwrap_or_default(),
    ]
    .iter()
    .any(|text| text.to_lowercase().contains(query))
        || task.children.iter().any(|child| matches(child, query))
}

/// What a session is doing, or last did, in a line: the request waiting, else its latest
/// item: a tool it called, what it said, or the prompt it was given.
pub fn activity(session: &Session) -> String {
    if let Some(approval) = session.approvals.first() {
        return first_line(&approval.summary).to_owned();
    }
    if let Some(question) = session.questions.first() {
        return first_line(&question.text).to_owned();
    }
    let streaming = session.streaming.iter().rev().map(|item| &item.body);
    let done = session
        .entries
        .iter()
        .rev()
        .filter_map(|entry| match entry {
            Entry::Item(item) => Some(&item.body),
            _ => None,
        });
    for body in streaming.chain(done) {
        let line = match body {
            ItemBody::AssistantMessage { text } | ItemBody::UserMessage { text, .. } => {
                first_line(text.trim()).to_owned()
            }
            ItemBody::ToolCall { name, input } => {
                let (label, target) = crate::tools::summary(name, input, None, &session.worktree);
                match (label.is_empty(), target.is_empty()) {
                    (true, true) => name.clone(),
                    (true, false) => target,
                    (false, true) => label,
                    (false, false) => format!("{label} {target}"),
                }
            }
            ItemBody::Reasoning { .. } | ItemBody::ToolResult { .. } | ItemBody::Unknown => {
                continue;
            }
        };
        if !line.is_empty() {
            return line;
        }
    }
    String::new()
}

#[cfg(test)]
pub mod tests {
    use herder_protocol::{
        AccountId, CiStatus, Event, EventBody, Mergeable, PermissionMode, PrState, Project,
        Provider, PullRequest, ReviewStatus, Role, Route,
    };

    use super::*;
    use herder_client_core::SessionUpdate;

    pub fn head(id: &str, project: Option<&str>) -> SessionHead {
        SessionHead {
            session_id: SessionId::new(id),
            host_id: None,
            head_seq: 0,
            status: SessionStatus::Idle,
            parent: None,
            parent_host: None,
            task: None,
            title: Some(format!("Title of {id}")),
            project_id: project.map(ProjectId::new),
            chat: false,
            account_id: AccountId::new("claude-main"),
            children_need_you: 0,
            queue: Vec::new(),
            threads: Vec::new(),
            origin: None,
        }
    }

    pub fn project(id: &str, name: &str) -> Project {
        serde_json::from_value(serde_json::json!({
            "project_id": id,
            "name": name,
            "paths": [format!("/home/dev/src/{name}")],
        }))
        .expect("a project")
    }

    pub fn machine(host: &str, name: &str, sessions: Vec<SessionHead>) -> Machine {
        Machine {
            host_id: HostId::new(host),
            name: name.to_owned(),
            addresses: vec!["127.0.0.1:7447".to_owned()],
            address: None,
            fingerprint: "ab".repeat(32),
            connection: ConnectionState::Connected,
            quality: Default::default(),
            role: Some(Role::Owner),
            user_id: None,
            person_id: None,
            herder_version: None,
            sessions,
            hosts: Vec::new(),
            projects: Vec::new(),
            accounts: Vec::new(),
            failover: Default::default(),
            providers: Vec::new(),
            terminals: Vec::new(),
            resources: None,
            session_usage: Default::default(),
            hub: None,
            skills: None,
            session_skills: Default::default(),
            update: None,
            approvals: Vec::new(),
            address_headers: Default::default(),
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
                    at: Timestamp::from_second(1_000 + seq.cast_signed()).expect("a time"),
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
            branch: Some(branch.to_owned()),
            provider: Provider::Claude,
            account_id: AccountId::new("claude-main"),
            model: "claude-opus".to_owned(),
            permission_mode: PermissionMode::Ask,
            parent: None,
            parent_host: None,
            task: None,
            failover_pin: None,
            chat: false,
            origin: None,
        }
    }

    pub fn pr(number: u64, state: PrState, ci: CiStatus) -> PullRequest {
        PullRequest {
            number,
            url: format!("https://github.com/org/app/pull/{number}"),
            title: format!("PR {number}"),
            head_branch: None,
            head_sha: None,
            unresolved_threads: None,
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

    pub fn approval(id: &str, summary: &str) -> EventBody {
        EventBody::ApprovalRequested {
            approval_id: ApprovalId::new(id),
            turn_id: herder_protocol::TurnId::new("t1"),
            tool_call_id: herder_protocol::ItemId::new("c1"),
            summary: summary.to_owned(),
            routed_to: Route::User,
            reason: None,
            secret: None,
            secret_request: None,
        }
    }

    /// `box` with a task tree in `api` and a chat; `nas` with an archived `api` session; and
    /// a hub that keeps a copy of `box`'s sessions.
    pub fn fleet() -> (Vec<Machine>, HashMap<SessionKey, Session>) {
        let child = SessionHead {
            parent: Some(SessionId::new("s1")),
            task: Some("write the tests".to_owned()),
            title: None,
            status: SessionStatus::Running,
            ..head("s2", Some("p-api"))
        };
        let chat = SessionHead {
            chat: true,
            ..head("s3", None)
        };
        let mut boxed = machine("h1", "box", vec![head("s1", Some("p-api")), child, chat]);
        boxed.projects = vec![project("p-api", "api")];
        let archived = SessionHead {
            status: SessionStatus::Archived,
            ..head("s4", Some("p-api"))
        };
        let mut nas = machine("h2", "nas", vec![archived]);
        nas.projects = vec![project("p-api", "api"), project("p-web", "web")];
        let mut hub = machine(
            "v1",
            "hub",
            vec![SessionHead {
                host_id: Some(HostId::new("h1")),
                ..head("s1", Some("p-api"))
            }],
        );
        hub.hosts = vec![
            serde_json::from_value(serde_json::json!({
                "host_id": "h1", "host_name": "box", "online": true,
                "last_seen": "2026-01-01T00:00:00Z",
            }))
            .expect("a host"),
        ];
        let mut sessions = HashMap::new();
        let mut s1 = Session::default();
        s1.apply(&update(
            "s1",
            vec![
                created("/src/api", "herder/api"),
                approval("a1", "Run cargo test"),
            ],
        ));
        sessions.insert(key("h1", "s1"), s1);
        let mut s4 = Session::default();
        s4.apply(&update("s4", vec![created("/src/api", "herder/old")]));
        sessions.insert(key("h2", "s4"), s4);
        (vec![boxed, nas, hub], sessions)
    }

    fn titles(view: &View) -> Vec<String> {
        view.days
            .iter()
            .flat_map(|(_, tasks)| tasks.iter().map(|task| task.title.clone()))
            .collect()
    }

    #[test]
    fn a_hubs_copy_of_a_paired_machines_session_is_not_followed() {
        let (machines, _) = fleet();
        let listed = keys(&machines);
        assert!(listed.contains(&key("h1", "s1")));
        assert!(!listed.contains(&key("v1", "s1")));
        assert_eq!(listed.len(), 4);
    }

    #[test]
    fn the_board_lists_task_trees_with_their_agents_and_archived_last() {
        let (machines, sessions) = fleet();
        let lists = Lists {
            machines: &machines,
            sessions: &sessions,
        };
        let view = lists.view(&Section::Board, "", |_| "Today".to_owned());
        assert_eq!(titles(&view), ["Title of s1"]);
        let task = &view.days[0].1[0];
        assert_eq!(task.children.len(), 1);
        assert_eq!(task.children[0].title, "write the tests");
        assert_eq!(task.children_state(), Some("Running"));
        assert_eq!(task.branch, "herder/api");
        assert_eq!(task.project.as_deref(), Some("api"));
        assert!(task.waiting);
        assert_eq!(task.activity, "Run cargo test");
        assert_eq!(view.archived.len(), 1);
        assert_eq!(view.archived[0].machine, "nas");
    }

    #[test]
    fn chats_and_projects_list_their_own_sessions() {
        let (machines, sessions) = fleet();
        let lists = Lists {
            machines: &machines,
            sessions: &sessions,
        };
        let chats = lists.view(&Section::Chats, "", |_| "Today".to_owned());
        assert_eq!(titles(&chats), ["Title of s3"]);
        let web = lists.view(&Section::Project(ProjectId::new("p-web")), "", |_| {
            "Today".to_owned()
        });
        assert!(web.days.is_empty() && web.archived.is_empty());
        let projects = lists.projects();
        let names: Vec<_> = projects.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["api", "web"]);
        assert_eq!((projects[0].waiting, projects[0].running), (1, 1));
    }

    #[test]
    fn requests_wait_on_the_user_and_the_search_filters_tasks() {
        let (machines, sessions) = fleet();
        let lists = Lists {
            machines: &machines,
            sessions: &sessions,
        };
        let requests = lists.requests(&Section::Board);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].machine, "box");
        assert!(matches!(
            &requests[0].kind,
            RequestKind::Approval { summary, .. } if summary == "Run cargo test"
        ));
        assert_eq!(lists.counts(&Section::Board), (1, 1));
        // A child's title finds its tree.
        let found = lists.view(&Section::Board, "TESTS", |_| "Today".to_owned());
        assert_eq!(titles(&found), ["Title of s1"]);
        let none = lists.view(&Section::Board, "nothing", |_| "Today".to_owned());
        assert!(titles(&none).is_empty());
        assert!(none.requests.is_empty());
    }

    #[test]
    fn ages_and_the_fleet_status_read_short() {
        let at = |s| Timestamp::from_second(s).expect("a time");
        assert_eq!(age(at(0), at(30)), "now");
        assert_eq!(age(at(0), at(125)), "2m");
        assert_eq!(age(at(0), at(7200)), "2h");
        assert_eq!(age(at(0), at(3 * 86_400)), "3d");
        let (mut machines, _) = fleet();
        assert_eq!(fleet_status(&machines), "All 3 machines connected");
        machines[1].connection = ConnectionState::Disconnected {
            error: "refused".to_owned(),
        };
        assert_eq!(fleet_status(&machines), "2 of 3 machines connected");
        assert_eq!(fleet_status(&[]), "No machines yet");
    }

    #[test]
    fn activity_names_the_latest_tool_or_reply() {
        let mut session = Session::default();
        session.apply(&update(
            "s1",
            vec![
                created("/src/api", "herder/api"),
                crate::session::tests::item(
                    "c1",
                    ItemBody::ToolCall {
                        name: "Read".to_owned(),
                        input: serde_json::json!({"file_path": "/srv/worktrees/herder/api/src/lib.rs"}),
                    },
                ),
            ],
        ));
        assert_eq!(activity(&session), "Read src/lib.rs");
        session.apply(&update(
            "s1",
            vec![crate::session::tests::item(
                "m1",
                ItemBody::AssistantMessage {
                    text: "Done. The tests pass.\nMore.".to_owned(),
                },
            )],
        ));
        assert_eq!(activity(&session), "Done. The tests pass.");
        let _ = pr(1, PrState::Open, CiStatus::Passing);
    }
}
