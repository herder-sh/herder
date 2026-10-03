//! The open session's tabs: the tab row over the main pane, and the tasks tab.
//!
//! ```text
//!  chat   tasks 2   prs 1   term                     claude-main · opus · ask
//! ```
//!
//! The tab in view is set off on the element background, the others are muted; a tap shows
//! one, the wheel over the row moves along them. `term`, for owners only, opens the terminal
//! picker.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::action::Action;
use crate::app::{App, Focus};
use crate::mouse::{Click, Hits, List as Rows, Wheel};
use crate::nav::Tab;
use crate::terminal;
use crate::ui::list::{ListView, Row as Item};
use crate::ui::state;
use crate::ui::{GAP, line_width};

/// The tab row of the open session in `area`'s first row.
pub(super) fn row(frame: &mut Frame, area: Rect, app: &App, hits: &mut Hits) {
    let (Some(key), Some(session)) = (&app.open, app.open_session()) else {
        return;
    };
    let ui = app.ui();
    let tasks = app.tasks().len();
    let count = |name: &str, n: usize| match n {
        0 => name.to_owned(),
        n => format!("{name} {n}"),
    };
    let mut tabs = vec![
        (Tab::Chat, "chat".to_owned()),
        (Tab::Tasks, count("tasks", tasks)),
        (Tab::Prs, count("prs", session.prs.len())),
    ];
    if terminal::refusal(&app.machines, &key.host_id).is_none() {
        tabs.push((Tab::Term, "term".to_owned()));
    }
    let current = app.tab();
    let mut x = area.x;
    for (tab, label) in tabs {
        let text = format!(" {label} ");
        let width = u16::try_from(text.chars().count()).unwrap_or(u16::MAX);
        if x + width > area.right() {
            break;
        }
        let style = if tab == current {
            Style::new()
                .fg(ui.theme.text)
                .bg(ui.theme.background_element)
                .add_modifier(Modifier::BOLD)
        } else {
            ui.muted()
        };
        let spot = Rect::new(x, area.y, width, 1);
        frame.render_widget(Span::styled(text, style), spot);
        hits.click(spot, Click::Act(Action::Tab(tab)));
        x += width + 1;
    }
    hits.wheel(Rect { height: 1, ..area }, Wheel::Tabs);

    // The session's account, model and mode at the right end, where they fit; in the chat
    // the prompt's meta line says them.
    if current == Tab::Chat {
        return;
    }
    let facts = Line::from(ui.joined(facts(app).into_iter().map(|f| Span::styled(f, ui.muted()))));
    let facts_width = u16::try_from(line_width(&facts)).unwrap_or(u16::MAX);
    let end = area.right();
    let gap = u16::try_from(GAP).unwrap_or(0);
    if x + gap + facts_width <= end {
        frame.render_widget(facts, Rect::new(end - facts_width, area.y, facts_width, 1));
    }
}

/// The open session's account (by its label), model and permission mode.
pub(super) fn facts(app: &App) -> Vec<String> {
    let (Some(key), Some(session)) = (&app.open, app.open_session()) else {
        return Vec::new();
    };
    let mut facts = Vec::new();
    if let Some(id) = &session.account_id {
        facts.push(
            crate::account_screen::find(&app.machines, &key.host_id, id)
                .map_or_else(|| id.to_string(), |account| account.label.clone()),
        );
    }
    if !session.model.is_empty() {
        facts.push(session.model.clone());
        facts.push(crate::session::mode_name(session.permission_mode).to_owned());
    }
    facts
}

/// The tasks tab: each task of the open session with its state, and what it runs on.
pub(super) fn tasks(frame: &mut Frame, area: Rect, app: &mut App, hits: &mut Hits) {
    let tasks = app.tasks();
    if tasks.is_empty() {
        let hint = Line::styled("No tasks: this session has spawned none.", app.ui().muted());
        frame.render_widget(hint, Rect { height: 1, ..area });
        return;
    }
    app.task_cursor = app.task_cursor.min(tasks.len() - 1);
    let ui = app.ui();
    let items: Vec<Item> = tasks
        .iter()
        .filter_map(|key| {
            let session = app.sessions.get(key)?;
            let state = app.state(key);
            let left = Line::from(vec![
                state::dot(ui, state),
                Span::raw(" "),
                Span::styled(session.title(), ui.text()),
            ]);
            let right = Line::from(ui.joined([
                Span::styled(session.branch.clone(), ui.muted()),
                Span::styled(state.label(), state::style(ui, state)),
            ]));
            Some(Item::item(left).right(right))
        })
        .collect();
    let mut offset = 0;
    let placed = ListView::new(ui, items)
        .select(Some(app.task_cursor))
        .focused(app.focus == Focus::Tasks)
        .render(area, frame.buffer_mut(), &mut offset);
    hits.wheel(area, Wheel::Keys);
    for (at, rect) in placed {
        hits.click(rect, Click::Row(Rows::Tasks, at));
    }
}
