package sh.herder.android.ui

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

// The agent's Markdown, as much of it as replies use: paragraphs, headings, lists, fenced code,
// and inline `code` and **bold**. Anything else reads as plain text.

/** A block of Markdown. */
sealed interface Block {
    data class Paragraph(val text: String) : Block

    data class Heading(val level: Int, val text: String) : Block

    /** A list item; [marker] is `•` or its number. */
    data class Item(val marker: String, val text: String) : Block

    data class Code(val text: String) : Block
}

private val Ordered = Regex("""^(\d+)[.)]\s+(.*)""")
private val HeadingLine = Regex("""^(#{1,6})\s+(.*)""")

/** [text]'s blocks, in order. */
fun blocks(text: String): List<Block> {
    val blocks = mutableListOf<Block>()
    val paragraph = mutableListOf<String>()
    fun flush() {
        if (paragraph.isNotEmpty()) blocks += Block.Paragraph(paragraph.joinToString(" "))
        paragraph.clear()
    }
    var code: MutableList<String>? = null
    for (line in text.lines()) {
        val trimmed = line.trim()
        if (code != null) {
            if (trimmed.startsWith("```")) {
                blocks += Block.Code(code.joinToString("\n"))
                code = null
            } else {
                code.add(line)
            }
            continue
        }
        val heading = HeadingLine.find(trimmed)
        val ordered = Ordered.find(trimmed)
        when {
            trimmed.startsWith("```") -> {
                flush()
                code = mutableListOf()
            }
            trimmed.isEmpty() -> flush()
            heading != null -> {
                flush()
                blocks += Block.Heading(heading.groupValues[1].length, heading.groupValues[2])
            }
            trimmed.startsWith("- ") || trimmed.startsWith("* ") -> {
                flush()
                blocks += Block.Item("•", trimmed.drop(2))
            }
            ordered != null -> {
                flush()
                blocks += Block.Item("${ordered.groupValues[1]}.", ordered.groupValues[2])
            }
            else -> paragraph += trimmed
        }
    }
    // An unclosed fence is still streaming.
    code?.let { blocks += Block.Code(it.joinToString("\n")) }
    flush()
    return blocks
}

/** [text] with its inline `code` in monospace on [codeBackground] and its **bold** bold. */
fun inline(text: String, codeBackground: Color): AnnotatedString = buildAnnotatedString {
    var at = 0
    while (at < text.length) {
        val tick = text.indexOf('`', at)
        val bold = text.indexOf("**", at)
        val next = listOf(tick, bold).filter { it >= 0 }.minOrNull()
        if (next == null) {
            append(text.substring(at))
            break
        }
        append(text.substring(at, next))
        if (next == tick) {
            val end = text.indexOf('`', tick + 1)
            if (end < 0) {
                append(text.substring(tick))
                break
            }
            // Monospace is wide already: without the body's letter spacing.
            withStyle(SpanStyle(fontFamily = FontFamily.Monospace, background = codeBackground, letterSpacing = 0.sp)) {
                append(text.substring(tick + 1, end))
            }
            at = end + 1
        } else {
            val end = text.indexOf("**", bold + 2)
            if (end < 0) {
                append(text.substring(bold))
                break
            }
            withStyle(SpanStyle(fontWeight = FontWeight.SemiBold)) { append(text.substring(bold + 2, end)) }
            at = end + 2
        }
    }
}

/** [text] drawn as Markdown; [cursor] marks text still streaming. */
@Composable
fun Markdown(text: String, cursor: Boolean = false, modifier: Modifier = Modifier) {
    val blocks = remember(text) { blocks(text) }
    val codeBackground = MaterialTheme.colorScheme.surfaceContainerHighest
    Column(modifier, verticalArrangement = Arrangement.spacedBy(8.dp)) {
        blocks.forEachIndexed { index, block ->
            val tail = if (cursor && index == blocks.lastIndex) " ▌" else ""
            when (block) {
                is Block.Paragraph -> Text(
                    inline(block.text + tail, codeBackground),
                    style = MaterialTheme.typography.bodyLarge,
                )
                is Block.Heading -> Text(
                    inline(block.text + tail, codeBackground),
                    style = if (block.level <= 2) MaterialTheme.typography.titleMedium else MaterialTheme.typography.titleSmall,
                )
                is Block.Item -> Row {
                    Text(
                        block.marker,
                        style = MaterialTheme.typography.bodyLarge,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.width(24.dp),
                    )
                    Text(inline(block.text + tail, codeBackground), style = MaterialTheme.typography.bodyLarge)
                }
                is Block.Code -> Surface(
                    color = MaterialTheme.colorScheme.surfaceContainerHigh,
                    shape = RoundedCornerShape(12.dp),
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(
                        block.text + tail,
                        style = MaterialTheme.typography.bodySmall.copy(fontFamily = FontFamily.Monospace),
                        softWrap = false,
                        modifier = Modifier.horizontalScroll(rememberScrollState()).padding(12.dp),
                    )
                }
            }
        }
        if (blocks.isEmpty() && cursor) Text("▌", style = MaterialTheme.typography.bodyLarge)
    }
}
