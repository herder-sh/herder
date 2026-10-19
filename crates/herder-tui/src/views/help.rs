//! The help, over everything else: every key and command, by mode and screen, from the key
//! tables ([`crate::keys::sections`]). Descriptions wrap; j / k scroll it when it does not fit.

use ratatui::Frame;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::keys;
use crate::mouse::{self, Hits};
use crate::ui::Ui;
use crate::ui::dialog::{Dialog, PAD_X, Size};
use crate::ui::glyphs::Glyphs;
use crate::ui::hints::Hint;

/// Columns between the keys and their description.
const SPACING: usize = 2;
/// The widest the keys column gets; longer keys take a line of their own.
const KEYS_WIDTH: usize = 19;

/// The help's lines for a body `width` columns wide.
fn lines(ui: Ui, width: usize) -> Vec<Line<'static>> {
    let sections = keys::sections();
    let key_width = sections
        .iter()
        .flat_map(|section| &section.rows)
        .map(|row| row.label().chars().count())
        .filter(|width| *width <= KEYS_WIDTH)
        .max()
        .unwrap_or(0);
    let indent = key_width + SPACING;
    let wrap_at = width.saturating_sub(indent).max(8);
    let mut lines = Vec::new();
    for section in sections {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.push(Line::styled(section.title, ui.accent()));
        for row in section.rows {
            let label = row.label();
            let keys = Span::styled(format!("{label:key_width$}"), ui.strong());
            // Keys too long for their column take a line of their own.
            let mut first = if label.chars().count() > key_width {
                lines.push(Line::from(keys));
                None
            } else {
                Some(keys)
            };
            for part in textwrap::wrap(&row.does, wrap_at) {
                let lead = first
                    .take()
                    .unwrap_or_else(|| Span::raw(" ".repeat(key_width)));
                lines.push(Line::from(vec![
                    lead,
                    Span::raw(" ".repeat(SPACING)),
                    Span::styled(part.into_owned(), ui.text()),
                ]));
            }
        }
    }
    lines
}

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App, hits: &mut Hits) {
    // The help scroll changes below, so the look is a copy.
    let theme = app.theme.clone();
    let ui = Ui::new(&theme, Glyphs::for_width(app.glyphs, app.width));
    // The dialog's body: its width less the border and padding.
    let body_width = usize::from(
        Size::Large
            .width()
            .min(area.width)
            .saturating_sub(2 + 2 * PAD_X),
    );
    let lines = lines(ui, body_width);
    let scroll = [Hint::new("j/k", "scroll"), Hint::new("esc", "close")];
    let close = [Hint::new("esc", "close")];
    let chrome = usize::from(Dialog::new(ui, "help", Size::Large).hints(&scroll).chrome());
    let shown = lines
        .len()
        .min(usize::from(area.height).saturating_sub(chrome));
    let last_top = lines.len() - shown;
    app.help_scroll = app.help_scroll.min(last_top);
    let hints: &[Hint] = if last_top > 0 { &scroll } else { &close };
    let areas = Dialog::new(ui, "help", Size::Large).hints(hints).render(
        area,
        u16::try_from(shown).unwrap_or(u16::MAX),
        frame.buffer_mut(),
    );
    let top = u16::try_from(app.help_scroll).unwrap_or(u16::MAX);
    frame.render_widget(Paragraph::new(lines).scroll((top, 0)), areas.body);
    // A tap anywhere closes the help.
    hits.click(area, mouse::key(KeyCode::Esc));
}
