//! The main pane: the open session's transcript, with the items streaming now at its end.
//!
//! Messages are wrapped to the pane, with light Markdown: headings, lists, quotes and code
//! fences. Tool calls and results take one line each, reasoning is dimmed. Lines are wrapped
//! here rather than by the paragraph, so the pane knows how many there are to scroll.

use herder_protocol::{Item, ItemBody};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use textwrap::Options;

use crate::app::{App, Focus};
use crate::session::{Entry, Session, Tone};

/// Marks the end of an item still streaming.
const CURSOR: &str = "▌";

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App) {
    let mut block = Block::bordered().border_style(super::border(app, Focus::Transcript));
    let Some(session) = app.open_session() else {
        let hint = Line::styled("Select a session and press Enter.", super::dim());
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(hint.centered(), super::centered(inner, inner.width, 1));
        return;
    };
    let (label, style) = super::sessions::badge(session.status);
    block = block.title(Line::from(vec![
        Span::raw(" "),
        Span::styled(session.title(), super::bold()),
        Span::raw(" "),
        Span::styled(label, style),
        Span::raw(" "),
    ]));
    // A child names its primary session, which may have answered some of its requests.
    if let Some(parent) = &session.parent {
        let primary = app
            .open
            .as_ref()
            .and_then(|key| app.primary(key))
            .map_or_else(|| parent.to_string(), |(_, primary)| primary.title());
        block = block.title_bottom(Line::styled(format!(" child of {primary} "), super::dim()));
    }
    if !session.model.is_empty() {
        let mode = crate::session::mode_name(session.permission_mode);
        let facts = format!(" {} · {mode} ", session.model);
        block = block.title(Line::styled(facts, super::dim()).right_aligned());
    }
    let inner = block.inner(area);
    let lines = if session.loaded {
        lines(session, usize::from(inner.width))
    } else {
        vec![Line::styled("loading…", super::dim())]
    };
    app.scroll.total = lines.len();
    app.scroll.height = usize::from(inner.height);
    let first = app.scroll.first_line();
    let shown: Vec<Line> = lines
        .into_iter()
        .skip(first)
        .take(app.scroll.height)
        .collect();
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(shown), inner);
}

/// The transcript as lines `width` wide.
pub(crate) fn lines(session: &Session, width: usize) -> Vec<Line<'static>> {
    let width = width.max(8);
    let mut out = Vec::new();
    for entry in &session.entries {
        match entry {
            Entry::Item(item) => self::item(&mut out, item, width, false),
            Entry::Notice { text, tone } => notice(&mut out, text, *tone, width),
        }
    }
    for item in &session.streaming {
        self::item(&mut out, item, width, true);
    }
    for prompt in &session.queued {
        if !out.is_empty() {
            out.push(Line::raw(""));
        }
        out.push(Line::from(vec![
            Span::styled(
                "you",
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" · queued", Style::new().fg(Color::Yellow)),
        ]));
        plain(&mut out, prompt, super::dim(), width);
    }
    out
}

fn item(out: &mut Vec<Line<'static>>, item: &Item, width: usize, streaming: bool) {
    match &item.body {
        ItemBody::UserMessage { text } => {
            heading(out, "you", Color::Cyan, streaming);
            plain(out, text, Style::new(), width);
        }
        ItemBody::AssistantMessage { text } => {
            heading(out, "assistant", Color::Green, streaming);
            markdown(out, text, width);
        }
        ItemBody::Reasoning { text } => {
            heading(out, "thinking", Color::DarkGray, streaming);
            let style = super::dim().add_modifier(Modifier::ITALIC);
            plain(out, text, style, width);
        }
        ItemBody::ToolCall { name, input } => {
            let summary = one_line(&tool_summary(input), width.saturating_sub(name.len() + 5));
            out.push(Line::from(vec![
                Span::styled("  ⚙ ", Style::new().fg(Color::Yellow)),
                Span::styled(name.clone(), Style::new().fg(Color::Yellow)),
                Span::raw(" "),
                Span::styled(summary, super::dim()),
            ]));
        }
        ItemBody::ToolResult {
            output, is_error, ..
        } => {
            let (mark, style) = if *is_error {
                ("✗ ", Style::new().fg(Color::Red))
            } else {
                ("⎿ ", super::dim())
            };
            let mut lines = output.lines().filter(|line| !line.trim().is_empty());
            let first = lines.next().unwrap_or("(no output)");
            let more = lines.count();
            let more = if more > 0 {
                format!(" (+{more} lines)")
            } else {
                String::new()
            };
            let text = one_line(first, width.saturating_sub(6 + more.len()));
            out.push(Line::from(vec![
                Span::styled(format!("    {mark}"), style),
                Span::styled(text, style),
                Span::styled(more, super::dim()),
            ]));
        }
        ItemBody::Unknown => {}
    }
    if streaming && let Some(last) = out.last_mut() {
        last.push_span(Span::styled(CURSOR, Style::new().fg(Color::Gray)));
    }
}

/// A message's name line, after a blank line.
fn heading(out: &mut Vec<Line<'static>>, name: &'static str, color: Color, streaming: bool) {
    if !out.is_empty() {
        out.push(Line::raw(""));
    }
    let mut line = Line::styled(name, Style::new().fg(color).add_modifier(Modifier::BOLD));
    if streaming {
        line.push_span(Span::styled(" …", super::dim()));
    }
    out.push(line);
}

fn notice(out: &mut Vec<Line<'static>>, text: &str, tone: Tone, width: usize) {
    let style = match tone {
        Tone::Info => super::dim(),
        Tone::Attention => Style::new().fg(Color::Magenta),
        Tone::Error => Style::new().fg(Color::Red),
    };
    out.push(Line::styled(
        format!("  · {}", one_line(text, width.saturating_sub(4))),
        style,
    ));
}

/// Text wrapped with a two-space indent, every line in `style`.
fn plain(out: &mut Vec<Line<'static>>, text: &str, style: Style, width: usize) {
    for line in text.lines() {
        wrapped(out, line, style, width, "  ", "  ");
    }
}

/// Assistant text: headings bold, list items with a hanging indent, quotes dimmed, fenced code
/// in its own colour and never re-flowed beyond the pane.
fn markdown(out: &mut Vec<Line<'static>>, text: &str, width: usize) {
    let code = Style::new().fg(Color::LightBlue);
    let mut in_code = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            out.push(Line::styled(format!("  {trimmed}"), super::dim()));
        } else if in_code {
            wrapped(out, line, code, width, "  ", "  ");
        } else if let Some(heading) = heading_text(trimmed) {
            wrapped(out, heading, super::bold(), width, "  ", "  ");
        } else if let Some(rest) = ["- ", "* ", "+ "]
            .iter()
            .find_map(|m| trimmed.strip_prefix(m))
        {
            let depth = " ".repeat(line.len() - trimmed.len());
            let first = format!("  {depth}• ");
            let next = format!("  {depth}  ");
            wrapped(out, rest, Style::new(), width, &first, &next);
        } else if let Some(rest) = trimmed.strip_prefix('>') {
            let style = super::dim().add_modifier(Modifier::ITALIC);
            wrapped(out, rest.trim_start(), style, width, "  │ ", "  │ ");
        } else {
            wrapped(out, line, Style::new(), width, "  ", "  ");
        }
    }
}

fn heading_text(line: &str) -> Option<&str> {
    let rest = line.trim_start_matches('#');
    let level = line.len() - rest.len();
    (1..=6).contains(&level).then(|| rest.strip_prefix(' '))?
}

fn wrapped(
    out: &mut Vec<Line<'static>>,
    text: &str,
    style: Style,
    width: usize,
    first: &str,
    next: &str,
) {
    if text.trim().is_empty() {
        out.push(Line::raw(""));
        return;
    }
    let options = Options::new(width)
        .initial_indent(first)
        .subsequent_indent(next);
    for line in textwrap::wrap(text, options) {
        out.push(Line::styled(line.into_owned(), style));
    }
}

/// `text` on one line at most `width` wide, cut with an ellipsis.
fn one_line(text: &str, width: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let width = width.max(2);
    let mut lines = textwrap::wrap(&text, width - 1);
    match lines.len() {
        0 => String::new(),
        1 => lines.remove(0).into_owned(),
        _ => format!("{}…", lines[0]),
    }
}

/// What a tool call does, from its arguments: the argument that says it best, else all of them.
fn tool_summary(input: &serde_json::Value) -> String {
    const TELLING: &[&str] = &[
        "command",
        "file_path",
        "path",
        "pattern",
        "url",
        "query",
        "description",
        "prompt",
    ];
    TELLING
        .iter()
        .find_map(|key| input.get(key).and_then(serde_json::Value::as_str))
        .map_or_else(|| input.to_string(), str::to_owned)
}
