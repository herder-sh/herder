//! What the TUI knows of one session, folded from its subscription's updates: the facts the
//! session list shows and the transcript the session view renders.

use std::collections::HashMap;

use herder_client_core::SessionUpdate;
use herder_protocol::{
    AccountId, Answer, Answerer, ApprovalId, ApprovalOutcome, ErrorClass, EscalationReason, Event,
    EventBody, HostId, Item, ItemBody, ItemId, PermissionMode, Provider, PullRequest, QuestionId,
    Route, SessionId, SessionStatus, Timestamp, TurnId,
};

/// A session of a machine; the key of everything per session.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SessionKey {
    /// The machine.
    pub host_id: HostId,
    /// The session.
    pub session_id: SessionId,
}

/// One session as built from its events.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    /// The session.
    pub id: SessionId,
    /// Whether its first update arrived; until then only the id is known.
    pub loaded: bool,
    /// Repository path on the host.
    pub repo: String,
    /// The session's worktree on the host.
    pub worktree: String,
    /// Branch the session works on now.
    pub branch: String,
    /// Current model, in the provider's naming.
    pub model: String,
    /// Primary session of its task; `None` for a top-level session.
    pub parent: Option<SessionId>,
    /// Task label, for a child session.
    pub task: Option<String>,
    /// Latest status.
    pub status: SessionStatus,
    /// Completed transcript entries, oldest first.
    pub entries: Vec<Entry>,
    /// Items streaming now, with their text so far.
    pub streaming: Vec<Item>,
    /// Account it runs on.
    pub account_id: Option<AccountId>,
    /// Provider of that account.
    pub provider: Option<Provider>,
    /// Current permission mode.
    pub permission_mode: PermissionMode,
    /// The turn running now, from its start to its end.
    pub turn: Option<TurnId>,
    /// When the running turn started.
    pub turn_started: Option<Timestamp>,
    /// When each item joined the transcript, and each turn started: what durations are
    /// counted from.
    pub times: HashMap<ItemId, Timestamp>,
    /// Tool calls an approval was asked for, by their item, and how it stands.
    pub tool_approvals: HashMap<ItemId, ToolApproval>,
    /// Approval requests nobody answered yet, oldest first.
    pub approvals: Vec<PendingApproval>,
    /// Questions nobody answered yet, oldest first.
    pub questions: Vec<PendingQuestion>,
    /// Prompts this TUI sent while a turn ran, not yet started; the daemon queues them.
    pub queued: Vec<String>,
    /// Pull requests linked to the session now, in the order they were linked.
    pub prs: Vec<PullRequest>,
}

/// An approval request waiting for an answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingApproval {
    /// The request.
    pub id: ApprovalId,
    /// The tool call it asks about.
    pub tool_call_id: ItemId,
    /// What the agent wants to do.
    pub summary: String,
    /// Who is asked first; a user can always answer.
    pub routed_to: Route,
    /// Why it went to the user rather than the primary session, for a child's request.
    pub reason: Option<EscalationReason>,
    /// What the primary session told the user when it escalated the request.
    pub note: Option<String>,
    /// When it was put to whoever it waits on now: asked, or escalated.
    pub since: Timestamp,
}

/// A question waiting for an answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingQuestion {
    /// The question.
    pub id: QuestionId,
    /// Turn it blocks; the question is cleared when that turn ends.
    pub turn_id: TurnId,
    /// The question, as Markdown.
    pub text: String,
    /// Answers to pick from; empty for free text.
    pub choices: Vec<String>,
    /// Who is asked first; a user can always answer.
    pub routed_to: Route,
    /// Why it went to the user rather than the primary session, for a child's question.
    pub reason: Option<EscalationReason>,
    /// What the primary session told the user when it escalated the question.
    pub note: Option<String>,
    /// When it was put to whoever it waits on now: asked, or escalated.
    pub since: Timestamp,
}

/// Where an approval for a tool call stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolApproval {
    /// Nobody answered yet.
    Pending,
    /// Allowed: the tool ran.
    Allowed,
    /// Denied, or expired: the tool did not run.
    Denied,
}

/// One completed entry of the transcript.
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    /// A transcript item, complete.
    Item(Item),
    /// Something the session went through that is not an item, as one line.
    Notice {
        /// The line.
        text: String,
        /// How loudly to show it.
        tone: Tone,
    },
    /// A turn ended without failing: the footer of the agent's reply.
    TurnEnded {
        /// Seconds from its start, when the start is known.
        took: Option<i64>,
        /// Whether it was interrupted.
        interrupted: bool,
    },
    /// A turn failed.
    TurnFailed {
        /// Which kind of failure.
        class: ErrorClass,
        /// What failed.
        message: String,
    },
    /// The session moved to another model, account, provider or permission mode.
    Switch(String),
    /// An approval or question was answered, or expired.
    Resolved {
        /// An approval, rather than a question.
        approval: bool,
        /// What was decided, and by whom.
        text: String,
    },
    /// The session spawned a child for a task.
    Child {
        /// The child.
        session_id: SessionId,
        /// Its task.
        task: String,
    },
    /// A child finished a turn and reported back.
    Report {
        /// The child.
        session_id: SessionId,
        /// What it reported.
        summary: String,
    },
    /// A pull request was linked; shown as it stands now, from [`Session::prs`].
    Pr(u64),
}

/// How a notice is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Ordinary progress.
    Info,
    /// Waiting on the user.
    Attention,
}

impl Session {
    /// A session known only by id, until its first update.
    pub fn new(id: SessionId) -> Self {
        Self {
            id,
            loaded: false,
            repo: String::new(),
            worktree: String::new(),
            branch: String::new(),
            model: String::new(),
            parent: None,
            task: None,
            status: SessionStatus::Idle,
            entries: Vec::new(),
            streaming: Vec::new(),
            account_id: None,
            provider: None,
            permission_mode: PermissionMode::Ask,
            turn: None,
            turn_started: None,
            times: HashMap::new(),
            tool_approvals: HashMap::new(),
            approvals: Vec::new(),
            questions: Vec::new(),
            queued: Vec::new(),
            prs: Vec::new(),
        }
    }

    /// Folds in a subscription update.
    pub fn apply(&mut self, update: SessionUpdate) {
        self.loaded = true;
        for event in update.events {
            self.event(event);
        }
        self.streaming = update.streaming;
    }

    /// The name the session list shows: the task label, else the repo's name and
    /// [`Session::name`].
    pub fn title(&self) -> String {
        self.titled(false)
    }

    /// [`Session::title`] with only a branch's last part, for narrow screens.
    pub fn short_title(&self) -> String {
        self.titled(true)
    }

    fn titled(&self, short: bool) -> String {
        if self.task.is_some() || !self.loaded {
            return self.name(short);
        }
        let repo = self.repo.rsplit('/').find(|part| !part.is_empty());
        match repo {
            Some(repo) => format!("{repo} · {}", self.name(short)),
            None => self.name(short),
        }
    }

    /// What the session is called under its repo: its task label; else its branch, without
    /// herder's `herder/` prefix (with `short`, only the branch's last part); else, for the
    /// branch herder made up from the session's id, a summary of its first prompt; the short
    /// id only when there is nothing else.
    pub fn name(&self, short: bool) -> String {
        if let Some(task) = &self.task {
            return task.clone();
        }
        let slug = slug(&self.id);
        let branch = self.branch.strip_prefix("herder/").unwrap_or(&self.branch);
        if !branch.is_empty() && branch != slug {
            return match branch.rsplit('/').next() {
                Some(last) if short => last.to_owned(),
                _ => branch.to_owned(),
            };
        }
        self.entries
            .iter()
            .find_map(|entry| match entry {
                Entry::Item(Item {
                    body: ItemBody::UserMessage { text, .. },
                    ..
                }) => summary(text),
                _ => None,
            })
            .unwrap_or(slug)
    }

    /// Whether the session waits on a user: its status says so, or an approval or question
    /// is put to the user.
    pub fn needs_user(&self) -> bool {
        self.status == SessionStatus::NeedsYou
            || self.approvals.iter().any(|a| a.routed_to == Route::User)
            || self.questions.iter().any(|q| q.routed_to == Route::User)
    }

    fn event(&mut self, event: Event) {
        let notice = |text: String, tone| Entry::Notice { text, tone };
        let at = event.at;
        let entry = match event.body {
            EventBody::SessionCreated {
                repo,
                worktree,
                branch,
                model,
                parent,
                task,
                provider,
                account_id,
                permission_mode,
                ..
            } => {
                self.account_id = Some(account_id);
                self.provider = Some(provider);
                self.permission_mode = permission_mode;
                self.repo = repo;
                self.worktree = worktree;
                self.branch = branch;
                self.model = model;
                self.parent = parent;
                self.task = task;
                None
            }
            EventBody::SessionStatusChanged { status } => {
                self.status = status;
                None
            }
            EventBody::BranchCheckedOut { branch } => {
                let text = format!("checked out {branch}");
                self.branch = branch;
                Some(notice(text, Tone::Info))
            }
            EventBody::ItemAdded { item } => {
                if let ItemBody::UserMessage { text, .. } = &item.body
                    && let Some(at) = self.queued.iter().position(|queued| queued == text)
                {
                    self.queued.remove(at);
                }
                self.times.insert(item.id.clone(), at);
                Some(Entry::Item(item))
            }
            EventBody::TurnStarted { turn_id } => {
                self.turn = Some(turn_id);
                self.turn_started = Some(at);
                None
            }
            EventBody::TurnCompleted { turn_id } => {
                let took = self.turn_ended(&turn_id, at);
                Some(Entry::TurnEnded {
                    took,
                    interrupted: false,
                })
            }
            EventBody::TurnInterrupted { turn_id } => {
                let took = self.turn_ended(&turn_id, at);
                Some(Entry::TurnEnded {
                    took,
                    interrupted: true,
                })
            }
            EventBody::TurnFailed { turn_id, error } => {
                self.turn_ended(&turn_id, at);
                Some(Entry::TurnFailed {
                    class: error.class,
                    message: error.message,
                })
            }
            EventBody::ApprovalRequested {
                approval_id,
                tool_call_id,
                summary,
                routed_to,
                reason,
                ..
            } => {
                let line = asked("approval", &summary, routed_to);
                self.tool_approvals
                    .insert(tool_call_id.clone(), ToolApproval::Pending);
                self.approvals.push(PendingApproval {
                    id: approval_id,
                    tool_call_id,
                    summary,
                    routed_to,
                    reason,
                    note: None,
                    since: at,
                });
                line
            }
            EventBody::ApprovalEscalated {
                approval_id,
                reason,
                note,
            } => {
                let line = escalated("approval", reason, note.as_deref());
                for approval in &mut self.approvals {
                    if approval.id == approval_id {
                        approval.routed_to = Route::User;
                        approval.reason = Some(reason);
                        approval.note.clone_from(&note);
                        approval.since = at;
                    }
                }
                Some(line)
            }
            EventBody::QuestionEscalated {
                question_id,
                reason,
                note,
            } => {
                let line = escalated("question", reason, note.as_deref());
                for question in &mut self.questions {
                    if question.id == question_id {
                        question.routed_to = Route::User;
                        question.reason = Some(reason);
                        question.note.clone_from(&note);
                        question.since = at;
                    }
                }
                Some(line)
            }
            EventBody::PermissionModeChanged { mode } => {
                self.permission_mode = mode;
                Some(Entry::Switch(format!("mode set to {}", mode_name(mode))))
            }
            EventBody::ApprovalResolved {
                approval_id,
                decision,
                answered_by,
            } => {
                let resolved = self
                    .approvals
                    .iter()
                    .position(|approval| approval.id == approval_id)
                    .map(|at| self.approvals.remove(at));
                let tool = resolved.as_ref().map(|approval| {
                    let state = match decision {
                        ApprovalOutcome::Allow => ToolApproval::Allowed,
                        ApprovalOutcome::Deny | ApprovalOutcome::Expired => ToolApproval::Denied,
                    };
                    self.tool_approvals
                        .insert(approval.tool_call_id.clone(), state);
                    self.tool_name(&approval.tool_call_id)
                        .unwrap_or_else(|| approval.summary.clone())
                });
                let decision = match decision {
                    ApprovalOutcome::Allow => "allowed",
                    ApprovalOutcome::Deny => "denied",
                    ApprovalOutcome::Expired => "expired",
                };
                let what =
                    tool.map_or_else(|| decision.to_owned(), |tool| format!("{decision} {tool}"));
                let text = match answered_by {
                    Answerer::Primary { .. } => format!("{what} by the primary session"),
                    Answerer::User => format!("{what} by you"),
                };
                Some(Entry::Resolved {
                    approval: true,
                    text,
                })
            }
            EventBody::QuestionAsked {
                question_id,
                turn_id,
                text,
                choices,
                routed_to,
                reason,
            } => {
                let line = asked("question", &text, routed_to);
                self.questions.push(PendingQuestion {
                    id: question_id,
                    turn_id,
                    text,
                    choices,
                    routed_to,
                    reason,
                    note: None,
                    since: at,
                });
                line
            }
            EventBody::QuestionAnswered {
                question_id,
                answer,
                answered_by,
            } => {
                let asked = self.questions.iter().position(|q| q.id == question_id);
                let asked = asked.map(|at| self.questions.remove(at));
                let answer = match answer {
                    Answer::Text { text } => text,
                    Answer::Choice { index } => asked
                        .as_ref()
                        .and_then(|q| q.choices.get(index as usize).cloned())
                        .unwrap_or_else(|| format!("choice {}", u64::from(index) + 1)),
                };
                let by = match answered_by {
                    Answerer::Primary { .. } => "the primary session",
                    Answerer::User => "you",
                };
                let text = match asked {
                    Some(asked) => format!("{} · {answer} · by {by}", first_line(&asked.text)),
                    None => format!("answered {answer} · by {by}"),
                };
                Some(Entry::Resolved {
                    approval: false,
                    text,
                })
            }
            EventBody::ChildSpawned {
                child_session_id,
                task,
            } => Some(Entry::Child {
                session_id: child_session_id,
                task,
            }),
            EventBody::ChildReported {
                child_session_id,
                summary,
                ..
            } => Some(Entry::Report {
                session_id: child_session_id,
                summary,
            }),
            EventBody::ModelSwitched { model } => {
                let text = format!("switched to {model}");
                self.model = model;
                Some(Entry::Switch(text))
            }
            // A switch nobody asked for is a failover from an account that hit its limit.
            EventBody::AccountSwitched { account_id } => {
                let text = match event.by {
                    Some(_) => format!("switched to {account_id}"),
                    None => format!("failed over to {account_id}: the last account hit its limit"),
                };
                self.account_id = Some(account_id);
                Some(Entry::Switch(text))
            }
            EventBody::ProviderSwitched {
                provider,
                account_id,
                model,
            } => {
                let to = format!("{account_id} · {model} (transcript replayed)");
                let text = match event.by {
                    Some(_) => format!("switched to {to}"),
                    None => format!("failed over to {to}"),
                };
                self.account_id = Some(account_id);
                self.provider = Some(provider);
                self.model = model;
                Some(Entry::Switch(text))
            }
            EventBody::PrLinked { pr } => {
                let number = pr.number;
                self.track(pr);
                Some(Entry::Pr(number))
            }
            EventBody::PrUpdated { pr } => {
                self.track(pr);
                None
            }
            EventBody::PrUnlinked { number } => {
                self.prs.retain(|pr| pr.number != number);
                Some(notice(
                    format!("pull request #{number} unlinked"),
                    Tone::Info,
                ))
            }
            EventBody::Unknown => None,
        };
        self.entries.extend(entry);
    }

    /// Ends `turn_id` at `at`; returns how many seconds it took, when its start is known.
    fn turn_ended(&mut self, turn_id: &TurnId, at: Timestamp) -> Option<i64> {
        let mut took = None;
        if self.turn.as_ref() == Some(turn_id) {
            self.turn = None;
            took = self
                .turn_started
                .take()
                .map(|start| at.as_second() - start.as_second());
        }
        self.questions
            .retain(|question| question.turn_id != *turn_id);
        took
    }

    /// The name of the tool the call item `id` calls.
    pub fn tool_name(&self, id: &ItemId) -> Option<String> {
        self.entries.iter().rev().find_map(|entry| match entry {
            Entry::Item(Item {
                id: item_id,
                body: ItemBody::ToolCall { name, .. },
                ..
            }) if item_id == id => Some(name.clone()),
            _ => None,
        })
    }

    /// Adds `pr`, or replaces the linked pull request with its number.
    fn track(&mut self, pr: PullRequest) {
        match self.prs.iter_mut().find(|known| known.number == pr.number) {
            Some(known) => *known = pr,
            None => self.prs.push(pr),
        }
    }
}

/// The transcript line for an approval or question put to the primary session first. One
/// put to the user has none: the request panel shows it until it is answered.
fn asked(what: &str, text: &str, routed_to: Route) -> Option<Entry> {
    (routed_to == Route::Primary).then(|| Entry::Notice {
        text: format!("{what} for the primary session: {}", first_line(text)),
        tone: Tone::Info,
    })
}

/// The first non-blank line of `text`.
pub fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
}

/// The transcript line for a request the primary session handed to the user.
fn escalated(what: &str, reason: EscalationReason, note: Option<&str>) -> Entry {
    let mut text = format!("{what} escalated to you: {}", reason_text(reason));
    if let Some(note) = note {
        text.push_str(&format!("; the primary says: {note}"));
    }
    Entry::Notice {
        text,
        tone: Tone::Attention,
    }
}

/// Why a child's request is the user's, in words.
pub fn reason_text(reason: EscalationReason) -> &'static str {
    match reason {
        EscalationReason::MarkedByPrimary => "the primary session left it to you",
        EscalationReason::ExceedsAuthority => "beyond what the primary session may decide",
        EscalationReason::Timeout => "the primary session did not answer in time",
    }
}

/// A permission mode as typed in the palette and shown in the session view.
pub fn mode_name(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::ReadOnly => "read_only",
        PermissionMode::Ask => "ask",
        PermissionMode::AutoEdit => "auto_edit",
        PermissionMode::FullAccess => "full_access",
    }
}

/// Every permission mode, least permissive first.
pub const MODES: [PermissionMode; 4] = [
    PermissionMode::ReadOnly,
    PermissionMode::Ask,
    PermissionMode::AutoEdit,
    PermissionMode::FullAccess,
];

/// Characters a prompt's summary keeps.
const SUMMARY: usize = 40;

/// The short form of a session id that herder names its branches after: the id's last 8
/// characters, lowercased, as the daemon's worktrees do.
fn slug(id: &SessionId) -> String {
    let id = id.as_str();
    let start = id.char_indices().rev().nth(7).map_or(0, |(at, _)| at);
    id[start..].to_lowercase()
}

/// A prompt's first line, cut at a word to about [`SUMMARY`] characters; `None` for a blank
/// prompt.
fn summary(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    if line.chars().count() <= SUMMARY {
        return Some(line.to_owned());
    }
    let cut: String = line.chars().take(SUMMARY).collect();
    let words = match cut.rfind(' ') {
        Some(at) if at > SUMMARY / 2 => &cut[..at],
        _ => cut.as_str(),
    };
    Some(format!(
        "{}…",
        words.trim_end_matches([',', '.', ':', ';', ' '])
    ))
}

#[cfg(test)]
mod tests {
    use herder_protocol::{TurnError, TurnId};

    use super::*;
    use crate::fake::{added, assistant, created, item, status, update};

    #[test]
    fn events_fill_in_the_list_facts_and_the_transcript() {
        let mut session = Session::new(SessionId::new("s1"));
        assert_eq!(session.title(), "s1");
        session.apply(update(
            "s1",
            1,
            vec![
                created("herder/api", None, None),
                status(SessionStatus::Running),
                added("i1", assistant("Looking.")),
                EventBody::TurnFailed {
                    turn_id: TurnId::new("turn-1"),
                    error: TurnError {
                        class: ErrorClass::Transient,
                        message: "network".into(),
                    },
                },
                status(SessionStatus::NeedsYou),
            ],
            vec![item("i2", assistant("Ha"))],
        ));
        // herder's own branch prefix says nothing.
        assert_eq!(session.title(), "app · api");
        assert_eq!(session.status, SessionStatus::NeedsYou);
        assert_eq!(
            session.entries,
            [
                Entry::Item(item("i1", assistant("Looking."))),
                Entry::TurnFailed {
                    class: ErrorClass::Transient,
                    message: "network".into(),
                },
            ]
        );
        assert_eq!(session.streaming, [item("i2", assistant("Ha"))]);

        // Each update replaces the streaming list.
        session.apply(update("s1", 6, vec![], vec![]));
        assert!(session.streaming.is_empty());
    }

    #[test]
    fn turns_time_their_replies_and_approvals_mark_their_tool_calls() {
        use crate::fake::{approval, at, started};
        let mut session = Session::new(SessionId::new("s1"));
        session.apply(at(
            update(
                "s1",
                1,
                vec![started("turn-1"), approval("a1", "$ ls")],
                vec![],
            ),
            100,
        ));
        assert_eq!(
            session.tool_approvals.get(&ItemId::new("call-1")),
            Some(&ToolApproval::Pending)
        );
        // A request put to the user takes no transcript line: the panel shows it.
        assert!(session.entries.is_empty());
        session.apply(at(
            update(
                "s1",
                3,
                vec![
                    EventBody::ApprovalResolved {
                        approval_id: ApprovalId::new("a1"),
                        decision: ApprovalOutcome::Deny,
                        answered_by: Answerer::User,
                    },
                    EventBody::TurnInterrupted {
                        turn_id: TurnId::new("turn-1"),
                    },
                ],
                vec![],
            ),
            172,
        ));
        assert_eq!(
            session.tool_approvals.get(&ItemId::new("call-1")),
            Some(&ToolApproval::Denied)
        );
        assert_eq!(
            session.entries,
            [
                Entry::Resolved {
                    approval: true,
                    text: "denied $ ls by you".into(),
                },
                Entry::TurnEnded {
                    took: Some(72),
                    interrupted: true,
                },
            ]
        );
    }

    #[test]
    fn a_child_is_titled_by_its_task_and_knows_its_parent() {
        let mut session = Session::new(SessionId::new("s3"));
        session.apply(update(
            "s3",
            1,
            vec![created("herder/t", Some("s2"), Some("write the tests"))],
            vec![],
        ));
        assert_eq!(session.title(), "write the tests");
        assert_eq!(session.parent, Some(SessionId::new("s2")));
    }

    #[test]
    fn an_escalation_puts_a_childs_request_to_the_user_with_why() {
        use crate::fake::{approval, at, question, to_primary};
        use herder_protocol::EscalationReason;

        let mut session = Session::new(SessionId::new("s3"));
        session.apply(at(
            update(
                "s3",
                1,
                vec![
                    created("herder/t", Some("s2"), Some("write the tests")),
                    to_primary(question("q1", "Which port?", &[])),
                    to_primary(approval("a1", "Edit src/lib.rs")),
                ],
                vec![],
            ),
            100,
        ));
        assert!(!session.needs_user());
        assert_eq!(session.questions[0].routed_to, Route::Primary);
        session.apply(at(
            update(
                "s3",
                4,
                vec![EventBody::QuestionEscalated {
                    question_id: QuestionId::new("q1"),
                    reason: EscalationReason::MarkedByPrimary,
                    note: Some("Your call.".into()),
                }],
                vec![],
            ),
            200,
        ));
        let question = &session.questions[0];
        assert_eq!(question.routed_to, Route::User);
        assert_eq!(question.reason, Some(EscalationReason::MarkedByPrimary));
        assert_eq!(question.note.as_deref(), Some("Your call."));
        assert_eq!(question.since, Timestamp::from_second(200).unwrap());
        assert!(session.needs_user());

        // The primary answers the approval.
        session.apply(update(
            "s3",
            5,
            vec![EventBody::ApprovalResolved {
                approval_id: ApprovalId::new("a1"),
                decision: ApprovalOutcome::Allow,
                answered_by: Answerer::Primary {
                    session_id: SessionId::new("s2"),
                },
            }],
            vec![],
        ));
        let notices: Vec<&str> = session
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Notice { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            notices,
            [
                "question for the primary session: Which port?",
                "approval for the primary session: Edit src/lib.rs",
                "question escalated to you: the primary session left it to you; \
                 the primary says: Your call.",
            ]
        );
        assert_eq!(
            session.entries.last(),
            Some(&Entry::Resolved {
                approval: true,
                text: "allowed Edit src/lib.rs by the primary session".into(),
            })
        );
    }

    #[test]
    fn a_question_the_primary_answered_says_so() {
        use crate::fake::{question, to_primary};
        let mut session = Session::new(SessionId::new("s3"));
        session.apply(update(
            "s3",
            1,
            vec![
                to_primary(question("q1", "Which port?", &["8080", "3000"])),
                EventBody::QuestionAnswered {
                    question_id: QuestionId::new("q1"),
                    answer: Answer::Choice { index: 1 },
                    answered_by: Answerer::Primary {
                        session_id: SessionId::new("s2"),
                    },
                },
            ],
            vec![],
        ));
        assert!(session.questions.is_empty());
        assert_eq!(
            session.entries.last(),
            Some(&Entry::Resolved {
                approval: false,
                text: "Which port? · 3000 · by the primary session".into(),
            })
        );
    }

    #[test]
    fn a_switch_nobody_asked_for_is_a_failover() {
        let mut session = Session::new(SessionId::new("s1"));
        let mut asked = update(
            "s1",
            1,
            vec![
                created("herder/t", None, None),
                EventBody::AccountSwitched {
                    account_id: AccountId::new("claude-work"),
                },
            ],
            vec![],
        );
        asked.events[1].by = Some(herder_protocol::UserId::new("ann"));
        session.apply(asked);
        session.apply(update(
            "s1",
            3,
            vec![EventBody::AccountSwitched {
                account_id: AccountId::new("claude-spare"),
            }],
            vec![],
        ));
        assert_eq!(
            session.entries,
            [
                Entry::Switch("switched to claude-work".into()),
                Entry::Switch("failed over to claude-spare: the last account hit its limit".into()),
            ]
        );
        assert_eq!(session.account_id, Some(AccountId::new("claude-spare")));
        assert_eq!(session.provider, Some(herder_protocol::Provider::Claude));
    }

    #[test]
    fn a_session_is_named_by_task_branch_first_prompt_then_id() {
        let mut session = Session::new(SessionId::new("01JABCDEFGHJKMNPEQ3Z0KAE"));
        session.loaded = true;
        session.repo = "/home/ann/src/app".into();
        // Only the id to go by: its short form, as herder's branches use.
        assert_eq!(session.name(false), "eq3z0kae");
        // The branch herder made up from the id says no more; the first prompt does.
        session.branch = "herder/eq3z0kae".into();
        session.entries.push(Entry::Item(item(
            "i1",
            ItemBody::UserMessage {
                text: "\n  Fix the login redirect after the session expires, and test it\nthanks"
                    .into(),
                attachments: Vec::new(),
            },
        )));
        assert_eq!(session.name(false), "Fix the login redirect after the…");
        assert_eq!(session.title(), "app · Fix the login redirect after the…");
        // A branch someone named, without herder's prefix; short, its last part.
        session.branch = "herder/feature/login".into();
        assert_eq!(session.name(false), "feature/login");
        assert_eq!(session.name(true), "login");
        // A task label wins.
        session.task = Some("write tests".into());
        assert_eq!(session.title(), "write tests");
    }
}
