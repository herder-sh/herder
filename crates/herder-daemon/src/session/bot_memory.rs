//! The sessions a chat bot starts: what they are told of the bot ([`crate::bots::memory`]), and
//! the `remember` and `forget` tools, which change the memory of that bot only.

use herder_protocol::{Bot, BotFact, SessionId, SessionOrigin};
use herder_tasktools::{
    ErrorCode, ForgetInput, ForgetOutput, MemoryFact, RememberInput, RememberOutput, ToolError,
};
use tracing::{info, warn};

use super::SessionManager;
use crate::bots::memory::{self, Change, Refused};

impl SessionManager {
    /// What `caller` is told of the bot that started it, with the facts it remembers now;
    /// `None` when no bot the host holds started it.
    pub(crate) async fn bot_brief(&self, caller: &SessionId) -> Option<String> {
        let (bot, _) = self.bot_of(caller).await.ok()?;
        match self.bot_facts(&bot.bot_id).await {
            Ok(facts) => Some(memory::brief(&bot, &facts)),
            Err(err) => {
                warn!(session_id = %caller, "cannot read a bot's memory: {}", err.message);
                Some(memory::brief(&bot, &[]))
            }
        }
    }

    /// `remember`, from `caller`'s agent: keeps a fact in the memory of the bot that started
    /// it, learned in the first of the bot's threads linked to it.
    pub(crate) async fn remember(
        &self,
        caller: SessionId,
        input: RememberInput,
    ) -> Result<RememberOutput, ToolError> {
        let (bot, origin) = self.bot_of(&caller).await?;
        let thread = self
            .inner
            .journal
            .threads(caller.clone())
            .await
            .map_err(|err| internal(format!("{err:#}")))?
            .into_iter()
            .find(|thread| thread.bot_id == bot.bot_id);
        let change = Change::Set {
            replaces: input.replaces,
            text: input.text,
            thread,
            place: Some(origin.place),
            by: Some(origin.by),
        };
        let (id, facts) = self.change(&bot, change).await?;
        info!(session_id = %caller, bot_id = %bot.bot_id, "a bot remembered a fact");
        Ok(RememberOutput {
            id: id.ok_or_else(|| internal("remembering set no fact"))?,
            memory: listed(facts),
        })
    }

    /// `forget`, from `caller`'s agent: drops a fact from the memory of the bot that started it.
    pub(crate) async fn forget(
        &self,
        caller: SessionId,
        input: ForgetInput,
    ) -> Result<ForgetOutput, ToolError> {
        let (bot, _) = self.bot_of(&caller).await?;
        let (_, facts) = self.change(&bot, Change::Delete(input.id)).await?;
        info!(session_id = %caller, bot_id = %bot.bot_id, "a bot forgot a fact");
        Ok(ForgetOutput {
            memory: listed(facts),
        })
    }

    async fn change(
        &self,
        bot: &Bot,
        change: Change,
    ) -> Result<(Option<herder_protocol::FactId>, Vec<BotFact>), ToolError> {
        match self.change_bot_facts(&bot.bot_id, change).await {
            Ok(Ok(changed)) => Ok(changed),
            Ok(Err(refused @ Refused::Invalid(_))) => Err(ToolError::new(
                ErrorCode::InvalidArguments,
                refused.to_string(),
            )),
            Ok(Err(refused @ Refused::NotFound(_))) => {
                Err(ToolError::new(ErrorCode::NotFound, refused.to_string()))
            }
            Err(err) => Err(internal(err.message)),
        }
    }

    /// The bot that started `session_id`, as it runs now, and where it started it.
    async fn bot_of(&self, session_id: &SessionId) -> Result<(Bot, SessionOrigin), ToolError> {
        let session = self
            .inner
            .journal
            .session(session_id.clone())
            .await
            .map_err(|err| internal(format!("{err:#}")))?;
        let not_a_bot = || {
            ToolError::new(
                ErrorCode::NotAllowed,
                "only a session a chat bot of this host started has a bot's memory",
            )
        };
        let origin = session
            .and_then(|session| session.origin)
            .ok_or_else(not_a_bot)?;
        let bot = self
            .inner
            .bots
            .get()
            .and_then(|bots| bots(&origin.bot_id))
            .ok_or_else(not_a_bot)?;
        Ok((bot, origin))
    }
}

fn listed(facts: Vec<BotFact>) -> Vec<MemoryFact> {
    facts
        .into_iter()
        .map(|fact| MemoryFact {
            id: fact.id,
            text: fact.text,
        })
        .collect()
}

fn internal(message: impl Into<String>) -> ToolError {
    ToolError::new(ErrorCode::Internal, message)
}
