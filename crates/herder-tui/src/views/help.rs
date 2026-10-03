//! The key help, over everything else. On a screen too small for it, descriptions wrap and
//! j / k scroll it.

use ratatui::Frame;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::{Constraint, Rect};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Row, Table, TableState};

use crate::action::HELP;
use crate::app::App;
use crate::mouse::{self, Hits};
use crate::ui::Ui;
use crate::ui::dialog::{Dialog, PAD_X, Size};
use crate::ui::glyphs::Glyphs;
use crate::ui::hints::Hint;

/// Columns between the key and its description.
const SPACING: usize = 2;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App, hits: &mut Hits) {
    // The help scroll changes below, so the look is a copy.
    let theme = app.theme.clone();
    let ui = Ui::new(&theme, Glyphs::for_width(app.glyphs, app.width));
    let key_width = HELP
        .iter()
        .map(|(key, _)| key.chars().count())
        .max()
        .unwrap_or(0);
    // The dialog's body: its width less the border and padding.
    let body_width = usize::from(
        Size::Large
            .width()
            .min(area.width)
            .saturating_sub(2 + 2 * PAD_X),
    );
    let wrap_at = body_width.saturating_sub(key_width + SPACING).max(8);
    let texts: Vec<Vec<Line>> = HELP
        .iter()
        .map(|(_, text)| {
            textwrap::wrap(text, wrap_at)
                .into_iter()
                .map(|part| Line::styled(part.into_owned(), ui.text()))
                .collect()
        })
        .collect();
    let lines: usize = texts.iter().map(Vec::len).sum();
    let scroll = [Hint::new("j/k", "scroll"), Hint::new("esc", "close")];
    let close = [Hint::new("any key", "close")];
    let chrome = usize::from(Dialog::new(ui, "keys", Size::Large).hints(&close).chrome());
    // The first row such that the rest still fill the dialog.
    let shown = lines.min(usize::from(area.height).saturating_sub(chrome));
    let mut fill = 0;
    let last_top = texts
        .iter()
        .rposition(|text| {
            fill += text.len();
            fill > shown
        })
        .map_or(0, |at| at + 1);
    app.help_scroll = app.help_scroll.min(last_top);
    let hints: &[Hint] = if last_top > 0 { &scroll } else { &close };
    let areas = Dialog::new(ui, "keys", Size::Large).hints(hints).render(
        area,
        u16::try_from(shown).unwrap_or(u16::MAX),
        frame.buffer_mut(),
    );
    let rows = HELP.iter().zip(texts).map(|((key, _), text)| {
        let height = u16::try_from(text.len()).unwrap_or(u16::MAX);
        Row::new([Text::styled(*key, ui.strong()), Text::from(text)]).height(height)
    });
    let key_width = u16::try_from(key_width).unwrap_or(u16::MAX);
    let table = Table::new(rows, [Constraint::Length(key_width), Constraint::Fill(1)])
        .column_spacing(u16::try_from(SPACING).unwrap_or(u16::MAX));
    let mut state = TableState::default().with_offset(app.help_scroll);
    frame.render_stateful_widget(table, areas.body, &mut state);
    // Like any key, a tap anywhere closes the help.
    hits.click(area, mouse::key(KeyCode::Esc));
}
