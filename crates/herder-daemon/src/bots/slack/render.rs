//! Each [`Post`] as the content of a Slack message: its `text`, which notifications show, and
//! its Block Kit `blocks`. A button carries its [`Action`] as JSON in its `value`.

use herder_protocol::ApprovalOutcome;
use serde_json::{Value, json};

use super::mrkdwn::mrkdwn;
use crate::bots::bridge::{Action, Post};

/// The most characters a section block's text holds.
const SECTION_CHARS: usize = 3000;

/// The most characters a button's label holds.
const LABEL_CHARS: usize = 75;

/// The most options a checkboxes element holds.
const CHECKBOXES: usize = 10;

/// The action id of a multi-select question's checkboxes, numbered from 0 when there are more
/// than [`CHECKBOXES`] choices.
pub const CHOICES: &str = "choices";

/// `post` as a message's `text` and `blocks`.
pub fn render(post: &Post) -> Value {
    let mut buttons = Buttons::default();
    let (text, blocks) = match post {
        Post::Working { session_id, status } => {
            let text = format!("_{status}_");
            let stop = buttons.action(
                "Stop",
                None,
                &Action::Stop {
                    session_id: session_id.clone(),
                },
            );
            let block = json!({
                "type": "section",
                "text": { "type": "mrkdwn", "text": text },
                "accessory": stop,
            });
            (text, vec![block])
        }
        Post::Text(markdown) => {
            let text = mrkdwn(markdown);
            let blocks = sections(&text);
            (text, blocks)
        }
        Post::Answer {
            session_id,
            text,
            more,
        } => {
            let text = mrkdwn(text);
            let mut blocks = sections(&text);
            if let Some((item_id, open)) = more {
                let full = Action::FullAnswer {
                    session_id: session_id.clone(),
                    item_id: item_id.clone(),
                };
                blocks.push(actions(vec![
                    buttons.action("Full answer", None, &full),
                    buttons.link("Open in herder", open),
                ]));
            }
            (text, blocks)
        }
        Post::Approval {
            session_id,
            approval_id,
            summary,
            resolved,
        } => {
            let text = format!("*Approval needed:* {}", mrkdwn(summary));
            let mut blocks = sections(&text);
            match resolved {
                None => {
                    let answer = |decision| Action::Approve {
                        session_id: session_id.clone(),
                        approval_id: approval_id.clone(),
                        decision,
                    };
                    blocks.push(actions(vec![
                        buttons.action(
                            "Allow",
                            Some("primary"),
                            &answer(herder_protocol::ApprovalDecision::Allow),
                        ),
                        buttons.action(
                            "Deny",
                            Some("danger"),
                            &answer(herder_protocol::ApprovalDecision::Deny),
                        ),
                    ]));
                }
                Some((ApprovalOutcome::Expired, _)) => blocks.push(context("Expired")),
                Some((outcome, by)) => {
                    let done = match outcome {
                        ApprovalOutcome::Allow => "Allowed",
                        _ => "Denied",
                    };
                    blocks.push(context(&format!("{done} by {by}")));
                }
            }
            (text, blocks)
        }
        Post::Question {
            session_id,
            question_id,
            text,
            choices,
            multi_select,
            answered,
        } => {
            let text = mrkdwn(text);
            let mut blocks = sections(&text);
            match answered {
                Some(answer) => blocks.push(context(&format!("Answered: {answer}"))),
                None if choices.is_empty() => {
                    blocks.push(context("Reply in this thread to answer."));
                }
                None if *multi_select => {
                    let mut elements: Vec<Value> = choices
                        .chunks(CHECKBOXES)
                        .enumerate()
                        .map(|(chunk, labels)| {
                            let options: Vec<Value> = labels
                                .iter()
                                .enumerate()
                                .map(|(index, label)| {
                                    json!({
                                        "text": { "type": "mrkdwn", "text": mrkdwn(label) },
                                        "value": (chunk * CHECKBOXES + index).to_string(),
                                    })
                                })
                                .collect();
                            json!({
                                "type": "checkboxes",
                                "action_id": format!("{CHOICES}-{chunk}"),
                                "options": options,
                            })
                        })
                        .collect();
                    let submit = Action::ChooseMany {
                        session_id: session_id.clone(),
                        question_id: question_id.clone(),
                        indices: Vec::new(),
                    };
                    elements.push(buttons.action("Submit", Some("primary"), &submit));
                    blocks.push(actions(elements));
                    blocks.push(context("Or reply in this thread with an answer."));
                }
                None => {
                    let elements = choices
                        .iter()
                        .enumerate()
                        .map(|(index, label)| {
                            let choose = Action::Choose {
                                session_id: session_id.clone(),
                                question_id: question_id.clone(),
                                index: index as u32,
                            };
                            buttons.action(label, None, &choose)
                        })
                        .collect();
                    blocks.push(actions(elements));
                    blocks.push(context("Or reply in this thread with an answer."));
                }
            }
            (text, blocks)
        }
        Post::Failure {
            session_id,
            text,
            retry,
            open,
        } => {
            let text = format!(":warning: {}", mrkdwn(text));
            let mut blocks = sections(&text);
            let mut elements = Vec::new();
            if *retry {
                let retry = Action::Retry {
                    session_id: session_id.clone(),
                };
                elements.push(buttons.action("Retry", None, &retry));
            }
            elements.push(buttons.link("Open in herder", open));
            blocks.push(actions(elements));
            (text, blocks)
        }
        Post::Picker { text, choices } => {
            let text = mrkdwn(text);
            let mut blocks = sections(&text);
            let elements = choices
                .iter()
                .map(|(label, action)| buttons.action(label, None, action))
                .collect();
            blocks.push(actions(elements));
            (text, blocks)
        }
        Post::Sessions { text, sessions } => {
            let text = mrkdwn(text);
            let mut blocks = sections(&text);
            for (session_id, line) in sessions {
                let next = Action::Continue {
                    session_id: session_id.clone(),
                };
                blocks.push(json!({
                    "type": "section",
                    "text": { "type": "mrkdwn", "text": truncate(&mrkdwn(line), SECTION_CHARS) },
                    "accessory": buttons.action("Continue here", None, &next),
                }));
            }
            (text, blocks)
        }
    };
    json!({ "text": text, "blocks": blocks })
}

/// Numbers a message's buttons, whose action ids must differ.
#[derive(Default)]
struct Buttons(usize);

impl Buttons {
    fn next_id(&mut self) -> String {
        self.0 += 1;
        format!("herder-{}", self.0)
    }

    /// A button that does `action`.
    fn action(&mut self, label: &str, style: Option<&str>, action: &Action) -> Value {
        let mut button = json!({
            "type": "button",
            "action_id": self.next_id(),
            "text": { "type": "plain_text", "text": label_of(label) },
            "value": serde_json::to_string(action).unwrap_or_default(),
        });
        if let Some(style) = style {
            button["style"] = json!(style);
        }
        button
    }

    /// A button that opens `url`.
    fn link(&mut self, label: &str, url: &str) -> Value {
        json!({
            "type": "button",
            "action_id": self.next_id(),
            "text": { "type": "plain_text", "text": label_of(label) },
            "url": url,
        })
    }
}

fn label_of(label: &str) -> String {
    truncate(label, LABEL_CHARS)
}

/// `text`, cut to at most `chars` characters with an ellipsis.
fn truncate(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(chars - 1).collect();
    short.push('…');
    short
}

fn actions(elements: Vec<Value>) -> Value {
    json!({ "type": "actions", "elements": elements })
}

fn context(text: &str) -> Value {
    json!({ "type": "context", "elements": [{ "type": "mrkdwn", "text": text }] })
}

/// `text` as section blocks of at most [`SECTION_CHARS`] characters each, split between lines
/// where it can be.
fn sections(text: &str) -> Vec<Value> {
    let mut chunks: Vec<String> = Vec::new();
    let mut chunk = String::new();
    for line in text.lines() {
        let mut line = line;
        loop {
            let room = SECTION_CHARS - chunk.chars().count() - usize::from(!chunk.is_empty());
            if line.chars().count() <= room {
                if !chunk.is_empty() {
                    chunk.push('\n');
                }
                chunk.push_str(line);
                break;
            }
            if chunk.is_empty() {
                let split = line
                    .char_indices()
                    .nth(SECTION_CHARS)
                    .map_or(line.len(), |(at, _)| at);
                chunks.push(line[..split].to_owned());
                line = &line[split..];
            } else {
                chunks.push(std::mem::take(&mut chunk));
            }
        }
    }
    if !chunk.trim().is_empty() {
        chunks.push(chunk);
    }
    chunks
        .into_iter()
        .map(|text| json!({ "type": "section", "text": { "type": "mrkdwn", "text": text } }))
        .collect()
}
