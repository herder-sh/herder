//! The board: every listed session with its work state, derived from its status and its linked
//! pull requests, attention first, so the user sees what needs them without asking each
//! agent. Archived and moved sessions are left out. Drawing is in `views::board`.

use herder_protocol::{CiStatus, Mergeable, PrState, PullRequest, ReviewStatus};
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::action::Action;
use crate::app::{App, Effect, Focus, Row};
use crate::session::{Session, SessionKey};
use crate::ui::state::State;

/// What a session's work waits on, in the order the board lists them: attention first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Work {
    /// An approval or question waits on the user, or the last turn failed.
    NeedsYou,
    /// An open PR's checks failed.
    CiFailed,
    /// A reviewer asked for changes on an open PR.
    ChangesRequested,
    /// An open PR conflicts with its base.
    Conflicting,
    /// An open PR passed its checks and merges cleanly.
    ReadyToMerge,
    /// Nothing runs and no PR is open or merged.
    Idle,
    /// A turn runs, or waits for its host to have capacity.
    Working,
    /// An open PR's checks still run.
    WaitingOnCi,
    /// A PR is open, but neither ready nor held up: a draft, or GitHub has not said yet.
    PrOpen,
    /// The PR merged.
    Merged,
}

impl Work {
    /// The state in words, as the board shows it.
    pub fn label(self) -> &'static str {
        match self {
            Self::NeedsYou => "needs you",
            Self::CiFailed => "CI failed",
            Self::ChangesRequested => "changes requested",
            Self::Conflicting => "conflicting",
            Self::ReadyToMerge => "ready to merge",
            Self::Idle => "idle, no PR",
            Self::Working => "working",
            Self::WaitingOnCi => "waiting on CI",
            Self::PrOpen => "PR open",
            Self::Merged => "merged",
        }
    }
}

/// The work state of a session in `state` with `prs`, and the PR that decides it: the one
/// whose own state comes first on the board. A running or asking session is working or needs
/// the user whatever its PRs say.
pub fn work(state: State, prs: &[PullRequest]) -> (Work, Option<&PullRequest>) {
    let pr = prs
        .iter()
        .filter_map(|pr| Some((pr_work(pr)?, pr)))
        .min_by_key(|(work, _)| *work);
    let by_pr = pr.map_or(Work::Idle, |(work, _)| work);
    let work = match state {
        State::NeedsYou | State::Error => Work::NeedsYou,
        State::Running | State::Waiting => Work::Working,
        State::Done | State::Idle | State::Archived | State::Moved | State::Unknown => by_pr,
    };
    (work, pr.map(|(_, pr)| pr))
}

/// What one PR says of the work; `None` for a closed one, which says nothing.
fn pr_work(pr: &PullRequest) -> Option<Work> {
    Some(match pr.state {
        PrState::Closed => return None,
        PrState::Merged => Work::Merged,
        PrState::Open | PrState::Draft if pr.ci == CiStatus::Failing => Work::CiFailed,
        PrState::Open | PrState::Draft if pr.review == ReviewStatus::ChangesRequested => {
            Work::ChangesRequested
        }
        PrState::Open | PrState::Draft if pr.mergeable == Mergeable::Conflicting => {
            Work::Conflicting
        }
        PrState::Open | PrState::Draft if pr.ci == CiStatus::Pending => Work::WaitingOnCi,
        PrState::Open if pr.ci == CiStatus::Passing && pr.mergeable == Mergeable::Clean => {
            Work::ReadyToMerge
        }
        PrState::Open | PrState::Draft => Work::PrOpen,
    })
}

/// One session on the board.
#[derive(Clone, Copy, Debug)]
pub struct Card<'a> {
    /// The session.
    pub key: &'a SessionKey,
    /// Its state.
    pub session: &'a Session,
    /// Its work state.
    pub work: Work,
    /// The PR that decides it, if any.
    pub pr: Option<&'a PullRequest>,
}

/// What the board keys ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoardAction {
    /// Show the board in the main pane, or leave it.
    Toggle,
    /// Move the selection by this many sessions.
    Move(isize),
    /// Select the first session.
    First,
    /// Select the last session.
    Last,
    /// Open the selected session.
    OpenSession,
    /// Leave the board: back to the open transcript, else to the session list.
    Close,
}

/// The board's own state.
#[derive(Debug, Default)]
pub struct Board {
    /// The selected session, kept by identity so sessions changing state do not move the
    /// selection onto another.
    pub selected: Option<SessionKey>,
    /// Where the selection was last; selects a neighbour once the selected session leaves.
    pub index: usize,
}

/// The board action a key asks for, if the board handles it in the app's current state.
pub fn for_key(key: KeyEvent, app: &App) -> Option<BoardAction> {
    if app.prs.prompt.is_some() {
        return None;
    }
    let board = app.focus == Focus::Board;
    let action = match key.code {
        KeyCode::Char('B') => BoardAction::Toggle,
        _ if !board => return None,
        KeyCode::Char('k') | KeyCode::Up => BoardAction::Move(-1),
        KeyCode::Char('j') | KeyCode::Down => BoardAction::Move(1),
        KeyCode::PageUp => BoardAction::Move(-10),
        KeyCode::PageDown => BoardAction::Move(10),
        KeyCode::Char('g') | KeyCode::Home => BoardAction::First,
        KeyCode::Char('G') | KeyCode::End => BoardAction::Last,
        KeyCode::Enter | KeyCode::Char('l' | 'o') | KeyCode::Right => BoardAction::OpenSession,
        KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('h') | KeyCode::Left => {
            BoardAction::Close
        }
        _ => return None,
    };
    Some(action)
}

impl App {
    /// Every listed session but the archived and moved ones, attention first; equals keep
    /// the session list's order.
    pub fn board(&self) -> Vec<Card<'_>> {
        let mut cards: Vec<Card<'_>> = self
            .all_rows()
            .iter()
            .filter_map(|row| self.sessions.get_key_value(row.session()?))
            .filter(|(key, _)| !matches!(self.state(key), State::Archived | State::Moved))
            .map(|(key, session)| {
                let (work, pr) = work(self.state(key), &session.prs);
                Card {
                    key,
                    session,
                    work,
                    pr,
                }
            })
            .collect();
        cards.sort_by_key(|card| card.work);
        cards
    }

    /// Index of the selected session in [`App::board`]: the one selected, else the one now
    /// where the selection last was.
    pub fn board_index(&self, cards: &[Card<'_>]) -> usize {
        let selected = self
            .board
            .selected
            .as_ref()
            .and_then(|selected| cards.iter().position(|card| card.key == selected));
        selected.unwrap_or(self.board.index.min(cards.len().saturating_sub(1)))
    }

    pub(crate) fn select_card(&mut self, at: usize) {
        let cards = self.board();
        let at = at.min(cards.len().saturating_sub(1));
        let selected = cards.get(at).map(|card| card.key.clone());
        self.board.index = at;
        self.board.selected = selected;
    }

    /// Carries out one board action.
    pub fn act_board(&mut self, action: BoardAction) -> Vec<Effect> {
        match action {
            BoardAction::Toggle if self.focus == Focus::Board => {
                return self.act_board(BoardAction::Close);
            }
            BoardAction::Toggle => {
                self.focus = Focus::Board;
                let at = self.board_index(&self.board());
                self.select_card(at);
            }
            BoardAction::Close => {
                self.focus = if self.open.is_some() {
                    Focus::Transcript
                } else {
                    Focus::Sessions
                };
            }
            BoardAction::Move(step) => {
                let at = self.board_index(&self.board());
                self.select_card(at.saturating_add_signed(step));
            }
            BoardAction::First => self.select_card(0),
            BoardAction::Last => self.select_card(usize::MAX),
            BoardAction::OpenSession => {
                let cards = self.board();
                let Some(key) = cards
                    .get(self.board_index(&cards))
                    .map(|card| card.key.clone())
                else {
                    return Vec::new();
                };
                self.reveal(&key);
                self.choose_row(Row::Session { key, depth: 0 });
                return self.act(Action::Open);
            }
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyModifiers;

    use super::*;
    use crate::app::Msg;
    use crate::fake::{self, key, pr};

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn open(ci: CiStatus, review: ReviewStatus, mergeable: Mergeable) -> PullRequest {
        let mut open = pr(1, "Open", PrState::Open);
        open.ci = ci;
        open.review = review;
        open.mergeable = mergeable;
        open
    }

    fn of(state: State, prs: &[PullRequest]) -> (Work, Option<u64>) {
        let (work, pr) = work(state, prs);
        (work, pr.map(|pr| pr.number))
    }

    #[test]
    fn a_session_without_prs_is_its_status() {
        assert_eq!(of(State::NeedsYou, &[]), (Work::NeedsYou, None));
        assert_eq!(of(State::Error, &[]), (Work::NeedsYou, None));
        assert_eq!(of(State::Running, &[]), (Work::Working, None));
        assert_eq!(of(State::Waiting, &[]), (Work::Working, None));
        assert_eq!(of(State::Idle, &[]), (Work::Idle, None));
        assert_eq!(of(State::Done, &[]), (Work::Idle, None));
        // A closed PR says nothing of the work.
        let closed = pr(4, "Closed", PrState::Closed);
        assert_eq!(of(State::Idle, &[closed]), (Work::Idle, None));
    }

    #[test]
    fn an_idle_sessions_open_pr_says_what_it_waits_on() {
        use {CiStatus as Ci, Mergeable as M, ReviewStatus as R};
        let idle = |pr: PullRequest| work(State::Idle, &[pr]).0;
        assert_eq!(
            idle(open(Ci::Failing, R::Approved, M::Clean)),
            Work::CiFailed
        );
        assert_eq!(
            idle(open(Ci::Passing, R::ChangesRequested, M::Clean)),
            Work::ChangesRequested
        );
        assert_eq!(
            idle(open(Ci::Passing, R::None, M::Conflicting)),
            Work::Conflicting
        );
        assert_eq!(
            idle(open(Ci::Pending, R::None, M::Clean)),
            Work::WaitingOnCi
        );
        assert_eq!(
            idle(open(Ci::Passing, R::Required, M::Clean)),
            Work::ReadyToMerge
        );
        // Not ready until GitHub says it merges cleanly, and never while a draft.
        assert_eq!(idle(open(Ci::Passing, R::None, M::Unknown)), Work::PrOpen);
        assert_eq!(idle(open(Ci::None, R::None, M::Clean)), Work::PrOpen);
        let mut draft = open(Ci::Passing, R::Approved, M::Clean);
        draft.state = PrState::Draft;
        assert_eq!(idle(draft), Work::PrOpen);
        assert_eq!(idle(pr(1, "Merged", PrState::Merged)), Work::Merged);
    }

    #[test]
    fn the_pr_that_needs_the_most_attention_decides() {
        let merged = pr(3, "Merged", PrState::Merged);
        let mut failing = open(CiStatus::Failing, ReviewStatus::None, Mergeable::Clean);
        failing.number = 5;
        let prs = [merged, failing];
        assert_eq!(of(State::Idle, &prs), (Work::CiFailed, Some(5)));
        // A running session is working on it, and the board names that PR.
        assert_eq!(of(State::Running, &prs), (Work::Working, Some(5)));
        assert_eq!(of(State::NeedsYou, &prs[..1]), (Work::NeedsYou, Some(3)));
    }

    #[test]
    fn the_board_lists_attention_first_without_archived_or_moved_sessions() {
        let app = fake::board();
        let listed: Vec<(String, Work)> = app
            .board()
            .iter()
            .map(|card| (card.key.session_id.to_string(), card.work))
            .collect();
        let expected = [
            ("b1", Work::NeedsYou),
            ("b2", Work::CiFailed),
            ("b3", Work::ChangesRequested),
            ("b4", Work::Conflicting),
            ("b5", Work::ReadyToMerge),
            ("b6", Work::Idle),
            ("b7", Work::Working),
            ("b8", Work::WaitingOnCi),
            ("b9", Work::Merged),
        ]
        .map(|(id, work)| (id.to_owned(), work));
        assert_eq!(listed, expected);
    }

    #[test]
    fn enter_opens_the_selected_session_and_esc_goes_back() {
        let mut app = fake::board();
        press(&mut app, KeyCode::Char('B'));
        assert_eq!(app.focus, Focus::Board);
        assert_eq!(app.board.selected, Some(key("h1", "b1")));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.board.selected, Some(key("h2", "b5")));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.open, Some(key("h2", "b5")));
        assert_eq!(
            app.selected().as_ref().and_then(Row::session),
            Some(&key("h2", "b5"))
        );
        // Back to the board, kept on its session, and out of it to the transcript.
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('B'));
        assert_eq!(app.focus, Focus::Board);
        assert_eq!(app.board.selected, Some(key("h2", "b5")));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Transcript);
    }

    #[test]
    fn the_selection_follows_its_session_when_its_state_changes() {
        let mut app = fake::board();
        press(&mut app, KeyCode::Char('B'));
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(app.board.selected, Some(key("h1", "b9")));
        // b9 starts a turn: it moves up among the working, and the selection with it.
        fake::feed(
            &mut app,
            "h1",
            "b9",
            fake::update(
                "b9",
                9,
                vec![fake::status(herder_protocol::SessionStatus::Running)],
                vec![],
            ),
        );
        let cards = app.board();
        let at = app.board_index(&cards);
        assert_eq!(cards[at].key, &key("h1", "b9"));
        assert_eq!(cards[at].work, Work::Working);
    }
}
