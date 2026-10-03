//! The widgets of a session's transcript, as docs/tui-design.md §5.1 draws a chat:
//!
//! - The user's message: on a card, behind a bar in the accent.
//! - The agent's text: Markdown, unframed; a reply ends with a footer,
//!   `▣ account · model · 1m 12s`.
//! - Reasoning: one muted line, `+ Thought: <first line>`, that expands.
//! - A tool call: one dense line, `→ Read src/api.rs`, which grows into a block when it has
//!   output or a diff; the block shows its first lines and expands to all of them.
//! - Switches: a rule across the transcript. Failures: a card with the bar in `error`.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use adw::prelude::*;
use gtk::pango;
use herder_protocol::{
    CiStatus, ErrorClass, Item, ItemBody, ItemId, PrState, PullRequest, ReviewStatus, SessionId,
};
use serde_json::Value;

use crate::markdown::{self, Block};
use crate::session::{Entry, Session, ToolApproval, duration, first_line};
use crate::tools::{self, Diff, Kind};

/// What drawing an entry needs besides the entry.
pub struct Context<'a> {
    pub session: &'a Session,
    /// Items expanded by the user, kept across redraws.
    pub expanded: &'a Rc<RefCell<HashSet<ItemId>>>,
    /// Opens a child session, from its task line.
    pub open_child: &'a Rc<dyn Fn(SessionId)>,
}

/// What an entry's drawing depends on beyond the entry itself, which never changes: a
/// tool call's result and approval, a pull request's state. An entry is redrawn when this
/// changes.
pub fn signature(session: &Session, entry: &Entry) -> String {
    match entry {
        Entry::Item(Item {
            id,
            body: ItemBody::ToolCall { .. },
            ..
        }) => format!(
            "{:?} {:?}",
            session
                .result(id)
                .map(|(output, error)| (output.len(), error)),
            session.tool_approval(id)
        ),
        Entry::Pr(number) => format!("{:?}", session.prs.iter().find(|pr| pr.number == *number)),
        _ => String::new(),
    }
}

/// The widget of `entry`; `None` for a tool result drawn with its call.
pub fn entry(cx: &Context, entry: &Entry) -> Option<gtk::Widget> {
    let widget: gtk::Widget = match entry {
        Entry::Item(item) => return self::item(cx, item, false),
        Entry::Notice { text, attention } => {
            let label = line_label(text);
            label.add_css_class("notice");
            if *attention {
                label.add_css_class("attention");
            }
            label.upcast()
        }
        Entry::TurnEnded {
            took,
            interrupted,
            account,
            model,
        } => {
            let mut parts: Vec<String> = [account.clone(), model.clone()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect();
            parts.extend(took.map(duration));
            if *interrupted {
                parts.push("interrupted".to_owned());
            }
            let row = hbox(8);
            row.add_css_class("reply-footer");
            let mark = gtk::Label::new(Some("▣"));
            mark.add_css_class("reply-mark");
            row.append(&mark);
            let text = line_label(&parts.join(" · "));
            text.add_css_class("muted");
            row.append(&text);
            row.upcast()
        }
        Entry::TurnFailed { class, message } => {
            let card = vbox(4);
            card.add_css_class("failure");
            card.add_css_class("block");
            let title = line_label(&format!("✗ turn failed · {}", class_text(*class)));
            title.add_css_class("failure-title");
            card.append(&title);
            card.append(&wrapped(message, false));
            if let Some(hint) = class_hint(*class) {
                let hint = wrapped(hint, false);
                hint.add_css_class("muted");
                card.append(&hint);
            }
            card.upcast()
        }
        Entry::Switch(text) => {
            let row = hbox(12);
            row.add_css_class("switch-rule");
            row.add_css_class("block");
            let left = gtk::Separator::new(gtk::Orientation::Horizontal);
            let right = gtk::Separator::new(gtk::Orientation::Horizontal);
            for separator in [&left, &right] {
                separator.set_hexpand(true);
                separator.set_valign(gtk::Align::Center);
            }
            let label = gtk::Label::builder()
                .label(text)
                .wrap(true)
                .wrap_mode(pango::WrapMode::WordChar)
                .justify(gtk::Justification::Center)
                .max_width_chars(60)
                .build();
            row.append(&left);
            row.append(&label);
            row.append(&right);
            row.upcast()
        }
        Entry::Resolved { approval, text } => {
            let label = line_label(&format!("{} {text}", if *approval { "△" } else { "?" }));
            label.add_css_class("notice");
            label.upcast()
        }
        Entry::Child { session_id, task } => {
            let line = tool_line("◇", "Task", task, &[("open ›".to_owned(), "tool-args")]);
            let button = gtk::Button::builder()
                .child(&line)
                .css_classes(["flat", "tool-header"])
                .tooltip_text("Open the child session")
                .halign(gtk::Align::Start)
                .build();
            let open = Rc::clone(cx.open_child);
            let child = session_id.clone();
            button.connect_clicked(move |_| open(child.clone()));
            button.upcast()
        }
        Entry::Report { summary } => {
            let label = line_label(&format!("↳ report: {}", first_line(summary)));
            label.add_css_class("notice");
            label.set_margin_start(18);
            label.upcast()
        }
        Entry::Pr(number) => {
            let pr = cx.session.prs.iter().find(|pr| pr.number == *number)?;
            pr_line(pr).upcast()
        }
    };
    Some(widget)
}

/// The widget of `item`; `streaming` marks text still being written.
pub fn item(cx: &Context, item: &Item, streaming: bool) -> Option<gtk::Widget> {
    let widget: gtk::Widget = match &item.body {
        ItemBody::UserMessage { text } => user_message(text, None).upcast(),
        ItemBody::AssistantMessage { text } => assistant(text, streaming).upcast(),
        ItemBody::Reasoning { text } => thought(cx, &item.id, text, streaming),
        ItemBody::ToolCall { name, input } => tool(cx, &item.id, name, input),
        ItemBody::ToolResult {
            call_id,
            output,
            is_error,
        } => {
            // Drawn with its call, unless the call is not in the transcript.
            if cx.session.tool_call(call_id).is_some() {
                return None;
            }
            let line = tool_line("›", "", first_line(&tools::clean(output)), &[]);
            if *is_error {
                line.add_css_class("failed");
            }
            line.upcast()
        }
        ItemBody::Unknown => return None,
    };
    Some(widget)
}

/// A prompt on a card behind the accent bar; `badge` marks one not in the transcript yet.
pub fn user_message(text: &str, badge: Option<&str>) -> gtk::Box {
    let card = vbox(6);
    card.add_css_class("user-message");
    card.add_css_class("block");
    if let Some(badge) = badge {
        card.add_css_class("queued");
        let label = gtk::Label::builder()
            .label(badge)
            .halign(gtk::Align::Start)
            .css_classes(["badge"])
            .build();
        card.append(&label);
    }
    card.append(&wrapped(text, true));
    card
}

/// The agent's Markdown, unframed; while it streams, a cursor ends it.
pub fn assistant(text: &str, streaming: bool) -> gtk::Box {
    let column = vbox(9);
    column.add_css_class("assistant");
    column.add_css_class("block");
    let mut blocks = markdown::blocks(text);
    if streaming {
        let cursor = " ▌";
        match blocks.last_mut() {
            Some(Block::Paragraph(text) | Block::Item(_, _, text)) => text.push_str(cursor),
            _ => blocks.push(Block::Paragraph(cursor.to_owned())),
        }
    }
    for block in blocks {
        let widget: gtk::Widget = match block {
            Block::Paragraph(markup) => markup_label(&markup).upcast(),
            Block::Heading(level, markup) => {
                let label = markup_label(&markup);
                label.add_css_class(&format!("heading-{}", level.min(3)));
                label.upcast()
            }
            Block::Item(marker, depth, markup) => {
                let row = hbox(8);
                row.set_margin_start(i32::try_from(depth).unwrap_or(0).saturating_mul(18));
                let marker = gtk::Label::builder()
                    .label(marker)
                    .valign(gtk::Align::Start)
                    .css_classes(["reply-mark"])
                    .build();
                row.append(&marker);
                row.append(&markup_label(&markup));
                row.upcast()
            }
            Block::Quote(markup) => {
                let label = markup_label(&markup);
                label.add_css_class("quote");
                label.upcast()
            }
            Block::Code(code) => {
                let label = wrapped(&code, true);
                label.add_css_class("mono");
                label.add_css_class("code");
                label.upcast()
            }
            Block::Rule => gtk::Separator::new(gtk::Orientation::Horizontal).upcast(),
        };
        column.append(&widget);
    }
    column
}

/// Reasoning: one line that expands to all of it.
fn thought(cx: &Context, id: &ItemId, text: &str, streaming: bool) -> gtk::Widget {
    let first = first_line(text);
    let summary = if streaming {
        "Thinking…".to_owned()
    } else {
        format!("Thought: {first}")
    };
    let header = gtk::Label::builder()
        .label(format!("+ {summary}"))
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .css_classes(["thought"])
        .build();
    let body = wrapped(text, true);
    body.add_css_class("thought");
    body.set_margin_start(14);
    foldable(cx, id, header.upcast(), body.upcast(), "foldable")
}

/// A header button that shows or hides `body`, remembered per item.
fn foldable(
    cx: &Context,
    id: &ItemId,
    header: gtk::Widget,
    body: gtk::Widget,
    class: &str,
) -> gtk::Widget {
    let column = vbox(4);
    column.add_css_class(class);
    let expanded = cx.expanded.borrow().contains(id);
    let revealer = gtk::Revealer::builder()
        .child(&body)
        .reveal_child(expanded)
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .build();
    let button = gtk::Button::builder()
        .child(&header)
        .css_classes(["flat", "tool-header"])
        .halign(gtk::Align::Fill)
        .build();
    let set = Rc::clone(cx.expanded);
    let id = id.clone();
    let shown = revealer.clone();
    button.connect_clicked(move |_| {
        let open = !shown.reveals_child();
        shown.set_reveal_child(open);
        let mut set = set.borrow_mut();
        if open {
            set.insert(id.clone());
        } else {
            set.remove(&id);
        }
    });
    column.append(&button);
    column.append(&revealer);
    column.upcast()
}

/// A tool call, with its result once it came.
fn tool(cx: &Context, id: &ItemId, name: &str, input: &Value) -> gtk::Widget {
    let session = cx.session;
    let worktree = session.worktree.as_str();
    let result = session.result(id);
    let output = result.map(|(output, _)| tools::clean(output));
    let failed = result.is_some_and(|(_, error)| error);
    let approval = session.tool_approval(id);
    let (label, args) = tools::summary(name, input, output.as_deref(), worktree);
    let kind = tools::kind(name);
    // Running until its result comes: a `~` in place of the glyph.
    let glyph = if result.is_none() && approval.is_none() {
        "~"
    } else {
        tools::glyph(name)
    };
    let mut extra = Vec::new();
    let mut lines = Vec::new();
    if kind == Kind::Edit {
        lines = tools::diff(name, input);
        let added = lines.iter().filter(|l| matches!(l, Diff::Added(_))).count();
        let removed = lines
            .iter()
            .filter(|l| matches!(l, Diff::Removed(_)))
            .count();
        extra.push((format!("+{added}"), "added-count"));
        extra.push((format!("−{removed}"), "removed-count"));
    }
    let line = tool_line(glyph, &label, &args, &extra);
    match approval {
        Some(ToolApproval::Pending) => line.add_css_class("pending"),
        Some(ToolApproval::Denied) => {
            line.add_css_class("denied");
            strike(&line);
        }
        _ if failed => line.add_css_class("failed"),
        _ => {}
    }
    let output = output.unwrap_or_default();
    let has_output = !output.trim().is_empty();
    let expanded = cx.expanded.borrow().contains(id);
    match kind {
        _ if failed && has_output => block(
            cx,
            id,
            line,
            Body::Output(output, tools::OUTPUT_LINES),
            expanded,
            true,
        ),
        Kind::Shell if has_output => {
            let command = tools::in_worktree(&tools::command(input), worktree);
            let header = match tools::arg(input, "description") {
                Some(description) => tool_line("#", description, "", &[]),
                None => tool_line("$", first_line(&command), "", &[]),
            };
            block(
                cx,
                id,
                header,
                Body::Output(output, tools::OUTPUT_LINES),
                expanded,
                false,
            )
        }
        Kind::Edit if !lines.is_empty() => block(cx, id, line, Body::Diff(lines), expanded, false),
        Kind::Write => {
            let content = tools::clean(tools::arg(input, "content").unwrap_or_default());
            if content.trim().is_empty() {
                return line.upcast();
            }
            let body = output_label(&content);
            body.add_css_class("output");
            foldable(cx, id, line.upcast(), body.upcast(), "tool-block")
        }
        Kind::Todo => {
            let list = vbox(2);
            list.set_margin_start(24);
            for (text, status) in tools::todos(input) {
                let (mark, class) = match status.as_str() {
                    "completed" => ("✓", "state-done"),
                    "in_progress" => ("•", "state-running"),
                    _ => (" ", "muted"),
                };
                let row = hbox(6);
                let mark = gtk::Label::builder()
                    .label(format!("[{mark}]"))
                    .css_classes(["mono", class])
                    .build();
                row.append(&mark);
                row.append(&wrapped(&text, false));
                list.append(&row);
            }
            foldable(cx, id, line.upcast(), list.upcast(), "foldable")
        }
        Kind::Other if has_output => block(
            cx,
            id,
            line,
            Body::Output(output, tools::OTHER_LINES),
            expanded,
            false,
        ),
        _ => line.upcast(),
    }
}

/// What a tool block shows under its header, and how many lines of it while collapsed.
enum Body {
    Output(String, usize),
    Diff(Vec<Diff>),
}

/// A tool block: its header, then its output or diff, capped until expanded.
fn block(
    cx: &Context,
    id: &ItemId,
    header: gtk::Box,
    body: Body,
    expanded: bool,
    failed: bool,
) -> gtk::Widget {
    let card = vbox(2);
    card.add_css_class("tool-block");
    card.add_css_class("block");
    if failed {
        card.add_css_class("failed");
    }
    let button = gtk::Button::builder()
        .child(&header)
        .css_classes(["flat", "tool-header"])
        .build();
    card.append(&button);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.append(&content);
    let more = gtk::Button::builder()
        .css_classes(["flat", "more"])
        .halign(gtk::Align::Start)
        .build();
    card.append(&more);

    let body = Rc::new(body);
    let fill = {
        let content = content.clone();
        let more = more.clone();
        let body = Rc::clone(&body);
        move |expanded: bool| {
            while let Some(child) = content.first_child() {
                content.remove(&child);
            }
            let (shown, total) = match &*body {
                Body::Output(text, cap) => {
                    let total = text.lines().count();
                    let shown = if expanded { total } else { total.min(*cap) };
                    let text: Vec<&str> = text.lines().take(shown).collect();
                    let label = output_label(&text.join("\n"));
                    label.add_css_class("output");
                    content.append(&label);
                    (shown, total)
                }
                Body::Diff(lines) => {
                    let total = lines.len();
                    let shown = if expanded {
                        total
                    } else {
                        total.min(tools::DIFF_LINES)
                    };
                    content.append(&diff(&lines[..shown]));
                    (shown, total)
                }
            };
            let hidden = total - shown;
            more.set_visible(hidden > 0 || (expanded && collapsible(&body)));
            more.set_label(&if hidden > 0 {
                format!(
                    "… {hidden} more {}",
                    if hidden == 1 { "line" } else { "lines" }
                )
            } else {
                "Show less".to_owned()
            });
        }
    };
    fill(expanded);
    let fill = Rc::new(fill);
    let toggle = {
        let set = Rc::clone(cx.expanded);
        let id = id.clone();
        let fill = Rc::clone(&fill);
        move || {
            let mut set = set.borrow_mut();
            let open = !set.contains(&id);
            if open {
                set.insert(id.clone());
            } else {
                set.remove(&id);
            }
            drop(set);
            fill(open);
        }
    };
    let toggle = Rc::new(toggle);
    let on_header = Rc::clone(&toggle);
    button.connect_clicked(move |_| on_header());
    more.connect_clicked(move |_| toggle());
    card.upcast()
}

/// Whether a body is longer than its collapsed cap, so it can be collapsed again.
fn collapsible(body: &Body) -> bool {
    match body {
        Body::Output(text, cap) => text.lines().count() > *cap,
        Body::Diff(lines) => lines.len() > tools::DIFF_LINES,
    }
}

/// Diff lines: a sign column and the text, added and removed lines on their colours.
fn diff(lines: &[Diff]) -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.add_css_class("diff");
    for line in lines {
        let (sign, text, class) = match line {
            Diff::Added(text) => ("+", text.as_str(), Some("added")),
            Diff::Removed(text) => ("−", text.as_str(), Some("removed")),
            Diff::Context(text) => (" ", text.as_str(), None),
            Diff::Gap => {
                let gap = gtk::Label::builder()
                    .label("⋯")
                    .xalign(0.0)
                    .css_classes(["mono", "diff-gap"])
                    .build();
                column.append(&gap);
                continue;
            }
        };
        let row = hbox(0);
        row.add_css_class("diff-line");
        if let Some(class) = class {
            row.add_css_class(class);
        }
        let sign = gtk::Label::builder()
            .label(sign)
            .xalign(0.0)
            .valign(gtk::Align::Start)
            .css_classes(["mono", "diff-sign"])
            .build();
        row.append(&sign);
        let text = output_label(&tools::clean(text));
        row.append(&text);
        column.append(&row);
    }
    column
}

/// The one-line form of a tool call: glyph, label, arguments, and extra counts.
pub fn tool_line(glyph: &str, label: &str, args: &str, extra: &[(String, &str)]) -> gtk::Box {
    let row = hbox(8);
    row.add_css_class("tool-line");
    let glyph = gtk::Label::builder()
        .label(glyph)
        .xalign(0.5)
        .valign(gtk::Align::Start)
        .css_classes(["tool-glyph"])
        .build();
    row.append(&glyph);
    if !label.is_empty() {
        let label = gtk::Label::builder()
            .label(label)
            .valign(gtk::Align::Start)
            .css_classes(["tool-label"])
            .build();
        row.append(&label);
    }
    if !args.is_empty() {
        let args = gtk::Label::builder()
            .label(args)
            .xalign(0.0)
            .hexpand(extra.is_empty())
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .valign(gtk::Align::Start)
            .css_classes(["tool-args"])
            .build();
        row.append(&args);
    }
    for (text, class) in extra {
        let label = gtk::Label::builder()
            .label(text.as_str())
            .valign(gtk::Align::Start)
            .css_classes([*class])
            .build();
        row.append(&label);
    }
    row
}

/// Strikes through a denied call's label and arguments.
fn strike(line: &gtk::Box) {
    let mut child = line.first_child();
    while let Some(widget) = child {
        if let Some(label) = widget.downcast_ref::<gtk::Label>()
            && !label.has_css_class("tool-glyph")
        {
            let attrs = pango::AttrList::new();
            attrs.insert(pango::AttrInt::new_strikethrough(true));
            label.set_attributes(Some(&attrs));
        }
        child = widget.next_sibling();
    }
}

/// A linked pull request: number, state, checks and title.
fn pr_line(pr: &PullRequest) -> gtk::Box {
    let state = match pr.state {
        PrState::Open => "open",
        PrState::Draft => "draft",
        PrState::Merged => "merged",
        PrState::Closed => "closed",
    };
    let mut args = vec![state.to_owned()];
    if matches!(pr.state, PrState::Open | PrState::Draft) {
        args.push(
            match pr.ci {
                CiStatus::Passing => "ci ✓",
                CiStatus::Failing => "ci ✗",
                CiStatus::Pending => "ci …",
                CiStatus::None => "",
            }
            .to_owned(),
        );
        if pr.review == ReviewStatus::ChangesRequested {
            args.push("changes requested".to_owned());
        }
    }
    args.push(pr.title.clone());
    args.retain(|arg| !arg.is_empty());
    let line = tool_line("⎇", &format!("#{}", pr.number), &args.join(" · "), &[]);
    line.set_tooltip_text(Some(&pr.url));
    line
}

fn class_text(class: ErrorClass) -> &'static str {
    match class {
        ErrorClass::LimitReached => "limit reached",
        ErrorClass::Auth => "login needed",
        ErrorClass::Transient => "temporary failure",
        ErrorClass::Fatal => "failed",
    }
}

fn class_hint(class: ErrorClass) -> Option<&'static str> {
    match class {
        ErrorClass::LimitReached => {
            Some("Switch to another account, or wait for the limit to reset.")
        }
        ErrorClass::Auth => Some("Log the account in again on its machine."),
        ErrorClass::Transient => Some("Send the prompt again to retry."),
        ErrorClass::Fatal => None,
    }
}

pub fn hbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, spacing)
}

pub fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

/// A one-line label that wraps rather than truncates.
pub fn line_label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .build()
}

/// Plain wrapped text, selectable when `selectable`.
pub fn wrapped(text: &str, selectable: bool) -> gtk::Label {
    let label = line_label(text);
    label.set_selectable(selectable);
    // A selectable label would take the focus, and select all of itself on it.
    label.set_focusable(false);
    label
}

fn markup_label(markup: &str) -> gtk::Label {
    let label = wrapped("", true);
    label.set_markup(markup);
    label
}

/// Tool output or code: monospace, wrapped at any character so nothing overflows.
fn output_label(text: &str) -> gtk::Label {
    let label = wrapped(text, true);
    label.add_css_class("mono");
    label.set_hexpand(true);
    label
}

#[cfg(test)]
pub fn texts(widget: &gtk::Widget) -> Vec<String> {
    let mut out = Vec::new();
    let mut child = widget.first_child();
    while let Some(widget) = child {
        match widget.downcast_ref::<gtk::Label>() {
            Some(label) if label.get_visible() => {
                if !label.text().is_empty() {
                    out.push(label.text().into());
                }
            }
            _ if widget.get_visible() => {
                if let Some(revealer) = widget.downcast_ref::<gtk::Revealer>()
                    && !revealer.reveals_child()
                {
                    child = widget.next_sibling();
                    continue;
                }
                out.extend(texts(&widget));
            }
            _ => {}
        }
        child = widget.next_sibling();
    }
    out
}
