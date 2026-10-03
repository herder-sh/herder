//! The key-hint bar: the keys that work right now, from one list of [`Hint`]s.
//!
//! - [`ModeBar`], on a desktop: a lead (the mode badge, a notice), the hints as `key label`,
//!   and a right side (each machine's connection). Hints that do not fit are left out whole,
//!   the last first: a hint is never cut.
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
    right: Line<'a>,
}

impl<'a> ModeBar<'a> {
    pub fn new(ui: Ui<'a>, hints: &'a [Hint]) -> Self {
        Self {
            ui,
            lead: Line::default(),
            hints,
            right: Line::default(),
        }
    }

    /// What comes before the hints: the mode badge, a notice.
    pub fn lead(mut self, lead: impl Into<Line<'a>>) -> Self {
        self.lead = lead.into();
        self
    }

    /// What sits at the right end: each machine's connection.
    pub fn right(mut self, right: impl Into<Line<'a>>) -> Self {
        self.right = right.into();
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
        // The right side, when it fits beside the lead.
        let right_width = u16::try_from(line_width(&self.right)).unwrap_or(u16::MAX);
        let mut limit = end;
        if right_width > 0 && start + lead_width + gap + right_width <= end {
            let right_x = end - right_width;
            self.right
                .render(Rect::new(right_x, area.y, right_width, 1), buf);
            limit = right_x.saturating_sub(gap);
        }
        let mut placed = Vec::new();
        for (at, hint) in self.hints.iter().enumerate() {
            let hint_width = hint.width();
            if x + hint_width > limit {
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

    fn connections(ui: Ui) -> Line<'static> {
        let theme = ui.theme;
        Line::from(vec![
            Span::styled(ui.glyphs.connected, Style::new().fg(theme.success)),
            Span::styled(" box  ", ui.text()),
            Span::styled(ui.glyphs.connected, Style::new().fg(theme.success)),
            Span::styled(" m2  ", ui.text()),
            Span::styled(ui.glyphs.connecting, Style::new().fg(theme.warning)),
            Span::styled(" vault", ui.text()),
        ])
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
                        .right(connections(ui))
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
