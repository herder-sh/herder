//! The fork dialog, over the main screen: where the session runs, the hosts it can be forked
//! onto, and on enter the fork; without such a host, the command to run on one.
//!
//! ```text
//! ┌─ fork · app · docs ────────────────────── esc ─┐
//! │                                                │
//! │  ✗ on laptop · offline, last seen 2h 5m ago    │
//! │                                                │
//! │  fork onto                                     │
//! │▶ ● devbox                                      │
//! │                                                │
//! │  enter forks it onto devbox                    │
//! └────────────────────────────────────────────────┘
//! ```

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::app::App;
use crate::fork;
use crate::mouse::{Click, Hits, List as Rows};
use crate::ui::dialog::{Dialog, PAD_X, Size};
use crate::ui::fit;
use crate::ui::hints::Hint;
use crate::ui::list::{ListView, Row};

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App, hits: &mut Hits) {
    let Some(dialog) = &app.fork else {
        return;
    };
    let Some(session) = app.sessions.get(&dialog.session) else {
        return;
    };
    let ui = app.ui();
    let theme = ui.theme;
    let targets = app.fork_targets();
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
    let host = app
        .host_name(&dialog.session)
        .unwrap_or_else(|| dialog.session.host_id.to_string());
    let mut source = vec![
        Span::styled("on ", ui.muted()),
        Span::styled(host, ui.strong()),
    ];
    if let Some(dead) = app.fleet_host(&dialog.session).filter(|host| !host.online) {
        source.insert(
            0,
            Span::styled(
                format!("{} ", ui.glyphs.disconnected),
                Style::new().fg(theme.error),
            ),
        );
        source.push(Span::styled(
            format!(
                "{}offline, last seen {} ago",
                ui.glyphs.separator,
                super::sessions::ago(dead)
            ),
            ui.muted(),
        ));
    }
    let mut top = vec![Line::from(source), Line::default()];
    if targets.is_empty() {
        top.extend(wrapped(
            "No host is paired here with you as its owner. Run on the host to fork it onto:",
            ui.text(),
        ));
    } else {
        top.push(Line::styled("fork onto", ui.muted()));
    }
    let rows: Vec<Row> = targets
        .iter()
        .map(|target| {
            Row::item(Line::from(vec![
                Span::styled(ui.glyphs.connected, Style::new().fg(theme.success)),
                Span::raw(" "),
                Span::styled(target.name.clone(), ui.text()),
            ]))
        })
        .collect();
    let mut bottom = vec![Line::default()];
    let here = chosen.is_some();
    match chosen {
        Some(chosen) if dialog.sending => bottom.push(Line::styled(
            format!("forking onto {}{}", chosen.name, ui.glyphs.ellipsis),
            ui.muted(),
        )),
        Some(chosen) => bottom.push(Line::from(vec![
            Span::styled("enter", ui.accent()),
            Span::styled(format!(" forks it onto {}", chosen.name), ui.text()),
        ])),
        None => bottom.push(Line::from(vec![
            Span::styled("$ ", ui.muted()),
            Span::styled(fork::command(&session.id), ui.strong()),
        ])),
    }
    if let Some(error) = &dialog.error {
        bottom.extend(wrapped(error, Style::new().fg(theme.error)));
    }
    bottom.push(Line::default());
    bottom.extend(wrapped(
        "The fork is a new session with this one's history, in a new worktree from its last \
         checkpoint. This one stays as it is.",
        ui.muted(),
    ));

    let hints = if here {
        vec![
            Hint::new("j/k", "host"),
            Hint::new("enter", "fork"),
            Hint::new("esc", "close"),
        ]
    } else {
        vec![Hint::new("j/k", "host"), Hint::new("esc", "close")]
    };
    let narrow = area.width < super::NARROW;
    let title = Line::from(ui.joined([Span::raw("fork"), Span::raw(session.short_title())]));
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
        hits.click(rect, Click::Row(Rows::Fork, at));
    }
    y += list.height;
    text(bottom, &mut y, buf);
}
