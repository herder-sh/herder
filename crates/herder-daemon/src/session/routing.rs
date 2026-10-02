//! Who answers a child session's questions and approval requests: its primary session first,
//! the user when the decision is not the primary's to make.
//!
//! # The rule
//!
//! A top-level session's requests go to the user. A child's go to its primary session, which
//! sees them through `wait_for` and `status` and settles them with `answer` or hands them on
//! with `escalate`. A request goes to the user instead when:
//!
//! - the primary escalates it (`marked_by_primary`, with the primary's note);
//! - it exceeds the primary's authority (`exceeds_authority`, decided when it is asked, so the
//!   primary never sees it): see [`within_authority`] and, for shell commands, [`commands`];
//! - the primary leaves it unanswered for [`PRIMARY_TIMEOUT`] (`timeout`).
//!
//! A user can answer any request at any time, whatever its route; the first answer wins. Every
//! request that goes to the user this way is also handed to the [`Notifier`].
//!
//! # Ids the primary sees
//!
//! Adapters mint question and approval ids unique within a session only, and `answer` and
//! `escalate` name a request by its id alone. So the primary sees each id prefixed with the
//! child's session id, `<child>/<id>` ([`primary_id`]), which [`split_id`] takes apart again.
//! Journals and clients keep the adapter's id.

mod commands;

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use herder_protocol::{EscalationReason, Provider, SessionId};
use herder_tasktools::Request;
use serde_json::Value;

/// How long a request routed to the primary session waits for its answer before it goes to
/// the user.
pub(super) const PRIMARY_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Told about every child request that goes to the user instead of its primary session, so it
/// can reach the user outside herder's clients.
pub trait Notifier: Send + Sync + 'static {
    /// `escalation` now waits for the user. Must not block.
    fn escalated(&self, escalation: &Escalation);
}

/// A child's request that went to the user instead of its primary session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Escalation {
    /// The primary session of the child's task.
    pub primary: SessionId,
    /// The child that asked.
    pub child: SessionId,
    /// What the child asked, with the adapter's ids.
    pub request: Request,
    /// Why it went to the user.
    pub reason: EscalationReason,
    /// What the primary session told the user about it, as Markdown.
    pub note: Option<String>,
}

/// `request` of `child` as its primary session sees it: ids prefixed with the child's id.
pub(super) fn primary_id(child: &SessionId, request: &Request) -> Request {
    let id = |id: &str| format!("{child}/{id}");
    match request {
        Request::Question {
            question_id,
            text,
            choices,
        } => Request::Question {
            question_id: herder_protocol::QuestionId::new(id(question_id.as_str())),
            text: text.clone(),
            choices: choices.clone(),
        },
        Request::Approval {
            approval_id,
            summary,
        } => Request::Approval {
            approval_id: herder_protocol::ApprovalId::new(id(approval_id.as_str())),
            summary: summary.clone(),
        },
    }
}

/// The child and the adapter's id of an id the primary session was given.
pub(super) fn split_id(id: &str) -> Option<(SessionId, &str)> {
    let (child, id) = id.split_once('/')?;
    (!child.is_empty() && !id.is_empty()).then(|| (SessionId::new(child), id))
}

/// Whether the primary session may decide an approval of tool call `name` with `input`, made
/// by `provider`, for a child working in `worktree`.
///
/// It may for a shell command not on the deny list ([`commands`]), and for a file edit inside
/// the worktree ([`edit_within_authority`]). The shell commands recognised are Claude's `Bash`
/// (`command`, run in the worktree) and Codex's `shell` (`command`, run in `cwd`); a command
/// tool of any other provider, or one whose command cannot be read, is beyond it.
pub(super) async fn within_authority(
    provider: &Provider,
    name: &str,
    input: &Value,
    worktree: &Path,
) -> bool {
    let cwd = match (provider, name) {
        (Provider::Claude, "Bash") => Some(worktree.to_owned()),
        (Provider::Codex, "shell") => input
            .get("cwd")
            .and_then(Value::as_str)
            .map(|cwd| worktree.join(cwd)),
        _ => return edit_within_authority(name, input, worktree),
    };
    let (Some(line), Some(cwd)) = (input.get("command").and_then(Value::as_str), cwd) else {
        return false;
    };
    let branches = commands::Branches::of(worktree).await;
    commands::primary_may_run(line, &cwd, worktree, &branches)
}

/// Whether the primary session may decide an approval of tool call `name` with `input` for a
/// child working in `worktree`, as a file edit.
///
/// It may only for a file edit whose every path is inside the child's worktree. Anything
/// else is beyond it:
///
/// - Every tool that is not an edit needs `full_access`, where herder's permission modes run
///   it unasked: fetches, MCP tools, anything unrecognised. Reads never ask. Shell commands
///   are [`within_authority`]'s.
/// - An edit is recognised by tool name and its paths read from the input: Claude's `Edit`,
///   `Write` and `MultiEdit` (`file_path`) and `NotebookEdit` (`notebook_path`), and Codex's
///   `apply_patch` (each of `changes[].path`, and a move's `kind.move_path`). An edit whose
///   paths cannot be read is beyond it.
/// - A path is inside the worktree when, taken relative to the worktree if it is relative,
///   with `.` and `..` resolved and the symlinks of its longest existing ancestor followed, it
///   is under the worktree's real path and not under its `.git`. A path starting with `~` is
///   outside, since the CLI expands it to the home directory.
fn edit_within_authority(name: &str, input: &Value, worktree: &Path) -> bool {
    let paths = match name {
        "Edit" | "Write" | "MultiEdit" => vec![input.get("file_path")],
        "NotebookEdit" => vec![input.get("notebook_path")],
        "apply_patch" => match input.get("changes").and_then(Value::as_array) {
            Some(changes) if !changes.is_empty() => changes
                .iter()
                .flat_map(|change| {
                    let moved = change.get("kind").and_then(|kind| kind.get("move_path"));
                    [change.get("path")].into_iter().chain(moved.map(Some))
                })
                .collect(),
            _ => return false,
        },
        _ => return false,
    };
    let Ok(root) = std::fs::canonicalize(worktree) else {
        return false;
    };
    paths.into_iter().all(|path| {
        path.and_then(Value::as_str)
            .is_some_and(|path| inside(&root, worktree, path))
    })
}

/// Whether `path`, relative to `base` if it is relative, is inside the worktree whose real
/// path is `root`.
fn inside(root: &Path, base: &Path, path: &str) -> bool {
    if path.is_empty() || path.starts_with('~') {
        return false;
    }
    let Some(path) = normalize(&base.join(path)) else {
        return false;
    };
    let Some(path) = resolve(&path) else {
        return false;
    };
    match path.strip_prefix(root) {
        Ok(rest) => rest.components().next() != Some(Component::Normal(".git".as_ref())),
        Err(_) => false,
    }
}

/// `path` with `.` and `..` resolved without touching the file system; `None` when `..` goes
/// above the root.
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            other => normalized.push(other),
        }
    }
    Some(normalized)
}

/// `path` with the symlinks of its longest existing ancestor followed.
fn resolve(path: &Path) -> Option<PathBuf> {
    let mut missing = Vec::new();
    let mut existing = path;
    loop {
        if let Ok(real) = std::fs::canonicalize(existing) {
            return Some(
                missing
                    .iter()
                    .rev()
                    .fold(real, |path, name| path.join(name)),
            );
        }
        missing.push(existing.file_name()?);
        existing = existing.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn edits_inside_the_worktree_are_the_primarys_to_decide() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("wt");
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        let file = worktree.join("src/main.rs");
        let within = |name: &str, input: Value| edit_within_authority(name, &input, &worktree);

        assert!(within("Edit", json!({ "file_path": file })));
        assert!(within("Write", json!({ "file_path": "new/file.txt" })));
        assert!(within(
            "MultiEdit",
            json!({ "file_path": "./src/../src/lib.rs" })
        ));
        assert!(within(
            "NotebookEdit",
            json!({ "notebook_path": "a.ipynb" })
        ));
        let changes = json!({ "changes": [
            { "path": file, "kind": { "type": "update" } },
            { "path": "src/b.rs", "kind": { "type": "update", "move_path": "src/c.rs" } },
        ]});
        assert!(within("apply_patch", changes));
    }

    #[test]
    fn everything_else_goes_to_the_user() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        std::os::unix::fs::symlink(dir.path(), worktree.join("escape")).unwrap();
        let within = |name: &str, input: Value| edit_within_authority(name, &input, &worktree);

        // Not an edit or a shell command: needs full_access.
        assert!(!within("mcp__github__create_issue", json!({})));
        assert!(!within("WebFetch", json!({ "url": "https://example.com" })));
        // Outside the worktree.
        assert!(!within("Edit", json!({ "file_path": "/etc/passwd" })));
        assert!(!within("Write", json!({ "file_path": "../other/file" })));
        assert!(!within("Write", json!({ "file_path": "escape/file" })));
        assert!(!within("Write", json!({ "file_path": "~/.bashrc" })));
        assert!(!within("Write", json!({ "file_path": ".git/config" })));
        assert!(!within(
            "Write",
            json!({ "file_path": "/../../etc/passwd" })
        ));
        // Unreadable paths.
        assert!(!within("Edit", json!({})));
        assert!(!within("Edit", json!({ "file_path": "" })));
        assert!(!within("apply_patch", json!({})));
        let moved_out = json!({ "changes": [
            { "path": "a.rs", "kind": { "type": "update", "move_path": "/tmp/a.rs" } },
        ]});
        assert!(!within("apply_patch", moved_out));
    }

    #[tokio::test]
    async fn claude_and_codex_commands_are_checked_against_the_deny_list() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("wt");
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        let within = async |provider: Provider, name: &str, input: Value| {
            within_authority(&provider, name, &input, &worktree).await
        };
        let ls = json!({ "command": "ls" });
        let sudo = json!({ "command": "sudo ls" });
        let codex = |command: &str, cwd: &Path| json!({ "command": command, "cwd": cwd });

        assert!(within(Provider::Claude, "Bash", ls.clone()).await);
        assert!(!within(Provider::Claude, "Bash", sudo.clone()).await);
        let shell = "/usr/bin/bash -lc 'cargo test'";
        assert!(
            within(
                Provider::Codex,
                "shell",
                codex(shell, &worktree.join("src"))
            )
            .await
        );
        let shell = "/usr/bin/bash -lc 'sudo ls'";
        assert!(!within(Provider::Codex, "shell", codex(shell, &worktree)).await);
        // Codex's command runs outside the worktree, or nowhere it says.
        assert!(!within(Provider::Codex, "shell", codex("ls", dir.path())).await);
        assert!(!within(Provider::Codex, "shell", ls.clone()).await);
        // No command to read.
        assert!(!within(Provider::Claude, "Bash", json!({})).await);
        // Another provider's tool of the same name.
        assert!(!within(Provider::Codex, "Bash", ls.clone()).await);
        assert!(!within(Provider::Claude, "shell", codex("ls", &worktree)).await);
        assert!(!within(Provider::Other("echo".into()), "Bash", ls).await);
        // Edits still go by their own rule.
        let edit = json!({ "file_path": "src/main.rs" });
        assert!(within(Provider::Claude, "Edit", edit).await);
    }

    #[test]
    fn the_primary_sees_ids_prefixed_with_the_child() {
        let child = SessionId::new("01CHILD");
        let request = Request::Approval {
            approval_id: herder_protocol::ApprovalId::new("approval-1"),
            summary: "Edit a file".into(),
        };
        let seen = primary_id(&child, &request);
        let Request::Approval { approval_id, .. } = &seen else {
            panic!("expected an approval");
        };
        assert_eq!(approval_id.as_str(), "01CHILD/approval-1");
        assert_eq!(split_id(approval_id.as_str()), Some((child, "approval-1")));
        assert_eq!(split_id("approval-1"), None);
        assert_eq!(split_id("/approval-1"), None);
    }
}
