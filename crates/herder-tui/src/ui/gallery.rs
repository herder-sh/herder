//! Every widget on one screen, with made-up data: what the screenshots show and `cargo run
//! -p herder-tui --example gallery` lets you drive. A change to a widget shows here first.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use ratatui_textarea::TextArea;

use super::badge;
use super::dialog::{Dialog, Size};
use super::hints::{ButtonBar, Hint, ModeBar};
use super::input::{Field, Prompt};
use super::list::{ListView, Row};
use super::state::{self, ALL, State};
use super::usage;
use super::{INSET, Ui, fill};

/// Screens narrower than this get the phone's button bar and one column.
const NARROW: u16 = crate::views::NARROW;

/// What has the keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    List,
    Prompt,
}

/// The gallery's state: the list's cursor, the prompt, and whether the dialog is open.
pub struct Gallery {
    pub selected: usize,
    pub offset: usize,
    pub focus: Focus,
    pub prompt: TextArea<'static>,
    pub dialog: bool,
    pub search: TextArea<'static>,
    pub button: Option<usize>,
}

impl Default for Gallery {
    fn default() -> Self {
        let mut prompt = TextArea::from(["write the docs page too, and link it from @README.md"]);
        prompt.set_placeholder_text("Write a prompt…");
        prompt.move_cursor(ratatui_textarea::CursorMove::End);
        let mut search = TextArea::from(["docs"]);
        search.move_cursor(ratatui_textarea::CursorMove::End);
        Self {
            selected: 1,
            offset: 0,
            focus: Focus::Prompt,
            prompt,
            dialog: false,
            search,
            button: None,
        }
    }
}

/// The sessions the list shows: state, title, project, machine.
const SESSIONS: [(State, &str, &str, &str); 7] = [
    (State::NeedsYou, "docs", "app", "box"),
    (State::NeedsYou, "api", "app", "box"),
    (State::Error, "deploy-preview", "infra", "box"),
    (State::Done, "fix-login", "app", "box"),
    (State::Running, "p2d-1-design", "herder", "m2"),
    (State::Running, "write tests", "app", "box"),
    (State::Waiting, "bump-deps", "herder", "m2"),
];

impl Gallery {
    /// Rows of the list: items are at 1..=SESSIONS.len().
    pub fn items() -> usize {
        SESSIONS.len()
    }

    /// Moves the list's cursor by `by` items, within the list.
    pub fn step(&mut self, by: isize) {
        let at = self.selected.saturating_sub(1).saturating_add_signed(by);
        self.selected = at.min(SESSIONS.len() - 1) + 1;
    }

    /// Draws the gallery over `area`; `label` names the theme and glyph set at the top right.
    pub fn draw(&mut self, ui: Ui, area: Rect, buf: &mut Buffer, label: &str) {
        fill(buf, area, ui.base());
        let narrow = area.width < NARROW;
        let [header, _, body, rule, bar] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);

        // The header: the brand, and what draws the screen.
        let title = Line::from(vec![
            Span::styled("herder", ui.strong()),
            Span::styled("  components", ui.muted()),
        ]);
        title.render(inset(header), buf);
        Line::from(Span::styled(label, ui.muted()))
            .right_aligned()
            .render(inset(header), buf);

        let content = if narrow {
            body
        } else {
            // The list as the sidebar; a rule; the rest.
            let side = (area.width / 3).clamp(30, 44);
            let [list, line, rest] = Layout::horizontal([
                Constraint::Length(side),
                Constraint::Length(3),
                Constraint::Fill(1),
            ])
            .areas(body);
            self.list(ui, list, buf);
            for y in line.top()..line.bottom() {
                buf[(line.x + 1, y)]
                    .set_symbol("│")
                    .set_style(Style::new().fg(ui.theme.border_subtle));
            }
            rest
        };
        let mut y = content.y;
        let width = content.width;
        let next = |y: &mut u16, height: u16| {
            let rect = Rect::new(content.x, *y, width, height).intersection(content);
            *y = y.saturating_add(height);
            rect
        };
        if narrow {
            let list_height = 9.min(content.height);
            let list = next(&mut y, list_height);
            self.list(ui, list, buf);
            y += 1;
        }

        // Status dots, in as many columns as fit.
        heading(ui, next(&mut y, 1), buf, "states");
        let cell = 13;
        let per_row = usize::from(width.saturating_sub(INSET) / cell).max(1);
        for chunk in ALL.chunks(per_row) {
            let row = inset(next(&mut y, 1));
            for (at, state) in chunk.iter().enumerate() {
                let x = row.x + u16::try_from(at).unwrap_or(0) * cell;
                state::labelled(ui, *state).render(
                    Rect {
                        x,
                        width: cell,
                        ..row
                    },
                    buf,
                );
            }
        }
        y += 1;

        heading(ui, next(&mut y, 1), buf, "badges");
        let theme = ui.theme;
        let gap = || Span::raw(" ");
        Line::from(vec![
            badge::solid(ui, "PROMPT", theme.primary),
            gap(),
            badge::solid(ui, "NAVIGATE", theme.secondary),
            gap(),
            badge::solid(ui, "APPROVAL", theme.attention),
        ])
        .render(inset(next(&mut y, 1)), buf);
        y += 1;
        Line::from(vec![
            badge::subtle(ui, "QUEUED", theme.warning),
            gap(),
            badge::subtle(ui, "open", theme.pr_open),
            gap(),
            badge::subtle(ui, "merged", theme.pr_merged),
            gap(),
            badge::subtle(ui, "draft", theme.pr_draft),
            gap(),
            badge::subtle(ui, "closed", theme.pr_closed),
        ])
        .render(inset(next(&mut y, 1)), buf);
        y += 1;

        heading(ui, next(&mut y, 1), buf, "usage · claude-main");
        let bar_width = width.saturating_sub(24).clamp(8, 30);
        for (window, percent, resets) in [
            ("5h", 38, "14:20"),
            ("week", 74, "Mon"),
            ("day", 93, "23:00"),
        ] {
            let mut line = usage::bar(ui, percent, bar_width);
            line.spans
                .insert(0, Span::styled(format!("{window:<5}"), ui.muted()));
            line.spans
                .push(Span::styled(format!("  {resets}"), ui.muted()));
            line.render(inset(next(&mut y, 1)), buf);
        }
        y += 1;

        // The prompt at the bottom of the column, as in a session.
        let prompt_height = Prompt::height(&self.prompt, width, 4, true);
        let bottom = content.bottom();
        let prompt_y = bottom.saturating_sub(prompt_height).max(y);
        let prompt = Rect::new(content.x, prompt_y, width, bottom.saturating_sub(prompt_y));
        let meta = Line::from(ui.joined([
            Span::styled("claude-main", ui.text()),
            Span::styled("opus", ui.muted()),
            Span::styled("ask", ui.muted()),
        ]));
        Prompt::new(ui, &mut self.prompt)
            .meta(meta)
            .focused(self.focus == Focus::Prompt && !self.dialog)
            .render(prompt, buf);

        // The bar: a rule over it, then hints or buttons.
        for x in rule.left()..rule.right() {
            buf[(x, rule.y)]
                .set_symbol("─")
                .set_style(Style::new().fg(ui.theme.border_subtle));
        }
        let hints = self.hints();
        if narrow {
            ButtonBar::new(ui, &hints)
                .focus(self.button)
                .render(bar, buf);
        } else {
            let (mode, color) = match (self.dialog, self.focus) {
                (true, _) => ("DIALOG", theme.accent),
                (false, Focus::Prompt) => ("PROMPT", theme.primary),
                (false, Focus::List) => ("NAVIGATE", theme.secondary),
            };
            let connection = |mark, color, name| {
                [
                    Span::styled(mark, Style::new().fg(color)),
                    Span::styled(format!(" {name}"), ui.text()),
                ]
            };
            let mut right = Vec::new();
            right.extend(connection(ui.glyphs.connected, theme.success, "box  "));
            right.extend(connection(ui.glyphs.connected, theme.success, "m2  "));
            right.extend(connection(ui.glyphs.connecting, theme.warning, "vault"));
            ModeBar::new(ui, &hints)
                .lead(badge::solid(ui, mode, color))
                .right(Line::from(right))
                .render(bar, buf);
        }

        if self.dialog {
            self.dialog(ui, area, buf);
        }
    }

    /// The keys that work now.
    pub fn hints(&self) -> Vec<Hint> {
        if self.dialog {
            return vec![Hint::new("enter", "open"), Hint::new("esc", "close")];
        }
        match self.focus {
            Focus::Prompt => vec![
                Hint::new("enter", "send"),
                Hint::new("tab", "list"),
                Hint::new("^t", "theme"),
                Hint::new("^g", "glyphs"),
                Hint::new("^o", "go to"),
                Hint::new("esc", "quit"),
            ],
            Focus::List => vec![
                Hint::new("j/k", "move"),
                Hint::new("tab", "prompt"),
                Hint::new("^t", "theme"),
                Hint::new("^g", "glyphs"),
                Hint::new("^o", "go to"),
                Hint::new("esc", "quit"),
            ],
        }
    }

    fn list(&mut self, ui: Ui, area: Rect, buf: &mut Buffer) {
        let mut rows = vec![Row::header("attention").right("priority")];
        for (at, (state, title, project, machine)) in SESSIONS.into_iter().enumerate() {
            let mut left = vec![state::dot(ui, state), Span::raw(" ")];
            left.extend(ui.joined([
                Span::styled(title, ui.text()),
                Span::styled(project, ui.muted()),
            ]));
            let mut row = Row::item(Line::from(left)).right(Span::styled(machine, ui.muted()));
            if at == 0 {
                row = row.body(vec![Line::styled(
                    "? which heading level for the API page",
                    ui.muted(),
                )]);
            }
            rows.push(row);
        }
        ListView::new(ui, rows)
            .select(Some(self.selected))
            .focused(self.focus == Focus::List && !self.dialog)
            .render(area, buf, &mut self.offset);
    }

    fn dialog(&mut self, ui: Ui, screen: Rect, buf: &mut Buffer) {
        let hints = self.hints();
        let areas = Dialog::new(ui, "go to", Size::Medium)
            .hints(&hints)
            .render(screen, 7, buf);
        let [search, _, list] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(areas.body);
        Field::new(ui, "/", &mut self.search)
            .focused(true)
            .render(search, buf, 2);
        let item = |state, title: &'static str, path: &'static str, machine: &'static str| {
            Row::item(Line::from(vec![
                state::dot(ui, state),
                Span::styled(format!(" {title}"), ui.text()),
                Span::styled(format!("  {path}"), ui.muted()),
            ]))
            .right(Span::styled(machine, ui.muted()))
        };
        let views = Line::from(ui.joined(
            ["inbox", "prs", "accounts", "fleet"].map(|view| Span::styled(view, ui.text())),
        ));
        let rows = vec![
            Row::header("sessions"),
            item(State::NeedsYou, "docs", "app › api › docs", "box"),
            item(State::Running, "p2d-1-design", "herder", "m2"),
            Row::header("views"),
            Row::item(views),
        ];
        // Headers line up with the search field; the pointer sits in the padding.
        let list = Rect {
            x: list.x.saturating_sub(1),
            width: list.width + 1,
            ..list
        };
        let mut offset = 0;
        ListView::new(ui, rows)
            .select(Some(1))
            .focused(true)
            .render(list, buf, &mut offset);
    }
}

/// A section's heading.
fn heading(ui: Ui, area: Rect, buf: &mut Buffer, text: &str) {
    Line::styled(text, ui.muted()).render(inset(area), buf);
}

/// `area` less [`INSET`] columns a side.
fn inset(area: Rect) -> Rect {
    Rect {
        x: area.x + INSET.min(area.width),
        width: area.width.saturating_sub(2 * INSET),
        ..area
    }
}

#[cfg(test)]
mod tests {
    use super::super::snapshot;
    use super::*;

    #[test]
    fn the_gallery_draws_every_widget() {
        for (width, height) in [(45, 40), (100, 30), (160, 40)] {
            for dialog in [false, true] {
                let variant = snapshot::variants().remove(0);
                let ui = variant.ui();
                let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
                let mut gallery = Gallery {
                    dialog,
                    ..Gallery::default()
                };
                gallery.draw(ui, buf.area, &mut buf, "dark · unicode");
                let text: String = buf.content.iter().map(|cell| cell.symbol()).collect();
                // A dialog covers the middle of the screen.
                let shown: &[&str] = if dialog {
                    &["claude-main", "docs", "┌ go to"]
                } else {
                    &["needs you", "merged", "38%", "claude-main", "docs"]
                };
                for expected in shown {
                    assert!(text.contains(expected), "{width}: {expected}");
                }
                assert_eq!(text.contains("┌ go to"), dialog);
            }
        }
    }
}
