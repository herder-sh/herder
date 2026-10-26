//! Telegram bots: each its own bot, made with @BotFather, polling the Bot API for updates with
//! its token.
//!
//! A private chat with the bot is its direct message; a group it is in is a channel, where
//! people mention its `@username`. For it to see those mentions, privacy mode may stay on. A
//! reply chain, or a forum topic, is a thread ([`payload`]). Its buttons are inline keyboards
//! ([`render`]), its notes ephemeral messages, and a snippet a Markdown document.
//!
//! What Telegram cannot do: an answer has no Open in herder button, since Telegram opens only
//! `http`, `https` and `tg` links. Which thread a reply belongs to is remembered in memory, so
//! after the daemon starts again a reply to an older message starts a new thread, outside forum
//! topics; and buttons whose action is forgotten say so when pressed.

pub mod api;
pub mod html;
pub mod payload;
pub mod render;

use std::sync::Arc;
use std::time::Duration;

use frankenstein::ParseMode;
use frankenstein::methods::{
    AnswerCallbackQueryParams, DeleteMessageParams, DeleteWebhookParams,
    EditMessageReplyMarkupParams, EditMessageTextParams, GetFileParams, GetUpdatesParams,
    SendMessageParams,
};
use frankenstein::types::{
    AllowedUpdate, EphemeralMessageParameters, File, LinkPreviewOptions, Message as Sent,
    ReplyMarkup, ReplyParameters, User,
};
use frankenstein::updates::{Update, UpdateContent};
use futures_util::future::BoxFuture;
use herder_protocol::{MAX_PROMPT_ATTACHMENT_BYTES, SecretValue};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use self::api::{Api, Failure, Upload};
use self::payload::{Me, Pressed, Threads};
use self::render::Keys;
use super::bridge::{self, Bridge, ChatFile, ConnectError, Connection, Inbound, Place, Post};
use crate::session::publish::media_type;

/// Messages and presses the connection holds before the runner takes them.
const INBOUND: usize = 64;

/// How long a poll for updates waits for one, in seconds.
const POLL_SECONDS: u32 = 50;

/// The first wait before polling again after a poll failed; it doubles with each failure, up
/// to [`MAX_BACKOFF`].
const MIN_BACKOFF: Duration = Duration::from_secs(1);

/// The longest wait before polling again.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// The prefix of a forum topic's thread id.
const TOPIC: &str = "topic:";

/// What the poller and the bridge share.
#[derive(Default)]
struct Chat {
    threads: Threads,
    keys: Keys,
}

/// Connects a bot with its token: checks the token, and polls for its updates until closed.
pub async fn connect(token: &SecretValue) -> Result<Connection, ConnectError> {
    let api = Arc::new(Api::new(token.expose()).map_err(ConnectError::Failed)?);
    let user: User = api
        .call("getMe", &serde_json::json!({}))
        .await
        .map_err(refused)?;
    let me = Me {
        id: user.id,
        username: user.username.unwrap_or_default(),
    };
    // Updates come by polling, which Telegram refuses while the bot has a webhook.
    api.call::<bool>(
        "deleteWebhook",
        &DeleteWebhookParams::builder()
            .drop_pending_updates(false)
            .build(),
    )
    .await
    .map_err(refused)?;
    let chat = Arc::new(Chat::default());
    let (sender, inbound) = mpsc::channel(INBOUND);
    let stop = CancellationToken::new();
    let polling = tokio::spawn(poll(
        Arc::clone(&api),
        me,
        Arc::clone(&chat),
        sender,
        stop.clone(),
    ));
    Ok(Connection {
        bridge: Arc::new(Telegram { api, chat }),
        inbound,
        close: Box::pin(async move {
            stop.cancel();
            let _ = polling.await;
        }),
    })
}

/// `failure` as a connect error: a refusal of the token when Telegram says so.
fn refused(failure: Failure) -> ConnectError {
    match failure.code {
        Some(401 | 404) => {
            ConnectError::Rejected(format!("Telegram refused the bot token: {failure}"))
        }
        Some(_) => ConnectError::Failed(format!("Telegram refused: {failure}")),
        None => ConnectError::Failed(failure.description),
    }
}

/// Polls for the bot's updates, passing them on to `inbound`, until `stop`, until the runner
/// stops taking them, or until Telegram refuses the token.
async fn poll(
    api: Arc<Api>,
    me: Me,
    chat: Arc<Chat>,
    inbound: mpsc::Sender<Inbound>,
    stop: CancellationToken,
) {
    let mut offset = None;
    let mut backoff = MIN_BACKOFF;
    loop {
        let params = GetUpdatesParams::builder()
            .maybe_offset(offset)
            .timeout(POLL_SECONDS)
            .allowed_updates(vec![AllowedUpdate::Message, AllowedUpdate::CallbackQuery])
            .build();
        let polled = tokio::select! {
            () = stop.cancelled() => return,
            polled = api.call::<Vec<Update>>("getUpdates", &params) => polled,
        };
        let updates = match polled {
            Ok(updates) => updates,
            Err(failure) if matches!(failure.code, Some(401 | 404)) => {
                warn!("Telegram refused the bot token: {failure}");
                return;
            }
            Err(failure) => {
                warn!("cannot poll Telegram: {failure}");
                tokio::select! {
                    () = stop.cancelled() => return,
                    () = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
                continue;
            }
        };
        backoff = MIN_BACKOFF;
        for update in updates {
            offset = Some(i64::from(update.update_id) + 1);
            let received = match update.content {
                UpdateContent::Message(message) => payload::message(&message, &me, &chat.threads),
                UpdateContent::CallbackQuery(query) => {
                    let pressed = payload::press(&query, &chat.keys, &chat.threads);
                    answer(&api, &query.id, &pressed).await;
                    match pressed {
                        Pressed::Press(press) => Some(Inbound::Press(press)),
                        _ => None,
                    }
                }
                _ => None,
            };
            if let Some(received) = received
                && inbound.send(received).await.is_err()
            {
                return;
            }
        }
    }
}

/// Answers the press of a button, which Telegram shows as loading until then; ticks or unticks
/// a choice, or says the button no longer works.
async fn answer(api: &Api, query_id: &str, pressed: &Pressed) {
    let mut params = AnswerCallbackQueryParams::builder()
        .callback_query_id(query_id)
        .build();
    match pressed {
        Pressed::Press(_) => {}
        Pressed::Toggle {
            chat_id,
            message_id,
            markup,
        } => {
            let edit = EditMessageReplyMarkupParams::builder()
                .chat_id(*chat_id)
                .message_id(*message_id)
                .reply_markup(markup.clone())
                .build();
            if let Err(failure) = api
                .call::<serde_json::Value>("editMessageReplyMarkup", &edit)
                .await
            {
                warn!("cannot tick a choice: {failure}");
            }
        }
        Pressed::Expired => {
            params.text = Some("This button no longer works.".into());
        }
    }
    if let Err(failure) = api.call::<bool>("answerCallbackQuery", &params).await {
        warn!("cannot answer a button press: {failure}");
    }
}

/// A connected bot's Bot API.
struct Telegram {
    api: Arc<Api>,
    chat: Arc<Chat>,
}

/// Where in a chat a message goes.
struct Target {
    chat_id: i64,
    /// The forum topic, for a thread that is one.
    topic: Option<i32>,
    /// The message it replies to: the first of its thread, for one that is a reply chain.
    reply_to: Option<i32>,
}

impl Target {
    fn of(place: &Place) -> bridge::Result<Self> {
        let chat_id = place
            .channel_id
            .parse()
            .map_err(|_| format!("{} is not a Telegram chat", place.channel_id))?;
        let id = |text: &str| {
            text.parse::<i32>()
                .map_err(|_| format!("{} is not a Telegram thread", place.thread_id))
        };
        Ok(match place.thread_id.strip_prefix(TOPIC) {
            Some(topic) => Self {
                chat_id,
                topic: Some(id(topic)?),
                reply_to: None,
            },
            None => Self {
                chat_id,
                topic: None,
                reply_to: Some(id(&place.thread_id)?),
            },
        })
    }

    fn reply_parameters(&self) -> Option<ReplyParameters> {
        self.reply_to.map(|message_id| {
            ReplyParameters::builder()
                .message_id(message_id)
                .allow_sending_without_reply(true)
                .build()
        })
    }

    /// The form fields that place a file.
    fn fields(&self) -> Vec<(&'static str, String)> {
        let mut fields = vec![("chat_id", self.chat_id.to_string())];
        if let Some(topic) = self.topic {
            fields.push(("message_thread_id", topic.to_string()));
        }
        if let Some(reply) = self.reply_parameters() {
            fields.push((
                "reply_parameters",
                serde_json::to_string(&reply).unwrap_or_default(),
            ));
        }
        fields
    }
}

impl Telegram {
    /// Sends `text`, as HTML, to `place`, with `markup`, as an ephemeral message to `receiver`
    /// when there is one; resolves to the message's id.
    async fn send(
        &self,
        place: &Place,
        text: String,
        markup: Option<ReplyMarkup>,
        receiver: Option<u64>,
    ) -> bridge::Result<i32> {
        let target = Target::of(place)?;
        self.send_to(&target, Some(&place.thread_id), text, markup, receiver)
            .await
    }

    /// Sends `text` to `target`, in `thread`, or as the start of a thread when there is none.
    async fn send_to(
        &self,
        target: &Target,
        thread: Option<&str>,
        text: String,
        markup: Option<ReplyMarkup>,
        receiver: Option<u64>,
    ) -> bridge::Result<i32> {
        let params = SendMessageParams::builder()
            .chat_id(target.chat_id)
            .maybe_message_thread_id(target.topic)
            .maybe_reply_parameters(target.reply_parameters())
            .text(text)
            .parse_mode(ParseMode::Html)
            .link_preview_options(LinkPreviewOptions::builder().is_disabled(true).build())
            .maybe_reply_markup(markup)
            .maybe_ephemeral_message_parameters(receiver.map(|receiver_user_id| {
                EphemeralMessageParameters::builder()
                    .receiver_user_id(receiver_user_id)
                    .build()
            }))
            .build();
        let sent: Sent = self
            .api
            .call("sendMessage", &params)
            .await
            .map_err(failed)?;
        let own = sent.message_id.to_string();
        self.chat
            .threads
            .record(target.chat_id, sent.message_id, thread.unwrap_or(&own));
        Ok(sent.message_id)
    }

    /// Sends `messages` to `place`, `keyboard` on the last; resolves to that
    /// message's id.
    async fn send_all(
        &self,
        place: &Place,
        messages: impl IntoIterator<Item = String>,
        keyboard: Option<ReplyMarkup>,
    ) -> bridge::Result<Option<i32>> {
        let mut messages = messages.into_iter().peekable();
        let mut keyboard = keyboard;
        let mut id = None;
        while let Some(text) = messages.next() {
            let markup = if messages.peek().is_none() {
                keyboard.take()
            } else {
                None
            };
            id = Some(self.send(place, text, markup, None).await?);
        }
        Ok(id)
    }

    /// Sends the file `upload` to `place`, as `method`, with `caption`.
    async fn share(
        &self,
        place: &Place,
        method: &str,
        upload: Upload<'_>,
        caption: Option<&str>,
    ) -> bridge::Result<()> {
        let target = Target::of(place)?;
        let mut fields = target.fields();
        fields.extend(caption.map(|caption| ("caption", caption.to_owned())));
        let sent: Sent = self
            .api
            .upload(method, &fields, upload)
            .await
            .map_err(failed)?;
        self.chat
            .threads
            .record(target.chat_id, sent.message_id, &place.thread_id);
        Ok(())
    }
}

impl Bridge for Telegram {
    fn post<'a>(
        &'a self,
        place: &'a Place,
        post: &'a Post,
    ) -> BoxFuture<'a, bridge::Result<String>> {
        Box::pin(async move {
            let rendered = render::render(post, &self.chat.keys);
            let keyboard = rendered.keyboard.map(ReplyMarkup::InlineKeyboardMarkup);
            let id = self.send_all(place, rendered.messages, keyboard).await?;
            id.map(|id| id.to_string())
                .ok_or_else(|| "there was nothing to post".to_owned())
        })
    }

    /// A private chat's id is its person's user id. Telegram lets a bot write only to people
    /// who have written to it first.
    fn direct<'a>(
        &'a self,
        user_id: &'a str,
        post: &'a Post,
    ) -> BoxFuture<'a, bridge::Result<Place>> {
        Box::pin(async move {
            let target = Target {
                chat_id: user_id
                    .parse()
                    .map_err(|_| format!("{user_id} is not a Telegram user"))?,
                topic: None,
                reply_to: None,
            };
            let rendered = render::render(post, &self.chat.keys);
            let mut keyboard = rendered.keyboard.map(ReplyMarkup::InlineKeyboardMarkup);
            let mut messages = rendered.messages.into_iter();
            let first = messages.next().unwrap_or_default();
            let rest: Vec<String> = messages.collect();
            let markup = if rest.is_empty() {
                keyboard.take()
            } else {
                None
            };
            let id = self.send_to(&target, None, first, markup, None).await?;
            let place = Place {
                channel_id: user_id.to_owned(),
                thread_id: id.to_string(),
            };
            self.send_all(&place, rest, keyboard).await?;
            Ok(place)
        })
    }

    fn edit<'a>(
        &'a self,
        place: &'a Place,
        message_id: &'a str,
        post: &'a Post,
    ) -> BoxFuture<'a, bridge::Result<()>> {
        Box::pin(async move {
            let target = Target::of(place)?;
            let rendered = render::render(post, &self.chat.keys);
            let params = EditMessageTextParams::builder()
                .chat_id(target.chat_id)
                .message_id(message_id.parse::<i32>().map_err(|err| err.to_string())?)
                .text(rendered.messages.into_iter().next().unwrap_or_default())
                .parse_mode(ParseMode::Html)
                .link_preview_options(LinkPreviewOptions::builder().is_disabled(true).build())
                .maybe_reply_markup(rendered.keyboard)
                .build();
            match self
                .api
                .call::<serde_json::Value>("editMessageText", &params)
                .await
            {
                Ok(_) => Ok(()),
                // Telegram refuses an edit that changes nothing.
                Err(failure) if failure.description.contains("message is not modified") => Ok(()),
                Err(failure) => Err(failed(failure)),
            }
        })
    }

    fn delete<'a>(
        &'a self,
        place: &'a Place,
        message_id: &'a str,
    ) -> BoxFuture<'a, bridge::Result<()>> {
        Box::pin(async move {
            let target = Target::of(place)?;
            let params = DeleteMessageParams::builder()
                .chat_id(target.chat_id)
                .message_id(message_id.parse::<i32>().map_err(|err| err.to_string())?)
                .build();
            self.api
                .call::<bool>("deleteMessage", &params)
                .await
                .map_err(failed)?;
            Ok(())
        })
    }

    fn ephemeral<'a>(
        &'a self,
        place: &'a Place,
        user_id: &'a str,
        text: &'a str,
    ) -> BoxFuture<'a, bridge::Result<()>> {
        Box::pin(async move {
            // In a private chat only they see it anyway.
            let receiver = if place.channel_id == user_id {
                None
            } else {
                Some(
                    user_id
                        .parse()
                        .map_err(|_| format!("{user_id} is not a Telegram user"))?,
                )
            };
            self.send(place, html::html(text), None, receiver).await?;
            Ok(())
        })
    }

    fn snippet<'a>(
        &'a self,
        place: &'a Place,
        title: &'a str,
        text: &'a str,
    ) -> BoxFuture<'a, bridge::Result<()>> {
        let upload = Upload {
            field: "document",
            name: "answer.md",
            media_type: "text/markdown",
            data: text.as_bytes().to_vec(),
        };
        Box::pin(self.share(place, "sendDocument", upload, Some(title)))
    }

    fn upload<'a>(
        &'a self,
        place: &'a Place,
        name: &'a str,
        data: Vec<u8>,
    ) -> BoxFuture<'a, bridge::Result<()>> {
        let media_type = media_type(name);
        let (method, field) = sent_as(media_type);
        let upload = Upload {
            field,
            name,
            media_type,
            data,
        };
        Box::pin(self.share(place, method, upload, None))
    }

    fn download<'a>(&'a self, file: &'a ChatFile) -> BoxFuture<'a, bridge::Result<Vec<u8>>> {
        Box::pin(async move {
            let params = GetFileParams::builder().file_id(file.url.clone()).build();
            let found: File = self
                .api
                .call("getFile", &params)
                .await
                .map_err(|failure| format!("cannot download it from Telegram: {failure}"))?;
            let path = found
                .file_path
                .ok_or_else(|| "Telegram has no copy of it to download".to_owned())?;
            self.api
                .download(&path, MAX_PROMPT_ATTACHMENT_BYTES)
                .await
                .map_err(|err| format!("cannot download it from Telegram: {err}"))
        })
    }

    fn link<'a>(&'a self, place: &'a Place) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move { link(place) })
    }
}

/// A web link to `place`: only a supergroup's messages have one.
fn link(place: &Place) -> Option<String> {
    let chat = place.channel_id.strip_prefix("-100")?;
    let message = place
        .thread_id
        .strip_prefix(TOPIC)
        .unwrap_or(&place.thread_id);
    Some(format!("https://t.me/c/{chat}/{message}"))
}

/// The method and field Telegram takes a file of `media_type` with: a photo, which it shows
/// inline, for the images it accepts as one; a document for the rest.
fn sent_as(media_type: &str) -> (&'static str, &'static str) {
    if matches!(media_type, "image/png" | "image/jpeg") {
        ("sendPhoto", "photo")
    } else {
        ("sendDocument", "document")
    }
}

fn failed(failure: Failure) -> String {
    format!("Telegram refused: {failure}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_supergroups_threads_have_links() {
        let place = |channel_id: &str, thread_id: &str| Place {
            channel_id: channel_id.into(),
            thread_id: thread_id.into(),
        };
        assert_eq!(
            link(&place("-1002000000001", "10")),
            Some("https://t.me/c/2000000001/10".into())
        );
        assert_eq!(
            link(&place("-1002000000001", "topic:5")),
            Some("https://t.me/c/2000000001/5".into())
        );
        assert_eq!(link(&place("700000001", "10")), None);
    }

    #[test]
    fn photos_go_as_photos_and_other_files_as_documents() {
        assert_eq!(sent_as(media_type("shot.PNG")), ("sendPhoto", "photo"));
        assert_eq!(sent_as(media_type("shot.jpg")), ("sendPhoto", "photo"));
        for name in ["report.pdf", "page.html", "flow.mov", "Makefile"] {
            assert_eq!(sent_as(media_type(name)), ("sendDocument", "document"));
        }
    }
}
