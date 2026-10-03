//! The open session's pending request, in place of its prompt ([`crate::ui::request`]), and
//! the same request full screen. A tap on an answer gives it; a tap on the header shows the
//! request full screen.

use herder_protocol::{ApprovalDecision, EscalationReason, Route, Timestamp};
use ratatui::Frame;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::action::Action;
use crate::app::App;
use crate::compose::Act;
use crate::mouse::{self, Click, Hits};
use crate::request::{Input, Pending};
use crate::ui::Ui;
use crate::ui::dialog::{Dialog, Size};
use crate::ui::glyphs::Glyphs;
use crate::ui::hints::Hint;
use crate::ui::request::{Kind, Request};

/// How long ago `since` was, in its largest unit: `12s`, `3m`, `2h`, `4d`; `None` when the
/// clock disagrees or it is over a week.
fn age(since: Timestamp) -> Option<String> {
    let secs = Timestamp::now().duration_since(since).as_secs();
    let age = match secs {
        ..0 => return None,
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        86_400..604_800 => format!("{}d", secs / 86_400),
        _ => return None,
    };
    Some(age)
}

/// Why a child's request is the user's, and what its primary session said; that the primary
/// is asked first.
fn notes<'a>(
    ui: Ui,
    routed: Route,
    reason: Option<EscalationReason>,
    note: Option<&str>,
) -> Vec<Line<'a>> {
    let mut out = Vec::new();
    if routed == Route::Primary {
        out.push(Line::styled(
            "asked the primary session first; answering here overrides it",
            ui.muted(),
        ));
    }
    if let Some(reason) = reason {
        out.push(Line::styled(
            crate::session::reason_text(reason).to_owned(),
            ui.muted(),
        ));
    }
    if let Some(note) = note {
        out.push(Line::styled(
            format!("the primary says: {note}"),
            ui.muted().add_modifier(Modifier::ITALIC),
        ));
    }
    out
}

/// The panel for the open session's request, `width` wide; `None` when it waits on none.
fn panel<'a>(app: &'a App, ui: Ui<'a>, compact: bool) -> Option<Request<'a>> {
    let session = app.open_session()?;
    let request = match app.pending()? {
        Pending::Approval(approval) => {
            let (tool, body) = split_summary(&approval.summary);
            let hint = if compact {
                Line::default()
            } else {
                Line::from(vec![
                    Span::styled("y", ui.strong()),
                    Span::styled(" allow  ", ui.muted()),
                    Span::styled("n", ui.strong()),
                    Span::styled(" deny", ui.muted()),
                ])
            };
            Request::new(ui, Kind::Approval, tool, body)
                .age(age(approval.since))
                .notes(notes(
                    ui,
                    approval.routed_to,
                    approval.reason,
                    approval.note.as_deref(),
                ))
                .hint(hint)
        }
        Pending::Question(question) => {
            let who = session.task.as_deref().unwrap_or("the agent");
            Request::new(ui, Kind::Question, who, &question.text)
                .age(age(question.since))
                .notes(notes(
                    ui,
                    question.routed_to,
                    question.reason,
                    question.note.as_deref(),
                ))
                .choices(&question.choices)
        }
    };
    Some(request.select(Some(app.request_cursor())))
}

/// An approval's summary as its tool and the rest: `Bash: rm -rf target` is `Bash` and
/// `rm -rf target`; one with no tool is all body.
fn split_summary(summary: &str) -> (&str, &str) {
    match summary.split_once(": ") {
        Some((tool, rest)) if !tool.contains(char::is_whitespace) && !tool.is_empty() => {
            (tool, rest)
        }
        _ => ("tool", summary),
    }
}

/// Rows the open session's request takes `width` columns wide; 0 when it waits on none.
pub(super) fn height(app: &App, width: u16, compact: bool) -> u16 {
    panel(app, app.ui(), compact).map_or(0, |request| request.height(width))
}

/// Draws the open session's request into `area`.
pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App, compact: bool, hits: &mut Hits) {
    let ui = app.ui();
    let Some(request) = panel(app, ui, compact) else {
        return;
    };
    let approval = app.approval_pending();
    let choices = match app.pending() {
        Some(Pending::Question(question)) => question.choices.len(),
        _ => 0,
    };
    let placed = request.render(area, frame.buffer_mut());
    hits.click(
        Rect { height: 1, ..area },
        Click::Act(Action::Request(Input::Full)),
    );
    for (at, rect) in placed {
        let act = match (approval, at) {
            (true, 0) => Act::Approve(ApprovalDecision::Allow),
            (true, _) => Act::Approve(ApprovalDecision::Deny),
            (false, at) if at == choices => Act::Write,
            (false, at) => Act::Choose(u32::try_from(at).unwrap_or(u32::MAX)),
        };
        hits.click(rect, Click::Act(Action::Compose(act)));
    }
}

/// The open session's request full screen, while `f` shows it.
pub(super) fn full(frame: &mut Frame, area: Rect, app: &mut App, hits: &mut Hits) {
    if !app.request.full {
        return;
    }
    let theme = app.theme.clone();
    let ui = Ui::new(&theme, Glyphs::for_width(app.glyphs, app.width));
    let Some(pending) = app.pending() else {
        return;
    };
    let (title, text, notes, hints) = match pending {
        Pending::Approval(approval) => {
            let (tool, body) = split_summary(&approval.summary);
            let hints = vec![
                Hint::new("y", "allow"),
                Hint::new("n", "deny"),
                Hint::new("j/k", "scroll"),
                Hint::new("esc", "back"),
            ];
            (
                format!("approval{}{tool}", ui.glyphs.separator),
                body.to_owned(),
                notes(
                    ui,
                    approval.routed_to,
                    approval.reason,
                    approval.note.as_deref(),
                ),
                hints,
            )
        }
        Pending::Question(question) => {
            let mut text = question.text.clone();
            for (at, choice) in question.choices.iter().enumerate() {
                text.push_str(&format!("\n  {} {choice}", at + 1));
            }
            let hints = vec![
                Hint::new(format!("1-{}", question.choices.len().max(1)), "pick"),
                Hint::new("j/k", "scroll"),
                Hint::new("esc", "back"),
            ];
            (
                "question".to_owned(),
                text,
                notes(
                    ui,
                    question.routed_to,
                    question.reason,
                    question.note.as_deref(),
                ),
                hints,
            )
        }
    };
    let dialog = Dialog::new(ui, title, Size::Large).hints(&hints);
    let body_width = Size::Large
        .width()
        .min(area.width)
        .saturating_sub(2 + 2 * crate::ui::dialog::PAD_X);
    let room = usize::from(body_width).max(8);
    let mut lines: Vec<Line> = text
        .lines()
        .flat_map(|line| {
            let parts = textwrap::wrap(line, room);
            if parts.is_empty() {
                return vec![Line::default()];
            }
            parts
                .into_iter()
                .map(|part| Line::styled(part.into_owned(), ui.text()))
                .collect()
        })
        .collect();
    if !notes.is_empty() {
        lines.push(Line::default());
        lines.extend(notes);
    }
    let shown = lines.len().clamp(
        1,
        usize::from(area.height.saturating_sub(dialog.chrome()).max(1)),
    );
    app.request.scroll = app.request.scroll.min(lines.len() - shown.min(lines.len()));
    let areas = dialog.render(
        area,
        u16::try_from(shown).unwrap_or(u16::MAX),
        frame.buffer_mut(),
    );
    let buf = frame.buffer_mut();
    for (line, y) in lines
        .into_iter()
        .skip(app.request.scroll)
        .zip(areas.body.y..areas.body.bottom())
    {
        crate::ui::fit(line, usize::from(areas.body.width), ui.glyphs).render(
            Rect {
                y,
                height: 1,
                ..areas.body
            },
            buf,
        );
    }
    super::palette::dialog_taps(hits, area, &areas);
    // A tap outside goes back rather than leaving the request.
    hits.click(area, mouse::key(KeyCode::Char('f')));
    hits.click(areas.outer, Click::Nothing);
    hits.click(areas.close, mouse::key(KeyCode::Esc));
}
