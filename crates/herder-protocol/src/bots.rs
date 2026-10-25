//! Chat bots: Slack or Telegram bots a host's daemon holds, so allowlisted people drive
//! sessions from a chat.
//!
//! Each bot is its own app and bot user in its chat, configured on one host as a `[[bots]]`
//! entry of the daemon's config, which owners write with `put_bot` and `remove_bot`. The entry
//! holds no secret: the bot's tokens are machine-level secrets of the host's vault, which the
//! entry names and the daemon reads into memory only.
//!
//! People in the chat are no herder users: a bot's allowlist names them by their chat user id,
//! with what each may do. A chat thread is linked to one session ([`ThreadLink`]); a session
//! may be linked to several threads.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AccountId, BotId, PermissionMode, ProjectId, Provider, SecretRef, Timestamp};

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
