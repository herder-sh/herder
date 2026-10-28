//! Chat bots: Slack or Telegram bots a host's daemon holds, so allowlisted people drive
//! sessions from a chat.
//!
//! Each bot is its own app and bot user in its chat, configured on one host as a `[[bots]]`
//! entry of the daemon's config, which owners write with `put_bot` and `remove_bot`. The entry
//! holds no secret: the bot's tokens are machine-level secrets of the host's vault, which the
//! entry names and the daemon reads into memory only.
//!
//! People in the chat are no herder users: a bot's allowlist names them by their chat user id,
//! with what each may do, and may link each to a paired herder user whose role their prompts
//! carry. A chat thread is linked to one session ([`ThreadLink`]); a session
//! may be linked to several threads.
//!
//! What a bot knows comes in two parts: its owners' [`Bot::instructions`], kept in its entry,
//! which the bot never changes; and its [`BotMemory`], short facts it keeps itself, kept in the
//! daemon's store. Every session the bot starts is told both.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AccountId, BotId, FactId, PermissionMode, ProjectId, Provider, SecretRef, Timestamp, UserId,
};

/// The longest a bot's [`Bot::instructions`] may be, in bytes of UTF-8.
pub const MAX_BOT_INSTRUCTIONS_BYTES: usize = 8 * 1024;

/// The most facts a bot's memory holds.
pub const MAX_BOT_FACTS: usize = 40;

/// The most a bot's memory holds, in bytes of UTF-8 of its facts' text together.
pub const MAX_BOT_MEMORY_BYTES: usize = 4 * 1024;

/// The longest one fact may be, in characters.
pub const MAX_BOT_FACT_CHARS: usize = 200;

/// A bot, as its `[[bots]]` entry configures it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Bot {
    /// The bot; unique on its host: ASCII letters, digits, `-`, `_` and `.`.
    pub bot_id: BotId,
    /// The chat it is a bot of.
    pub kind: BotKind,
    /// Display name, as the chat shows the bot user.
    pub name: String,
    /// The machine-level secret holding the bot's token: Slack's bot token (`xoxb-`), or
    /// Telegram's bot token.
    pub token: SecretRef,
    /// The machine-level secret holding Slack's app-level token (`xapp-`), which Socket Mode
    /// connects with; required for Slack, refused for Telegram.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_token: Option<SecretRef>,
    /// The people the bot answers, each at most once; everyone else gets a note and nothing
    /// reaches a session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<BotUser>,
    /// What a session the bot starts runs on.
    #[serde(default)]
    pub defaults: BotDefaults,
    /// The project of new threads in each channel, each channel at most once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channel_bindings: Vec<ChannelBinding>,
    /// What its owners tell every session it starts, as Markdown, at most
    /// [`MAX_BOT_INSTRUCTIONS_BYTES`]; the bot never changes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

/// The chat a bot is a bot of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BotKind {
    /// A Slack app, connected over Socket Mode.
    Slack,
    /// A Telegram bot.
    Telegram,
    /// A kind newer than this build.
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Unknown,
}

/// A person a bot answers, and what they may do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BotUser {
    /// Their user id in the chat, such as Slack's `U012AB3CD`.
    pub external_user_id: String,
    /// Their name, as herder records their actions: "via ‹bot›, ‹name›".
    pub display_name: String,
    /// Whether they may drive sessions in direct messages with the bot.
    #[serde(default)]
    pub dm: bool,
    /// Whether they may drive sessions in channels, by mentioning the bot.
    #[serde(default)]
    pub channels: bool,
    /// Whether they may answer approval requests.
    #[serde(default)]
    pub approve: bool,
    /// Whether the bot messages them directly when a session on the host waits for them or
    /// fails.
    #[serde(default)]
    pub notify: bool,
    /// The paired herder user they are, which only owners set: an agent runs herder commands
    /// for a turn they prompted with this user's role. An unlinked person has no role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<UserId>,
}

/// What a session a bot starts runs on; an absent value is chosen as `create_session` chooses
/// it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BotDefaults {
    /// Project of a new thread its channel binds to none and its message names none; a chat
    /// when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    /// Provider sessions run on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<Provider>,
    /// Accounts sessions start on and rotate across, all of `provider` when it is set; every
    /// available account of the provider when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub account_ids: Vec<AccountId>,
    /// Model, in the provider's own naming.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Permission mode sessions start in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
}

/// The project new threads of a channel work on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChannelBinding {
    /// The channel's id in the chat, such as Slack's `C012AB3CD`.
    pub channel_id: String,
    /// The project.
    pub project_id: ProjectId,
}

/// How a bot's connection to its chat stands, answering `get_bot_status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BotStatus {
    /// The bot.
    pub bot_id: BotId,
    /// Where its connection stands.
    pub state: BotState,
    /// The latest error, kept after the bot recovers from it; absent when it had none since
    /// the daemon started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<BotError>,
}

/// Where a bot's connection to its chat stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BotState {
    /// Connecting, or connecting again after it dropped.
    Connecting,
    /// Connected: it answers in its chat.
    Connected,
    /// Its tokens cannot be read: the host's vault is not linked, unreachable, or does not
    /// hand them out. It retries.
    WaitingForVault,
    /// The chat refused its tokens; it waits for new ones.
    TokenRejected,
    /// This daemon cannot run bots of its kind.
    Unsupported,
    /// A state newer than this build.
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Unknown,
}

/// An error a bot hit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BotError {
    /// When it happened.
    pub at: Timestamp,
    /// Human-readable detail.
    pub message: String,
}

/// A chat thread linked to a session: the thread's messages reach the session, and the bot
/// posts the session's answers there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ThreadLink {
    /// The chat.
    pub kind: BotKind,
    /// The bot that holds the thread, on the session's host.
    pub bot_id: BotId,
    /// The channel or direct message the thread is in, as the chat names it.
    pub channel_id: String,
    /// The thread within the channel, as the chat names it: Slack's `thread_ts`.
    pub thread_id: String,
    /// Web link to the thread; absent when the chat gives none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// The bot that started a session, and who it started it for where. Set when the session is
/// created and never changed; a thread linked to a session later does not make it one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionOrigin {
    /// The bot, on the session's host.
    pub bot_id: BotId,
    /// The chat.
    pub kind: BotKind,
    /// Who wrote the message that started it, by their allowlist display name.
    pub by: String,
    /// Where they wrote it.
    pub place: BotPlace,
}

/// Where in a chat a message to a bot was written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BotPlace {
    /// A direct message with the bot.
    Direct,
    /// A channel or group, or a thread in one.
    Channel {
        /// The channel's name, as the chat showed it when the session started, such as
        /// `general`; its id when the chat gave no name.
        name: String,
    },
    /// A place newer than this build.
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Unknown,
}

/// The facts a bot keeps, answering `get_bot_memory` and the commands that change them: at
/// most [`MAX_BOT_FACTS`], of [`MAX_BOT_MEMORY_BYTES`] together.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BotMemory {
    /// The facts, oldest first; an edited fact keeps its place.
    pub facts: Vec<BotFact>,
}

/// One fact a bot remembers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BotFact {
    /// The fact, within its bot.
    pub id: FactId,
    /// The fact: one line, at most [`MAX_BOT_FACT_CHARS`].
    pub text: String,
    /// When it was learned, or last changed.
    pub learned_at: Timestamp,
    /// The thread it was learned in; absent for one an owner set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<ThreadLink>,
    /// Where in the chat it was learned, with the channel's name; absent for one an owner set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<BotPlace>,
    /// Who it was learned from: for a fact the bot learned in a chat, the person who started
    /// the conversation, by their allowlist display name; absent for one an owner set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}
