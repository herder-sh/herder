//! Tool calls in the transcript (docs/tui-design.md §5.1): each one dense line, a glyph by
//! tool, a summary of its arguments, and at the right its status and how long it took. `e`
//! (or a tap) grows the line into a block on the panel; a block shows at most 10 lines of
//! output or 20 of a diff, then `… N more lines` (`c` copies them all).
//!
//! ```text
//!   $ cargo test --workspace                                         4s
//!   ← Edit src/health.rs  +12 -1
//!   $ cargo publish                                      ✗ failed · 2s
//! ```
//!
//! | tool | line | expanded |
//! |---|---|---|
//! | Bash / shell | `$ cmd` | its output |
//! | Read | `→ Read path` | its output |
//! | Write | `← Write path` | the content, numbered |
//! | Edit / patch | `← Edit path +12 -1` | the diff; split at 120 columns and up |
//! | Grep / Glob | `✱ Grep "pat" in dir (N matches)` | its output |
//! | Web fetch / search | `◈ Fetch url` | its output |
//! | Todo | `☐ Todos 2/5` | the list |
//! | task tools | `◇ spawn …` | its output |
//! | other / MCP | `› name k=v` | its output |
//!
//! Names are the vendor CLI's own: Claude's (`Bash`, `Edit`, ...) and Codex's (`shell`,
//! `apply_patch`, ...). A tool the table does not know still gets the generic line.

use herder_protocol::{CiStatus, Item, ItemBody, PrState, PullRequest};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use similar::{ChangeTag, TextDiff};

use super::markdown::wrap;
use super::transcript::{Builder, Row};
use crate::session::{Entry, Session, ToolApproval, first_line};
use crate::ui::{Ui, fit, spread, width};

/// Output lines a block shows.
const OUTPUT_LINES: usize = 10;
/// Diff rows an edit's block shows.
const DIFF_ROWS: usize = 20;
/// Context lines kept around each change of a diff.
const CONTEXT: usize = 3;

/// What a tool does, by its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Shell,
    Read,
    Write,
    Edit,
    Search,
    Web,
    Todo,
    Task,
    Other,
}

fn kind(name: &str) -> Kind {
    match name {
        "Bash" | "shell" | "exec_command" | "local_shell" => Kind::Shell,
        "Read" | "NotebookRead" => Kind::Read,
        "Write" => Kind::Write,
        "Edit" | "MultiEdit" | "NotebookEdit" | "apply_patch" => Kind::Edit,
        "Grep" | "Glob" | "LS" => Kind::Search,
        "WebFetch" | "WebSearch" | "web_search" => Kind::Web,
        "TodoWrite" | "TodoRead" | "update_plan" => Kind::Todo,
        "Task" | "Agent" => Kind::Task,
        name if name.starts_with("mcp__herder__") => Kind::Task,
        _ => Kind::Other,
    }
}

/// The string argument `key` of `input`.
fn arg<'v>(input: &'v Value, key: &str) -> Option<&'v str> {
    input.get(key).and_then(Value::as_str)
}

/// The command a shell call runs: a string, or Codex's argument list.
fn command(input: &Value) -> String {
    for key in ["command", "cmd"] {
        match input.get(key) {
            Some(Value::String(command)) => return command.clone(),
            Some(Value::Array(parts)) => {
                let parts: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
                // `bash -lc "<script>"`: the script says it.
                if let [shell, "-lc" | "-c", script] = parts.as_slice()
                    && shell.ends_with("sh")
                {
                    return (*script).to_owned();
                }
                return parts.join(" ");
            }
            _ => {}
        }
    }
    String::new()
}

/// `path` relative to `worktree`.
fn relative(worktree: &str, path: &str) -> String {
    let worktree = worktree.trim_end_matches('/');
    if worktree.is_empty() {
        return path.to_owned();
    }
    path.strip_prefix(worktree)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(path)
        .to_owned()
}

/// `text` with the worktree's paths relative to it, as a shell command run there reads.
pub(super) fn in_worktree(text: &str, worktree: &str) -> String {
    let worktree = worktree.trim_end_matches('/');
    if worktree.is_empty() {
        return text.to_owned();
    }
    text.replace(&format!("{worktree}/"), "")
}

/// The path a call works on.
fn path(input: &Value, worktree: &str) -> String {
    ["file_path", "path", "notebook_path"]
        .iter()
        .find_map(|key| arg(input, key))
        .map(|path| relative(worktree, path))
        .unwrap_or_default()
}

/// How many matches a search's output lists.
fn matches(output: &str) -> usize {
    let first = output.lines().next().unwrap_or("");
    if let Some(count) = first
        .strip_prefix("Found ")
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|count| count.parse().ok())
    {
        return count;
    }
    if first.starts_with("No files found") || first.starts_with("No matches") {
        return 0;
    }
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
}

/// A tool call's line: its label (shown as text) and what it works on (muted).
fn summary(name: &str, input: &Value, output: Option<&str>, worktree: &str) -> (String, String) {
    let kind = kind(name);
    match kind {
        Kind::Shell => (
            String::new(),
            in_worktree(first_line(&command(input)), worktree),
        ),
        Kind::Read => ("Read".into(), path(input, worktree)),
        Kind::Write => ("Write".into(), path(input, worktree)),
        Kind::Edit => {
            let label = if name == "apply_patch" {
                "Patch"
            } else {
                "Edit"
            };
            let path = match patch_text(input) {
                Some(patch) => patch_path(patch).unwrap_or_default(),
                None => path(input, worktree),
            };
            (label.into(), relative(worktree, &path))
        }
        Kind::Search => {
            let pattern = arg(input, "pattern").unwrap_or_default();
            let mut args = if name == "LS" {
                path(input, worktree)
            } else {
                format!("\"{pattern}\"")
            };
            if name != "LS"
                && let Some(dir) = arg(input, "path")
            {
                let dir = relative(worktree, dir);
                if !dir.is_empty() {
                    args.push_str(&format!(" in {dir}"));
                }
            }
            if let Some(output) = output {
                let n = matches(output);
                let noun = if n == 1 { "match" } else { "matches" };
                args.push_str(&format!(" ({n} {noun})"));
            }
            (name.into(), args)
        }
        Kind::Web => {
            if let Some(url) = arg(input, "url") {
                ("Fetch".into(), url.to_owned())
            } else {
                let query = arg(input, "query").unwrap_or_default();
                ("Search".into(), format!("\"{query}\""))
            }
        }
        Kind::Todo => {
            let todos = todos(input);
            let done = todos
                .iter()
                .filter(|(_, status)| *status == "completed")
                .count();
            ("Todos".into(), format!("{done}/{}", todos.len()))
        }
        Kind::Task => {
            let label = name.strip_prefix("mcp__herder__").unwrap_or(name);
            let what = [
                "description",
                "task",
                "prompt",
                "message",
                "summary",
                "title",
            ]
            .iter()
            .find_map(|key| arg(input, key))
            .map(|text| first_line(text).to_owned())
            .unwrap_or_default();
            (label.into(), what)
        }
        Kind::Other => {
            let label = match name.strip_prefix("mcp__") {
                Some(rest) => rest.replacen("__", " ", 1),
                None => name.to_owned(),
            };
            let args = match input {
                Value::Object(map) => map
                    .iter()
                    .filter_map(|(key, value)| match value {
                        Value::String(s) => Some(format!("{key}={}", first_line(s))),
                        Value::Number(n) => Some(format!("{key}={n}")),
                        Value::Bool(b) => Some(format!("{key}={b}")),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => String::new(),
            };
            (label, args)
        }
    }
}

/// A todo list's items: text and status.
fn todos(input: &Value) -> Vec<(String, String)> {
    let list = input
        .get("todos")
        .or_else(|| input.get("plan"))
        .and_then(Value::as_array);
    list.into_iter()
        .flatten()
        .map(|todo| {
            let text = ["content", "step", "text"]
                .iter()
                .find_map(|key| arg(todo, key))
                .unwrap_or_default();
            let status = arg(todo, "status").unwrap_or("pending");
            (text.to_owned(), status.to_owned())
        })
        .collect()
}

/// A patch's text, for Codex's `apply_patch`.
fn patch_text(input: &Value) -> Option<&str> {
    input
        .as_str()
        .or_else(|| arg(input, "input"))
        .or_else(|| arg(input, "patch"))
}

/// The first file a patch touches.
fn patch_path(patch: &str) -> Option<String> {
    patch.lines().find_map(|line| {
        ["*** Update File: ", "*** Add File: ", "*** Delete File: "]
            .iter()
            .find_map(|mark| line.strip_prefix(mark))
            .map(str::to_owned)
    })
}

/// A line of a diff.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Diff {
    Context(String),
    Added(String),
    Removed(String),
    /// A gap between hunks.
    Gap,
}

/// The diff an edit makes, as lines with [`CONTEXT`] lines around each change.
fn diff(name: &str, input: &Value) -> Vec<Diff> {
    if let Some(patch) = patch_text(input).filter(|_| name == "apply_patch") {
        let mut lines = Vec::new();
        for line in patch.lines() {
            if line.starts_with("***") {
                continue;
            }
            if line.starts_with("@@") {
                if !lines.is_empty() {
                    lines.push(Diff::Gap);
                }
                continue;
            }
            lines.push(match line.split_at_checked(1) {
                Some(("+", rest)) => Diff::Added(rest.to_owned()),
                Some(("-", rest)) => Diff::Removed(rest.to_owned()),
                Some((_, rest)) => Diff::Context(rest.to_owned()),
                None => Diff::Context(String::new()),
            });
        }
        return lines;
    }
    let edits: Vec<(&str, &str)> = match input.get("edits").and_then(Value::as_array) {
        Some(edits) => edits
            .iter()
            .map(|edit| {
                (
                    arg(edit, "old_string").unwrap_or_default(),
                    arg(edit, "new_string").unwrap_or_default(),
                )
            })
            .collect(),
        None => vec![(
            arg(input, "old_string").unwrap_or_default(),
            arg(input, "new_string").unwrap_or_default(),
        )],
    };
    let mut lines = Vec::new();
    for (old, new) in edits {
        let diff = TextDiff::from_lines(old, new);
        for group in diff.grouped_ops(CONTEXT) {
            if !lines.is_empty() {
                lines.push(Diff::Gap);
            }
            for op in group {
                for change in diff.iter_changes(&op) {
                    let text = change.value().trim_end_matches(['\n', '\r']).to_owned();
                    lines.push(match change.tag() {
                        ChangeTag::Equal => Diff::Context(text),
                        ChangeTag::Insert => Diff::Added(text),
                        ChangeTag::Delete => Diff::Removed(text),
                    });
                }
            }
        }
    }
    lines
}

/// `text` without control characters, tabs as four spaces.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\t' => out.push_str("    "),
            // An ANSI escape: `ESC [ ... letter`.
            '\u{1b}' => {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
            }
            '\n' => out.push('\n'),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Draws the tool call `item`, with its result when it came: one line, the tool's glyph,
/// name and arguments, its status and how long it took at the right; `e` grows it into a
/// block of its output, diff or content, capped at [`OUTPUT_LINES`] or [`DIFF_ROWS`].
pub(super) fn tool(
    b: &mut Builder,
    item: &Item,
    name: &str,
    input: &Value,
    result: Option<(&str, bool)>,
) {
    let ui = b.ui;
    let theme = ui.theme;
    let kind = kind(name);
    let output = result.map(|(output, _)| output);
    let failed = result.is_some_and(|(_, error)| error);
    let approval = b.session.tool_approvals.get(&item.id).copied();
    let expanded = b.expanded(&item.id) && b.chat.details;
    let (label, args) = summary(name, input, output, b.worktree);
    // The glyph's colour, and the status at the right: waiting for an approval, denied,
    // running, failed or done. Only the glyph and the status word take a colour; the
    // arguments stay muted, as OpenCode's.
    let glyph = ui.glyphs.tool(name);
    let took = took(b, &item.id).map(super::transcript::duration);
    let mut status = Vec::new();
    let mut denied = false;
    let glyph_style = match (approval, result) {
        (Some(ToolApproval::Pending), _) => {
            status.push(Span::styled(
                "needs approval",
                Style::new().fg(theme.warning),
            ));
            Style::new().fg(theme.warning)
        }
        (Some(ToolApproval::Denied), _) => {
            denied = true;
            status.push(Span::styled("denied", ui.muted()));
            ui.muted()
        }
        (_, None) => {
            status.push(Span::styled("running", ui.muted()));
            Style::new().fg(theme.state_running)
        }
        (_, Some((_, true))) => {
            status.push(Span::styled(
                format!("{} ", ui.glyphs.check_fail),
                Style::new().fg(theme.error),
            ));
            status.push(Span::styled("failed", ui.muted()));
            Style::new().fg(theme.error)
        }
        (_, Some(_)) => ui.muted(),
    };
    if let Some(took) = took.filter(|_| result.is_some()) {
        if !status.is_empty() {
            status.push(Span::styled(ui.glyphs.separator, ui.muted()));
        }
        status.push(Span::styled(took, ui.muted()));
    }
    let crossed = |style: Style| {
        if denied {
            style.add_modifier(Modifier::CROSSED_OUT)
        } else {
            style
        }
    };
    let mut spans = vec![Span::styled(glyph, glyph_style)];
    if !label.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(label.clone(), crossed(ui.text())));
    }
    if !args.is_empty() {
        spans.push(Span::raw(" "));
        // A shell call's command is all its line says: text, as a tool's name is; muted
        // once it failed, its marker saying so.
        let style = if label.is_empty() && !failed {
            ui.text()
        } else {
            ui.muted()
        };
        spans.push(Span::styled(args.clone(), crossed(style)));
    }
    let diff_lines = if kind == Kind::Edit {
        diff(name, input)
    } else {
        Vec::new()
    };
    if kind == Kind::Edit {
        let added = diff_lines
            .iter()
            .filter(|l| matches!(l, Diff::Added(_)))
            .count();
        let removed = diff_lines
            .iter()
            .filter(|l| matches!(l, Diff::Removed(_)))
            .count();
        if added + removed > 0 {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("+{added}"),
                Style::new().fg(theme.diff_added),
            ));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!("-{removed}"),
                Style::new().fg(theme.diff_removed),
            ));
        }
    }
    let status = Line::from(status);
    let out = output.map(clean).unwrap_or_default();
    let has_output = !out.trim().is_empty();
    let block = expanded
        && match kind {
            Kind::Edit => !diff_lines.is_empty(),
            Kind::Write => arg(input, "content").is_some_and(|c| !c.is_empty()),
            Kind::Todo => false,
            _ => has_output,
        };
    if !block {
        b.start(false);
        b.select(&item.id);
        inline(b, spans, status);
        if expanded && kind == Kind::Todo {
            todo_lines(b, input);
        }
        return;
    }
    b.start(true);
    b.select(&item.id);
    let title = |b: &Builder, spans: Vec<Span<'static>>| -> Vec<Span<'static>> {
        spread(
            Line::from(spans),
            status.clone(),
            b.width.saturating_sub(3),
            ui.glyphs,
        )
        .spans
    };
    let bar = if failed { theme.error } else { theme.border };
    match kind {
        Kind::Edit => {
            let title = title(b, spans);
            diff_block(b, title, &diff_lines);
        }
        Kind::Write => {
            let content = clean(arg(input, "content").unwrap_or_default());
            let digits = content.lines().count().max(1).to_string().len();
            let room = b.width.saturating_sub(digits + 5).max(1);
            let lines: Vec<&str> = content.lines().collect();
            let shown = lines.len().min(OUTPUT_LINES);
            let mut body = Vec::new();
            for (at, line) in lines.iter().take(shown).enumerate() {
                let number = Span::styled(
                    format!("{:>digits$} ", at + 1),
                    Style::new().fg(theme.diff_line_number),
                );
                let pad = Span::raw(" ".repeat(digits + 1));
                body.extend(wrap(
                    &[((*line).to_owned(), ui.text())],
                    room,
                    &[number],
                    &[pad],
                ));
            }
            more_row(b, &mut body, lines.len() - shown);
            let title = title(b, spans);
            b.block(bar, title, body);
        }
        Kind::Shell => {
            let command = in_worktree(&command(input), b.worktree);
            let mut body = Vec::new();
            // The description heads the block, with the command under it.
            let head = match arg(input, "description") {
                Some(description) => {
                    let room = b.width.saturating_sub(5);
                    body.extend(wrap(
                        &[(format!("$ {}", first_line(&command)), ui.muted())],
                        room,
                        &[],
                        &[],
                    ));
                    vec![
                        Span::styled(glyph, glyph_style),
                        Span::raw(" "),
                        Span::styled(description.to_owned(), ui.text()),
                    ]
                }
                None => spans,
            };
            let (lines, more) = output_lines(b, &out, OUTPUT_LINES, ui.text());
            body.extend(lines);
            more_row(b, &mut body, more);
            let title = title(b, head);
            b.block(bar, title, body);
        }
        _ => {
            let (mut body, more) = output_lines(b, &out, OUTPUT_LINES, ui.text());
            more_row(b, &mut body, more);
            let title = title(b, spans);
            b.block(bar, title, body);
        }
    }
}

/// A todo call's list, under its line.
fn todo_lines(b: &mut Builder, input: &Value) {
    let ui = b.ui;
    let theme = ui.theme;
    for (text, status) in todos(input) {
        let (mark, mark_style, text_style) = match status.as_str() {
            "completed" => (
                ui.glyphs.check_pass,
                Style::new().fg(theme.success),
                ui.muted(),
            ),
            "in_progress" => (ui.glyphs.bullet, Style::new().fg(theme.warning), ui.text()),
            _ => (" ", ui.muted(), ui.text()),
        };
        let line = Line::from(vec![
            Span::raw(" ".repeat(super::transcript::INDENT + 2)),
            Span::styled("[", ui.muted()),
            Span::styled(mark, mark_style),
            Span::styled("] ", ui.muted()),
            Span::styled(text, text_style),
        ]);
        b.line(fit(line, b.width, ui.glyphs));
    }
}

/// Seconds from the call `id` to its result.
fn took(b: &Builder, id: &herder_protocol::ItemId) -> Option<i64> {
    let session = b.session;
    let called = session.times.get(id)?;
    let result = session.entries.iter().find_map(|entry| match entry {
        Entry::Item(Item {
            id: result,
            body: ItemBody::ToolResult { call_id, .. },
            ..
        }) if call_id == id => session.times.get(result),
        _ => None,
    })?;
    // Under a second says nothing worth the room.
    Some(result.as_second() - called.as_second()).filter(|took| *took > 0)
}

/// A result whose call is not in the transcript: one line, its output's first.
pub(super) fn orphan(b: &mut Builder, item: &Item, output: &str, is_error: bool) {
    let ui = b.ui;
    let glyph_style = if is_error {
        Style::new().fg(ui.theme.error)
    } else {
        ui.muted()
    };
    b.start(false);
    b.select(&item.id);
    let glyph = ui.glyphs.tools[7];
    let status = if is_error {
        Line::from(vec![
            Span::styled(
                format!("{} ", ui.glyphs.check_fail),
                Style::new().fg(ui.theme.error),
            ),
            Span::styled("failed", ui.muted()),
        ])
    } else {
        Line::default()
    };
    inline(
        b,
        vec![
            Span::styled(glyph, glyph_style),
            Span::raw(" "),
            Span::styled(first_line(&clean(output)).to_owned(), ui.muted()),
        ],
        status,
    );
}

/// A one-line tool item, its status at the right end.
fn inline(b: &mut Builder, spans: Vec<Span<'static>>, status: Line<'static>) {
    let mut line = vec![Builder::indent()];
    line.extend(spans);
    // Short of the right edge by the transcript blocks' margin, so statuses line up with
    // their titles.
    let line = spread(
        Line::from(line),
        status,
        b.width.saturating_sub(1),
        b.ui.glyphs,
    );
    b.line(line);
}

/// `output`'s first `cap` lines, wrapped to a block; with how many lines were left out.
fn output_lines(
    b: &Builder,
    output: &str,
    cap: usize,
    style: Style,
) -> (Vec<Line<'static>>, usize) {
    let room = b.width.saturating_sub(3).max(1);
    let lines: Vec<&str> = output.trim_end().lines().collect();
    let shown = lines.len().min(cap);
    let mut out = Vec::new();
    for line in &lines[..shown] {
        out.extend(wrap(&[((*line).to_owned(), style)], room, &[], &[]));
    }
    (out, lines.len() - shown)
}

/// `… N more lines`, with the key that shows them.
fn more_row(b: &Builder, body: &mut Vec<Line<'static>>, more: usize) {
    if more == 0 {
        return;
    }
    let ui = b.ui;
    let noun = if more == 1 { "line" } else { "lines" };
    let left = Line::from(Span::styled(
        format!("{} {more} more {noun}", ui.glyphs.ellipsis),
        ui.muted(),
    ));
    let right = Line::from(vec![
        Span::styled("c", ui.text()),
        Span::styled(" copy all", ui.muted()),
    ]);
    body.push(spread(left, right, b.width.saturating_sub(3), ui.glyphs));
}

/// An edit's block: its line as the title, then the diff, unified or split.
fn diff_block(b: &mut Builder, title: Vec<Span<'static>>, lines: &[Diff]) {
    let ui = b.ui;
    let theme = ui.theme;
    let bar = theme.border;
    if b.padded {
        b.push(Row::panel(ui, bar, vec![]));
    }
    let title = fit(Line::from(title), b.width.saturating_sub(3), ui.glyphs);
    b.push(Row::panel(ui, bar, title.spans));
    let rows = if b.split {
        split_rows(b, lines)
    } else {
        unified_rows(b, lines)
    };
    let total = rows.len();
    let shown = total.min(DIFF_ROWS);
    for row in rows.into_iter().take(shown) {
        b.push(row);
    }
    let mut more = Vec::new();
    more_row(b, &mut more, total - shown);
    for line in more {
        b.push(Row::panel(ui, bar, line.spans));
    }
    if b.padded {
        b.push(Row::panel(ui, bar, vec![]));
    }
}

/// The colours of a diff line: its sign, its text, its background.
fn diff_style(ui: Ui, line: &Diff) -> (&'static str, Style, Style, Option<Color>) {
    let theme = ui.theme;
    match line {
        Diff::Added(_) => (
            "+",
            Style::new()
                .fg(theme.diff_added)
                .add_modifier(Modifier::BOLD),
            ui.text(),
            Some(theme.diff_added_bg),
        ),
        Diff::Removed(_) => (
            "-",
            Style::new()
                .fg(theme.diff_removed)
                .add_modifier(Modifier::BOLD),
            ui.text(),
            Some(theme.diff_removed_bg),
        ),
        Diff::Context(_) => (" ", ui.muted(), Style::new().fg(theme.diff_context), None),
        Diff::Gap => (" ", ui.muted(), ui.muted(), None),
    }
}

fn diff_text(line: &Diff) -> &str {
    match line {
        Diff::Added(text) | Diff::Removed(text) | Diff::Context(text) => text,
        Diff::Gap => "",
    }
}

/// One column of diff rows, `+` and `-` lines on their own backgrounds.
fn unified_rows(b: &Builder, lines: &[Diff]) -> Vec<Row> {
    let ui = b.ui;
    let room = b.width.saturating_sub(5).max(1);
    let mut rows = Vec::new();
    for line in lines {
        if *line == Diff::Gap {
            let gap = Span::styled(
                ui.glyphs.ellipsis,
                Style::new().fg(ui.theme.diff_hunk_header),
            );
            rows.push(Row::panel(ui, ui.theme.border, vec![gap]));
            continue;
        }
        let (sign, sign_style, text_style, bg) = diff_style(ui, line);
        let first = [Span::styled(sign, sign_style), Span::raw(" ")];
        let next = [Span::raw("  ")];
        for wrapped in wrap(
            &[(clean(diff_text(line)), text_style)],
            room + 2,
            &first,
            &next,
        ) {
            let mut row = Row::panel(ui, ui.theme.border, wrapped.spans);
            if let Some(bg) = bg {
                row.fills.push((2, 0, Style::new().bg(bg)));
            }
            rows.push(row);
        }
    }
    rows
}

/// Two columns of diff rows: what was on the left, what is now on the right, a removed run
/// beside the added run that replaced it.
fn split_rows(b: &Builder, lines: &[Diff]) -> Vec<Row> {
    let ui = b.ui;
    let half = b.width.saturating_sub(3) / 2;
    let mut pairs: Vec<(Option<&Diff>, Option<&Diff>)> = Vec::new();
    let mut at = 0;
    while at < lines.len() {
        match &lines[at] {
            Diff::Removed(_) => {
                let removed = lines[at..]
                    .iter()
                    .take_while(|line| matches!(line, Diff::Removed(_)))
                    .count();
                let added = lines[at + removed..]
                    .iter()
                    .take_while(|line| matches!(line, Diff::Added(_)))
                    .count();
                for i in 0..removed.max(added) {
                    pairs.push((
                        (i < removed).then(|| &lines[at + i]),
                        (i < added).then(|| &lines[at + removed + i]),
                    ));
                }
                at += removed + added;
            }
            Diff::Added(_) => {
                pairs.push((None, Some(&lines[at])));
                at += 1;
            }
            line => {
                pairs.push((Some(line), Some(line)));
                at += 1;
            }
        }
    }
    let cell = |line: Option<&Diff>| -> (Vec<Span<'static>>, Option<Color>) {
        let Some(line) = line else {
            return (vec![Span::raw(" ".repeat(half))], None);
        };
        if *line == Diff::Gap {
            let gap = Span::styled(
                ui.glyphs.ellipsis,
                Style::new().fg(ui.theme.diff_hunk_header),
            );
            let pad = half.saturating_sub(width(ui.glyphs.ellipsis));
            return (vec![gap, Span::raw(" ".repeat(pad))], None);
        }
        let (sign, sign_style, text_style, bg) = diff_style(ui, line);
        let line = Line::from(vec![
            Span::styled(sign, sign_style),
            Span::raw(" "),
            Span::styled(clean(diff_text(line)), text_style),
        ]);
        let line = fit(line, half, ui.glyphs);
        let pad = half.saturating_sub(crate::ui::line_width(&line));
        let mut spans = line.spans;
        spans.push(Span::raw(" ".repeat(pad)));
        (spans, bg)
    };
    let half16 = u16::try_from(half).unwrap_or(0);
    pairs
        .into_iter()
        .map(|(old, new)| {
            let (mut spans, old_bg) = cell(old);
            let (new_spans, new_bg) = cell(new);
            spans.push(Span::raw(" "));
            spans.extend(new_spans);
            let mut row = Row::panel(ui, ui.theme.border, spans);
            if let Some(bg) = old_bg {
                row.fills.push((2, half16, Style::new().bg(bg)));
            }
            if let Some(bg) = new_bg {
                row.fills.push((3 + half16, half16, Style::new().bg(bg)));
            }
            row
        })
        .collect()
}

/// What a child is doing now, for the line under its task: the tool it runs, that it waits
/// on the user, or how many tool calls it made.
pub(super) fn child_now(ui: Ui, child: &Session) -> Vec<Span<'static>> {
    if child.needs_user() {
        return vec![
            Span::styled(
                ui.glyphs.state(crate::ui::state::State::NeedsYou),
                Style::new().fg(ui.theme.attention),
            ),
            Span::styled(" needs you", Style::new().fg(ui.theme.attention)),
        ];
    }
    let mut calls = child.entries.iter().filter_map(|entry| match entry {
        Entry::Item(Item {
            body: ItemBody::ToolCall { name, input },
            ..
        }) => Some((name, input)),
        _ => None,
    });
    if child.turn.is_some() {
        return match calls.next_back() {
            Some((name, input)) => {
                let (label, args) = summary(name, input, None, &child.worktree);
                let mut spans = vec![Span::styled(ui.glyphs.tool(name), ui.muted())];
                for part in [label, args].into_iter().filter(|part| !part.is_empty()) {
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(part, ui.muted()));
                }
                spans
            }
            None => vec![Span::styled("working", ui.muted())],
        };
    }
    let count = calls.count();
    let noun = if count == 1 {
        "tool call"
    } else {
        "tool calls"
    };
    vec![Span::styled(format!("{count} {noun}"), ui.muted())]
}

/// A pull request's state and checks: `open · ci ✓`.
pub(super) fn pr_state(ui: Ui, pr: &PullRequest) -> Vec<Span<'static>> {
    let theme = ui.theme;
    let (state, color) = match pr.state {
        PrState::Draft => ("draft", theme.pr_draft),
        PrState::Open => ("open", theme.pr_open),
        PrState::Merged => ("merged", theme.pr_merged),
        PrState::Closed => ("closed", theme.pr_closed),
    };
    let (check, check_color) = match pr.ci {
        CiStatus::None => (ui.glyphs.check_none, theme.text_muted),
        CiStatus::Pending => (ui.glyphs.check_pending, theme.warning),
        CiStatus::Passing => (ui.glyphs.check_pass, theme.success),
        CiStatus::Failing => (ui.glyphs.check_fail, theme.error),
    };
    vec![
        Span::styled(state, Style::new().fg(color)),
        Span::styled(ui.glyphs.separator, ui.muted()),
        Span::styled("ci ", ui.muted()),
        Span::styled(check, Style::new().fg(check_color)),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn summaries_name_what_each_tool_works_on() {
        let wt = "/w/api";
        let s = |name, input, output| summary(name, &input, output, wt);
        assert_eq!(
            s("Bash", json!({"command": "cargo test\n--all"}), None),
            (String::new(), "cargo test".into())
        );
        assert_eq!(
            s("shell", json!({"command": ["bash", "-lc", "ls -la"]}), None),
            (String::new(), "ls -la".into())
        );
        assert_eq!(
            s("Read", json!({"file_path": "/w/api/src/api.rs"}), None),
            ("Read".into(), "src/api.rs".into())
        );
        assert_eq!(
            s(
                "Grep",
                json!({"pattern": "Router::new", "path": "/w/api/src"}),
                Some("Found 3 files\na\nb\nc")
            ),
            ("Grep".into(), "\"Router::new\" in src (3 matches)".into())
        );
        assert_eq!(
            s("Glob", json!({"pattern": "**/*.rs"}), Some("a.rs\n")),
            ("Glob".into(), "\"**/*.rs\" (1 match)".into())
        );
        assert_eq!(
            s("WebFetch", json!({"url": "https://x.dev"}), None),
            ("Fetch".into(), "https://x.dev".into())
        );
        assert_eq!(
            s(
                "TodoWrite",
                json!({"todos": [{"content": "a", "status": "completed"}, {"content": "b", "status": "pending"}]}),
                None
            ),
            ("Todos".into(), "1/2".into())
        );
        assert_eq!(
            s("mcp__herder__spawn", json!({"task": "write tests"}), None),
            ("spawn".into(), "write tests".into())
        );
        let page = json!({"title": "Latency\nby endpoint", "html": "<!doctype html><svg/>"});
        assert_eq!(
            s("mcp__herder__publish", page, None),
            ("publish".into(), "Latency".into())
        );
        assert_eq!(
            s(
                "mcp__github__search",
                json!({"q": "x", "n": 2, "deep": {}}),
                None
            ),
            ("github search".into(), "n=2 q=x".into())
        );
    }

    #[test]
    fn edits_diff_with_context_and_patches_read_as_diffs() {
        let lines = diff(
            "Edit",
            &json!({"old_string": "a\nb\nc\n", "new_string": "a\nB\nc\n"}),
        );
        assert_eq!(
            lines,
            [
                Diff::Context("a".into()),
                Diff::Removed("b".into()),
                Diff::Added("B".into()),
                Diff::Context("c".into()),
            ]
        );
        let patch = "*** Begin Patch\n*** Update File: src/a.rs\n@@\n x\n-y\n+z\n*** End Patch";
        let input = json!({"input": patch});
        assert_eq!(
            patch_path(patch_text(&input).unwrap()).as_deref(),
            Some("src/a.rs")
        );
        assert_eq!(
            diff("apply_patch", &input),
            [
                Diff::Context("x".into()),
                Diff::Removed("y".into()),
                Diff::Added("z".into()),
            ]
        );
    }

    #[test]
    fn commands_show_worktree_paths_relative() {
        let input = json!({"command": "cat /w/api/src/main.rs /etc/hosts"});
        assert_eq!(
            summary("Bash", &input, None, "/w/api/").1,
            "cat src/main.rs /etc/hosts"
        );
    }

    #[test]
    fn output_is_cleaned_of_escapes() {
        assert_eq!(
            clean("\u{1b}[32mok\u{1b}[0m\tdone\r\nnext"),
            "ok    done\nnext"
        );
    }
}
