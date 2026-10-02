//! What the TUI knows of one session, folded from its subscription's updates: the facts the
//! session list shows and the transcript the session view renders.

use herder_client_core::SessionUpdate;
use herder_protocol::{
    AccountId, Answer, ApprovalId, ApprovalOutcome, Event, EventBody, HostId, Item, ItemBody,
    PermissionMode, PullRequest, QuestionId, Route, SessionId, SessionStatus, TurnId,
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
    /// Current permission mode.
    pub permission_mode: PermissionMode,
    /// The turn running now, from its start to its end.
    pub turn: Option<TurnId>,
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
    /// What the agent wants to do.
    pub summary: String,
    /// Who is asked first; a user can always answer.
    pub routed_to: Route,
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
}

/// One completed line of the transcript.
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
}

/// How a notice is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Ordinary progress.
    Info,
    /// Waiting on the user.
    Attention,
    /// A failure.
    Error,
}

impl Session {
    /// A session known only by id, until its first update.
    pub fn new(id: SessionId) -> Self {
        Self {
            id,
            loaded: false,
            repo: String::new(),
            branch: String::new(),
            model: String::new(),
            parent: None,
            task: None,
            status: SessionStatus::Idle,
            entries: Vec::new(),
            streaming: Vec::new(),
            account_id: None,
            permission_mode: PermissionMode::Ask,
            turn: None,
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

    /// The name the session list shows: the task label, else the repo's name and branch.
    pub fn title(&self) -> String {
        if let Some(task) = &self.task {
            return task.clone();
        }
        if !self.loaded {
            return self.id.to_string();
        }
        let repo = self.repo.rsplit('/').find(|part| !part.is_empty());
        match repo {
            Some(repo) => format!("{repo} · {}", self.branch),
            None => self.branch.clone(),
        }
    }

    fn event(&mut self, event: Event) {
        let notice = |text: String, tone| Entry::Notice { text, tone };
        let entry = match event.body {
            EventBody::SessionCreated {
                repo,
                branch,
                model,
                parent,
                task,
                account_id,
                permission_mode,
                ..
            } => {
                self.account_id = Some(account_id);
                self.permission_mode = permission_mode;
                self.repo = repo;
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
                if let ItemBody::UserMessage { text } = &item.body
                    && let Some(at) = self.queued.iter().position(|queued| queued == text)
                {
                    self.queued.remove(at);
                }
                Some(Entry::Item(item))
            }
            EventBody::TurnStarted { turn_id } => {
                self.turn = Some(turn_id);
                None
            }
            EventBody::TurnCompleted { turn_id } => {
                self.turn_ended(&turn_id);
                None
            }
            EventBody::TurnInterrupted { turn_id } => {
                self.turn_ended(&turn_id);
                Some(notice("turn interrupted".to_owned(), Tone::Info))
            }
            EventBody::TurnFailed { turn_id, error } => {
                self.turn_ended(&turn_id);
                Some(notice(
                    format!("turn failed: {}", error.message),
                    Tone::Error,
                ))
            }
            EventBody::ApprovalRequested {
                approval_id,
                summary,
                routed_to,
                ..
            } => {
                let text = format!("approval needed: {summary}");
                self.approvals.push(PendingApproval {
                    id: approval_id,
                    summary,
                    routed_to,
                });
                Some(notice(text, Tone::Attention))
            }
            EventBody::ApprovalEscalated { approval_id, .. } => {
                for approval in &mut self.approvals {
                    if approval.id == approval_id {
                        approval.routed_to = Route::User;
                    }
                }
                None
            }
            EventBody::QuestionEscalated { question_id, .. } => {
                for question in &mut self.questions {
                    if question.id == question_id {
                        question.routed_to = Route::User;
                    }
                }
                None
            }
            EventBody::PermissionModeChanged { mode } => {
                self.permission_mode = mode;
                Some(notice(
                    format!("permission mode set to {}", mode_name(mode)),
                    Tone::Info,
                ))
            }
            EventBody::ApprovalResolved {
                approval_id,
                decision,
                ..
            } => {
                self.approvals.retain(|approval| approval.id != approval_id);
                let decision = match decision {
                    ApprovalOutcome::Allow => "allowed",
                    ApprovalOutcome::Deny => "denied",
                    ApprovalOutcome::Expired => "approval expired",
                };
                Some(notice(decision.to_owned(), Tone::Info))
            }
            EventBody::QuestionAsked {
                question_id,
                turn_id,
                text,
                choices,
                routed_to,
                ..
            } => {
                let line = format!("question: {text}");
                self.questions.push(PendingQuestion {
                    id: question_id,
                    turn_id,
                    text,
                    choices,
                    routed_to,
                });
                Some(notice(line, Tone::Attention))
            }
            EventBody::QuestionAnswered {
                question_id,
                answer,
                ..
            } => {
                let asked = self.questions.iter().position(|q| q.id == question_id);
                let asked = asked.map(|at| self.questions.remove(at));
                let answer = match answer {
                    Answer::Text { text } => text,
                    Answer::Choice { index } => asked
                        .and_then(|q| q.choices.get(index as usize).cloned())
                        .unwrap_or_else(|| format!("choice {}", u64::from(index) + 1)),
                };
                Some(notice(format!("answered: {answer}"), Tone::Info))
            }
            EventBody::ChildSpawned { task, .. } => {
                Some(notice(format!("spawned child: {task}"), Tone::Info))
            }
            EventBody::ChildReported { summary, .. } => {
                Some(notice(format!("child reported: {summary}"), Tone::Info))
            }
            EventBody::ModelSwitched { model } => {
                let text = format!("model switched to {model}");
                self.model = model;
                Some(notice(text, Tone::Info))
            }
            EventBody::AccountSwitched { account_id } => Some(notice(
                format!("account switched to {account_id}"),
                Tone::Info,
            )),
            EventBody::ProviderSwitched {
                provider, model, ..
            } => {
                let text = format!("switched to {} ({model})", provider.as_str());
                self.model = model;
                Some(notice(text, Tone::Info))
            }
            EventBody::PrLinked { pr } => {
                let text = format!("pull request #{} linked: {}", pr.number, pr.title);
                self.track(pr);
                Some(notice(text, Tone::Info))
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

    fn turn_ended(&mut self, turn_id: &TurnId) {
        if self.turn.as_ref() == Some(turn_id) {
            self.turn = None;
        }
        self.questions
            .retain(|question| question.turn_id != *turn_id);
    }

    /// Adds `pr`, or replaces the linked pull request with its number.
    fn track(&mut self, pr: PullRequest) {
        match self.prs.iter_mut().find(|known| known.number == pr.number) {
            Some(known) => *known = pr,
            None => self.prs.push(pr),
        }
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

#[cfg(test)]
mod tests {
    use herder_protocol::{ErrorClass, TurnError, TurnId};

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
        assert_eq!(session.title(), "app · herder/api");
        assert_eq!(session.status, SessionStatus::NeedsYou);
        assert_eq!(
            session.entries,
            [
                Entry::Item(item("i1", assistant("Looking."))),
                Entry::Notice {
                    text: "turn failed: network".into(),
                    tone: Tone::Error
                },
            ]
        );
        assert_eq!(session.streaming, [item("i2", assistant("Ha"))]);

        // Each update replaces the streaming list.
        session.apply(update("s1", 6, vec![], vec![]));
        assert!(session.streaming.is_empty());
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
}
