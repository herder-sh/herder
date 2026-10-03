//! The open session's transcript, as OpenCode draws a chat.
//!
//! - The user's message: a bar in the accent, on the panel background.
//! - The agent's text: Markdown, three columns in, no frame; a reply ends with a footer,
//!   `▣ account · model · 1m 12s`.
//! - Reasoning: one line, `+ Thought: <first line> · 4s`.
//! - A tool call: one dense line, `→ Read src/api.rs`, which grows into a block on the panel
//!   when it has output or a diff ([`super::tools`]).
//! - Switches: a rule across the pane. Failures: a block with the bar in `error`.
//!
//! Items the item cursor (`[` / `]`) reaches are marked in the margin while the transcript
//! has the keys; `e` or a tap expands one. Rows are built here, wrapped to the pane, so the
//! pane knows how many there are to scroll.

use std::collections::{HashMap, HashSet};

use herder_protocol::{Attachment, AttachmentId, ErrorClass, Item, ItemBody, ItemId, SessionId};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use super::markdown::{self, wrap};
use super::tools;
use crate::action::Action;
use crate::app::{App, Focus};
use crate::chat::{Chat, ChatAct};
use crate::mouse::{Click, Hits, Wheel};
use crate::session::{Entry, Session, Tone, first_line};
use crate::ui::glyphs::Glyphs;
use crate::ui::{Ui, badge, fill, fit, spread};

/// Columns the agent's text and one-line items start in.
pub(super) const INDENT: usize = 3;

/// Screens at least this wide split diffs into two columns, as OpenCode's.
const SPLIT: u16 = 120;

/// One row of the transcript.
pub(crate) struct Row {
    pub line: Line<'static>,
    /// Backgrounds under the line: from which column, how many (0 to the end), what style.
    pub fills: Vec<(u16, u16, Style)>,
    /// The item the row belongs to, by index into the items drawn.
    pub item: Option<usize>,
}

impl Row {
    pub(super) fn new(line: Line<'static>) -> Self {
        Self {
            line,
            fills: Vec::new(),
            item: None,
        }
    }

    /// A row of a block on the panel, behind a bar of `bar`'s colour.
    pub(super) fn panel(ui: Ui, bar: ratatui::style::Color, rest: Vec<Span<'static>>) -> Self {
        let mut spans = vec![
            Span::styled(ui.glyphs.bar, Style::new().fg(bar)),
            Span::raw(" "),
        ];
        spans.extend(rest);
        let mut row = Self::new(Line::from(spans));
        row.fills.push((0, 0, ui.panel()));
        row
    }
}

/// Builds the transcript's rows.
pub(super) struct Builder<'a> {
    pub ui: Ui<'a>,
    /// Columns the rows take.
    pub width: usize,
    /// Blocks get a blank row above and below, as on a desktop.
    pub padded: bool,
    /// Diffs are split into two columns.
    pub split: bool,
    pub chat: &'a Chat,
    pub session: &'a Session,
    /// The open session's worktree, which paths are shown relative to.
    pub worktree: &'a str,
    /// Each tool call's result: its output, and whether it failed.
    pub results: HashMap<&'a ItemId, (&'a str, bool)>,
    /// The session's listed children.
    pub children: HashMap<&'a SessionId, &'a Session>,
    /// `account` and `model`, for a reply's footer.
    pub footer: (String, String),
    pub rows: Vec<Row>,
    /// The items the cursor reaches, in order.
    pub items: Vec<ItemId>,
    /// The item the rows being added belong to.
    item: Option<usize>,
    /// Whether the last thing added was a block, which a blank row sets off.
    last_block: bool,
}

impl<'a> Builder<'a> {
    /// Starts a new thing: one line (`block` false) or a block. A blank row sets blocks off
    /// from what is around them; one-line things run together.
    pub fn start(&mut self, block: bool) {
        if !self.rows.is_empty() && (block || self.last_block) {
            self.rows.push(Row::new(Line::raw("")));
        }
        self.last_block = block;
        self.item = None;
    }

    /// The rows added from now on belong to the item `id`, which the cursor can reach.
    pub fn select(&mut self, id: &ItemId) {
        self.item = Some(self.items.len());
        self.items.push(id.clone());
    }

    pub fn push(&mut self, mut row: Row) {
        row.item = self.item;
        self.rows.push(row);
    }

    pub fn line(&mut self, line: Line<'static>) {
        self.push(Row::new(line));
    }

    /// Whether `id` is expanded.
    pub fn expanded(&self, id: &ItemId) -> bool {
        self.chat.expanded.contains(id)
    }

    /// The indent of a one-line item.
    pub fn indent() -> Span<'static> {
        Span::raw(" ".repeat(INDENT))
    }

    fn entry(&mut self, entry: &'a Entry) {
        let ui = self.ui;
        let theme = ui.theme;
        match entry {
            Entry::Item(item) => self.item(item, false),
            Entry::Notice { text, tone } => {
                let style = match tone {
                    Tone::Info => ui.muted(),
                    Tone::Attention => Style::new().fg(theme.attention),
                };
                self.start(false);
                self.one_line(
                    vec![Span::styled(
                        ui.glyphs.states[8],
                        style.add_modifier(Modifier::BOLD),
                    )],
                    text,
                    style,
                );
            }
            Entry::Resolved { approval, text } => {
                let mark = if *approval {
                    ui.glyphs.approval
                } else {
                    ui.glyphs.question
                };
                self.start(false);
                self.one_line(vec![Span::styled(mark, ui.muted())], text, ui.muted());
            }
            Entry::TurnEnded { took, interrupted } => {
                let (account, model) = self.footer.clone();
                let mut parts = vec![Span::styled(account, ui.text())];
                if !model.is_empty() {
                    parts.push(Span::styled(model, ui.muted()));
                }
                if let Some(took) = took {
                    parts.push(Span::styled(duration(*took), ui.muted()));
                }
                if *interrupted {
                    parts.push(Span::styled("interrupted", Style::new().fg(theme.warning)));
                }
                let mut spans = vec![
                    Self::indent(),
                    Span::styled(ui.glyphs.reply, ui.accent()),
                    Span::raw(" "),
                ];
                spans.extend(ui.joined(parts));
                self.start(true);
                self.line(fit(Line::from(spans), self.width, ui.glyphs));
            }
            Entry::TurnFailed { class, message } => {
                let class = match class {
                    ErrorClass::LimitReached => "limit reached",
                    ErrorClass::Auth => "login needed",
                    ErrorClass::Transient => "transient",
                    ErrorClass::Fatal => "fatal",
                };
                self.start(true);
                let title = vec![
                    Span::styled(
                        ui.glyphs.state(crate::ui::state::State::Error),
                        Style::new().fg(theme.error),
                    ),
                    Span::raw(" "),
                    Span::styled("turn failed", ui.strong()),
                    Span::styled(format!("{}{class}", ui.glyphs.separator), ui.muted()),
                ];
                let body: Vec<Line<'static>> = message
                    .lines()
                    .flat_map(|line| {
                        wrap(
                            &[(line.to_owned(), ui.text())],
                            self.width.saturating_sub(3).max(1),
                            &[],
                            &[],
                        )
                    })
                    .collect();
                self.block(theme.error, title, body);
            }
            Entry::Switch(text) => {
                self.start(true);
                let label = format!(" {text} ");
                let room = self.width.saturating_sub(crate::ui::width(&label));
                let left = room / 2;
                let rule = Style::new().fg(theme.border_subtle);
                let line = Line::from(vec![
                    Span::styled("─".repeat(left), rule),
                    Span::styled(label, ui.muted()),
                    Span::styled("─".repeat(room - left), rule),
                ]);
                self.line(fit(line, self.width, ui.glyphs));
            }
            Entry::Child { session_id, task } => {
                self.start(false);
                let child = self.children.get(session_id).copied();
                let mut spans = vec![
                    Self::indent(),
                    Span::styled(ui.glyphs.tools[6], ui.muted()),
                    Span::raw(" "),
                    Span::styled("Task", ui.text()),
                    Span::raw(" "),
                    Span::styled(task.clone(), ui.muted()),
                ];
                if let Some(child) = child.filter(|child| !child.branch.is_empty()) {
                    spans.push(Span::styled(
                        format!(" {} ", ui.glyphs.states[7]),
                        ui.muted(),
                    ));
                    spans.push(Span::styled(child.branch.clone(), ui.muted()));
                }
                self.line(fit(Line::from(spans), self.width, ui.glyphs));
                if let Some(child) = child {
                    let now = tools::child_now(ui, child);
                    let mut spans = vec![
                        Span::raw(" ".repeat(INDENT + 2)),
                        Span::styled(format!("{} ", ui.glyphs.last), ui.muted()),
                    ];
                    spans.extend(now);
                    self.line(fit(Line::from(spans), self.width, ui.glyphs));
                }
            }
            Entry::Report {
                session_id,
                summary,
            } => {
                self.start(false);
                let task = self
                    .children
                    .get(session_id)
                    .map_or_else(|| session_id.to_string(), |child| child.title());
                let text = format!("report from {task}: {}", first_line(summary));
                self.one_line(
                    vec![Span::styled(ui.glyphs.last, ui.muted())],
                    &text,
                    ui.muted(),
                );
            }
            Entry::Pr(number) => {
                self.start(false);
                let pr = self.session.prs.iter().find(|pr| pr.number == *number);
                let mut spans = vec![
                    Self::indent(),
                    Span::styled(ui.glyphs.pr, ui.muted()),
                    Span::raw(" "),
                    Span::styled(format!("#{number}"), ui.text()),
                ];
                if let Some(pr) = pr {
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(pr.title.clone(), ui.muted()));
                    spans.push(Span::styled(ui.glyphs.separator, ui.muted()));
                    spans.extend(tools::pr_state(ui, pr));
                }
                self.line(fit(Line::from(spans), self.width, ui.glyphs));
            }
        }
    }

    /// A one-line item: `mark`, then `text` in `style`, cut to the pane.
    fn one_line(&mut self, mark: Vec<Span<'static>>, text: &str, style: Style) {
        let mut spans = vec![Self::indent()];
        spans.extend(mark);
        spans.push(Span::raw(" "));
        spans.push(Span::styled(first_line(text).to_owned(), style));
        let line = fit(Line::from(spans), self.width, self.ui.glyphs);
        self.line(line);
    }

    /// A block: a bar of `bar`'s colour down the left on the panel, `title`, then `body`.
    pub fn block(
        &mut self,
        bar: ratatui::style::Color,
        title: Vec<Span<'static>>,
        body: Vec<Line<'static>>,
    ) {
        let ui = self.ui;
        let room = self.width.saturating_sub(3);
        if self.padded {
            self.push(Row::panel(ui, bar, vec![]));
        }
        let title = fit(Line::from(title), room, ui.glyphs);
        self.push(Row::panel(ui, bar, title.spans));
        for line in body {
            self.push(Row::panel(ui, bar, line.spans));
        }
        if self.padded {
            self.push(Row::panel(ui, bar, vec![]));
        }
    }

    fn item(&mut self, item: &'a Item, streaming: bool) {
        let ui = self.ui;
        match &item.body {
            ItemBody::UserMessage { text, attachments } => {
                self.user(Some(&item.id), text, attachments, false);
            }
            ItemBody::AssistantMessage { text } => {
                self.start(true);
                self.select(&item.id);
                for row in markdown::markdown(ui, text, INDENT, self.width.saturating_sub(1)) {
                    self.push(row);
                }
            }
            ItemBody::Reasoning { text } => {
                if !self.chat.thinking {
                    return;
                }
                self.start(false);
                self.select(&item.id);
                let took = self.took(&item.id);
                let label = if streaming { "Thinking:" } else { "Thought:" };
                let thought = Style::new().fg(ui.theme.warning);
                let mut spans = vec![
                    Self::indent(),
                    Span::styled(format!("+ {label} "), thought),
                    Span::styled(first_line(text).to_owned(), ui.muted()),
                ];
                if let Some(took) = took {
                    spans.push(Span::styled(
                        format!("{}{}", ui.glyphs.separator, duration(took)),
                        ui.muted(),
                    ));
                }
                self.line(fit(Line::from(spans), self.width, ui.glyphs));
                if self.expanded(&item.id) {
                    let style = ui.muted().add_modifier(Modifier::ITALIC);
                    let pad = vec![Span::raw(" ".repeat(INDENT + 2))];
                    for line in text.lines().skip_while(|line| line.trim().is_empty()) {
                        for line in wrap(&[(line.to_owned(), style)], self.width - 1, &pad, &pad) {
                            self.line(line);
                        }
                    }
                }
            }
            ItemBody::ToolCall { name, input } => {
                let result = self.results.get(&item.id).copied();
                tools::tool(self, item, name, input, result);
            }
            ItemBody::ToolResult {
                call_id,
                output,
                is_error,
            } => {
                // Shown with its call; a result whose call is not in the transcript shows
                // on its own.
                if !self
                    .session
                    .entries
                    .iter()
                    .any(|entry| matches!(entry, Entry::Item(call) if call.id == *call_id))
                {
                    tools::orphan(self, item, output, *is_error);
                }
            }
            ItemBody::Unknown => {}
        }
        if streaming && let Some(last) = self.rows.last_mut() {
            last.line
                .push_span(Span::styled(ui.glyphs.cursor, ui.accent()));
        }
    }

    /// The user's message: a bar in the accent, the text on the panel; `queued` while the
    /// daemon holds it behind a running turn.
    fn user(&mut self, id: Option<&ItemId>, text: &str, images: &[Attachment], queued: bool) {
        let ui = self.ui;
        self.start(true);
        if let Some(id) = id {
            self.select(id);
        }
        let bar = ui.theme.primary;
        let room = self.width.saturating_sub(3).max(1);
        if self.padded {
            self.push(Row::panel(ui, bar, vec![]));
        }
        let mut lines: Vec<Line<'static>> = text
            .lines()
            .flat_map(|line| wrap(&[(line.to_owned(), ui.text())], room, &[], &[]))
            .collect();
        if queued && let Some(first) = lines.first_mut() {
            let badge = badge::subtle(ui, "queued", ui.theme.warning);
            *first = spread(first.clone(), Line::from(badge), room, ui.glyphs);
        }
        for line in lines {
            self.push(Row::panel(ui, bar, line.spans));
        }
        if !images.is_empty() && !text.is_empty() {
            self.push(Row::panel(ui, bar, vec![]));
        }
        let chips = image_chips(ui, images, &self.session.not_backed_up);
        for row in badge::chip_rows(chips, room) {
            self.push(Row::panel(ui, bar, row.spans));
        }
        if self.padded {
            self.push(Row::panel(ui, bar, vec![]));
        }
    }

    /// Seconds from what came before the item `id` in its turn to the item.
    fn took(&self, id: &ItemId) -> Option<i64> {
        let at = self.session.times.get(id)?;
        let mut before = None;
        for entry in &self.session.entries {
            if let Entry::Item(item) = entry {
                if item.id == *id {
                    break;
                }
                before = self.session.times.get(&item.id);
            }
        }
        let before = before?;
        Some(at.as_second() - before.as_second()).filter(|took| *took > 0)
    }
}

/// `seconds` as `4s`, `1m 12s` or `2h 05m`.
pub(super) fn duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h {:02}m", seconds / 3600, seconds % 3600 / 60),
    }
}

/// The transcript of `session` as rows `width` wide, with the items the cursor reaches.
pub(crate) fn rows(app: &App, session: &Session, width: u16) -> (Vec<Row>, Vec<ItemId>) {
    let ui = app.ui();
    let key = app.open.as_ref();
    let results = session
        .entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Item(item) => Some(item),
            _ => None,
        })
        .chain(&session.streaming)
        .filter_map(|item| match &item.body {
            ItemBody::ToolResult {
                call_id,
                output,
                is_error,
            } => Some((call_id, (output.as_str(), *is_error))),
            _ => None,
        })
        .collect();
    let children = key
        .map(|key| {
            app.children(key)
                .into_iter()
                .filter_map(|child| Some((&child.session_id, app.sessions.get(child)?)))
                .collect()
        })
        .unwrap_or_default();
    let account = key
        .zip(session.account_id.as_ref())
        .map(|(key, id)| {
            crate::account_screen::find(&app.machines, &key.host_id, id)
                .map_or_else(|| id.to_string(), |account| account.label.clone())
        })
        .unwrap_or_default();
    let mut builder = Builder {
        ui,
        width: usize::from(width).max(8),
        padded: width >= super::NARROW,
        split: app.width >= SPLIT,
        chat: &app.chat,
        session,
        worktree: &session.worktree,
        results,
        children,
        footer: (account, session.model.clone()),
        rows: Vec::new(),
        items: Vec::new(),
        item: None,
        last_block: false,
    };
    for entry in &session.entries {
        builder.entry(entry);
    }
    for item in &session.streaming {
        builder.item(item, true);
    }
    for prompt in &session.queued {
        builder.user(None, prompt, &[], true);
    }
    (builder.rows, builder.items)
}

/// Draws the open session's transcript in `area`, keeping a column of margin each side.
pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App, hits: &mut Hits) {
    let theme = app.theme.clone();
    let ui = Ui::new(&theme, Glyphs::for_width(app.glyphs, app.width));
    let Some(session) = app.open_session() else {
        let hint = Line::styled("Select a session and press Enter.", ui.muted());
        frame.render_widget(hint.centered(), super::centered(area, area.width, 1));
        return;
    };
    if area.width < 4 || area.height == 0 {
        return;
    }
    let inner = Rect {
        x: area.x + 1,
        width: area.width - 2,
        ..area
    };
    let (rows, items) = if session.loaded {
        rows(app, session, inner.width)
    } else {
        (
            vec![Row::new(Line::styled("loading…", ui.muted()))],
            Vec::new(),
        )
    };
    let cursor = app
        .chat
        .cursor
        .as_ref()
        .and_then(|id| items.iter().position(|item| item == id));
    app.scroll.total = rows.len();
    app.scroll.height = usize::from(inner.height);
    // Bring the item under a cursor that just moved into view.
    if std::mem::take(&mut app.chat.reveal)
        && let Some(cursor) = cursor
        && let Some(top) = rows.iter().position(|row| row.item == Some(cursor))
    {
        let first = app.scroll.first_line();
        if top < first || top >= first + app.scroll.height {
            app.scroll.top = Some(top.saturating_sub(1));
            app.scroll.by(0);
        }
    }
    app.chat.items = items;
    hits.click(area, Click::Open);
    let first = app.scroll.first_line();
    let marked = app.focus == Focus::Transcript;
    let buf = frame.buffer_mut();
    for (y, row) in (inner.y..inner.bottom()).zip(rows.iter().skip(first)) {
        let line_area = Rect::new(inner.x, y, inner.width, 1);
        for &(x, width, style) in &row.fills {
            let x = inner.x + x.min(inner.width);
            let right = if width == 0 {
                inner.right()
            } else {
                (x + width).min(inner.right())
            };
            fill(buf, Rect::new(x, y, right - x, 1), style);
        }
        row.line.clone().render(line_area, buf);
        if let Some(item) = row.item {
            if marked && Some(item) == cursor {
                buf[(area.x, y)]
                    .set_symbol(ui.glyphs.cursor)
                    .set_fg(ui.theme.primary);
            }
            hits.click(line_area, Click::Act(Action::Chat(ChatAct::ToggleAt(item))));
        }
    }
    hits.wheel(area, Wheel::Transcript);
}

/// A prompt's images as chips, `image 1 · 340 KB`. The terminal shows no images: `o` opens
/// them with the desktop's viewer, `w` saves them; the apps fetch and show them inline.
fn image_chips(
    ui: Ui,
    images: &[Attachment],
    not_backed_up: &HashSet<AttachmentId>,
) -> Vec<Span<'static>> {
    images
        .iter()
        .zip(1..)
        .map(|(image, n)| {
            let (fact, style) = if not_backed_up.contains(&image.attachment_id) {
                ("not backed up".to_owned(), ui.muted())
            } else {
                (crate::attach::size(image.size), ui.text())
            };
            badge::chip(
                ui,
                &format!("image {n}{}{fact}", ui.glyphs.separator),
                style,
            )
        })
        .collect()
}
