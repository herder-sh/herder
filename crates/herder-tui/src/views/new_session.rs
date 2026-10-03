//! The new-session dialog, over everything else: the project picker, the machine picker, then
//! the form.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::action::Action;
use crate::app::App;
use crate::mouse::{Click, Hits};
use crate::new_session::{Choice, Field, Input, Step};
use crate::session::mode_name;
use crate::ui::dialog::{Dialog, Size};
use crate::ui::glyphs::Glyphs;
use crate::ui::hints::Hint;
use crate::ui::input::{Choice as ChoiceField, Field as TextField};
use crate::ui::list::Row;
use crate::ui::select::{Select, label_width};
use crate::ui::{Ui, fit};

/// Labels of the form, so its values line up.
const LABEL: &str = "account";

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App, hits: &mut Hits) {
    let Some(dialog) = &app.compose.dialog else {
        return;
    };
    let step = dialog.step;
    let theme = app.theme.clone();
    let ui = Ui::new(&theme, Glyphs::for_width(app.glyphs, app.width));
    let project = dialog.project.as_ref().map(|p| app.project_name(p));
    let machine = app
        .machines
        .iter()
        .find(|m| m.host_id == dialog.host_id)
        .map_or_else(|| dialog.host_id.to_string(), |m| m.name.clone());
    let mut parts = vec!["new session".to_owned()];
    if step != Step::Project {
        parts.push(project.unwrap_or_else(|| "by path".to_owned()));
    }
    if step == Step::Form {
        parts.push(machine);
    }
    let title = Line::from(ui.joined(parts.into_iter().map(Span::raw)));
    match step {
        Step::Form => form(frame, area, app, ui, title, hits),
        _ => picker(frame, area, app, ui, title, hits),
    }
}

/// The project or machine picker.
fn picker(frame: &mut Frame, area: Rect, app: &mut App, ui: Ui, title: Line, hits: &mut Hits) {
    let choices = app.new_session_choices();
    let rows: Vec<Row> = choices.iter().map(|choice| row(app, ui, choice)).collect();
    let Some(dialog) = &mut app.compose.dialog else {
        return;
    };
    let cursor = dialog.selected.min(choices.len().saturating_sub(1));
    let (hints, empty): (&[Hint], _) = if dialog.step == Step::Project {
        (
            &[Hint::new("enter", "choose"), Hint::new("esc", "close")],
            "no project matches",
        )
    } else {
        (
            &[
                Hint::new("enter", "choose"),
                Hint::new("⌫", "back"),
                Hint::new("esc", "close"),
            ],
            "no machine matches",
        )
    };
    let heading = if dialog.step == Step::Project {
        "project"
    } else {
        "machine"
    };
    let mut rows = rows;
    if !rows.is_empty() {
        rows.insert(0, Row::header(heading));
    }
    let placed = Select::new(ui, title, Size::Medium, &mut dialog.search)
        .rows(rows, (!choices.is_empty()).then_some(cursor + 1))
        .empty(empty)
        .hints(hints)
        .render(area, frame.buffer_mut(), &mut dialog.offset);
    super::palette::dialog_taps(hits, area, &placed.dialog);
    for (row, rect) in placed.rows {
        if let Some(at) = row.checked_sub(1) {
            hits.click(rect, Click::Act(Action::NewSession(Input::Pick(at))));
        }
    }
}

/// A row of the picker: a project and the machines it has clones on; a machine and the
/// clone's path there.
fn row<'a>(app: &App, ui: Ui, choice: &Choice) -> Row<'a> {
    match choice {
        Choice::Project(project) => {
            let machines: Vec<String> = app
                .clones(project)
                .into_iter()
                .filter_map(|clone| {
                    app.machines
                        .iter()
                        .find(|m| m.host_id == clone.host_id)
                        .map(|m| m.name.clone())
                })
                .fold(Vec::new(), |mut names, name| {
                    if !names.contains(&name) {
                        names.push(name);
                    }
                    names
                });
            Row::item(Line::from(vec![
                Span::styled(app.project_name(project), ui.text()),
                Span::raw("  "),
                Span::styled(project.as_str().to_owned(), ui.muted()),
            ]))
            .right(Line::from(
                ui.joined(
                    machines
                        .into_iter()
                        .map(|name| Span::styled(name, ui.muted())),
                ),
            ))
        }
        Choice::ByPath => Row::item(Line::from(vec![
            Span::styled("+ ", ui.accent()),
            Span::styled("a repository by path", ui.text()),
        ])),
        Choice::Machine(host_id, repo) => {
            let machine = app.machines.iter().find(|m| m.host_id == *host_id);
            let name = machine.map_or_else(|| host_id.to_string(), |m| m.name.clone());
            let (mark, color) = machine.map_or((ui.glyphs.disconnected, ui.theme.error), |m| {
                super::add_machine::connection_mark(ui, m)
            });
            let mut left = vec![
                Span::styled(mark, Style::new().fg(color)),
                Span::raw(" "),
                Span::styled(name, ui.text()),
            ];
            if let Some(repo) = repo {
                left.push(Span::raw("  "));
                left.push(Span::styled(repo.clone(), ui.muted()));
            }
            let project = app.compose.dialog.as_ref().and_then(|d| d.project.clone());
            let account = machine
                .and_then(|m| {
                    m.accounts
                        .get(app.default_account(host_id, project.as_ref()))
                })
                .map(|account| account.label.clone())
                .unwrap_or_else(|| "no accounts".to_owned());
            Row::item(Line::from(left)).right(Span::styled(account, ui.muted()))
        }
    }
}

/// The form: repo, account, model and mode.
fn form(frame: &mut Frame, area: Rect, app: &mut App, ui: Ui, title: Line, hits: &mut Hits) {
    let Some(dialog) = &app.compose.dialog else {
        return;
    };
    let machine = app.machines.iter().find(|m| m.host_id == dialog.host_id);
    let account = machine.and_then(|m| m.accounts.get(dialog.account));
    let account = match account {
        Some(account) => Line::from(vec![
            Span::styled(account.label.clone(), ui.text()),
            Span::styled(format!("  {}", account.provider.as_str()), ui.muted()),
        ]),
        None => Line::styled("no accounts", ui.muted()),
    };
    let (note, note_style) = match (&dialog.error, dialog.sending) {
        (Some(error), _) => (error.clone(), Style::new().fg(ui.theme.error)),
        (None, true) => (format!("creating{}", ui.glyphs.ellipsis), ui.muted()),
        (None, false) => (
            "its own worktree and branch, on that clone".to_owned(),
            ui.muted(),
        ),
    };
    // The note wraps rather than being cut.
    let room = usize::from(
        Size::Medium
            .width()
            .min(area.width)
            .saturating_sub(2 + 2 * crate::ui::dialog::PAD_X),
    )
    .max(8);
    let note: Vec<String> = textwrap::wrap(&note, room)
        .into_iter()
        .map(|line| line.into_owned())
        .collect();
    let hints = [
        Hint::new("enter", "create"),
        Hint::new("tab", "next"),
        Hint::new("←/→", "change"),
        Hint::new("esc", "close"),
    ];
    let narrow = area.width < super::NARROW;
    // Each field, a blank row between, then the note.
    let gap = u16::from(!narrow);
    let body = 4 + 3 * gap + 1 + u16::try_from(note.len()).unwrap_or(1);
    let areas = Dialog::new(ui, title, Size::Medium)
        .hints(if narrow { &hints[..1] } else { &hints })
        .render(area, body, frame.buffer_mut());
    super::palette::dialog_taps(hits, area, &areas);
    let label = label_width(LABEL);
    let mode = dialog.mode;
    let focus = dialog.field;
    let Some(dialog) = &mut app.compose.dialog else {
        return;
    };
    let buf = frame.buffer_mut();
    let mut y = areas.body.y;
    for field in Field::ALL {
        let row = Rect::new(areas.body.x, y, areas.body.width, 1).intersection(areas.body);
        if row.is_empty() {
            break;
        }
        let focused = focus == field;
        match field {
            Field::Repo => TextField::new(ui, field.label(), &mut dialog.repo)
                .focused(focused)
                .render(row, buf, label),
            Field::Model => TextField::new(ui, field.label(), &mut dialog.model)
                .focused(focused)
                .render(row, buf, label),
            Field::Account | Field::Mode => {
                let value = if field == Field::Account {
                    account.clone()
                } else {
                    Line::styled(mode_name(mode), ui.text())
                };
                let [prev, value] = ChoiceField::new(ui, field.label(), value)
                    .focused(focused)
                    .render(row, buf, label);
                hits.click(row, Click::Act(Action::NewSession(Input::Focus(field))));
                hits.click(
                    value,
                    Click::Act(Action::NewSession(Input::Choose(field, 1))),
                );
                hits.click(
                    prev,
                    Click::Act(Action::NewSession(Input::Choose(field, -1))),
                );
                y += 1 + gap;
                continue;
            }
        }
        hits.click(row, Click::Act(Action::NewSession(Input::Focus(field))));
        y += 1 + gap;
    }
    for (at, line) in (0..).zip(note) {
        let row =
            Rect::new(areas.body.x, y + 1 - gap + at, areas.body.width, 1).intersection(areas.body);
        fit(
            Line::styled(line, note_style),
            usize::from(row.width),
            ui.glyphs,
        )
        .render(row, buf);
    }
}
