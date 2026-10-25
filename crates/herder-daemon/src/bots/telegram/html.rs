//! Markdown, as agents write it, turned into the HTML Telegram messages take.
//!
//! Telegram's HTML has bold, italics, strikethrough, code, preformatted blocks, quotes and
//! links, but no headings, lists or tables: headings become bold lines and list items bullets
//! or numbers, indented as nested. Links open only on `http`, `https` and `tg` urls, so others
//! show as text. Only `&`, `<` and `>` are escaped, and `"` in a link's url.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// `markdown` as Telegram HTML.
pub fn html(markdown: &str) -> String {
    let mut out = String::new();
    // Each open list's next number; `None` for a bulleted one.
    let mut lists: Vec<Option<u64>> = Vec::new();
    // Each open link's destination, and where its text starts in `out`.
    let mut links: Vec<(String, usize)> = Vec::new();
    // Where each open quote starts in `out`.
    let mut quotes: Vec<usize> = Vec::new();
    for event in Parser::new_ext(markdown, Options::ENABLE_STRIKETHROUGH) {
        match event {
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => end_block(&mut out, !lists.is_empty()),
            Event::Start(Tag::Heading { .. }) => {
                start_line(&mut out);
                out.push_str("<b>");
            }
            Event::End(TagEnd::Heading(_)) => {
                out.push_str("</b>");
                end_block(&mut out, false);
            }
            Event::Start(Tag::BlockQuote(_)) => {
                start_line(&mut out);
                quotes.push(out.len());
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                let start = quotes.pop().unwrap_or(out.len());
                let quoted = out.split_off(start);
                out.push_str("<blockquote>");
                out.push_str(quoted.trim_end());
                out.push_str("</blockquote>");
                end_block(&mut out, false);
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                start_line(&mut out);
                match kind {
                    CodeBlockKind::Fenced(language) if !language.is_empty() => {
                        out.push_str(&format!(
                            "<pre><code class=\"language-{}\">",
                            escape(&language).replace('"', "&quot;")
                        ));
                    }
                    _ => out.push_str("<pre><code>"),
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if out.ends_with('\n') {
                    out.pop();
                }
                out.push_str("</code></pre>");
                end_block(&mut out, !lists.is_empty());
            }
            Event::Start(Tag::List(first)) => {
                start_line(&mut out);
                lists.push(first);
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
                if lists.is_empty() {
                    end_block(&mut out, false);
                }
            }
            Event::Start(Tag::Item) => {
                start_line(&mut out);
                out.push_str(&"    ".repeat(lists.len().saturating_sub(1)));
                match lists.last_mut() {
                    Some(Some(number)) => {
                        out.push_str(&format!("{number}. "));
                        *number += 1;
                    }
                    _ => out.push_str("• "),
                }
            }
            Event::End(TagEnd::Item) => start_line(&mut out),
            Event::Start(Tag::Emphasis) => out.push_str("<i>"),
            Event::End(TagEnd::Emphasis) => out.push_str("</i>"),
            Event::Start(Tag::Strong) => out.push_str("<b>"),
            Event::End(TagEnd::Strong) => out.push_str("</b>"),
            Event::Start(Tag::Strikethrough) => out.push_str("<s>"),
            Event::End(TagEnd::Strikethrough) => out.push_str("</s>"),
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                links.push((dest_url.into_string(), out.len()));
            }
            Event::End(TagEnd::Link | TagEnd::Image) => {
                if let Some((url, start)) = links.pop() {
                    let text = out.split_off(start);
                    let shown = if text.is_empty() { escape(&url) } else { text };
                    if opens(&url) {
                        let href = escape(&url).replace('"', "&quot;");
                        out.push_str(&format!("<a href=\"{href}\">{shown}</a>"));
                    } else if shown == escape(&url) {
                        out.push_str(&shown);
                    } else {
                        out.push_str(&format!("{shown} ({})", escape(&url)));
                    }
                }
            }
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                out.push_str(&escape(&text));
            }
            Event::Code(code) => {
                out.push_str("<code>");
                out.push_str(&escape(&code));
                out.push_str("</code>");
            }
            Event::SoftBreak | Event::HardBreak => out.push('\n'),
            Event::Rule => {
                start_line(&mut out);
                out.push_str("───");
                end_block(&mut out, false);
            }
            Event::TaskListMarker(done) => out.push_str(if done { "☑ " } else { "☐ " }),
            _ => {}
        }
    }
    out.trim_end().to_owned()
}

/// Escapes what Telegram's HTML reads as markup.
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Whether Telegram opens a link to `url`.
fn opens(url: &str) -> bool {
    ["http://", "https://", "tg://"]
        .iter()
        .any(|scheme| url.starts_with(scheme))
}

/// Ends the line `out` is on, if it has started one.
fn start_line(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

/// Ends a block: with a blank line, or a line break only inside a list.
fn end_block(out: &mut String, in_list: bool) {
    start_line(out);
    if !in_list && !out.ends_with("\n\n") {
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_becomes_telegram_html() {
        let markdown = "# Done\n\n\
                        I fixed **the bug** in `main.rs`, _carefully_ and ~~quickly~~.\n\n\
                        - one\n- two\n  1. nested\n  2. again\n\n\
                        > a quote\n> of two lines\n\n\
                        ```rust\nlet a = 1 < 2 && true;\n```\n\n\
                        See [the PR](https://github.com/o/r/pull/1), <https://example.com> \
                        or [the session](herder://session?host=h).";
        assert_eq!(
            html(markdown),
            "<b>Done</b>\n\n\
             I fixed <b>the bug</b> in <code>main.rs</code>, <i>carefully</i> and \
             <s>quickly</s>.\n\n\
             • one\n• two\n    1. nested\n    2. again\n\n\
             <blockquote>a quote\nof two lines</blockquote>\n\n\
             <pre><code class=\"language-rust\">let a = 1 &lt; 2 &amp;&amp; true;</code></pre>\n\n\
             See <a href=\"https://github.com/o/r/pull/1\">the PR</a>, \
             <a href=\"https://example.com\">https://example.com</a> \
             or the session (herder://session?host=h)."
        );
    }

    #[test]
    fn plain_text_stays_as_it_is() {
        assert_eq!(
            html("Hello there.\nSecond line."),
            "Hello there.\nSecond line."
        );
    }
}
