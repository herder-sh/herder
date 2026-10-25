//! The chat bots this host holds, as owners list, put and remove them from a client: the
//! `[[bots]]` entries of its config file ([`crate::config::put_bot`]); and the bots running.
//!
//! Each bot runs while its entry is there: it holds its tokens, read from the host's vault as
//! machine-level secrets and kept in memory only, connects to its chat over that chat's
//! [`bridge::Bridge`], and drives the host's sessions from it ([`runner`]). A bot whose tokens
//! cannot be read waits for the vault and retries; one whose chat refuses them waits for new
//! ones; one whose chat cannot be reached connects again, backing off. Changing a bot's entry
//! connects it again. Slack bots run ([`slack`]); Telegram ones are `unsupported` for now.

pub mod bridge;
#[cfg(test)]
mod fake;
pub mod runner;
pub mod slack;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, PoisonError};
use std::time::Duration;

use herder_protocol::{
    Account, Bot, BotError, BotId, BotKind, BotState, BotStatus, CommandBody, CommandResult,
    ErrorCode, ErrorInfo, Event, Item, ItemId, SessionHead, SessionId, Timestamp,
};
use tokio::sync::{Mutex, broadcast};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::config::{self, Config};
use crate::session::EventSink;
use crate::vault::{Held, HostSecrets};

/// Session events the bots have not read yet, at most; a bot that falls further behind misses
/// some, and says so.
pub const EVENTS: usize = 1024;

/// The first wait before connecting again a bot that could not connect; it doubles with each
/// failure, up to [`MAX_BACKOFF`].
const MIN_BACKOFF: Duration = Duration::from_secs(1);

/// The longest wait before connecting again.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// The host's bots: its config file's `[[bots]]` entries, and those running.
pub struct Bots {
    /// The config file.
    path: PathBuf,
    /// Held across a change, so changes apply one at a time.
    changing: Mutex<()>,
    /// What bots run with, once started ([`Bots::start`]).
    runtime: OnceLock<Runtime>,
    /// Each running bot as it runs, and what stops it.
    running: std::sync::Mutex<HashMap<BotId, (Bot, CancellationToken)>>,
    /// How each running bot's connection stands.
    statuses: Statuses,
}

/// What bots run with.
#[derive(Clone)]
pub struct Runtime {
    /// The host whose sessions they drive.
    pub host: runner::Host,
    /// Where their tokens come from.
    pub secrets: Arc<HostSecrets>,
    /// The sessions' events, as [`Watch`] sends them.
    pub events: broadcast::Sender<Event>,
    /// Stops every bot.
    pub shutdown: CancellationToken,
}

type Statuses = Arc<std::sync::Mutex<HashMap<BotId, BotStatus>>>;

impl Bots {
    /// The bots of the daemon `config` started; none runs until [`Bots::start`].
    pub fn new(config: &Config) -> Self {
        Self {
            path: config.path.clone(),
            changing: Mutex::new(()),
            runtime: OnceLock::new(),
            running: std::sync::Mutex::default(),
            statuses: Statuses::default(),
        }
    }

    /// Runs the config's bots on `runtime`, and from then on each bot an owner puts.
    pub fn start(&self, runtime: Runtime, bots: &[Bot]) -> anyhow::Result<()> {
        self.runtime
            .set(runtime)
            .map_err(|_| anyhow::anyhow!("the bots run already"))?;
        self.run(bots);
        Ok(())
    }

    /// Runs `bots`, and only them: stops those that are gone or changed, starts those that are
    /// new or changed.
    fn run(&self, bots: &[Bot]) {
        let Some(runtime) = self.runtime.get() else {
            return;
        };
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        running.retain(|bot_id, (bot, stop)| {
            let keep = bots.contains(bot);
            if !keep {
                stop.cancel();
                info!(%bot_id, "a bot stopped");
                self.statuses
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(bot_id);
            }
            keep
        });
        for bot in bots {
            if running.contains_key(&bot.bot_id) {
                continue;
            }
            let stop = runtime.shutdown.child_token();
            let status = Status {
                bot_id: bot.bot_id.clone(),
                statuses: Arc::clone(&self.statuses),
            };
            if bot.kind == BotKind::Slack {
                status.set(BotState::Connecting, None);
                tokio::spawn(keep(bot.clone(), runtime.clone(), status, stop.clone()));
            } else {
                status.set(BotState::Unsupported, None);
            }
            running.insert(bot.bot_id.clone(), (bot.clone(), stop));
        }
    }

    /// Answers `list_bots`, `put_bot`, `remove_bot` and `get_bot_status`.
    pub async fn command(&self, command: CommandBody) -> Result<CommandResult, ErrorInfo> {
        match command {
            CommandBody::ListBots => Ok(CommandResult::Bots {
                bots: self.load().await?.bots,
            }),
            CommandBody::PutBot { bot } => {
                let _changing = self.changing.lock().await;
                let path = self.path.clone();
                let bot_id = bot.bot_id.clone();
                blocking(move || config::put_bot(&path, &bot))
                    .await?
                    .map_err(|err| error(ErrorCode::BadRequest, format!("{err:#}")))?;
                info!(%bot_id, "an owner put a bot");
                self.run(&self.load().await?.bots);
                Ok(CommandResult::Applied)
            }
            CommandBody::RemoveBot { bot_id } => {
                let _changing = self.changing.lock().await;
                let path = self.path.clone();
                let id = bot_id.clone();
                let removed = blocking(move || config::remove_bot(&path, &id))
                    .await?
                    .map_err(|err| error(ErrorCode::Internal, format!("{err:#}")))?;
                if !removed {
                    return Err(error(
                        ErrorCode::NotFound,
                        format!("bot {bot_id} does not exist"),
                    ));
                }
                info!(%bot_id, "an owner removed a bot");
                self.run(&self.load().await?.bots);
                Ok(CommandResult::Applied)
            }
            CommandBody::GetBotStatus => {
                let bots = self.load().await?.bots;
                let statuses = self.statuses.lock().unwrap_or_else(PoisonError::into_inner);
                Ok(CommandResult::BotStatus {
                    bots: bots
                        .into_iter()
                        .map(|bot| match statuses.get(&bot.bot_id) {
                            Some(status) => status.clone(),
                            None => BotStatus {
                                state: match bot.kind {
                                    BotKind::Slack => BotState::Connecting,
                                    _ => BotState::Unsupported,
                                },
                                bot_id: bot.bot_id,
                                last_error: None,
                            },
                        })
                        .collect(),
                })
            }
            _ => Err(error(
                ErrorCode::Internal,
                "the bots do not handle this command",
            )),
        }
    }

    /// The config file as the daemon would load it now.
    async fn load(&self) -> Result<Config, ErrorInfo> {
        let path = self.path.clone();
        blocking(move || Config::load_file(&path))
            .await?
            .map_err(|err| error(ErrorCode::Internal, format!("{err:#}")))
    }
}

/// Where one bot's status is kept.
struct Status {
    bot_id: BotId,
    statuses: Statuses,
}

impl Status {
    /// Sets the bot's state, and its latest error when `error` is one; an earlier error stays
    /// otherwise.
    fn set(&self, state: BotState, error: Option<String>) {
        let mut statuses = self.statuses.lock().unwrap_or_else(PoisonError::into_inner);
        let status = statuses
            .entry(self.bot_id.clone())
            .or_insert_with(|| BotStatus {
                bot_id: self.bot_id.clone(),
                state,
                last_error: None,
            });
        status.state = state;
        if let Some(message) = error {
            status.last_error = Some(BotError {
                at: Timestamp::now(),
                message,
            });
        }
    }
}

/// Keeps a Slack bot connected until `stop`: holds its tokens, connects with them, and runs it
/// while connected, connecting again when its tokens change or its connection fails.
async fn keep(bot: Bot, runtime: Runtime, status: Status, stop: CancellationToken) {
    let mut tokens = vec![bot.token.clone()];
    tokens.extend(bot.app_token.clone());
    // Holds the tokens until the bot stops.
    let holding = runtime.secrets.hold(format!("bot:{}", bot.bot_id), tokens);
    let mut held = holding.watch();
    let mut backoff = MIN_BACKOFF;
    loop {
        let values = match held.borrow_and_update().clone() {
            Held::Ok(values) => values,
            Held::WaitingForVault => {
                status.set(BotState::WaitingForVault, None);
                Vec::new()
            }
            Held::Denied(denied) => {
                status.set(BotState::WaitingForVault, Some(denied.message));
                Vec::new()
            }
        };
        let [bot_token, app_token] = values.as_slice() else {
            tokio::select! {
                () = stop.cancelled() => return,
                changed = held.changed() => if changed.is_err() { return },
            }
            continue;
        };
        status.set(BotState::Connecting, None);
        let connected = tokio::select! {
            () = stop.cancelled() => return,
            connected = slack::connect(&bot_token.value, &app_token.value) => connected,
        };
        let wait = match connected {
            Ok(connection) => {
                backoff = MIN_BACKOFF;
                status.set(BotState::Connected, None);
                info!(bot_id = %bot.bot_id, "a bot connected");
                let runner =
                    runner::Runner::new(bot.clone(), connection.bridge, runtime.host.clone());
                let events = runtime.events.subscribe();
                let running = stop.child_token();
                tokio::select! {
                    () = runner.run(connection.inbound, events, running.clone()) => {}
                    _ = held.changed() => {}
                }
                running.cancel();
                connection.closer.close().await;
                if stop.is_cancelled() {
                    return;
                }
                status.set(BotState::Connecting, None);
                continue;
            }
            Err(slack::ConnectError::Rejected(message)) => {
                warn!(bot_id = %bot.bot_id, "{message}");
                status.set(BotState::TokenRejected, Some(message));
                None
            }
            Err(slack::ConnectError::Failed(message)) => {
                warn!(bot_id = %bot.bot_id, "{message}");
                status.set(BotState::Connecting, Some(message));
                let wait = backoff;
                backoff = (backoff * 2).min(MAX_BACKOFF);
                Some(wait)
            }
        };
        tokio::select! {
            () = stop.cancelled() => return,
            changed = held.changed() => if changed.is_err() { return },
            () = sleep_or_forever(wait) => {}
        }
    }
}

async fn sleep_or_forever(wait: Option<Duration>) {
    match wait {
        Some(wait) => tokio::time::sleep(wait).await,
        None => std::future::pending().await,
    }
}

/// Passes the sessions' events on to the bots too.
pub struct Watch {
    /// Where every event goes on to.
    pub next: Arc<dyn EventSink>,
    /// Where the bots read them.
    pub events: broadcast::Sender<Event>,
}

impl EventSink for Watch {
    fn event(&self, event: &Event) {
        // No bot runs while none subscribes.
        let _ = self.events.send(event.clone());
        self.next.event(event);
    }

    fn snapshot(&self, session_id: &SessionId, item: &Item) {
        self.next.snapshot(session_id, item);
    }

    fn delta(&self, session_id: &SessionId, item_id: &ItemId, text: &str) {
        self.next.delta(session_id, item_id, text);
    }

    fn sessions_changed(&self, sessions: &[SessionHead]) {
        self.next.sessions_changed(sessions);
    }

    fn accounts_changed(&self, accounts: &[Account]) {
        self.next.accounts_changed(accounts);
    }
}

/// Runs `call` on the blocking pool.
async fn blocking<T: Send + 'static>(
    call: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ErrorInfo> {
    tokio::task::spawn_blocking(call)
        .await
        .map_err(|err| error(ErrorCode::Internal, format!("{err}")))
}

fn error(code: ErrorCode, message: impl Into<String>) -> ErrorInfo {
    ErrorInfo {
        code,
        message: message.into(),
    }
}
