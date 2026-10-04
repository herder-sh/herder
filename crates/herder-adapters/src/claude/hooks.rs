//! The Claude Code hook herder adds to the sessions it runs; see the module docs.

use serde_json::{Value, json};

use crate::McpServer;

/// The arguments to the herder binary that run the hook: `herder hook claude-pre-tool-use`.
pub const PRE_TOOL_USE_ARGS: [&str; 2] = ["hook", "claude-pre-tool-use"];

/// Why an `Agent` call with `isolation: "worktree"` is denied, told to the model.
const WORKTREE_AGENT_REASON: &str = "herder runs this session: do not start an Agent with \
     isolation \"worktree\". Its builds would share this session's memory limit and its work \
     would not show in herder. Call mcp__herder__spawn to start a child session for the work \
     instead. If spawn is refused because this is already a child session, do the work \
     yourself in this session.";

/// The `--settings` JSON that adds the PreToolUse hook for `Agent` calls. `mcp` is herder's
/// own MCP server, whose command is the herder binary, so the hook runs that same binary.
pub(super) fn settings(mcp: &McpServer) -> Value {
    json!({
        "hooks": {
            "PreToolUse": [{
                "matcher": "Agent",
                "hooks": [{
                    "type": "command",
                    "command": mcp.command.to_string_lossy(),
                    "args": PRE_TOOL_USE_ARGS,
                }]
            }]
        }
    })
}

/// The hook's answer to the PreToolUse `input` Claude Code wrote on its stdin: a deny for an
/// `Agent` call with `isolation: "worktree"`, `None` (no opinion) for any other call.
pub fn pre_tool_use(input: &Value) -> Option<Value> {
    let worktree_agent = input["tool_name"] == "Agent"
        && input["tool_input"]["isolation"].as_str() == Some("worktree");
    worktree_agent.then(|| {
        json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": WORKTREE_AGENT_REASON,
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(tool_name: &str, tool_input: Value) -> Value {
        json!({
            "session_id": "499f57af-e4af-4c6c-b348-d47c9b704e70",
            "cwd": "/worktrees/s1",
            "permission_mode": "bypassPermissions",
            "hook_event_name": "PreToolUse",
            "tool_name": tool_name,
            "tool_input": tool_input,
            "tool_use_id": "toolu_01",
        })
    }

    #[test]
    fn denies_a_worktree_agent_and_points_to_spawn() {
        let call = input(
            "Agent",
            json!({
                "description": "Fix the build",
                "prompt": "Fix it",
                "subagent_type": "general-purpose",
                "isolation": "worktree",
            }),
        );
        let output = pre_tool_use(&call).unwrap();
        let decision = &output["hookSpecificOutput"];
        assert_eq!(decision["hookEventName"], "PreToolUse");
        assert_eq!(decision["permissionDecision"], "deny");
        let reason = decision["permissionDecisionReason"].as_str().unwrap();
        assert!(reason.contains("mcp__herder__spawn"));
        assert!(reason.contains("do the work yourself"));
    }

    #[test]
    fn leaves_other_calls_alone() {
        let explore = input(
            "Agent",
            json!({"description": "Find it", "prompt": "Where?", "subagent_type": "Explore"}),
        );
        assert_eq!(pre_tool_use(&explore), None);
        let remote = input(
            "Agent",
            json!({"description": "Do it", "prompt": "Do it", "isolation": "remote"}),
        );
        assert_eq!(pre_tool_use(&remote), None);
        let bash = input(
            "Bash",
            json!({"command": "echo worktree", "isolation": "worktree"}),
        );
        assert_eq!(pre_tool_use(&bash), None);
        assert_eq!(pre_tool_use(&json!({})), None);
    }
}
