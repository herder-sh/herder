package sh.herder.android

import org.junit.Assert.assertEquals
import org.junit.Test
import sh.herder.android.ui.Block
import sh.herder.android.ui.blocks

class MarkdownTest {
    @Test
    fun repliesSplitIntoBlocks() {
        val text = "# Done\n\nI added the route\nand a test.\n\n- one\n2. two\n\n```\nfn main() {}\n```\nAfter."
        assertEquals(
            listOf(
                Block.Heading(1, "Done"),
                Block.Paragraph("I added the route and a test."),
                Block.Item("•", "one"),
                Block.Item("2.", "two"),
                Block.Code("fn main() {}"),
                Block.Paragraph("After."),
            ),
            blocks(text),
        )
    }

    @Test
    fun anUnclosedFenceIsStillCode() {
        assertEquals(listOf(Block.Paragraph("Look:"), Block.Code("let x = 1;")), blocks("Look:\n```rust\nlet x = 1;"))
    }
}
