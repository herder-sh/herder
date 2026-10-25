//! Under the open transcript, as OpenCode's session view ends:
//!
//! - a pending approval or question, inline as a [`Request`] panel. An approval takes the
//!   prompt's place; a question sits over it, and the prompt takes a typed answer;
//! - the [`Prompt`], a box a blank row under the transcript, with its images' chips over
//!   the text (`image 1 · 340 KB`) and `provider · model · mode` in its bottom border, as
//!   the Mac app's prompt box holds its model and permission menus; and the `/` and `@`
//!   [`Popup`] over the transcript;
//! - under the box, the status row: the spinner and how long the turn has run, and at the
//!   right the session's account with its busiest usage window, as the Mac app's footer
//!   names it.
//!
//! ```text
//! ╭──────────────────────────────────────────────────────────╮
//! │ █ Write a prompt…                                        │
//! │                                                          │
//! ╰─────────────────────── claude · claude-opus-5-5 · ask ─╯
//!   ⠋ working · 12s                            claude-main 5h 27%
//! //! ```
//!
//! A tap on a button or a choice answers; a tap on the prompt writes in it.

use herder_protocol::{ApprovalDecision, Route, SessionStatus};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use super::markdown::wrap;
use super::transcript::duration;
use crate::action::Action;
use crate::app::{App, Focus};
use crate::compose::Act;
use crate::mouse::{Click, Hits};
use crate::session::{Delivery, Session};
use crate::ui::input::Prompt;
use crate::ui::popup::Popup;
use crate::ui::request::{self, Request};
use crate::ui::{Ui, badge, fit, spread};

/// Rows of a request's body before `f` shows the rest.
const REQUEST_ROWS: usize = 15;

/// Screens shorter than this give the prompt one text row, not two.
const SHORT: u16 = 20;

/// Where a tap on the request panel answers.
enum Tap {
    /// The footer's buttons, by where they start and how wide they are.
    Buttons(Vec<(u16, u16, Click)>),
    /// Body rows, by index, that pick a choice.
    Rows(Vec<(usize, Click)>),
}

/// The open session's pending request as a panel `width` wide, with where taps answer it.
fn request<'a>(
    app: &'a App,
    session: &'a Session,
    width: u16,
    compact: bool,
) -> Option<(Request<'a>, Tap)> {
    let ui = app.ui();
    let room = Request::text_width(width);
    let age = |since: herder_protocol::Timestamp| {
        let seconds = app.now().as_second() - since.as_second();
        let asked = if compact { "" } else { "asked " };
        Span::styled(format!("{asked}{} ago", duration(seconds)), ui.muted())
    };
    let more = session.approvals.len() + session.questions.len();
    let more = (more > 1).then(|| format!("+{} more{}", more - 1, ui.glyphs.separator));
    let right = |since| {
        let mut spans = Vec::new();
        if let Some(more) = &more {
            spans.push(Span::styled(more.clone(), ui.muted()));
        }
        spans.push(age(since));
        Line::from(spans)
    };
    let cap = if app.compose.full {
        usize::MAX
    } else {
        REQUEST_ROWS
    };
    let mut body = Vec::new();
    let text = |body: &mut Vec<Line<'static>>, text: &str, style: Style| {
        for line in text.lines() {
            body.extend(wrap(&[(line.to_owned(), style)], room, &[], &[]));
        }
    };
    let escalation = |body: &mut Vec<Line<'static>>, reason, note: Option<&str>, routed: Route| {
        if let Some(reason) = reason {
            text(
                body,
                &format!("escalated: {}", crate::session::reason_text(reason)),
                ui.muted(),
            );
        }
        if let Some(note) = note {
            let style = ui.muted().add_modifier(ratatui::style::Modifier::ITALIC);
            text(body, &format!("the primary says: {note}"), style);
        }
        if routed == Route::Primary {
            text(
                body,
                "asked the primary session first; you can answer too",
                ui.muted(),
            );
        }
    };
    if let Some(approval) = session.approvals.first() {
        let tool = session
            .tool_name(&approval.tool_call_id)
            .unwrap_or_else(|| "tool".to_owned());
        let mut header = request::header(ui, ui.glyphs.approval, "approval".to_owned());
        header.push(Span::styled(
            format!("{}{tool}", ui.glyphs.separator),
            ui.muted(),
        ));
        let summary = super::tools::in_worktree(&approval.summary, &session.worktree);
        text(&mut body, &summary, ui.text());
        let hidden = body.len().saturating_sub(cap);
        body.truncate(cap);
        if hidden > 0 {
            body.push(Line::styled(
                format!("{} {hidden} more lines · f full", ui.glyphs.ellipsis),
                ui.muted(),
            ));
        }
        escalation(
            &mut body,
            approval.reason,
            approval.note.as_deref(),
            approval.routed_to,
        );
        let (buttons, taps) = request::buttons(ui, &["allow", "deny"], app.compose.button);
        let keys = if compact {
            "y allow · n deny"
        } else {
            "y allow · n deny · ←/→ enter · f full"
        };
        let hint = Line::from(Span::styled(fit_sep(ui, keys), ui.muted()));
        let decisions = [ApprovalDecision::Allow, ApprovalDecision::Deny];
        let taps = taps
            .into_iter()
            .zip(decisions)
            .map(|((x, w), decision)| (x, w, Click::Act(Action::Compose(Act::Approve(decision)))))
            .collect();
        let panel = Request::new(ui, header, right(approval.since))
            .body(body)
            .footer(buttons, hint)
            .padded(!compact);
        return Some((panel, Tap::Buttons(taps)));
    }
    let question = session.questions.first()?;
    let header = request::header(ui, ui.glyphs.question, "question".to_owned());
    text(&mut body, &question.text, ui.text());
    body.truncate(cap);
    let mut rows = Vec::new();
    for (at, choice) in (0..).zip(&question.choices) {
        let first = body.len();
        let mut lead = vec![Span::styled(format!(" {} ", at + 1), ui.accent())];
        let mut pad = vec![Span::raw("   ")];
        let mut style = ui.text();
        // A multi-select question shows a box per choice, and the one Space picks.
        if question.multi_select {
            let mark = if question.picked.contains(&at) {
                "[x] "
            } else {
                "[ ] "
            };
            lead.push(Span::styled(mark, ui.accent()));
            pad.push(Span::raw("    "));
            if at as usize == app.compose.option {
                style = ui.strong();
            }
        }
        body.extend(wrap(&[(choice.clone(), style)], room, &lead, &pad));
        let click = Click::Act(Action::Compose(Act::Choose(at)));
        rows.extend((first..body.len()).map(|row| (row, click.clone())));
    }
    escalation(
        &mut body,
        question.reason,
        question.note.as_deref(),
        question.routed_to,
    );
    let mut panel = Request::new(ui, header, right(question.since))
        .body(body)
        .padded(!compact);
    if question.multi_select {
        let picked = format!("{} picked", question.picked.len());
        let keys = if compact {
            "space pick · enter submit"
        } else {
            "↑/↓ · space pick · enter submit"
        };
        let hint = Span::styled(fit_sep(ui, keys), ui.muted());
        panel = panel.footer(Span::styled(picked, ui.muted()), hint);
    }
    Some((panel, Tap::Rows(rows)))
}

/// `text` with the glyph set's separator for `·`.
fn fit_sep(ui: Ui, text: &str) -> String {
    text.replace(" · ", ui.glyphs.separator)
}

/// The chips of the prompt's images, then of those loading, in rows `width` wide.
fn chips(app: &App, width: u16) -> Vec<Line<'static>> {
    let ui = app.ui();
    let compose = &app.compose;
    let loaded = compose.images.iter().map(|image| {
        let size = crate::attach::size(image.data.0.len() as u64);
        (size, ui.text())
    });
    let loading = (0..compose.loading).map(|_| ("loading…".to_owned(), ui.muted()));
    let chips = loaded
        .chain(loading)
        .zip(1..)
        .map(|((what, style), n)| {
            let label = format!("image {n}{}{what}", ui.glyphs.separator);
            badge::chip(ui, &label.replace('…', ui.glyphs.ellipsis), style)
        })
        .collect();
    badge::chip_rows(chips, usize::from(Prompt::text_width(width)))
}

/// The prompt's meta, `provider · model · mode`, `width` columns wide: the provider
/// gives way first, then the model is cut; the mode, what the agent may do, stays whole.
fn meta(app: &App, session: &Session, width: u16) -> Line<'static> {
    let ui = app.ui();
    let mode = Span::styled(mode_label(session.permission_mode), ui.text());
    let model =
        (!session.model.is_empty()).then(|| Span::styled(session.model.clone(), ui.muted()));
    let provider = session
        .provider
        .as_ref()
        .map(|provider| Span::styled(provider.as_str().to_owned(), ui.muted()));
    let width = usize::from(width);
    let full = Line::from(
        ui.joined(
            provider
                .into_iter()
                .chain(model.clone())
                .chain([mode.clone()]),
        ),
    );
    if full.width() <= width {
        return full;
    }
    // A model cut to less than a few columns says nothing: the mode alone then.
    let room = width.saturating_sub(mode.width() + crate::ui::width(ui.glyphs.separator));
    match model.filter(|_| room >= 6) {
        Some(model) => {
            let mut line = fit(Line::from(model), room, ui.glyphs);
            line.spans
                .push(Span::styled(ui.glyphs.separator, ui.muted()));
            line.spans.push(mode);
            line
        }
        None => fit(Line::from(mode), width, ui.glyphs),
    }
}

/// A permission mode as the prompt names it.
fn mode_label(mode: herder_protocol::PermissionMode) -> &'static str {
    use herder_protocol::PermissionMode;
    match mode {
        PermissionMode::ReadOnly => "read only",
        PermissionMode::Ask => "ask",
        PermissionMode::AutoEdit => "auto edit",
        PermissionMode::FullAccess => "full access",
    }
}

/// Rows the controls take: the request panel's, and the prompt's (0 while an approval
/// takes its place).
#[derive(Clone, Copy, Debug)]
pub(super) struct Heights {
    request: u16,
    prompt: u16,
}

/// The controls' heights in a main pane `area`, on a screen `tall` enough for a roomy prompt.
fn heights(app: &App, session: &Session, area: Rect, compact: bool, tall: bool) -> Heights {
    let width = area.width.saturating_sub(2);
    let request = request(app, session, width, compact).map_or(0, |(panel, _)| panel.height());
    let prompt = if !session.approvals.is_empty() {
        0
    } else if app
        .open
        .as_ref()
        .and_then(|key| app.read_only(key))
        .is_some()
        || session.status == SessionStatus::Archived
    {
        1
    } else {
        let max_lines = (area.height / 3).max(6);
        // The chips, and a row between them and the text.
        let chips = match chips(app, width).len() {
            0 => 0,
            rows => u16::try_from(rows + 1).unwrap_or(0),
        };
        Prompt::height(&app.compose.editor, width, max_lines, tall) + chips
    };
    Heights { request, prompt }
}

/// Splits the main pane into the transcript and, below it, the controls, on a screen
/// `screen_height` rows tall.
pub(super) fn split(
    area: Rect,
    app: &App,
    compact: bool,
    screen_height: u16,
) -> (Rect, Option<(Rect, Heights)>) {
    let Some(session) = app.open_session() else {
        return (area, None);
    };
    let heights = heights(app, session, area, compact, screen_height >= SHORT);
    // A blank row over the controls, the status row under them.
    let wanted = 1 + heights.request + heights.prompt + 1;
    // The transcript keeps at least half the pane, unless the request shows in full.
    let most = if app.compose.full {
        area.height.saturating_sub(2)
    } else {
        area.height / 2
    };
    let height = wanted.min(most.max(heights.prompt + 2));
    let height = height.min(area.height);
    let transcript = Rect {
        height: area.height - height,
        ..area
    };
    let controls = Rect {
        y: transcript.bottom(),
        height,
        ..area
    };
    (transcript, Some((controls, heights)))
}

/// `compact`, on a narrow screen, keeps the hints short and the panels unpadded.
pub(super) fn draw(
    frame: &mut Frame,
    area: Rect,
    heights: Heights,
    app: &App,
    compact: bool,
    hits: &mut Hits,
) {
    let Some(session) = app.open_session() else {
        return;
    };
    let ui = app.ui();
    let inner = Rect {
        x: area.x + 1,
        width: area.width.saturating_sub(2),
        ..area
    };
    // Bottom up: the status row, the prompt, the request; what does not fit is the request's.
    let status = Rect {
        y: inner.bottom().saturating_sub(1),
        height: 1.min(inner.height),
        ..inner
    };
    let prompt = Rect {
        y: status.y.saturating_sub(heights.prompt).max(inner.y),
        height: heights.prompt.min(status.y - inner.y),
        ..inner
    };
    let request_top = (inner.y + 1).min(prompt.y);
    let request_area = Rect {
        y: request_top,
        height: prompt.y - request_top,
        ..inner
    };
    let buf = frame.buffer_mut();
    if let Some((panel, taps)) = request(app, session, inner.width, compact) {
        let body_top = request_area.y + u16::from(!compact) + 1;
        let footer = panel.render(request_area, buf);
        match taps {
            Tap::Buttons(buttons) => {
                if let Some(row) = footer {
                    for (x, width, click) in buttons {
                        hits.click(Rect::new(row.x + x, row.y, width, 1), click);
                    }
                }
            }
            Tap::Rows(rows) => {
                for (row, click) in rows {
                    let y = body_top + u16::try_from(row).unwrap_or(u16::MAX);
                    if y < request_area.bottom() {
                        hits.click(Rect::new(request_area.x, y, request_area.width, 1), click);
                    }
                }
            }
        }
    }
    // The status row lines up with the prompt's text, inside its border and padding.
    let status_text = Rect {
        x: status.x + 2.min(status.width),
        width: status.width.saturating_sub(4),
        ..status
    };
    status_row(frame, status_text, app, session, compact);
    let buf = frame.buffer_mut();
    if let Some((text, fork)) = app.open.as_ref().and_then(|key| app.read_only(key)) {
        let style = if fork {
            Style::new().fg(ui.theme.error)
        } else {
            ui.muted()
        };
        fit(
            Line::styled(text, style),
            usize::from(prompt.width),
            ui.glyphs,
        )
        .render(prompt, buf);
        if fork {
            hits.click(prompt, Click::Act(Action::OpenFork));
        }
        return;
    }
    if session.status == SessionStatus::Archived {
        Line::styled("archived · :unarchive brings it back", ui.muted()).render(prompt, buf);
        return;
    }
    if heights.prompt == 0 {
        return;
    }
    let focused = app.focus == Focus::Composer;
    let mut widget = Prompt::new(ui, &app.compose.editor)
        .focused(focused)
        .chips(chips(app, inner.width))
        .meta(meta(app, session, inner.width.saturating_sub(6)));
    if !session.questions.is_empty() {
        widget = widget
            .accent(ui.theme.attention)
            .placeholder("or type an answer…");
    }
    widget.render(prompt, buf);
    hits.click(prompt, Click::Act(Action::Compose(Act::Write)));
    // The popup opens over the transcript, above the prompt.
    let completions = if focused {
        app.completions()
    } else {
        Vec::new()
    };
    if !completions.is_empty() {
        let height = Popup::height(completions.len()).min(prompt.y.saturating_sub(frame.area().y));
        let popup = Rect {
            y: prompt.y.saturating_sub(height),
            height,
            ..prompt
        };
        let rows = completions
            .into_iter()
            .map(|c| {
                (
                    Line::from(Span::styled(c.label, ui.text())),
                    Line::from(Span::styled(c.does, ui.muted())),
                )
            })
            .collect::<Vec<_>>();
        let selected = app.compose.popup.min(rows.len() - 1);
        Popup::new(ui, rows)
            .select(selected)
            .render(popup, frame.buffer_mut());
        hits.cover(popup);
    }
}

/// The status row: the spinner, how long the turn has run, and how to stop it; or the last
/// command's error. At the right, the busiest usage window of the session's account.
fn status_row(frame: &mut Frame, area: Rect, app: &App, session: &Session, compact: bool) {
    let ui = app.ui();
    let theme = ui.theme;
    let mut left = Vec::new();
    let error = app
        .open
        .as_ref()
        .and_then(|key| app.compose.errors.get(key));
    if let Some(error) = error {
        left.push(Span::styled(
            format!("{} ", ui.glyphs.check_fail),
            Style::new().fg(theme.error),
        ));
        left.push(Span::styled(error.clone(), ui.muted()));
    } else if session.turn.is_some() {
        let elapsed = session.turn_started.map_or(0, |start| {
            app.now().as_millisecond() - start.as_millisecond()
        });
        let frame_glyph = ui.glyphs.spinner_frame(u64::try_from(elapsed).unwrap_or(0));
        left.push(Span::styled(frame_glyph, ui.accent()));
        left.push(Span::styled(" working", ui.text()));
        if session.turn_started.is_some() {
            left.push(Span::styled(
                format!("{}{}", ui.glyphs.separator, duration(elapsed / 1000)),
                ui.muted(),
            ));
        }
    }
    let queued = session
        .outbox
        .iter()
        .filter(|sent| !matches!(sent.state, Delivery::Failed(_)))
        .count();
    if session.turn.is_some() && queued > 0 && error.is_none() {
        if !left.is_empty() {
            left.push(Span::raw("  "));
        }
        left.push(Span::styled(
            format!("{queued} queued"),
            Style::new().fg(theme.warning),
        ));
    }
    let left = Line::from(left);
    let right = usage(app, session, compact).unwrap_or_default();
    // An error that does not fit beside the usage takes the row: it says what to do now.
    let right = if error.is_some() && left.width() + 2 + right.width() > usize::from(area.width) {
        Line::default()
    } else {
        right
    };
    let line = spread(left, right, usize::from(area.width), ui.glyphs);
    line.render(area, frame.buffer_mut());
}

/// `account 5h 38%`: the session's account and its busiest usage window, either where the
/// other is not known; on a `compact` screen, the window alone.
fn usage(app: &App, session: &Session, compact: bool) -> Option<Line<'static>> {
    let ui = app.ui();
    let key = app.open.as_ref()?;
    let id = session.account_id.as_ref()?;
    let account = crate::account_screen::find(&app.machines, &key.host_id, id);
    let window = account.and_then(|account| {
        account
            .usage
            .iter()
            .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
    });
    let mut spans = Vec::new();
    if !compact || window.is_none() {
        let label = account.map_or_else(|| id.to_string(), |account| account.label.clone());
        spans.push(Span::styled(label, ui.muted()));
    }
    if let Some(window) = window {
        let label = match window.window.as_str() {
            "five_hour" => "5h".to_owned(),
            "seven_day" | "weekly" => "week".to_owned(),
            "daily" => "day".to_owned(),
            other => crate::account_screen::window_label(other),
        };
        // A share from 0 to 100, as the theme's usage colours take it.
        let percent = window.used_percent.clamp(0.0, 100.0).round() as u8;
        if !spans.is_empty() {
            spans.push(Span::raw(" "));
        }
        spans.extend([
            Span::styled(label, ui.muted()),
            Span::raw(" "),
            Span::styled(
                format!("{percent}%"),
                Style::new().fg(ui.theme.usage(percent)),
            ),
        ]);
    }
    Some(Line::from(spans))
}

/// [`waiting`] as one glyph, for compact rows.
pub(super) fn waiting_glyph(session: &Session) -> Option<(&'static str, Style)> {
    waiting(session).map(|(label, style)| (if label == "question" { "?" } else { "!" }, style))
}

/// The session list's badge for a session waiting on the user, instead of its status. A
/// child's request put to its primary session first does not wait on the user yet.
pub(super) fn waiting(session: &Session) -> Option<(&'static str, Style)> {
    let user = |route: &Route| *route == Route::User;
    let attention = Style::new()
        .fg(ratatui::style::Color::Magenta)
        .add_modifier(ratatui::style::Modifier::BOLD);
    if session.approvals.iter().any(|a| user(&a.routed_to)) {
        Some(("approve?", attention))
    } else if session.questions.iter().any(|q| user(&q.routed_to)) {
        Some(("question", attention))
    } else {
        None
    }
}
