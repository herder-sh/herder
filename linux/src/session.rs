//! What the app knows of one session, folded from its subscription's updates: the transcript
//! the session view draws, the requests waiting on someone, and what the composer's controls
//! show. The same fold as the TUI's, in the same words.

use std::collections::HashMap;

use herder_client_core::SessionUpdate;
use herder_protocol::{
    AccountId, Answer, Answerer, ApprovalId, ApprovalOutcome, ErrorClass, EscalationReason, Event,
    EventBody, Item, ItemBody, ItemId, PermissionMode, Provider, PullRequest, QuestionId, Route,
    SessionId, SessionStatus, Timestamp, TurnId,
};

/// One session as built from its events.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    /// Whether its first update arrived; until then nothing below is known.
    pub loaded: bool,
    /// Repository path on the host.
    pub repo: String,
    /// The session's worktree on the host.
    pub worktree: String,
    /// Branch the session works on now.
    pub branch: String,
    /// Task label, for a child session.
    pub task: Option<String>,
    /// Current model, in the provider's naming.
    pub model: String,
    /// Account it runs on.
    pub account_id: Option<AccountId>,
    /// Provider of that account.
    pub provider: Option<Provider>,
    /// Current permission mode.
    pub permission_mode: PermissionMode,
    /// Latest status, as its events say.
    pub status: SessionStatus,
    /// Completed transcript entries, oldest first. Only ever extended.
    pub entries: Vec<Entry>,
    /// Items streaming now, with their text so far.
    pub streaming: Vec<Item>,
    /// When the running turn started; `None` between turns.
    pub turn_started: Option<Timestamp>,
    turn: Option<TurnId>,
    /// Approval requests nobody answered yet, oldest first.
    pub approvals: Vec<PendingApproval>,
    /// Questions nobody answered yet, oldest first.
    pub questions: Vec<PendingQuestion>,
    /// Tool calls an approval was asked for, and how it stands.
    pub tool_approvals: Vec<(ItemId, ToolApproval)>,
    /// Pull requests linked to the session now, in the order they were linked.
    pub prs: Vec<PullRequest>,
    /// When each item was added, for how long a tool call took.
    added: HashMap<ItemId, Timestamp>,
}

/// An approval request waiting for an answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingApproval {
    pub id: ApprovalId,
    /// The tool call it asks about.
    pub tool_call_id: ItemId,
    /// What the agent wants to do.
    pub summary: String,
    /// Who is asked first; a user can always answer.
    pub routed_to: Route,
    /// Why a child's request went to the user rather than its primary session.
    pub reason: Option<EscalationReason>,
    /// What the primary session said when it escalated the request.
    pub note: Option<String>,
    /// When it was put to whoever it waits on now.
    pub since: Timestamp,
}

/// A question waiting for an answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingQuestion {
    pub id: QuestionId,
    turn_id: TurnId,
    /// The question, as Markdown.
    pub text: String,
    /// Answers to pick from; empty for free text.
    pub choices: Vec<String>,
    pub routed_to: Route,
    pub reason: Option<EscalationReason>,
    pub note: Option<String>,
    pub since: Timestamp,
}

/// Where an approval for a tool call stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolApproval {
    Pending,
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
    Notice { text: String, attention: bool },
    /// A turn ended without failing: the footer of the agent's reply.
    TurnEnded {
        /// Seconds from its start, when the start is known.
        took: Option<i64>,
        interrupted: bool,
        /// The account and model it ran on.
        account: String,
        model: String,
    },
    /// A turn failed.
    TurnFailed { class: ErrorClass, message: String },
    /// The session moved to another model, account, provider or permission mode.
    Switch(String),
    /// An approval or question was answered, or expired.
    Resolved { approval: bool, text: String },
    /// The session spawned a child for a task.
    Child { session_id: SessionId, task: String },
    /// A child finished a turn and reported back.
    Report { summary: String },
    /// A pull request was linked; drawn as it stands now, from [`Session::prs`].
    Pr(u64),
}

impl Default for Session {
    fn default() -> Self {
        Self {
            loaded: false,
            repo: String::new(),
            worktree: String::new(),
            branch: String::new(),
            task: None,
            model: String::new(),
            account_id: None,
            provider: None,
            permission_mode: PermissionMode::Ask,
            status: SessionStatus::Idle,
            entries: Vec::new(),
            streaming: Vec::new(),
            turn_started: None,
            turn: None,
            approvals: Vec::new(),
            questions: Vec::new(),
            tool_approvals: Vec::new(),
            prs: Vec::new(),
            added: HashMap::new(),
        }
    }
}

impl Session {
    /// Folds in a subscription update.
    pub fn apply(&mut self, update: &SessionUpdate) {
        self.loaded = true;
        for event in &update.events {
            self.event(event);
        }
        self.streaming.clone_from(&update.streaming);
    }

    /// The task label, else the repo's name and branch, as the TUI titles a session.
    pub fn title(&self) -> String {
        if let Some(task) = &self.task {
            return task.clone();
        }
        let repo = self.repo.rsplit('/').find(|part| !part.is_empty());
        match repo {
            Some(repo) => format!("{repo} · {}", self.branch),
            None => self.branch.clone(),
        }
    }

    /// Whether a turn runs now.
    pub fn running(&self) -> bool {
        self.turn.is_some()
    }

    /// How the approval for the tool call `id` stands, when one was asked.
    pub fn tool_approval(&self, id: &ItemId) -> Option<ToolApproval> {
        self.tool_approvals
            .iter()
            .find(|(call, _)| call == id)
            .map(|(_, state)| *state)
    }

    /// The result of the tool call `id`: its output and whether it failed.
    pub fn result(&self, id: &ItemId) -> Option<(&str, bool)> {
        self.entries.iter().find_map(|entry| match entry {
            Entry::Item(Item {
                body:
                    ItemBody::ToolResult {
                        call_id,
                        output,
                        is_error,
                    },
                ..
            }) if call_id == id => Some((output.as_str(), *is_error)),
            _ => None,
        })
    }

    /// Seconds from the tool call `id` to its result, once the result came.
    pub fn took(&self, id: &ItemId) -> Option<i64> {
        let result = self.entries.iter().find_map(|entry| match entry {
            Entry::Item(Item {
                id: result,
                body: ItemBody::ToolResult { call_id, .. },
                ..
            }) if call_id == id => Some(result),
            _ => None,
        })?;
        Some(self.added.get(result)?.as_second() - self.added.get(id)?.as_second())
    }

    /// The tool call item `id`: its name and input.
    pub fn tool_call(&self, id: &ItemId) -> Option<(&str, &serde_json::Value)> {
        self.entries.iter().find_map(|entry| match entry {
            Entry::Item(Item {
                id: item_id,
                body: ItemBody::ToolCall { name, input },
                ..
            }) if item_id == id => Some((name.as_str(), input)),
            _ => None,
        })
    }

    fn set_tool_approval(&mut self, id: ItemId, state: ToolApproval) {
        match self.tool_approvals.iter_mut().find(|(call, _)| *call == id) {
            Some((_, known)) => *known = state,
            None => self.tool_approvals.push((id, state)),
        }
    }

    fn event(&mut self, event: &Event) {
        let at = event.at;
        let notice = |text: String| Entry::Notice {
            text,
            attention: false,
        };
        let entry = match &event.body {
            EventBody::SessionCreated {
                repo,
                worktree,
                branch,
                provider,
                account_id,
                model,
                permission_mode,
                task,
                ..
            } => {
                self.repo.clone_from(repo);
                self.worktree.clone_from(worktree);
                self.branch.clone_from(branch);
                self.provider = Some(provider.clone());
                self.account_id = Some(account_id.clone());
                self.model.clone_from(model);
                self.permission_mode = *permission_mode;
                self.task.clone_from(task);
                None
            }
            EventBody::SessionStatusChanged { status, .. } => {
                self.status = *status;
                None
            }
            EventBody::BranchCheckedOut { branch } => {
                self.branch.clone_from(branch);
                Some(notice(format!("checked out {branch}")))
            }
            EventBody::ItemAdded { item } => {
                self.added.insert(item.id.clone(), at);
                Some(Entry::Item(item.clone()))
            }
            EventBody::TurnStarted { turn_id } => {
                self.turn = Some(turn_id.clone());
                self.turn_started = Some(at);
                None
            }
            EventBody::TurnCompleted { turn_id } | EventBody::TurnInterrupted { turn_id } => {
                let took = self.turn_ended(turn_id, at);
                Some(Entry::TurnEnded {
                    took,
                    interrupted: matches!(event.body, EventBody::TurnInterrupted { .. }),
                    account: self
                        .account_id
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                    model: self.model.clone(),
                })
            }
            EventBody::TurnFailed { turn_id, error } => {
                self.turn_ended(turn_id, at);
                Some(Entry::TurnFailed {
                    class: error.class,
                    message: error.message.clone(),
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
                self.set_tool_approval(tool_call_id.clone(), ToolApproval::Pending);
                self.approvals.push(PendingApproval {
                    id: approval_id.clone(),
                    tool_call_id: tool_call_id.clone(),
                    summary: summary.clone(),
                    routed_to: *routed_to,
                    reason: *reason,
                    note: None,
                    since: at,
                });
                asked("approval", summary, *routed_to)
            }
            EventBody::ApprovalEscalated {
                approval_id,
                reason,
                note,
            } => {
                for approval in &mut self.approvals {
                    if approval.id == *approval_id {
                        approval.routed_to = Route::User;
                        approval.reason = Some(*reason);
                        approval.note.clone_from(note);
                        approval.since = at;
                    }
                }
                Some(escalated("approval", *reason))
            }
            EventBody::ApprovalResolved {
                approval_id,
                decision,
                answered_by,
            } => {
                let resolved = self
                    .approvals
                    .iter()
                    .position(|approval| approval.id == *approval_id)
                    .map(|at| self.approvals.remove(at));
                let tool = resolved.map(|approval| {
                    let state = match decision {
                        ApprovalOutcome::Allow => ToolApproval::Allowed,
                        ApprovalOutcome::Deny | ApprovalOutcome::Expired => ToolApproval::Denied,
                    };
                    self.set_tool_approval(approval.tool_call_id.clone(), state);
                    self.tool_call(&approval.tool_call_id)
                        .map_or(approval.summary, |(name, _)| name.to_owned())
                });
                let decision = match decision {
                    ApprovalOutcome::Allow => "allowed",
                    ApprovalOutcome::Deny => "denied",
                    ApprovalOutcome::Expired => "expired",
                };
                let what =
                    tool.map_or_else(|| decision.to_owned(), |tool| format!("{decision} {tool}"));
                Some(Entry::Resolved {
                    approval: true,
                    text: format!("{what} · by {}", answerer(answered_by)),
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
                self.questions.push(PendingQuestion {
                    id: question_id.clone(),
                    turn_id: turn_id.clone(),
                    text: text.clone(),
                    choices: choices.clone(),
                    routed_to: *routed_to,
                    reason: *reason,
                    note: None,
                    since: at,
                });
                asked("question", text, *routed_to)
            }
            EventBody::QuestionEscalated {
                question_id,
                reason,
                note,
            } => {
                for question in &mut self.questions {
                    if question.id == *question_id {
                        question.routed_to = Route::User;
                        question.reason = Some(*reason);
                        question.note.clone_from(note);
                        question.since = at;
                    }
                }
                Some(escalated("question", *reason))
            }
            EventBody::QuestionAnswered {
                question_id,
                answer,
                answered_by,
            } => {
                let asked = self
                    .questions
                    .iter()
                    .position(|q| q.id == *question_id)
                    .map(|at| self.questions.remove(at));
                let answer = match answer {
                    Answer::Text { text } => text.clone(),
                    Answer::Choice { index } => asked
                        .as_ref()
                        .and_then(|q| q.choices.get(*index as usize).cloned())
                        .unwrap_or_else(|| format!("choice {}", u64::from(*index) + 1)),
                };
                let by = answerer(answered_by);
                Some(Entry::Resolved {
                    approval: false,
                    text: match asked {
                        Some(asked) => format!("{} · {answer} · by {by}", first_line(&asked.text)),
                        None => format!("answered {answer} · by {by}"),
                    },
                })
            }
            EventBody::ChildSpawned {
                child_session_id,
                task,
            } => Some(Entry::Child {
                session_id: child_session_id.clone(),
                task: task.clone(),
            }),
            EventBody::ChildReported { summary, .. } => Some(Entry::Report {
                summary: summary.clone(),
            }),
            EventBody::ModelSwitched { model } => {
                self.model.clone_from(model);
                Some(Entry::Switch(format!("switched to {model}")))
            }
            // A switch nobody asked for is a failover from an account that hit its limit.
            EventBody::AccountSwitched { account_id } => {
                self.account_id = Some(account_id.clone());
                Some(Entry::Switch(match event.by {
                    Some(_) => format!("switched to {account_id}"),
                    None => format!("failed over to {account_id}: the last account hit its limit"),
                }))
            }
            EventBody::ProviderSwitched {
                provider,
                account_id,
                model,
            } => {
                self.provider = Some(provider.clone());
                self.account_id = Some(account_id.clone());
                self.model.clone_from(model);
                let to = format!("{account_id} · {model} (transcript replayed)");
                Some(Entry::Switch(match event.by {
                    Some(_) => format!("switched to {to}"),
                    None => format!("failed over to {to}"),
                }))
            }
            EventBody::PermissionModeChanged { mode } => {
                self.permission_mode = *mode;
                Some(Entry::Switch(format!("mode set to {}", mode_name(*mode))))
            }
            EventBody::PrLinked { pr } => {
                self.track(pr);
                Some(Entry::Pr(pr.number))
            }
            EventBody::PrUpdated { pr } => {
                self.track(pr);
                None
            }
            EventBody::PrUnlinked { number } => {
                self.prs.retain(|pr| pr.number != *number);
                Some(notice(format!("pull request #{number} unlinked")))
            }
            EventBody::SessionForked { from_host, .. } => Some(Entry::Switch(format!(
                "switched to this machine (from {from_host})"
            ))),
            // Shown from the session list's title.
            EventBody::TitleChanged { .. } | EventBody::Unknown => None,
        };
        self.entries.extend(entry);
    }

    /// Ends `turn_id` at `at`; how many seconds it took, when its start is known.
    fn turn_ended(&mut self, turn_id: &TurnId, at: Timestamp) -> Option<i64> {
        let mut took = None;
        if self.turn.as_ref() == Some(turn_id) {
            self.turn = None;
            took = self
                .turn_started
                .take()
                .map(|start| at.as_second() - start.as_second());
        }
        // A question dies with its turn; approvals are resolved by the daemon.
        self.questions.retain(|q| q.turn_id != *turn_id);
        took
    }

    fn track(&mut self, pr: &PullRequest) {
        match self.prs.iter_mut().find(|known| known.number == pr.number) {
            Some(known) => known.clone_from(pr),
            None => self.prs.push(pr.clone()),
        }
    }
}

/// The line for a request put to the primary session first. One put to the user has none:
/// its card shows it until it is answered.
fn asked(what: &str, text: &str, routed_to: Route) -> Option<Entry> {
    (routed_to == Route::Primary).then(|| Entry::Notice {
        text: format!("{what} for the primary session: {}", first_line(text)),
        attention: false,
    })
}

fn escalated(what: &str, reason: EscalationReason) -> Entry {
    Entry::Notice {
        text: format!("{what} escalated to you: {}", reason_text(reason)),
        attention: true,
    }
}

fn answerer(answerer: &Answerer) -> &'static str {
    match answerer {
        Answerer::Primary { .. } => "the primary session",
        Answerer::User => "you",
    }
}

/// The first non-blank line of `text`.
pub fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
}

/// Why a child's request is the user's, in words.
pub fn reason_text(reason: EscalationReason) -> &'static str {
    match reason {
        EscalationReason::MarkedByPrimary => "the primary session left it to you",
        EscalationReason::ExceedsAuthority => "beyond what the primary session may decide",
        EscalationReason::Timeout => "the primary session did not answer in time",
    }
}

/// A permission mode as the TUI names it.
pub fn mode_name(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::ReadOnly => "read_only",
        PermissionMode::Ask => "ask",
        PermissionMode::AutoEdit => "auto_edit",
        PermissionMode::FullAccess => "full_access",
    }
}

/// What a permission mode lets the agent do, in words.
pub fn mode_description(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::ReadOnly => "Reads only; every write or command is refused",
        PermissionMode::Ask => "Asks before every write or command",
        PermissionMode::AutoEdit => "Edits files freely; asks before commands",
        PermissionMode::FullAccess => "Does anything without asking",
    }
}

/// Seconds as the TUI shows a duration: `42s`, `1m 02s`, `2h 05m`.
pub fn duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h {:02}m", seconds / 3600, seconds % 3600 / 60),
    }
}

#[cfg(test)]
pub mod tests {
    use herder_protocol::{ApprovalDecision, HostId, Seq, TurnError};

    use super::*;

    /// An update of events in order, `at` one second apart from the epoch's 1000th second.
    pub fn events(bodies: Vec<(Option<&str>, EventBody)>) -> SessionUpdate {
        SessionUpdate {
            events: bodies
                .into_iter()
                .enumerate()
                .map(|(n, (by, body))| Event {
                    session_id: SessionId::new("s1"),
                    seq: Seq::try_from(n + 1).unwrap_or_default(),
                    at: Timestamp::from_second(1000 + i64::try_from(n).unwrap_or(0))
                        .unwrap_or_default(),
                    by: by.map(herder_protocol::UserId::new),
                    body,
                })
                .collect(),
            streaming: Vec::new(),
        }
    }

    pub fn item(id: &str, body: ItemBody) -> EventBody {
        EventBody::ItemAdded {
            item: Item {
                agent_message: None,
                parent_call_id: None,
                id: ItemId::new(id),
                turn_id: TurnId::new("t1"),
                body,
            },
        }
    }

    pub fn created() -> EventBody {
        EventBody::SessionCreated {
            repo: "/srv/app".into(),
            worktree: "/srv/wt/api".into(),
            branch: "herder/api".into(),
            provider: Provider::Claude,
            account_id: AccountId::new("claude-main"),
            model: "opus".into(),
            permission_mode: PermissionMode::Ask,
            parent: None,
            task: None,
            max_children: None,
            failover_pin: None,
        }
    }

    #[test]
    fn a_turn_with_an_approval_folds_into_the_transcript() {
        let mut session = Session::default();
        session.apply(&events(vec![
            (None, created()),
            (
                None,
                EventBody::TurnStarted {
                    turn_id: TurnId::new("t1"),
                },
            ),
            (
                Some("alice"),
                item(
                    "i1",
                    ItemBody::UserMessage {
                        text: "Run the tests.".into(),
                        attachments: Vec::new(),
                    },
                ),
            ),
            (
                None,
                item(
                    "i2",
                    ItemBody::ToolCall {
                        name: "Bash".into(),
                        input: serde_json::json!({"command": "cargo test"}),
                    },
                ),
            ),
            (
                None,
                EventBody::ApprovalRequested {
                    approval_id: ApprovalId::new("a1"),
                    turn_id: TurnId::new("t1"),
                    tool_call_id: ItemId::new("i2"),
                    summary: "Run cargo test".into(),
                    routed_to: Route::User,
                    reason: None,
                },
            ),
        ]));
        assert_eq!(session.title(), "app · herder/api");
        assert!(session.running());
        assert_eq!(session.approvals.len(), 1);
        assert_eq!(
            session.tool_approval(&ItemId::new("i2")),
            Some(ToolApproval::Pending)
        );

        session.apply(&events(vec![]));
        let mut rest = events(vec![
            (
                Some("alice"),
                EventBody::ApprovalResolved {
                    approval_id: ApprovalId::new("a1"),
                    decision: ApprovalDecision::Allow.into(),
                    answered_by: Answerer::User,
                },
            ),
            (
                None,
                item(
                    "i3",
                    ItemBody::ToolResult {
                        call_id: ItemId::new("i2"),
                        output: "ok".into(),
                        is_error: false,
                    },
                ),
            ),
            (
                None,
                EventBody::TurnCompleted {
                    turn_id: TurnId::new("t1"),
                },
            ),
        ]);
        // Later events than the first update's.
        for event in &mut rest.events {
            event.at = Timestamp::from_second(event.at.as_second() + 70).unwrap_or_default();
        }
        session.apply(&rest);
        assert!(!session.running());
        assert!(session.approvals.is_empty());
        assert_eq!(session.result(&ItemId::new("i2")), Some(("ok", false)));
        // Called at 1003, its result came at 1071.
        assert_eq!(session.took(&ItemId::new("i2")), Some(68));
        assert_eq!(
            session.tool_approval(&ItemId::new("i2")),
            Some(ToolApproval::Allowed)
        );
        let tail: Vec<&Entry> = session.entries.iter().rev().take(3).collect();
        assert_eq!(
            tail[2],
            &Entry::Resolved {
                approval: true,
                text: "allowed Bash · by you".into()
            }
        );
        assert_eq!(
            tail[0],
            &Entry::TurnEnded {
                took: Some(71),
                interrupted: false,
                account: "claude-main".into(),
                model: "opus".into()
            }
        );
    }

    #[test]
    fn switches_failures_and_questions_read_as_the_tui_says_them() {
        let mut session = Session::default();
        session.apply(&events(vec![
            (None, created()),
            (
                None,
                EventBody::TurnStarted {
                    turn_id: TurnId::new("t1"),
                },
            ),
            (
                None,
                EventBody::QuestionAsked {
                    question_id: QuestionId::new("q1"),
                    turn_id: TurnId::new("t1"),
                    text: "Which heading level?".into(),
                    choices: vec!["h2".into(), "h1".into()],
                    routed_to: Route::Primary,
                    reason: None,
                },
            ),
            (
                None,
                EventBody::QuestionEscalated {
                    question_id: QuestionId::new("q1"),
                    reason: EscalationReason::Timeout,
                    note: Some("ask them".into()),
                },
            ),
        ]));
        assert_eq!(session.questions[0].routed_to, Route::User);
        assert_eq!(session.questions[0].note.as_deref(), Some("ask them"));
        session.apply(&events(vec![
            (
                None,
                EventBody::TurnFailed {
                    turn_id: TurnId::new("t1"),
                    error: TurnError {
                        class: ErrorClass::LimitReached,
                        message: "5h limit".into(),
                    },
                },
            ),
            (
                None,
                EventBody::AccountSwitched {
                    account_id: AccountId::new("claude-alt"),
                },
            ),
            (
                Some("alice"),
                EventBody::ProviderSwitched {
                    provider: Provider::Codex,
                    account_id: AccountId::new("codex-work"),
                    model: "gpt-5".into(),
                },
            ),
            (
                Some("alice"),
                EventBody::PermissionModeChanged {
                    mode: PermissionMode::AutoEdit,
                },
            ),
            (
                Some("alice"),
                EventBody::SessionForked {
                    from_session: SessionId::new("s0"),
                    from_host: HostId::new("laptop"),
                },
            ),
        ]));
        // The failed turn took its question with it.
        assert!(session.questions.is_empty());
        assert_eq!(session.provider, Some(Provider::Codex));
        assert_eq!(session.model, "gpt-5");
        assert_eq!(session.permission_mode, PermissionMode::AutoEdit);
        let switches: Vec<&str> = session
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Switch(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            switches,
            [
                "failed over to claude-alt: the last account hit its limit",
                "switched to codex-work · gpt-5 (transcript replayed)",
                "mode set to auto_edit",
                "switched to this machine (from laptop)",
            ]
        );
    }

    #[test]
    fn durations_read_as_in_the_tui() {
        assert_eq!(duration(4), "4s");
        assert_eq!(duration(62), "1m 02s");
        assert_eq!(duration(7500), "2h 05m");
    }
}
