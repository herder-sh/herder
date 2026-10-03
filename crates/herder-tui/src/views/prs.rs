//! Pull requests on screen: the open session's `prs` tab, the badge on each session-list row,
//! the cross-session view, and the link prompt. Grouped by project, the cross-session view
//! lists each project's PRs together, from every session and machine.
//!
//! ```text
//!  pull requests · 3 open                                              by project
//!
//!  app
//!    #12  open    ci ✓  review …  merge ✓  Add health endpoint             api
//!  ▶ #9   draft   ci ✗  review ✗  merge ✗  Fix login redirect        fix-login
//! ```
//!
//! The PR's state takes its `pr*` colour; each check its pass, fail or pending colour. On a
//! narrow pane a row keeps the number, the state, CI, the review and the title.

use herder_protocol::{CiStatus, Mergeable, PrState, PullRequest, ReviewStatus};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui_textarea::TextArea;

use crate::app::{App, Focus};
use crate::mouse::{Click, Hits, List as Rows, Wheel};
use crate::palette::search_line;
use crate::projects::Grouping;
use crate::session::Session;
use crate::ui::Ui;
use crate::ui::dialog::{Dialog, Size};
use crate::ui::hints::Hint;
use crate::ui::input::Field;
use crate::ui::list::{ListView, Row};
use crate::ui::state;

/// Most PRs a session-list badge names; the rest are counted.
const BADGE_PRS: usize = 2;

/// The open session's PRs, one row each, their head branch at the right; `compact` on a narrow
/// pane.
pub(super) fn strip(frame: &mut Frame, area: Rect, app: &App, compact: bool, hits: &mut Hits) {
    let ui = app.ui();
    let prs = app.strip_prs();
    if area.height == 0 {
        return;
    }
    if prs.is_empty() {
        super::nothing(frame, area, ui, "No pull requests linked yet: L links one.");
        return;
    }
    let number_width = number_width(prs.iter().map(|(_, pr)| *pr));
    let rows: Vec<Row> = prs
        .iter()
        .map(|(_, pr)| {
            let branch = match &pr.head_branch {
                // On a phone the title keeps the room.
                Some(branch) if app.width >= super::NARROW => branch.clone(),
                _ => String::new(),
            };
            row(ui, pr, number_width, compact).right(Span::styled(branch, ui.muted()))
        })
        .collect();
    let mut offset = 0;
    let placed = ListView::new(ui, rows)
        .select(Some(app.pr_index()))
        .focused(app.focus == Focus::Prs)
        .render(area, frame.buffer_mut(), &mut offset);
    hits.wheel(area, Wheel::Strip);
    for (at, rect) in placed {
        hits.click(rect, Click::Row(Rows::Strip, at));
    }
}

/// Every session's PRs, under a heading per project or per session, in session-list order;
/// `compact` on a phone.
pub(super) fn all(frame: &mut Frame, area: Rect, app: &App, compact: bool, hits: &mut Hits) {
    let ui = app.ui();
    let prs = app.all_prs();
    let by_project = app.grouping == Grouping::Projects;
    let open = prs.iter().filter(|(_, pr)| live(pr)).count();
    let area = super::heading(
        frame,
        area,
        ui,
        "pull requests",
        &format!("{open} open"),
        if by_project {
            "by project"
        } else {
            "by session"
        },
        compact,
    );
    if prs.is_empty() {
        super::nothing(
            frame,
            area,
            ui,
            "No pull requests are linked to any session yet.",
        );
        return;
    }
    // On a phone the title keeps the row's room; narrow panes get compact rows.
    let phone = compact;
    let compact = compact || area.width < 80;
    let number_width = number_width(prs.iter().map(|(_, pr)| *pr));
    let mut rows = Vec::new();
    // The PR of each row, by index; headings and gaps have none.
    let mut picks = Vec::new();
    let mut selected = None;
    let mut group = None;
    for (index, (key, pr)) in prs.iter().enumerate() {
        let session = app.sessions.get(*key);
        let this = if by_project {
            Some(app.project_of(key).map_or_else(
                || "no project yet".to_owned(),
                |project| app.project_name(&project),
            ))
        } else {
            Some(format!("{}/{}", key.host_id, key.session_id))
        };
        if index == 0 || this != group {
            group = this;
            if !rows.is_empty() {
                rows.push(Row::Gap);
                picks.push(None);
            }
            rows.push(if by_project {
                Row::header(group.clone().unwrap_or_default())
            } else {
                session_header(app, ui, key, session)
            });
            picks.push(None);
        }
        if index == app.pr_index() {
            selected = Some(rows.len());
        }
        let right = if by_project {
            session.map_or_else(String::new, |s| super::projects::session_name(s, true))
        } else {
            pr.head_branch.clone().unwrap_or_default()
        };
        let right = if phone { String::new() } else { right };
        rows.push(row(ui, pr, number_width, compact).right(Span::styled(right, ui.muted())));
        picks.push(Some(index));
    }
    let mut offset = 0;
    let placed = ListView::new(ui, rows)
        .select(selected)
        .focused(app.focus == Focus::AllPrs)
        .render(area, frame.buffer_mut(), &mut offset);
    hits.wheel(area, Wheel::Keys);
    for (at, rect) in placed {
        if let Some(Some(index)) = picks.get(at) {
            hits.click(rect, Click::Row(Rows::AllPrs, *index));
        }
    }
}

/// A session's heading, when PRs are listed by session: its state and name, its machine at
/// the right.
fn session_header<'a>(
    app: &App,
    ui: Ui,
    key: &crate::session::SessionKey,
    session: Option<&Session>,
) -> Row<'a> {
    let state = app.state(key);
    let machine = app
        .machines
        .iter()
        .find(|machine| machine.host_id == key.host_id)
        .map_or_else(String::new, |machine| machine.name.clone());
    let title = session.map(Session::short_title).unwrap_or_default();
    Row::header(Line::from(vec![
        state::dot(ui, state),
        Span::raw(" "),
        Span::raw(title),
    ]))
    .right(machine)
}

/// The session-list badge: the first open PRs by number with their checks, then a count of
/// the rest; `compact` names only the first. Empty for a session with no PRs.
pub(super) fn badge(ui: Ui, session: &Session, compact: bool) -> Vec<Span<'static>> {
    let named = if compact { 1 } else { BADGE_PRS };
    let mut prs: Vec<&PullRequest> = session.prs.iter().collect();
    // Live PRs first; the sort is stable, so each group keeps its link order.
    prs.sort_by_key(|pr| !live(pr));
    let mut spans = Vec::new();
    for pr in prs.iter().take(named) {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("#{}", pr.number),
            state_style(ui, pr.state),
        ));
        if live(pr) {
            if pr.ci != CiStatus::None {
                // Apart from the number, so `#7 v` never reads as one word.
                let (mark, style) = ci(ui, pr.ci);
                spans.push(Span::raw(" "));
                spans.push(Span::styled(mark, style));
            }
            if blocked(pr) {
                spans.push(Span::styled("!", Style::new().fg(ui.theme.error)));
            }
        }
    }
    if let Some(more) = prs.len().checked_sub(named).filter(|more| *more > 0) {
        spans.push(Span::styled(format!(" +{more}"), ui.muted()));
    }
    spans
}

/// The link prompt, over everything else: a dialog with one field.
pub(super) fn prompt(frame: &mut Frame, area: Rect, app: &App) {
    let Some(prompt) = &app.prs.prompt else {
        return;
    };
    let ui = app.ui();
    let title = app
        .sessions
        .get(&prompt.key)
        .map(Session::short_title)
        .unwrap_or_default();
    let hints = [Hint::new("enter", "link"), Hint::new("esc", "cancel")];
    let title = Line::from(ui.joined([Span::raw("link a pull request"), Span::raw(title)]));
    let areas =
        Dialog::new(ui, title, Size::Medium)
            .hints(&hints)
            .render(area, 1, frame.buffer_mut());
    let mut editor: TextArea = search_line(&prompt.text, "number or URL");
    Field::new(ui, "pr", &mut editor).focused(true).render(
        areas.body,
        frame.buffer_mut(),
        crate::ui::select::label_width("pr"),
    );
}

/// One PR on one short line, as the details panel lists it: number, CI and title.
pub(super) fn line(ui: Ui, pr: &PullRequest) -> Line<'static> {
    let (mark, style) = ci(ui, pr.ci);
    Line::from(vec![
        Span::styled(
            format!("#{}", pr.number),
            state_style(ui, pr.state).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(mark, style),
        Span::raw(" "),
        Span::styled(pr.title.clone(), ui.text()),
    ])
}

/// A PR as a list row: number, state, CI, review, mergeability and title on one line; with
/// `compact`, on a narrow pane, the checks go on a line under the title, which keeps its room.
fn row<'a>(ui: Ui, pr: &PullRequest, number_width: usize, compact: bool) -> Row<'a> {
    let state = state_style(ui, pr.state);
    let label = match pr.state {
        PrState::Open => "open",
        PrState::Draft => "draft",
        PrState::Merged => "merged",
        PrState::Closed => "closed",
    };
    let mut head = vec![
        Span::styled(
            format!("{:<number_width$}", format!("#{}", pr.number)),
            state.add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(format!("{label:<6}"), state),
        Span::raw("  "),
    ];
    let mut checks = Vec::new();
    let mut check = |name: &'static str, (mark, style): (&'static str, Style)| {
        if !checks.is_empty() {
            checks.push(Span::raw("  "));
        }
        checks.push(Span::styled(format!("{name} "), ui.muted()));
        checks.push(Span::styled(mark, style));
    };
    check("ci", ci(ui, pr.ci));
    check("review", review(ui, pr.review));
    if live(pr) {
        check("merge", merge(ui, pr.mergeable));
    }
    let title = Span::styled(pr.title.clone(), ui.text());
    if compact {
        head.push(title);
        return Row::item(Line::from(head)).body(vec![Line::from(checks)]);
    }
    if !live(pr) {
        // Merged or closed: there is nothing to merge, but the titles stay in line.
        checks.push(Span::raw(" ".repeat("  merge ?".len())));
    }
    head.extend(checks);
    head.push(Span::raw("  "));
    head.push(title);
    Row::item(Line::from(head))
}

/// Whether the PR is still open, as a draft or not: only then do checks and reviews matter.
fn live(pr: &PullRequest) -> bool {
    matches!(pr.state, PrState::Open | PrState::Draft)
}

/// Whether a conflict or a requested change holds the PR up.
fn blocked(pr: &PullRequest) -> bool {
    pr.mergeable == Mergeable::Conflicting || pr.review == ReviewStatus::ChangesRequested
}

fn number_width<'a>(prs: impl Iterator<Item = &'a PullRequest>) -> usize {
    prs.map(|pr| pr.number.to_string().len() + 1)
        .max()
        .unwrap_or(0)
}

fn state_style(ui: Ui, state: PrState) -> Style {
    let theme = ui.theme;
    Style::new().fg(match state {
        PrState::Open => theme.pr_open,
        PrState::Draft => theme.pr_draft,
        PrState::Merged => theme.pr_merged,
        PrState::Closed => theme.pr_closed,
    })
}

/// A check's mark: passed, failed, pending, or none.
fn check(ui: Ui, passed: Option<bool>, pending: bool) -> (&'static str, Style) {
    let glyphs = ui.glyphs;
    let theme = ui.theme;
    match (passed, pending) {
        (Some(true), _) => (glyphs.check_pass, Style::new().fg(theme.success)),
        (Some(false), _) => (glyphs.check_fail, Style::new().fg(theme.error)),
        (None, true) => (glyphs.check_pending, Style::new().fg(theme.warning)),
        (None, false) => (glyphs.check_none, ui.muted()),
    }
}

fn ci(ui: Ui, ci: CiStatus) -> (&'static str, Style) {
    match ci {
        CiStatus::Passing => check(ui, Some(true), false),
        CiStatus::Failing => check(ui, Some(false), false),
        CiStatus::Pending => check(ui, None, true),
        CiStatus::None => check(ui, None, false),
    }
}

fn review(ui: Ui, review: ReviewStatus) -> (&'static str, Style) {
    match review {
        ReviewStatus::Approved => check(ui, Some(true), false),
        ReviewStatus::ChangesRequested => check(ui, Some(false), false),
        ReviewStatus::Required => check(ui, None, true),
        ReviewStatus::None => check(ui, None, false),
    }
}

fn merge(ui: Ui, mergeable: Mergeable) -> (&'static str, Style) {
    match mergeable {
        Mergeable::Clean => check(ui, Some(true), false),
        Mergeable::Conflicting => check(ui, Some(false), false),
        Mergeable::Unknown => ("?", ui.muted()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::glyphs::Glyphs;
    use crate::ui::theme::Theme;

    #[test]
    fn a_row_keeps_its_columns_whatever_the_state() {
        let theme = Theme::ansi();
        let ui = Ui::new(&theme, Glyphs::Unicode);
        let pr = |state| crate::fake::pr(7, "Title", state);
        let title_at = |pr: &PullRequest, compact| {
            let Row::Item { left, .. } = row(ui, pr, 3, compact) else {
                panic!("a PR is an item");
            };
            let text: String = left.spans.iter().map(|s| s.content.clone()).collect();
            text.find("Title").unwrap()
        };
        for compact in [false, true] {
            let open = title_at(&pr(PrState::Open), compact);
            assert_eq!(title_at(&pr(PrState::Merged), compact), open);
            assert_eq!(title_at(&pr(PrState::Closed), compact), open);
        }
        // Compact, the checks go under the title.
        assert_eq!(row(ui, &pr(PrState::Open), 3, true).height(), 2);
    }
}
