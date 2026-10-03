//! The sidebar, as Herdr's: the brand with the inbox count and `«`; *projects*, the tree of
//! each project's sessions and their tasks (or with `v` each machine's, a vault's under each
//! host), every row with its rolled-up state; *attention*, every session with something
//! going on, the most pressing first; and a footer with `+ new` and `≡ menu`.
//!
//! ```text
//!  herder         inbox 2 «
//!
//!  projects      by project
//!  ◉ app                 1
//!    ├ ✓ fix-login
//!    └ ◉ api             ?
//!  ───────────────────────
//!  attention       priority
//!  ◉ api · app         box
//!  ───────────────────────
//!  + new            ≡ menu
//! ```
//!
//! Collapsed (`ctrl+x b`), it is a strip of the projects' and the attention rows' glyphs. On
//! a phone the same tree and list fill the screen as the switcher ([`switcher`]), with the
//! views and the menu under them.

use herder_client_core::ConnectionState;
use herder_protocol::{FleetHost, SessionStatus, Timestamp};
use ratatui::Frame;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::account_screen;
use crate::action::Action;
use crate::app::{App, Focus, Row};
use crate::compose::Act;
use crate::inbox::InboxAction;
use crate::mouse::{self, Click, Hits, List as Rows, Wheel};
use crate::projects::Grouping;
use crate::prs::PrAction;
use crate::session::SessionKey;
use crate::ui::list::{ListView, Row as Item};
use crate::ui::state::{self, State};
use crate::ui::{INSET, Ui, line_width};

/// Sidebars at least this wide name the machines and PRs on their rows.
const ROOMY: u16 = 30;

/// The desktop sidebar in `area`, or its collapsed strip.
pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App, hits: &mut Hits) {
    if app.layout.collapsed {
        strip(frame, area, app, hits);
        return;
    }
    let rows = app.rows();
    let attention = app.attention();
    let [top, _, middle, rule, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    header(frame, top, app, hits);

    // The tree takes what it needs and a blank row; attention the rest. When both do not
    // fit, attention gets up to half.
    let archived = u16::from(app.archived_count() > 0);
    let tree_need = u16::try_from(rows.len() + 2).unwrap_or(u16::MAX) + archived;
    let attention_need = match attention.len() {
        0 => 0,
        n => u16::try_from(n + 2).unwrap_or(u16::MAX),
    };
    // Attention shows only with a row of its own under its heading and rule.
    let tree_height = if tree_need + attention_need <= middle.height {
        tree_need
    } else if middle.height / 2 >= 3 {
        middle
            .height
            .saturating_sub(attention_need.min(middle.height / 2))
    } else {
        middle.height
    };
    let [tree, attention_area] =
        Layout::vertical([Constraint::Length(tree_height), Constraint::Fill(1)]).areas(middle);
    let roomy = area.width >= ROOMY;
    project_tree(frame, tree, app, &rows, roomy, hits);
    let ui = app.ui();
    if !attention.is_empty() && attention_area.height > 1 {
        let [rule, list] =
            Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(attention_area);
        divider(frame, rule, ui);
        let mut items = vec![Item::header("attention").right("priority")];
        items.extend(
            attention
                .iter()
                .map(|key| attention_item(app, key, list_room(list))),
        );
        let mut offset = 0;
        let placed = ListView::new(ui, items).render(list, frame.buffer_mut(), &mut offset);
        for (at, rect) in placed {
            hits.click(rect, Click::Row(Rows::Attention, at - 1));
        }
    }
    divider(frame, rule, ui);
    let new = Span::styled("+ new", ui.accent());
    let menu = Span::styled(ui.glyphs.menu, ui.accent());
    let [new_area, menu_area] = ends(footer, &new, &menu);
    frame.render_widget(new, new_area);
    frame.render_widget(menu, menu_area);
    hits.click(new_area, Click::Act(Action::Compose(Act::NewSession)));
    hits.click(menu_area, Click::Act(Action::Compose(Act::Palette)));
}

/// The top row: the brand, the inbox count and `«`, which collapses the sidebar.
fn header(frame: &mut Frame, area: Rect, app: &App, hits: &mut Hits) {
    let ui = app.ui();
    let waiting = app.waiting().len();
    let inbox_style = if waiting > 0 {
        state::style(ui, State::NeedsYou)
    } else {
        ui.muted()
    };
    let brand = Span::styled("herder", ui.strong());
    let inbox = Span::styled(format!("inbox {waiting}"), inbox_style);
    let collapse = Span::styled(ui.glyphs.collapse, ui.accent());
    let [brand_area, collapse_area] = ends(area, &brand, &collapse);
    frame.render_widget(brand, brand_area);
    frame.render_widget(collapse, collapse_area);
    let inbox_width = u16::try_from(inbox.width()).unwrap_or(0);
    let inbox_area = Rect {
        x: collapse_area.x.saturating_sub(inbox_width + 1),
        width: inbox_width,
        ..collapse_area
    };
    if inbox_area.x > brand_area.right() {
        frame.render_widget(inbox, inbox_area);
        hits.click(inbox_area, Click::Act(Action::Inbox(InboxAction::Toggle)));
    }
    hits.click(collapse_area, Click::Act(Action::ToggleSidebar));
}

/// The project tree in `area`: a heading, then a row per project, machine, host or session.
fn project_tree(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    rows: &[Row],
    roomy: bool,
    hits: &mut Hits,
) {
    let ui = app.ui();
    let grouping = match app.grouping {
        Grouping::Projects => "by project",
        Grouping::Machines => "by machine",
    };
    let label = match app.grouping {
        Grouping::Projects => "projects",
        Grouping::Machines => "machines",
    };
    let archived = app.archived_count();
    // Under the tree, how many sessions are archived, and the switch that shows them.
    let (area, archived_row) = if archived > 0 && area.height > 2 {
        let [list, row, _] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);
        (list, Some(row))
    } else {
        (area, None)
    };
    if let Some(row) = archived_row {
        let text = Rect {
            x: row.x + INSET,
            width: row.width.saturating_sub(2 * INSET),
            ..row
        };
        let toggle = if app.show_archived {
            "H hide"
        } else {
            "H show"
        };
        let line = crate::ui::spread(
            Line::from(Span::styled(format!("{archived} archived"), ui.muted())),
            Line::from(Span::styled(toggle, ui.accent())),
            usize::from(text.width),
            ui.glyphs,
        );
        frame.render_widget(line, text);
        hits.click(row, Click::Act(Action::ToggleArchived));
    }
    let mut items = vec![Item::header(label).right(grouping)];
    items.extend(tree_items(app, rows, roomy, roomy));
    let selected = app.selected_index(rows).map(|at| at + 1);
    let mut offset = app.tree_offset;
    let placed = ListView::new(ui, items)
        .select(selected)
        .focused(app.focus == Focus::Sessions)
        .render(area, frame.buffer_mut(), &mut offset);
    app.tree_offset = offset;
    hits.wheel(area, Wheel::Sessions);
    for (at, rect) in placed {
        hits.click(rect, Click::Row(Rows::Sessions, at - 1));
    }
    // The heading's grouping switches it.
    if offset == 0 && area.height > 0 {
        let width = u16::try_from(grouping.len()).unwrap_or(0);
        let right = Rect::new(area.right().saturating_sub(INSET + width), area.y, width, 1);
        hits.click(right, Click::Act(Action::Group));
    }
}

/// The collapsed sidebar: `»`, then a glyph per project (or machine) and per attention row.
fn strip(frame: &mut Frame, area: Rect, app: &App, hits: &mut Hits) {
    let ui = app.ui();
    let spot = |y| Rect::new(area.x, y, area.width, 1);
    let glyph = |y| {
        Rect::new(
            area.x + INSET,
            y,
            2.min(area.width.saturating_sub(INSET)),
            1,
        )
    };
    frame.render_widget(Span::styled(ui.glyphs.expand, ui.accent()), glyph(area.y));
    hits.click(spot(area.y), Click::Act(Action::ToggleSidebar));
    let mut y = area.y + 2;
    for (at, row) in app.rows().iter().enumerate() {
        if !matches!(row, Row::Project(_) | Row::Machine(_)) || y >= area.bottom() {
            continue;
        }
        frame.render_widget(state::dot(ui, app.row_state(row)), glyph(y));
        hits.click(spot(y), Click::Row(Rows::Sessions, at));
        y += 1;
    }
    let attention = app.attention();
    if attention.is_empty() || y + 2 > area.bottom() {
        return;
    }
    divider(frame, spot(y + 1), ui);
    y += 2;
    for (at, key) in attention.iter().enumerate() {
        if y >= area.bottom() {
            break;
        }
        frame.render_widget(state::dot(ui, app.state(key)), glyph(y));
        hits.click(spot(y), Click::Row(Rows::Attention, at));
        y += 1;
    }
}

/// The phone's switcher, full screen: attention, the project tree with `+ new session`, the
/// views and the menu, each a tap; the keys move over the tree.
pub(super) fn switcher(frame: &mut Frame, area: Rect, app: &mut App, hits: &mut Hits) {
    let ui = app.ui();
    let rows = app.rows();
    let attention = app.attention();
    let mut items = Vec::new();
    let mut clicks: Vec<Option<Click>> = Vec::new();
    if !attention.is_empty() {
        items.push(Item::header("attention"));
        clicks.push(None);
        for (at, key) in attention.iter().enumerate() {
            items.push(attention_item(app, key, list_room(area)));
            clicks.push(Some(Click::Row(Rows::Attention, at)));
        }
        items.push(Item::Gap);
        clicks.push(None);
    }
    let label = match app.grouping {
        Grouping::Projects => "projects",
        Grouping::Machines => "machines",
    };
    items.push(Item::header(label));
    clicks.push(Some(Click::Act(Action::Group)));
    let first_row = items.len();
    items.extend(tree_items(app, &rows, true, false));
    clicks.extend((0..rows.len()).map(|at| Some(Click::Row(Rows::Sessions, at))));
    items.push(Item::item(Span::styled("+ new session", ui.accent())));
    clicks.push(Some(Click::Act(Action::Compose(Act::NewSession))));
    items.push(Item::Gap);
    clicks.push(None);

    // The views and the menu: a row of words, each its own tap.
    let waiting = app.waiting().len();
    let prs = app.all_prs().len();
    let views = [
        (
            format!("inbox {waiting}"),
            Click::Act(Action::Inbox(InboxAction::Toggle)),
        ),
        (
            format!("prs {prs}"),
            Click::Act(Action::Pr(PrAction::ToggleAll)),
        ),
        ("accounts".to_owned(), Click::Act(Action::OpenAccounts)),
        ("fleet".to_owned(), Click::Act(Action::OpenMachines)),
    ];
    let menu = [
        ("keys".to_owned(), Click::Act(Action::ToggleHelp)),
        ("reconnect".to_owned(), mouse::key(KeyCode::Char('r'))),
        ("quit".to_owned(), Click::Act(Action::Quit)),
    ];
    let words_row = |words: &[(String, Click)]| {
        let spans: Vec<Span> = words
            .iter()
            .flat_map(|(word, _)| [Span::styled(word.clone(), ui.accent()), Span::raw("   ")])
            .collect();
        Item::item(Line::from(spans))
    };
    items.push(Item::header("views"));
    clicks.push(None);
    let views_at = items.len();
    items.push(words_row(&views));
    clicks.push(None);
    items.push(Item::Gap);
    clicks.push(None);
    items.push(Item::header("menu"));
    clicks.push(None);
    let menu_at = items.len();
    items.push(words_row(&menu));
    clicks.push(None);

    let selected = app.selected_index(&rows).map(|at| at + first_row);
    let mut offset = app.tree_offset;
    let placed = ListView::new(ui, items)
        .select(selected)
        .focused(true)
        .render(area, frame.buffer_mut(), &mut offset);
    app.tree_offset = offset;
    hits.wheel(area, Wheel::Sessions);
    for (at, rect) in placed {
        let words = if at == views_at {
            &views[..]
        } else if at == menu_at {
            &menu[..]
        } else {
            if let Some(click) = clicks[at].clone() {
                hits.click(rect, click);
            }
            continue;
        };
        // Past the pointer column and its space, as the list draws an item's text.
        let mut x = rect.x + 2;
        for (word, click) in words {
            let width = u16::try_from(word.len()).unwrap_or(0);
            hits.click(Rect::new(x, rect.y, width, 1), click.clone());
            x += width + 3;
        }
    }
}

/// A list item per tree row: its guides (`├ └ │`), state dot, name and meta; `roomy` names
/// the machines of a project and the host offline since when, `prs` each session's first PR.
fn tree_items(app: &App, rows: &[Row], roomy: bool, prs: bool) -> Vec<Item<'static>> {
    let ui = app.ui();
    let levels = levels(rows);
    let lasts = lasts(&levels);
    // Whether each level's open branch ended, for the `│` of the levels below.
    let mut ended: Vec<bool> = Vec::new();
    rows.iter()
        .zip(levels.iter().zip(&lasts))
        .map(|(row, (level, last))| {
            ended.truncate(*level);
            let mut spans = Vec::new();
            if *level > 0 {
                let mut guides = String::from("  ");
                for done in ended.iter().skip(1) {
                    guides.push_str(if *done { "  " } else { ui.glyphs.pipe });
                    if !*done {
                        guides.push(' ');
                    }
                }
                guides.push_str(if *last {
                    ui.glyphs.last
                } else {
                    ui.glyphs.branch
                });
                guides.push(' ');
                spans.push(Span::styled(guides, Style::new().fg(ui.theme.border)));
            }
            ended.resize(*level + 1, false);
            ended[*level] = *last;
            spans.push(state::dot(ui, app.row_state(row)));
            spans.push(Span::raw(" "));
            let (name, right) = row_text(app, row, roomy, prs);
            spans.push(name);
            Item::item(Line::from(spans)).right(right)
        })
        .collect()
}

/// Each row's level in the tree: projects and machines at 0, a vault's hosts at 1, sessions
/// under them, tasks under their session.
fn levels(rows: &[Row]) -> Vec<usize> {
    let mut base = 0;
    rows.iter()
        .map(|row| match row {
            Row::Machine(_) | Row::Project(_) => {
                base = 1;
                0
            }
            Row::Host { .. } => {
                base = 2;
                1
            }
            Row::Session { depth, .. } => base + depth,
        })
        .collect()
}

/// Whether each row is the last of its level under its parent.
fn lasts(levels: &[usize]) -> Vec<bool> {
    levels
        .iter()
        .enumerate()
        .map(|(at, level)| {
            levels[at + 1..]
                .iter()
                .find(|next| *next <= level)
                .is_none_or(|next| next < level)
        })
        .collect()
}

/// A tree row's name, and the meta at its right end.
fn row_text(app: &App, row: &Row, roomy: bool, prs: bool) -> (Span<'static>, Line<'static>) {
    let ui = app.ui();
    let needing = |row: &Row| match app.row_needing(row) {
        0 => Vec::new(),
        n => vec![Span::styled(
            n.to_string(),
            state::style(ui, State::NeedsYou),
        )],
    };
    match row {
        Row::Project(project) => {
            let name = project
                .as_ref()
                .map_or_else(|| "no project yet".to_owned(), |p| app.project_name(p));
            let mut right = Vec::new();
            if roomy {
                let on: Vec<String> = app
                    .machines
                    .iter()
                    .filter(|m| {
                        app.row_sessions(row)
                            .iter()
                            .any(|key| key.host_id == m.host_id)
                    })
                    .map(|m| m.name.clone())
                    .collect();
                if !on.is_empty() {
                    right.push(Span::styled(on.join(ui.glyphs.separator), ui.muted()));
                }
            }
            push_spaced(&mut right, needing(row));
            (Span::styled(name, ui.text()), Line::from(right))
        }
        Row::Machine(host_id) => {
            let machine = app.machines.iter().find(|m| m.host_id == *host_id);
            let name = machine.map_or_else(|| host_id.to_string(), |m| m.name.clone());
            let right = match machine.map(|m| &m.connection) {
                Some(ConnectionState::Connected) | None => needing(row),
                Some(ConnectionState::Connecting) => {
                    vec![Span::styled(
                        "connecting",
                        Style::new().fg(ui.theme.warning),
                    )]
                }
                Some(ConnectionState::Disconnected { .. }) => {
                    vec![Span::styled("offline", Style::new().fg(ui.theme.error))]
                }
            };
            (Span::styled(name, ui.text()), Line::from(right))
        }
        Row::Host { vault, host } => {
            let host = app
                .machines
                .iter()
                .find(|m| m.host_id == *vault)
                .and_then(|m| m.hosts.iter().find(|h| h.host_id == *host));
            let Some(host) = host else {
                return (Span::raw(""), Line::default());
            };
            let right = if host.online {
                needing(row)
            } else {
                let mut spans = vec![Span::styled("offline", Style::new().fg(ui.theme.error))];
                if roomy {
                    spans.push(Span::styled(
                        format!("{}{} ago", ui.glyphs.separator, ago(host)),
                        ui.muted(),
                    ));
                }
                spans
            };
            (
                Span::styled(host.host_name.clone(), ui.text()),
                Line::from(right),
            )
        }
        Row::Session { key, .. } => session_text(app, key, prs),
    }
}

/// A session row's name and meta: where it moved, how many tasks a fold hides, with `prs` its
/// first PR, how many of its tasks need the user, and what it waits on (`?` a question, `!`
/// an approval).
fn session_text(app: &App, key: &SessionKey, prs: bool) -> (Span<'static>, Line<'static>) {
    let ui = app.ui();
    let Some(session) = app.sessions.get(key) else {
        return (Span::raw(""), Line::default());
    };
    let name = match app.grouping {
        Grouping::Projects => super::projects::session_name(session, true),
        Grouping::Machines => session.short_title(),
    };
    let mut style = ui.text();
    if app.open.as_ref() == Some(key) {
        style = ui.strong();
    }
    if session.status == SessionStatus::Archived {
        style = ui.muted();
    }
    let mut right = Vec::new();
    // A vault's read-only copy, standing in for a host paired here but not reachable.
    if app.vault_copy(key) {
        right.push(Span::styled("vault", ui.muted()));
    }
    if let Some(to) = app.moved_to(key) {
        right.push(Span::styled(
            format!("{} {to}", ui.glyphs.state(State::Moved)),
            state::style(ui, State::Moved),
        ));
    }
    if app.folded.contains(key) {
        let hidden = app.subtree(key).len() - 1;
        right.push(Span::styled(
            format!("{}{hidden}", ui.glyphs.folded),
            ui.muted(),
        ));
    }
    if prs && !session.prs.is_empty() {
        let mut prs = super::prs::badge(ui, session, true);
        if let Some(first) = prs.first_mut().filter(|span| span.content == " ") {
            first.content = "".into();
        }
        push_spaced(&mut right, prs);
    }
    // How many of its tasks need the user, then what it asks itself.
    let needing = app.row_needing(&Row::Session {
        key: key.clone(),
        depth: 0,
    });
    if needing > 0 {
        push_spaced(
            &mut right,
            vec![Span::styled(
                needing.to_string(),
                state::style(ui, State::NeedsYou),
            )],
        );
    }
    if let Some((mark, _)) = super::composer::waiting_glyph(session) {
        push_spaced(
            &mut right,
            vec![Span::styled(mark, state::style(ui, State::NeedsYou))],
        );
    }
    (Span::styled(name, style), Line::from(right))
}

/// An attention row: dot, `name · project`, and the machine at the right end where there is
/// room; on a narrower list, as much of `name · project · machine` as fits `room` columns
/// whole.
fn attention_item(app: &App, key: &SessionKey, room: u16) -> Item<'static> {
    let ui = app.ui();
    let Some(session) = app.sessions.get(key) else {
        return Item::item("");
    };
    let mut parts = vec![Span::styled(
        super::projects::session_name(session, true),
        ui.text(),
    )];
    if let Some(project) = app.project_of(key) {
        parts.push(Span::styled(app.project_name(&project), ui.muted()));
    }
    let machine = app
        .machines
        .iter()
        .find(|m| m.host_id == key.host_id)
        .map_or_else(|| key.host_id.to_string(), |m| m.name.clone());
    let machine = Span::styled(machine, ui.muted());
    let line = |parts: Vec<Span<'static>>| {
        let mut spans = vec![state::dot(ui, app.state(key)), Span::raw(" ")];
        spans.extend(ui.joined(parts));
        Line::from(spans)
    };
    let room = usize::from(room);
    if room >= usize::from(ROOMY) - 3 {
        return Item::item(line(parts)).right(machine);
    }
    parts.push(machine);
    while parts.len() > 1 && line_width(&line(parts.clone())) > room {
        parts.pop();
    }
    Item::item(line(parts))
}

/// Columns an item's text has in a list `area` wide: past the pointer, one in from the end.
fn list_room(area: Rect) -> u16 {
    area.width.saturating_sub(2 + INSET)
}

/// `spans` after what `line` holds, a space between.
fn push_spaced(line: &mut Vec<Span<'static>>, spans: Vec<Span<'static>>) {
    if spans.is_empty() {
        return;
    }
    if !line.is_empty() {
        line.push(Span::raw(" "));
    }
    line.extend(spans);
}

/// Where `left` and `right` go on a row: one column in from either end.
fn ends(area: Rect, left: &Span, right: &Span) -> [Rect; 2] {
    let width = |span: &Span| u16::try_from(span.width()).unwrap_or(u16::MAX);
    let start = area.x + INSET.min(area.width);
    let end = area.right().saturating_sub(INSET);
    let right_width = width(right).min(end.saturating_sub(start));
    [
        Rect::new(start, area.y, width(left).min(end - start), 1),
        Rect::new(end - right_width, area.y, right_width, 1),
    ]
}

/// A rule across `area`'s first row, one column in from either end.
fn divider(frame: &mut Frame, area: Rect, ui: Ui) {
    let inner = Rect::new(
        area.x + INSET,
        area.y,
        area.width.saturating_sub(2 * INSET),
        1,
    );
    let rule = "─".repeat(usize::from(inner.width));
    frame.render_widget(
        Span::styled(rule, Style::new().fg(ui.theme.border_subtle)),
        inner,
    );
}

/// How long ago the vault last heard from `host`.
pub(super) fn ago(host: &FleetHost) -> String {
    let secs = Timestamp::now().duration_since(host.last_seen).as_secs();
    account_screen::until(secs)
}

/// `text` cut to `width` characters, with an ellipsis when cut.
pub(super) fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let mut clipped: String = text.chars().take(width.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
}
