//! The board in the main pane: one line per session with its work state and the PR that
//! decides it, its machine at the right; attention first.
//!
//! ```text
//!  board · 9 sessions · 6 to act on                                  attention first
//!
//! ▶ ◉ needs you          ask-port                                               box
//!   ✗ CI failed     #21  health                                                 box
//!   ● ready to merge #24 docs                                                laptop
//! ```

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::{App, Focus};
use crate::board::{Card, Work};
use crate::mouse::{Click, Hits, List as Rows, Wheel};
use crate::ui::Ui;
use crate::ui::list::{ListView, Row};
use crate::ui::state::{self, State};

/// The widest label, so the PR numbers line up.
const LABEL_WIDTH: usize = "changes requested".len();

/// `compact`, on a phone, leaves the heading to the phone's header and the machine out.
pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App, compact: bool, hits: &mut Hits) {
    let ui = app.ui();
    let cards = app.board();
    let to_act = cards
        .iter()
        .filter(|card| card.work < Work::Working)
        .count();
    let area = super::heading(
        frame,
        area,
        ui,
        "board",
        &format!("{} sessions · {to_act} to act on", cards.len()),
        "attention first",
        compact,
    );
    if cards.is_empty() {
        super::nothing(frame, area, ui, "No sessions yet: n starts one.");
        return;
    }
    let number_width = cards
        .iter()
        .filter_map(|card| card.pr)
        .map(|pr| pr.number.to_string().len() + 1)
        .max()
        .unwrap_or(0);
    let rows: Vec<Row> = cards
        .iter()
        .map(|card| row(app, ui, card, number_width, compact))
        .collect();
    let mut offset = 0;
    let placed = ListView::new(ui, rows)
        .select(Some(app.board_index(&cards)))
        .focused(app.focus == Focus::Board)
        .render(area, frame.buffer_mut(), &mut offset);
    hits.wheel(area, Wheel::Keys);
    for (at, rect) in placed {
        hits.click(rect, Click::Row(Rows::Board, at));
    }
}

/// One session: its work state's mark and words, the deciding PR's number, the title; its
/// machine at the right.
fn row<'a>(app: &App, ui: Ui, card: &Card<'_>, number_width: usize, compact: bool) -> Row<'a> {
    let style = style(ui, card.work);
    let number = card
        .pr
        .map_or_else(String::new, |pr| format!("#{}", pr.number));
    let label = card.work.label();
    let (label, gap) = if compact {
        (label.to_owned(), " ")
    } else {
        (format!("{label:<LABEL_WIDTH$}"), "  ")
    };
    let mut left = vec![
        Span::styled(ui.glyphs.state(mark(card.work)), style),
        Span::raw(" "),
        Span::styled(label, style),
        Span::raw(gap),
    ];
    if compact {
        if !number.is_empty() {
            left.push(Span::styled(number, ui.muted()));
            left.push(Span::raw(gap));
        }
    } else if number_width > 0 {
        left.push(Span::styled(format!("{number:<number_width$}"), ui.muted()));
        left.push(Span::raw(gap));
    }
    left.push(Span::styled(card.session.short_title(), ui.text()));
    let machine = if compact {
        String::new()
    } else {
        app.host_name(card.key).unwrap_or_default()
    };
    Row::item(Line::from(left)).right(Span::styled(machine, ui.muted()))
}

/// The status mark a work state borrows, so it reads without its colour too.
fn mark(work: Work) -> State {
    match work {
        Work::NeedsYou => State::NeedsYou,
        Work::CiFailed | Work::ChangesRequested | Work::Conflicting => State::Error,
        Work::ReadyToMerge => State::Done,
        Work::Working => State::Running,
        Work::WaitingOnCi => State::Waiting,
        Work::Idle | Work::PrOpen | Work::Merged => State::Idle,
    }
}

fn style(ui: Ui, work: Work) -> Style {
    let theme = ui.theme;
    match work {
        Work::NeedsYou => state::style(ui, State::NeedsYou),
        Work::CiFailed | Work::ChangesRequested | Work::Conflicting => Style::new().fg(theme.error),
        Work::ReadyToMerge => Style::new().fg(theme.success).add_modifier(Modifier::BOLD),
        Work::Working => state::style(ui, State::Running),
        Work::WaitingOnCi => Style::new().fg(theme.warning),
        Work::PrOpen => Style::new().fg(theme.pr_open),
        Work::Merged => Style::new().fg(theme.pr_merged),
        Work::Idle => ui.muted(),
    }
}
