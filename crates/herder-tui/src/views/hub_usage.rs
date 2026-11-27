//! The hub's usage screen, over the fleet: one row per login with what it used over the
//! period on every machine, next to how full its limit windows are; an opened login lists its
//! machines, models and days.
//!
//! ```text
//!  usage · home-hub                                                     last 7 days
//!
//! ▶ Work  claude                                                    14.7M   $90.05
//!     5-hour   ━━━━━━━━━━━━━━━━━━━━━━  42%  resets in 2h 0m
//!     3 limit hits · 2 failovers off it
//!     machines   work-mbp            12.5M   $83.95
//!                trash-can-01         2.2M    $6.10
//!   ada@example.com  codex                                          820.0K   ~$3.40
//! ```

use herder_protocol::{ProviderLogin, Timestamp};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::account_screen;
use crate::app::App;
use crate::hub_usage::{self, HubUsage, LoginRow, Part};
use crate::mouse::{Hits, Wheel};
use crate::secrets::Load;
use crate::ui::list::{ListView, Row};
use crate::ui::{Ui, fit, usage};

/// Widest a limit's bar gets.
const BAR: u16 = 24;

/// Widest a machine's, model's or day's name gets.
const NAME: usize = 22;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App, screen: &HubUsage, hits: &mut Hits) {
    let ui = app.ui();
    let compact = app.width < super::NARROW;
    let hub = app.machines.iter().find(|m| m.host_id == screen.hub);
    let name = hub.map_or_else(|| screen.hub.to_string(), |m| m.name.clone());
    super::clear(frame, area, ui);
    let period = hub_usage::title(screen.period);
    let mut area = super::heading(frame, area, ui, "usage", &name, period, compact);
    if compact && area.height > 2 {
        frame.render_widget(
            Line::styled(format!("{name}{}{period}", ui.glyphs.separator), ui.muted()),
            Rect { height: 1, ..area },
        );
        area.y += 2;
        area.height -= 2;
    }
    let width = usize::from(area.width.saturating_sub(2)).max(8);
    let failed = match (&screen.usage, &screen.logins) {
        (Load::Failed(why), _) | (_, Load::Failed(why)) => Some(why),
        _ => None,
    };
    if let Some(why) = failed {
        let mut lines = super::failure(ui, &format!("{name} has no usage to show"), width);
        lines.push(Line::default());
        lines.extend(wrap(why, width, ui.text()));
        lines.push(Line::default());
        lines.extend(wrap(
            "What hub logins used adds up on the hub, from the sessions its hosts back up to \
             it. Press r to ask again.",
            width,
            ui.muted(),
        ));
        return lines_at(frame, area, ui, lines);
    }
    let Some(rows) = screen.rows(|id| hub_usage::host_name(hub, id)) else {
        let text = format!("Asking {name}{}", ui.glyphs.ellipsis);
        frame.render_widget(Line::styled(text, ui.muted()), Rect { height: 1, ..area });
        return;
    };
    if rows.is_empty() {
        return lines_at(
            frame,
            area,
            ui,
            wrap(
                "No logins yet. Add one to the hub's CLIProxyAPI from the app, and what \
                 sessions use on it adds up here.",
                width,
                ui.muted(),
            ),
        );
    }
    let now = app.now();
    let selected = screen.selected(&rows);
    let items: Vec<Row<'static>> = rows
        .iter()
        .map(|row| login_row(ui, row, screen.open.contains(&row.pin), width, now))
        .collect();
    let mut offset = 0;
    ListView::new(ui, items)
        .select(selected)
        .focused(true)
        .render(area, frame.buffer_mut(), &mut offset);
    hits.wheel(area, Wheel::Keys);
}

/// A login: its name and provider and what it used; under it how full its limit windows are,
/// its limit hits and failovers, and opened, its machines, models and days.
fn login_row(ui: Ui, row: &LoginRow, open: bool, width: usize, now: Timestamp) -> Row<'static> {
    let mut left = vec![
        Span::styled(row.title.clone(), ui.strong()),
        Span::styled(format!("  {}", row.provider.as_str()), ui.muted()),
    ];
    match &row.login {
        Some(login) if login.fallback => left.push(Span::styled("  fallback", ui.muted())),
        None => left.push(Span::styled("  removed", ui.muted())),
        Some(_) => {}
    }
    let right = if row.amount.turns == 0 {
        Line::styled("—", ui.muted())
    } else {
        Line::from(vec![
            Span::styled(format!("{:>7}", row.amount.tokens()), ui.muted()),
            Span::styled(format!("  {:>9}", row.amount.dollars()), ui.text()),
        ])
    };
    let mut body = row
        .login
        .as_ref()
        .map(|login| limits(ui, login, width, now))
        .unwrap_or_default();
    if let Some(note) = row.limits() {
        body.push(Line::styled(format!("    {note}"), ui.muted()));
    }
    if open {
        body.extend(parts(ui, "machines", &row.machines));
        body.extend(parts(ui, "models", &row.models));
        body.extend(parts(ui, "days", &row.days));
    }
    Row::item(Line::from(left)).right(right).body(body)
}

/// How full each of the login's windows is, as it last reported, with when it resets.
fn limits(ui: Ui, login: &ProviderLogin, width: usize, now: Timestamp) -> Vec<Line<'static>> {
    login
        .usage
        .iter()
        .map(|window| {
            let label = account_screen::window_label(&window.window);
            // Clamped to 0..=100 first, so the cast cannot wrap.
            let percent = window.used_percent.clamp(0.0, 100.0).round() as u8;
            let bar = u16::try_from(width.saturating_sub(4 + 8 + 2 + 5 + 20))
                .unwrap_or(BAR)
                .clamp(6, BAR);
            let mut spans = vec![Span::styled(format!("    {label:<8}  "), ui.muted())];
            spans.extend(usage::bar(ui, percent, bar).spans);
            if let Some(at) = window.resets_at {
                let until = account_screen::until(at.duration_since(now).as_secs());
                spans.push(Span::styled(format!("  resets in {until}"), ui.muted()));
            }
            Line::from(spans)
        })
        .collect()
}

/// `title` and each part beside it, one a line.
fn parts(ui: Ui, title: &str, parts: &[Part]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (at, part) in parts.iter().enumerate() {
        let heading = if at == 0 { title } else { "" };
        let name = fit(Line::styled(part.name.clone(), ui.text()), NAME, ui.glyphs);
        let pad = NAME.saturating_sub(crate::ui::line_width(&name));
        let mut spans = vec![Span::styled(format!("    {heading:<10} "), ui.muted())];
        spans.extend(name.spans);
        spans.extend([
            Span::raw(" ".repeat(pad)),
            Span::styled(format!(" {:>7}", part.amount.tokens()), ui.muted()),
            Span::styled(format!("  {:>9}", part.amount.dollars()), ui.text()),
        ]);
        lines.push(Line::from(spans));
    }
    lines
}

fn lines_at(frame: &mut Frame, area: Rect, ui: Ui, lines: Vec<Line<'static>>) {
    let buf = frame.buffer_mut();
    for (line, y) in lines.into_iter().zip(area.y..area.bottom()) {
        ratatui::widgets::Widget::render(
            fit(line, usize::from(area.width), ui.glyphs),
            Rect::new(area.x, y, area.width, 1),
            buf,
        );
    }
}

fn wrap(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    let options =
        textwrap::Options::new(width.max(8)).word_splitter(textwrap::WordSplitter::NoHyphenation);
    textwrap::wrap(text, options)
        .into_iter()
        .map(|line| Line::styled(line.into_owned(), style))
        .collect()
}
