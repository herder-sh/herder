//! Arguments and results of each task tool.
//!
//! Field docs become the `description`s in the tool schemas, so they are written for the agent
//! that calls the tool.

use herder_protocol::{
    Account, Answer, ApprovalDecision, ApprovalId, CommandBody, FactId, PermissionMode, Project,
    Provider, QuestionId, SecretRef, SessionHead, SessionId, SessionStatus, Timestamp, TurnId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Arguments of `spawn`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpawnInput {
    /// Short label for the child's work, shown to the user in the task tree, e.g. "Fix the flaky auth tests".
    pub task: String,
    /// Complete instructions for the child: the goal, the files and context it needs, constraints, and what to report back when done. The child sees nothing else of your conversation.
    pub prompt: String,
    /// Vendor CLI that runs the child, e.g. "claude" or "codex"; must be one of this task's providers. Defaults to yours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Provider")]
    pub provider: Option<Provider>,
    /// Model, in the provider's own naming. Defaults to yours on your provider, otherwise to that provider's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub model: Option<String>,
    /// What the child may do without asking; at most your own mode. Defaults to yours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "PermissionMode")]
    pub permission_mode: Option<PermissionMode>,
}

/// Result of `spawn`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SpawnOutput {
    /// The new child; pass it to send, status and wait_for.
    pub child: SessionId,
    /// Branch the child works on, in this repository; absent when this folder is not a git repository with a commit, so the child works in the folder itself, as you do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub branch: Option<String>,
}

/// Arguments of `send`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendInput {
    /// The child, as returned by spawn.
    pub child: SessionId,
    /// The message, as a prompt to the child.
    pub text: String,
}

/// Result of `send`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SendOutput {
    /// True when the message waits behind the child's running turn; false when the child started a turn on it.
    pub queued: bool,
}

/// Arguments of `status`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatusInput {
    /// Children to report on. Defaults to all of yours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Vec<SessionId>")]
    pub children: Option<Vec<SessionId>>,
}

/// Result of `status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StatusOutput {
    /// One entry per child, oldest first.
    pub children: Vec<ChildStatus>,
}

/// Where one child stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChildStatus {
    /// The child.
    pub child: SessionId,
    /// The child's task label.
    pub task: String,
    /// Branch the child works on, in this repository; absent when this folder is not a git repository with a commit, so the child works in the folder itself, as you do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub branch: Option<String>,
    /// `running` while it works or waits on you, `waiting_for_capacity` while its next turn waits for this machine to have room (it still counts as working), `idle` when its turn ended, `needs_you` when it waits on the user, `needs_login` while its account needs logging in again (what you send it waits until then), `error` when it cannot continue, `archived` when it was archived, by herder once all its pull requests merged or by hand (send unarchives it).
    pub status: SessionStatus,
    /// Summary of its latest finished turn; absent before the first one ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub last_report: Option<String>,
    /// Its questions and approval requests waiting for your answer.
    pub open_questions: Vec<Request>,
}

/// A child's question or approval request that waits for an answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    /// The child asked a question. Answer with `question_id` and `text`, or `choice` when it
    /// lists choices, or `choices` when it also takes several.
    Question {
        /// The question.
        question_id: QuestionId,
        /// The question, as Markdown.
        text: String,
        /// Answers to pick from by position; empty for a free-text answer.
        choices: Vec<String>,
        /// Whether several choices may be picked, with `choices`.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        multi_select: bool,
    },
    /// The child wants to run a tool call it may not run unasked. Answer with `approval_id` and `decision`.
    Approval {
        /// The request.
        approval_id: ApprovalId,
        /// What the child wants to do.
        summary: String,
    },
}

/// Arguments of `wait_for`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaitForInput {
    /// Wait for this child only. Omit to wait for any of yours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "SessionId")]
    pub child: Option<SessionId>,
    /// Longest wait, in seconds.
    #[schemars(range(min = 1, max = 600))]
    pub timeout_secs: u32,
}

/// Result of `wait_for`: what ended the wait.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WaitForOutput {
    /// A child finished a turn and is idle again, or stopped on an error.
    Report {
        /// The child.
        child: SessionId,
        /// The turn that ended.
        turn_id: TurnId,
        /// The child's final message of that turn, or what went wrong.
        summary: String,
        /// The child's status now: `archived` when herder archived it after this turn, as all
        /// its pull requests are merged.
        status: SessionStatus,
    },
    /// A child is blocked on a question or approval request waiting for your answer.
    Request {
        /// The child.
        child: SessionId,
        /// What it waits for.
        request: Request,
    },
    /// Nothing happened within `timeout_secs`; your children are still working.
    Timeout,
    /// No awaited child is working and nothing waits for you, so nothing would happen.
    Idle,
}

/// Arguments of `answer`: a question's answer or an approval's decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "AnswerArgs", into = "AnswerArgs")]
pub enum AnswerInput {
    /// Answers a question.
    Question {
        /// The child that asked.
        child: SessionId,
        /// The question.
        question_id: QuestionId,
        /// The answer.
        answer: Answer,
    },
    /// Decides an approval request.
    Approval {
        /// The child that asked.
        child: SessionId,
        /// The request.
        approval_id: ApprovalId,
        /// The decision.
        decision: ApprovalDecision,
    },
}

/// Wire shape of `answer`'s arguments. Flat because tool input schemas cannot be a `oneOf`
/// at the top level; [`AnswerInput`] enforces the combinations.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AnswerArgs {
    /// The child whose question or approval request this is, as wait_for or status named it.
    #[schemars(with = "SessionId")]
    pub child: Option<SessionId>,
    /// The question to answer. Pass it with `text`, `choice` or `choices`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "QuestionId")]
    pub question_id: Option<QuestionId>,
    /// The approval request to decide. Pass it with `decision`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "ApprovalId")]
    pub approval_id: Option<ApprovalId>,
    /// Free-text answer to the question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub text: Option<String>,
    /// Position of the chosen answer in the question's `choices`, from 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "u32")]
    pub choice: Option<u32>,
    /// Positions of the chosen answers in a multi-select question's `choices`, from 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Vec<u32>")]
    pub choices: Option<Vec<u32>>,
    /// `allow` runs the tool call once, `deny` refuses it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "ApprovalDecision")]
    pub decision: Option<ApprovalDecision>,
}

impl TryFrom<AnswerArgs> for AnswerInput {
    type Error = String;

    fn try_from(args: AnswerArgs) -> Result<Self, String> {
        let Some(child) = args.child.clone() else {
            return Err("pass `child`, the child that asked".into());
        };
        match args {
            AnswerArgs {
                question_id: Some(question_id),
                approval_id: None,
                text,
                choice,
                choices,
                decision: None,
                ..
            } => {
                let answer = match (text, choice, choices) {
                    (Some(text), None, None) => Answer::Text { text },
                    (None, Some(index), None) => Answer::Choice { index },
                    (None, None, Some(indices)) => Answer::Choices { indices },
                    _ => {
                        return Err(
                            "a question takes exactly one of `text`, `choice` or `choices`".into(),
                        );
                    }
                };
                Ok(AnswerInput::Question {
                    child,
                    question_id,
                    answer,
                })
            }
            AnswerArgs {
                question_id: None,
                approval_id: Some(approval_id),
                text: None,
                choice: None,
                choices: None,
                decision: Some(decision),
                ..
            } => Ok(AnswerInput::Approval {
                child,
                approval_id,
                decision,
            }),
            AnswerArgs {
                question_id: None,
                approval_id: Some(_),
                ..
            } => {
                Err("an approval takes `decision` and none of `text`, `choice` or `choices`".into())
            }
            AnswerArgs {
                question_id: Some(_),
                approval_id: None,
                ..
            } => Err("a question takes `text`, `choice` or `choices`, not `decision`".into()),
            _ => Err("pass exactly one of `question_id` or `approval_id`".into()),
        }
    }
}

impl From<AnswerInput> for AnswerArgs {
    fn from(input: AnswerInput) -> Self {
        match input {
            AnswerInput::Question {
                child,
                question_id,
                answer,
            } => {
                let (text, choice, choices) = match answer {
                    Answer::Text { text } => (Some(text), None, None),
                    Answer::Choice { index } => (None, Some(index), None),
                    Answer::Choices { indices } => (None, None, Some(indices)),
                };
                AnswerArgs {
                    child: Some(child),
                    question_id: Some(question_id),
                    text,
                    choice,
                    choices,
                    ..AnswerArgs::default()
                }
            }
            AnswerInput::Approval {
                child,
                approval_id,
                decision,
            } => AnswerArgs {
                child: Some(child),
                approval_id: Some(approval_id),
                decision: Some(decision),
                ..AnswerArgs::default()
            },
        }
    }
}

/// Result of `answer`: the child received the answer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AnswerOutput {}

/// A question or approval request, by id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RequestRef {
    /// A question.
    Question(QuestionId),
    /// An approval request.
    Approval(ApprovalId),
}

/// Arguments of `escalate`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "EscalateArgs", into = "EscalateArgs")]
pub struct EscalateInput {
    /// The child that asked.
    pub child: SessionId,
    /// The request handed to the user.
    pub request: RequestRef,
    /// Context for the user.
    pub note: Option<String>,
}

/// Wire shape of `escalate`'s arguments; flat for the same reason as [`AnswerArgs`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EscalateArgs {
    /// The child whose question or approval request this is, as wait_for or status named it.
    #[schemars(with = "SessionId")]
    pub child: Option<SessionId>,
    /// The question to hand over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "QuestionId")]
    pub question_id: Option<QuestionId>,
    /// The approval request to hand over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "ApprovalId")]
    pub approval_id: Option<ApprovalId>,
    /// What the user should know to decide: your view, the options, the risk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub note: Option<String>,
}

impl TryFrom<EscalateArgs> for EscalateInput {
    type Error = String;

    fn try_from(args: EscalateArgs) -> Result<Self, String> {
        let Some(child) = args.child else {
            return Err("pass `child`, the child that asked".into());
        };
        let request = match (args.question_id, args.approval_id) {
            (Some(id), None) => RequestRef::Question(id),
            (None, Some(id)) => RequestRef::Approval(id),
            _ => return Err("pass exactly one of `question_id` or `approval_id`".into()),
        };
        Ok(EscalateInput {
            child,
            request,
            note: args.note,
        })
    }
}

impl From<EscalateInput> for EscalateArgs {
    fn from(input: EscalateInput) -> Self {
        let (question_id, approval_id) = match input.request {
            RequestRef::Question(id) => (Some(id), None),
            RequestRef::Approval(id) => (None, Some(id)),
        };
        EscalateArgs {
            child: Some(input.child),
            question_id,
            approval_id,
            note: input.note,
        }
    }
}

/// Result of `escalate`: the request now waits for the user.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EscalateOutput {}

/// Send a message to an existing independent session on the same daemon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendSessionInput {
    /// Destination session ID on this host; never your own session.
    pub session_id: SessionId,
    /// Message text. The recipient sees it as an agent message, not human instructions.
    pub text: String,
    /// Stable unique key for this delivery. Reuse on retries; never reuse for different text.
    pub message_id: String,
}

/// Acceptance of a same-host agent message, not a promise of a reply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SendSessionOutput {
    /// Whether the message was accepted behind an active turn or an existing queued prompt.
    pub queued: bool,
    /// True when this sender/key was already accepted; no second prompt was created.
    pub duplicate: bool,
}

/// Arguments of `publish`: `title` and exactly one of `path` and `html`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PublishInput {
    /// Short label shown with the artifact, e.g. "Settings screen after the fix".
    pub title: String,
    /// A file to publish, of any type: absolute, or relative to your working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// One complete, self-contained HTML document to publish, with all CSS, JavaScript, SVG
    /// and data inline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
}

/// Result of `publish`: the artifact is in the thread.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PublishOutput {
    /// Public link to the artifact, for your reply; absent when the project keeps artifacts
    /// private.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// When `url` stops working; absent when it never expires or there is no `url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
}

/// Arguments of `overview`: none.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OverviewInput {}

/// Result of `overview`: what this host's herder daemon holds, as its apps see it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OverviewOutput {
    /// Your own session.
    pub you: SessionId,
    /// Every session on this host, archived ones included.
    pub sessions: Vec<SessionHead>,
    /// Every project with a clone on this host.
    pub projects: Vec<Project>,
    /// Every account sessions may run on, with its usage.
    pub accounts: Vec<Account>,
}

/// Arguments of `secrets`: none.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SecretsInput {}

/// Result of `secrets`: what you may use, never the values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SecretsOutput {
    /// Every secret this host may use, ordered by set, then name.
    pub secrets: Vec<SecretEntry>,
    /// The command that runs a program with secrets in its environment: append the secrets,
    /// `--` and the program with its arguments.
    pub run: String,
}

/// A secret you may use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SecretEntry {
    /// What to pass to `herder secret run`: the name, or `set/name` when more than one set has
    /// the name.
    pub secret: SecretRef,
    /// The environment variable the program gets it as.
    pub name: String,
    /// Its set.
    pub set: String,
    /// What it is for.
    pub description: String,
}

/// Arguments of `store_secret`. There is no value: the daemon reads it from `from_file`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StoreSecretInput {
    /// The set to store it in, such as `personal`; the owner who allows it may pick another.
    pub set: String,
    /// Its name, the environment variable commands get it as: uppercase letters, digits and
    /// underscores, not starting with a digit, such as `ASC_PRIVATE_KEY`.
    pub name: String,
    /// What it is for, on one line of at most 200 characters; agents read it to pick a secret.
    pub description: String,
    /// The file on this machine holding the value, as it is: absolute, `~/...`, or relative to
    /// your working directory. Text of at most 64 KiB; newlines are kept.
    pub from_file: String,
    /// Whether to delete the file once the vault has stored the value.
    #[serde(default)]
    pub delete_file_after: bool,
}

/// Result of `store_secret`: what was stored, never the value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StoreSecretOutput {
    /// The set it was stored in, which the owner may have picked over the one asked for.
    pub set: String,
    /// Its name.
    pub name: String,
    /// The value's size in bytes.
    pub bytes: u64,
    /// The first 8 hex digits of the value's SHA-256.
    pub fingerprint: String,
    /// Whether the file was deleted.
    pub file_deleted: bool,
}

/// Arguments of `command`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommandInput {
    /// The herder command, as a herder app sends it: its `type` and that command's arguments
    /// beside it, e.g. `{"type": "rename_session", "session_id": "...", "title": "..."}`. A
    /// call with arguments missing or wrong fails with the command's JSON Schema, so to learn
    /// a command's arguments, call it with its `type` alone.
    #[schemars(schema_with = "crate::command_schema")]
    pub command: CommandBody,
}

/// Arguments of `remember`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RememberInput {
    /// The fact, as one short line a future conversation can act on, e.g. "Staging deploys go
    /// through `make deploy-staging`". Under about 150 characters.
    pub text: String,
    /// The id of a fact this one updates, which it replaces in its place. Prefer this to adding
    /// a fact when one already covers the subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub replaces: Option<FactId>,
}

/// Result of `remember`: the fact is kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RememberOutput {
    /// The fact's id, for `replaces` and `forget`.
    pub id: FactId,
    /// Everything the bot remembers now, oldest first.
    pub memory: Vec<MemoryFact>,
}

/// Arguments of `forget`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForgetInput {
    /// The id of the fact to forget.
    pub id: FactId,
}

/// Result of `forget`: the fact is gone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ForgetOutput {
    /// Everything the bot remembers now, oldest first.
    pub memory: Vec<MemoryFact>,
}

/// A fact the bot remembers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryFact {
    /// The fact's id.
    pub id: FactId,
    /// The fact.
    pub text: String,
}
