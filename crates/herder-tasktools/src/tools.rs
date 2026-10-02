//! Arguments and results of each task tool.
//!
//! Field docs become the `description`s in the tool schemas, so they are written for the agent
//! that calls the tool.

use herder_protocol::{
    Answer, ApprovalDecision, ApprovalId, PermissionMode, Provider, QuestionId, SessionId,
    SessionStatus, TurnId,
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
    /// Branch the child works on, in this repository.
    pub branch: String,
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
    /// Branch the child works on, in this repository.
    pub branch: String,
    /// `running` while it works or waits on you, `waiting_for_capacity` while its next turn waits for this machine to have room (it still counts as working), `idle` when its turn ended, `needs_you` when it waits on the user, `error` when it cannot continue.
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
    /// The child asked a question. Answer with `question_id` and `text`, or `choice` when it lists choices.
    Question {
        /// The question.
        question_id: QuestionId,
        /// The question, as Markdown.
        text: String,
        /// Answers to pick from by position; empty for a free-text answer.
        choices: Vec<String>,
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
        /// The child's status now.
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
    /// The question to answer. Pass it with `text` or `choice`.
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
                decision: None,
                ..
            } => {
                let answer = match (text, choice) {
                    (Some(text), None) => Answer::Text { text },
                    (None, Some(index)) => Answer::Choice { index },
                    _ => return Err("a question takes exactly one of `text` or `choice`".into()),
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
            } => Err("an approval takes `decision` and neither `text` nor `choice`".into()),
            AnswerArgs {
                question_id: Some(_),
                approval_id: None,
                ..
            } => Err("a question takes `text` or `choice`, not `decision`".into()),
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
                let (text, choice) = match answer {
                    Answer::Text { text } => (Some(text), None),
                    Answer::Choice { index } => (None, Some(index)),
                };
                AnswerArgs {
                    child: Some(child),
                    question_id: Some(question_id),
                    text,
                    choice,
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
