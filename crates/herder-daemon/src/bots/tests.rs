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

    use herder_adapters::fake::FakeAdapter;
    use herder_protocol::{
        ApprovalOutcome, Attachment, AttachmentId, EventBody, HostId, ItemBody, SessionStatus,
        TurnId, UserId,
    };
    use herder_store::Store;
    use serde_json::Value;
    use slack_morphism::prelude::{SlackInteractionEvent, SlackPushEventCallback};
    use tokio::sync::mpsc;

    use super::super::bridge::{Inbound, Place, Post};
    use super::super::fake::{FakeBridge, Sent};
    use super::super::runner::{self, Runner};
    use super::super::slack::{payload, render};
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
        async fn start(script: &str, files: HashMap<String, Vec<u8>>) -> Self {
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
            let runner = Runner::new(bot(), Arc::clone(&bridge) as _, host);
            let (inbound, received) = mpsc::channel(16);
            let stop = CancellationToken::new();
            tokio::spawn(runner.run(received, events.subscribe(), stop.clone()));
            Self {
                sessions,
                bridge,
                inbound,
                events,
                dir,
                stop,
                _tmp: tmp,
            }
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

    #[tokio::test]
    async fn a_direct_message_starts_a_session_and_a_reply_continues_it() {
        let bot = Running::start("dm.jsonl", HashMap::new()).await;
        bot.send(slack("dm_message.json")).await;

        // Only the final answer is posted, as mrkdwn; the working message goes.
        let posted = bot.posted(&dm(), answer("Fixed **it**.")).await;
        assert_eq!(render::render(&posted)["text"], "Fixed *it*.");
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
            place: dm(),
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
        assert_eq!(head.threads[0].channel_id, "D0ADA00001");
        assert_eq!(head.threads[0].thread_id, "1761400000.000100");
        assert!(head.threads[0].url.is_some());

        bot.send(slack("dm_reply.json")).await;
        bot.posted(&dm(), answer("Added one.")).await;
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

    #[tokio::test]
    async fn in_a_channel_thread_only_mentions_are_prompts() {
        let bot = Running::start("channel.jsonl", HashMap::new()).await;
        bot.send(slack("channel_mention.json")).await;
        bot.posted(&channel(), answer("It answers.")).await;

        // A reply without a mention is the people's own talk: Slack sends it as a channel
        // message, which the bot drops, and the runner ignores it should it arrive.
        assert_eq!(slack("channel_reply.json"), None);
        let Some(Inbound::Message(mut unmentioned)) = slack("channel_mention_reply.json") else {
            panic!("a mention is a message");
        };
        unmentioned.mentioned = false;
        unmentioned.text = "Looks good to me".into();
        bot.inbound
            .send(Inbound::Message(unmentioned))
            .await
            .unwrap();

        bot.send(slack("channel_mention_reply.json")).await;
        bot.posted(&channel(), answer("With fakes.")).await;
        let head = bot.session().await;
        let prompts: Vec<_> = bot
            .prompts(&head.session_id)
            .await
            .into_iter()
            .map(|(_, text)| text)
            .collect();
        assert_eq!(prompts, ["What does the bot do?", "and how is it tested?"]);
    }

    #[tokio::test]
    async fn people_off_the_allowlist_get_one_note_and_reach_nothing() {
        let bot = Running::start("dm.jsonl", HashMap::new()).await;
        // Bots, the bot itself among them, are never heard.
        assert_eq!(slack("dm_bot.json"), None);
        assert_eq!(slack("dm_own.json"), None);

        bot.send(slack("dm_stranger.json")).await;
        bot.send(slack("dm_stranger.json")).await;
        let place = Place {
            channel_id: "D0EVE00001".into(),
            thread_id: "1761400120.000300".into(),
        };
        let sent = bot.bridge.until(|sent| !sent.is_empty()).await;
        // Ada's message, sent after, is taken: the stranger's were handled before it.
        bot.send(slack("dm_message.json")).await;
        bot.posted(&dm(), answer("Fixed **it**.")).await;
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
        assert_eq!((at, user_id.as_str()), (&place, EVE));
        assert_eq!(bot.sessions.sessions().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn only_people_with_approve_answer_approvals() {
        let bot = Running::start("approval.jsonl", HashMap::new()).await;
        bot.send(slack("dm_message.json")).await;
        let approval = bot
            .posted(&dm(), |post| matches!(post, Post::Approval { .. }))
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
        bot.send(press("press_button.json", BOB, &approval, "Allow"))
            .await;
        bot.bridge
            .until(|sent| {
                sent.iter()
                    .any(|sent| matches!(sent, Sent::Ephemeral { user_id, .. } if user_id == BOB))
            })
            .await;
        assert_eq!(bot.session().await.status, SessionStatus::NeedsYou);

        bot.send(press("press_button.json", ADA, &approval, "Allow"))
            .await;
        bot.posted(&dm(), answer("All green.")).await;
        let edited = bot.bridge.sent().into_iter().any(|sent| {
            matches!(
                sent,
                Sent::Edit { post: Post::Approval { resolved: Some((ApprovalOutcome::Allow, by)), .. }, .. }
                    if by == "Ada"
            )
        });
        assert!(edited, "{:#?}", bot.bridge.sent());
    }

    #[tokio::test]
    async fn a_multi_select_question_is_answered_with_the_ticked_choices() {
        let bot = Running::start("question.jsonl", HashMap::new()).await;
        bot.send(slack("dm_message.json")).await;
        let question = bot
            .posted(&dm(), |post| matches!(post, Post::Question { .. }))
            .await;
        // Ticking a box is no answer yet.
        assert_eq!(
            payload::interaction(serde_json::from_value(recorded("press_tick.json")).unwrap()),
            None
        );
        bot.send(press("press_submit.json", ADA, &question, "Submit"))
            .await;
        bot.posted(&dm(), answer("Set up.")).await;
    }

    #[tokio::test]
    async fn a_reply_answers_the_question_the_session_waits_on() {
        let bot = Running::start("reply.jsonl", HashMap::new()).await;
        bot.send(slack("dm_message.json")).await;
        bot.posted(&dm(), |post| matches!(post, Post::Question { .. }))
            .await;
        bot.send(slack("dm_reply.json")).await;
        bot.posted(&dm(), answer("Will do.")).await;
        let answered = bot.bridge.sent().into_iter().any(|sent| {
            matches!(
                sent,
                Sent::Edit { post: Post::Question { answered: Some(answer), .. }, .. }
                    if answer == "Thanks, now add a test. — Ada"
            )
        });
        assert!(answered, "{:#?}", bot.bridge.sent());
    }

    #[tokio::test]
    async fn a_project_named_in_the_message_is_where_the_thread_works() {
        let bot = Running::start("dm.jsonl", HashMap::new()).await;
        let project = |id: &str| {
            let repo = bot.dir.join(id);
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
                project_id: ProjectId::new(format!("github.com/{id}/trip1")),
                name: "trip1".into(),
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
        };
        let projects = [project("ada"), project("team")];
        bot.sessions.set_projects(&projects).await;

        // Two projects are named so: the bot asks which.
        let Some(Inbound::Message(mut message)) = slack("dm_message.json") else {
            panic!("a direct message is a message");
        };
        message.text = "Trip1: Fix the login bug".into();
        bot.inbound.send(Inbound::Message(message)).await.unwrap();
        let picker = bot
            .posted(&dm(), |post| matches!(post, Post::Picker { .. }))
            .await;
        assert_eq!(
            picker,
            Post::Picker {
                text: "Which project?".into(),
                projects: projects
                    .iter()
                    .map(|project| (project.project_id.clone(), "trip1".into()))
                    .collect(),
            }
        );
        assert!(bot.sessions.sessions().await.unwrap().is_empty());

        // The one picked starts with the message, less the project's name.
        let pick = press("press_button.json", ADA, &picker, "trip1");
        bot.send(pick).await;
        bot.posted(&dm(), answer("Fixed **it**.")).await;
        let head = bot.session().await;
        assert_eq!(head.project_id.as_ref(), Some(&projects[0].project_id));
        assert_eq!(
            bot.prompts(&head.session_id).await[0].1,
            "Fix the login bug"
        );
    }

    #[tokio::test]
    async fn a_long_answer_posts_its_start_and_the_rest_on_full_answer() {
        let bot = Running::start("long.jsonl", HashMap::new()).await;
        bot.send(slack("dm_message.json")).await;
        let posted = bot
            .posted(&dm(), |post| matches!(post, Post::Answer { .. }))
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
        let rendered = render::render(&posted);
        assert!(
            rendered.to_string().contains("Open in herder"),
            "{rendered:#}"
        );

        bot.send(press("press_button.json", ADA, &posted, "Full answer"))
            .await;
        let sent = bot
            .bridge
            .until(|sent| sent.iter().any(|sent| matches!(sent, Sent::Snippet { .. })))
            .await;
        let rest = (31..=40).map(|i| format!("Line {i}.")).collect::<Vec<_>>();
        assert!(sent.contains(&Sent::Snippet {
            place: dm(),
            title: "Full answer".into(),
            text: rest.join("\n"),
        }));
    }

    #[tokio::test]
    async fn stop_interrupts_the_turn() {
        let bot = Running::start("stop.jsonl", HashMap::new()).await;
        bot.send(slack("dm_message.json")).await;
        let working = bot
            .posted(&dm(), |post| matches!(post, Post::Working { .. }))
            .await;
        // Stop works once the turn runs.
        bot.status(SessionStatus::Running).await;
        bot.send(press("press_button.json", ADA, &working, "Stop"))
            .await;
        bot.posted(&dm(), |post| post == &Post::Text("Stopped.".into()))
            .await;
        bot.status(SessionStatus::Idle).await;
    }

    #[tokio::test]
    async fn a_prompt_from_the_app_is_mirrored_into_the_thread() {
        let bot = Running::start("mirror.jsonl", HashMap::new()).await;
        bot.send(slack("dm_message.json")).await;
        bot.posted(&dm(), answer("Fixed it.")).await;
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
        bot.posted(&dm(), |post| {
            post == &Post::Text("Alice (app): Also update the docs.".into())
        })
        .await;
        bot.posted(&dm(), answer("Updated.")).await;
    }

    #[tokio::test]
    async fn images_reach_the_agent_and_the_agents_images_are_uploaded() {
        let png = b"\x89PNG\r\n\x1a\npng".to_vec();
        let url = "https://files.slack.com/files-pri/T0HERDER01-F0SCREEN01/download/screen.png";
        let bot = Running::start(
            "image.jsonl",
            HashMap::from([(url.to_owned(), png.clone())]),
        )
        .await;
        bot.send(slack("dm_image.json")).await;
        let place = Place {
            channel_id: "D0ADA00001".into(),
            thread_id: "1761400300.000600".into(),
        };
        bot.posted(&place, answer("The button is cut off.")).await;

        // The agent publishes a screenshot: it is uploaded into the thread.
        let session_id = bot.session().await.session_id;
        let attachment = Attachment {
            attachment_id: AttachmentId::new("shot"),
            media_type: "image/png".into(),
            size: png.len() as u64,
            name: None,
        };
        let dir = bot.dir.join("attachments").join(session_id.as_str());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("shot.png"), &png).unwrap();
        bot.events
            .send(Event {
                session_id,
                seq: 100,
                at: Timestamp::now(),
                by: None,
                body: EventBody::ArtifactPublished {
                    title: "Fixed layout".into(),
                    attachment,
                    url: None,
                    expires_at: None,
                },
            })
            .unwrap();
        let sent = bot
            .bridge
            .until(|sent| sent.iter().any(|sent| matches!(sent, Sent::Upload { .. })))
            .await;
        assert!(sent.contains(&Sent::Upload {
            place,
            name: "Fixed layout.png".into(),
            data: png,
        }));
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
            if states[0].1 == BotState::WaitingForVault {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            states,
            [
                ("trash-can".to_owned(), BotState::WaitingForVault, true),
                ("tg".to_owned(), BotState::Unsupported, false),
            ]
        );
        shutdown.cancel();
    }

    #[test]
    fn every_post_renders_as_slack_message_content() {
        use herder_protocol::ApprovalId;
        use slack_morphism::prelude::SlackMessageContent;

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
                projects: vec![(ProjectId::new("github.com/a/trip1"), "trip1".into())],
            },
        ];
        for post in posts {
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
}
