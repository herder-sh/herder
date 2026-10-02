//! Pull requests on screen: the strip over the open session's transcript, the badge on each
//! session-list row, the cross-session view, and the link prompt. Grouped by project, the
//! cross-session view lists each project's PRs together, from every session and machine.
//!
//! A PR's row ends with its head branch, except on a narrow screen.
//!
//! Colours carry the state everywhere: green open, grey draft, magenta merged, red closed; for
//! checks green passing, red failing, yellow running.

use herder_protocol::{CiStatus, Mergeable, PrState, PullRequest, ReviewStatus};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};

use crate::app::{App, Focus};
use crate::session::Session;

/// Most PRs the strip shows at once; more scroll.
const STRIP_ROWS: usize = 4;
/// Most PRs a session-list badge names; the rest are counted.
const BADGE_PRS: usize = 2;

/// Rows the strip takes over the transcript: none when the open session has no PRs.
pub(super) fn strip_height(app: &App) -> u16 {
    match app.strip_prs().len() {
        0 => 0,
        n => u16::try_from(n.min(STRIP_ROWS) + 2).unwrap_or(u16::MAX),
    }
}

/// The open session's PRs, one row each; `compact` on a narrow screen.
pub(super) fn strip(frame: &mut Frame, area: Rect, app: &App, compact: bool) {
    let prs = app.strip_prs();
    if area.height == 0 || prs.is_empty() {
        return;
    }
    let number_width = number_width(prs.iter().map(|(_, pr)| *pr));
    let items: Vec<ListItem> = prs
        .iter()
        .map(|(_, pr)| ListItem::new(row(pr, number_width, compact)))
        .collect();
    let mut block = Block::bordered()
        .title(Line::from(vec![
            Span::styled(" pull requests ", super::bold()),
            Span::styled(format!("({}) ", prs.len()), super::dim()),
        ]))
        .border_style(super::border(app, Focus::Prs));
    if app.focus == Focus::Prs {
        let hint = if compact {
            " Enter open · x · L · ⌫ back "
        } else {
            " Enter open · x unlink · L link · Esc back "
        };
        block = block.title_bottom(Line::styled(hint, super::dim()).right_aligned());
    } else {
        block = block.title(Line::styled(" p ", super::dim()).right_aligned());
    }
    let mut state = ListState::default();
    if app.focus == Focus::Prs {
        state.select(Some(app.pr_index()));
    }
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, area, &mut state);
}

/// Every session's PRs, under a heading per session, in session-list order; `compact` on a
/// narrow screen.
pub(super) fn all(frame: &mut Frame, area: Rect, app: &App, compact: bool) {
    let hint = if compact {
        " Enter open · l session · ⌫ back "
    } else {
        " Enter open · l session · x unlink · L link · Esc back "
    };
    let by_project = app.grouping == crate::projects::Grouping::Projects;
    let title = if by_project {
        " pull requests · by project "
    } else {
        " pull requests · every session "
    };
    let block = Block::bordered()
        .title(Line::styled(title, super::bold()))
        .title_bottom(Line::styled(hint, super::dim()).right_aligned())
        .border_style(super::border(app, Focus::AllPrs));
    let prs = app.all_prs();
    if prs.is_empty() {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let hint = Line::styled(
            "No pull requests are linked to any session yet.",
            super::dim(),
        );
        frame.render_widget(hint.centered(), super::centered(inner, inner.width, 1));
        return;
    }
    let number_width = number_width(prs.iter().map(|(_, pr)| *pr));
    let selected = app.pr_index();
    let mut items = Vec::new();
    let mut selected_item = 0;
    let mut previous = None;
    let mut project = None;
    for (index, (key, pr)) in prs.iter().enumerate() {
        if by_project && (index == 0 || project != app.project_of(key)) {
            project = app.project_of(key);
            if !items.is_empty() {
                items.push(ListItem::new(""));
            }
            let name = project
                .as_ref()
                .map_or_else(|| "no project yet".to_owned(), |p| app.project_name(p));
            items.push(ListItem::new(Line::from(vec![
                Span::styled("◆ ", Style::new().fg(Color::Cyan)),
                Span::styled(name, super::bold()),
            ])));
            previous = None;
        }
        if previous != Some(*key) {
            previous = Some(*key);
            let machine = app
                .machines
                .iter()
                .find(|machine| machine.host_id == key.host_id)
                .map_or("", |machine| machine.name.as_str());
            let mut heading = vec![
                Span::styled(format!("{machine} · "), super::dim()),
                Span::styled(
                    app.sessions
                        .get(*key)
                        .map(Session::title)
                        .unwrap_or_default(),
                    super::bold(),
                ),
            ];
            if let Some(session) = app.sessions.get(*key) {
                let (label, style) = super::sessions::badge(session.status);
                heading.push(Span::raw("  "));
                heading.push(Span::styled(label, style));
            }
            if !items.is_empty() && !by_project {
                items.push(ListItem::new(""));
            }
            if by_project {
                heading.insert(0, Span::raw("  "));
            }
            items.push(ListItem::new(Line::from(heading)));
        }
        if index == selected {
            selected_item = items.len();
        }
        let mut line = row(pr, number_width, compact);
        line.spans
            .insert(0, Span::raw(if by_project { "    " } else { "  " }));
        items.push(ListItem::new(line));
    }
    let mut state = ListState::default().with_selected(Some(selected_item));
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, area, &mut state);
}

/// The session-list badge: the first open PRs by number with their checks, then a count of
/// the rest; `compact` names only the first. Empty for a session with no PRs.
pub(super) fn badge(session: &Session, compact: bool) -> Vec<Span<'static>> {
    let named = if compact { 1 } else { BADGE_PRS };
    let mut prs: Vec<&PullRequest> = session.prs.iter().collect();
    // Live PRs first; the sort is stable, so each group keeps its link order.
    prs.sort_by_key(|pr| matches!(pr.state, PrState::Merged | PrState::Closed));
    let mut spans = Vec::new();
    for pr in prs.iter().take(named) {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("#{}", pr.number),
            state_style(pr.state),
        ));
        if live(pr) {
            let (mark, style) = ci(pr.ci);
            if pr.ci != CiStatus::None {
                spans.push(Span::styled(mark, style));
            }
            if pr.mergeable == Mergeable::Conflicting || pr.review == ReviewStatus::ChangesRequested
            {
                spans.push(Span::styled("!", Style::new().fg(Color::Red)));
            }
        }
    }
    if let Some(more) = prs.len().checked_sub(named).filter(|more| *more > 0) {
        spans.push(Span::styled(format!(" +{more}"), super::dim()));
    }
    spans
}

/// The link prompt, over everything else.
pub(super) fn prompt(frame: &mut Frame, area: Rect, app: &App) {
    let Some(prompt) = &app.prs.prompt else {
        return;
    };
    let title = app
        .sessions
        .get(&prompt.key)
        .map(Session::title)
        .unwrap_or_default();
    let popup = super::centered(area, area.width.min(64), 3);
    let block = Block::bordered()
        .border_style(Style::new().fg(Color::Cyan))
        .title(Line::from(vec![
            Span::raw(" link a pull request to "),
            Span::styled(title, super::bold()),
            Span::raw(" "),
        ]))
        .title_bottom(Line::styled(" Enter link · Esc cancel ", super::dim()).right_aligned());
    const LABEL: &str = "number or URL: ";
    // Keep the end of a long URL in view.
    let room = usize::from(block.inner(popup).width).saturating_sub(LABEL.len() + 1);
    let typed = prompt.text.chars().count();
    let shown: String = prompt
        .text
        .chars()
        .skip(typed.saturating_sub(room))
        .collect();
    let line = Line::from(vec![
        Span::styled(LABEL, super::dim()),
        Span::raw(shown),
        Span::styled("▌", Style::new().fg(Color::Gray)),
    ]);
    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(line).block(block), popup);
}

/// One PR on one line: number, state, checks, review, mergeability, title. `compact` keeps
/// the number, the checks' mark, a `!` for a conflict or requested changes, and the title.
fn row(pr: &PullRequest, number_width: usize, compact: bool) -> Line<'static> {
    let state = state_style(pr.state);
    let (state_label, review, merge) = match pr.state {
        PrState::Open => ("open", review(pr.review), merge(pr.mergeable)),
        PrState::Draft => ("draft", review(pr.review), merge(pr.mergeable)),
        PrState::Merged => ("merged", blank(), blank()),
        PrState::Closed => ("closed", blank(), blank()),
    };
    let (ci_mark, mut ci_style) = ci(pr.ci);
    if !live(pr) {
        ci_style = super::dim();
    }
    if compact {
        let number = format!("{:<number_width$} ", format!("#{}", pr.number));
        let mut spans = vec![
            Span::styled(number, state.add_modifier(Modifier::BOLD)),
            Span::styled(format!("{ci_mark} "), ci_style),
        ];
        if live(pr)
            && (pr.mergeable == Mergeable::Conflicting
                || pr.review == ReviewStatus::ChangesRequested)
        {
            spans.push(Span::styled("! ", Style::new().fg(Color::Red)));
        }
        spans.push(Span::raw(pr.title.clone()));
        return Line::from(spans);
    }
    let ci_text = if pr.ci == CiStatus::None {
        "  –  ".to_owned()
    } else {
        format!("{ci_mark} ci  ")
    };
    Line::from(vec![
        Span::styled(
            format!("{:<number_width$} ", format!("#{}", pr.number)),
            state.add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("{state_label:<7}"), state),
        Span::styled(ci_text, ci_style),
        Span::styled(format!("{:<11}", review.0), review.1),
        Span::styled(format!("{:<11}", merge.0), merge.1),
        Span::raw(pr.title.clone()),
        Span::styled(
            pr.head_branch
                .as_ref()
                .map_or_else(String::new, |branch| format!("  {branch}")),
            super::dim(),
        ),
    ])
}

/// Whether the PR is still open, as a draft or not: only then do checks and reviews matter.
fn live(pr: &PullRequest) -> bool {
    matches!(pr.state, PrState::Open | PrState::Draft)
}

fn number_width<'a>(prs: impl Iterator<Item = &'a PullRequest>) -> usize {
    prs.map(|pr| pr.number.to_string().len() + 1)
        .max()
        .unwrap_or(0)
}

fn state_style(state: PrState) -> Style {
    match state {
        PrState::Open => Style::new().fg(Color::Green),
        PrState::Draft => Style::new().fg(Color::Gray),
        PrState::Merged => Style::new().fg(Color::Magenta),
        PrState::Closed => Style::new().fg(Color::Red),
    }
}

fn ci(ci: CiStatus) -> (&'static str, Style) {
    match ci {
        CiStatus::Passing => ("✓", Style::new().fg(Color::Green)),
        CiStatus::Failing => ("✗", Style::new().fg(Color::Red)),
        CiStatus::Pending => ("…", Style::new().fg(Color::Yellow)),
        CiStatus::None => ("–", super::dim()),
    }
}

fn review(review: ReviewStatus) -> (&'static str, Style) {
    match review {
        ReviewStatus::Approved => ("✓ approved", Style::new().fg(Color::Green)),
        ReviewStatus::ChangesRequested => ("✗ changes", Style::new().fg(Color::Red)),
        ReviewStatus::Required => ("… review", Style::new().fg(Color::Yellow)),
        ReviewStatus::None => ("", super::dim()),
    }
}

fn merge(mergeable: Mergeable) -> (&'static str, Style) {
    match mergeable {
        Mergeable::Clean => ("✓ merge", Style::new().fg(Color::Green)),
        Mergeable::Conflicting => ("✗ conflict", Style::new().fg(Color::Red)),
        Mergeable::Unknown => ("? merge", super::dim()),
    }
}

fn blank() -> (&'static str, Style) {
    ("", Style::new())
}
