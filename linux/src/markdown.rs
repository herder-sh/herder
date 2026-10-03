//! Light Markdown for the transcript, as the TUI reads it: headings, lists, quotes, rules,
//! fenced code, and inline `code`, **strong**, *emphasis* and [links](url). Each block comes
//! out as Pango markup for a label; the transcript styles the blocks.

use gtk::glib;

/// One block of a Markdown text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// A paragraph, as markup; lines of it joined by spaces.
    Paragraph(String),
    /// A heading of level 1 to 6, as markup.
    Heading(u8, String),
    /// A list item: its marker (`•` or `3.`), nesting from 0, and its text as markup.
    Item(String, usize, String),
    /// A quoted paragraph, as markup.
    Quote(String),
    /// Fenced code, verbatim.
    Code(String),
    /// A horizontal rule.
    Rule,
}

/// The blocks of `text`.
pub fn blocks(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    let mut code: Option<Vec<&str>> = None;
    let flush = |paragraph: &mut Vec<&str>, blocks: &mut Vec<Block>| {
        if !paragraph.is_empty() {
            blocks.push(Block::Paragraph(inline(&paragraph.join(" "))));
            paragraph.clear();
        }
    };
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            match code.take() {
                Some(lines) => blocks.push(Block::Code(lines.join("\n"))),
                None => {
                    flush(&mut paragraph, &mut blocks);
                    code = Some(Vec::new());
                }
            }
            continue;
        }
        if let Some(lines) = &mut code {
            lines.push(line);
            continue;
        }
        let depth = (line.len() - trimmed.len()) / 2;
        if trimmed.is_empty() {
            flush(&mut paragraph, &mut blocks);
        } else if let Some((level, title)) = heading(trimmed) {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Heading(level, inline(title)));
        } else if is_rule(trimmed) {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Rule);
        } else if let Some(rest) = trimmed
            .strip_prefix("> ")
            .or(trimmed.strip_prefix('>').filter(|r| r.is_empty()))
        {
            flush(&mut paragraph, &mut blocks);
            match blocks.last_mut() {
                Some(Block::Quote(quote)) if !rest.is_empty() => {
                    quote.push(' ');
                    quote.push_str(&inline(rest));
                }
                _ => blocks.push(Block::Quote(inline(rest))),
            }
        } else if let Some((marker, rest)) = list_item(trimmed) {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Item(marker, depth, inline(rest)));
        } else if let (Some(Block::Item(_, _, item)), true) =
            (blocks.last_mut(), paragraph.is_empty() && depth > 0)
        {
            // A continuation line of a list item.
            item.push(' ');
            item.push_str(&inline(trimmed));
        } else {
            paragraph.push(trimmed);
        }
    }
    if let Some(lines) = code {
        blocks.push(Block::Code(lines.join("\n")));
    }
    flush(&mut paragraph, &mut blocks);
    blocks
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    let rest = line[hashes..].strip_prefix(' ')?;
    let level = u8::try_from(hashes)
        .ok()
        .filter(|level| (1..=6).contains(level))?;
    Some((level, rest.trim()))
}

fn is_rule(line: &str) -> bool {
    let line: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    line.len() >= 3
        && ["-", "*", "_"]
            .iter()
            .any(|mark| line.chars().all(|c| c.to_string() == *mark))
}

fn list_item(line: &str) -> Option<(String, &str)> {
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(bullet) {
            return Some(("•".to_owned(), rest));
        }
    }
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    let rest = line[digits..].strip_prefix(". ")?;
    (digits > 0).then(|| (format!("{}.", &line[..digits]), rest))
}

/// `text`'s inline Markdown as Pango markup: everything escaped, then `code`, **strong**,
/// *emphasis* or _emphasis_, and [links](url).
pub fn inline(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    let mut strong = false;
    let mut emphasis = false;
    let mut prev = None;
    while let Some(c) = rest.chars().next() {
        if c == '`'
            && let Some(end) = rest[1..].find('`')
        {
            out.push_str("<tt>");
            out.push_str(&glib::markup_escape_text(&rest[1..=end]));
            out.push_str("</tt>");
            rest = &rest[end + 2..];
            continue;
        }
        if rest.starts_with("**") && (strong || rest[2..].contains("**")) {
            out.push_str(if strong { "</b>" } else { "<b>" });
            strong = !strong;
            rest = &rest[2..];
            continue;
        }
        if (c == '*' || c == '_') && emphasis_mark(rest, prev, emphasis) {
            out.push_str(if emphasis { "</i>" } else { "<i>" });
            emphasis = !emphasis;
            rest = &rest[1..];
            continue;
        }
        if c == '['
            && let Some((label, url, after)) = link(rest)
        {
            out.push_str(&format!(
                "<a href=\"{}\">{}</a>",
                glib::markup_escape_text(url),
                glib::markup_escape_text(label)
            ));
            rest = after;
            continue;
        }
        out.push_str(&glib::markup_escape_text(&c.to_string()));
        rest = &rest[c.len_utf8()..];
        prev = Some(c);
    }
    // Close what the text left open, so the markup parses.
    if emphasis {
        out.push_str("</i>");
    }
    if strong {
        out.push_str("</b>");
    }
    out
}

/// Whether the `*` or `_` starting `rest`, after `prev`, opens emphasis (`open` false) or
/// closes it: an opening mark starts a word and is closed later; `snake_case` and `2 * 3`
/// are not emphasis.
fn emphasis_mark(rest: &str, prev: Option<char>, open: bool) -> bool {
    let mark = rest.as_bytes()[0] as char;
    let next = rest[1..].chars().next();
    if open {
        return next.is_none_or(|c| !c.is_alphanumeric());
    }
    prev.is_none_or(|c| !c.is_alphanumeric())
        && next.is_some_and(|c| !c.is_whitespace() && c != mark)
        && rest[1..].contains(mark)
}

/// A `[label](url)` at the start of `text`: the label, the url and what follows.
fn link(text: &str) -> Option<(&str, &str, &str)> {
    let close = text.find("](")?;
    let label = &text[1..close];
    let end = text[close + 2..].find(')')?;
    let url = &text[close + 2..close + 2 + end];
    (!label.contains('[') && !url.contains(' ')).then(|| (label, url, &text[close + 3 + end..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_markdown_becomes_escaped_markup() {
        assert_eq!(
            inline("I added `GET /health<T>`; it **returns** 200 & *fast*"),
            "I added <tt>GET /health&lt;T&gt;</tt>; it <b>returns</b> 200 &amp; <i>fast</i>"
        );
        assert_eq!(
            inline("see [the docs](https://x.dev/a)."),
            "see <a href=\"https://x.dev/a\">the docs</a>."
        );
        assert_eq!(
            inline("snake_case_name and 2 * 3"),
            "snake_case_name and 2 * 3"
        );
        assert_eq!(inline("**open"), "**open");
    }

    #[test]
    fn blocks_follow_the_text() {
        let text = "# Done\n\nI added it;\nit works.\n\n- one route\n- one `test`\n  carried on\n\n```rust\nfn main() {}\n```\n> quoted\n\n---";
        assert_eq!(
            blocks(text),
            [
                Block::Heading(1, "Done".into()),
                Block::Paragraph("I added it; it works.".into()),
                Block::Item("•".into(), 0, "one route".into()),
                Block::Item("•".into(), 0, "one <tt>test</tt> carried on".into()),
                Block::Code("fn main() {}".into()),
                Block::Quote("quoted".into()),
                Block::Rule,
            ]
        );
    }
}
