//! The backup dialog, over the fleet view: where the machine backs up, or the vaults to pick
//! from, and how linking or stopping goes.
//!
//! ```text
//! ┌─ back up · devbox ─────────────────────────── esc ─┐
//! │                                                    │
//! │  Its sessions replicate to the vault as they run;  │
//! │  any host of the vault can take one over.          │
//! │                                                    │
//! │  back up to                                        │
//! │▶ ● vault   vault.lan:7447 · 3f9a…77c0              │
//! │                                                    │
//! │  The vault gives devbox a key that only            │
//! │  replicates: it reads nothing there.               │
//! └────────────────────────────────────────────────────┘
//! ```

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::app::App;
use crate::backup::{Backup, Known, Step};
use crate::ui::dialog::{Dialog, PAD_X, Size};
use crate::ui::fit;
use crate::ui::hints::Hint;
use crate::ui::list::{ListView, Row};

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App, backup: &Backup) {
    let Some(machine) = app.machines.iter().find(|m| m.host_id == backup.host) else {
        return;
    };
    let ui = app.ui();
    let theme = ui.theme;
    let name = machine.name.as_str();
    let known = app
        .machine_panel
        .as_ref()
        .and_then(|panel| panel.links.get(&backup.host));
    let vaults = app.vaults(&backup.host);
    let cursor = backup.selected.min(vaults.len().saturating_sub(1));
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
    let ok = |text: &str| -> Vec<Line<'static>> {
        let mut lines = wrapped(text, ui.text());
        if let Some(first) = lines.first_mut() {
            first.spans.insert(
                0,
                Span::styled(
                    format!("{} ", ui.glyphs.connected),
                    Style::new().fg(theme.success),
                ),
            );
        }
        lines
    };
    let mut top = Vec::new();
    let mut rows: Vec<Row> = Vec::new();
    let mut bottom = Vec::new();
    let mut hints = vec![Hint::new("esc", "close")];
    match (&backup.step, known) {
        (Step::Done(text), _) => {
            top.extend(ok(text));
            hints = vec![Hint::new("enter", "done")];
        }
        (Step::Failed(why), _) => {
            top.extend(super::failure(ui, why, width));
            hints = vec![Hint::new("enter", "back"), Hint::new("esc", "back")];
        }
        (Step::Linking { vault }, _) => {
            let vault = app
                .machines
                .iter()
                .find(|m| m.host_id == *vault)
                .map_or("the vault", |m| m.name.as_str());
            top.extend(wrapped(
                &format!("Pairing {name} with {vault}{}", ui.glyphs.ellipsis),
                ui.muted(),
            ));
            hints.clear();
        }
        (Step::Stopping, _) => {
            top.extend(wrapped(
                &format!("Stopping {name}'s backup{}", ui.glyphs.ellipsis),
                ui.muted(),
            ));
            hints.clear();
        }
        (Step::ConfirmStop, _) => {
            top.push(Line::styled(
                format!("Stop backing {name} up?"),
                Style::new().fg(theme.warning),
            ));
            top.push(Line::default());
            top.extend(wrapped(
                "New sessions and turns stay on this machine only. What the vault holds stays \
                 there, and the machine's key no longer opens it.",
                ui.muted(),
            ));
            hints = vec![Hint::new("y", "stop"), Hint::new("n", "keep")];
        }
        (Step::Choose, None | Some(Known::Asking)) => {
            top.extend(wrapped(
                &format!("Asking {name}{}", ui.glyphs.ellipsis),
                ui.muted(),
            ));
        }
        (Step::Choose, Some(Known::Vault)) => {
            top.extend(wrapped(
                &format!(
                    "{name} is a vault: other machines back up to it. Choose one of them here \
                     to back it up."
                ),
                ui.text(),
            ));
        }
        (Step::Choose, Some(Known::Unknown(why))) => {
            top.extend(super::failure(
                ui,
                &format!("{name} cannot say where it backs up: {why}"),
                width,
            ));
        }
        (Step::Choose, Some(Known::Linked(linked))) => {
            let vault = app.paired_vault(linked);
            let line = vec![
                Span::styled(
                    format!("{} ", ui.glyphs.connected),
                    Style::new().fg(theme.success),
                ),
                Span::styled(format!("{name} backs up to "), ui.text()),
                Span::styled(
                    vault.map_or("a vault", |v| v.name.as_str()).to_owned(),
                    ui.strong(),
                ),
            ];
            top.push(Line::from(line));
            // Its own line, so a phone shows it whole.
            top.push(Line::styled(format!("  {}", linked.address), ui.muted()));
            top.push(Line::default());
            top.extend(wrapped(
                "Its sessions replicate there as they run; any host of the vault can take one \
                 over if this machine goes down.",
                ui.muted(),
            ));
            hints = vec![
                Hint::new("enter", "stop backing up"),
                Hint::new("esc", "close"),
            ];
        }
        (Step::Choose, Some(Known::Unlinked)) => {
            top.extend(wrapped(
                "Its sessions replicate to the vault as they run; any host of the vault can take \
                 one over if this machine goes down.",
                ui.muted(),
            ));
            top.push(Line::default());
            if vaults.is_empty() {
                top.extend(wrapped(
                    "No vault is paired here. Pair this client with one as its owner: run \
                     `herder pair` on the vault, then a here.",
                    ui.text(),
                ));
            } else {
                top.push(Line::styled("back up to", ui.muted()));
                rows = vaults
                    .iter()
                    .map(|vault| {
                        Row::item(Line::from(vec![
                            Span::styled(ui.glyphs.connected, Style::new().fg(theme.success)),
                            Span::raw(" "),
                            Span::styled(vault.name.clone(), ui.text()),
                        ]))
                        .right(Line::styled(
                            vault.addresses.first().cloned().unwrap_or_default(),
                            ui.muted(),
                        ))
                    })
                    .collect();
                bottom.push(Line::default());
                bottom.extend(wrapped(
                    &format!(
                        "The vault gives {name} a key that only replicates: it reads nothing \
                         there."
                    ),
                    ui.muted(),
                ));
                hints = vec![
                    Hint::new("enter", "back up"),
                    Hint::new("j/k", "vault"),
                    Hint::new("esc", "close"),
                ];
            }
        }
    }

    let narrow = area.width < super::NARROW;
    let title = Line::from(ui.joined([Span::raw("back up"), Span::raw(name.to_owned())]));
    let height = u16::try_from(top.len() + rows.len() + bottom.len()).unwrap_or(u16::MAX);
    let areas = Dialog::new(ui, title, Size::Medium)
        // On a phone the button bar has them.
        .hints(if narrow { &[] } else { &hints })
        .render(area, height, frame.buffer_mut());
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
    if !rows.is_empty() {
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
        ListView::new(ui, rows)
            .select(Some(cursor))
            .focused(true)
            .render(list, buf, &mut offset);
        y += list.height;
    }
    text(bottom, &mut y, buf);
}
