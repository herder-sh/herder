//! The chat bots this host holds, as owners list, put and remove them from a client: the
//! `[[bots]]` entries of its config file ([`crate::config::put_bot`]), and how each one's
//! connection stands.
//!
//! This build connects no bot to its chat yet, so every bot's status is `unsupported`.

use std::path::PathBuf;

use herder_protocol::{BotState, BotStatus, CommandBody, CommandResult, ErrorCode, ErrorInfo};
use tokio::sync::Mutex;
use tracing::info;

use crate::config::{self, Config};

/// The host's bots: its config file's `[[bots]]` entries.
pub struct Bots {
    /// The config file.
    path: PathBuf,
    /// Held across a change, so changes apply one at a time.
    changing: Mutex<()>,
}

impl Bots {
    /// The bots of the daemon `config` started.
    pub fn new(config: &Config) -> Self {
        Self {
            path: config.path.clone(),
            changing: Mutex::new(()),
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
                Ok(CommandResult::Applied)
            }
            CommandBody::GetBotStatus => Ok(CommandResult::BotStatus {
                bots: self
                    .load()
                    .await?
                    .bots
                    .into_iter()
                    .map(|bot| BotStatus {
                        bot_id: bot.bot_id,
                        state: BotState::Unsupported,
                        last_error: None,
                    })
                    .collect(),
            }),
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

#[cfg(test)]
mod tests {
    use herder_protocol::{
        AccountId, Bot, BotDefaults, BotId, BotKind, BotUser, ChannelBinding, PermissionMode,
        ProjectId, Provider,
    };

    use super::*;

    const CONFIG: &str = "# the daemon's config\n\
                          [[accounts]]\nid = \"claude-main\"\nprovider = \"claude\"\n\
                          config_dir = \"/h/.claude-main\"\n\n\
                          [[accounts]]\nid = \"codex-main\"\nprovider = \"codex\"\n\
                          config_dir = \"/h/.codex-main\"\n";

    fn slack(id: &str) -> Bot {
        Bot {
            bot_id: BotId::new(id),
            kind: BotKind::Slack,
            name: id.into(),
            token: "slack/BOT_TOKEN".parse().unwrap(),
            app_token: Some("slack/APP_TOKEN".parse().unwrap()),
            allow: vec![BotUser {
                external_user_id: "U012AB3CD".into(),
                display_name: "Ada".into(),
                dm: true,
                channels: false,
                approve: true,
                notify: false,
            }],
            defaults: BotDefaults {
                project_id: Some(ProjectId::new("github.com/herder-sh/herder")),
                provider: Some(Provider::Claude),
                account_ids: vec![AccountId::new("claude-main")],
                model: Some("opus".into()),
                permission_mode: Some(PermissionMode::Ask),
            },
            channel_bindings: vec![ChannelBinding {
                channel_id: "C012AB3CD".into(),
                project_id: ProjectId::new("github.com/herder-sh/trip1"),
            }],
        }
    }

    fn telegram(id: &str) -> Bot {
        Bot {
            bot_id: BotId::new(id),
            kind: BotKind::Telegram,
            name: id.into(),
            token: "TELEGRAM_TOKEN".parse().unwrap(),
            app_token: None,
            allow: Vec::new(),
            defaults: BotDefaults::default(),
            channel_bindings: Vec::new(),
        }
    }

    fn bots(tmp: &tempfile::TempDir) -> (PathBuf, Bots) {
        let path = tmp.path().join("daemon.toml");
        std::fs::write(&path, CONFIG).unwrap();
        let bots = Bots::new(&Config::load_file(&path).unwrap());
        (path, bots)
    }

    async fn list(bots: &Bots) -> Vec<Bot> {
        match bots.command(CommandBody::ListBots).await.unwrap() {
            CommandResult::Bots { bots } => bots,
            other => panic!("listed as {other:?}"),
        }
    }

    #[tokio::test]
    async fn put_list_and_remove_round_trip_through_the_config_file() {
        let tmp = tempfile::tempdir().unwrap();
        let (path, bots) = bots(&tmp);
        assert_eq!(list(&bots).await, []);

        for bot in [slack("trash-can"), telegram("tg")] {
            let put = bots
                .command(CommandBody::PutBot { bot: Box::new(bot) })
                .await
                .unwrap();
            assert_eq!(put, CommandResult::Applied);
        }
        assert_eq!(list(&bots).await, [slack("trash-can"), telegram("tg")]);
        // The file holds the bots as a client put them, and only the names of their tokens.
        let file = std::fs::read_to_string(&path).unwrap();
        assert!(file.starts_with(CONFIG), "{file}");
        assert!(file.contains("[[bots]]\nbot_id = \"trash-can\""), "{file}");
        assert!(file.contains("token = \"slack/BOT_TOKEN\""), "{file}");
        assert_eq!(
            Config::load_file(&path).unwrap().bots,
            [slack("trash-can"), telegram("tg")]
        );

        // Putting a bot again replaces it in its place.
        let mut renamed = slack("trash-can");
        renamed.name = "Trash can".into();
        renamed.allow.clear();
        bots.command(CommandBody::PutBot {
            bot: Box::new(renamed.clone()),
        })
        .await
        .unwrap();
        assert_eq!(list(&bots).await, [renamed.clone(), telegram("tg")]);

        let status = bots.command(CommandBody::GetBotStatus).await.unwrap();
        let CommandResult::BotStatus { bots: status } = status else {
            panic!("answered {status:?}");
        };
        assert_eq!(
            status
                .iter()
                .map(|status| (status.bot_id.as_str(), status.state))
                .collect::<Vec<_>>(),
            [
                ("trash-can", BotState::Unsupported),
                ("tg", BotState::Unsupported)
            ]
        );

        for id in ["tg", "trash-can"] {
            let removed = bots
                .command(CommandBody::RemoveBot {
                    bot_id: BotId::new(id),
                })
                .await
                .unwrap();
            assert_eq!(removed, CommandResult::Applied);
        }
        assert_eq!(list(&bots).await, []);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), CONFIG);
        let missing = bots
            .command(CommandBody::RemoveBot {
                bot_id: BotId::new("tg"),
            })
            .await
            .unwrap_err();
        assert_eq!(missing.code, ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn an_invalid_bot_is_refused_and_changes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (path, bots) = bots(&tmp);
        bots.command(CommandBody::PutBot {
            bot: Box::new(slack("trash-can")),
        })
        .await
        .unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let mut no_app_token = slack("a");
        no_app_token.app_token = None;
        let mut app_token = telegram("b");
        app_token.app_token = Some("APP_TOKEN".parse().unwrap());
        let mut person_twice = slack("c");
        person_twice.allow.push(person_twice.allow[0].clone());
        let mut channel_twice = slack("d");
        channel_twice
            .channel_bindings
            .push(channel_twice.channel_bindings[0].clone());
        let mut unknown_account = slack("e");
        unknown_account.defaults.account_ids = vec![AccountId::new("nobody")];
        let mut other_provider = slack("f");
        other_provider.defaults.account_ids = vec![AccountId::new("codex-main")];
        let mut mixed = slack("g");
        mixed.defaults.provider = None;
        mixed.defaults.account_ids =
            vec![AccountId::new("claude-main"), AccountId::new("codex-main")];
        let mut blank_name = slack("h");
        blank_name.name = " ".into();
        for (bot, expected) in [
            (
                slack("not an id"),
                "bot id \"not an id\" must be ASCII letters",
            ),
            (no_app_token, "needs app_token"),
            (app_token, "has no app_token"),
            (person_twice, "allow names \"U012AB3CD\" twice"),
            (channel_twice, "channel_bindings names \"C012AB3CD\" twice"),
            (unknown_account, "nobody is not an account"),
            (other_provider, "codex-main is not a claude account"),
            (mixed, "codex-main is not a claude account"),
            (blank_name, "name must be one line"),
        ] {
            let err = bots
                .command(CommandBody::PutBot { bot: Box::new(bot) })
                .await
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::BadRequest, "{}", err.message);
            assert!(err.message.contains(expected), "{}", err.message);
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }
}
