//! A generic adapter for agents that speak the Agent Client Protocol (ACP) over stdio.
//!
//! One [`AcpAdapter`] drives any ACP agent; what differs between agents (program, arguments,
//! the variable that points the agent at an account's config dir) is an [`AgentProfile`].
//! The JSON-RPC framing is done here, over a [`Transport`], so recorded fixtures replay: request
//! ids count up from 1, where the `agent-client-protocol` SDK mints random ids that a recording
//! can never answer. The schema types are a small local subset, see `schema.rs` for why.
//!
//! # Mapping
//!
//! - `initialize`, then `session/new` in the session's worktree, run in
//!   [`Adapter::start`]. A JSON-RPC error there fails the start, classified like a turn error.
//! - [`AdapterCommand::SendPrompt`] is `session/prompt`: an `image` block per image, then the
//!   text. Its `session/update` notifications
//!   become items: agent message and thought chunks stream as `assistant_message` and
//!   `reasoning` items; a tool call is completed once it starts running, is approved, or
//!   finishes, and its `completed`/`failed` status becomes a `tool_result`. The prompt's
//!   response ends the turn: `cancelled` is `TurnInterrupted`, `refusal` is a fatal
//!   `TurnFailed`, every other stop reason is `TurnCompleted`.
//! - [`AdapterCommand::Interrupt`] is `session/cancel`; pending permission requests are
//!   answered `cancelled`, as ACP requires.
//! - `session/request_permission` is decided by the session's [`PermissionMode`]: reads,
//!   searches and thinking are always allowed; edits are refused in `read_only`, asked in `ask`,
//!   allowed from `auto_edit`; commands and everything else are refused in `read_only`, asked in
//!   `ask` and `auto_edit`, allowed in `full_access`. Asking is [`AdapterEvent::ApprovalRequested`].
//!   Since the adapter decides, every mode switch is native; this relies on the agent asking
//!   before every write or command, which each profile arranges at launch.
//! - The model is the agent's `model` config option (`session/set_config_option`) when it has
//!   one, which makes model switches native. `session/set_model` is unstable and unreliable in
//!   OpenCode, so it is not used. A profile with a model flag passes the starting model on the
//!   command line instead.
//! - Whether the agent takes images is its `promptCapabilities.image` from `initialize`.
//!   [`Adapter::accepts_images`] must answer before any start, so it gives the profile's
//!   [`AgentProfile::images`] until an agent started, then what the latest one advertised. A
//!   session whose agent turns out to take none still runs a prompt that carries images: each
//!   becomes a line of text naming it, so the agent knows one was attached.
//! - [`StartRequest::seed`] is rendered as a transcript in front of the first prompt; ACP has
//!   no way to insert history.
//!
//! # Not mapped
//!
//! - `session/load`: [`Capabilities::native_resume`] is false, so the daemon never passes
//!   [`StartRequest::resume`]: every start is `session/new` and continuity comes from the seed.
//! - Limit windows: ACP has none (`usage_update` is the context window and cost), so
//!   [`Capabilities::reports_usage`] is false and limits surface only as errors.
//! - Client file system and terminal capabilities are not offered; agents use their own tools.
//!
//! # Turn usage
//!
//! A `session/prompt` response's `usage`, from agents that send it (OpenCode does, Grok does
//! not), gives a completed turn's tokens. Its cost is what the session's cost in the
//! last `usage_update` grew by during the turn; an agent that reports no cost in US dollars
//! gets the price table's estimate on the current model.

mod classify;
mod profile;
mod rpc;
mod schema;
mod session;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use herder_protocol::{ErrorClass, TurnError};

use crate::fixture::Fixture;
use crate::transport::Transport;
use crate::{Adapter, StartFuture, StartRequest};

pub use profile::AgentProfile;

#[cfg(doc)]
use crate::{AdapterCommand, AdapterEvent, Capabilities};
#[cfg(doc)]
use herder_protocol::PermissionMode;

/// Runs sessions on an ACP agent described by a profile.
#[derive(Clone, Debug)]
pub struct AcpAdapter {
    profile: AgentProfile,
    /// Replays this fixture instead of spawning the agent.
    fixture: Option<PathBuf>,
    /// Whether the agent takes images: the profile's guess until an agent says in
    /// `initialize`, then what the latest one said.
    images: Arc<AtomicBool>,
}

impl AcpAdapter {
    /// An adapter that spawns the agent `profile` describes.
    pub fn new(profile: AgentProfile) -> Self {
        Self {
            images: Arc::new(AtomicBool::new(profile.images)),
            profile,
            fixture: None,
        }
    }

    /// An adapter that replays the recorded fixture at `path` instead of spawning the agent;
    /// every start replays it afresh.
    pub fn replaying(profile: AgentProfile, path: impl Into<PathBuf>) -> Self {
        Self {
            images: Arc::new(AtomicBool::new(profile.images)),
            profile,
            fixture: Some(path.into()),
        }
    }

    /// The profile sessions run with.
    pub fn profile(&self) -> &AgentProfile {
        &self.profile
    }
}

impl Adapter for AcpAdapter {
    fn start(&self, request: StartRequest) -> StartFuture {
        let profile = self.profile.clone();
        let fixture = self.fixture.clone();
        let images = Arc::clone(&self.images);
        Box::pin(async move {
            let fatal = |message: String| TurnError {
                class: ErrorClass::Fatal,
                message,
            };
            let transport = match fixture {
                Some(path) => {
                    Transport::replay(Fixture::load(path).map_err(|err| fatal(err.to_string()))?)
                }
                None => Transport::spawn(profile.command(&request))
                    .map_err(|err| fatal(format!("running {}: {err}", profile.program)))?,
            };
            session::start(&profile, request, transport, &images).await
        })
    }

    fn accepts_images(&self) -> bool {
        self.images.load(Ordering::Relaxed)
    }
}
