//! What the TUI knows of one session, folded from its subscription's updates: the facts the
//! session list shows and the transcript the session view renders.

use herder_client_core::SessionUpdate;
use herder_protocol::{
    Answer, ApprovalOutcome, Event, EventBody, HostId, Item, SessionId, SessionStatus,
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
                ..
            } => {
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
            EventBody::ItemAdded { item } => Some(Entry::Item(item)),
            EventBody::TurnInterrupted { .. } => {
                Some(notice("turn interrupted".to_owned(), Tone::Info))
            }
            EventBody::TurnFailed { error, .. } => Some(notice(
                format!("turn failed: {}", error.message),
                Tone::Error,
            )),
            EventBody::ApprovalRequested { summary, .. } => Some(notice(
                format!("approval needed: {summary}"),
                Tone::Attention,
            )),
            EventBody::ApprovalResolved { decision, .. } => {
                let decision = match decision {
                    ApprovalOutcome::Allow => "allowed",
                    ApprovalOutcome::Deny => "denied",
                    ApprovalOutcome::Expired => "approval expired",
                };
                Some(notice(decision.to_owned(), Tone::Info))
            }
            EventBody::QuestionAsked { text, .. } => {
                Some(notice(format!("question: {text}"), Tone::Attention))
            }
            EventBody::QuestionAnswered { answer, .. } => {
                let answer = match answer {
                    Answer::Text { text } => text,
                    Answer::Choice { index } => format!("choice {}", u64::from(index) + 1),
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
            EventBody::PrLinked { pr } => Some(notice(
                format!("pull request #{} linked: {}", pr.number, pr.title),
                Tone::Info,
            )),
            EventBody::TurnStarted { .. }
            | EventBody::TurnCompleted { .. }
            | EventBody::ApprovalEscalated { .. }
            | EventBody::QuestionEscalated { .. }
            | EventBody::PermissionModeChanged { .. }
            | EventBody::PrUpdated { .. }
            | EventBody::PrUnlinked { .. }
            | EventBody::Unknown => None,
        };
        self.entries.extend(entry);
    }
}

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
