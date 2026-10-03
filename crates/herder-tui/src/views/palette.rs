//! The command palette, over everything else; the prompt's `/` popup, above the prompt; and
//! the status line's quit hint while Ctrl-C waits for a second press.

use ratatui::Frame;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Widget};

use crate::action::Action;
use crate::app::App;
use crate::compose::Act;
use crate::mouse::{self, Click, Hits};
use crate::palette::{COMMANDS, Command, Input};
use crate::ui::dialog::Size;
use crate::ui::hints::Hint;
use crate::ui::list::{ListView, Row};
use crate::ui::select::Select;
use crate::ui::{GAP, Ui, fill};

/// Most rows the `/` popup shows.
const SLASH_ROWS: usize = 10;

/// The quit hint while Ctrl-C waits for a second press; `false` when it does not, so the
/// status line shows.
pub(super) fn quit_hint(frame: &mut Frame, area: Rect, app: &App) -> bool {
    if !app.compose.quit_armed {
        return false;
    }
    let ui = app.ui();
    Line::from(vec![
        Span::raw(" "),
        Span::styled("ctrl+c", ui.strong()),
        Span::styled(" again to quit", ui.muted()),
    ])
    .render(area, frame.buffer_mut());
    true
}

/// `/name args`, padded to `pad` columns, then the title: a command as listed.
fn command_line(ui: Ui, command: &Command, prefix: &str, pad: usize) -> Line<'static> {
    let name = format!("{prefix}{}", command.name);
    let width = crate::ui::width(&name)
        + if command.args.is_empty() {
            0
        } else {
            1 + crate::ui::width(command.args)
        };
    let mut spans = vec![Span::styled(name, ui.text())];
    if !command.args.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(command.args, ui.muted()));
    }
    spans.push(Span::raw(" ".repeat(pad.saturating_sub(width).max(GAP))));
    spans.push(Span::styled(command.title, ui.muted()));
    Line::from(spans)
}

/// Columns the name and argument column takes: the widest, and a gap.
fn name_column(prefix: &str) -> usize {
    COMMANDS
        .iter()
        .map(|c| crate::ui::width(prefix) + crate::ui::width(c.name) + crate::ui::width(c.args) + 1)
        .max()
        .unwrap_or(0)
        + GAP
}

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App, hits: &mut Hits) {
    if app.compose.palette.is_none() {
        return;
    }
    let matches = app.palette_matches();
    let theme = app.theme.clone();
    let ui = Ui::new(
        &theme,
        crate::ui::glyphs::Glyphs::for_width(app.glyphs, app.width),
    );
    let Some(palette) = &mut app.compose.palette else {
        return;
    };
    let cursor = palette.selected.min(matches.len().saturating_sub(1));
    // On a phone the name column gives way: the title follows the name.
    let pad = if area.width < super::NARROW {
        0
    } else {
        name_column("")
    };
    let mut rows = Vec::new();
    let mut items = Vec::new();
    for (header, command) in &matches {
        if let Some(header) = header {
            if !rows.is_empty() {
                rows.push(Row::Gap);
            }
            rows.push(Row::header(*header));
        }
        items.push(rows.len());
        rows.push(
            Row::item(command_line(ui, command, "", pad))
                .right(Span::styled(command.key, ui.muted())),
        );
    }
    let selected = items.get(cursor).copied();
    let mut footer = Vec::new();
    if let Some(error) = &palette.error {
        footer.push(Line::styled(error.clone(), Style::new().fg(theme.error)));
    } else if let Some((_, command)) = matches.get(cursor)
        && !command.args.is_empty()
    {
        footer.push(Line::from(vec![
            Span::styled(format!("{} {}", command.name, command.args), ui.strong()),
            Span::styled(format!("  {}", command.title), ui.muted()),
        ]));
    }
    let hints = [
        Hint::new("enter", "run"),
        Hint::new("tab", "complete"),
        Hint::new("esc", "close"),
    ];
    let placed = Select::new(ui, "commands", Size::Large, &mut palette.search)
        .label(":")
        .rows(rows, selected)
        .empty("no command matches")
        .footer(footer)
        .hints(&hints)
        .render(area, frame.buffer_mut(), &mut palette.offset);
    dialog_taps(hits, area, &placed.dialog);
    for (row, rect) in placed.rows {
        if let Some(at) = items.iter().position(|item| *item == row) {
            hits.click(rect, Click::Act(Action::Palette(Input::Pick(at))));
        }
    }
}

/// A dialog's taps: outside it and on `esc` it closes, inside it nothing but what is
/// recorded after.
pub(super) fn dialog_taps(hits: &mut Hits, screen: Rect, dialog: &crate::ui::dialog::Areas) {
    let close = mouse::key(KeyCode::Esc);
    hits.cover(screen);
    hits.click(screen, close.clone());
    hits.click(dialog.outer, Click::Nothing);
    hits.click(dialog.close, close);
}

/// The `/` popup, above the prompt at `prompt`: the commands the prompt's first word
/// matches, the cursor's solid.
pub(super) fn slash(frame: &mut Frame, prompt: Rect, app: &App, hits: &mut Hits) {
    let Some(matches) = crate::compose::slash(app) else {
        return;
    };
    let ui = app.ui();
    let shown = matches.len().clamp(1, SLASH_ROWS);
    let height = u16::try_from(shown + 2).unwrap_or(u16::MAX);
    let area = Rect {
        y: prompt.y.saturating_sub(height),
        height: height.min(prompt.y),
        ..prompt
    };
    if area.height < 3 {
        return;
    }
    let buf = frame.buffer_mut();
    Clear.render(area, buf);
    fill(buf, area, ui.panel());
    let block = Block::bordered()
        .border_style(ui.border(false))
        .style(ui.panel());
    let inner = block.inner(area);
    block.render(area, buf);
    hits.cover(area);
    if matches.is_empty() {
        Line::styled("  no command matches", ui.muted()).render(inner, buf);
        return;
    }
    let cursor = app.compose.slash.min(matches.len() - 1);
    let pad = if area.width < super::NARROW {
        0
    } else {
        name_column("/")
    };
    let rows = matches
        .iter()
        .map(|command| {
            Row::item(command_line(ui, command, "/", pad))
                .right(Span::styled(command.key, ui.muted()))
        })
        .collect();
    let mut offset = cursor.saturating_sub(shown - 1);
    let placed = ListView::new(ui, rows)
        .select(Some(cursor))
        .focused(true)
        .render(inner, buf, &mut offset);
    for (at, rect) in placed {
        let click = if at == cursor {
            Click::Act(Action::Compose(Act::Submit))
        } else {
            Click::Act(Action::Compose(Act::Slash(
                isize::try_from(at).unwrap_or(0) - isize::try_from(cursor).unwrap_or(0),
            )))
        };
        hits.click(rect, click);
    }
}
