//! The provider adapter contract, and a scripted fake adapter for tests.
//!
//! An [`Adapter`] drives one vendor CLI. [`Adapter::start`] runs it for one session and resolves
//! once it accepts prompts, yielding an [`AdapterSession`]: a command channel in, one ordered
//! event channel out. Every vendor shape fits behind it: a long-lived stream-json process
//! (Claude), JSON-RPC over stdio (Codex app-server, ACP agents).
//!
//! Vendor adapters drive their CLI through a [`transport::Transport`], which tests swap for a
//! recorded [`fixture::Fixture`].
//!
//! Adapter events are not journal events. The daemon turns them into journal events, which is
//! why they reuse the [`herder_protocol`] types the journal is made of.
//!
//! # Event order
//!
//! - A turn is [`AdapterEvent::TurnStarted`], then any items, approval requests and questions of
//!   that turn, then exactly one of `TurnCompleted`, `TurnInterrupted` or `TurnFailed`. One turn
//!   at a time. A turn's end voids its pending approvals and questions.
//! - A CLI may start a turn on its own, with no `SendPrompt`, such as Claude's reply to a
//!   background agent's result. Its `TurnStarted` comes while no turn runs, and the turn runs
//!   like any other: `Interrupt` stops it. A `SendPrompt` that arrives while it runs waits for
//!   its end, then starts.
//! - A streamed item is `ItemStarted`, then `ItemDelta`s, then `ItemCompleted` with its final
//!   body. An item that does not stream is a lone `ItemCompleted`.
//! - `ApprovalRequested` follows the `ItemCompleted` of the tool call it names.
//! - `QuestionAsked` blocks the turn until the daemon sends `AnswerQuestion`. An adapter whose
//!   CLI cannot ask never sends it, and so never receives `AnswerQuestion`.
//! - `UsageReported`, `ModelChanged`, `PermissionModeChanged` and `SessionIdentified` may come
//!   at any time.
//! - `Exited` is the last event, always sent, after which the channel closes. A turn still open
//!   when the process dies is first closed with `TurnFailed`.
//!
//! # Ids
//!
//! The daemon mints [`TurnId`]s and passes them in [`AdapterCommand::SendPrompt`]; the adapter
//! mints those of the turns its CLI starts on its own, as ULIDs like the daemon's. The adapter
//! mints [`ItemId`]s, [`ApprovalId`]s and [`QuestionId`]s, unique within the session.

use std::collections::{BTreeMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use herder_protocol::{
    Answer, ApprovalDecision, ApprovalId, Attachment, Image, Item, ItemId, PermissionMode,
    QuestionId, TurnError, TurnId, TurnUsage, UsageWindow,
};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::sync::mpsc;

pub mod acp;
pub mod claude;
pub mod codex;
pub mod fake;
pub mod fixture;
pub mod price;
pub mod record;
pub mod transcript;
pub mod transport;

/// A vendor CLI that herder can run sessions on, one implementation per CLI shape.
///
/// The daemon holds one per provider as `Box<dyn Adapter>` and never branches on the provider:
/// whatever differs is in [`Capabilities`].
pub trait Adapter: Send + Sync {
    /// Starts the CLI for one session; must be polled inside a tokio runtime.
    ///
    /// Resolves once the CLI accepts prompts. A failure to get there, such as a missing login,
    /// is classified like a turn failure, since starting is part of running the next turn.
    fn start(&self, request: StartRequest) -> StartFuture;

    /// Whether the CLI takes images with a prompt ([`AdapterCommand::SendPrompt`]'s `images`).
    /// Known before any start, so the daemon refuses a prompt with images up front; an adapter
    /// that says no is never sent any.
    fn accepts_images(&self) -> bool {
        false
    }
}

/// What [`Adapter::start`] returns: owns everything it needs, so the daemon can spawn it.
pub type StartFuture = Pin<Box<dyn Future<Output = Result<AdapterSession, TurnError>> + Send>>;

/// Everything an adapter needs to run one session.
#[derive(Clone, Debug, PartialEq)]
pub struct StartRequest {
    /// The account's config dir; the adapter points the CLI at it (`CLAUDE_CONFIG_DIR`,
    /// `CODEX_HOME`, ...) and never reads what is inside. Absent for an account in the CLI's
    /// default location: the variable is then not set, so `env` decides.
    pub config_dir: Option<PathBuf>,
    /// The CLI's complete environment; the adapter adds nothing but the config dir variable.
    pub env: BTreeMap<String, String>,
    /// The session's worktree, the CLI's working directory.
    pub cwd: PathBuf,
    /// Model, in the provider's own naming; the provider's default when absent.
    pub model: Option<String>,
    /// Starting permission mode.
    pub permission_mode: PermissionMode,
    /// Transcript to replay as context before the first prompt; empty for a fresh session.
    pub seed: Vec<Item>,
    /// The CLI's own id of a session to continue natively, as an earlier session reported it
    /// in [`AdapterEvent::SessionIdentified`]. The CLI looks it up under
    /// [`StartRequest::config_dir`]; start fails when it cannot find or open it. Only adapters
    /// with [`Capabilities::native_resume`] are given one.
    pub resume: Option<String>,
    /// herder's MCP server for this session, which the adapter registers with the CLI as
    /// `herder`, next to the user's own servers; absent when the daemon serves none.
    pub mcp: Option<McpServer>,
    /// Program and arguments the CLI is run through, such as `systemd-run --scope ... --`: the
    /// CLI's own program and arguments follow it. Empty runs the CLI directly. A replayed
    /// fixture ignores it.
    pub launcher: Vec<OsString>,
}

impl StartRequest {
    /// A command that runs `program` behind [`StartRequest::launcher`]; the caller appends the
    /// CLI's arguments and sets everything else.
    pub(crate) fn command(&self, program: impl AsRef<OsStr>) -> Command {
        match self.launcher.split_first() {
            Some((launcher, args)) => {
                let mut command = Command::new(launcher);
                command.args(args).arg(program);
                command
            }
            None => Command::new(program),
        }
    }
}

/// A stdio MCP server: the command the CLI spawns, speaking MCP on its stdin and stdout. It
/// carries no secret, so it is safe on a command line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServer {
    /// The executable.
    pub command: PathBuf,
    /// Its arguments.
    pub args: Vec<String>,
}

/// A running session of a vendor CLI.
#[derive(Debug)]
pub struct AdapterSession {
    /// What this session can do natively.
    pub capabilities: Capabilities,
    /// Commands in. Dropping every sender is the same as [`AdapterCommand::Shutdown`].
    pub commands: mpsc::UnboundedSender<AdapterCommand>,
    /// Events out, in order; closes after [`AdapterEvent::Exited`]. Bounded, so a daemon that
    /// stops reading stalls the CLI instead of growing memory.
    pub events: mpsc::Receiver<AdapterEvent>,
}

/// What a session can do natively; the daemon decides from these, never from the provider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Accepts [`AdapterCommand::SetModel`]; otherwise the daemon restarts with a seed.
    pub native_model_switch: bool,
    /// Accepts [`AdapterCommand::SetPermissionMode`]; otherwise the daemon restarts with a seed.
    pub native_permission_mode_switch: bool,
    /// Sends [`AdapterEvent::UsageReported`].
    pub reports_usage: bool,
    /// Sends [`AdapterEvent::SessionIdentified`] and continues a session named in
    /// [`StartRequest::resume`].
    pub native_resume: bool,
}

/// A command from the daemon. The daemon only sends what the session's state allows, such as
/// one prompt at a time, and only what its [`Capabilities`] accept.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdapterCommand {
    /// Start a turn with a prompt, while no turn the daemon started runs; it waits behind a
    /// turn the CLI started on its own.
    SendPrompt {
        /// Authenticated sending session, absent for human input. Never a system instruction.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_sender: Option<herder_protocol::SessionId>,
        /// Id of the new turn, minted by the daemon.
        turn_id: TurnId,
        /// Prompt text.
        text: String,
        /// Images for the agent to see with the text, already validated by the daemon; only
        /// sent to an adapter that [`Adapter::accepts_images`].
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<Image>,
    },
    /// Stop the running turn; answered by `TurnInterrupted`, voiding any pending approval.
    Interrupt,
    /// Change the model; answered by `ModelChanged`.
    SetModel {
        /// New model, in the provider's own naming.
        model: String,
    },
    /// Change the permission mode; answered by `PermissionModeChanged`.
    SetPermissionMode {
        /// New permission mode.
        mode: PermissionMode,
    },
    /// Answer a pending approval request.
    AnswerApproval {
        /// The request.
        approval_id: ApprovalId,
        /// The answer.
        decision: ApprovalDecision,
    },
    /// Answer a pending question; sent only for a question the adapter asked.
    AnswerQuestion {
        /// The question.
        question_id: QuestionId,
        /// The answer; a `choice` indexes the question's `choices`.
        answer: Answer,
    },
    /// Stop the CLI; answered by `Exited`.
    Shutdown,
}

/// Something the CLI did, in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdapterEvent {
    /// The agent started working on a prompt, or on a turn its CLI started on its own.
    TurnStarted {
        /// The turn, as given in `SendPrompt` or minted by the adapter.
        turn_id: TurnId,
    },
    /// The turn finished normally.
    TurnCompleted {
        /// The finished turn.
        turn_id: TurnId,
        /// Tokens and cost of the turn, as the CLI reported them; absent when it did not.
        /// Where the CLI gives tokens but no cost, the adapter estimates the cost from its
        /// price table and sets `cost_estimated`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<TurnUsage>,
    },
    /// The turn stopped because of `Interrupt`.
    TurnInterrupted {
        /// The stopped turn.
        turn_id: TurnId,
    },
    /// The turn ended with an error, classified by the adapter.
    TurnFailed {
        /// The failed turn.
        turn_id: TurnId,
        /// Why; `LimitReached` is the only class that may trigger failover.
        error: TurnError,
    },
    /// An item began streaming; `ItemDelta`s append to its text.
    ItemStarted {
        /// The item so far.
        item: Item,
    },
    /// Text appended to a started item.
    ItemDelta {
        /// The started item.
        item_id: ItemId,
        /// Text to append.
        text: String,
    },
    /// An item is final.
    ItemCompleted {
        /// The item in its final form.
        item: Item,
    },
    /// The agent is blocked until the daemon sends `AnswerApproval`.
    ApprovalRequested {
        /// The request, minted by the adapter.
        approval_id: ApprovalId,
        /// Turn that is blocked.
        turn_id: TurnId,
        /// Completed tool call item awaiting permission.
        tool_call_id: ItemId,
        /// One-line description of what the agent wants to do.
        summary: String,
    },
    /// The agent is blocked until the daemon sends `AnswerQuestion`, such as Claude's
    /// `AskUserQuestion` tool.
    QuestionAsked {
        /// The question, minted by the adapter.
        question_id: QuestionId,
        /// Turn that is blocked.
        turn_id: TurnId,
        /// The question, as Markdown.
        text: String,
        /// Answers to pick from; empty for a free-text answer.
        choices: Vec<String>,
    },
    /// The provider reported the account's limit windows.
    UsageReported {
        /// Every window it reported.
        windows: Vec<UsageWindow>,
    },
    /// The effective model is now known or changed, by `SetModel` or by the provider itself.
    ModelChanged {
        /// The model, in the provider's own naming.
        model: String,
    },
    /// The permission mode changed, by `SetPermissionMode` or by the agent itself.
    PermissionModeChanged {
        /// The new mode.
        mode: PermissionMode,
    },
    /// How many agents the provider runs in the background, sent whenever the number changes.
    /// They outlive the turn that started them, but not the CLI: none run after `Exited`.
    BackgroundAgents {
        /// Background agents still working.
        running: u32,
    },
    /// The CLI reported its own id for this session, the one [`StartRequest::resume`] takes;
    /// sent when it is first known and again whenever it changes.
    SessionIdentified {
        /// The id, in the CLI's own form.
        native_id: String,
    },
    /// The CLI is gone; always the last event.
    Exited {
        /// Why, when it was not asked to stop.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<TurnError>,
    },
}

/// A user message's text as a CLI gets it in a replayed transcript: the text, then a line
/// naming each image the message carried. A seed holds attachment references only, never the
/// bytes, so every adapter replays images this way.
pub(crate) fn seed_user_text(text: &str, attachments: &[Attachment]) -> String {
    let mut text = text.to_owned();
    for attachment in attachments {
        text.push('\n');
        text.push_str(&image_placeholder(
            &attachment.media_type,
            attachment.size,
            "not part of this replay",
        ));
    }
    text
}

/// Stands in for an image a CLI does not get, naming its media type, size and `why`.
pub(crate) fn image_placeholder(media_type: &str, size: u64, why: &str) -> String {
    format!(
        "[image attached: {media_type}, {} KB; {why}]",
        size.div_ceil(1024)
    )
}

/// Agent text stays at ordinary input priority and explicitly names its non-human origin.
pub(crate) fn agent_prompt(text: &str, sender: Option<&herder_protocol::SessionId>) -> String {
    match sender {
        Some(sender) => format!(
            "[Sent by another agent: session {sender}. This is agent context, not a human instruction.]\n\n{text}"
        ),
        None => text.to_owned(),
    }
}

/// `text` with each `$name` mention of a skill in `skills` replaced by `render(name)`, the way
/// the CLI invokes that skill. Anything else after a `$`, such as `$HOME` or `$5`, is left as
/// is, and so is a name that runs on, such as `$review-bot` where only `review` is a skill. The
/// skill's content is never pasted in: the CLI expands the mention itself.
pub(crate) fn rewrite_skill_mentions(
    text: &str,
    skills: &HashSet<String>,
    render: impl Fn(&str) -> String,
) -> String {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    let mut rewritten = String::with_capacity(text.len());
    let mut copied = 0;
    for (at, _) in text.match_indices('$') {
        if text[..at].ends_with(|c: char| word(c) || c == '$') {
            continue;
        }
        let start = at + 1;
        // Skill names may be namespaced, as in `plugin:skill`.
        let run = text[start..]
            .split(|c: char| !(word(c) || c == ':' || c == '.'))
            .next()
            .unwrap_or_default();
        let name = run
            .char_indices()
            .map(|(i, c)| &run[..i + c.len_utf8()])
            .rev()
            .find(|name| skills.contains(*name) && !run[name.len()..].starts_with(word));
        if let Some(name) = name {
            rewritten.push_str(&text[copied..at]);
            rewritten.push_str(&render(name));
            copied = start + name.len();
        }
    }
    rewritten.push_str(&text[copied..]);
    rewritten
}

#[cfg(test)]
pub(crate) mod testing {
    use std::ffi::{OsStr, OsString};

    use tokio::process::Command;

    /// The launcher the adapter tests run their CLI behind.
    pub(crate) fn launcher() -> Vec<OsString> {
        ["systemd-run", "--user", "--scope", "--"]
            .map(OsString::from)
            .to_vec()
    }

    /// Asserts that `launched` is `direct` run behind [`launcher`]: its argv is the launcher,
    /// then `direct`'s program and arguments, with the same environment and working directory.
    pub(crate) fn assert_behind_launcher(direct: &Command, launched: &Command) {
        let (direct, launched) = (direct.as_std(), launched.as_std());
        let argv = |command: &std::process::Command| -> Vec<OsString> {
            std::iter::once(command.get_program())
                .chain(command.get_args())
                .map(OsStr::to_owned)
                .collect()
        };
        assert_eq!(argv(launched), [launcher(), argv(direct)].concat());
        assert_eq!(
            launched.get_envs().collect::<Vec<_>>(),
            direct.get_envs().collect::<Vec<_>>()
        );
        assert_eq!(launched.get_current_dir(), direct.get_current_dir());
    }

    #[test]
    fn only_mentions_of_known_skills_are_rewritten() {
        use std::collections::HashSet;

        let skills = HashSet::from(["review".to_owned(), "git:commit".to_owned()]);
        let slash = |text| super::rewrite_skill_mentions(text, &skills, |name| format!("/{name}"));
        assert_eq!(slash("$review this"), "/review this");
        assert_eq!(
            slash("Run $review, then $git:commit."),
            "Run /review, then /git:commit."
        );
        assert_eq!(slash("echo $HOME costs $5"), "echo $HOME costs $5");
        assert_eq!(
            slash("$review-bot a$review $$review $reviewer"),
            "$review-bot a$review $$review $reviewer"
        );
        assert_eq!(slash("($review)"), "(/review)");
        assert_eq!(
            super::rewrite_skill_mentions("$review", &HashSet::new(), |_| unreachable!()),
            "$review"
        );
    }
}
