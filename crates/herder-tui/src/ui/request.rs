//! The request panel: an approval or a question, in place of the prompt (OpenCode's
//! permission panel, with herder's two outcomes).
//!
//! The panel is the prompt's: a bar down the left, here in `attention`, on the panel
//! background. A header names the request and its age, the body is the command or the
//! question, capped at [`MAX_BODY`] rows, then why it was escalated, then the answers: two
//! buttons for an approval, the numbered choices and "type an answer" for a question. The
//! answer the arrows moved to is drawn solid.
//!
//! ```text
//! ┃ △ approval · Bash                         asked 12s ago
//! ┃ $ rm -rf target/
//! ┃
//! ┃  allow    deny                         y allow  n deny
//! ┃
//! ```

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use super::{GAP, Ui, fill, fit, line_width, width};

/// Most rows the body takes before the rest is folded; `f` shows it all.
pub const MAX_BODY: usize = 15;

/// What is asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Allow or deny a tool call.
    Approval,
    /// Pick a choice or type an answer.
    Question,
}

/// A request panel ready to draw.
pub struct Request<'a> {
    ui: Ui<'a>,
    kind: Kind,
    title: &'a str,
    age: Option<String>,
    body: &'a str,
    notes: Vec<Line<'a>>,
    choices: &'a [String],
    selected: Option<usize>,
    hint: Line<'a>,
}

impl<'a> Request<'a> {
    /// A request of `kind` about `title` (the tool, or who asks), saying `body`.
    pub fn new(ui: Ui<'a>, kind: Kind, title: &'a str, body: &'a str) -> Self {
        Self {
            ui,
            kind,
            title,
            age: None,
            body,
            notes: Vec::new(),
            choices: &[],
            selected: None,
            hint: Line::default(),
        }
    }

    /// How long it has waited: `12s`.
    pub fn age(mut self, age: Option<String>) -> Self {
        self.age = age;
        self
    }

    /// Lines under the body: why it was escalated, what the primary said.
    pub fn notes(mut self, notes: Vec<Line<'a>>) -> Self {
        self.notes = notes;
        self
    }

    /// A question's choices.
    pub fn choices(mut self, choices: &'a [String]) -> Self {
        self.choices = choices;
        self
    }

    /// The answer the arrows moved to: allow (0) or deny (1); a choice, or one past the
    /// choices for "type an answer".
    pub fn select(mut self, selected: Option<usize>) -> Self {
        self.selected = selected;
        self
    }

    /// The keys, at the right of an approval's buttons.
    pub fn hint(mut self, hint: impl Into<Line<'a>>) -> Self {
        self.hint = hint.into();
        self
    }

    /// The body wrapped `width` columns wide, and the rows folded away.
    fn body_lines(&self, width: u16) -> (Vec<String>, usize) {
        let room = usize::from(width.saturating_sub(3)).max(8);
        let mut lines: Vec<String> = self
            .body
            .lines()
            .flat_map(|line| {
                textwrap::wrap(line, room)
                    .into_iter()
                    .map(|part| part.into_owned())
                    .collect::<Vec<_>>()
            })
            .collect();
        let folded = lines.len().saturating_sub(MAX_BODY);
        if folded > 0 {
            // The last shown row says what is folded.
            lines.truncate(MAX_BODY - 1);
            return (lines, folded + 1);
        }
        (lines, 0)
    }

    /// Rows of the answers: the buttons' row, or each choice and "type an answer".
    fn answer_rows(&self) -> u16 {
        match self.kind {
            Kind::Approval => 1,
            Kind::Question => u16::try_from(self.choices.len() + 1).unwrap_or(u16::MAX),
        }
    }

    /// Rows the panel takes `width` columns wide.
    pub fn height(&self, width: u16) -> u16 {
        let (body, folded) = self.body_lines(width);
        let body = body.len() + usize::from(folded > 0);
        let rows = 1 + body + self.notes.len() + 1;
        u16::try_from(rows)
            .unwrap_or(u16::MAX)
            .saturating_add(self.answer_rows() + 1)
    }

    /// Draws into `area`; returns each answer drawn, by index as [`Request::select`] counts,
    /// and where.
    pub fn render(self, area: Rect, buf: &mut Buffer) -> Vec<(usize, Rect)> {
        let ui = self.ui;
        let theme = ui.theme;
        let area = area.intersection(buf.area);
        if area.height < 2 || area.width < 8 {
            return Vec::new();
        }
        fill(buf, area, ui.panel());
        for y in area.top()..area.bottom() {
            buf[(area.x, y)]
                .set_symbol(ui.glyphs.bar)
                .set_fg(theme.attention);
        }
        let text = Rect {
            x: area.x + 2,
            width: area.width - 3,
            height: 1,
            ..area
        };
        let mut y = area.y;
        let mut row = |buf: &mut Buffer, line: Line| {
            if y < area.bottom() {
                fit(line, usize::from(text.width), ui.glyphs).render(Rect { y, ..text }, buf);
            }
            y += 1;
        };

        let attention = Style::new()
            .fg(theme.attention)
            .add_modifier(Modifier::BOLD);
        let (glyph, word) = match self.kind {
            Kind::Approval => (ui.glyphs.approval, "approval"),
            Kind::Question => (ui.glyphs.question, "question"),
        };
        let mut header = vec![
            Span::styled(format!("{glyph} {word}"), attention),
            Span::styled(ui.glyphs.separator, ui.muted()),
            Span::styled(self.title, ui.strong()),
        ];
        if let Some(age) = &self.age {
            let age = format!("asked {age} ago");
            let used = line_width(&Line::from(header.clone()));
            let pad = usize::from(text.width).saturating_sub(used + width(&age));
            if pad >= GAP {
                header.push(Span::raw(" ".repeat(pad)));
                header.push(Span::styled(age, ui.muted()));
            }
        }
        row(buf, Line::from(header));

        let (body, folded) = self.body_lines(area.width);
        let body_style = match self.kind {
            Kind::Approval => ui.text(),
            Kind::Question => ui.strong(),
        };
        for line in body {
            row(buf, Line::styled(line, body_style));
        }
        if folded > 0 {
            row(
                buf,
                Line::from(vec![
                    Span::styled(
                        format!("{} {folded} more lines", ui.glyphs.ellipsis),
                        ui.muted(),
                    ),
                    Span::raw("  "),
                    Span::styled("f", ui.strong()),
                    Span::styled(" full", ui.muted()),
                ]),
            );
        }
        for note in self.notes {
            row(buf, note);
        }
        row(buf, Line::default());

        let mut placed = Vec::new();
        let solid = Style::new()
            .fg(theme.selected_list_item_text)
            .bg(theme.primary)
            .add_modifier(Modifier::BOLD);
        let raised = Style::new().fg(theme.text).bg(theme.background_element);
        match self.kind {
            Kind::Approval => {
                if y < area.bottom() {
                    let mut x = text.x;
                    for (at, label) in ["allow", "deny"].into_iter().enumerate() {
                        let style = if self.selected == Some(at) {
                            solid
                        } else {
                            raised
                        };
                        let button = format!("  {label}  ");
                        let w = u16::try_from(width(&button)).unwrap_or(u16::MAX);
                        let rect = Rect::new(x, y, w, 1).intersection(text_row(text, y));
                        Span::styled(button, style).render(rect, buf);
                        placed.push((at, rect));
                        x += w + 2;
                    }
                    let hint_width = u16::try_from(line_width(&self.hint)).unwrap_or(u16::MAX);
                    if x + hint_width + 1 < text.right() {
                        self.hint
                            .render(Rect::new(text.right() - hint_width, y, hint_width, 1), buf);
                    }
                }
            }
            Kind::Question => {
                let typed = self.choices.len();
                let labels = self
                    .choices
                    .iter()
                    .enumerate()
                    .map(|(at, choice)| (format!("{}", at + 1), choice.as_str()))
                    .chain([("enter".to_owned(), "or type an answer")]);
                for (at, (key, label)) in labels.enumerate() {
                    if y >= area.bottom() {
                        break;
                    }
                    let selected = self.selected == Some(at);
                    let rect = text_row(text, y);
                    if selected {
                        fill(buf, rect, Style::new().bg(theme.background_element));
                    }
                    let label_style = match (selected, at == typed) {
                        (true, _) => ui.strong(),
                        (false, true) => ui.muted(),
                        (false, false) => ui.text(),
                    };
                    fit(
                        Line::from(vec![
                            Span::raw(" "),
                            Span::styled(key, ui.accent().add_modifier(Modifier::BOLD)),
                            Span::raw(" "),
                            Span::styled(label, label_style),
                        ]),
                        usize::from(rect.width),
                        ui.glyphs,
                    )
                    .render(rect, buf);
                    placed.push((at, rect));
                    y += 1;
                }
            }
        }
        placed
    }
}

/// Row `y` of the text column.
fn text_row(text: Rect, y: u16) -> Rect {
    Rect { y, ..text }
}

#[cfg(test)]
mod tests {
    use super::super::snapshot;
    use super::*;

    #[test]
    fn approvals_and_questions() {
        let choices = vec![
            "h2 under Reference".to_owned(),
            "h1, its own page".to_owned(),
        ];
        for width in [45, 100] {
            snapshot::each(&format!("request-{width}"), |variant| {
                snapshot::render(variant, width, 16, |ui, area, buf| {
                    let hint = Line::from(vec![
                        Span::styled("y", ui.strong()),
                        Span::styled(" allow  ", ui.muted()),
                        Span::styled("n", ui.strong()),
                        Span::styled(" deny", ui.muted()),
                    ]);
                    let approval = Request::new(ui, Kind::Approval, "Bash", "$ rm -rf target/")
                        .age(Some("12s".to_owned()))
                        .select(Some(0))
                        .hint(hint);
                    let height = approval.height(area.width);
                    assert_eq!(height, 5);
                    let placed = approval.render(Rect { height, ..area }, buf);
                    assert_eq!(placed.len(), 2);
                    let question = Request::new(
                        ui,
                        Kind::Question,
                        "docs",
                        "Which heading level for the API page?",
                    )
                    .notes(vec![Line::styled(
                        "escalated: exceeds authority",
                        ui.muted(),
                    )])
                    .choices(&choices)
                    .select(Some(1));
                    let below = Rect {
                        y: area.y + height + 1,
                        height: question.height(area.width),
                        ..area
                    };
                    let placed = question.render(below, buf);
                    // Each choice, then "type an answer".
                    assert_eq!(placed.len(), 3);
                })
            });
        }
    }

    #[test]
    fn a_long_body_folds_and_says_how_much() {
        let variant = snapshot::variants().remove(0);
        let ui = variant.ui();
        let body = (1..=40)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let request = Request::new(ui, Kind::Approval, "Write", &body);
        // Header, 14 lines, the fold, a blank, the buttons, the foot.
        assert_eq!(request.height(80), 1 + MAX_BODY as u16 + 1 + 1 + 1);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 20));
        request.render(buf.area, &mut buf);
        let fold: String = (0..80).map(|x| buf[(x, 15)].symbol().to_owned()).collect();
        assert!(fold.contains("26 more lines"), "{fold}");
    }
}
