//! Under the open transcript: the pending approval or question, then the composer.

use herder_protocol::{Route, SessionStatus};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::{App, Focus};
use crate::session::Session;

/// Most lines the composer grows to before it scrolls.
const MAX_LINES: usize = 6;

/// Splits the main pane into the transcript and, below it, the controls.
pub(super) fn split(area: Rect, app: &App) -> (Rect, Option<Rect>) {
    let Some(session) = app.open_session() else {
        return (area, None);
    };
    let width = usize::from(area.width.saturating_sub(2)).max(8);
    let mut height = prompt(session, width).map_or(0, |lines| lines.len() + 2);
    if session.status != SessionStatus::Archived {
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

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App) {
    let Some(session) = app.open_session() else {
        return;
    };
    let width = usize::from(area.width.saturating_sub(2)).max(8);
    let prompt_lines = prompt(session, width);
    let archived = session.status == SessionStatus::Archived;
    let prompt_height = prompt_lines.as_ref().map_or(0, |lines| lines.len() + 2);
    let prompt_height = u16::try_from(prompt_height).unwrap_or(u16::MAX);
    let [prompt_area, composer_area] =
        Layout::vertical([Constraint::Length(prompt_height), Constraint::Fill(1)]).areas(area);
    if let Some(lines) = prompt_lines {
        let title = if session.approvals.is_empty() {
            " question "
        } else {
            " approval needed "
        };
        let waiting = session.approvals.len() + session.questions.len();
        let mut block = Block::bordered()
            .border_style(Style::new().fg(Color::Magenta))
            .title(Line::styled(title, attention()));
        if waiting > 1 {
            block = block.title(
                Line::styled(format!(" +{} more ", waiting - 1), super::dim()).right_aligned(),
            );
        }
        frame.render_widget(Paragraph::new(lines).block(block), prompt_area);
    }
    if archived {
        return;
    }
    let title = composer_title(session);
    let focused = app.focus == Focus::Composer;
    let error = app
        .open
        .as_ref()
        .and_then(|key| app.compose.errors.get(key))
        .cloned();
    let bottom = match error {
        Some(error) => Line::styled(format!(" {error} "), Style::new().fg(Color::Red)),
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
    editor.set_cursor_style(if focused {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new()
    });
    editor.set_block(block);
    frame.render_widget(&*editor, composer_area);
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

/// The pending approval, else question, as lines `width` wide; `None` when nothing waits.
fn prompt(session: &Session, width: usize) -> Option<Vec<Line<'static>>> {
    let mut out = Vec::new();
    let keys = |text: &'static str| Line::styled(text, super::dim());
    if let Some(approval) = session.approvals.first() {
        wrap(&mut out, &approval.summary, super::bold(), width);
        if approval.routed_to == Route::Primary {
            out.push(keys(
                "asked the primary session first; y allow · n deny to answer yourself",
            ));
        } else {
            out.push(keys("y allow · n deny"));
        }
        return Some(out);
    }
    let question = session.questions.first()?;
    wrap(&mut out, &question.text, super::bold(), width);
    for (at, choice) in question.choices.iter().enumerate() {
        wrap(
            &mut out,
            &format!("{}. {choice}", at + 1),
            Style::new(),
            width,
        );
    }
    let routed = if question.routed_to == Route::Primary {
        "asked the primary session first; "
    } else {
        ""
    };
    let hint = if question.choices.is_empty() {
        format!("{routed}type the answer below")
    } else {
        format!(
            "{routed}1-{} pick · or type an answer below",
            question.choices.len()
        )
    };
    out.push(Line::styled(hint, super::dim()));
    Some(out)
}

fn wrap(out: &mut Vec<Line<'static>>, text: &str, style: Style, width: usize) {
    for line in text.lines() {
        for part in textwrap::wrap(line, width) {
            out.push(Line::styled(part.into_owned(), style));
        }
    }
}

fn attention() -> Style {
    Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD)
}

/// The session list's badge for a session waiting on the user, instead of its status.
pub(super) fn waiting(session: &Session) -> Option<(&'static str, Style)> {
    if !session.approvals.is_empty() {
        Some(("approve?", attention()))
    } else if !session.questions.is_empty() {
        Some(("question", attention()))
    } else {
        None
    }
}
