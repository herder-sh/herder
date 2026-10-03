//! The recover dialog, over the main screen: the offline host, the online hosts that can take
//! the session over, and the command to run on the chosen one.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Padding, Paragraph};

use crate::app::App;
use crate::mouse::{Click, Hits, List as Rows};
use crate::recover;

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
    let targets = app.recover_targets(&dialog.session);
    let chosen = targets.get(dialog.selected.min(targets.len().saturating_sub(1)));
    let width = area.width.saturating_sub(4).min(64);
    // Inside the borders and padding.
    let wrap_at = usize::from(width.saturating_sub(4)).max(8);
    let mut lines = Vec::new();
    let wrapped = |lines: &mut Vec<Line<'static>>, text: &str, style: Style| {
        for part in textwrap::wrap(text, wrap_at) {
            lines.push(Line::styled(part.into_owned(), style));
        }
    };
    let offline = format!(
        "{} is offline · last seen {} ago",
        dead.host_name,
        super::sessions::ago(dead)
    );
    wrapped(&mut lines, &offline, Style::new().fg(Color::Red));
    lines.push(Line::raw(""));
    if targets.is_empty() {
        wrapped(
            &mut lines,
            "No host of this vault is online. Start herder on another host paired with it, then \
             run there:",
            Style::new(),
        );
    } else {
        wrapped(&mut lines, "Take it over on:", Style::new());
    }
    let first_target = lines.len();
    for target in &targets {
        let name = Span::raw(target.host_name.clone());
        let picked = chosen.is_some_and(|chosen| chosen.host_id == target.host_id);
        let (mark, name) = if picked {
            ("▸ ", name.style(super::bold().reversed()))
        } else {
            ("  ", name)
        };
        lines.push(Line::from(vec![
            Span::styled(mark, Style::new().fg(Color::Cyan)),
            name,
        ]));
    }
    lines.push(Line::raw(""));
    if let Some(chosen) = chosen {
        lines.push(Line::styled(
            format!("run on {}:", chosen.host_name),
            super::dim(),
        ));
    }
    lines.push(Line::styled(
        format!("  {}", recover::command(&session.id)),
        super::bold().fg(Color::Yellow),
    ));
    lines.push(Line::raw(""));
    let after = format!(
        "It keeps its id and continues there from its last checkpoint. If {} comes back, its \
         copy turns read-only.",
        dead.host_name
    );
    wrapped(&mut lines, &after, super::dim());
    let keys = if area.width < super::NARROW {
        " ⌫ close "
    } else {
        " j/k host  Esc close "
    };
    let block = Block::bordered()
        .title(Line::styled(
            format!(" recover {} ", session.short_title()),
            super::bold(),
        ))
        .title_bottom(Line::styled(keys, super::dim()).centered())
        .border_style(Style::new().fg(Color::Cyan))
        .padding(Padding::uniform(1));
    let height = u16::try_from(lines.len() + 4).unwrap_or(u16::MAX);
    let area = super::centered(area, width, height);
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block), area);
    let first = inner.y + u16::try_from(first_target).unwrap_or(u16::MAX);
    let rows = Rect::new(inner.x, first, inner.width, inner.height).intersection(inner);
    hits.list(rows, 0, &vec![1; targets.len()], |at| {
        Some(Click::Row(Rows::Recover, at))
    });
}
