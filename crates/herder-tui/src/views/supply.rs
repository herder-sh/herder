//! The dialog supplying a secret an agent asked for, over the main screen: what it asks for,
//! the agent's hint, the warning when it replaces a value, and the value, masked.
//!
//! ```text
//! ┌─ secret · coingate/GITHUB_SIEM_PAT ─────── esc ─┐
//! │                                                 │
//! │  The agent asks for GITHUB_SIEM_PAT in coingate │
//! │  GitHub fine-grained PAT, read-only on the org  │
//! │  audit log                                      │
//! │  coingate holds it already: storing replaces it │
//! │                                                 │
//! │  value  •••••••••••••                           │
//! │                                                 │
//! │  It goes straight to the vault; the agent only  │
//! │  learns that it was stored.                     │
//! └─────────────────────────────────────────────────┘
//! ```

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::app::App;
use crate::mouse::Hits;
use crate::supply::Sent;
use crate::ui::dialog::{Dialog, PAD_X, Size};
use crate::ui::fit;
use crate::ui::hints::Hint;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App, hits: &mut Hits) {
    let Some(dialog) = &app.supply else {
        return;
    };
    let ui = app.ui();
    let theme = ui.theme;
    let request = &dialog.request;
    let width = usize::from(
        Size::Medium
            .width()
            .min(area.width)
            .saturating_sub(2 + 2 * PAD_X),
    )
    .max(8);
    let wrapped = |text: &str, style: Style| -> Vec<Line<'static>> {
        let options =
            textwrap::Options::new(width).word_splitter(textwrap::WordSplitter::NoHyphenation);
        textwrap::wrap(text, options)
            .into_iter()
            .map(|line| Line::styled(line.into_owned(), style))
            .collect()
    };
    let mut lines = vec![Line::from(vec![
        Span::styled("The agent asks for ", ui.text()),
        Span::styled(request.name.clone(), ui.strong()),
        Span::styled(format!(" in {}", request.set), ui.text()),
    ])];
    if !request.description.is_empty() {
        lines.extend(wrapped(&request.description, ui.muted()));
    }
    if request.replaces {
        lines.extend(wrapped(
            &format!(
                "{} holds it already: storing replaces its value",
                request.set
            ),
            Style::new().fg(theme.warning),
        ));
    }
    lines.push(Line::default());
    let value = if dialog.value.is_empty() {
        Span::styled("type or paste it, hidden", ui.muted())
    } else {
        Span::styled(super::secrets::masked(ui, dialog.value.len()), ui.text())
    };
    lines.push(Line::from(vec![Span::styled("value  ", ui.muted()), value]));
    if let Some(error) = &dialog.error {
        lines.extend(wrapped(error, Style::new().fg(theme.error)));
    }
    lines.push(Line::default());
    let status = match &dialog.step {
        None => "It goes straight to the vault; the agent only learns that it was stored.",
        Some(Sent::Link | Sent::List(_)) => "Finding the machine's vault…",
        Some(Sent::Create(_)) => "Creating the set…",
        Some(Sent::Put) => "Storing it on the vault…",
        Some(Sent::Confirm) => "Telling the agent…",
    };
    lines.extend(wrapped(status, ui.muted()));

    let hints = [Hint::new("enter", "store"), Hint::new("esc", "close")];
    let narrow = area.width < super::NARROW;
    let title = Line::from(ui.joined([
        Span::raw("secret"),
        Span::raw(format!("{}/{}", request.set, request.name)),
    ]));
    let height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let areas = Dialog::new(ui, title, Size::Medium)
        .hints(if narrow { &[] } else { &hints })
        .render(area, height, frame.buffer_mut());
    super::palette::dialog_taps(hits, area, &areas);
    let body = areas.body;
    let buf = frame.buffer_mut();
    for (y, line) in (body.y..body.bottom()).zip(lines) {
        fit(line, usize::from(body.width), ui.glyphs)
            .render(Rect::new(body.x, y, body.width, 1), buf);
    }
}
