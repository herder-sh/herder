//! The recover dialog, over the main screen: the offline host, the online hosts that can take
//! the session over, and the command to run on the chosen one.
//!
//! ```text
//! ┌─ recover · app · docs ─────────────────── esc ─┐
//! │                                                │
//! │  ✗ laptop is offline · last seen 2h 5m ago     │
//! │                                                │
//! │  take it over on                               │
//! │▶ ● devbox                                      │
//! │                                                │
//! │  run on devbox                                 │
//! │  $ herder recover s2                           │
//! └────────────────────────────────────────────────┘
//! ```

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::app::App;
use crate::mouse::{Click, Hits, List as Rows};
use crate::recover;
use crate::ui::dialog::{Dialog, PAD_X, Size};
use crate::ui::fit;
use crate::ui::hints::Hint;
use crate::ui::list::{ListView, Row};

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App, hits: &mut Hits) {
    let Some(dialog) = &app.recover else {
        return;
    };
    let (Some(session), Some(dead)) = (
        app.sessions.get(&dialog.session),
        app.fleet_host(&dialog.session),
    ) else {
        return;
    };
    let ui = app.ui();
    let theme = ui.theme;
    let targets = app.recover_targets(&dialog.session);
    let cursor = dialog.selected.min(targets.len().saturating_sub(1));
    let chosen = targets.get(cursor);
    let width = usize::from(
        Size::Medium
            .width()
            .min(area.width)
            .saturating_sub(2 + 2 * PAD_X),
    )
    .max(8);
    let wrapped = |text: &str, style: Style| -> Vec<Line<'static>> {
        // Whole words: `read-only` stays on one line.
        let options =
            textwrap::Options::new(width).word_splitter(textwrap::WordSplitter::NoHyphenation);
        textwrap::wrap(text, options)
            .into_iter()
            .map(|line| Line::styled(line.into_owned(), style))
            .collect()
    };
    let mut top = vec![Line::from(vec![
        Span::styled(
            format!("{} ", ui.glyphs.disconnected),
            Style::new().fg(theme.error),
        ),
        Span::styled(dead.host_name.clone(), ui.strong()),
        Span::styled(" is offline", ui.text()),
        Span::styled(
            format!(
                "{}last seen {} ago",
                ui.glyphs.separator,
                super::sessions::ago(dead)
            ),
            ui.muted(),
        ),
    ])];
    top.push(Line::default());
    if targets.is_empty() {
        top.extend(wrapped(
            "No host of this vault is online. Start herder on another host paired with it, then \
             run there:",
            ui.text(),
        ));
    } else {
        top.push(Line::styled("take it over on", ui.muted()));
    }
    let rows: Vec<Row> = targets
        .iter()
        .map(|target| {
            Row::item(Line::from(vec![
                Span::styled(ui.glyphs.connected, Style::new().fg(theme.success)),
                Span::raw(" "),
                Span::styled(target.host_name.clone(), ui.text()),
            ]))
        })
        .collect();
    let mut bottom = vec![Line::default()];
    if let Some(chosen) = chosen {
        bottom.push(Line::styled(
            format!("run on {}", chosen.host_name),
            ui.muted(),
        ));
    }
    bottom.push(Line::from(vec![
        Span::styled("$ ", ui.muted()),
        Span::styled(recover::command(&session.id), ui.strong()),
    ]));
    bottom.push(Line::default());
    bottom.extend(wrapped(
        &format!(
            "It keeps its id and continues there from its last checkpoint. If {} comes back, \
             its copy turns read-only.",
            dead.host_name
        ),
        ui.muted(),
    ));

    let hints = [Hint::new("j/k", "host"), Hint::new("esc", "close")];
    let narrow = area.width < super::NARROW;
    let title = Line::from(ui.joined([Span::raw("recover"), Span::raw(session.short_title())]));
    let height = u16::try_from(top.len() + rows.len() + bottom.len()).unwrap_or(u16::MAX);
    let areas = Dialog::new(ui, title, Size::Medium)
        // On a phone the button bar has them.
        .hints(if narrow { &[] } else { &hints })
        .render(area, height, frame.buffer_mut());
    super::palette::dialog_taps(hits, area, &areas);
    let body = areas.body;
    let buf = frame.buffer_mut();
    let mut y = body.y;
    let text = |lines: Vec<Line<'static>>, y: &mut u16, buf: &mut ratatui::buffer::Buffer| {
        for line in lines {
            if *y < body.bottom() {
                fit(line, usize::from(body.width), ui.glyphs)
                    .render(Rect::new(body.x, *y, body.width, 1), buf);
            }
            *y += 1;
        }
    };
    text(top, &mut y, buf);
    // As a picker's: the pointer sits in the padding, the cursor's row a column past the
    // text either side.
    let list = Rect::new(
        body.x.saturating_sub(1),
        y,
        body.width + 2,
        u16::try_from(rows.len()).unwrap_or(u16::MAX),
    )
    .intersection(areas.outer);
    let mut offset = 0;
    let placed = ListView::new(ui, rows)
        .select(chosen.map(|_| cursor))
        .focused(true)
        .render(list, buf, &mut offset);
    for (at, rect) in placed {
        hits.click(rect, Click::Row(Rows::Recover, at));
    }
    y += list.height;
    text(bottom, &mut y, buf);
}
