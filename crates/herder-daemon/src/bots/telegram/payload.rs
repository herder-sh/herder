//! What Telegram sends a bot, as [`Inbound`]s.
//!
//! A private chat is the direct message and a group the channel. A forum topic is a thread,
//! named `topic:‹id›`; elsewhere a reply chain is, named by the id of its first message. A
//! reply carries only the message it replies to, so the bot remembers the thread of each
//! message it sees or posts ([`Threads`]). In a group, only messages that mention the bot's
//! `@username` are taken, as in a Slack channel. Bots' messages, the bot's own among them, and
//! bare commands such as the `/start` Telegram sends first, are dropped.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use frankenstein::types::{
    CallbackQuery, ChatType, MaybeInaccessibleMessage, Message as TelegramMessage,
    MessageEntityType,
};

use super::render::{Keys, TOGGLE, ticked, toggle};
use crate::bots::bridge::{Action, ChatFile, Inbound, Message, Place, Press};

/// The most messages [`Threads`] remembers the thread of; the oldest are forgotten first.
const MESSAGES: usize = 10_000;

/// The bot, as Telegram's `getMe` names it.
#[derive(Clone, Debug)]
pub struct Me {
    pub id: u64,
    /// Its `@username`, without the `@`.
    pub username: String,
}

/// The thread of each message the bot saw or posted, by chat and message id.
#[derive(Default)]
pub struct Threads(Mutex<Seen>);

#[derive(Default)]
struct Seen {
    threads: HashMap<(i64, i32), String>,
    order: VecDeque<(i64, i32)>,
}

impl Threads {
    /// Remembers that the message `message_id` of the chat `chat_id` is in `thread`.
    pub fn record(&self, chat_id: i64, message_id: i32, thread: &str) {
        let mut seen = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if seen
            .threads
            .insert((chat_id, message_id), thread.to_owned())
            .is_none()
        {
            seen.order.push_back((chat_id, message_id));
            if seen.order.len() > MESSAGES
                && let Some(oldest) = seen.order.pop_front()
            {
                seen.threads.remove(&oldest);
            }
        }
    }

    /// The thread of `message`, now remembered: its topic's, the thread of the message it
    /// replies to, or the one it starts.
    pub fn of(&self, message: &TelegramMessage) -> String {
        let chat_id = message.chat.id;
        if let Some(thread) = self.known(chat_id, message.message_id) {
            return thread;
        }
        let thread = match (message.is_topic_message, message.message_thread_id) {
            (Some(true), Some(topic)) => format!("topic:{topic}"),
            _ => match &message.reply_to_message {
                Some(replied) => self
                    .known(chat_id, replied.message_id)
                    .unwrap_or_else(|| replied.message_id.to_string()),
                None => message.message_id.to_string(),
            },
        };
        self.record(chat_id, message.message_id, &thread);
        thread
    }

    fn known(&self, chat_id: i64, message_id: i32) -> Option<String> {
        let seen = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        seen.threads.get(&(chat_id, message_id)).cloned()
    }
}

/// `message` as a message for the bot `me`; `None` for one it ignores.
pub fn message(message: &TelegramMessage, me: &Me, threads: &Threads) -> Option<Inbound> {
    let thread_id = threads.of(message);
    let from = message.from.as_ref()?;
    if from.is_bot {
        return None;
    }
    let text = message
        .text
        .as_deref()
        .or(message.caption.as_deref())
        .unwrap_or_default();
    let entities = message
        .entities
        .as_ref()
        .or(message.caption_entities.as_ref());
    let command = entities.is_some_and(|entities| {
        entities.iter().any(|entity| {
            entity.type_field == MessageEntityType::BotCommand
                && entity.offset == 0
                && usize::from(entity.length) == text.encode_utf16().count()
        })
    });
    let direct = message.chat.type_field == ChatType::Private;
    let handle = format!("@{}", me.username);
    let mentioned = find(text, &handle).is_some();
    if command || (!direct && !mentioned) {
        return None;
    }
    Some(Inbound::Message(Message {
        place: Place {
            channel_id: message.chat.id.to_string(),
            thread_id,
        },
        user_id: from.id.to_string(),
        direct,
        mentioned,
        text: without(text, &handle),
        files: files(message),
    }))
}

/// What a press of a button on one of the bot's messages does.
#[derive(Debug, PartialEq)]
pub enum Pressed {
    /// It acts, through the runner.
    Press(Press),
    /// It ticks or unticks a choice: the message's buttons become `markup`.
    Toggle {
        chat_id: i64,
        message_id: i32,
        markup: frankenstein::types::InlineKeyboardMarkup,
    },
    /// Its action is forgotten, or its message too old to read.
    Expired,
}

/// The press `query`, of a button whose action `keys` holds.
pub fn press(query: &CallbackQuery, keys: &Keys, threads: &Threads) -> Pressed {
    let (Some(MaybeInaccessibleMessage::Message(message)), Some(data)) =
        (&query.message, &query.data)
    else {
        return Pressed::Expired;
    };
    if data.starts_with(TOGGLE) {
        return match &message.reply_markup {
            Some(markup) => Pressed::Toggle {
                chat_id: message.chat.id,
                message_id: message.message_id,
                markup: toggle(markup, data),
            },
            None => Pressed::Expired,
        };
    }
    let Some(mut action) = keys.action(data) else {
        return Pressed::Expired;
    };
    if let Action::ChooseMany { indices, .. } = &mut action {
        *indices = message
            .reply_markup
            .as_ref()
            .map(|markup| ticked(markup))
            .unwrap_or_default();
    }
    Pressed::Press(Press {
        place: Place {
            channel_id: message.chat.id.to_string(),
            thread_id: threads.of(message),
        },
        user_id: query.from.id.to_string(),
        direct: message.chat.type_field == ChatType::Private,
        action,
    })
}

/// Where `handle` starts in `text`, ignoring case.
fn find(text: &str, handle: &str) -> Option<usize> {
    text.char_indices().map(|(at, _)| at).find(|&at| {
        text.get(at..at + handle.len())
            .is_some_and(|word| word.eq_ignore_ascii_case(handle))
    })
}

/// `text` without its mentions of `handle`.
fn without(text: &str, handle: &str) -> String {
    let mut rest = text;
    let mut out = String::new();
    while let Some(at) = find(rest, handle) {
        out.push_str(&rest[..at]);
        rest = &rest[at + handle.len()..];
    }
    out.push_str(rest);
    out.trim().to_owned()
}

/// The files `message` carries: its photo, at its largest, or its document. A [`ChatFile`]'s
/// url is Telegram's file id.
fn files(message: &TelegramMessage) -> Vec<ChatFile> {
    let mut files = Vec::new();
    if let Some(photo) = message.photo.as_ref().and_then(|sizes| sizes.last()) {
        files.push(ChatFile {
            name: "photo.jpg".into(),
            media_type: "image/jpeg".into(),
            url: photo.file_id.clone(),
        });
    }
    if let Some(document) = &message.document {
        files.push(ChatFile {
            name: document.file_name.clone().unwrap_or_else(|| "file".into()),
            media_type: document
                .mime_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".into()),
            url: document.file_id.clone(),
        });
    }
    files
}

#[cfg(test)]
mod tests {
    use frankenstein::updates::{Update, UpdateContent};

    use super::*;

    fn me() -> Me {
        Me {
            id: 7_000_000_001,
            username: "trash_can_bot".into(),
        }
    }

    fn update(json: serde_json::Value) -> TelegramMessage {
        let update: Update = serde_json::from_value(json).unwrap();
        match update.content {
            UpdateContent::Message(message) => *message,
            other => panic!("not a message: {other:?}"),
        }
    }

    fn group(id: i32, reply_to: Option<i32>, topic: Option<i32>, text: &str) -> TelegramMessage {
        let mut message = serde_json::json!({
            "message_id": id,
            "date": 1_761_400_000,
            "chat": { "id": -1_002_000_000_001_i64, "type": "supergroup", "title": "team" },
            "from": { "id": 700_000_001, "is_bot": false, "first_name": "Ada" },
            "text": text,
        });
        if let Some(reply_to) = reply_to {
            message["reply_to_message"] = serde_json::json!({
                "message_id": reply_to,
                "date": 1_761_400_000,
                "chat": { "id": -1_002_000_000_001_i64, "type": "supergroup", "title": "team" },
                "from": { "id": 7_000_000_001_u64, "is_bot": true, "first_name": "trash-can" },
                "text": "an answer",
            });
        }
        if let Some(topic) = topic {
            message["message_thread_id"] = topic.into();
            message["is_topic_message"] = true.into();
        }
        update(serde_json::json!({ "update_id": 1, "message": message }))
    }

    #[test]
    fn a_reply_to_the_bots_message_is_in_the_thread_the_bot_posted_it_in() {
        let threads = Threads::default();
        let start = group(10, None, None, "@Trash_Can_Bot fix it");
        let Some(Inbound::Message(started)) = message(&start, &me(), &threads) else {
            panic!("a mention is a message");
        };
        assert_eq!(started.place.thread_id, "10");
        assert_eq!(started.text, "fix it");
        assert!(started.mentioned && !started.direct);

        // The bot answered with message 11, in the thread; a reply to it continues the thread,
        // though Telegram says only that it replies to 11.
        threads.record(-1_002_000_000_001, 11, "10");
        let reply = group(12, Some(11), None, "and test it @trash_can_bot");
        let Some(Inbound::Message(replied)) = message(&reply, &me(), &threads) else {
            panic!("a mention is a message");
        };
        assert_eq!(replied.place.thread_id, "10");
        assert_eq!(replied.text, "and test it");

        // A reply that does not mention the bot is the people's own talk, but still in the
        // thread, so a mention replying to it is too.
        assert_eq!(
            message(&group(13, Some(12), None, "nice"), &me(), &threads),
            None
        );
        let Some(Inbound::Message(later)) = message(
            &group(14, Some(13), None, "@trash_can_bot ship it"),
            &me(),
            &threads,
        ) else {
            panic!("a mention is a message");
        };
        assert_eq!(later.place.thread_id, "10");
    }

    #[test]
    fn a_forum_topic_is_one_thread() {
        let threads = Threads::default();
        for (id, reply_to) in [(20, None), (21, Some(5))] {
            let Some(Inbound::Message(taken)) = message(
                &group(id, reply_to, Some(5), "@trash_can_bot hi"),
                &me(),
                &threads,
            ) else {
                panic!("a mention is a message");
            };
            assert_eq!(taken.place.thread_id, "topic:5");
        }
    }

    #[test]
    fn a_bare_command_is_dropped() {
        let start = update(serde_json::json!({
            "update_id": 1,
            "message": {
                "message_id": 1,
                "date": 1_761_400_000,
                "chat": { "id": 700_000_001, "type": "private", "first_name": "Ada" },
                "from": { "id": 700_000_001, "is_bot": false, "first_name": "Ada" },
                "text": "/start",
                "entities": [{ "type": "bot_command", "offset": 0, "length": 6 }],
            },
        }));
        assert_eq!(message(&start, &me(), &Threads::default()), None);
    }
}
