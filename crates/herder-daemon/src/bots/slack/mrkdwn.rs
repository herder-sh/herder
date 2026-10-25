//! Markdown, as agents write it, turned into Slack's mrkdwn.
//!
//! mrkdwn has bold (`*…*`), italics (`_…_`), strikethrough (`~…~`), code, code blocks, quotes
//! and links (`<url|text>`), but no headings, lists or tables: headings become bold lines and
//! list items bullets or numbers, indented as nested. Only `&`, `<` and `>` are escaped.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// `markdown` as mrkdwn.
pub fn mrkdwn(markdown: &str) -> String {
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
                out.push('*');
            }
            Event::End(TagEnd::Heading(_)) => {
                out.push('*');
                end_block(&mut out, false);
            }
            Event::Start(Tag::BlockQuote(_)) => {
                start_line(&mut out);
                quotes.push(out.len());
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                let start = quotes.pop().unwrap_or(out.len());
                let quoted = out.split_off(start);
                for line in quoted.trim_end().lines() {
                    out.push_str("> ");
                    out.push_str(line);
                    out.push('\n');
                }
                out.push('\n');
            }
            Event::Start(Tag::CodeBlock(_)) => {
                start_line(&mut out);
                out.push_str("```\n");
            }
            Event::End(TagEnd::CodeBlock) => {
                start_line(&mut out);
                out.push_str("```");
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
            Event::Start(Tag::Emphasis) | Event::End(TagEnd::Emphasis) => out.push('_'),
            Event::Start(Tag::Strong) | Event::End(TagEnd::Strong) => out.push('*'),
            Event::Start(Tag::Strikethrough) | Event::End(TagEnd::Strikethrough) => {
                out.push('~');
            }
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                links.push((dest_url.into_string(), out.len()));
            }
            Event::End(TagEnd::Link | TagEnd::Image) => {
                if let Some((url, start)) = links.pop() {
                    let text = out.split_off(start);
                    if text.is_empty() || text == escape(&url) {
                        out.push_str(&format!("<{url}>"));
                    } else {
                        out.push_str(&format!("<{url}|{text}>"));
                    }
                }
            }
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                out.push_str(&escape(&text));
            }
            Event::Code(code) => {
                out.push('`');
                out.push_str(&escape(&code));
                out.push('`');
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

/// Escapes what mrkdwn reads as markup.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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
    fn markdown_becomes_mrkdwn() {
        let markdown = "# Done\n\n\
                        I fixed **the bug** in `main.rs`, _carefully_ and ~~quickly~~.\n\n\
                        - one\n- two\n  1. nested\n  2. again\n\n\
                        > a quote\n> of two lines\n\n\
                        ```rust\nlet a = 1 < 2 && true;\n```\n\n\
                        See [the PR](https://github.com/o/r/pull/1) or <https://example.com>.";
        assert_eq!(
            mrkdwn(markdown),
            "*Done*\n\n\
             I fixed *the bug* in `main.rs`, _carefully_ and ~quickly~.\n\n\
             • one\n• two\n    1. nested\n    2. again\n\n\
             > a quote\n> of two lines\n\n\
             ```\nlet a = 1 &lt; 2 &amp;&amp; true;\n```\n\n\
             See <https://github.com/o/r/pull/1|the PR> or <https://example.com>."
        );
    }

    #[test]
    fn plain_text_stays_as_it_is() {
        assert_eq!(
            mrkdwn("Hello there.\nSecond line."),
            "Hello there.\nSecond line."
        );
    }
}
