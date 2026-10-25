//! Each [`Post`] as Telegram messages: their HTML text, and the inline keyboard of the last.
//!
//! A button's callback data holds at most 64 bytes, too few for an [`Action`], so it holds the
//! action's key instead ([`Keys`]). Telegram has no checkboxes: a multi-select question's
//! choices are buttons that tick and untick themselves ([`toggle`]), and Submit reads the ticked
//! ones off the message ([`ticked`]).

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use frankenstein::types::{ButtonStyle, InlineKeyboardButton, InlineKeyboardMarkup};
use herder_protocol::{ApprovalDecision, ApprovalOutcome};
use sha2::{Digest, Sha256};

use super::html::{escape, html};
use crate::bots::bridge::{Action, Post};

/// The most characters of Markdown a message carries; Telegram takes 4096 once formatted.
const MESSAGE_CHARS: usize = 3500;

/// The most characters a button's label shows.
const LABEL_CHARS: usize = 60;

/// The callback data of a choice's tick, before the choice's index.
pub const TOGGLE: &str = "t:";

/// The callback data of an action, before its key.
const ACTION: &str = "a:";

const TICKED: &str = "☑ ";
const UNTICKED: &str = "☐ ";

/// The most actions [`Keys`] remembers; the oldest are forgotten first.
const KEYS: usize = 10_000;

/// The actions of the bot's buttons, by the key their callback data holds.
///
/// A key is a hash of its action, so a button keeps its key across edits and restarts, and an
/// old button whose action was forgotten is told apart from any other.
#[derive(Default)]
pub struct Keys(Mutex<Remembered>);

#[derive(Default)]
struct Remembered {
    actions: HashMap<String, Action>,
    order: VecDeque<String>,
}

impl Keys {
    /// The callback data of a button that does `action`.
    pub fn data(&self, action: &Action) -> String {
        let json = serde_json::to_string(action).unwrap_or_default();
        let key: String = Sha256::digest(json.as_bytes())
            .iter()
            .take(12)
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let mut remembered = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if !remembered.actions.contains_key(&key) {
            remembered.actions.insert(key.clone(), action.clone());
            remembered.order.push_back(key.clone());
            if remembered.order.len() > KEYS
                && let Some(oldest) = remembered.order.pop_front()
            {
                remembered.actions.remove(&oldest);
            }
        }
        format!("{ACTION}{key}")
    }

    /// The action of the button whose callback data is `data`; `None` for one not the bot's,
    /// or forgotten.
    pub fn action(&self, data: &str) -> Option<Action> {
        let key = data.strip_prefix(ACTION)?;
        let remembered = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        remembered.actions.get(key).cloned()
    }
}

/// A post as Telegram messages.
#[derive(Debug, PartialEq)]
pub struct Rendered {
    /// Each message's HTML text, in order; one, unless the post is long text.
    pub messages: Vec<String>,
    /// The last message's buttons.
    pub keyboard: Option<InlineKeyboardMarkup>,
}

/// `post` as messages, its buttons' actions remembered in `keys`.
pub fn render(post: &Post, keys: &Keys) -> Rendered {
    let button = |label: &str, action: &Action| {
        InlineKeyboardButton::builder()
            .text(label_of(label))
            .callback_data(keys.data(action))
            .build()
    };
    let (messages, rows) = match post {
        Post::Working { session_id, status } => {
            let stop = Action::Stop {
                session_id: session_id.clone(),
            };
            (
                vec![format!("<i>{}</i>", escape(status))],
                vec![vec![button("Stop", &stop)]],
            )
        }
        Post::Text(markdown) => (chunks(markdown), Vec::new()),
        Post::Answer {
            session_id,
            text,
            more,
        } => {
            let mut rows = Vec::new();
            if let Some((item_id, _)) = more {
                let full = Action::FullAnswer {
                    session_id: session_id.clone(),
                    item_id: item_id.clone(),
                };
                rows.push(vec![button("Full answer", &full)]);
            }
            (chunks(text), rows)
        }
        Post::Approval {
            session_id,
            approval_id,
            summary,
            resolved,
        } => {
            let mut text = format!("<b>Approval needed:</b> {}", html(&cut(summary)));
            let mut rows = Vec::new();
            match resolved {
                None => {
                    let answer = |decision| Action::Approve {
                        session_id: session_id.clone(),
                        approval_id: approval_id.clone(),
                        decision,
                    };
                    let mut allow = button("Allow", &answer(ApprovalDecision::Allow));
                    allow.style = Some(ButtonStyle::Success);
                    let mut deny = button("Deny", &answer(ApprovalDecision::Deny));
                    deny.style = Some(ButtonStyle::Danger);
                    rows.push(vec![allow, deny]);
                }
                Some((ApprovalOutcome::Expired, _)) => text.push_str("\n\n<i>Expired</i>"),
                Some((outcome, by)) => {
                    let done = match outcome {
                        ApprovalOutcome::Allow => "Allowed",
                        _ => "Denied",
                    };
                    text.push_str(&format!("\n\n<i>{done} by {}</i>", escape(by)));
                }
            }
            (vec![text], rows)
        }
        Post::Question {
            session_id,
            question_id,
            text,
            choices,
            multi_select,
            answered,
        } => {
            let mut text = html(&cut(text));
            let mut rows = Vec::new();
            match answered {
                Some(answer) => {
                    text.push_str(&format!("\n\n<i>Answered: {}</i>", escape(answer)));
                }
                None if choices.is_empty() => {
                    text.push_str("\n\n<i>Reply to this message to answer.</i>");
                }
                None if *multi_select => {
                    for (index, label) in choices.iter().enumerate() {
                        rows.push(vec![
                            InlineKeyboardButton::builder()
                                .text(format!("{UNTICKED}{}", label_of(label)))
                                .callback_data(format!("{TOGGLE}{index}"))
                                .build(),
                        ]);
                    }
                    let submit = Action::ChooseMany {
                        session_id: session_id.clone(),
                        question_id: question_id.clone(),
                        indices: Vec::new(),
                    };
                    let mut submit = button("Submit", &submit);
                    submit.style = Some(ButtonStyle::Primary);
                    rows.push(vec![submit]);
                    text.push_str("\n\n<i>Or reply to this message with an answer.</i>");
                }
                None => {
                    for (index, label) in choices.iter().enumerate() {
                        let choose = Action::Choose {
                            session_id: session_id.clone(),
                            question_id: question_id.clone(),
                            index: index as u32,
                        };
                        rows.push(vec![button(label, &choose)]);
                    }
                    text.push_str("\n\n<i>Or reply to this message with an answer.</i>");
                }
            }
            (vec![text], rows)
        }
        Post::Failure {
            session_id,
            text,
            retry,
            ..
        } => {
            let mut messages = chunks(text);
            if let Some(first) = messages.first_mut() {
                first.insert_str(0, "⚠️ ");
            }
            let mut rows = Vec::new();
            if *retry {
                let retry = Action::Retry {
                    session_id: session_id.clone(),
                };
                rows.push(vec![button("Retry", &retry)]);
            }
            (messages, rows)
        }
        Post::Picker { text, projects } => {
            let rows = projects
                .iter()
                .map(|(project_id, name)| {
                    let pick = Action::Pick {
                        project_id: project_id.clone(),
                    };
                    vec![button(name, &pick)]
                })
                .collect();
            (vec![html(&cut(text))], rows)
        }
    };
    Rendered {
        messages,
        keyboard: (!rows.is_empty()).then(|| {
            InlineKeyboardMarkup::builder()
                .inline_keyboard(rows)
                .build()
        }),
    }
}

/// `markup` with the choice whose tick has the callback data `data` ticked, or unticked.
pub fn toggle(markup: &InlineKeyboardMarkup, data: &str) -> InlineKeyboardMarkup {
    let mut markup = markup.clone();
    for button in markup.inline_keyboard.iter_mut().flatten() {
        if button.callback_data.as_deref() != Some(data) {
            continue;
        }
        button.text = match button.text.strip_prefix(TICKED) {
            Some(label) => format!("{UNTICKED}{label}"),
            None => format!(
                "{TICKED}{}",
                button.text.strip_prefix(UNTICKED).unwrap_or(&button.text)
            ),
        };
    }
    markup
}

/// The indices of the choices ticked on `markup`, in order.
pub fn ticked(markup: &InlineKeyboardMarkup) -> Vec<u32> {
    let mut indices: Vec<u32> = markup
        .inline_keyboard
        .iter()
        .flatten()
        .filter(|button| button.text.starts_with(TICKED))
        .filter_map(|button| button.callback_data.as_deref()?.strip_prefix(TOGGLE))
        .filter_map(|index| index.parse().ok())
        .collect();
    indices.sort_unstable();
    indices
}

fn label_of(label: &str) -> String {
    if label.chars().count() <= LABEL_CHARS {
        return label.to_owned();
    }
    let mut short: String = label.chars().take(LABEL_CHARS - 1).collect();
    short.push('…');
    short
}

/// `markdown` cut to one message's worth.
fn cut(markdown: &str) -> String {
    if markdown.chars().count() <= MESSAGE_CHARS {
        return markdown.to_owned();
    }
    let mut short: String = markdown.chars().take(MESSAGE_CHARS - 1).collect();
    short.push('…');
    short
}

/// `markdown` as the HTML of messages of at most [`MESSAGE_CHARS`] characters of Markdown
/// each, split between lines where it can be.
fn chunks(markdown: &str) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut chunk = String::new();
    for line in markdown.lines() {
        let mut line = line;
        loop {
            let room = MESSAGE_CHARS - chunk.chars().count() - usize::from(!chunk.is_empty());
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
                    .nth(MESSAGE_CHARS)
                    .map_or(line.len(), |(at, _)| at);
                chunks.push(line[..split].to_owned());
                line = &line[split..];
            } else {
                chunks.push(std::mem::take(&mut chunk));
            }
        }
    }
    if !chunk.trim().is_empty() || chunks.is_empty() {
        chunks.push(chunk);
    }
    chunks.iter().map(|chunk| html(chunk)).collect()
}
