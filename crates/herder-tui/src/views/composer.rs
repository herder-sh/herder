//! Under the open transcript: the pending approval or question, then the composer.

use herder_protocol::{EscalationReason, Route, SessionStatus};
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
    let mut height = prompt(session, width, "").map_or(0, |lines| lines.len() + 2);
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

/// `compact`, on a narrow screen, keeps the hints short.
pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App, compact: bool) {
    let Some(session) = app.open_session() else {
        return;
    };
    let width = usize::from(area.width.saturating_sub(2)).max(8);
    // Answer keys go to the composer while it has them: say how to leave it first.
    let leave = match app.focus {
        Focus::Composer if app.compose.editor.is_empty() => "⌫, then ",
        Focus::Composer => "Esc, then ",
        _ => "",
    };
    let prompt_lines = prompt(session, width, leave);
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
/// `esc` says how to leave the composer first, while it has the keys.
fn prompt(session: &Session, width: usize, esc: &str) -> Option<Vec<Line<'static>>> {
    let mut out = Vec::new();
    if let Some(approval) = session.approvals.first() {
        wrap(&mut out, &approval.summary, super::bold(), width);
        escalation(&mut out, approval.reason, approval.note.as_deref(), width);
        let hint = if approval.routed_to == Route::Primary {
            format!("asked the primary session first; {esc}y allow · n deny to answer yourself")
        } else {
            format!("{esc}y allow · n deny")
        };
        out.push(Line::styled(hint, super::dim()));
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
    escalation(&mut out, question.reason, question.note.as_deref(), width);
    let routed = if question.routed_to == Route::Primary {
        "asked the primary session first; "
    } else {
        ""
    };
    let hint = if question.choices.is_empty() {
        format!("{routed}type the answer below")
    } else {
        format!(
            "{routed}{esc}1-{} pick · or type an answer below",
            question.choices.len()
        )
    };
    out.push(Line::styled(hint, super::dim()));
    Some(out)
}

/// Why a child's request is the user's, and what its primary session said, when known.
fn escalation(
    out: &mut Vec<Line<'static>>,
    reason: Option<EscalationReason>,
    note: Option<&str>,
    width: usize,
) {
    if let Some(reason) = reason {
        wrap(
            out,
            crate::session::reason_text(reason),
            super::dim(),
            width,
        );
    }
    if let Some(note) = note {
        let style = Style::new().fg(Color::Cyan).add_modifier(Modifier::ITALIC);
        wrap(out, &format!("the primary says: {note}"), style, width);
    }
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
