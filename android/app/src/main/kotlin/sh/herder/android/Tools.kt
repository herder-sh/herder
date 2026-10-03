package sh.herder.android

import org.json.JSONArray
import org.json.JSONException
import org.json.JSONObject
import org.json.JSONTokener

// How a tool call reads in the transcript, as docs/tui-design.md §5.1 and the GTK app's
// `linux/src/tools.rs` say it: a glyph by tool name, a one-line summary, and what its block
// shows once expanded. The table matches on the providers' names (Claude `Bash`, `Edit`, …;
// Codex `shell`, `apply_patch`, …); an unknown tool still reads generically.

/** What a tool does, which picks its glyph and summary. */
enum class ToolKind { Shell, Read, Write, Edit, Search, Web, Todo, Task, Other }

fun toolKind(name: String): ToolKind = when {
    name in setOf("Bash", "shell", "exec_command", "local_shell") -> ToolKind.Shell
    name in setOf("Read", "NotebookRead") -> ToolKind.Read
    name == "Write" -> ToolKind.Write
    name in setOf("Edit", "MultiEdit", "NotebookEdit", "apply_patch") -> ToolKind.Edit
    name in setOf("Grep", "Glob", "LS") -> ToolKind.Search
    name in setOf("WebFetch", "WebSearch", "web_search") -> ToolKind.Web
    name in setOf("TodoWrite", "TodoRead", "update_plan") -> ToolKind.Todo
    name in setOf("Task", "Agent") || name.startsWith("mcp__herder__") -> ToolKind.Task
    else -> ToolKind.Other
}

/** The glyph a tool's line starts with. */
fun toolGlyph(name: String): String = when (toolKind(name)) {
    ToolKind.Shell -> "$"
    ToolKind.Read -> "→"
    ToolKind.Write, ToolKind.Edit -> "←"
    ToolKind.Search -> "✱"
    ToolKind.Web -> "◈"
    ToolKind.Todo -> "☐"
    ToolKind.Task -> "◇"
    ToolKind.Other -> "⚙"
}

/** A tool call's input, parsed: an object, an array, a string, or `null` when it does not parse. */
fun parseInput(input: String): Any? = try {
    JSONTokener(input).nextValue()
} catch (_: JSONException) {
    null
}

private fun Any?.arg(key: String): String? = (this as? JSONObject)?.opt(key) as? String

/** A shell call's command line; `sh -c script` reads as the script. */
fun command(input: Any?): String {
    val obj = input as? JSONObject ?: return ""
    for (key in listOf("command", "cmd")) {
        when (val value = obj.opt(key)) {
            is String -> return value
            is JSONArray -> {
                val parts = (0 until value.length()).mapNotNull { value.opt(it) as? String }
                if (parts.size == 3 && parts[0].endsWith("sh") && parts[1] in setOf("-lc", "-c")) return parts[2]
                return parts.joinToString(" ")
            }
        }
    }
    return ""
}

private fun relative(worktree: String, path: String): String {
    val root = worktree.trimEnd('/')
    if (root.isEmpty()) return path
    return path.removePrefix("$root/")
}

/** [text] with the worktree's paths made relative to it. */
fun inWorktree(text: String, worktree: String): String {
    val root = worktree.trimEnd('/')
    return if (root.isEmpty()) text else text.replace("$root/", "")
}

private fun path(input: Any?, worktree: String): String =
    listOf("file_path", "path", "notebook_path").firstNotNullOfOrNull { input.arg(it) }
        ?.let { relative(worktree, it) }.orEmpty()

/** How many matches a search's output reports. */
private fun matches(output: String): Int {
    val first = output.lineSequence().firstOrNull().orEmpty()
    first.removePrefix("Found ").takeIf { it != first }?.split(' ')?.firstOrNull()?.toIntOrNull()?.let { return it }
    if (first.startsWith("No files found") || first.startsWith("No matches")) return 0
    return output.lineSequence().count { it.isNotBlank() }
}

/**
 * A tool call's one-line summary: a label and its arguments, `Read` `src/api.rs`. A shell call
 * has no label, only its command; a search counts its matches once [output] is known.
 */
fun toolSummary(name: String, input: String, output: String?, worktree: String): Pair<String, String> {
    val parsed = parseInput(input)
    return when (toolKind(name)) {
        ToolKind.Shell -> "" to inWorktree(firstLine(command(parsed)), worktree)
        ToolKind.Read -> "Read" to path(parsed, worktree)
        ToolKind.Write -> "Write" to path(parsed, worktree)
        ToolKind.Edit -> {
            val label = if (name == "apply_patch") "Patch" else "Edit"
            val path = patchText(parsed)?.let(::patchPath) ?: path(parsed, worktree)
            val lines = editLines(name, parsed)
            val counts = if (lines.isEmpty()) {
                ""
            } else {
                "  +${lines.count { it.kind == DiffKind.Added }} −${lines.count { it.kind == DiffKind.Removed }}"
            }
            label to relative(worktree, path) + counts
        }
        ToolKind.Search -> {
            val pattern = parsed.arg("pattern").orEmpty()
            var args = if (name == "LS") path(parsed, worktree) else "\"$pattern\""
            if (name != "LS") {
                parsed.arg("path")?.let { relative(worktree, it) }?.takeIf { it.isNotEmpty() }?.let { args += " in $it" }
            }
            if (output != null) {
                val n = matches(output)
                args += " ($n ${if (n == 1) "match" else "matches"})"
            }
            name to args
        }
        ToolKind.Web -> parsed.arg("url")?.let { "Fetch" to it } ?: ("Search" to "\"${parsed.arg("query").orEmpty()}\"")
        ToolKind.Todo -> {
            val todos = todos(parsed)
            "Todos" to "${todos.count { it.second == "completed" }}/${todos.size}"
        }
        ToolKind.Task -> {
            val what = listOf("description", "task", "prompt", "message", "summary")
                .firstNotNullOfOrNull { parsed.arg(it) }?.let(::firstLine).orEmpty()
            name.removePrefix("mcp__herder__") to what
        }
        ToolKind.Other -> {
            val label = name.removePrefix("mcp__").takeIf { it != name }?.replaceFirst("__", " ") ?: name
            val obj = parsed as? JSONObject
            val args = obj?.keys()?.asSequence()?.mapNotNull { key ->
                when (val value = obj.opt(key)) {
                    is String -> "$key=${firstLine(value)}"
                    is Number, is Boolean -> "$key=$value"
                    else -> null
                }
            }?.joinToString(" ").orEmpty()
            label to args
        }
    }
}

/** A todo list's items: each one's text and status. */
fun todos(input: Any?): List<Pair<String, String>> {
    val obj = input as? JSONObject ?: return emptyList()
    val list = obj.optJSONArray("todos") ?: obj.optJSONArray("plan") ?: return emptyList()
    return (0 until list.length()).mapNotNull { list.optJSONObject(it) }.map { todo ->
        val text = listOf("content", "step", "text").firstNotNullOfOrNull { todo.arg(it) }.orEmpty()
        text to (todo.arg("status") ?: "pending")
    }
}

private fun patchText(input: Any?): String? = input as? String ?: input.arg("input") ?: input.arg("patch")

private fun patchPath(patch: String): String? = patch.lineSequence().firstNotNullOfOrNull { line ->
    listOf("*** Update File: ", "*** Add File: ", "*** Delete File: ")
        .firstNotNullOfOrNull { mark -> line.removePrefix(mark).takeIf { it != line } }
}

enum class DiffKind { Context, Added, Removed }

/** A line of a diff. */
data class DiffLine(val kind: DiffKind, val text: String)

/** Most lines on either side a diff is worked out for; past that, all old lines go and all new come. */
private const val DIFF_LIMIT = 400

/** The lines an edit removes and adds; empty for a tool that is not an edit. */
fun editLines(name: String, input: Any?): List<DiffLine> {
    if (name == "apply_patch") {
        val patch = patchText(input) ?: return emptyList()
        return patch.lines().mapNotNull { line ->
            when {
                line.startsWith("***") || line.startsWith("@@") -> null
                line.startsWith("+") -> DiffLine(DiffKind.Added, line.drop(1))
                line.startsWith("-") -> DiffLine(DiffKind.Removed, line.drop(1))
                line.startsWith(" ") -> DiffLine(DiffKind.Context, line.drop(1))
                else -> null
            }
        }
    }
    val obj = input as? JSONObject ?: return emptyList()
    val edits = obj.optJSONArray("edits")?.let { list -> (0 until list.length()).mapNotNull { list.optJSONObject(it) } }
        ?: listOf(obj)
    return edits.flatMap { edit ->
        val old = edit.arg("old_string") ?: return@flatMap emptyList<DiffLine>()
        val new = edit.arg("new_string") ?: return@flatMap emptyList<DiffLine>()
        diff(old.lines(), new.lines())
    }
}

/** A line diff of [old] to [new], by their longest common subsequence. */
fun diff(old: List<String>, new: List<String>): List<DiffLine> {
    if (old.size > DIFF_LIMIT || new.size > DIFF_LIMIT) {
        return old.map { DiffLine(DiffKind.Removed, it) } + new.map { DiffLine(DiffKind.Added, it) }
    }
    // common[i][j]: the longest common subsequence of old[i..] and new[j..].
    val common = Array(old.size + 1) { IntArray(new.size + 1) }
    for (i in old.indices.reversed()) {
        for (j in new.indices.reversed()) {
            common[i][j] = if (old[i] == new[j]) common[i + 1][j + 1] + 1 else maxOf(common[i + 1][j], common[i][j + 1])
        }
    }
    val lines = mutableListOf<DiffLine>()
    var i = 0
    var j = 0
    while (i < old.size || j < new.size) {
        when {
            i < old.size && j < new.size && old[i] == new[j] -> {
                lines += DiffLine(DiffKind.Context, old[i])
                i++
                j++
            }
            // What goes before what comes, as unified diffs read.
            i < old.size && (j == new.size || common[i + 1][j] >= common[i][j + 1]) -> lines += DiffLine(DiffKind.Removed, old[i++])
            else -> lines += DiffLine(DiffKind.Added, new[j++])
        }
    }
    return lines
}
