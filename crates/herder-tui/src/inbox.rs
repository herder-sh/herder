//! The inbox: every approval and question waiting on the user, across every session of every
//! machine, newest first. A child's request is here once it is routed or escalated to the
//! user; while its primary session is asked first, it is the primary's. Drawing is in
//! `views::inbox`.

use herder_protocol::{
    Answer, ApprovalDecision, ApprovalId, CommandBody, QuestionId, Route, Timestamp,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::Style;
use ratatui_textarea::TextArea;

use crate::action::Action;
use crate::app::{App, Effect, Focus, Row};
use crate::compose::Origin;
use crate::session::{PendingApproval, PendingQuestion, Session, SessionKey};

/// What the inbox keys ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InboxAction {
    /// Show the inbox in the main pane, or leave it.
    Toggle,
    /// Move the selection by this many requests.
    Move(isize),
    /// Select the newest request.
    First,
    /// Select the oldest request.
    Last,
    /// Answer the selected approval.
    Approve(ApprovalDecision),
    /// Answer the selected question with a choice, from 0.
    Choose(u32),
    /// Start typing an answer to the selected question.
    StartAnswer,
    /// A key for the answer being typed.
    Key(KeyEvent),
    /// Send the typed answer.
    Submit,
    /// Drop the typed answer.
    Cancel,
    /// Open the selected request's session.
    OpenSession,
    /// Leave the inbox: back to the open transcript, else to the session list.
    Close,
}

/// The inbox's own state.
#[derive(Debug, Default)]
pub struct Inbox {
    /// The selected request, kept by identity so newer requests listed above it do not move
    /// the selection onto another.
    pub selected: Option<Request>,
    /// Where the selection was last; selects a neighbour once the selected request is answered.
    pub index: usize,
    /// The answer being typed to the selected question.
    pub answer: Option<TextArea<'static>>,
}

/// One request waiting on the user, by identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The asking session.
    pub key: SessionKey,
    /// Which of its requests.
    pub ask: Ask,
}

/// An approval request or a question of one session.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ask {
    /// An approval request.
    Approval(ApprovalId),
    /// A question.
    Question(QuestionId),
}

/// One request waiting on the user, with what the inbox shows of it.
#[derive(Clone, Copy, Debug)]
pub struct Waiting<'a> {
    /// The asking session.
    pub key: &'a SessionKey,
    /// The asking session's state.
    pub session: &'a Session,
    /// The request.
    pub what: What<'a>,
}

/// The request of a [`Waiting`].
#[derive(Clone, Copy, Debug)]
pub enum What<'a> {
    /// An approval request.
    Approval(&'a PendingApproval),
    /// A question.
    Question(&'a PendingQuestion),
}

impl Waiting<'_> {
    /// The request's identity.
    pub fn request(&self) -> Request {
        let ask = match self.what {
            What::Approval(approval) => Ask::Approval(approval.id.clone()),
            What::Question(question) => Ask::Question(question.id.clone()),
        };
        Request {
            key: self.key.clone(),
            ask,
        }
    }

    /// When it was put to the user.
    pub fn since(&self) -> Timestamp {
        match self.what {
            What::Approval(approval) => approval.since,
            What::Question(question) => question.since,
        }
    }
}

/// The inbox action a key asks for, if the inbox handles it in the app's current state.
pub fn for_key(key: KeyEvent, app: &App) -> Option<InboxAction> {
    if app.prs.prompt.is_some() {
        return None;
    }
    let inbox = app.focus == Focus::Inbox;
    if inbox && app.inbox.answer.is_some() {
        return Some(match key.code {
            KeyCode::Esc => InboxAction::Cancel,
            KeyCode::Backspace if app.inbox.answer.as_ref().is_some_and(|a| a.is_empty()) => {
                InboxAction::Cancel
            }
            KeyCode::Enter if key.modifiers.is_empty() => InboxAction::Submit,
            _ => InboxAction::Key(key),
        });
    }
    let action = match key.code {
        KeyCode::Char('I') => InboxAction::Toggle,
        KeyCode::Char('i') if inbox || app.focus == Focus::Sessions => InboxAction::Toggle,
        _ if !inbox => return None,
        KeyCode::Char('k') | KeyCode::Up => InboxAction::Move(-1),
        KeyCode::Char('j') | KeyCode::Down => InboxAction::Move(1),
        KeyCode::Char('g') | KeyCode::Home => InboxAction::First,
        KeyCode::Char('G') | KeyCode::End => InboxAction::Last,
        KeyCode::Char('y') => InboxAction::Approve(ApprovalDecision::Allow),
        KeyCode::Char('n') => InboxAction::Approve(ApprovalDecision::Deny),
        KeyCode::Char(digit @ '1'..='9') => InboxAction::Choose(u32::from(digit) - u32::from('1')),
        KeyCode::Enter | KeyCode::Char('a') => InboxAction::StartAnswer,
        KeyCode::Char('l' | 'o') | KeyCode::Right => InboxAction::OpenSession,
        KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('h') | KeyCode::Left => {
            InboxAction::Close
        }
        _ => return None,
    };
    Some(action)
}

impl App {
    /// Every request waiting on the user, newest first.
    pub fn waiting(&self) -> Vec<Waiting<'_>> {
        let mut list: Vec<Waiting<'_>> = self
            .sessions
            .iter()
            .flat_map(|(key, session)| {
                let approvals = session
                    .approvals
                    .iter()
                    .filter(|approval| approval.routed_to == Route::User)
                    .map(What::Approval);
                let questions = session
                    .questions
                    .iter()
                    .filter(|question| question.routed_to == Route::User)
                    .map(What::Question);
                approvals
                    .chain(questions)
                    .map(move |what| Waiting { key, session, what })
            })
            .collect();
        list.sort_by(|a, b| {
            b.since()
                .cmp(&a.since())
                .then_with(|| a.key.host_id.cmp(&b.key.host_id))
                .then_with(|| a.key.session_id.cmp(&b.key.session_id))
                .then_with(|| a.request().ask.cmp(&b.request().ask))
        });
        list
    }

    /// Index of the selected request in [`App::waiting`]: the one selected, else the one now
    /// where the selection last was.
    pub fn inbox_index(&self, list: &[Waiting<'_>]) -> usize {
        let selected = self.inbox.selected.as_ref().and_then(|selected| {
            list.iter()
                .position(|waiting| waiting.request() == *selected)
        });
        selected.unwrap_or(self.inbox.index.min(list.len().saturating_sub(1)))
    }

    /// The selected request, while the inbox has focus.
    pub(crate) fn selected_request(&self) -> Option<Waiting<'_>> {
        if self.focus != Focus::Inbox {
            return None;
        }
        let list = self.waiting();
        let at = self.inbox_index(&list);
        list.into_iter().nth(at)
    }

    pub(crate) fn select_request(&mut self, at: usize) {
        let list = self.waiting();
        let at = at.min(list.len().saturating_sub(1));
        let selected = list.get(at).map(Waiting::request);
        self.inbox.index = at;
        self.inbox.selected = selected;
    }

    /// Inserts pasted text into the answer being typed; whether there was one.
    pub(crate) fn paste_inbox(&mut self, text: &str) -> bool {
        match &mut self.inbox.answer {
            Some(answer) if self.focus == Focus::Inbox => {
                answer.insert_str(text.replace(['\r', '\n'], " "));
                true
            }
            _ => false,
        }
    }

    /// Carries out one inbox action.
    pub fn act_inbox(&mut self, action: InboxAction) -> Vec<Effect> {
        match action {
            InboxAction::Toggle if self.focus == Focus::Inbox => {
                return self.act_inbox(InboxAction::Close);
            }
            InboxAction::Toggle => {
                self.focus = Focus::Inbox;
                self.inbox.answer = None;
                let at = self.inbox_index(&self.waiting());
                self.select_request(at);
            }
            InboxAction::Close => {
                self.inbox.answer = None;
                self.focus = if self.open.is_some() {
                    Focus::Transcript
                } else {
                    Focus::Sessions
                };
            }
            InboxAction::Move(step) => {
                let at = self.inbox_index(&self.waiting());
                self.select_request(at.saturating_add_signed(step));
            }
            InboxAction::First => self.select_request(0),
            InboxAction::Last => self.select_request(usize::MAX),
            InboxAction::Approve(decision) => {
                let Some(waiting) = self.selected_request() else {
                    return Vec::new();
                };
                let What::Approval(approval) = waiting.what else {
                    self.notice = Some("this is a question: pick a number or press Enter".into());
                    return Vec::new();
                };
                let command = CommandBody::AnswerApproval {
                    session_id: waiting.key.session_id.clone(),
                    approval_id: approval.id.clone(),
                    decision,
                };
                return vec![send(waiting.key, command)];
            }
            InboxAction::Choose(index) => {
                let Some(waiting) = self.selected_request() else {
                    return Vec::new();
                };
                let What::Question(question) = waiting.what else {
                    return Vec::new();
                };
                if usize::try_from(index).map_or(true, |at| at >= question.choices.len()) {
                    return Vec::new();
                }
                let command = CommandBody::AnswerQuestion {
                    session_id: waiting.key.session_id.clone(),
                    question_id: question.id.clone(),
                    answer: Answer::Choice { index },
                };
                return vec![send(waiting.key, command)];
            }
            InboxAction::StartAnswer => match self.selected_request().map(|w| w.what) {
                Some(What::Question(_)) => {
                    let mut answer = TextArea::default();
                    answer.set_cursor_line_style(Style::new());
                    answer.set_placeholder_text("Type an answer…");
                    self.inbox.answer = Some(answer);
                }
                Some(What::Approval(_)) => {
                    self.notice = Some("an approval takes y allow or n deny".into());
                }
                None => {}
            },
            InboxAction::Key(key) => {
                if let Some(answer) = &mut self.inbox.answer {
                    answer.input(key);
                }
            }
            InboxAction::Cancel => self.inbox.answer = None,
            InboxAction::Submit => {
                let text = self
                    .inbox
                    .answer
                    .as_ref()
                    .map(|answer| answer.lines().join("\n"))
                    .unwrap_or_default();
                if text.trim().is_empty() {
                    return Vec::new();
                }
                let Some(waiting) = self.selected_request() else {
                    self.inbox.answer = None;
                    return Vec::new();
                };
                let What::Question(question) = waiting.what else {
                    self.inbox.answer = None;
                    return Vec::new();
                };
                let command = CommandBody::AnswerQuestion {
                    session_id: waiting.key.session_id.clone(),
                    question_id: question.id.clone(),
                    answer: Answer::Text { text },
                };
                let effect = send(waiting.key, command);
                self.inbox.answer = None;
                return vec![effect];
            }
            InboxAction::OpenSession => {
                let Some(key) = self.selected_request().map(|w| w.key.clone()) else {
                    return Vec::new();
                };
                self.inbox.answer = None;
                self.reveal(&key);
                self.choose_row(Row::Session { key, depth: 0 });
                return self.act(Action::Open);
            }
        }
        Vec::new()
    }
}

/// A command answering a request of `key`'s session; a refusal shows on the status line.
fn send(key: &SessionKey, command: CommandBody) -> Effect {
    Effect::Send {
        host_id: key.host_id.clone(),
        command,
        origin: Origin::Session(key.clone()),
    }
}

#[cfg(test)]
mod tests {
    use herder_protocol::{Answerer, EventBody, SessionId};
    use ratatui::crossterm::event::KeyModifiers;

    use super::*;
    use crate::app::Msg;
    use crate::fake::{self, at, key, question, to_primary, type_text, update};

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn listed(app: &App) -> Vec<Request> {
        app.waiting().iter().map(Waiting::request).collect()
    }

    fn q1() -> Request {
        Request {
            key: key("h1", "s3"),
            ask: Ask::Question(QuestionId::new("q1")),
        }
    }

    fn a1() -> Request {
        Request {
            key: key("h1", "s1"),
            ask: Ask::Approval(ApprovalId::new("a1")),
        }
    }

    #[test]
    fn a_childs_question_reaches_the_inbox_only_once_escalated() {
        let mut app = fake::tree();
        let asked = to_primary(question("q1", "Which port?", &[]));
        fake::feed(&mut app, "h1", "s3", update("s3", 3, vec![asked], vec![]));
        assert!(app.waiting().is_empty(), "the primary is asked first");

        let escalation = EventBody::QuestionEscalated {
            question_id: QuestionId::new("q1"),
            reason: herder_protocol::EscalationReason::Timeout,
            note: None,
        };
        fake::feed(
            &mut app,
            "h1",
            "s3",
            update("s3", 4, vec![escalation], vec![]),
        );
        assert_eq!(listed(&app), [q1()]);
    }

    #[test]
    fn the_inbox_lists_every_machines_requests_newest_first() {
        let mut app = fake::escalated();
        let mut machines = app.machines.clone();
        machines.push(fake::machine("h2", "laptop", &["x1"]));
        app.update(Msg::Machines(machines));
        let asked = question("q9", "Ship it?", &["yes", "no"]);
        fake::feed(
            &mut app,
            "h2",
            "x1",
            at(update("x1", 1, vec![asked], vec![]), 120),
        );
        let q9 = Request {
            key: key("h2", "x1"),
            ask: Ask::Question(QuestionId::new("q9")),
        };
        // The escalated question counts from its escalation, at 200 s, not its asking.
        assert_eq!(listed(&app), [q1(), a1(), q9]);
    }

    #[test]
    fn the_selection_stays_on_its_request_when_a_newer_one_arrives() {
        let mut app = fake::escalated();
        press(&mut app, KeyCode::Char('I'));
        assert_eq!(app.focus, Focus::Inbox);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.inbox.selected, Some(a1()));

        let newer = question("q2", "Rename it?", &[]);
        fake::feed(
            &mut app,
            "h1",
            "s1",
            at(update("s1", 5, vec![newer], vec![]), 300),
        );
        // `y` still answers the approval, not the question now listed above it.
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            [send(
                &key("h1", "s1"),
                CommandBody::AnswerApproval {
                    session_id: SessionId::new("s1"),
                    approval_id: ApprovalId::new("a1"),
                    decision: ApprovalDecision::Allow,
                }
            )]
        );

        // Once answered, the selection moves to the request now in its place.
        let resolved = EventBody::ApprovalResolved {
            approval_id: ApprovalId::new("a1"),
            decision: herder_protocol::ApprovalOutcome::Allow,
            answered_by: Answerer::User,
        };
        fake::feed(
            &mut app,
            "h1",
            "s1",
            at(update("s1", 6, vec![resolved], vec![]), 301),
        );
        let list = app.waiting();
        assert_eq!(list.len(), 2);
        assert_eq!(list[app.inbox_index(&list)].request(), q1());
    }

    /// The node's "Done when": an escalated child question is answered from the inbox.
    #[test]
    fn an_escalated_child_question_is_answered_from_the_inbox() {
        let mut app = fake::escalated();
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(app.inbox.selected, Some(q1()));
        let answer = |answer| {
            vec![send(
                &key("h1", "s3"),
                CommandBody::AnswerQuestion {
                    session_id: SessionId::new("s3"),
                    question_id: QuestionId::new("q1"),
                    answer,
                },
            )]
        };
        // A choice by its number; only listed choices are answers, and y is not one.
        assert_eq!(press(&mut app, KeyCode::Char('3')), []);
        assert_eq!(press(&mut app, KeyCode::Char('y')), []);
        assert_eq!(
            press(&mut app, KeyCode::Char('1')),
            answer(Answer::Choice { index: 0 })
        );

        // Or typed: Enter opens the answer, keys type into it, even the inbox's own.
        press(&mut app, KeyCode::Enter);
        type_text(&mut app, "use 9000, ");
        app.update(Msg::Paste("not\nthose".into()));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            answer(Answer::Text {
                text: "use 9000, not those".into()
            })
        );
        assert!(app.inbox.answer.is_none());

        let answered = EventBody::QuestionAnswered {
            question_id: QuestionId::new("q1"),
            answer: Answer::Text {
                text: "use 9000".into(),
            },
            answered_by: Answerer::User,
        };
        fake::feed(
            &mut app,
            "h1",
            "s3",
            update("s3", 7, vec![answered], vec![]),
        );
        assert_eq!(listed(&app), [a1()]);
    }

    #[test]
    fn esc_drops_a_typed_answer_and_then_leaves_the_inbox() {
        let mut app = fake::escalated();
        press(&mut app, KeyCode::Char('I'));
        press(&mut app, KeyCode::Enter);
        type_text(&mut app, "q");
        press(&mut app, KeyCode::Esc);
        assert!(app.inbox.answer.is_none());
        assert_eq!(app.focus, Focus::Inbox);
        // An approval takes no typed answer.
        press(&mut app, KeyCode::Char('G'));
        press(&mut app, KeyCode::Enter);
        assert!(app.inbox.answer.is_none());
        assert!(app.notice.is_some());
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Sessions);
    }

    #[test]
    fn opening_a_request_unfolds_its_task_and_opens_the_child() {
        let mut app = fake::escalated();
        app.folded.insert(key("h1", "s2"));
        press(&mut app, KeyCode::Char('I'));
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.open, Some(key("h1", "s3")));
        assert_eq!(app.focus, Focus::Transcript);
        assert!(app.folded.is_empty());
        assert_eq!(
            app.selected().as_ref().and_then(Row::session),
            Some(&key("h1", "s3"))
        );
        // From the transcript, `i` writes; `I` opens the inbox again.
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(app.focus, Focus::Composer);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('I'));
        assert_eq!(app.focus, Focus::Inbox);
        press(&mut app, KeyCode::Char('I'));
        assert_eq!(app.focus, Focus::Transcript);
    }

    #[test]
    fn a_refused_answer_shows_on_the_status_line() {
        let mut app = fake::escalated();
        press(&mut app, KeyCode::Char('I'));
        app.update(Msg::Sent {
            origin: Origin::Session(key("h1", "s3")),
            result: Err("the question was already answered".into()),
        });
        assert_eq!(
            app.notice.as_deref(),
            Some("the question was already answered")
        );
    }
}
