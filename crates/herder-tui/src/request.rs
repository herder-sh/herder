//! The open session's pending request, in place of its prompt: the approval or question it
//! waits on, its answers chosen with ←/→ (or h/l) and Enter, or `y`/`n` and digits at once;
//! `f` shows it full screen, for a long command or diff.
//!
//! The oldest approval comes first, then the oldest question. The answer the arrows rest on
//! starts at `allow`, or at "type an answer" for a question, and is kept per request.

use herder_protocol::ApprovalDecision;
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::action::Action;
use crate::app::{App, Effect, Focus};
use crate::compose::Act;
use crate::session::{PendingApproval, PendingQuestion};

/// A pending request of the open session.
#[derive(Clone, Copy, Debug)]
pub enum Pending<'a> {
    Approval(&'a PendingApproval),
    Question(&'a PendingQuestion),
}

impl Pending<'_> {
    /// Identifies the request, so a choice made on one is not kept for the next.
    fn id(&self) -> String {
        match self {
            Pending::Approval(approval) => format!("approval {}", approval.id.as_str()),
            Pending::Question(question) => format!("question {}", question.id.as_str()),
        }
    }

    /// Answers there are to move between: allow and deny; each choice and "type an answer".
    pub fn answers(&self) -> usize {
        match self {
            Pending::Approval(_) => 2,
            Pending::Question(question) => question.choices.len() + 1,
        }
    }

    /// The answer the arrows start at: allow, or "type an answer".
    fn first(&self) -> usize {
        match self {
            Pending::Approval(_) => 0,
            Pending::Question(question) => question.choices.len(),
        }
    }
}

/// Where the arrows rest, and whether the request is shown full screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestView {
    /// The request `selected` is for.
    id: Option<String>,
    /// The answer the arrows rest on.
    selected: usize,
    /// Shown full screen.
    pub full: bool,
    /// The first line shown full screen.
    pub scroll: usize,
}

/// Input to the request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// Move to the previous (-1) or next (1) answer.
    Move(isize),
    /// Give the answer the arrows rest on.
    Confirm,
    /// Show it full screen, or stop.
    Full,
    /// Scroll the full-screen view by this many lines.
    Scroll(isize),
}

/// The action a key asks for while the request shows full screen.
pub fn full_key(key: KeyEvent) -> Option<Action> {
    let action = match key.code {
        KeyCode::Char('y') => Action::Compose(Act::Approve(ApprovalDecision::Allow)),
        KeyCode::Char('n') => Action::Compose(Act::Approve(ApprovalDecision::Deny)),
        KeyCode::Char(digit @ '1'..='9') => {
            Action::Compose(Act::Choose(u32::from(digit) - u32::from('1')))
        }
        KeyCode::Char('j') | KeyCode::Down => Action::Request(Input::Scroll(1)),
        KeyCode::Char('k') | KeyCode::Up => Action::Request(Input::Scroll(-1)),
        KeyCode::Char(' ') | KeyCode::PageDown => Action::Request(Input::Scroll(10)),
        KeyCode::Char('b') | KeyCode::PageUp => Action::Request(Input::Scroll(-10)),
        KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('f' | 'q') => {
            Action::Request(Input::Full)
        }
        _ => return None,
    };
    Some(action)
}

/// The action a key asks for in the open session while it waits on a request: the arrows,
/// Enter and `f`; `None` for the keys the transcript keeps.
pub fn key(key: KeyEvent) -> Option<Action> {
    let input = match key.code {
        KeyCode::Left | KeyCode::Char('h') => Input::Move(-1),
        KeyCode::Right | KeyCode::Char('l') => Input::Move(1),
        KeyCode::Enter => Input::Confirm,
        KeyCode::Char('f') => Input::Full,
        _ => return None,
    };
    Some(Action::Request(input))
}

impl App {
    /// The request the open session waits on: its oldest approval, else its oldest question.
    pub fn pending(&self) -> Option<Pending<'_>> {
        let session = self.open_session()?;
        session
            .approvals
            .first()
            .map(Pending::Approval)
            .or_else(|| session.questions.first().map(Pending::Question))
    }

    /// Whether the open session waits on an approval, which takes the prompt's place and
    /// its keys.
    pub fn approval_pending(&self) -> bool {
        matches!(self.pending(), Some(Pending::Approval(_)))
    }

    /// The answer the arrows rest on.
    pub fn request_cursor(&self) -> usize {
        let Some(pending) = self.pending() else {
            return 0;
        };
        if self.request.id.as_deref() == Some(pending.id().as_str()) {
            self.request.selected.min(pending.answers() - 1)
        } else {
            pending.first()
        }
    }

    /// Carries out one input to the request.
    pub(crate) fn request_input(&mut self, input: Input) -> Vec<Effect> {
        let Some(pending) = self.pending() else {
            self.request.full = false;
            return Vec::new();
        };
        let (id, answers) = (pending.id(), pending.answers());
        let question = matches!(pending, Pending::Question(_));
        let at = self.request_cursor();
        match input {
            Input::Move(by) => {
                self.request.id = Some(id);
                self.request.selected = at.saturating_add_signed(by).min(answers - 1);
            }
            Input::Full => {
                self.request.full = !self.request.full;
                self.request.scroll = 0;
            }
            Input::Scroll(by) => {
                // The view clamps it to its last page as it draws.
                self.request.scroll = self.request.scroll.saturating_add_signed(by);
            }
            Input::Confirm => {
                self.request.full = false;
                let act = match (question, at) {
                    (false, 0) => Act::Approve(ApprovalDecision::Allow),
                    (false, _) => Act::Approve(ApprovalDecision::Deny),
                    (true, at) if at + 1 == answers => Act::Write,
                    (true, at) => Act::Choose(u32::try_from(at).unwrap_or(u32::MAX)),
                };
                return self.compose(act);
            }
        }
        Vec::new()
    }

    /// Whether keys in the open session go to its request: it waits on one and the
    /// transcript has them. An approval takes the prompt's place, so it takes the keys from
    /// the prompt as it arrives.
    pub fn answering(&self) -> bool {
        self.focus == Focus::Transcript && self.pending().is_some()
    }
}

#[cfg(test)]
mod tests {
    use herder_protocol::{Answer, CommandBody, QuestionId, SessionId};
    use ratatui::crossterm::event::KeyModifiers;

    use super::*;
    use crate::app::Msg;
    use crate::fake::{self, approval, question, started, update};

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn open_s2(bodies: Vec<herder_protocol::EventBody>) -> App {
        let mut app = fake::tree();
        fake::feed(&mut app, "h1", "s2", update("s2", 3, bodies, Vec::new()));
        press(&mut app, KeyCode::Enter);
        app
    }

    fn sent(effects: &[Effect]) -> Vec<CommandBody> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Send { command, .. } => Some(command.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn arrows_choose_an_approval_answer_and_enter_gives_it() {
        let mut app = open_s2(vec![
            started("turn-1"),
            approval("a1", "Bash: rm -rf target"),
        ]);
        assert_eq!(app.request_cursor(), 0);
        press(&mut app, KeyCode::Right);
        assert_eq!(app.request_cursor(), 1);
        // Past the last answer it stays; h goes back, l forward, as on a phone keyboard.
        press(&mut app, KeyCode::Right);
        assert_eq!(app.request_cursor(), 1);
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.request_cursor(), 0);
        press(&mut app, KeyCode::Char('l'));
        let effects = press(&mut app, KeyCode::Enter);
        assert!(
            matches!(
                sent(&effects).as_slice(),
                [CommandBody::AnswerApproval {
                    decision: ApprovalDecision::Deny,
                    ..
                }]
            ),
            "{effects:?}"
        );
    }

    #[test]
    fn the_prompt_an_approval_replaces_gives_its_keys_to_it() {
        let mut app = open_s2(vec![started("turn-1")]);
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(app.focus, Focus::Composer);
        fake::feed(
            &mut app,
            "h1",
            "s2",
            update("s2", 4, vec![approval("a1", "Bash: ls")], vec![]),
        );
        // y answers rather than types, and i does not reopen the prompt.
        let effects = press(&mut app, KeyCode::Char('y'));
        assert_eq!(sent(&effects).len(), 1, "{effects:?}");
        assert!(app.compose.editor.is_empty());
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('i'));
        assert_ne!(app.focus, Focus::Composer);
    }

    #[test]
    fn a_question_starts_at_typing_and_the_arrows_reach_its_choices() {
        let mut app = open_s2(vec![
            started("turn-1"),
            question("q1", "Which database?", &["SQLite", "Postgres"]),
        ]);
        assert_eq!(app.request_cursor(), 2);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus, Focus::Composer);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Left);
        let effects = press(&mut app, KeyCode::Enter);
        assert_eq!(
            sent(&effects),
            [CommandBody::AnswerQuestion {
                session_id: SessionId::new("s2"),
                question_id: QuestionId::new("q1"),
                answer: Answer::Choice { index: 1 },
            }]
        );
    }

    #[test]
    fn f_shows_the_request_full_screen_where_y_still_answers() {
        let long = (1..=40)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut app = open_s2(vec![started("turn-1"), approval("a1", &long)]);
        press(&mut app, KeyCode::Char('f'));
        assert!(app.request.full);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.request.scroll, 1);
        press(&mut app, KeyCode::Esc);
        assert!(!app.request.full);
        press(&mut app, KeyCode::Char('f'));
        let effects = press(&mut app, KeyCode::Char('y'));
        assert_eq!(sent(&effects).len(), 1);
    }
}
