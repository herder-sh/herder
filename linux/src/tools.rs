//! Tool calls, as docs/tui-design.md §5.1 shows them and the TUI words them: a glyph by
//! tool and a one-line summary of the arguments; for an edit, its diff.
//!
//! Names are the vendor CLI's own: Claude's (`Bash`, `Edit`, ...) and Codex's (`shell`,
//! `apply_patch`, ...). A tool the table does not know still gets the generic line.

use serde_json::Value;
use similar::{ChangeTag, TextDiff};

use crate::session::first_line;

/// Output lines a collapsed block shows.
pub const OUTPUT_LINES: usize = 10;
/// Diff lines a collapsed edit shows.
pub const DIFF_LINES: usize = 20;
/// Output lines a collapsed block of an unknown tool shows.
pub const OTHER_LINES: usize = 3;
/// Context lines kept around each change of a diff.
const CONTEXT: usize = 3;

/// The glyph of a tool, by what it does.
pub fn glyph(name: &str) -> &'static str {
    match kind(name) {
        Kind::Shell => "$",
        Kind::Read => "→",
        Kind::Write | Kind::Edit => "←",
        Kind::Search => "✱",
        Kind::Web => "◈",
        Kind::Todo => "☐",
        Kind::Task => "◇",
        Kind::Other => "›",
    }
}

/// What a tool does, by its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
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

pub fn kind(name: &str) -> Kind {
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
pub fn arg<'v>(input: &'v Value, key: &str) -> Option<&'v str> {
    input.get(key).and_then(Value::as_str)
}

/// The command a shell call runs: a string, or Codex's argument list.
pub fn command(input: &Value) -> String {
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
pub fn in_worktree(text: &str, worktree: &str) -> String {
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
pub fn summary(
    name: &str,
    input: &Value,
    output: Option<&str>,
    worktree: &str,
) -> (String, String) {
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
            let what = ["description", "task", "prompt", "message", "summary"]
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
pub fn todos(input: &Value) -> Vec<(String, String)> {
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
pub enum Diff {
    Context(String),
    Added(String),
    Removed(String),
    /// A gap between hunks.
    Gap,
}

/// The diff an edit makes, as lines with [`CONTEXT`] lines around each change.
pub fn diff(name: &str, input: &Value) -> Vec<Diff> {
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
pub fn clean(text: &str) -> String {
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn tool_lines_read_as_in_the_tui() {
        let wt = "/srv/wt/api";
        let line = |name: &str, input: Value, output: Option<&str>| {
            let (label, args) = summary(name, &input, output, wt);
            format!("{} {label} {args}", glyph(name)).replace("  ", " ")
        };
        assert_eq!(
            line("Bash", json!({"command": "cd /srv/wt/api/src && ls"}), None),
            "$ cd src && ls"
        );
        assert_eq!(
            line("Read", json!({"file_path": "/srv/wt/api/src/api.rs"}), None),
            "→ Read src/api.rs"
        );
        assert_eq!(
            line(
                "Grep",
                json!({"pattern": "fn main", "path": "/srv/wt/api/src"}),
                Some("Found 2 files\na\nb")
            ),
            "✱ Grep \"fn main\" in src (2 matches)"
        );
        assert_eq!(
            line("mcp__github__get_pr", json!({"number": 12}), None),
            "› github get_pr number=12"
        );
        let edit = json!({"file_path": "/srv/wt/api/a.rs", "old_string": "a\nb\n", "new_string": "a\nc\n"});
        assert_eq!(
            diff("Edit", &edit),
            [
                Diff::Context("a".into()),
                Diff::Removed("b".into()),
                Diff::Added("c".into()),
            ]
        );
    }
}
