//! What a row's status dot shows: a session's [`State`], rolled up from its subtree.
//!
//! A state is always a glyph and a colour, never colour alone: shapes differ on a phone, to
//! the colour blind and in the `ansi` theme.

use herder_protocol::SessionStatus;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::Ui;

/// A row's state, in the order [`super::glyphs::GlyphSet::states`] lists their marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum State {
    /// An approval or question waits on the user, or something below it does.
    NeedsYou,
    /// The last turn failed.
    Error,
    /// A turn finished since this client last looked.
    Done,
    /// A turn is running.
    Running,
    /// A prompt waits for its host to have capacity.
    Waiting,
    /// Nothing to do, and already seen.
    Idle,
    /// Put away; read-only.
    Archived,
    /// Taken over by another host.
    Moved,
    /// Not loaded, or a status newer than this build.
    Unknown,
}

/// Every state, in declared order.
pub const ALL: [State; 9] = [
    State::NeedsYou,
    State::Error,
    State::Done,
    State::Running,
    State::Waiting,
    State::Idle,
    State::Archived,
    State::Moved,
    State::Unknown,
];

impl State {
    /// The state of a session with `status`; `done` when a turn finished since the client
    /// last looked.
    pub fn of(status: SessionStatus, done: bool) -> Self {
        match status {
            SessionStatus::NeedsYou => Self::NeedsYou,
            SessionStatus::Error => Self::Error,
            SessionStatus::Idle if done => Self::Done,
            SessionStatus::Idle => Self::Idle,
            SessionStatus::Running => Self::Running,
            SessionStatus::WaitingForCapacity => Self::Waiting,
            SessionStatus::Archived => Self::Archived,
            SessionStatus::Moved => Self::Moved,
            SessionStatus::Unknown => Self::Unknown,
        }
    }

    /// Roll-up priority: a parent shows its highest-priority descendant.
    pub fn priority(self) -> u8 {
        match self {
            Self::NeedsYou => 6,
            Self::Error => 5,
            Self::Done => 4,
            Self::Running => 3,
            Self::Waiting => 2,
            Self::Idle => 1,
            Self::Archived | Self::Moved | Self::Unknown => 0,
        }
    }

    /// The state in words, as the details panel and phone header say it.
    pub fn label(self) -> &'static str {
        match self {
            Self::NeedsYou => "needs you",
            Self::Error => "error",
            Self::Done => "done",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Idle => "idle",
            Self::Archived => "archived",
            Self::Moved => "moved",
            Self::Unknown => "unknown",
        }
    }
}

/// The state a parent shows for `states`, its own and its subtree's: the highest priority,
/// the first of equals; [`State::Unknown`] for none.
pub fn rollup(states: impl IntoIterator<Item = State>) -> State {
    states
        .into_iter()
        .reduce(|shown, state| {
            if state.priority() > shown.priority() {
                state
            } else {
                shown
            }
        })
        .unwrap_or(State::Unknown)
}

/// A status dot: the state's glyph in its colour, bold when it needs the user.
pub fn dot(ui: Ui, state: State) -> Span<'static> {
    Span::styled(ui.glyphs.state(state), style(ui, state))
}

/// A status dot and the state in words: `◉ needs you`.
pub fn labelled(ui: Ui, state: State) -> Line<'static> {
    Line::from(vec![
        dot(ui, state),
        Span::styled(format!(" {}", state.label()), style(ui, state)),
    ])
}

/// The style of `state`'s glyph and words.
pub fn style(ui: Ui, state: State) -> Style {
    let style = Style::new().fg(ui.theme.state(state));
    if state == State::NeedsYou {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollup_takes_the_highest_priority() {
        use State::*;
        assert_eq!(rollup([Idle, Running, Done]), Done);
        assert_eq!(rollup([Done, Error]), Error);
        assert_eq!(rollup([Error, NeedsYou, Running]), NeedsYou);
        assert_eq!(rollup([Waiting, Idle]), Waiting);
        assert_eq!(rollup([Archived, Idle]), Idle);
        assert_eq!(rollup([Moved, Archived]), Moved);
        assert_eq!(rollup([]), Unknown);
        // Every priority is distinct above the read-only states.
        let mut priorities: Vec<u8> = ALL.iter().map(|s| s.priority()).collect();
        priorities.sort_unstable();
        priorities.dedup();
        assert_eq!(priorities, [0, 1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn status_dots() {
        use super::super::snapshot;
        snapshot::each("status-dots", |variant| {
            let ui = variant.ui();
            let mut lines: Vec<Line<'static>> = ALL
                .iter()
                .map(|state| {
                    let mut line = labelled(ui, *state);
                    line.spans.insert(0, Span::raw(" "));
                    line
                })
                .collect();
            let mut dots = vec![Span::raw(" ")];
            for state in ALL {
                dots.extend([dot(ui, state), Span::raw(" ")]);
            }
            lines.push(Line::from(dots));
            snapshot::lines(variant, 20, lines)
        });
    }

    #[test]
    fn statuses_map_to_states() {
        assert_eq!(State::of(SessionStatus::Idle, true), State::Done);
        assert_eq!(State::of(SessionStatus::Idle, false), State::Idle);
        assert_eq!(
            State::of(SessionStatus::WaitingForCapacity, false),
            State::Waiting
        );
        assert_eq!(State::of(SessionStatus::NeedsYou, true), State::NeedsYou);
        // The glyph table follows the declared order.
        for (at, state) in ALL.iter().enumerate() {
            assert_eq!(*state as usize, at);
        }
    }
}
