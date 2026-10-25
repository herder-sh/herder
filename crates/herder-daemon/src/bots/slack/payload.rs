//! What Slack sends a bot over Socket Mode, as [`Inbound`]s.
//!
//! The bot subscribes to `app_mention`, for channels, and `message.im`, for direct messages.
//! A message of a channel arrives as an `app_mention` when it mentions the bot, so channel
//! messages that arrive as `message` events are dropped: a mention is never taken twice. Bots'
//! messages, the bot's own among them, edits, deletions and other subtypes are dropped too.

use slack_morphism::prelude::{
    SlackEventCallbackBody, SlackFile, SlackInteractionEvent, SlackMessageEventType,
    SlackPushEventCallback,
};

use super::render::CHOICES;
use crate::bots::bridge::{Action, ChatFile, Inbound, Message, Place, Press};

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
                    thread_id: message.origin.thread_ts.unwrap_or(message.origin.ts).0,
                },
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
                    thread_id: mention.origin.thread_ts.unwrap_or(mention.origin.ts).0,
                },
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
