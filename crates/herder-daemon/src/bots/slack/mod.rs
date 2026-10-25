//! Slack bots: each its own Slack app, connected over Socket Mode with its app-level token
//! (`xapp-`), posting with its bot token (`xoxb-`).
//!
//! The app needs the bot scopes `app_mentions:read`, `im:history`, `chat:write`,
//! `files:read` and `files:write`, the events `app_mention` and `message.im`, and
//! interactivity, for its buttons.

pub mod mrkdwn;
pub mod payload;
pub mod render;

use std::sync::Arc;

use futures_util::future::BoxFuture;
use herder_protocol::{MAX_PROMPT_ATTACHMENT_BYTES, SecretValue};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use slack_morphism::errors::SlackClientError;
use slack_morphism::prelude::*;
use tokio::sync::mpsc;

use super::bridge::{self, Bridge, ChatFile, Inbound, Place, Post};

/// Messages and presses the connection holds before the runner takes them.
const INBOUND: usize = 64;

/// Why a bot did not connect.
#[derive(Debug)]
pub enum ConnectError {
    /// Slack refused its tokens.
    Rejected(String),
    /// Anything else, such as Slack being unreachable; trying again may connect.
    Failed(String),
}

/// A bot connected to Slack.
pub struct Connection {
    /// Where it posts.
    pub bridge: Arc<dyn Bridge>,
    /// What arrives for it.
    pub inbound: mpsc::Receiver<Inbound>,
    /// What disconnects it.
    pub closer: Closer,
}

/// Disconnects a bot from Slack.
pub struct Closer(SlackClientSocketModeListener<SlackClientHyperHttpsConnector>);

impl Closer {
    /// Disconnects the bot.
    pub async fn close(self) {
        self.0.shutdown().await;
    }
}

/// What the Socket Mode callbacks need, kept as the listener's user state.
struct Listening {
    inbound: mpsc::Sender<Inbound>,
    /// The bot's user id.
    me: String,
}

/// Connects a bot with its bot token and app-level token.
pub async fn connect(
    bot_token: &SecretValue,
    app_token: &SecretValue,
) -> Result<Connection, ConnectError> {
    let connector = SlackClientHyperConnector::new()
        .map_err(|err| ConnectError::Failed(format!("cannot set up TLS: {err}")))?;
    let client = Arc::new(SlackClient::new(connector));
    let token = SlackApiToken::new(bot_token.expose().to_owned().into());
    let me = client
        .open_session(&token)
        .auth_test()
        .await
        .map_err(|err| refused("the bot token", err))?
        .user_id
        .0;
    let (sender, inbound) = mpsc::channel(INBOUND);
    let environment = Arc::new(
        SlackClientEventsListenerEnvironment::new(Arc::clone(&client))
            .with_error_handler(on_error)
            .with_user_state(Listening {
                inbound: sender,
                me,
            }),
    );
    let callbacks = SlackSocketModeListenerCallbacks::new()
        .with_push_events(on_push)
        .with_interaction_events(on_interaction);
    let listener = SlackClientSocketModeListener::new(
        &SlackClientSocketModeConfig::new(),
        environment,
        callbacks,
    );
    let app_token = SlackApiToken::new(app_token.expose().to_owned().into());
    listener
        .listen_for(&app_token)
        .await
        .map_err(|err| refused("the app-level token", err))?;
    listener.start().await;
    Ok(Connection {
        bridge: Arc::new(Slack { client, token }),
        inbound,
        closer: Closer(listener),
    })
}

/// `err` as a connect error: a refusal of `token` when Slack says so.
fn refused(token: &str, err: SlackClientError) -> ConnectError {
    const REFUSALS: [&str; 6] = [
        "invalid_auth",
        "not_authed",
        "account_inactive",
        "token_revoked",
        "token_expired",
        "not_allowed_token_type",
    ];
    match &err {
        SlackClientError::ApiError(api) if REFUSALS.contains(&api.code.as_str()) => {
            ConnectError::Rejected(format!("Slack refused {token}: {}", api.code))
        }
        _ => ConnectError::Failed(format!("cannot reach Slack: {err}")),
    }
}

async fn on_push(
    event: SlackPushEventCallback,
    _client: Arc<SlackHyperClient>,
    states: SlackClientEventsUserState,
) -> UserCallbackResult<()> {
    let states = states.read().await;
    if let Some(listening) = states.get_user_state::<Listening>()
        && let Some(inbound) = payload::push_event(event, &listening.me)
    {
        // A closed channel means the runner stopped, and the listener with it.
        let _ = listening.inbound.send(inbound).await;
    }
    Ok(())
}

async fn on_interaction(
    event: SlackInteractionEvent,
    _client: Arc<SlackHyperClient>,
    states: SlackClientEventsUserState,
) -> UserCallbackResult<()> {
    let states = states.read().await;
    if let Some(listening) = states.get_user_state::<Listening>()
        && let Some(inbound) = payload::interaction(event)
    {
        let _ = listening.inbound.send(inbound).await;
    }
    Ok(())
}

fn on_error(
    err: Box<dyn std::error::Error + Send + Sync>,
    _client: Arc<SlackHyperClient>,
    _states: SlackClientEventsUserState,
) -> HttpStatusCode {
    tracing::warn!("a Slack event failed: {err}");
    HttpStatusCode::OK
}

/// A connected bot's Web API.
struct Slack {
    client: Arc<SlackHyperClient>,
    token: SlackApiToken,
}

impl Slack {
    fn session(&self) -> SlackClientSession<'_, SlackClientHyperHttpsConnector> {
        self.client.open_session(&self.token)
    }

    /// Uploads `data` as the file `name` into `place`, as a snippet with `snippet`.
    async fn share(
        &self,
        place: &Place,
        name: &str,
        title: Option<&str>,
        data: Vec<u8>,
        snippet: bool,
    ) -> bridge::Result<()> {
        let session = self.session();
        let mut ask = SlackApiFilesGetUploadUrlExternalRequest::new(name.to_owned(), data.len());
        if snippet {
            ask = ask.with_snippet_type("text".to_owned().into());
        }
        let upload = session
            .get_upload_url_external(&ask)
            .await
            .map_err(failed)?;
        let content_type = if snippet {
            "text/plain"
        } else {
            media_type(name)
        };
        session
            .files_upload_via_url(&SlackApiFilesUploadViaUrlRequest::new(
                upload.upload_url,
                data,
                content_type.to_owned(),
            ))
            .await
            .map_err(failed)?;
        let complete = request(json!({
            "files": [{ "id": upload.file_id, "title": title.unwrap_or(name) }],
            "channel_id": place.channel_id,
            "thread_ts": place.thread_id,
        }))?;
        session
            .files_complete_upload_external(&complete)
            .await
            .map_err(failed)?;
        Ok(())
    }
}

impl Bridge for Slack {
    fn post<'a>(
        &'a self,
        place: &'a Place,
        post: &'a Post,
    ) -> BoxFuture<'a, bridge::Result<String>> {
        Box::pin(async move {
            let mut message = render::render(post);
            message["channel"] = json!(place.channel_id);
            message["thread_ts"] = json!(place.thread_id);
            let posted = self
                .session()
                .chat_post_message(&request(message)?)
                .await
                .map_err(failed)?;
            Ok(posted.ts.0)
        })
    }

    fn edit<'a>(
        &'a self,
        place: &'a Place,
        message_id: &'a str,
        post: &'a Post,
    ) -> BoxFuture<'a, bridge::Result<()>> {
        Box::pin(async move {
            let mut message = render::render(post);
            message["channel"] = json!(place.channel_id);
            message["ts"] = json!(message_id);
            self.session()
                .chat_update(&request(message)?)
                .await
                .map_err(failed)?;
            Ok(())
        })
    }

    fn delete<'a>(
        &'a self,
        place: &'a Place,
        message_id: &'a str,
    ) -> BoxFuture<'a, bridge::Result<()>> {
        Box::pin(async move {
            let delete = SlackApiChatDeleteRequest::new(
                place.channel_id.clone().into(),
                message_id.to_owned().into(),
            );
            self.session().chat_delete(&delete).await.map_err(failed)?;
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
            let mut message = render::render(&Post::Text(text.to_owned()));
            message["channel"] = json!(place.channel_id);
            message["user"] = json!(user_id);
            message["thread_ts"] = json!(place.thread_id);
            self.session()
                .chat_post_ephemeral(&request(message)?)
                .await
                .map_err(failed)?;
            Ok(())
        })
    }

    fn snippet<'a>(
        &'a self,
        place: &'a Place,
        title: &'a str,
        text: &'a str,
    ) -> BoxFuture<'a, bridge::Result<()>> {
        Box::pin(self.share(
            place,
            "answer.md",
            Some(title),
            text.as_bytes().to_vec(),
            true,
        ))
    }

    fn upload<'a>(
        &'a self,
        place: &'a Place,
        name: &'a str,
        data: Vec<u8>,
    ) -> BoxFuture<'a, bridge::Result<()>> {
        Box::pin(self.share(place, name, None, data, false))
    }

    fn download<'a>(&'a self, file: &'a ChatFile) -> BoxFuture<'a, bridge::Result<Vec<u8>>> {
        let url = file.url.clone();
        let authorization = format!("Bearer {}", self.token.token_value.0);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let mut response = ureq::get(&url)
                    .header("Authorization", &authorization)
                    .call()
                    .map_err(|err| format!("cannot download it from Slack: {err}"))?;
                response
                    .body_mut()
                    .with_config()
                    .limit(MAX_PROMPT_ATTACHMENT_BYTES as u64)
                    .read_to_vec()
                    .map_err(|err| format!("cannot download it from Slack: {err}"))
            })
            .await
            .map_err(|err| err.to_string())?
        })
    }

    fn link<'a>(&'a self, place: &'a Place) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            let ask = SlackApiChatGetPermalinkRequest::new(
                place.channel_id.clone().into(),
                place.thread_id.clone().into(),
            );
            let link = self.session().chat_get_permalink(&ask).await.ok()?;
            Some(link.permalink.to_string())
        })
    }
}

/// The media type of the file `name`, by its extension.
fn media_type(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, extension)| extension) {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "application/octet-stream",
    }
}

/// A Web API request, from its JSON as Slack documents it.
fn request<T: DeserializeOwned>(value: Value) -> bridge::Result<T> {
    serde_json::from_value(value).map_err(|err| format!("cannot build the Slack request: {err}"))
}

fn failed(err: SlackClientError) -> String {
    format!("Slack refused: {err}")
}
