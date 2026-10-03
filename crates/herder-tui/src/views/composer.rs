//! Under the open transcript: the pending approval or question ([`super::request`]), then the
//! composer. An approval takes the composer's place; a question shows it while an answer is
//! typed. A tap on the composer writes in it.

use herder_protocol::{Route, SessionStatus};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::action::Action;
use crate::app::{App, Focus};
use crate::compose::Act;
use crate::mouse::{Click, Hits};
use crate::session::Session;

/// Most lines the composer grows to before it scrolls.
const MAX_LINES: usize = 6;

/// Splits the main pane into the transcript and, below it, the controls.
pub(super) fn split(area: Rect, app: &App) -> (Rect, Option<Rect>) {
    let Some(session) = app.open_session() else {
        return (area, None);
    };
    let width = usize::from(area.width.saturating_sub(2)).max(8);
    let mut height = usize::from(super::request::height(app, area.width, false));
    if app
        .open
        .as_ref()
        .and_then(|key| app.read_only(key))
        .is_some()
    {
        height += 3;
    } else if session.status != SessionStatus::Archived && composer_shown(app) {
        height += composer_lines(app, width) + 2;
    }
    // The transcript keeps at least half the pane.
    let height = u16::try_from(height)
        .unwrap_or(u16::MAX)
        .min(area.height / 2);
    if height == 0 {
        return (area, None);
    }
    let [transcript, controls] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(height)]).areas(area);
    (transcript, Some(controls))
}

/// `compact`, on a narrow screen, keeps the hints short.
pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App, compact: bool, hits: &mut Hits) {
    let Some(session) = app.open_session() else {
        return;
    };
    let archived = session.status == SessionStatus::Archived;
    let request_height = super::request::height(app, area.width, compact);
    let [request_area, composer_area] =
        Layout::vertical([Constraint::Length(request_height), Constraint::Fill(1)]).areas(area);
    super::request::draw(frame, request_area, app, compact, hits);
    if let Some((text, recover)) = app.open.as_ref().and_then(|key| app.read_only(key)) {
        let block = Block::bordered().border_style(super::dim());
        let style = if recover {
            Style::new().fg(Color::Red)
        } else {
            super::dim()
        };
        frame.render_widget(
            Paragraph::new(Line::styled(format!(" {text}"), style)).block(block),
            composer_area,
        );
        if recover {
            hits.click(composer_area, Click::Act(Action::OpenRecover));
        }
        return;
    }
    if archived || !composer_shown(app) {
        return;
    }
    let title = composer_title(session);
    let placeholder = if session.questions.is_empty() {
        "Write a prompt…"
    } else {
        "Type an answer…"
    };
    let focused = app.focus == Focus::Composer;
    let error = app
        .open
        .as_ref()
        .and_then(|key| app.compose.errors.get(key))
        .cloned();
    let bottom = match error {
        Some(error) => Line::styled(format!(" {error} "), Style::new().fg(Color::Red)),
        None if focused && compact => Line::styled(" Enter send · ⌫ leave ", super::dim()),
        None if focused => Line::styled(
            " Enter send · Alt-Enter new line · Esc leave ",
            super::dim(),
        ),
        None => Line::styled(" i to write ", super::dim()),
    };
    let block = Block::bordered()
        .border_style(super::border(app, Focus::Composer))
        .title(title)
        .title_bottom(bottom.right_aligned());
    let editor = &mut app.compose.editor;
    editor.set_placeholder_text(placeholder);
    editor.set_cursor_style(if focused {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new()
    });
    editor.set_block(block);
    frame.render_widget(&*editor, composer_area);
    hits.click(composer_area, Click::Act(Action::Compose(Act::Write)));
    super::palette::slash(frame, composer_area, app, hits);
}

/// Whether the composer shows: not while an approval takes its place, nor while a question
/// waits until an answer is being typed.
fn composer_shown(app: &App) -> bool {
    match app.pending() {
        None => true,
        Some(crate::request::Pending::Approval(_)) => false,
        Some(crate::request::Pending::Question(_)) => app.focus == Focus::Composer,
    }
}

/// What the composer's text will do: answer the pending question, or prompt the agent, queued
/// behind the running turn.
fn composer_title(session: &Session) -> Line<'static> {
    if !session.questions.is_empty() {
        return Line::styled(" answer ", attention());
    }
    let mut spans = vec![Span::raw(" message ")];
    if !session.queued.is_empty() {
        spans.push(Span::styled(
            format!("· {} queued ", session.queued.len()),
            Style::new().fg(Color::Yellow),
        ));
    } else if session.turn.is_some() {
        spans.push(Span::styled("· queues behind the turn ", super::dim()));
    }
    Line::from(spans)
}

/// Lines the composer's text takes, wrapped to `width`, at least one and at most
/// [`MAX_LINES`].
fn composer_lines(app: &App, width: usize) -> usize {
    let lines: usize = app
        .compose
        .editor
        .lines()
        .iter()
        .map(|line| line.chars().count().div_ceil(width).max(1))
        .sum();
    lines.clamp(1, MAX_LINES)
}

fn attention() -> Style {
    Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD)
}

/// [`waiting`] as one glyph, for compact rows.
pub(super) fn waiting_glyph(session: &Session) -> Option<(&'static str, Style)> {
    waiting(session).map(|(label, style)| (if label == "question" { "?" } else { "!" }, style))
}

/// The session list's badge for a session waiting on the user, instead of its status. A
/// child's request put to its primary session first does not wait on the user yet.
pub(super) fn waiting(session: &Session) -> Option<(&'static str, Style)> {
    let user = |route: &Route| *route == Route::User;
    if session.approvals.iter().any(|a| user(&a.routed_to)) {
        Some(("approve?", attention()))
    } else if session.questions.iter().any(|q| user(&q.routed_to)) {
        Some(("question", attention()))
    } else {
        None
    }
}
