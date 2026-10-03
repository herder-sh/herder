package sh.herder.android

import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

// org.json is Android's: Robolectric provides it.
@RunWith(RobolectricTestRunner::class)
class ToolsTest {
    private val worktree = "/srv/wt/api"

    private fun summary(name: String, input: String, output: String? = null) = toolSummary(name, input, output, worktree)

    @Test
    fun eachToolReadsAsOneLine() {
        assertEquals("" to "cargo test", summary("Bash", """{"command":"cargo test\necho done"}"""))
        assertEquals("" to "ls src", summary("shell", """{"command":["bash","-lc","ls /srv/wt/api/src"]}"""))
        assertEquals("Read" to "src/api.rs", summary("Read", """{"file_path":"/srv/wt/api/src/api.rs"}"""))
        assertEquals(
            "Grep" to "\"Router::new\" in src (3 matches)",
            summary("Grep", """{"pattern":"Router::new","path":"/srv/wt/api/src"}""", "Found 3 files\na\nb\nc"),
        )
        assertEquals("Fetch" to "https://example.com", summary("WebFetch", """{"url":"https://example.com"}"""))
        assertEquals(
            "Todos" to "1/2",
            summary("TodoWrite", """{"todos":[{"content":"a","status":"completed"},{"content":"b"}]}"""),
        )
        assertEquals("spawn" to "write the tests", summary("mcp__herder__spawn", """{"task":"write the tests"}"""))
        assertEquals("github get_issue" to "number=4", summary("mcp__github__get_issue", """{"number":4}"""))
        assertEquals("⚙", toolGlyph("mcp__github__get_issue"))
        assertEquals("$", toolGlyph("Bash"))
    }

    @Test
    fun anEditCountsItsLines() {
        val input = """{"file_path":"/srv/wt/api/src/api.rs","old_string":"a\nb","new_string":"a\nc\nd"}"""
        assertEquals("Edit" to "src/api.rs  +2 −1", summary("Edit", input))
        assertEquals(
            listOf(DiffLine(DiffKind.Context, "a"), DiffLine(DiffKind.Removed, "b"), DiffLine(DiffKind.Added, "c"), DiffLine(DiffKind.Added, "d")),
            editLines("Edit", parseInput(input)),
        )
        val patch = """{"input":"*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"}"""
        assertEquals("Patch" to "src/lib.rs  +1 −1", summary("apply_patch", patch))
    }

    @Test
    fun inputThatIsNotJsonStillReads() {
        assertEquals("Read" to "", summary("Read", "not json"))
    }
}
