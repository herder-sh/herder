//! The key-hint bar: the keys that work right now, from one list of [`Hint`]s.
//!
//! - [`ModeBar`], on a desktop: a lead (the mode badge, a notice), the hints as `key label`,
//!   and at the right each machine's connection. Hints that do not fit are left out whole,
//!   from the second-to-last back: a hint is never cut, and the last (help, or the way out)
//!   stays. Machine names give way to it, cut with an ellipsis, then the last machines.
//! - [`ButtonBar`], on a phone: the same hints as buttons a finger or Tab reaches. Tab's focus
//!   is drawn solid, and the bar scrolls so it stays in view.
//!
//! Both return where each hint landed, for taps.

use std::borrow::Cow;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use super::{GAP, INSET, Ui, line_width, width};

/// A key and what it does, in a word or two: `enter` `send`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint {
    pub key: Cow<'static, str>,
    pub label: Cow<'static, str>,
}

impl Hint {
    pub fn new(key: impl Into<Cow<'static, str>>, label: impl Into<Cow<'static, str>>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
        }
    }

    /// Columns of `key label`.
    fn width(&self) -> u16 {
        u16::try_from(width(&self.key) + 1 + width(&self.label)).unwrap_or(u16::MAX)
    }
}

/// The desktop's bottom bar.
pub struct ModeBar<'a> {
    ui: Ui<'a>,
    lead: Line<'a>,
    hints: &'a [Hint],
    machines: Vec<(Span<'a>, String)>,
}

/// Columns a machine's name keeps, ellipsis included, before the machine is left out.
const SHORTEST_NAME: usize = 4;

impl<'a> ModeBar<'a> {
    pub fn new(ui: Ui<'a>, hints: &'a [Hint]) -> Self {
        Self {
            ui,
            lead: Line::default(),
            hints,
            machines: Vec::new(),
        }
    }

    /// What comes before the hints: the mode badge, a notice.
    pub fn lead(mut self, lead: impl Into<Line<'a>>) -> Self {
        self.lead = lead.into();
        self
    }

    /// What sits at the right end: each machine's connection mark and name.
    pub fn machines(mut self, machines: Vec<(Span<'a>, String)>) -> Self {
        self.machines = machines;
        self
    }

    /// Draws the bar on the first row of `area`; returns each hint drawn, by index, and where.
    pub fn render(self, area: Rect, buf: &mut Buffer) -> Vec<(usize, Rect)> {
        let ui = self.ui;
        let area = Rect { height: 1, ..area }.intersection(buf.area);
        let start = area.x + INSET.min(area.width);
        let end = area.right().saturating_sub(INSET);
        let room = usize::from(end.saturating_sub(start));
        let lead = super::fit(self.lead, room, ui.glyphs);
        let lead_width = u16::try_from(line_width(&lead)).unwrap_or(u16::MAX);
        lead.render(Rect::new(start, area.y, lead_width, 1), buf);
        let gap = u16::try_from(GAP).unwrap_or(u16::MAX);
        let mut x = if lead_width > 0 {
            start + lead_width + gap
        } else {
            start
        };
        // The last hint, help or the way out, is never left out: room is kept for it.
        let last = self.hints.len().saturating_sub(1);
        let kept = self.hints.last().map_or(0, |hint| hint.width() + gap);
        // The machines, in what the lead and the last hint leave.
        let right = machines(self.machines, usize::from(end.saturating_sub(x + kept)), ui);
        let right_width = u16::try_from(line_width(&right)).unwrap_or(u16::MAX);
        let mut limit = end;
        if right_width > 0 {
            let right_x = end - right_width;
            right.render(Rect::new(right_x, area.y, right_width, 1), buf);
            limit = right_x.saturating_sub(gap);
        }
        let mut placed = Vec::new();
        let mut full = false;
        for (at, hint) in self.hints.iter().enumerate() {
            let hint_width = hint.width();
            let room = if at < last {
                limit.saturating_sub(kept)
            } else {
                limit
            };
            full |= x + hint_width > room;
            if full && at < last {
                continue;
            }
            if x + hint_width > room {
                break;
            }
            Line::from(vec![
                Span::styled(hint.key.clone(), ui.strong()),
                Span::styled(format!(" {}", hint.label), ui.muted()),
            ])
            .render(Rect::new(x, area.y, hint_width, 1), buf);
            placed.push((at, Rect::new(x, area.y, hint_width, 1)));
            x += hint_width + gap;
        }
        placed
    }
}

/// `machines` as `● box  ● m2` in at most `room` columns: whole if they fit, else each name
/// cut to the longest that lets them all fit, with an ellipsis; machines whose names would
/// keep fewer than [`SHORTEST_NAME`] columns are left out from the last.
fn machines<'a>(machines: Vec<(Span<'a>, String)>, room: usize, ui: Ui) -> Line<'a> {
    let line = |machines: &[(Span<'a>, String)], cap: usize| {
        let mut spans = Vec::new();
        for (mark, name) in machines {
            if !spans.is_empty() {
                spans.push(Span::raw("  "));
            }
            spans.push(mark.clone());
            let name = super::fit(Line::raw(name.clone()), cap, ui.glyphs);
            let name: String = name.spans.iter().map(|span| &*span.content).collect();
            spans.push(Span::styled(format!(" {name}"), ui.text()));
        }
        Line::from(spans)
    };
    let longest = machines
        .iter()
        .map(|(_, name)| width(name))
        .max()
        .unwrap_or(0);
    for shown in (1..=machines.len()).rev() {
        let machines = &machines[..shown];
        for cap in (SHORTEST_NAME.min(longest)..=longest).rev() {
            let line = line(machines, cap);
            if line_width(&line) <= room {
                return line;
            }
        }
    }
    Line::default()
}

/// The phone's bar of buttons.
pub struct ButtonBar<'a> {
    ui: Ui<'a>,
    hints: &'a [Hint],
    focus: Option<usize>,
}

impl<'a> ButtonBar<'a> {
    pub fn new(ui: Ui<'a>, hints: &'a [Hint]) -> Self {
        Self {
            ui,
            hints,
            focus: None,
        }
    }

    /// The button Tab moved to.
    pub fn focus(mut self, focus: Option<usize>) -> Self {
        self.focus = focus;
        self
    }

    /// Draws the buttons on the first row of `area`, as many as fit from the first, or so the
    /// focused one shows. Returns each button drawn, by index, and where, with the gap after
    /// it: every column of the bar taps something.
    pub fn render(self, area: Rect, buf: &mut Buffer) -> Vec<(usize, Rect)> {
        let ui = self.ui;
        let area = Rect { height: 1, ..area }.intersection(buf.area);
        // ` key label ` and the gap after it.
        let widths: Vec<u16> = self.hints.iter().map(|hint| hint.width() + 3).collect();
        let mut first = 0;
        if let Some(focus) = self.focus.filter(|at| *at < widths.len()) {
            let fits = |first: usize| {
                widths[first..=focus]
                    .iter()
                    .map(|width| u32::from(*width))
                    .sum::<u32>()
                    <= u32::from(area.width) + 1
            };
            while first < focus && !fits(first) {
                first += 1;
            }
        }
        let theme = ui.theme;
        let mut placed = Vec::new();
        let mut x = area.x;
        for (at, (hint, width)) in self.hints.iter().zip(&widths).enumerate().skip(first) {
            let drawn = width - 1;
            if x + drawn > area.right() {
                break;
            }
            let (key, label) = if self.focus == Some(at) {
                let solid = Style::new()
                    .fg(theme.selected_list_item_text)
                    .bg(theme.primary)
                    .add_modifier(Modifier::BOLD);
                (solid, solid)
            } else {
                let raised = Style::new().bg(theme.background_element);
                (
                    raised.fg(theme.primary).add_modifier(Modifier::BOLD),
                    raised.fg(theme.text),
                )
            };
            Line::from(vec![
                Span::styled(format!(" {}", hint.key), key),
                Span::styled(format!(" {} ", hint.label), label),
            ])
            .render(Rect::new(x, area.y, drawn, 1), buf);
            placed.push((
                at,
                Rect::new(x, area.y, (drawn + 1).min(area.right() - x), 1),
            ));
            x += drawn + 1;
        }
        placed
    }
}

#[cfg(test)]
mod tests {
    use super::super::badge;
    use super::super::snapshot;
    use super::*;

    fn hints() -> Vec<Hint> {
        vec![
            Hint::new("enter", "send"),
            Hint::new("shift+enter", "newline"),
            Hint::new("/", "commands"),
            Hint::new("@", "mention"),
            Hint::new("esc", "navigate"),
        ]
    }

    fn connections(ui: Ui) -> Vec<(Span<'static>, String)> {
        let theme = ui.theme;
        vec![
            (
                Span::styled(ui.glyphs.connected, Style::new().fg(theme.success)),
                "box".to_owned(),
            ),
            (
                Span::styled(ui.glyphs.connected, Style::new().fg(theme.success)),
                "m2".to_owned(),
            ),
            (
                Span::styled(ui.glyphs.connecting, Style::new().fg(theme.warning)),
                "vault".to_owned(),
            ),
        ]
    }

    #[test]
    fn mode_bars() {
        let hints = hints();
        for width in [45, 100] {
            snapshot::each(&format!("mode-bar-{width}"), |variant| {
                snapshot::render(variant, width, 2, |ui, area, buf| {
                    let badge = badge::solid(ui, "PROMPT", ui.theme.primary);
                    let placed = ModeBar::new(ui, &hints)
                        .lead(badge)
                        .machines(connections(ui))
                        .render(area, buf);
                    // Hints never overlap and never pass the right side.
                    assert!(placed.windows(2).all(|w| w[0].1.right() < w[1].1.x));
                    let navigate = badge::solid(ui, "NAVIGATE", ui.theme.secondary);
                    ModeBar::new(ui, &hints[2..])
                        .lead(navigate)
                        .render(Rect { y: 1, ..area }, buf);
                })
            });
        }
    }

    #[test]
    fn a_bar_cuts_machine_names_not_hints_and_keeps_help() {
        let mut hints = hints();
        hints.extend([Hint::new("ctrl+c", "stop"), Hint::new("ctrl+x ?", "help")]);
        for width in [60, 100, 160] {
            let variants = snapshot::variants();
            let ui = variants[0].ui();
            let mut buf = Buffer::empty(Rect::new(0, 0, width, 1));
            let machines = vec![
                (
                    Span::raw(ui.glyphs.connected),
                    "Coingate-CJH9FXV367".to_owned(),
                ),
                (Span::raw(ui.glyphs.connected), "trash-can".to_owned()),
            ];
            let placed = ModeBar::new(ui, &hints)
                .lead(badge::solid(ui, "PROMPT", ui.theme.primary))
                .machines(machines)
                .render(buf.area, &mut buf);
            let row: String = (0..width).map(|x| buf[(x, 0)].symbol()).collect();
            // Help, the last hint, always shows; every hint shown is whole.
            assert_eq!(
                placed.last().map(|(at, _)| *at),
                Some(hints.len() - 1),
                "{row}"
            );
            for (at, _) in &placed {
                let hint = &hints[*at];
                assert!(
                    row.contains(&format!("{} {}", hint.key, hint.label)),
                    "{row}"
                );
            }
            // Both machines show, each whole or cut with an ellipsis.
            let right = &row[row.find(ui.glyphs.connected).expect(&row)..];
            for name in ["Coingate-CJH9FXV367", "trash-can"] {
                let shown = right
                    .split(ui.glyphs.connected)
                    .map(str::trim)
                    .filter(|shown| !shown.is_empty())
                    .find(|shown| name.starts_with(shown.trim_end_matches('…')))
                    .unwrap_or_else(|| panic!("{name} in {row}"));
                assert!(shown == name || shown.ends_with('…'), "{row}");
            }
            if width >= 100 {
                assert!(right.contains("Coingate-CJH9FXV367"), "{row}");
            } else {
                assert!(right.contains('…'), "{row}");
            }
            // Nothing in the last column.
            assert_eq!(buf[(width - 1, 0)].symbol(), " ", "{row}");
        }
    }

    #[test]
    fn button_bars() {
        let hints = vec![
            Hint::new("y", "allow"),
            Hint::new("n", "deny"),
            Hint::new("<", "back"),
            Hint::new("^c", "stop"),
            Hint::new(":", "cmd"),
            Hint::new("?", "help"),
        ];
        snapshot::each("button-bar-45", |variant| {
            snapshot::render(variant, 45, 3, |ui, area, buf| {
                let all = ButtonBar::new(ui, &hints).render(area, buf);
                assert_eq!(all.first().map(|(at, _)| *at), Some(0));
                ButtonBar::new(ui, &hints)
                    .focus(Some(0))
                    .render(Rect { y: 1, ..area }, buf);
                // Focus on the last button scrolls the bar to it.
                let scrolled = ButtonBar::new(ui, &hints)
                    .focus(Some(5))
                    .render(Rect { y: 2, ..area }, buf);
                assert_eq!(scrolled.last().map(|(at, _)| *at), Some(5));
                assert!(scrolled.first().is_some_and(|(at, _)| *at > 0));
            })
        });
    }
}
