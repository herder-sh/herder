//! What a bot's runner ([`super::runner`]) needs of its chat, whichever chat it is: messages
//! and button presses coming in, posts, edits, notes, snippets and uploads going out.
//!
//! A chat's bridge turns its own payloads into [`Inbound`]s and renders each [`Post`] its own
//! way; the runner decides what to post, and never sees a chat's wire format.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use herder_protocol::{
    ApprovalDecision, ApprovalId, ApprovalOutcome, ItemId, ProjectId, QuestionId, SessionId,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// A thread of a chat: a channel or direct message, and the thread within it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Place {
    /// The channel or direct message, as the chat names it.
    pub channel_id: String,
    /// The thread, as the chat names it: Slack's `thread_ts`, which is the `ts` of the
    /// thread's first message.
    pub thread_id: String,
}

/// What arrives from the chat.
#[derive(Clone, Debug, PartialEq)]
pub enum Inbound {
    /// A person wrote a message the bot can see.
    Message(Message),
    /// A person pressed one of the bot's buttons.
    Press(Press),
}

/// A person's message. The bridge drops the bot's own messages and other bots'.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    /// Where it was written: the thread it replies in, or the thread it starts.
    pub place: Place,
    /// Its writer's user id in the chat.
    pub user_id: String,
    /// Whether it is a direct message with the bot.
    pub direct: bool,
    /// Whether it mentions the bot.
    pub mentioned: bool,
    /// Its text, as Markdown, without the bot's mention.
    pub text: String,
    /// The files it carries.
    pub files: Vec<ChatFile>,
}

/// A file a message carries, downloaded with [`Bridge::download`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatFile {
    /// Its name.
    pub name: String,
    /// Its media type, as the chat reports it.
    pub media_type: String,
    /// Where the bridge downloads it from.
    pub url: String,
}

/// A press of one of the bot's buttons.
#[derive(Clone, Debug, PartialEq)]
pub struct Press {
    /// The thread of the message the button is on.
    pub place: Place,
    /// Who pressed it, by their user id in the chat.
    pub user_id: String,
    /// Whether it was in a direct message with the bot.
    pub direct: bool,
    /// What the button does.
    pub action: Action,
}

/// What a button does; bridges carry it on the button, as the chat lets them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Action {
    /// Stop the session's running turn.
    Stop { session_id: SessionId },
    /// Answer an approval request.
    Approve {
        session_id: SessionId,
        approval_id: ApprovalId,
        decision: ApprovalDecision,
    },
    /// Answer a question with one of its choices.
    Choose {
        session_id: SessionId,
        question_id: QuestionId,
        index: u32,
    },
    /// Answer a multi-select question with the choices ticked, which the bridge reads off the
    /// message when the button is pressed.
    ChooseMany {
        session_id: SessionId,
        question_id: QuestionId,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        indices: Vec<u32>,
    },
    /// Post the rest of a trimmed answer.
    FullAnswer {
        session_id: SessionId,
        item_id: ItemId,
    },
    /// Send the session's latest prompt again.
    Retry { session_id: SessionId },
    /// Start the thread's session on a project the message named ambiguously.
    Pick { project_id: ProjectId },
    /// Link the thread to a session, which its prompts then continue.
    Continue { session_id: SessionId },
    /// Bind the channel to a project, which its new threads then work on.
    Use { project_id: ProjectId },
}

impl Action {
    /// The bot's session the button acts on; `None` for one that starts or links a session,
    /// or binds a channel.
    pub fn session_id(&self) -> Option<&SessionId> {
        match self {
            Self::Stop { session_id }
            | Self::Approve { session_id, .. }
            | Self::Choose { session_id, .. }
            | Self::ChooseMany { session_id, .. }
            | Self::FullAnswer { session_id, .. }
            | Self::Retry { session_id } => Some(session_id),
            Self::Pick { .. } | Self::Continue { .. } | Self::Use { .. } => None,
        }
    }
}

/// Something the bot posts, as the chat renders it. Text is Markdown, which the bridge turns
/// into the chat's own markup.
#[derive(Clone, Debug, PartialEq)]
pub enum Post {
    /// The one message of a running turn, edited as the session's status changes, with a
    /// Stop button.
    Working {
        session_id: SessionId,
        /// What the session is doing, such as "working…".
        status: String,
    },
    /// Plain text.
    Text(String),
    /// A turn's final answer.
    Answer {
        session_id: SessionId,
        /// The answer, or its start when it was trimmed.
        text: String,
        /// When trimmed: the answer's item, for Full answer, and where Open in herder goes.
        more: Option<(ItemId, String)>,
    },
    /// An approval request, with Allow and Deny until it is resolved.
    Approval {
        session_id: SessionId,
        approval_id: ApprovalId,
        summary: String,
        /// How it was resolved, and by whom; absent while it waits.
        resolved: Option<(ApprovalOutcome, String)>,
    },
    /// A question, with its choices as buttons, or checkboxes and Submit for a multi-select
    /// one, until it is answered.
    Question {
        session_id: SessionId,
        question_id: QuestionId,
        text: String,
        choices: Vec<String>,
        multi_select: bool,
        /// The answer, and who gave it; absent while it waits.
        answered: Option<String>,
    },
    /// A failed turn, with what fixes it.
    Failure {
        session_id: SessionId,
        text: String,
        /// Whether Retry would help.
        retry: bool,
        /// Where Open in herder goes.
        open: String,
    },
    /// A choice, as one button per choice: of projects, for a name that names several, or of
    /// sessions, for a title several match.
    Picker {
        text: String,
        /// Each button's label, and what it does.
        choices: Vec<(String, Action)>,
    },
    /// Sessions a thread may continue, each a line of Markdown with Continue here.
    Sessions {
        text: String,
        sessions: Vec<(SessionId, String)>,
    },
}

/// A bot connected to its chat.
pub struct Connection {
    /// Where it posts.
    pub bridge: Arc<dyn Bridge>,
    /// What arrives for it; closed when the connection ends for good.
    pub inbound: mpsc::Receiver<Inbound>,
    /// Disconnects it, once awaited.
    pub close: BoxFuture<'static, ()>,
}

/// Why a bot did not connect.
#[derive(Debug)]
pub enum ConnectError {
    /// The chat refused its tokens.
    Rejected(String),
    /// Anything else, such as the chat being unreachable; trying again may connect.
    Failed(String),
}

/// What a bridge's calls fail with: the chat's own error, as it says it.
pub type Result<T> = std::result::Result<T, String>;

/// A connection to a chat, as a bot's runner drives it.
pub trait Bridge: Send + Sync + 'static {
    /// Posts `post` in `place`; resolves to the posted message's id.
    fn post<'a>(&'a self, place: &'a Place, post: &'a Post) -> BoxFuture<'a, Result<String>>;
    /// Posts `post` as a new thread of the direct message with `user_id`; resolves to the
    /// thread, whose id is the posted message's too.
    fn direct<'a>(&'a self, user_id: &'a str, post: &'a Post) -> BoxFuture<'a, Result<Place>>;
    /// Replaces the message `message_id` of `place` with `post`.
    fn edit<'a>(
        &'a self,
        place: &'a Place,
        message_id: &'a str,
        post: &'a Post,
    ) -> BoxFuture<'a, Result<()>>;
    /// Deletes the bot's message `message_id` of `place`.
    fn delete<'a>(&'a self, place: &'a Place, message_id: &'a str) -> BoxFuture<'a, Result<()>>;
    /// Posts `text` in `place` that only `user_id` sees.
    fn ephemeral<'a>(
        &'a self,
        place: &'a Place,
        user_id: &'a str,
        text: &'a str,
    ) -> BoxFuture<'a, Result<()>>;
    /// Posts `text` in `place` as a snippet: a file shown as text, however long.
    fn snippet<'a>(
        &'a self,
        place: &'a Place,
        title: &'a str,
        text: &'a str,
    ) -> BoxFuture<'a, Result<()>>;
    /// Uploads the file `name` into `place`.
    fn upload<'a>(
        &'a self,
        place: &'a Place,
        name: &'a str,
        data: Vec<u8>,
    ) -> BoxFuture<'a, Result<()>>;
    /// The bytes of a file a message carried.
    fn download<'a>(&'a self, file: &'a ChatFile) -> BoxFuture<'a, Result<Vec<u8>>>;
    /// A web link to `place`, if the chat has one.
    fn link<'a>(&'a self, place: &'a Place) -> BoxFuture<'a, Option<String>>;
}
