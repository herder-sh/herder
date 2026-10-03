//! The request panel: an approval or a question put to the user, drawn inline where the
//! prompt was, as OpenCode's permission panel. Its bar takes the `attention` colour, as the
//! session waits on the user; the body is the caller's, wrapped to [`Request::text_width`].
//!
//! ```text
//! ┃ △ approval · Bash                          asked 12s ago
//! ┃ $ rm -rf target/
//! ┃
//! ┃  allow   deny                  y allow · n deny · ←/→ enter
//! ```

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use super::{GAP, Ui, badge, fill, line_width, spread, width};

pub struct Request<'a> {
    ui: Ui<'a>,
    header: Line<'a>,
    age: Line<'a>,
    body: Vec<Line<'a>>,
    footer: Option<(Line<'a>, Line<'a>)>,
    padded: bool,
}

impl<'a> Request<'a> {
    /// A panel headed `header`, with how long it has waited at the right.
    pub fn new(ui: Ui<'a>, header: impl Into<Line<'a>>, age: impl Into<Line<'a>>) -> Self {
        Self {
            ui,
            header: header.into(),
            age: age.into(),
            body: Vec::new(),
            footer: None,
            padded: false,
        }
    }

    /// The command, diff or question, one line per row.
    pub fn body(mut self, body: Vec<Line<'a>>) -> Self {
        self.body = body;
        self
    }

    /// The last row, after a blank one: the buttons, and the keys at the right.
    pub fn footer(mut self, left: impl Into<Line<'a>>, right: impl Into<Line<'a>>) -> Self {
        self.footer = Some((left.into(), right.into()));
        self
    }

    /// A blank row above and below, as on a desktop.
    pub fn padded(mut self, padded: bool) -> Self {
        self.padded = padded;
        self
    }

    /// Columns a body line may take in a panel `width` columns wide.
    pub fn text_width(width: u16) -> usize {
        usize::from(width.saturating_sub(3)).max(1)
    }

    /// Rows the panel takes.
    pub fn height(&self) -> u16 {
        let rows = 1 + self.body.len() + if self.footer.is_some() { 2 } else { 0 };
        u16::try_from(rows).unwrap_or(u16::MAX) + if self.padded { 2 } else { 0 }
    }

    /// Draws the panel; returns the footer's row, for taps on its buttons.
    pub fn render(self, area: Rect, buf: &mut Buffer) -> Option<Rect> {
        let ui = self.ui;
        let area = area.intersection(buf.area);
        if area.height == 0 || area.width < 4 {
            return None;
        }
        fill(buf, area, ui.panel());
        for y in area.top()..area.bottom() {
            buf[(area.x, y)]
                .set_symbol(ui.glyphs.bar)
                .set_fg(ui.theme.attention);
        }
        let text_width = Self::text_width(area.width);
        let text = |y: u16| Rect::new(area.x + 2, y, area.width - 3, 1);
        let mut y = area.y + u16::from(self.padded);
        let mut rows = vec![spread(self.header, self.age, text_width, ui.glyphs)];
        rows.extend(self.body);
        for line in rows {
            if y >= area.bottom() {
                return None;
            }
            super::fit(line, text_width, ui.glyphs).render(text(y), buf);
            y += 1;
        }
        let (left, right) = self.footer?;
        y += 1;
        if y >= area.bottom() {
            return None;
        }
        // The buttons come first: the keys beside them go when both do not fit.
        let right = if line_width(&left) + GAP + line_width(&right) > text_width {
            Line::default()
        } else {
            right
        };
        spread(left, right, text_width, ui.glyphs).render(text(y), buf);
        Some(text(y))
    }
}

/// Buttons in a row: the `chosen` one solid in `attention`, the others muted; with where
/// each starts and how wide it is.
pub fn buttons(ui: Ui, labels: &[&str], chosen: usize) -> (Line<'static>, Vec<(u16, u16)>) {
    let mut spans = Vec::new();
    let mut taps = Vec::new();
    let mut x = 0;
    for (at, label) in labels.iter().enumerate() {
        if at > 0 {
            spans.push(Span::raw(" ".repeat(GAP)));
            x += GAP;
        }
        let span = if at == chosen {
            badge::solid(ui, label, ui.theme.attention)
        } else {
            Span::styled(format!(" {label} "), ui.muted())
        };
        let w = width(&span.content);
        taps.push((
            u16::try_from(x).unwrap_or(u16::MAX),
            u16::try_from(w).unwrap_or(u16::MAX),
        ));
        x += w;
        spans.push(span);
    }
    (Line::from(spans), taps)
}

/// The header's start: the request's mark in `attention`, then `title` bold.
pub fn header<'a>(ui: Ui<'a>, mark: &'static str, title: String) -> Vec<Span<'a>> {
    vec![
        Span::styled(mark, Style::new().fg(ui.theme.attention)),
        Span::raw(" "),
        Span::styled(title, ui.strong()),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::snapshot;
    use super::*;

    #[test]
    fn requests() {
        snapshot::each("request", |variant| {
            snapshot::render(variant, 60, 14, |ui, area, buf| {
                let mut head = header(ui, ui.glyphs.approval, "approval".into());
                head.push(Span::styled(
                    format!("{}Bash", ui.glyphs.separator),
                    ui.muted(),
                ));
                let (allow, taps) = buttons(ui, &["allow", "deny"], 0);
                assert_eq!(taps, [(0, 7), (9, 6)]);
                let approval = Request::new(ui, head, Span::styled("asked 12s ago", ui.muted()))
                    .body(vec![Line::styled("$ rm -rf target/", ui.text())])
                    .footer(allow, Span::styled("y allow · n deny", ui.muted()))
                    .padded(true);
                assert_eq!(approval.height(), 6);
                let footer = approval.render(Rect { height: 6, ..area }, buf);
                assert_eq!(footer, Some(Rect::new(2, 4, 57, 1)));
                let question = Request::new(
                    ui,
                    header(ui, ui.glyphs.question, "question".into()),
                    Line::default(),
                )
                .body(vec![
                    Line::styled("Which heading level for the API page?", ui.text()),
                    Line::styled(" 1 h2 under Reference", ui.text()),
                    Line::styled(" 2 h1, its own page", ui.text()),
                ]);
                assert_eq!(question.height(), 4);
                question.render(
                    Rect {
                        y: 7,
                        height: 4,
                        ..area
                    },
                    buf,
                );
                // Too narrow for the keys too: the buttons stay whole.
                let (deny, _) = buttons(ui, &["allow", "deny"], 1);
                let narrow =
                    Request::new(ui, header(ui, ui.glyphs.approval, "approval".into()), "")
                        .footer(deny, Span::styled("y allow · n deny", ui.muted()));
                let area = Rect::new(0, 11, 22, 3);
                narrow.render(area, buf);
            })
        });
    }
}
