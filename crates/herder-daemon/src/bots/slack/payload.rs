//! What Slack sends a bot over Socket Mode, as [`Inbound`]s.
//!
//! The bot subscribes to `app_mention`, for channels, and `message.im`, for direct messages.
//! A message of a channel arrives as an `app_mention` when it mentions the bot, so channel
//! messages that arrive as `message` events are dropped: a mention is never taken twice. Bots'
//! messages, the bot's own among them, edits, deletions and other subtypes are dropped too.

use std::cmp::Ordering;
use std::collections::HashMap;

use slack_morphism::prelude::{
    SlackEventCallbackBody, SlackFile, SlackHistoryMessage, SlackInteractionEvent,
    SlackMessageEventType, SlackPushEventCallback, SlackUser,
};

use super::render::CHOICES;
use crate::bots::bridge::{
    Action, ChatFile, Earlier, Inbound, Message, Place, Press, RECENT_MESSAGES, RECENT_SECONDS,
};

/// The pushed event as a message for the bot whose user id is `me`; `None` for one it ignores.
pub fn push_event(event: SlackPushEventCallback, me: &str) -> Option<Inbound> {
    match event.event {
        SlackEventCallbackBody::Message(message) => {
            let direct = message
                .origin
                .channel_type
                .as_ref()
                .is_some_and(|kind| kind.0 == "im");
            let plain = matches!(
                message.subtype,
                None | Some(SlackMessageEventType::FileShare)
            );
            if !direct || !plain || message.sender.bot_id.is_some() {
                return None;
            }
            let user_id = message.sender.user?.0;
            if user_id == me {
                return None;
            }
            let channel_id = message.origin.channel?.0;
            let (text_of, files_of) = message
                .content
                .map_or((None, None), |content| (content.text, content.files));
            Some(Inbound::Message(Message {
                place: Place {
                    channel_id,
                    thread_id: message
                        .origin
                        .thread_ts
                        .unwrap_or_else(|| message.origin.ts.clone())
                        .0,
                },
                message_id: message.origin.ts.0,
                user_id,
                direct: true,
                mentioned: mentions(text_of.as_deref(), me),
                text: text(text_of.as_deref(), me),
                files: files(files_of),
            }))
        }
        SlackEventCallbackBody::AppMention(mention) => {
            if mention.user.0 == me {
                return None;
            }
            Some(Inbound::Message(Message {
                place: Place {
                    channel_id: mention.channel.0.clone(),
                    thread_id: mention
                        .origin
                        .thread_ts
                        .unwrap_or_else(|| mention.origin.ts.clone())
                        .0,
                },
                message_id: mention.origin.ts.0,
                user_id: mention.user.0,
                direct: mention.channel.0.starts_with('D'),
                mentioned: true,
                text: text(mention.content.text.as_deref(), me),
                files: files(mention.content.files),
            }))
        }
        _ => None,
    }
}

/// The press of one of the bot's buttons; `None` for anything else, such as ticking a
/// checkbox or opening a link.
pub fn interaction(event: SlackInteractionEvent) -> Option<Inbound> {
    let SlackInteractionEvent::BlockActions(event) = event else {
        return None;
    };
    let pressed = event.actions?.into_iter().next()?;
    let mut action: Action = serde_json::from_str(pressed.value.as_deref()?).ok()?;
    if let Action::ChooseMany { indices, .. } = &mut action {
        let ticked = event
            .state
            .iter()
            .flat_map(|state| state.values.values())
            .flat_map(|values| values.iter())
            .filter(|(action_id, _)| action_id.0.starts_with(CHOICES))
            .flat_map(|(_, value)| value.selected_options.iter().flatten())
            .filter_map(|option| option.value.parse::<u32>().ok());
        indices.extend(ticked);
        indices.sort_unstable();
    }
    let message = event.message?;
    let channel_id = event.channel?.id.0;
    Some(Inbound::Press(Press {
        place: Place {
            channel_id: channel_id.clone(),
            thread_id: message.origin.thread_ts.unwrap_or(message.origin.ts).0,
        },
        user_id: event.user?.id.0,
        direct: channel_id.starts_with('D'),
        action,
    }))
}

/// The messages `conversations.replies` answered that were written after `after`, if any,
/// and before `before`, as the bot whose user id is `me` reads them: people's and other bots'
/// messages, not its own, nor joins, edits or deletions.
pub fn earlier(
    messages: Vec<SlackHistoryMessage>,
    me: &str,
    after: Option<&str>,
    before: &str,
) -> Vec<Earlier> {
    messages
        .into_iter()
        .filter(|message| {
            let ts = &message.origin.ts.0;
            after.is_none_or(|after| order(ts, after) == Ordering::Greater)
                && order(ts, before) == Ordering::Less
        })
        .filter(|message| {
            matches!(
                message.subtype,
                None | Some(
                    SlackMessageEventType::FileShare
                        | SlackMessageEventType::ThreadBroadcast
                        | SlackMessageEventType::BotMessage
                        | SlackMessageEventType::MeMessage
                )
            )
        })
        .filter_map(|message| {
            let user_id = match (message.sender.user, message.sender.bot_id) {
                (Some(user), _) => user.0,
                (None, Some(bot)) => bot.0,
                (None, None) => return None,
            };
            if user_id == me {
                return None;
            }
            // A bot's message carries its name; a person's, only their user id.
            let name = message
                .sender
                .bot_profile
                .map(|bot| bot.name)
                .or(message.sender.username);
            Some(Earlier {
                message_id: message.origin.ts.0,
                user_id,
                name,
                text: text(message.content.text.as_deref(), me),
                files: files(message.content.files),
            })
        })
        .collect()
}

/// The messages `conversations.history` answered that were written before `before`, as
/// [`earlier`] reads them: at most [`RECENT_MESSAGES`], of the last [`RECENT_SECONDS`], oldest
/// first.
pub fn recent(mut messages: Vec<SlackHistoryMessage>, me: &str, before: &str) -> Vec<Earlier> {
    messages.sort_by(|a, b| order(&a.origin.ts.0, &b.origin.ts.0));
    let seconds = before
        .split_once('.')
        .map_or(before, |(seconds, _)| seconds);
    let since = seconds
        .parse::<u64>()
        .unwrap_or(0)
        .saturating_sub(RECENT_SECONDS);
    let mut recent = earlier(messages, me, Some(&format!("{since}.000000")), before);
    let older = recent.len().saturating_sub(RECENT_MESSAGES);
    recent.drain(..older);
    recent
}

/// The user ids of the people `messages` were written by, unnamed yet, or mention.
pub fn unnamed(messages: &[Earlier]) -> Vec<String> {
    let mut ids: Vec<String> = messages
        .iter()
        .filter(|message| message.name.is_none())
        .map(|message| message.user_id.clone())
        .chain(
            messages
                .iter()
                .flat_map(|message| mentioned(&message.text))
                .map(str::to_owned),
        )
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// `messages` with their writers named, and the people they mention, by `names`: user ids'
/// names.
pub fn named(messages: &mut [Earlier], names: &HashMap<String, String>) {
    for message in messages {
        if message.name.is_none() {
            message.name = names.get(&message.user_id).cloned();
        }
        message.text = name_mentions(&message.text, names);
    }
}

/// `text` with the people it mentions as `@‹name›`, those `names` names.
fn name_mentions(text: &str, names: &HashMap<String, String>) -> String {
    let mut named = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("<@") {
        named.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest.find('>') else {
            break;
        };
        let mention = &rest[..=end];
        let id = mention[2..end].split('|').next().unwrap_or_default();
        match names.get(id) {
            Some(name) => {
                named.push('@');
                named.push_str(name);
            }
            None => named.push_str(mention),
        }
        rest = &rest[end + 1..];
    }
    named.push_str(rest);
    named
}

/// The user ids `text` mentions, as Slack writes them: `<@U…>` or `<@U…|name>`.
fn mentioned(text: &str) -> Vec<&str> {
    text.split("<@")
        .skip(1)
        .filter_map(|rest| {
            let end = rest.find(['>', '|'])?;
            Some(&rest[..end])
        })
        .collect()
}

/// The name Slack shows for `user`: their display name, else their full name, else their
/// username.
pub fn display_name(user: SlackUser) -> String {
    let (display, full) = user.profile.map_or((None, None), |profile| {
        (profile.display_name, profile.real_name)
    });
    [display, full, user.real_name, user.name]
        .into_iter()
        .flatten()
        .find(|name| !name.trim().is_empty())
        .unwrap_or(user.id.0)
}

/// The order of the Slack timestamps `a` and `b`, such as `1761400400.000700`.
fn order(a: &str, b: &str) -> Ordering {
    let parts = |ts: &str| {
        let (seconds, micros) = ts.split_once('.').unwrap_or((ts, "0"));
        (seconds.parse::<u64>().ok(), micros.parse::<u64>().ok())
    };
    parts(a).cmp(&parts(b))
}

/// Whether `text` mentions the user `me`.
fn mentions(text: Option<&str>, me: &str) -> bool {
    text.is_some_and(|text| text.contains(&format!("<@{me}>")))
}

/// A message's text without its mentions of `me`, unescaped.
fn text(text: Option<&str>, me: &str) -> String {
    text.unwrap_or_default()
        .replace(&format!("<@{me}>"), "")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .trim()
        .to_owned()
}

fn files(files: Option<Vec<SlackFile>>) -> Vec<ChatFile> {
    files
        .unwrap_or_default()
        .into_iter()
        .filter_map(|file| {
            let url = file.url_private_download.or(file.url_private)?;
            Some(ChatFile {
                name: file.name.unwrap_or_else(|| file.id.0.clone()),
                media_type: file
                    .mimetype
                    .map_or_else(|| "application/octet-stream".into(), |kind| kind.0),
                url: url.to_string(),
            })
        })
        .collect()
}
