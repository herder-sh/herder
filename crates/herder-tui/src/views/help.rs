//! The key help, over everything else. On a screen too small for it, descriptions wrap and
//! j / k scroll it.

use ratatui::Frame;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::{Constraint, Rect};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Clear, Padding, Row, Table, TableState};

use crate::action::HELP;
use crate::app::App;
use crate::mouse::{self, Hits};

/// Columns between the key and its description.
const SPACING: usize = 2;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App, hits: &mut Hits) {
    let key_width = HELP
        .iter()
        .map(|(key, _)| key.chars().count())
        .max()
        .unwrap_or(0);
    let text_width = HELP
        .iter()
        .map(|(_, text)| text.chars().count())
        .max()
        .unwrap_or(0);
    // Borders and padding take two columns a side, and one spare.
    let width = (key_width + SPACING + text_width + 5).min(usize::from(area.width));
    let wrap_at = width.saturating_sub(key_width + SPACING + 4).max(8);
    let texts: Vec<Vec<Line>> = HELP
        .iter()
        .map(|(_, text)| {
            textwrap::wrap(text, wrap_at)
                .into_iter()
                .map(|part| Line::raw(part.into_owned()))
                .collect()
        })
        .collect();
    let lines: usize = texts.iter().map(Vec::len).sum();
    let height = (lines + 4).min(usize::from(area.height));
    // The first row such that the rest still fill the popup.
    let shown = height.saturating_sub(4);
    let mut fill = 0;
    let last_top = texts
        .iter()
        .rposition(|text| {
            fill += text.len();
            fill > shown
        })
        .map_or(0, |at| at + 1);
    app.help_scroll = app.help_scroll.min(last_top);
    let popup = super::centered(
        area,
        u16::try_from(width).unwrap_or(u16::MAX),
        u16::try_from(height).unwrap_or(u16::MAX),
    );
    let rows = HELP.iter().zip(texts).map(|((key, _), text)| {
        let height = u16::try_from(text.len()).unwrap_or(u16::MAX);
        Row::new([Text::styled(*key, super::bold()), Text::from(text)]).height(height)
    });
    let hint = if last_top > 0 {
        " j/k scroll · other keys or a tap close "
    } else {
        " any key or a tap closes "
    };
    let key_width = u16::try_from(key_width).unwrap_or(u16::MAX);
    let table = Table::new(rows, [Constraint::Length(key_width), Constraint::Fill(1)])
        .column_spacing(u16::try_from(SPACING).unwrap_or(u16::MAX))
        .block(
            Block::bordered()
                .title(" keys ")
                .title_bottom(Line::styled(hint, super::dim()).centered())
                .padding(Padding::uniform(1)),
        );
    let mut state = TableState::default().with_offset(app.help_scroll);
    frame.render_widget(Clear, popup);
    frame.render_stateful_widget(table, popup, &mut state);
    // Like any key, a tap anywhere closes the help.
    hits.click(area, mouse::key(KeyCode::Esc));
}
