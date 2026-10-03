//! Light Markdown for the transcript, in the theme's `markdown*` tokens: headings, lists,
//! quotes, rules, fenced code on the panel background, and inline `code`, **strong**,
//! *emphasis* and [links](url). Text is wrapped here, at word boundaries, so the transcript
//! knows how many rows it takes.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::transcript::Row;
use crate::ui::{Ui, width};

/// A run of text in one style.
pub(super) type Run = (String, Style);

/// `text`'s Markdown as rows `width` wide, each starting `indent` columns in.
pub(super) fn markdown(ui: Ui, text: &str, indent: usize, width: usize) -> Vec<Row> {
    let theme = ui.theme;
    let base = Style::new().fg(theme.markdown_text);
    let pad = " ".repeat(indent);
    let room = width.saturating_sub(indent).max(1);
    let mut rows = Vec::new();
    let mut code = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let depth = line.len() - trimmed.len();
        if trimmed.starts_with("```") {
            code = !code;
            continue;
        }
        if code {
            // Code keeps its indentation, on the panel from the indent to the edge.
            let style = Style::new().fg(theme.markdown_code_block);
            let runs = vec![(line.to_owned(), style)];
            for line in wrap(&runs, room.saturating_sub(2).max(1), &[], &[]) {
                let mut spans = vec![Span::raw(format!("{pad}  "))];
                spans.extend(line.spans);
                let mut row = Row::new(Line::from(spans));
                row.fills.push((
                    u16::try_from(indent).unwrap_or(0),
                    0,
                    Style::new().bg(theme.background_panel),
                ));
                rows.push(row);
            }
            continue;
        }
        if trimmed.is_empty() {
            rows.push(Row::new(Line::raw("")));
            continue;
        }
        let (first, next, body, style): (Vec<Span>, Vec<Span>, &str, Style) =
            if let Some(heading) = heading(trimmed) {
                let style = Style::new()
                    .fg(theme.markdown_heading)
                    .add_modifier(Modifier::BOLD);
                (vec![], vec![], heading, style)
            } else if is_rule(trimmed) {
                let rule = "─".repeat(room);
                let style = Style::new().fg(theme.markdown_horizontal_rule);
                rows.push(Row::new(Line::from(vec![
                    Span::raw(pad.clone()),
                    Span::styled(rule, style),
                ])));
                continue;
            } else if let Some(rest) = ["- ", "* ", "+ "]
                .iter()
                .find_map(|mark| trimmed.strip_prefix(mark))
            {
                let lead = " ".repeat(depth);
                let bullet = Style::new().fg(theme.markdown_list_item);
                (
                    vec![
                        Span::raw(lead.clone()),
                        Span::styled(ui.glyphs.bullet, bullet),
                        Span::raw(" "),
                    ],
                    vec![Span::raw(format!(
                        "{lead}{} ",
                        " ".repeat(width_of(ui.glyphs.bullet))
                    ))],
                    rest,
                    base,
                )
            } else if let Some((number, rest)) = numbered(trimmed) {
                let lead = " ".repeat(depth);
                let mark = Style::new().fg(theme.markdown_list_enumeration);
                let hang = " ".repeat(width_of(number) + 1);
                (
                    vec![
                        Span::raw(lead.clone()),
                        Span::styled(number.to_owned(), mark),
                        Span::raw(" "),
                    ],
                    vec![Span::raw(format!("{lead}{hang}"))],
                    rest,
                    base,
                )
            } else if let Some(rest) = trimmed.strip_prefix('>') {
                let bar = Style::new().fg(theme.markdown_block_quote);
                let mark = vec![Span::styled(format!("{} ", ui.glyphs.pipe), bar)];
                let style = Style::new()
                    .fg(theme.markdown_block_quote)
                    .add_modifier(Modifier::ITALIC);
                (mark.clone(), mark, rest.trim_start(), style)
            } else {
                (vec![], vec![], line, base)
            };
        for line in wrap(&inline(ui, body, style), room, &first, &next) {
            let mut spans = vec![Span::raw(pad.clone())];
            spans.extend(line.spans);
            rows.push(Row::new(Line::from(spans)));
        }
    }
    // A reply's trailing blank lines take no rows.
    while rows
        .last()
        .is_some_and(|row| crate::ui::line_width(&row.line) == 0)
    {
        rows.pop();
    }
    rows
}

fn width_of(text: &str) -> usize {
    width(text)
}

/// The text of a `# heading`.
fn heading(line: &str) -> Option<&str> {
    let rest = line.trim_start_matches('#');
    let level = line.len() - rest.len();
    (1..=6).contains(&level).then(|| rest.strip_prefix(' '))?
}

/// Whether `line` is a rule: `---`, `***` or `___`.
fn is_rule(line: &str) -> bool {
    let line: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    line.len() >= 3 && ["-", "*", "_"].iter().any(|c| line == c.repeat(line.len()))
}

/// The `1.` and the rest of a numbered list item.
fn numbered(line: &str) -> Option<(&str, &str)> {
    let digits = line.find(|c: char| !c.is_ascii_digit())?;
    if digits == 0 || digits > 3 {
        return None;
    }
    let rest = line[digits..].strip_prefix(". ")?;
    Some((&line[..=digits], rest))
}

/// `text`'s inline Markdown as runs over `base`.
pub(super) fn inline(ui: Ui, text: &str, base: Style) -> Vec<Run> {
    let theme = ui.theme;
    let mut runs: Vec<Run> = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    let flush = |plain: &mut String, runs: &mut Vec<Run>| {
        if !plain.is_empty() {
            runs.push((std::mem::take(plain), base));
        }
    };
    while let Some(c) = rest.chars().next() {
        let styled = match c {
            '`' => rest[1..].find('`').map(|end| {
                let code = Style::new().fg(theme.markdown_code);
                (rest[1..=end].to_owned(), code, end + 2)
            }),
            '*' if rest.starts_with("**") => {
                rest[2..].find("**").filter(|&end| end > 0).map(|end| {
                    let strong = base.fg(theme.markdown_strong).add_modifier(Modifier::BOLD);
                    (rest[2..2 + end].to_owned(), strong, end + 4)
                })
            }
            '*' => rest[1..]
                .find('*')
                .filter(|&end| end > 0 && !rest[1..].starts_with(' '))
                .map(|end| {
                    let emph = base.fg(theme.markdown_emph).add_modifier(Modifier::ITALIC);
                    (rest[1..=end].to_owned(), emph, end + 2)
                }),
            '[' => link(rest).map(|(label, len)| {
                let style = Style::new()
                    .fg(theme.markdown_link_text)
                    .add_modifier(Modifier::UNDERLINED);
                (label.to_owned(), style, len)
            }),
            _ => None,
        };
        match styled {
            Some((text, style, len)) => {
                flush(&mut plain, &mut runs);
                runs.push((text, style));
                rest = &rest[len..];
            }
            None => {
                plain.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    flush(&mut plain, &mut runs);
    runs
}

/// A `[label](url)` at the start of `text`: the label, and the link's length.
fn link(text: &str) -> Option<(&str, usize)> {
    let close = text.find("](")?;
    let end = text[close..].find(')')? + close;
    let label = &text[1..close];
    (!label.is_empty() && !label.contains('[')).then_some((label, end + 1))
}

/// `runs` wrapped to `width` columns at word boundaries: the first line starts with `first`,
/// the rest with `next`. A word longer than a line is split.
pub(super) fn wrap(
    runs: &[Run],
    width: usize,
    first: &[Span<'static>],
    next: &[Span<'static>],
) -> Vec<Line<'static>> {
    let indent = |spans: &[Span<'static>]| spans.iter().map(|s| width_of(&s.content)).sum();
    let mut lines = Vec::new();
    let mut spans: Vec<Span<'static>> = first.to_vec();
    let mut used: usize = indent(first);
    let mut words = 0;
    // Spaces after the last word: written only if another word follows on the line.
    let mut space: Option<(String, Style)> = None;
    let push = |spans: &mut Vec<Span<'static>>, text: String, style: Style| match spans.last_mut() {
        Some(last) if last.style == style && words_of(last) => {
            last.content.to_mut().push_str(&text)
        }
        _ => spans.push(Span::styled(text, style)),
    };
    for (text, style) in runs {
        let mut rest = text.as_str();
        while !rest.is_empty() {
            let blank = rest.starts_with(char::is_whitespace);
            let end = rest
                .find(|c: char| c.is_whitespace() != blank)
                .unwrap_or(rest.len());
            let (token, after) = rest.split_at(end);
            rest = after;
            if blank {
                if words > 0 {
                    space = Some((token.to_owned(), *style));
                } else if lines.is_empty() && used + width_of(token) < width {
                    // Leading indentation, as code's, stays on the first line.
                    used += width_of(token);
                    push(&mut spans, token.to_owned(), *style);
                }
                continue;
            }
            let w = width_of(token);
            let gap = space.as_ref().map_or(0, |(s, _)| width_of(s));
            if words > 0 && used + gap + w > width {
                lines.push(Line::from(std::mem::take(&mut spans)));
                spans = next.to_vec();
                used = indent(next);
                words = 0;
                space = None;
            }
            if let Some((s, style)) = space.take() {
                used += width_of(&s);
                push(&mut spans, s, style);
            }
            if used + w <= width || words > 0 {
                used += w;
                push(&mut spans, token.to_owned(), *style);
            } else {
                // Longer than the room left on an empty line: split it.
                let mut part = String::new();
                for c in token.chars() {
                    let mut buf = [0; 4];
                    let cw = width_of(c.encode_utf8(&mut buf));
                    if used + cw > width && !part.is_empty() {
                        push(&mut spans, std::mem::take(&mut part), *style);
                        lines.push(Line::from(std::mem::take(&mut spans)));
                        spans = next.to_vec();
                        used = indent(next);
                    }
                    used += cw;
                    part.push(c);
                }
                push(&mut spans, part, *style);
            }
            words += 1;
        }
    }
    lines.push(Line::from(spans));
    lines
}

/// Whether `span` is text a word may join: not an indent or a mark.
fn words_of(span: &Span) -> bool {
    !span.content.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::glyphs::Glyphs;
    use crate::ui::theme::Theme;

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn wrapping_keeps_words_whole_and_hangs_indents() {
        let runs = vec![("one two three four".to_owned(), Style::new())];
        let lines = wrap(&runs, 9, &[Span::raw("- ")], &[Span::raw("  ")]);
        assert_eq!(text(&lines), ["- one two", "  three", "  four"]);
        let long = vec![("abcdefghij".to_owned(), Style::new())];
        assert_eq!(text(&wrap(&long, 4, &[], &[])), ["abcd", "efgh", "ij"]);
    }

    #[test]
    fn inline_markdown_takes_its_tokens() {
        let theme = Theme::herder(crate::ui::theme::Mode::Dark);
        let ui = Ui::new(&theme, Glyphs::Unicode);
        let runs = inline(ui, "a `b` **c** *d* [e](http://x) f_g", Style::new());
        let texts: Vec<&str> = runs.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(texts, ["a ", "b", " ", "c", " ", "d", " ", "e", " f_g"]);
        assert_eq!(runs[1].1.fg, Some(theme.markdown_code));
        assert!(runs[3].1.add_modifier.contains(Modifier::BOLD));
        assert!(runs[7].1.add_modifier.contains(Modifier::UNDERLINED));
        let rows = markdown(ui, "# Plan\n1. one\n- two\n```\ncode\n```\n\n", 2, 30);
        let lines: Vec<Line> = rows.into_iter().map(|row| row.line).collect();
        assert_eq!(text(&lines), ["  Plan", "  1. one", "  • two", "    code"]);
    }
}
