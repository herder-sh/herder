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

fn slack_bot(id: &str) -> Bot {
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

    for bot in [slack_bot("trash-can"), telegram("tg")] {
        let put = bots
            .command(CommandBody::PutBot { bot: Box::new(bot) })
            .await
            .unwrap();
        assert_eq!(put, CommandResult::Applied);
    }
    assert_eq!(list(&bots).await, [slack_bot("trash-can"), telegram("tg")]);
    // The file holds the bots as a client put them, and only the names of their tokens.
    let file = std::fs::read_to_string(&path).unwrap();
    assert!(file.starts_with(CONFIG), "{file}");
    assert!(file.contains("[[bots]]\nbot_id = \"trash-can\""), "{file}");
    assert!(file.contains("token = \"slack/BOT_TOKEN\""), "{file}");
    assert_eq!(
        Config::load_file(&path).unwrap().bots,
        [slack_bot("trash-can"), telegram("tg")]
    );

    // Putting a bot again replaces it in its place.
    let mut renamed = slack_bot("trash-can");
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
            ("trash-can", BotState::Connecting),
            ("tg", BotState::Connecting)
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
        bot: Box::new(slack_bot("trash-can")),
    })
    .await
    .unwrap();
    let before = std::fs::read_to_string(&path).unwrap();

    let mut no_app_token = slack_bot("a");
    no_app_token.app_token = None;
    let mut app_token = telegram("b");
    app_token.app_token = Some("APP_TOKEN".parse().unwrap());
    let mut person_twice = slack_bot("c");
    person_twice.allow.push(person_twice.allow[0].clone());
    let mut channel_twice = slack_bot("d");
    channel_twice
        .channel_bindings
        .push(channel_twice.channel_bindings[0].clone());
    let mut unknown_account = slack_bot("e");
    unknown_account.defaults.account_ids = vec![AccountId::new("nobody")];
    let mut other_provider = slack_bot("f");
    other_provider.defaults.account_ids = vec![AccountId::new("codex-main")];
    let mut mixed = slack_bot("g");
    mixed.defaults.provider = None;
    mixed.defaults.account_ids = vec![AccountId::new("claude-main"), AccountId::new("codex-main")];
    let mut blank_name = slack_bot("h");
    blank_name.name = " ".into();
    for (bot, expected) in [
        (
            slack_bot("not an id"),
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

// The bots running: a bot's runner over the fake bridge, fed recorded Slack payloads, driving
// sessions on the fake adapter.

mod running {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use frankenstein::updates::{Update, UpdateContent};
    use herder_adapters::fake::FakeAdapter;
    use herder_protocol::{
        ApprovalOutcome, Attachment, AttachmentId, EventBody, FILE_MEDIA_TYPE, HostId, ItemBody,
        SessionStatus, TurnId, UserId,
    };
    use herder_store::Store;
    use serde_json::Value;
    use slack_morphism::prelude::{SlackInteractionEvent, SlackPushEventCallback};
    use tokio::sync::mpsc;

    use super::super::bridge::{Action, Inbound, Place, Post};
    use super::super::fake::{FakeBridge, Sent};
    use super::super::runner::{self, Runner};
    use super::super::slack::{payload, render};
    use super::super::telegram;
    use super::super::telegram::payload::{Me, Pressed, Threads};
    use super::super::telegram::render::Keys;
    use super::*;
    use crate::session::{AccountConfig, Accounts, Adapters, SessionManager, Setup};
    use crate::worktree::Worktrees;

    const ME: &str = "U0BOT00001";
    const ADA: &str = "U0ADA00001";
    const BOB: &str = "U0BOB00001";
    const EVE: &str = "U0EVE00001";

    fn dm() -> Place {
        Place {
            channel_id: "D0ADA00001".into(),
            thread_id: "1761400000.000100".into(),
        }
    }

    fn channel() -> Place {
        Place {
            channel_id: "C0TEAM0001".into(),
            thread_id: "1761400400.000700".into(),
        }
    }

    fn fixtures() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
    }

    /// The recorded Socket Mode envelope `name`'s payload.
    fn recorded(name: &str) -> Value {
        let text = std::fs::read_to_string(fixtures().join("slack").join(name)).unwrap();
        let envelope: Value = serde_json::from_str(&text).unwrap();
        envelope["payload"].clone()
    }

    /// The recorded event `name`, as the bot takes it; `None` when it ignores it.
    fn slack(name: &str) -> Option<Inbound> {
        let event: SlackPushEventCallback = serde_json::from_value(recorded(name)).unwrap();
        payload::push_event(event, ME)
    }

    /// The recorded press `name`, by `user`, of the button labelled `label` on `post`.
    fn press(name: &str, user: &str, post: &Post, label: &str) -> Option<Inbound> {
        let mut payload = recorded(name);
        payload["user"]["id"] = user.into();
        payload["actions"][0]["value"] = button(post, label).into();
        let event: SlackInteractionEvent = serde_json::from_value(payload).unwrap();
        payload::interaction(event)
    }

    /// The value of the button labelled `label` that Slack shows on `post`.
    fn button(post: &Post, label: &str) -> String {
        let rendered = render::render(post);
        let buttons = rendered["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|block| {
                let mut elements: Vec<Value> =
                    block["elements"].as_array().cloned().unwrap_or_default();
                elements.push(block["accessory"].clone());
                elements
            })
            .collect::<Vec<_>>();
        buttons
            .iter()
            .find(|element| element["text"]["text"] == label)
            .and_then(|element| element["value"].as_str())
            .unwrap_or_else(|| panic!("no {label} button on {rendered:#}"))
            .to_owned()
    }

    /// A chat the bots run on, with its recorded payloads under `fixtures/‹chat›`.
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Chat {
        Slack,
        Telegram,
    }

    impl Chat {
        fn ada(self) -> &'static str {
            match self {
                Self::Slack => ADA,
                Self::Telegram => "700000001",
            }
        }

        fn bob(self) -> &'static str {
            match self {
                Self::Slack => BOB,
                Self::Telegram => "700000002",
            }
        }

        fn eve(self) -> &'static str {
            match self {
                Self::Slack => EVE,
                Self::Telegram => "700000003",
            }
        }

        fn place(channel_id: &str, thread_id: &str) -> Place {
            Place {
                channel_id: channel_id.into(),
                thread_id: thread_id.into(),
            }
        }

        /// The thread Ada's direct message starts.
        fn dm(self) -> Place {
            match self {
                Self::Slack => dm(),
                Self::Telegram => Self::place("700000001", "101"),
            }
        }

        /// The thread Ada's mention in the team's channel starts.
        fn channel(self) -> Place {
            match self {
                Self::Slack => channel(),
                Self::Telegram => Self::place("-1002000000001", "401"),
            }
        }

        /// The thread Eve's direct message starts.
        fn stranger(self) -> Place {
            match self {
                Self::Slack => Self::place("D0EVE00001", "1761400120.000300"),
                Self::Telegram => Self::place("700000003", "201"),
            }
        }

        /// The thread Ada's direct message with a screenshot starts, and where the bridge
        /// downloads the screenshot from.
        fn image(self) -> (Place, &'static str) {
            match self {
                Self::Slack => (
                    Self::place("D0ADA00001", "1761400300.000600"),
                    "https://files.slack.com/files-pri/T0HERDER01-F0SCREEN01/download/screen.png",
                ),
                Self::Telegram => (Self::place("700000001", "301"), "BQACAgQAAxkBAAMtZScreen01"),
            }
        }

        /// The recorded update `name`, as getUpdates returns it.
        fn update(name: &str) -> UpdateContent {
            let text = std::fs::read_to_string(fixtures().join("telegram").join(name)).unwrap();
            let update: Update = serde_json::from_str(&text).unwrap();
            update.content
        }

        /// The recorded message `name`, as the bot takes it; `None` when it ignores it.
        fn message(self, name: &str) -> Option<Inbound> {
            match self {
                Self::Slack => slack(name),
                Self::Telegram => {
                    let UpdateContent::Message(message) = Self::update(name) else {
                        panic!("{name} is not a message");
                    };
                    telegram::payload::message(&message, &me(), &Threads::default())
                }
            }
        }

        /// The recorded press `name`, by `user`, of the button labelled `label` on `post`;
        /// with no label, the recorded press as it is. `None` when it is no answer yet.
        fn press(self, name: &str, user: &str, post: &Post, label: &str) -> Option<Inbound> {
            match self {
                Self::Slack if label.is_empty() => {
                    payload::interaction(serde_json::from_value(recorded(name)).unwrap())
                }
                Self::Slack => press(name, user, post, label),
                Self::Telegram => {
                    let keys = Keys::default();
                    let UpdateContent::CallbackQuery(mut query) = Self::update(name) else {
                        panic!("{name} is not a press");
                    };
                    if !label.is_empty() {
                        query.from.id = user.parse().unwrap();
                        query.data = Some(telegram_button(post, label, &keys));
                    }
                    match telegram::payload::press(&query, &keys, &Threads::default()) {
                        Pressed::Press(press) => Some(Inbound::Press(press)),
                        _ => None,
                    }
                }
            }
        }

        /// What the chat shows of `post`'s text.
        fn shown(self, post: &Post) -> String {
            match self {
                Self::Slack => render::render(post)["text"].as_str().unwrap().to_owned(),
                Self::Telegram => telegram::render::render(post, &Keys::default())
                    .messages
                    .join("\n"),
            }
        }

        fn bot(self) -> Bot {
            match self {
                Self::Slack => bot(),
                Self::Telegram => Bot {
                    kind: BotKind::Telegram,
                    token: "telegram/BOT_TOKEN".parse().unwrap(),
                    app_token: None,
                    allow: vec![
                        person(self.ada(), "Ada", true),
                        person(self.bob(), "Bob", false),
                    ],
                    ..bot()
                },
            }
        }
    }

    fn me() -> Me {
        Me {
            id: 7_000_000_001,
            username: "trash_can_bot".into(),
        }
    }

    /// The callback data of the button labelled `label` that Telegram shows on `post`.
    fn telegram_button(post: &Post, label: &str, keys: &Keys) -> String {
        let rendered = telegram::render::render(post, keys);
        rendered
            .keyboard
            .iter()
            .flat_map(|keyboard| keyboard.inline_keyboard.iter().flatten())
            .find(|button| button.text == label)
            .and_then(|button| button.callback_data.clone())
            .unwrap_or_else(|| panic!("no {label} button on {rendered:#?}"))
    }

    fn person(id: &str, name: &str, approve: bool) -> BotUser {
        BotUser {
            external_user_id: id.into(),
            display_name: name.into(),
            dm: true,
            channels: true,
            approve,
            notify: false,
        }
    }

    fn bot() -> Bot {
        Bot {
            bot_id: BotId::new("trash-can"),
            kind: BotKind::Slack,
            name: "trash-can".into(),
            token: "slack/BOT_TOKEN".parse().unwrap(),
            app_token: Some("slack/APP_TOKEN".parse().unwrap()),
            allow: vec![person(ADA, "Ada", true), person(BOB, "Bob", false)],
            defaults: BotDefaults {
                provider: Some(Provider::Other("fake".into())),
                account_ids: vec![AccountId::new("account-1")],
                ..BotDefaults::default()
            },
            channel_bindings: Vec::new(),
        }
    }

    /// Publishes nothing: the runner reads the sessions' events from the broadcast.
    struct Nowhere;

    impl EventSink for Nowhere {
        fn event(&self, _: &Event) {}
        fn snapshot(&self, _: &SessionId, _: &Item) {}
        fn delta(&self, _: &SessionId, _: &ItemId, _: &str) {}
        fn sessions_changed(&self, _: &[SessionHead]) {}
        fn accounts_changed(&self, _: &[Account]) {}
    }

    /// A running bot over the fake bridge, and the host it drives.
    struct Running {
        sessions: SessionManager,
        bridge: Arc<FakeBridge>,
        inbound: mpsc::Sender<Inbound>,
        events: broadcast::Sender<Event>,
        dir: PathBuf,
        /// The config file holding the bot.
        config: PathBuf,
        stop: CancellationToken,
        _tmp: tempfile::TempDir,
    }

    impl Drop for Running {
        fn drop(&mut self) {
            self.stop.cancel();
        }
    }

    impl Running {
        /// A bot whose sessions run the fake script `script`, and whose messages carry
        /// `files`, by url.
        async fn start(chat: Chat, script: &str, files: HashMap<String, Vec<u8>>) -> Self {
            Self::with(chat.bot(), script, files).await
        }

        /// `bot` running, as [`Running::start`] runs the test bot.
        async fn with(bot: Bot, script: &str, files: HashMap<String, Vec<u8>>) -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().to_owned();
            let (sessions, events) = sessions(&dir, &fixtures().join("bots").join(script)).await;
            let bridge = Arc::new(FakeBridge::new(files));
            let host = runner::Host {
                host_id: HostId::new("host-1"),
                sessions: sessions.clone(),
                names: Arc::new(|user_id: &UserId| {
                    (user_id.as_str() == "alice").then(|| "Alice".to_owned())
                }),
            };
            let config = dir.join("daemon.toml");
            let account = dir.join("account");
            std::fs::write(
                &config,
                format!(
                    "[[accounts]]\nid = \"account-1\"\nprovider = \"claude\"\n\
                     config_dir = {account:?}\n"
                ),
            )
            .unwrap();
            // A config file runs no fake sessions: it holds the bot without its defaults,
            // which `use` leaves as they are.
            let mut entry = bot.clone();
            entry.defaults = BotDefaults::default();
            crate::config::put_bot(&config, &entry).unwrap();
            let bindings = Bots::new(&Config::load_file(&config).unwrap()).bindings(&bot.bot_id);
            let runner = Runner::new(bot, Arc::clone(&bridge) as _, host, bindings);
            let (inbound, received) = mpsc::channel(16);
            let stop = CancellationToken::new();
            tokio::spawn(runner.run(received, events.subscribe(), stop.clone()));
            Self {
                sessions,
                bridge,
                inbound,
                events,
                dir,
                config,
                stop,
                _tmp: tmp,
            }
        }

        /// The bots the config file lists now.
        async fn listed(&self) -> Vec<Bot> {
            list(&Bots::new(&Config::load_file(&self.config).unwrap())).await
        }

        /// A project named `name`, a git repository in the bot's directory, whose id is
        /// `github.com/‹owner›/‹name›`.
        fn project(&self, owner: &str, name: &str) -> herder_protocol::Project {
            let repo = self.dir.join(owner);
            std::fs::create_dir_all(&repo).unwrap();
            for args in [
                &["init", "--quiet", "--initial-branch=main"][..],
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@example.com",
                    "commit",
                    "--quiet",
                    "--allow-empty",
                    "-m",
                    "init",
                ],
            ] {
                let status = std::process::Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(args)
                    .status()
                    .unwrap();
                assert!(status.success());
            }
            herder_protocol::Project {
                project_id: ProjectId::new(format!("github.com/{owner}/{name}")),
                name: name.into(),
                paths: vec![repo.to_str().unwrap().to_owned()],
                remote: None,
                default_permission_mode: None,
                default_account: None,
                setup_command: None,
                icon: None,
                icon_uploaded: false,
                icon_background: None,
                private_artifacts: false,
            }
        }

        /// A session someone started in the app on `project_id`, titled `title`.
        async fn started(&self, project_id: &ProjectId, title: &str) -> SessionId {
            let alice = UserId::new("alice");
            let created = self
                .sessions
                .handle(
                    alice.clone(),
                    CommandBody::CreateSession {
                        repo: None,
                        chat: false,
                        project_id: Some(project_id.clone()),
                        branch: None,
                        account_id: Some(AccountId::new("account-1")),
                        provider: Some(Provider::Other("fake".into())),
                        model: None,
                        permission_mode: None,
                        failover_pin: None,
                    },
                )
                .await
                .unwrap();
            let CommandResult::SessionCreated { session_id } = created else {
                panic!("created as {created:?}");
            };
            let rename = CommandBody::RenameSession {
                session_id: session_id.clone(),
                title: title.into(),
            };
            self.sessions.handle(alice, rename).await.unwrap();
            session_id
        }

        async fn send(&self, inbound: Option<Inbound>) {
            self.inbound.send(inbound.unwrap()).await.unwrap();
        }

        /// The only session, once it exists.
        async fn session(&self) -> SessionHead {
            for _ in 0..500 {
                let heads = self.sessions.sessions().await.unwrap();
                if let [head] = heads.as_slice() {
                    return head.clone();
                }
                assert!(heads.is_empty(), "{heads:#?}");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("no session started");
        }

        /// Waits until the only session is in `status`.
        async fn status(&self, status: SessionStatus) {
            for _ in 0..500 {
                if self.session().await.status == status {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("the session never became {status:?}");
        }

        /// Waits until the bot has posted, in `place`, a post `matches` picks; returns it.
        async fn posted(&self, place: &Place, matches: impl Fn(&Post) -> bool) -> Post {
            let sent = self
                .bridge
                .until(|sent| sent.iter().any(|sent| is_post(sent, place, &matches)))
                .await;
            sent.into_iter()
                .find_map(|sent| match sent {
                    Sent::Post {
                        place: at, post, ..
                    } if at == *place && matches(&post) => Some(post),
                    _ => None,
                })
                .unwrap()
        }

        /// Every prompt the session got, with who sent it.
        async fn prompts(&self, session_id: &SessionId) -> Vec<(Option<UserId>, String)> {
            let events = self.sessions.read_since(session_id, 0, 1000).await.unwrap();
            events
                .into_iter()
                .filter_map(|event| match event.body {
                    EventBody::ItemAdded { item } => match item.body {
                        ItemBody::UserMessage { text, .. } => Some((event.by, text)),
                        _ => None,
                    },
                    _ => None,
                })
                .collect()
        }
    }

    /// The recorded message `name`, saying `text` in the thread `thread_id`.
    fn said(name: &str, text: &str, thread_id: &str) -> Option<Inbound> {
        let Some(Inbound::Message(mut message)) = slack(name) else {
            panic!("{name} is a message");
        };
        message.text = text.into();
        message.place.thread_id = thread_id.into();
        Some(Inbound::Message(message))
    }

    /// [`press`], in the thread `place`.
    fn press_at(place: &Place, user: &str, post: &Post, label: &str) -> Option<Inbound> {
        let Some(Inbound::Press(mut press)) = press("press_button.json", user, post, label) else {
            panic!("a press");
        };
        press.direct = place.channel_id.starts_with('D');
        press.place = place.clone();
        Some(Inbound::Press(press))
    }

    fn text(text: &str) -> impl Fn(&Post) -> bool + '_ {
        move |post| matches!(post, Post::Text(posted) if posted == text)
    }

    fn is_post(sent: &Sent, place: &Place, matches: impl Fn(&Post) -> bool) -> bool {
        matches!(sent, Sent::Post { place: at, post, .. } if at == place && matches(post))
    }

    fn answer(text: &str) -> impl Fn(&Post) -> bool + '_ {
        move |post| matches!(post, Post::Answer { text: answer, .. } if answer == text)
    }

    /// A session manager on `dir` running `script` for the fake provider, publishing its events
    /// to the returned broadcast.
    async fn sessions(dir: &Path, script: &Path) -> (SessionManager, broadcast::Sender<Event>) {
        let fake = Provider::Other("fake".into());
        let mut adapters = Adapters::new();
        adapters.register(fake.clone(), Arc::new(FakeAdapter::new(script)));
        let mut accounts = Accounts::new();
        accounts.insert(
            AccountId::new("account-1"),
            AccountConfig {
                provider: fake,
                label: "Account 1".into(),
                config_dir: Some(dir.join("account")),
                fallback: false,
            },
        );
        let (events, _) = broadcast::channel(EVENTS);
        let turns = AtomicU64::new(0);
        let setup = Setup {
            store: Store::open(dir.join("herder.db")).unwrap(),
            adapters,
            accounts,
            sink: Arc::new(Watch {
                next: Arc::new(Nowhere),
                events: events.clone(),
            }),
            turn_ids: Box::new(move || {
                TurnId::new(format!("turn-{}", turns.fetch_add(1, Ordering::SeqCst) + 1))
            }),
            worktrees: Worktrees::new(dir.join("worktrees")),
            attachments: dir.join("attachments"),
            chats: dir.join("chats"),
        };
        let manager = SessionManager::open(setup, CancellationToken::new())
            .await
            .unwrap();
        (manager, events)
    }

    macro_rules! on_every_chat {
        ($($test:ident),* $(,)?) => {
            mod on_slack {
                $(
                    #[tokio::test]
                    async fn $test() {
                        super::$test(super::Chat::Slack).await;
                    }
                )*
            }

            mod on_telegram {
                $(
                    #[tokio::test]
                    async fn $test() {
                        super::$test(super::Chat::Telegram).await;
                    }
                )*
            }
        };
    }

    // The scenarios each chat's bot runs, with its own recorded payloads.
    on_every_chat!(
        a_direct_message_starts_a_session_and_a_reply_continues_it,
        in_a_channel_thread_only_mentions_are_prompts,
        people_off_the_allowlist_get_one_note_and_reach_nothing,
        only_people_with_approve_answer_approvals,
        a_multi_select_question_is_answered_with_the_ticked_choices,
        a_reply_answers_the_question_the_session_waits_on,
        a_project_named_in_the_message_is_where_the_thread_works,
        a_long_answer_posts_its_start_and_the_rest_on_full_answer,
        stop_interrupts_the_turn,
        a_prompt_start_in_the_app_is_mirrored_into_the_thread,
        images_reach_the_agent_and_what_it_publishes_is_uploaded,
    );

    async fn a_direct_message_starts_a_session_and_a_reply_continues_it(chat: Chat) {
        let bot = Running::start(chat, "dm.jsonl", HashMap::new()).await;
        bot.send(chat.message("dm_message.json")).await;

        // Only the final answer is posted, in the chat's markup; the working message goes.
        let posted = bot.posted(&chat.dm(), answer("Fixed **it**.")).await;
        let shown = match chat {
            Chat::Slack => "Fixed *it*.",
            Chat::Telegram => "Fixed <b>it</b>.",
        };
        assert_eq!(chat.shown(&posted), shown);
        let sent = bot.bridge.sent();
        let working: Vec<_> = sent
            .iter()
            .filter_map(|sent| match sent {
                Sent::Post {
                    id,
                    post: Post::Working { .. },
                    ..
                } => Some(id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(working.len(), 1, "{sent:#?}");
        assert!(sent.contains(&Sent::Delete {
            place: chat.dm(),
            id: working[0].clone(),
        }));
        let posts: Vec<_> = sent
            .iter()
            .filter(|sent| matches!(sent, Sent::Post { .. }))
            .collect();
        assert_eq!(
            posts.len(),
            2,
            "tool calls and early text are never posted: {sent:#?}"
        );

        // The thread is linked, and the prompt recorded as the person's, via the bot.
        let head = bot.session().await;
        assert!(head.chat);
        assert_eq!(head.threads.len(), 1);
        assert_eq!(head.threads[0].channel_id, chat.dm().channel_id);
        assert_eq!(head.threads[0].thread_id, chat.dm().thread_id);
        assert!(head.threads[0].url.is_some());

        bot.send(chat.message("dm_reply.json")).await;
        bot.posted(&chat.dm(), answer("Added one.")).await;
        assert_eq!(bot.sessions.sessions().await.unwrap().len(), 1);
        let via = Some(UserId::new("via trash-can, Ada"));
        assert_eq!(
            bot.prompts(&head.session_id).await,
            [
                (via.clone(), "Fix the login bug".to_owned()),
                (via, "Thanks, now add a test.".to_owned()),
            ]
        );
    }

    async fn in_a_channel_thread_only_mentions_are_prompts(chat: Chat) {
        let bot = Running::start(chat, "channel.jsonl", HashMap::new()).await;
        bot.send(chat.message("channel_mention.json")).await;
        bot.posted(&chat.channel(), answer("It answers.")).await;

        // A reply without a mention is the people's own talk: Slack sends it as a channel
        // message, which the bot drops, and the runner ignores it should it arrive.
        assert_eq!(chat.message("channel_reply.json"), None);
        let Some(Inbound::Message(mut unmentioned)) = chat.message("channel_mention_reply.json")
        else {
            panic!("a mention is a message");
        };
        unmentioned.mentioned = false;
        unmentioned.text = "Looks good to me".into();
        bot.inbound
            .send(Inbound::Message(unmentioned))
            .await
            .unwrap();

        bot.send(chat.message("channel_mention_reply.json")).await;
        bot.posted(&chat.channel(), answer("With fakes.")).await;
        let head = bot.session().await;
        let prompts: Vec<_> = bot
            .prompts(&head.session_id)
            .await
            .into_iter()
            .map(|(_, text)| text)
            .collect();
        assert_eq!(prompts, ["What does the bot do?", "and how is it tested?"]);
    }

    async fn people_off_the_allowlist_get_one_note_and_reach_nothing(chat: Chat) {
        let bot = Running::start(chat, "dm.jsonl", HashMap::new()).await;
        // Bots, the bot itself among them, are never heard.
        assert_eq!(chat.message("dm_bot.json"), None);
        assert_eq!(chat.message("dm_own.json"), None);

        bot.send(chat.message("dm_stranger.json")).await;
        bot.send(chat.message("dm_stranger.json")).await;
        let place = chat.stranger();
        let sent = bot.bridge.until(|sent| !sent.is_empty()).await;
        // Ada's message, sent after, is taken: the stranger's were handled before it.
        bot.send(chat.message("dm_message.json")).await;
        bot.posted(&chat.dm(), answer("Fixed **it**.")).await;
        let notes: Vec<_> = bot
            .bridge
            .sent()
            .into_iter()
            .filter(|sent| matches!(sent, Sent::Ephemeral { .. }))
            .collect();
        assert_eq!(notes.len(), 1, "{sent:#?}");
        let Sent::Ephemeral {
            place: at, user_id, ..
        } = &notes[0]
        else {
            unreachable!()
        };
        assert_eq!((at, user_id.as_str()), (&place, chat.eve()));
        assert_eq!(bot.sessions.sessions().await.unwrap().len(), 1);
    }

    async fn only_people_with_approve_answer_approvals(chat: Chat) {
        let bot = Running::start(chat, "approval.jsonl", HashMap::new()).await;
        bot.send(chat.message("dm_message.json")).await;
        let approval = bot
            .posted(&chat.dm(), |post| matches!(post, Post::Approval { .. }))
            .await;
        let Post::Approval {
            summary, resolved, ..
        } = &approval
        else {
            unreachable!()
        };
        assert_eq!((summary.as_str(), resolved), ("Run cargo test", &None));

        // Bob may drive sessions but not approve: he gets a note, and the request waits.
        bot.status(SessionStatus::NeedsYou).await;
        bot.send(chat.press("press_button.json", chat.bob(), &approval, "Allow"))
            .await;
        bot.bridge
            .until(|sent| {
                sent.iter().any(
                    |sent| matches!(sent, Sent::Ephemeral { user_id, .. } if user_id == chat.bob()),
                )
            })
            .await;
        assert_eq!(bot.session().await.status, SessionStatus::NeedsYou);

        bot.send(chat.press("press_button.json", chat.ada(), &approval, "Allow"))
            .await;
        bot.posted(&chat.dm(), answer("All green.")).await;
        let edited = bot.bridge.sent().into_iter().any(|sent| {
            matches!(
                sent,
                Sent::Edit { post: Post::Approval { resolved: Some((ApprovalOutcome::Allow, by)), .. }, .. }
                    if by == "Ada"
            )
        });
        assert!(edited, "{:#?}", bot.bridge.sent());
    }

    async fn a_multi_select_question_is_answered_with_the_ticked_choices(chat: Chat) {
        let bot = Running::start(chat, "question.jsonl", HashMap::new()).await;
        bot.send(chat.message("dm_message.json")).await;
        let question = bot
            .posted(&chat.dm(), |post| matches!(post, Post::Question { .. }))
            .await;
        // Ticking a box is no answer yet.
        assert_eq!(
            chat.press("press_tick.json", chat.ada(), &question, ""),
            None
        );
        bot.send(chat.press("press_submit.json", chat.ada(), &question, "Submit"))
            .await;
        bot.posted(&chat.dm(), answer("Set up.")).await;
    }

    async fn a_reply_answers_the_question_the_session_waits_on(chat: Chat) {
        let bot = Running::start(chat, "reply.jsonl", HashMap::new()).await;
        bot.send(chat.message("dm_message.json")).await;
        bot.posted(&chat.dm(), |post| matches!(post, Post::Question { .. }))
            .await;
        bot.send(chat.message("dm_reply.json")).await;
        bot.posted(&chat.dm(), answer("Will do.")).await;
        let answered = bot.bridge.sent().into_iter().any(|sent| {
            matches!(
                sent,
                Sent::Edit { post: Post::Question { answered: Some(answer), .. }, .. }
                    if answer == "Thanks, now add a test. — Ada"
            )
        });
        assert!(answered, "{:#?}", bot.bridge.sent());
    }

    async fn a_project_named_in_the_message_is_where_the_thread_works(chat: Chat) {
        let bot = Running::start(chat, "dm.jsonl", HashMap::new()).await;
        let projects = [bot.project("ada", "trip1"), bot.project("team", "trip1")];
        bot.sessions.set_projects(&projects).await;

        // Two projects are named so: the bot asks which.
        let Some(Inbound::Message(mut message)) = chat.message("dm_message.json") else {
            panic!("a direct message is a message");
        };
        message.text = "Trip1: Fix the login bug".into();
        bot.inbound.send(Inbound::Message(message)).await.unwrap();
        let picker = bot
            .posted(&chat.dm(), |post| matches!(post, Post::Picker { .. }))
            .await;
        assert_eq!(
            picker,
            Post::Picker {
                text: "Which project?".into(),
                choices: projects
                    .iter()
                    .map(|project| {
                        let project_id = project.project_id.clone();
                        ("trip1".into(), Action::Pick { project_id })
                    })
                    .collect(),
            }
        );
        assert!(bot.sessions.sessions().await.unwrap().is_empty());

        // The one picked starts with the message, less the project's name.
        let pick = chat.press("press_button.json", chat.ada(), &picker, "trip1");
        bot.send(pick).await;
        bot.posted(&chat.dm(), answer("Fixed **it**.")).await;
        let head = bot.session().await;
        assert_eq!(head.project_id.as_ref(), Some(&projects[0].project_id));
        assert_eq!(
            bot.prompts(&head.session_id).await[0].1,
            "Fix the login bug"
        );
    }

    async fn a_long_answer_posts_its_start_and_the_rest_on_full_answer(chat: Chat) {
        let bot = Running::start(chat, "long.jsonl", HashMap::new()).await;
        bot.send(chat.message("dm_message.json")).await;
        let posted = bot
            .posted(&chat.dm(), |post| matches!(post, Post::Answer { .. }))
            .await;
        let Post::Answer { text, more, .. } = &posted else {
            unreachable!()
        };
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), runner::ANSWER_LINES + 1);
        assert_eq!(lines[0], "Line 1.");
        assert_eq!(lines[runner::ANSWER_LINES - 1], "Line 30.");
        let (_, open) = more.as_ref().unwrap();
        let session_id = bot.session().await.session_id;
        assert_eq!(
            open,
            &format!("herder://session?host=host-1&session={session_id}")
        );
        // Telegram opens no herder:// link.
        if chat == Chat::Slack {
            let rendered = render::render(&posted);
            assert!(
                rendered.to_string().contains("Open in herder"),
                "{rendered:#}"
            );
        }

        bot.send(chat.press("press_button.json", chat.ada(), &posted, "Full answer"))
            .await;
        let sent = bot
            .bridge
            .until(|sent| sent.iter().any(|sent| matches!(sent, Sent::Snippet { .. })))
            .await;
        let rest = (31..=40).map(|i| format!("Line {i}.")).collect::<Vec<_>>();
        assert!(sent.contains(&Sent::Snippet {
            place: chat.dm(),
            title: "Full answer".into(),
            text: rest.join("\n"),
        }));
    }

    async fn stop_interrupts_the_turn(chat: Chat) {
        let bot = Running::start(chat, "stop.jsonl", HashMap::new()).await;
        bot.send(chat.message("dm_message.json")).await;
        let working = bot
            .posted(&chat.dm(), |post| matches!(post, Post::Working { .. }))
            .await;
        // Stop works once the turn runs.
        bot.status(SessionStatus::Running).await;
        bot.send(chat.press("press_button.json", chat.ada(), &working, "Stop"))
            .await;
        bot.posted(&chat.dm(), |post| post == &Post::Text("Stopped.".into()))
            .await;
        bot.status(SessionStatus::Idle).await;
    }

    async fn a_prompt_start_in_the_app_is_mirrored_into_the_thread(chat: Chat) {
        let bot = Running::start(chat, "mirror.jsonl", HashMap::new()).await;
        bot.send(chat.message("dm_message.json")).await;
        bot.posted(&chat.dm(), answer("Fixed it.")).await;
        let session_id = bot.session().await.session_id;

        bot.sessions
            .handle(
                UserId::new("alice"),
                CommandBody::SendPrompt {
                    session_id,
                    text: "Also update the docs.".into(),
                    images: Vec::new(),
                    files: Vec::new(),
                },
            )
            .await
            .unwrap();
        bot.posted(&chat.dm(), |post| {
            post == &Post::Text("Alice (app): Also update the docs.".into())
        })
        .await;
        bot.posted(&chat.dm(), answer("Updated.")).await;
    }

    /// The test bot, working on a project, with Ada asking to be notified.
    fn notifying() -> Bot {
        let mut bot = bot();
        bot.allow[0].notify = true;
        bot.defaults.project_id = Some(ProjectId::new("github.com/ada/trip1"));
        bot
    }

    /// Ada's direct message with the bot.
    const ADA_DM: &str = "D0ADA00001";

    impl Running {
        /// A session of the bot's project that Alice starts in the app, sending it `text`.
        async fn start_in_the_app(&self, text: &str) -> SessionId {
            self.sessions
                .set_projects(&[self.project("ada", "trip1")])
                .await;
            let alice = UserId::new("alice");
            let created = self
                .sessions
                .handle(
                    alice.clone(),
                    CommandBody::CreateSession {
                        repo: None,
                        chat: false,
                        project_id: Some(ProjectId::new("github.com/ada/trip1")),
                        branch: None,
                        account_id: Some(AccountId::new("account-1")),
                        provider: Some(Provider::Other("fake".into())),
                        model: None,
                        permission_mode: None,
                        failover_pin: None,
                    },
                )
                .await
                .unwrap();
            let CommandResult::SessionCreated { session_id } = created else {
                panic!("created as {created:?}");
            };
            let prompt = CommandBody::SendPrompt {
                session_id: session_id.clone(),
                text: text.into(),
                images: Vec::new(),
                files: Vec::new(),
            };
            self.sessions.handle(alice, prompt).await.unwrap();
            session_id
        }

        /// Waits until the bot has messaged Ada a post `matches` picks; returns its thread.
        async fn notified(&self, matches: impl Fn(&Post) -> bool) -> Place {
            let sent = self
                .bridge
                .until(|sent| {
                    sent.iter().any(|sent| {
                        matches!(sent, Sent::Post { place, post, .. }
                            if place.channel_id == ADA_DM && matches(post))
                    })
                })
                .await;
            sent.into_iter()
                .find_map(|sent| match sent {
                    Sent::Post { place, .. } if place.channel_id == ADA_DM => Some(place),
                    _ => None,
                })
                .unwrap()
        }
    }

    /// Every direct message the bot started.
    fn directs(sent: &[Sent]) -> Vec<&Sent> {
        sent.iter()
            .filter(|sent| matches!(sent, Sent::Post { place, .. } if place.channel_id.starts_with('D')))
            .collect()
    }

    #[tokio::test]
    async fn a_session_waiting_in_the_app_messages_those_who_opted_in_once() {
        let bot = Running::with(notifying(), "reply.jsonl", HashMap::new()).await;
        let session_id = bot.start_in_the_app("Fix the login bug").await;
        let place = bot
            .notified(|post| matches!(post, Post::Question { answered: None, .. }))
            .await;

        // While it keeps waiting, a second question notifies no one again.
        bot.events
            .send(Event {
                session_id: session_id.clone(),
                seq: 100,
                at: Timestamp::now(),
                by: None,
                body: EventBody::QuestionAsked {
                    question_id: herder_protocol::QuestionId::new("question-2"),
                    turn_id: TurnId::new("turn-1"),
                    text: "And the docs?".into(),
                    choices: Vec::new(),
                    multi_select: false,
                    routed_to: herder_protocol::Route::User,
                    reason: None,
                },
            })
            .unwrap();

        // Answered in the app, the message says so.
        bot.sessions
            .handle(
                UserId::new("alice"),
                CommandBody::AnswerQuestion {
                    session_id: session_id.clone(),
                    question_id: herder_protocol::QuestionId::new("question-1"),
                    answer: herder_protocol::Answer::Text {
                        text: "Thanks, now add a test.".into(),
                    },
                    images: Vec::new(),
                    files: Vec::new(),
                },
            )
            .await
            .unwrap();
        let sent = bot
            .bridge
            .until(|sent| sent.iter().any(|sent| matches!(sent, Sent::Edit { .. })))
            .await;
        let edited = sent.iter().any(|sent| {
            matches!(sent, Sent::Edit { place: at, id, post: Post::Question { answered: Some(answer), .. } }
                if *at == place && *id == place.thread_id
                    && answer == "Thanks, now add a test. — Alice in the app")
        });
        assert!(edited, "{sent:#?}");
        // Ada got one message, and Bob, who did not opt in, none; the session stays unlinked.
        assert_eq!(directs(&sent).len(), 1, "{sent:#?}");
        assert!(bot.session().await.threads.is_empty());
    }

    #[tokio::test]
    async fn replying_to_a_notification_answers_and_links_the_session() {
        let bot = Running::with(notifying(), "reply.jsonl", HashMap::new()).await;
        let session_id = bot.start_in_the_app("Fix the login bug").await;
        let place = bot
            .notified(|post| matches!(post, Post::Question { .. }))
            .await;

        bot.inbound
            .send(Inbound::Message(super::super::bridge::Message {
                place: place.clone(),
                user_id: ADA.into(),
                direct: true,
                mentioned: false,
                text: "Thanks, now add a test.".into(),
                files: Vec::new(),
            }))
            .await
            .unwrap();
        bot.posted(&place, answer("Will do.")).await;
        let head = bot.session().await;
        assert_eq!(head.session_id, session_id);
        assert_eq!(
            head.threads
                .iter()
                .map(|thread| (thread.channel_id.as_str(), thread.thread_id.as_str()))
                .collect::<Vec<_>>(),
            [(ADA_DM, place.thread_id.as_str())]
        );
        let answered = bot.bridge.sent().into_iter().any(|sent| {
            matches!(sent, Sent::Edit { post: Post::Question { answered: Some(answer), .. }, .. }
                if answer == "Thanks, now add a test. — Ada")
        });
        assert!(answered, "{:#?}", bot.bridge.sent());
    }

    #[tokio::test]
    async fn a_failure_messages_those_who_opted_in_with_retry() {
        let bot = Running::with(notifying(), "failed.jsonl", HashMap::new()).await;
        bot.start_in_the_app("Fix the login bug").await;
        bot.notified(|post| matches!(post, Post::Failure { retry: true, .. }))
            .await;
    }

    async fn images_reach_the_agent_and_what_it_publishes_is_uploaded(chat: Chat) {
        let png = b"\x89PNG\r\n\x1a\npng".to_vec();
        let (place, url) = chat.image();
        let bot = Running::start(
            chat,
            "image.jsonl",
            HashMap::from([(url.to_owned(), png.clone())]),
        )
        .await;
        bot.send(chat.message("dm_image.json")).await;
        bot.posted(&place, answer("The button is cut off.")).await;

        // What the agent publishes is uploaded into the thread as a file, whatever its type,
        // though publish keeps it as a plain file.
        let session_id = bot.session().await.session_id;
        let dir = bot.dir.join("attachments").join(session_id.as_str());
        let publish = |name: &str, data: &[u8], url: Option<&str>| {
            // Each kept in a folder named after its id, as publish keeps it.
            let id = name.split('.').next().unwrap();
            std::fs::create_dir_all(dir.join(id)).unwrap();
            std::fs::write(dir.join(id).join(name), data).unwrap();
            let attachment = Attachment {
                attachment_id: AttachmentId::new(id),
                media_type: FILE_MEDIA_TYPE.into(),
                size: data.len() as u64,
                name: Some(name.into()),
            };
            bot.events
                .send(Event {
                    session_id: session_id.clone(),
                    seq: 100,
                    at: Timestamp::now(),
                    by: None,
                    body: EventBody::ArtifactPublished {
                        title: "Fixed layout".into(),
                        attachment,
                        url: url.map(str::to_owned),
                        expires_at: None,
                    },
                })
                .unwrap();
        };
        let uploaded = |name: &str| {
            let name = name.to_owned();
            move |sent: &[Sent]| {
                sent.iter()
                    .any(|sent| matches!(sent, Sent::Upload { name: n, .. } if *n == name))
            }
        };
        publish("shot.png", &png, None);
        let sent = bot.bridge.until(uploaded("shot.png")).await;
        assert!(sent.contains(&Sent::Upload {
            place: place.clone(),
            name: "shot.png".into(),
            data: png,
        }));
        let pdf = b"%PDF-1.7 report".to_vec();
        publish("report.pdf", &pdf, Some("https://krowk.com/a/art_2"));
        let sent = bot.bridge.until(uploaded("report.pdf")).await;
        assert!(sent.contains(&Sent::Upload {
            place: place.clone(),
            name: "report.pdf".into(),
            data: pdf,
        }));

        // Its link is posted only when the upload fails.
        let link = |sent: &Sent| matches!(sent, Sent::Post { post: Post::Text(text), .. } if text.contains("krowk.com"));
        assert!(!bot.bridge.sent().iter().any(link));
        bot.bridge.refuse_uploads();
        publish(
            "page.html",
            b"<html></html>",
            Some("https://krowk.com/a/art_3"),
        );
        bot.posted(&place, |post| {
            *post == Post::Text("[Fixed layout](https://krowk.com/a/art_3)".into())
        })
        .await;
        assert_eq!(
            bot.bridge.sent().iter().filter(|sent| link(sent)).count(),
            1
        );
    }

    #[tokio::test]
    async fn a_bot_whose_tokens_cannot_be_read_waits_for_the_vault() {
        let tmp = tempfile::tempdir().unwrap();
        let (path, bots) = super::bots(&tmp);
        let (sessions, events) =
            sessions(tmp.path(), &fixtures().join("bots").join("dm.jsonl")).await;
        let shutdown = CancellationToken::new();
        bots.start(
            Runtime {
                host: runner::Host {
                    host_id: HostId::new("host-1"),
                    sessions,
                    names: Arc::new(|_: &UserId| None),
                },
                secrets: Arc::new(HostSecrets::default()),
                events,
                shutdown: shutdown.clone(),
            },
            &[],
        )
        .unwrap();
        for bot in [slack_bot("trash-can"), telegram("tg")] {
            bots.command(CommandBody::PutBot { bot: Box::new(bot) })
                .await
                .unwrap();
        }
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("trash-can")
        );
        let mut states = Vec::new();
        for _ in 0..500 {
            let CommandResult::BotStatus { bots: statuses } =
                bots.command(CommandBody::GetBotStatus).await.unwrap()
            else {
                panic!("not a status");
            };
            states = statuses
                .iter()
                .map(|status| {
                    (
                        status.bot_id.to_string(),
                        status.state,
                        status.last_error.is_some(),
                    )
                })
                .collect();
            if states
                .iter()
                .all(|(_, state, _)| *state == BotState::WaitingForVault)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            states,
            [
                ("trash-can".to_owned(), BotState::WaitingForVault, true),
                ("tg".to_owned(), BotState::WaitingForVault, true),
            ]
        );
        shutdown.cancel();
    }

    /// The usual bot, with `project_id` its default project.
    fn bot_on(project_id: &ProjectId) -> Bot {
        let mut bot = bot();
        bot.defaults.project_id = Some(project_id.clone());
        bot
    }

    #[tokio::test]
    async fn help_lists_the_commands_and_status_what_runs_and_waits() {
        let bot = Running::start(Chat::Slack, "approval.jsonl", HashMap::new()).await;
        bot.send(said("dm_message.json", "help", "1761400900.000100"))
            .await;
        let help = Place {
            thread_id: "1761400900.000100".into(),
            ..dm()
        };
        let posted = bot
            .posted(&help, |post| matches!(post, Post::Text(_)))
            .await;
        let Post::Text(posted) = posted else {
            unreachable!()
        };
        for command in ["**sessions**", "**continue ‹name›**", "**use ‹project›**"] {
            assert!(posted.contains(command), "{posted}");
        }
        // A command starts no session.
        assert!(bot.sessions.sessions().await.unwrap().is_empty());

        let status = Place {
            thread_id: "1761400910.000100".into(),
            ..dm()
        };
        bot.send(said("dm_message.json", "Status", &status.thread_id))
            .await;
        bot.posted(
            &status,
            text("Nothing runs or waits for you on this machine."),
        )
        .await;

        // A session waits for an approval: status says so.
        bot.send(slack("dm_message.json")).await;
        bot.posted(&dm(), |post| matches!(post, Post::Approval { .. }))
            .await;
        bot.status(SessionStatus::NeedsYou).await;
        let again = Place {
            thread_id: "1761400920.000100".into(),
            ..dm()
        };
        bot.send(said("dm_message.json", "status", &again.thread_id))
            .await;
        bot.posted(
            &again,
            text("**Waiting for you** (1)\n• Untitled session · chat · waiting for you"),
        )
        .await;
    }

    #[tokio::test]
    async fn sessions_lists_the_bots_sessions_and_continue_here_links_the_thread() {
        let trip1 = ProjectId::new("github.com/ada/trip1");
        let bot = Running::with(bot_on(&trip1), "continue.jsonl", HashMap::new()).await;
        let projects = [bot.project("ada", "trip1"), bot.project("team", "other")];
        bot.sessions.set_projects(&projects).await;
        let login = bot.started(&trip1, "Fix the login bug").await;
        // Another project's sessions are not the bot's.
        bot.started(&projects[1].project_id, "Fix the team bug")
            .await;

        bot.send(said("dm_message.json", "sessions", &dm().thread_id))
            .await;
        let list = bot
            .posted(&dm(), |post| matches!(post, Post::Sessions { .. }))
            .await;
        assert_eq!(
            list,
            Post::Sessions {
                text: "Recent sessions, those waiting for you first:".into(),
                sessions: vec![(login.clone(), "Fix the login bug · trip1".into())],
            }
        );
        let rendered = render::render(&list).to_string();
        assert!(rendered.contains("Continue here"), "{rendered}");

        bot.send(press_at(&dm(), ADA, &list, "Continue here")).await;
        bot.posted(
            &dm(),
            text("This thread continues **Fix the login bug**. Reply here to prompt it."),
        )
        .await;
        let heads = bot.sessions.sessions().await.unwrap();
        let head = heads.iter().find(|head| head.session_id == login).unwrap();
        assert_eq!(head.threads.len(), 1);
        assert_eq!(head.threads[0].thread_id, dm().thread_id);

        // The thread's replies are the session's prompts now.
        bot.send(slack("dm_reply.json")).await;
        bot.posted(&dm(), answer("Added one.")).await;
        assert_eq!(
            bot.prompts(&login).await,
            [(
                Some(UserId::new("via trash-can, Ada")),
                "Thanks, now add a test.".to_owned()
            )]
        );
    }

    #[tokio::test]
    async fn continue_finds_a_session_by_title_or_offers_a_picker() {
        let trip1 = ProjectId::new("github.com/ada/trip1");
        let bot = Running::with(bot_on(&trip1), "continue.jsonl", HashMap::new()).await;
        bot.sessions
            .set_projects(&[bot.project("ada", "trip1")])
            .await;
        let login = bot.started(&trip1, "Fix the login bug").await;
        let logout = bot.started(&trip1, "Fix the logout bug").await;
        let thread = |thread_id: &str| Place {
            thread_id: thread_id.into(),
            ..dm()
        };

        bot.send(said("dm_message.json", "continue nothing", "1761401000.1"))
            .await;
        bot.posted(&thread("1761401000.1"), |post| {
            matches!(post, Post::Text(text) if text.starts_with("No session in my projects"))
        })
        .await;

        // Several titles match: the bot asks which, newest first.
        bot.send(said("dm_message.json", "continue FIX", "1761401010.1"))
            .await;
        let picker = bot
            .posted(&thread("1761401010.1"), |post| {
                matches!(post, Post::Picker { .. })
            })
            .await;
        assert_eq!(
            picker,
            Post::Picker {
                text: "Which session?".into(),
                choices: vec![
                    (
                        "Fix the logout bug".into(),
                        Action::Continue {
                            session_id: logout.clone()
                        }
                    ),
                    (
                        "Fix the login bug".into(),
                        Action::Continue {
                            session_id: login.clone()
                        }
                    ),
                ],
            }
        );
        bot.send(press_at(
            &thread("1761401010.1"),
            ADA,
            &picker,
            "Fix the logout bug",
        ))
        .await;
        let linked = "This thread continues **Fix the logout bug**. Reply here to prompt it.";
        bot.posted(&thread("1761401010.1"), text(linked)).await;

        // One matches: the thread continues it at once.
        bot.send(said(
            "dm_message.json",
            "continue login bug",
            &dm().thread_id,
        ))
        .await;
        let linked = "This thread continues **Fix the login bug**. Reply here to prompt it.";
        bot.posted(&dm(), text(linked)).await;
        bot.send(slack("dm_reply.json")).await;
        bot.posted(&dm(), answer("Added one.")).await;
        assert_eq!(bot.prompts(&login).await.len(), 1);
        assert!(bot.prompts(&logout).await.is_empty());
    }

    #[tokio::test]
    async fn use_binds_the_channel_and_list_bots_shows_it() {
        let bot = Running::start(Chat::Slack, "channel.jsonl", HashMap::new()).await;
        let projects = [bot.project("ada", "trip1"), bot.project("team", "trip1")];
        bot.sessions.set_projects(&projects).await;
        let at = |thread_id: &str| Place {
            thread_id: thread_id.into(),
            ..channel()
        };

        // A direct message has no channel to bind.
        let direct = Place {
            thread_id: "1761401100.1".into(),
            ..dm()
        };
        bot.send(said("dm_message.json", "use trip1", &direct.thread_id))
            .await;
        bot.posted(
            &direct,
            text("**use** binds a channel: mention me with it in one."),
        )
        .await;

        bot.send(said("channel_mention.json", "use nowhere", "1761401110.1"))
            .await;
        bot.posted(
            &at("1761401110.1"),
            text("No project is named \"nowhere\"."),
        )
        .await;

        // Two projects are named trip1: the bot asks which, by id.
        bot.send(said("channel_mention.json", "use trip1", "1761401120.1"))
            .await;
        let picker = bot
            .posted(&at("1761401120.1"), |post| {
                matches!(post, Post::Picker { .. })
            })
            .await;
        bot.send(press_at(
            &at("1761401120.1"),
            ADA,
            &picker,
            "github.com/team/trip1",
        ))
        .await;
        bot.posted(
            &at("1761401120.1"),
            text("New threads in this channel work on **trip1**."),
        )
        .await;
        let bound = vec![ChannelBinding {
            channel_id: channel().channel_id,
            project_id: projects[1].project_id.clone(),
        }];
        assert_eq!(bot.listed().await[0].channel_bindings, bound);

        // The channel's new threads work on it.
        bot.send(slack("channel_mention.json")).await;
        bot.posted(&channel(), answer("It answers.")).await;
        let head = bot.session().await;
        assert_eq!(head.project_id.as_ref(), Some(&projects[1].project_id));

        bot.send(said("channel_mention.json", "use none", "1761401130.1"))
            .await;
        bot.posted(
            &at("1761401130.1"),
            text("This channel is bound to no project now."),
        )
        .await;
        assert_eq!(bot.listed().await[0].channel_bindings, []);
    }

    #[test]
    fn only_a_whole_command_is_one() {
        use super::super::runner::Command;
        assert_eq!(Command::parse(" Help "), Some(Command::Help));
        assert_eq!(Command::parse("use trip1"), Some(Command::Use("trip1")));
        assert_eq!(
            Command::parse("continue the login bug"),
            Some(Command::Continue("the login bug"))
        );
        for prompt in [
            "help me fix this",
            "use the new API",
            "continue",
            "statuses",
        ] {
            assert_eq!(Command::parse(prompt), None, "{prompt}");
        }
    }

    /// One post of every kind, long ones among them.
    fn every_post() -> Vec<Post> {
        use herder_protocol::ApprovalId;

        let session_id = SessionId::new("s1");
        let long = (1..=200)
            .map(|i| format!("Line {i} of a long answer."))
            .collect::<Vec<_>>();
        let posts = [
            Post::Working {
                session_id: session_id.clone(),
                status: "working…".into(),
            },
            Post::Text("Hello **there**".into()),
            Post::Answer {
                session_id: session_id.clone(),
                text: long.join("\n"),
                more: Some((
                    ItemId::new("i1"),
                    "herder://session?host=h&session=s1".into(),
                )),
            },
            Post::Approval {
                session_id: session_id.clone(),
                approval_id: ApprovalId::new("a1"),
                summary: "Run cargo test".into(),
                resolved: None,
            },
            Post::Approval {
                session_id: session_id.clone(),
                approval_id: ApprovalId::new("a1"),
                summary: "Run cargo test".into(),
                resolved: Some((ApprovalOutcome::Deny, "Ada".into())),
            },
            Post::Question {
                session_id: session_id.clone(),
                question_id: herder_protocol::QuestionId::new("q1"),
                text: "Which checks?".into(),
                choices: (1..=12).map(|i| format!("Check {i}")).collect(),
                multi_select: true,
                answered: None,
            },
            Post::Question {
                session_id: session_id.clone(),
                question_id: herder_protocol::QuestionId::new("q1"),
                text: "Which one?".into(),
                choices: vec!["A".into(), "B".into()],
                multi_select: false,
                answered: None,
            },
            Post::Failure {
                session_id: session_id.clone(),
                text: "The CLI crashed.".into(),
                retry: true,
                open: "herder://session?host=h&session=s1".into(),
            },
            Post::Picker {
                text: "Which project?".into(),
                choices: vec![(
                    "trip1".into(),
                    Action::Pick {
                        project_id: ProjectId::new("github.com/a/trip1"),
                    },
                )],
            },
            Post::Sessions {
                text: "Recent sessions, those waiting for you first:".into(),
                sessions: vec![(session_id.clone(), "Fix the login bug · trip1".into())],
            },
        ];
        posts.into()
    }

    #[test]
    fn every_post_renders_as_slack_message_content() {
        use slack_morphism::prelude::SlackMessageContent;

        for post in every_post() {
            let rendered = render::render(&post);
            let content: SlackMessageContent = serde_json::from_value(rendered.clone())
                .unwrap_or_else(|err| panic!("{err}: {rendered:#}"));
            for block in content.blocks.unwrap() {
                let block = serde_json::to_value(block).unwrap();
                if let Some(text) = block["text"]["text"].as_str() {
                    assert!(text.chars().count() <= 3000, "{post:?}");
                }
            }
        }
    }

    #[test]
    fn every_post_renders_as_telegram_messages() {
        for post in every_post() {
            let rendered = telegram::render::render(&post, &Keys::default());
            assert!(!rendered.messages.is_empty(), "{post:?}");
            for message in &rendered.messages {
                // Telegram takes 4096 characters, once its HTML is read.
                assert!(message.chars().count() <= 4096, "{post:?}");
            }
            for button in rendered
                .keyboard
                .iter()
                .flat_map(|k| k.inline_keyboard.iter().flatten())
            {
                let data = button.callback_data.as_deref().unwrap();
                assert!(data.len() <= 64, "{data}");
            }
        }
        // A long answer is split between lines, into messages that each read as a whole.
        let Post::Answer { text, .. } = &every_post()[2] else {
            panic!("the third post is an answer");
        };
        let long = text.repeat(4);
        let rendered = telegram::render::render(&Post::Text(long.clone()), &Keys::default());
        assert!(rendered.messages.len() > 1);
        assert_eq!(rendered.messages.join("\n"), long);
    }
}
