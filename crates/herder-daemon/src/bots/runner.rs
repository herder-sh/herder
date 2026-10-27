//! One connected bot: what reaches its sessions from its chat, and what it posts back.
//!
//! A thread is one session. A direct message, or a mention in a channel, outside a linked
//! thread starts a session and links the thread to it; in a linked thread every direct message
//! is a prompt, and in a channel only those that mention the bot. Only people the bot's
//! allowlist names drive sessions, each where their switches say, and only those with Approve
//! answer approval requests; anyone else gets one note that only they see, and nothing reaches
//! the session.
//!
//! The bot posts quietly: one "working…" message with Stop while a session works, edited as its
//! status changes; then each turn's final answer, trimmed past [`ANSWER_LINES`] lines with
//! Full answer and Open in herder; approval requests and questions as buttons; failures with
//! what fixes them; and images the agent publishes. Tool calls, reasoning and children are
//! never posted. Prompts sent from elsewhere, such as the app, post into the session's threads
//! as "‹name› (app): …", and their answers go to every thread; a thread's own prompts are
//! answered there only.
//!
//! People whose allowlist entry has `notify` get a direct message when a session of the bot's,
//! linked to it or in one of its projects, waits for them or fails, unless they follow it in a
//! direct message already. It carries the same buttons as a thread, and replying in its thread,
//! or pressing one, links that thread to the session. A session that keeps waiting is notified
//! of once; the message is edited when it is answered, saying where.
//!
//! A mention in a channel carries what was said before it, with the files, as far as the
//! prompt has room for them: in a thread, from the thread's start the first time, then what
//! was said since the bot's last mention there, at most [`THREAD_MESSAGES`] messages; outside
//! one, the channel's latest messages ([`RECENT_MESSAGES`](super::bridge::RECENT_MESSAGES) of
//! the last two hours).
//!
//! Outside a linked thread, a message that is one of the bot's commands ([`Command`]) is
//! answered by the bot itself: `sessions`, `continue ‹name›`, `use ‹project›`, `status` and
//! `help`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use herder_protocol::{
    Answer, Bot, BotPlace, BotUser, Bytes, CommandBody, CommandResult, ErrorClass, ErrorCode,
    ErrorInfo, Event, EventBody, HostId, IMAGE_MEDIA_TYPES, Image, ItemBody, ItemId,
    MAX_IMAGE_BYTES, MAX_PROMPT_ATTACHMENT_BYTES, ProjectId, PromptFile, QuestionId, Route,
    SessionHead, SessionId, SessionOrigin, SessionStatus, ThreadLink, TurnError, UserId,
};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::Bindings;
use super::bridge::{
    Action, Bridge, ChatFile, Earlier, HistoryError, Inbound, Message, Place, Post, Press,
};
use crate::session::SessionManager;

/// How many lines of an answer are posted before Full answer.
pub const ANSWER_LINES: usize = 30;

/// The most sessions `sessions` lists, and the most a `continue` picker offers.
pub const SESSIONS: usize = 10;

/// The most messages of a thread a mention carries: the first, and the latest.
pub const THREAD_MESSAGES: usize = 50;

/// What `help` answers.
const HELP: &str = "**sessions**: recent sessions in my projects, and those waiting for you, \
                    to continue in a thread\n\
                    **continue ‹name›**: continue the session whose title has ‹name›\n\
                    **use ‹project›**: start this channel's new threads on a project; \
                    **use none** clears it\n\
                    **status**: what runs and what waits for you on this machine\n\
                    **help**: these commands\n\
                    Anything else starts a session, on the project it names first as \
                    \"‹project›: …\" if it names one.";

/// The name of the herder user `by` names, for "‹name› (app)"; `None` for an unknown one.
pub type Names = Arc<dyn Fn(&UserId) -> Option<String> + Send + Sync>;

/// What a bot's runner drives.
#[derive(Clone)]
pub struct Host {
    /// This host, for Open in herder links.
    pub host_id: HostId,
    /// Its sessions.
    pub sessions: SessionManager,
    /// The names of its users.
    pub names: Names,
}

/// A bot driving the host's sessions over its chat's bridge.
pub struct Runner {
    bot: Bot,
    bridge: Arc<dyn Bridge>,
    host: Host,
    /// Where `use` changes the bot's channel bindings.
    bindings: Bindings,
    /// The session of each thread linked to it.
    links: HashMap<Place, SessionId>,
    /// The threads of this bot linked to each session, in the order linked.
    threads: HashMap<SessionId, Vec<Place>>,
    /// The latest status of each linked session.
    statuses: HashMap<SessionId, SessionStatus>,
    /// Where each linked session's running turn answers.
    turns: HashMap<SessionId, Vec<Place>>,
    /// The "working…" message of each session in each thread, with what it says.
    working: HashMap<SessionId, Vec<(Place, String, &'static str)>>,
    /// The latest final-answer candidate of each session's running turn.
    answers: HashMap<SessionId, (ItemId, String)>,
    /// The threads of the prompts the bot sent each session that have not run yet, in order.
    sent: HashMap<SessionId, VecDeque<Place>>,
    /// The question each session waits on, which a message in its thread answers.
    questions: HashMap<SessionId, QuestionId>,
    /// Approval requests and questions posted, by id, to edit once answered.
    asks: HashMap<String, Vec<(Place, String, Post)>>,
    /// Messages that named an ambiguous project, waiting for a pick, by thread.
    picking: HashMap<Place, Message>,
    /// Who has been told, in which thread, that they may not.
    noted: HashSet<(String, Place)>,
    /// The direct-message thread of each notification nobody has acted in yet, and its
    /// session; a reply or press there links it.
    notices: HashMap<Place, SessionId>,
    /// The notification of each waiting session: the approval request or question it is
    /// about, and its direct messages, each with its post.
    waiting: HashMap<SessionId, (String, Vec<(Place, Post)>)>,
    /// Each person's direct message with the bot, by their user id, once known.
    directs: HashMap<String, String>,
    /// The last message the bot read of each channel thread it was mentioned in.
    seen: HashMap<Place, String>,
    /// The threads the bot has said it cannot read.
    unreadable: HashSet<Place>,
}

impl Runner {
    pub fn new(bot: Bot, bridge: Arc<dyn Bridge>, host: Host, bindings: Bindings) -> Self {
        Self {
            bot,
            bridge,
            host,
            bindings,
            links: HashMap::new(),
            threads: HashMap::new(),
            statuses: HashMap::new(),
            turns: HashMap::new(),
            working: HashMap::new(),
            answers: HashMap::new(),
            sent: HashMap::new(),
            questions: HashMap::new(),
            asks: HashMap::new(),
            picking: HashMap::new(),
            noted: HashSet::new(),
            notices: HashMap::new(),
            waiting: HashMap::new(),
            directs: HashMap::new(),
            seen: HashMap::new(),
            unreadable: HashSet::new(),
        }
    }

    /// Runs until `stop`, or until the bridge closes `inbound`. `events` must be subscribed
    /// before the call, so no thread linked meanwhile is missed.
    pub async fn run(
        mut self,
        mut inbound: mpsc::Receiver<Inbound>,
        mut events: broadcast::Receiver<Event>,
        stop: CancellationToken,
    ) {
        match self.host.sessions.sessions().await {
            Ok(heads) => {
                for head in heads {
                    let bot_id = self.bot.bot_id.clone();
                    for thread in head.threads.iter().filter(|t| t.bot_id == bot_id) {
                        self.statuses.insert(head.session_id.clone(), head.status);
                        self.add_link(&head.session_id, place_of(thread));
                    }
                }
            }
            Err(err) => {
                warn!(bot_id = %self.bot.bot_id, "cannot list the linked sessions: {err:#}")
            }
        }
        loop {
            tokio::select! {
                () = stop.cancelled() => return,
                received = inbound.recv() => match received {
                    Some(Inbound::Message(message)) => self.message(message).await,
                    Some(Inbound::Press(press)) => self.press(press).await,
                    None => return,
                },
                event = events.recv() => match event {
                    Ok(event) => self.event(event).await,
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        warn!(bot_id = %self.bot.bot_id, missed, "the bot missed session events");
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                },
            }
        }
    }

    // Inbound.

    async fn message(&mut self, message: Message) {
        if !message.direct && !message.mentioned {
            return;
        }
        let Some(user) = self.driver(&message.user_id, message.direct) else {
            self.refuse(&message.place, &message.user_id).await;
            return;
        };
        if message.direct {
            self.directs
                .insert(message.user_id.clone(), message.place.channel_id.clone());
        }
        self.adopt(&message.place).await;
        let by = self.by(&user);
        if let Some(session_id) = self.links.get(&message.place).cloned() {
            match self.questions.get(&session_id).cloned() {
                Some(question_id) => {
                    self.reply(&session_id, question_id, &message, by).await;
                }
                None => {
                    let context = self.context(&message).await;
                    self.prompt(
                        &session_id,
                        &message.place,
                        by,
                        &message.text,
                        &message.files,
                        context,
                    )
                    .await;
                }
            }
            return;
        }
        if let Some(command) = Command::parse(&message.text) {
            self.command(command, &message).await;
            return;
        }
        let (project_id, text) = match self.project_for(&message) {
            Pick::Project(project_id, text) => (Some(project_id), text),
            Pick::Chat => (None, message.text.clone()),
            Pick::Ambiguous(projects, text) => {
                let choices = projects
                    .into_iter()
                    .map(|(project_id, name)| (name, Action::Pick { project_id }))
                    .collect();
                let post = Post::Picker {
                    text: "Which project?".into(),
                    choices,
                };
                self.post(&message.place, &post).await;
                self.picking
                    .insert(message.place.clone(), Message { text, ..message });
                return;
            }
        };
        let context = self.context(&message).await;
        self.start(project_id, &message, by, &text, context).await;
    }

    async fn press(&mut self, press: Press) {
        // Buttons act only on the bot's own sessions, and those it notified of.
        if press.action.session_id().is_some_and(|session_id| {
            !self.threads.contains_key(session_id)
                && self.notices.get(&press.place) != Some(session_id)
        }) {
            return;
        }
        let user = self.user(&press.user_id).cloned();
        let allowed = user.as_ref().is_some_and(|user| match &press.action {
            Action::Approve { .. } => user.approve,
            _ if press.direct => user.dm,
            _ => user.channels,
        });
        let Some(user) = user.filter(|_| allowed) else {
            self.refuse(&press.place, &press.user_id).await;
            return;
        };
        self.adopt(&press.place).await;
        let by = self.by(&user);
        let command = match press.action {
            Action::Stop { session_id } => CommandBody::Interrupt { session_id },
            Action::Approve {
                session_id,
                approval_id,
                decision,
            } => CommandBody::AnswerApproval {
                session_id,
                approval_id,
                decision,
            },
            Action::Choose {
                session_id,
                question_id,
                index,
            } => CommandBody::AnswerQuestion {
                session_id,
                question_id,
                answer: Answer::Choice { index },
                images: Vec::new(),
                files: Vec::new(),
            },
            Action::ChooseMany { indices, .. } if indices.is_empty() => {
                let text = "Tick at least one choice, then Submit.";
                self.note(&press.place, &press.user_id, text).await;
                return;
            }
            Action::ChooseMany {
                session_id,
                question_id,
                indices,
            } => CommandBody::AnswerQuestion {
                session_id,
                question_id,
                answer: Answer::Choices { indices },
                images: Vec::new(),
                files: Vec::new(),
            },
            Action::FullAnswer {
                session_id,
                item_id,
            } => {
                self.full_answer(&press.place, &session_id, &item_id).await;
                return;
            }
            Action::Retry { session_id } => {
                let prompt = self.read_last(&session_id, |body| match body {
                    ItemBody::UserMessage { text, .. } => Some(text),
                    _ => None,
                });
                let Some(text) = prompt.await else {
                    let text = "There is no prompt to send again.";
                    self.note(&press.place, &press.user_id, text).await;
                    return;
                };
                self.prompt(
                    &session_id,
                    &press.place,
                    by,
                    &text,
                    &[],
                    Context::default(),
                )
                .await;
                return;
            }
            Action::Pick { project_id } => {
                if let Some(message) = self.picking.remove(&press.place) {
                    let context = self.context(&message).await;
                    self.start(Some(project_id), &message, by, &message.text, context)
                        .await;
                }
                return;
            }
            Action::Continue { session_id } => {
                self.continue_session(&session_id, &press.place, press.direct, &press.user_id)
                    .await;
                return;
            }
            Action::Use { project_id } => {
                if !press.direct {
                    let name = self.project_name(Some(&project_id));
                    self.bind(&press.place, Some((project_id, name))).await;
                }
                return;
            }
        };
        if let Err(err) = self.host.sessions.handle(by, command).await {
            self.note(&press.place, &press.user_id, &err.message).await;
        }
    }

    // Commands.

    /// Answers `command`, which `message` gave.
    async fn command(&mut self, command: Command<'_>, message: &Message) {
        let place = &message.place;
        let post = match command {
            Command::Help => Some(Post::Text(HELP.into())),
            Command::Status => Some(Post::Text(self.status().await)),
            Command::Sessions => {
                let heads = self.scoped().await;
                Some(if heads.is_empty() {
                    Post::Text("No sessions in my projects to continue.".into())
                } else {
                    Post::Sessions {
                        text: "Recent sessions, those waiting for you first:".into(),
                        sessions: heads
                            .iter()
                            .map(|head| (head.session_id.clone(), self.line(head)))
                            .collect(),
                    }
                })
            }
            Command::Continue(name) => {
                let needle = name.to_lowercase();
                let heads = self.scoped().await;
                let titled = |head: &&SessionHead| {
                    head.title
                        .as_ref()
                        .is_some_and(|title| title.to_lowercase().contains(&needle))
                };
                let found: Vec<_> = heads.iter().filter(titled).collect();
                match found.as_slice() {
                    [] => {
                        let text = format!(
                            "No session in my projects has \"{name}\" in its title. \
                             **sessions** lists them."
                        );
                        Some(Post::Text(text))
                    }
                    [head] => {
                        let session_id = head.session_id.clone();
                        self.continue_session(&session_id, place, message.direct, &message.user_id)
                            .await;
                        None
                    }
                    several => {
                        let choices = several
                            .iter()
                            .map(|head| {
                                let session_id = head.session_id.clone();
                                (title(head).to_owned(), Action::Continue { session_id })
                            })
                            .collect();
                        Some(Post::Picker {
                            text: "Which session?".into(),
                            choices,
                        })
                    }
                }
            }
            Command::Use(_) if message.direct => {
                let text = "**use** binds a channel: mention me with it in one.";
                Some(Post::Text(text.into()))
            }
            Command::Use(name) if name.eq_ignore_ascii_case("none") => {
                self.bind(place, None).await;
                None
            }
            Command::Use(name) => {
                let named: Vec<_> = self
                    .host
                    .sessions
                    .project_list()
                    .into_iter()
                    .filter(|project| {
                        project.name.eq_ignore_ascii_case(name)
                            || project.project_id.as_str() == name
                    })
                    .collect();
                match <[_; 1]>::try_from(named) {
                    Ok([project]) => {
                        self.bind(place, Some((project.project_id, project.name)))
                            .await;
                        None
                    }
                    Err(named) if named.is_empty() => {
                        let text = format!("No project is named \"{name}\".");
                        Some(Post::Text(text))
                    }
                    // The names are the same: the buttons tell the projects apart by id.
                    Err(named) => {
                        let choices = named
                            .into_iter()
                            .map(|project| {
                                let label = project.project_id.to_string();
                                let project_id = project.project_id;
                                (label, Action::Use { project_id })
                            })
                            .collect();
                        Some(Post::Picker {
                            text: "Which project?".into(),
                            choices,
                        })
                    }
                }
            }
        };
        if let Some(post) = post {
            self.post(place, &post).await;
        }
    }

    /// What `status` answers: the sessions of this host that run, and those that wait for
    /// someone.
    async fn status(&self) -> String {
        let heads = match self.host.sessions.sessions().await {
            Ok(heads) => heads,
            Err(err) => return format!("Cannot list the sessions: {err:#}"),
        };
        let (mut running, mut waiting) = (Vec::new(), Vec::new());
        for head in heads.iter().filter(|head| head.parent.is_none()) {
            match head.status {
                SessionStatus::Running | SessionStatus::WaitingForCapacity => {
                    running.push(head);
                }
                SessionStatus::NeedsYou | SessionStatus::NeedsLogin => waiting.push(head),
                _ => {}
            }
        }
        if running.is_empty() && waiting.is_empty() {
            return "Nothing runs or waits for you on this machine.".into();
        }
        let mut lines = Vec::new();
        for (heading, heads) in [("Running", running), ("Waiting for you", waiting)] {
            if heads.is_empty() {
                continue;
            }
            lines.push(format!("**{heading}** ({})", heads.len()));
            lines.extend(heads.iter().map(|head| format!("• {}", self.line(head))));
        }
        lines.join("\n")
    }

    /// The sessions `sessions` lists and `continue` finds: those of the bot's projects, and
    /// those linked to its threads; waiting ones first, then the newest; at most
    /// [`SESSIONS`].
    async fn scoped(&self) -> Vec<SessionHead> {
        let mut heads: Vec<_> = match self.host.sessions.sessions().await {
            Ok(heads) => heads
                .into_iter()
                .filter(|head| self.in_scope(head))
                .collect(),
            Err(err) => {
                warn!(bot_id = %self.bot.bot_id, "cannot list the sessions: {err:#}");
                Vec::new()
            }
        };
        // Session ids are ULIDs: the newest sorts last.
        heads.sort_by(|a, b| {
            let waits = |head: &SessionHead| head.status == SessionStatus::NeedsYou;
            waits(b)
                .cmp(&waits(a))
                .then_with(|| b.session_id.as_str().cmp(a.session_id.as_str()))
        });
        heads.truncate(SESSIONS);
        heads
    }

    /// Whether `head` is a session the bot's threads may continue.
    fn in_scope(&self, head: &SessionHead) -> bool {
        let live = !matches!(head.status, SessionStatus::Archived | SessionStatus::Moved);
        let ours = head
            .project_id
            .as_ref()
            .is_some_and(|project_id| self.projects().contains(&project_id))
            || head
                .threads
                .iter()
                .any(|thread| thread.bot_id == self.bot.bot_id);
        live && head.parent.is_none() && ours
    }

    /// The bot's projects: its default one, and those its channels are bound to.
    fn projects(&self) -> Vec<&ProjectId> {
        let bound = self.bot.channel_bindings.iter().map(|b| &b.project_id);
        self.bot.defaults.project_id.iter().chain(bound).collect()
    }

    /// Links `place` to `session_id`, at `user_id`'s asking, so its prompts continue it.
    async fn continue_session(
        &mut self,
        session_id: &SessionId,
        place: &Place,
        direct: bool,
        user_id: &str,
    ) {
        if let Some(linked) = self.links.get(place) {
            let text = if linked == session_id {
                "This thread continues that session already."
            } else {
                "This thread continues another session already. Start a new thread for this one."
            };
            self.note(place, user_id, text).await;
            return;
        }
        let head = match self.host.sessions.sessions().await {
            Ok(heads) => heads
                .into_iter()
                .find(|head| head.session_id == *session_id && self.in_scope(head)),
            Err(err) => {
                warn!(bot_id = %self.bot.bot_id, "cannot list the sessions: {err:#}");
                None
            }
        };
        let Some(head) = head else {
            self.note(place, user_id, "That session cannot be continued here.")
                .await;
            return;
        };
        self.statuses.insert(session_id.clone(), head.status);
        self.link(session_id, place).await;
        let how = if direct {
            "Reply here to prompt it."
        } else {
            "Mention me here to prompt it."
        };
        let text = format!("This thread continues **{}**. {how}", title(&head));
        self.post(place, &Post::Text(text)).await;
    }

    /// Binds the channel of `place` to `project`, by id and name, or to none, and says so
    /// there.
    async fn bind(&mut self, place: &Place, project: Option<(ProjectId, String)>) {
        let project_id = project.as_ref().map(|(project_id, _)| project_id.clone());
        let text = match self.bindings.bind(&place.channel_id, project_id).await {
            Ok(bindings) => {
                self.bot.channel_bindings = bindings;
                match project {
                    Some((_, name)) => format!("New threads in this channel work on **{name}**."),
                    None => "This channel is bound to no project now.".into(),
                }
            }
            Err(err) => format!("Cannot bind this channel: {}", err.message),
        };
        self.post(place, &Post::Text(text)).await;
    }

    /// `head` as a line of a list: its title, its project, and what it waits on.
    fn line(&self, head: &SessionHead) -> String {
        let project = self.project_name(head.project_id.as_ref());
        let status = match head.status {
            SessionStatus::NeedsYou => " · waiting for you",
            SessionStatus::NeedsLogin => " · needs signing in",
            SessionStatus::Running => " · running",
            _ => "",
        };
        format!("{} · {project}{status}", title(head))
    }

    /// The name of the project `project_id`; "chat" for none.
    fn project_name(&self, project_id: Option<&ProjectId>) -> String {
        let Some(project_id) = project_id else {
            return "chat".into();
        };
        self.host
            .sessions
            .project_list()
            .into_iter()
            .find(|project| project.project_id == *project_id)
            .map_or_else(|| project_id.to_string(), |project| project.name)
    }

    /// Starts a session on `project_id`, or a chat, for `message`, links its place to it and
    /// sends it `text`.
    async fn start(
        &mut self,
        project_id: Option<ProjectId>,
        message: &Message,
        by: UserId,
        text: &str,
        context: Context,
    ) {
        let place = &message.place;
        let origin = self.origin(message).await;
        let defaults = &self.bot.defaults;
        let command = CommandBody::CreateSession {
            repo: None,
            chat: project_id.is_none(),
            project_id,
            branch: None,
            account_id: defaults.account_ids.first().cloned(),
            provider: defaults.provider.clone(),
            model: defaults.model.clone(),
            permission_mode: defaults.permission_mode,
            failover_pin: None,
        };
        let started = self
            .host
            .sessions
            .handle_from(by.clone(), command, Some(origin));
        let session_id = match started.await {
            Ok(CommandResult::SessionCreated { session_id }) => session_id,
            Ok(other) => {
                warn!(bot_id = %self.bot.bot_id, "creating a session answered {other:?}");
                return;
            }
            Err(err) => {
                warn!(bot_id = %self.bot.bot_id, "cannot start a session: {}", err.message);
                let post = Post::NotStarted {
                    text: self.not_started(&err),
                    open: format!("herder://host?host={}", self.host.host_id),
                };
                self.post(place, &post).await;
                return;
            }
        };
        self.link(&session_id, place).await;
        self.prompt(&session_id, place, by, text, &message.files, context)
            .await;
    }

    /// The origin of a session `message` starts: who wrote it, and where.
    async fn origin(&self, message: &Message) -> SessionOrigin {
        let place = if message.direct {
            BotPlace::Direct
        } else {
            let name = self.bridge.channel_name(&message.place).await;
            BotPlace::Channel {
                name: name.unwrap_or_else(|| message.place.channel_id.clone()),
            }
        };
        SessionOrigin {
            bot_id: self.bot.bot_id.clone(),
            kind: self.bot.kind,
            by: self
                .user(&message.user_id)
                .map_or_else(|| message.user_id.clone(), |user| user.display_name.clone()),
            place,
        }
    }

    /// Links `place` to `session_id`.
    async fn link(&mut self, session_id: &SessionId, place: &Place) {
        let thread = ThreadLink {
            kind: self.bot.kind,
            bot_id: self.bot.bot_id.clone(),
            channel_id: place.channel_id.clone(),
            thread_id: place.thread_id.clone(),
            url: self.bridge.link(place).await,
        };
        if let Err(err) = self.host.sessions.link_thread(session_id, thread).await {
            warn!(%session_id, "cannot link a thread: {}", err.message);
        }
        self.add_link(session_id, place.clone());
    }

    /// Links the thread of a notification someone acts in to its session.
    async fn adopt(&mut self, place: &Place) {
        if let Some(session_id) = self.notices.remove(place) {
            self.link(&session_id, place).await;
        }
    }

    /// Sends `text` with `files` to `session_id` as `by`, from `place`, after what `context`
    /// holds of the thread.
    async fn prompt(
        &mut self,
        session_id: &SessionId,
        place: &Place,
        by: UserId,
        text: &str,
        files: &[ChatFile],
        context: Context,
    ) {
        let Some((mut images, mut files)) = self.download(place, files).await else {
            return;
        };
        let text = self
            .with_context(text, context, &mut images, &mut files)
            .await;
        let command = CommandBody::SendPrompt {
            session_id: session_id.clone(),
            text,
            images,
            files,
        };
        self.sent
            .entry(session_id.clone())
            .or_default()
            .push_back(place.clone());
        if let Err(err) = self.host.sessions.handle(by, command).await {
            if let Some(sent) = self.sent.get_mut(session_id) {
                sent.pop_back();
            }
            self.post(place, &Post::Text(err.message)).await;
            return;
        }
        if !self.has_working(session_id, place) {
            let status = self.statuses.get(session_id).copied();
            let status = status.and_then(working_status).unwrap_or("working…");
            self.post_working(session_id, place, status).await;
        }
    }

    /// Answers the question `question_id` of `session_id` with `message`, as `by`.
    async fn reply(
        &mut self,
        session_id: &SessionId,
        question_id: QuestionId,
        message: &Message,
        by: UserId,
    ) {
        let Some((images, files)) = self.download(&message.place, &message.files).await else {
            return;
        };
        let command = CommandBody::AnswerQuestion {
            session_id: session_id.clone(),
            question_id,
            answer: Answer::Text {
                text: message.text.clone(),
            },
            images,
            files,
        };
        if let Err(err) = self.host.sessions.handle(by, command).await {
            self.post(&message.place, &Post::Text(err.message)).await;
        }
    }

    /// The images and other files of `files`, downloaded; `None`, once said in `place`, when
    /// one cannot be.
    async fn download(
        &self,
        place: &Place,
        files: &[ChatFile],
    ) -> Option<(Vec<Image>, Vec<PromptFile>)> {
        let mut images = Vec::new();
        let mut prompt_files = Vec::new();
        for file in files {
            match self.bridge.download(file).await {
                Ok(data) => attach(file, data, &mut images, &mut prompt_files),
                Err(err) => {
                    let text = format!("Cannot read {}: {err}", file.name);
                    self.post(place, &Post::Text(text)).await;
                    return None;
                }
            }
        }
        Some((images, prompt_files))
    }

    /// What was said before `message`, a mention in a channel: in its thread, since the bot
    /// last read it, or from its start; for one that starts a thread, the channel's latest
    /// messages. A direct message has none: the bot sees every one.
    async fn context(&mut self, message: &Message) -> Context {
        let place = &message.place;
        if message.direct {
            return Context::default();
        }
        let starts = place.thread_id == message.message_id;
        let after = self.seen.get(place).cloned();
        let read = if starts {
            self.bridge.recent(place, &message.message_id)
        } else {
            self.bridge
                .history(place, after.as_deref(), &message.message_id)
        };
        let mut earlier = match read.await {
            Ok(earlier) => earlier,
            Err(HistoryError::Unreadable(text)) => {
                if self.unreadable.insert(place.clone()) {
                    self.post(place, &Post::Text(text)).await;
                }
                return Context::default();
            }
            Err(HistoryError::Failed(err)) => {
                warn!(bot_id = %self.bot.bot_id, "cannot read a channel: {err}");
                return Context::default();
            }
        };
        self.seen.insert(place.clone(), message.message_id.clone());
        let cut = earlier.len() > THREAD_MESSAGES;
        if cut {
            earlier.drain(1..=earlier.len() - THREAD_MESSAGES);
        }
        let mut context = Context {
            heading: if starts {
                "Earlier in this channel:"
            } else {
                "Earlier in this thread:"
            },
            cut,
            ..Context::default()
        };
        for Earlier {
            user_id,
            name,
            text,
            files,
            ..
        } in earlier
        {
            let who = match self.user(&user_id) {
                Some(user) => user.display_name.clone(),
                None => name.unwrap_or(user_id),
            };
            let mut line = format!("{who}: {text}");
            if !files.is_empty() {
                let names: Vec<_> = files.iter().map(|file| file.name.as_str()).collect();
                line.push_str(&format!(" [attached: {}]", names.join(", ")));
            }
            context.lines.push(line);
            context.files.extend(files);
        }
        context
    }

    /// `text` after what `context` holds, whose files go into `images` and `files` as far as
    /// [`MAX_PROMPT_ATTACHMENT_BYTES`] leaves room, the latest first.
    async fn with_context(
        &self,
        text: &str,
        context: Context,
        images: &mut Vec<Image>,
        files: &mut Vec<PromptFile>,
    ) -> String {
        if context.lines.is_empty() {
            return text.to_owned();
        }
        let taken: usize = images.iter().map(|image| image.data.0.len()).sum::<usize>()
            + files.iter().map(|file| file.data.0.len()).sum::<usize>();
        let mut room = MAX_PROMPT_ATTACHMENT_BYTES.saturating_sub(taken);
        let (mut earlier_images, mut earlier_files) = (Vec::new(), Vec::new());
        let (mut unread, mut left_out) = (Vec::new(), Vec::new());
        for file in context.files.iter().rev() {
            match self.bridge.download(file).await {
                Ok(data) if data.len() <= room => {
                    room -= data.len();
                    attach(file, data, &mut earlier_images, &mut earlier_files);
                }
                Ok(_) => left_out.push(file.name.as_str()),
                Err(err) => unread.push(format!("{}: {err}", file.name)),
            }
        }
        earlier_images.reverse();
        earlier_images.append(images);
        *images = earlier_images;
        earlier_files.reverse();
        earlier_files.append(files);
        *files = earlier_files;
        let mut lines = vec![context.heading.to_owned()];
        if context.cut {
            lines.push(format!(
                "(It is longer: only the first of these messages and the last {} are here.)",
                THREAD_MESSAGES - 1
            ));
        }
        lines.extend(context.lines);
        if !left_out.is_empty() {
            left_out.reverse();
            lines.push(format!("(Left out, for size: {}.)", left_out.join(", ")));
        }
        if !unread.is_empty() {
            lines.push(format!("(Cannot read {}.)", unread.join("; ")));
        }
        format!("{}\n\n{text}", lines.join("\n"))
    }

    /// Posts the rest of the answer `item_id` of `session_id`, past its first lines.
    async fn full_answer(&mut self, place: &Place, session_id: &SessionId, item_id: &ItemId) {
        let mut after = 0;
        let text = loop {
            let events = match self.host.sessions.read_since(session_id, after, 512).await {
                Ok(events) => events,
                Err(err) => {
                    warn!(%session_id, "cannot read the answer: {err:#}");
                    return;
                }
            };
            let Some(last) = events.last() else { return };
            after = last.seq;
            let found = events.into_iter().find_map(|event| match event.body {
                EventBody::ItemAdded { item } if item.id == *item_id => match item.body {
                    ItemBody::AssistantMessage { text } => Some(text),
                    _ => None,
                },
                _ => None,
            });
            if let Some(text) = found {
                break text;
            }
        };
        if let Some((_, rest)) = trim(&text)
            && let Err(err) = self.bridge.snippet(place, "Full answer", &rest).await
        {
            warn!(bot_id = %self.bot.bot_id, "cannot post a snippet: {err}");
        }
    }

    /// What `read` makes of the last top-level item of `session_id` it reads anything of.
    async fn read_last(
        &self,
        session_id: &SessionId,
        read: impl Fn(ItemBody) -> Option<String>,
    ) -> Option<String> {
        let mut after = 0;
        let mut found = None;
        loop {
            let events = match self.host.sessions.read_since(session_id, after, 512).await {
                Ok(events) => events,
                Err(err) => {
                    warn!(%session_id, "cannot read the session: {err:#}");
                    return None;
                }
            };
            let Some(last) = events.last() else {
                return found;
            };
            after = last.seq;
            for event in events {
                if let EventBody::ItemAdded { item } = event.body
                    && item.parent_call_id.is_none()
                    && let Some(text) = read(item.body)
                {
                    found = Some(text);
                }
            }
        }
    }

    // Session events.

    async fn event(&mut self, event: Event) {
        if let EventBody::ThreadLinked { thread } = &event.body {
            if thread.bot_id == self.bot.bot_id {
                self.add_link(&event.session_id, place_of(thread));
            }
            return;
        }
        self.follow(&event).await;
        let session_id = event.session_id;
        if !self.threads.contains_key(&session_id) {
            return;
        }
        match event.body {
            EventBody::ItemAdded { item } if item.parent_call_id.is_none() => match item.body {
                ItemBody::UserMessage { text, .. } => {
                    self.turn_started(&session_id, event.by.as_ref(), text)
                        .await;
                }
                ItemBody::AssistantMessage { text } => {
                    self.answers.insert(session_id, (item.id, text));
                }
                _ => {}
            },
            EventBody::SessionStatusChanged { status, .. } => {
                self.statuses.insert(session_id.clone(), status);
                let Some(text) = working_status(status) else {
                    return;
                };
                let post = Post::Working {
                    session_id: session_id.clone(),
                    status: text.into(),
                };
                let mut working = self.working.remove(&session_id).unwrap_or_default();
                for (place, message_id, says) in &mut working {
                    if *says != text {
                        self.edit(place, message_id, &post).await;
                        *says = text;
                    }
                }
                self.working.insert(session_id, working);
            }
            body @ (EventBody::ApprovalRequested { .. } | EventBody::QuestionAsked { .. }) => {
                if let Some((id, post)) = asked(&session_id, &body) {
                    self.ask(&session_id, id, post).await;
                }
            }
            body @ (EventBody::ApprovalResolved { .. } | EventBody::QuestionAnswered { .. }) => {
                let by = self.who(event.by.as_ref());
                if let Some(id) = answer_of(&body) {
                    self.answered(id, |post| resolve(post, &body, &by)).await;
                }
            }
            EventBody::TurnCompleted { .. } => {
                let post = match self.answers.remove(&session_id) {
                    Some((item_id, text)) => match trim(&text) {
                        Some((start, _)) => Post::Answer {
                            session_id: session_id.clone(),
                            text: start,
                            more: Some((item_id, self.open(&session_id))),
                        },
                        None => Post::Answer {
                            session_id: session_id.clone(),
                            text,
                            more: None,
                        },
                    },
                    None => Post::Text("Done.".into()),
                };
                self.turn_ended(&session_id, post).await;
            }
            EventBody::TurnInterrupted { .. } => {
                self.answers.remove(&session_id);
                self.turn_ended(&session_id, Post::Text("Stopped.".into()))
                    .await;
            }
            EventBody::TurnFailed { error, .. } => {
                self.answers.remove(&session_id);
                let post = self.failure(&session_id, &error).await;
                self.turn_ended(&session_id, post).await;
            }
            EventBody::ArtifactPublished {
                title,
                attachment,
                url,
                ..
            } => {
                let data = match self
                    .host
                    .sessions
                    .attachment_data(&session_id, &attachment)
                    .await
                {
                    Ok(data) => data.0,
                    Err(err) => {
                        warn!(%session_id, "cannot read a published file: {}", err.message);
                        return;
                    }
                };
                // Its stored media type says only that it is a file: the bridge tells an image
                // from the rest by its name.
                let name = attachment.name.clone().unwrap_or_else(|| title.clone());
                for place in self.answer_places(&session_id) {
                    let Err(err) = self.bridge.upload(&place, &name, data.clone()).await else {
                        continue;
                    };
                    warn!(bot_id = %self.bot.bot_id, "cannot upload a published file: {err}");
                    if let Some(url) = &url {
                        self.post(&place, &Post::Text(format!("[{title}]({url})")))
                            .await;
                    }
                }
            }
            _ => {}
        }
    }

    /// What every session's events tell the bot, linked to it or not: the question each waits
    /// on, and when one waits for or fails the people it notifies.
    async fn follow(&mut self, event: &Event) {
        let session_id = &event.session_id;
        match &event.body {
            body @ (EventBody::ApprovalRequested { .. } | EventBody::QuestionAsked { .. }) => {
                let Some((id, post)) = asked(session_id, body) else {
                    return;
                };
                if let Post::Question { question_id, .. } = &post {
                    self.questions
                        .insert(session_id.clone(), question_id.clone());
                }
                // One notification while the session keeps waiting.
                if self.waiting.contains_key(session_id) {
                    return;
                }
                let posted = self.notify(session_id, &post).await;
                if !posted.is_empty() {
                    self.waiting.insert(session_id.clone(), (id, posted));
                }
            }
            body @ (EventBody::ApprovalResolved { .. } | EventBody::QuestionAnswered { .. }) => {
                if matches!(body, EventBody::QuestionAnswered { .. }) {
                    self.questions.remove(session_id);
                }
                let id = answer_of(body);
                if self
                    .waiting
                    .get(session_id)
                    .is_none_or(|(asked, _)| Some(asked.as_str()) != id)
                {
                    return;
                }
                let Some((_, posted)) = self.waiting.remove(session_id) else {
                    return;
                };
                let mut by = self.who(event.by.as_ref());
                if event.by.as_ref().is_some_and(|by| !self.ours(by)) {
                    by.push_str(" in the app");
                }
                for (place, mut post) in posted {
                    resolve(&mut post, body, &by);
                    self.edit(&place, &place.thread_id, &post).await;
                }
            }
            EventBody::TurnCompleted { .. } | EventBody::TurnInterrupted { .. } => {
                self.questions.remove(session_id);
            }
            EventBody::TurnFailed { error, .. } => {
                self.questions.remove(session_id);
                let post = self.failure(session_id, error).await;
                self.notify(session_id, &post).await;
            }
            _ => {}
        }
    }

    /// Messages `post` to each person the bot notifies, in a new thread of their direct message
    /// with it, when `session_id` is one of the bot's and they do not follow it in a direct
    /// message already; returns the threads, each with its post.
    async fn notify(&mut self, session_id: &SessionId, post: &Post) -> Vec<(Place, Post)> {
        let people: Vec<String> = self
            .bot
            .allow
            .iter()
            .filter(|user| user.notify)
            .map(|user| user.external_user_id.clone())
            .collect();
        if people.is_empty() || !self.concerns(session_id).await {
            return Vec::new();
        }
        let threads = self.threads.get(session_id).cloned().unwrap_or_default();
        let mut posted = Vec::new();
        for user_id in people {
            let following = self.directs.get(&user_id).is_some_and(|channel_id| {
                threads.iter().any(|place| place.channel_id == *channel_id)
            });
            if following {
                continue;
            }
            match self.bridge.direct(&user_id, post).await {
                Ok(place) => {
                    self.directs.insert(user_id, place.channel_id.clone());
                    self.notices.insert(place.clone(), session_id.clone());
                    posted.push((place, post.clone()));
                }
                Err(err) => {
                    warn!(bot_id = %self.bot.bot_id, "cannot message a person directly: {err}");
                }
            }
        }
        posted
    }

    /// Whether `session_id` is one of the bot's: linked to it, or in one of its projects.
    async fn concerns(&self, session_id: &SessionId) -> bool {
        if self.threads.contains_key(session_id) {
            return true;
        }
        let projects = self.projects();
        if projects.is_empty() {
            return false;
        }
        match self.host.sessions.sessions().await {
            Ok(heads) => heads.iter().any(|head| {
                head.session_id == *session_id
                    && head
                        .project_id
                        .as_ref()
                        .is_some_and(|project_id| projects.contains(&project_id))
            }),
            Err(err) => {
                warn!(bot_id = %self.bot.bot_id, "cannot list the sessions: {err:#}");
                false
            }
        }
    }

    /// A turn of `session_id` started with the prompt `text` by `by`.
    async fn turn_started(&mut self, session_id: &SessionId, by: Option<&UserId>, text: String) {
        let ours = by.is_some_and(|by| self.ours(by));
        let sent = if ours {
            self.sent.get_mut(session_id).and_then(VecDeque::pop_front)
        } else {
            None
        };
        let places = match sent {
            Some(place) => vec![place],
            None => {
                let places = self.threads.get(session_id).cloned().unwrap_or_default();
                if let Some(by) = by.filter(|_| !ours) {
                    let name = (self.host.names)(by).unwrap_or_else(|| "someone".into());
                    let mirror = Post::Text(format!("{name} (app): {text}"));
                    for place in &places {
                        self.post(place, &mirror).await;
                    }
                }
                places
            }
        };
        self.answers.remove(session_id);
        for place in &places {
            if !self.has_working(session_id, place) {
                self.post_working(session_id, place, "working…").await;
            }
        }
        self.turns.insert(session_id.clone(), places);
    }

    /// The running turn of `session_id` ended: its "working…" messages go, and `post` takes
    /// their place.
    async fn turn_ended(&mut self, session_id: &SessionId, post: Post) {
        let places = self.answer_places(session_id);
        self.turns.remove(session_id);
        let working = self.working.remove(session_id).unwrap_or_default();
        let (done, waiting): (Vec<_>, Vec<_>) = working
            .into_iter()
            .partition(|(place, _, _)| places.contains(place));
        if !waiting.is_empty() {
            self.working.insert(session_id.clone(), waiting);
        }
        for (place, message_id, _) in done {
            if let Err(err) = self.bridge.delete(&place, &message_id).await {
                warn!(bot_id = %self.bot.bot_id, "cannot delete a message: {err}");
            }
        }
        for place in &places {
            self.post(place, &post).await;
        }
    }

    /// Posts an approval request or question, `id`, where `session_id` answers.
    async fn ask(&mut self, session_id: &SessionId, id: String, post: Post) {
        let mut posted = Vec::new();
        for place in self.answer_places(session_id) {
            if let Some(message_id) = self.post(&place, &post).await {
                posted.push((place, message_id, post.clone()));
            }
        }
        self.asks.insert(id, posted);
    }

    /// Edits the posts of the approval request or question `id` as `answer` changes them.
    async fn answered(&mut self, id: &str, answer: impl Fn(&mut Post)) {
        for (place, message_id, mut post) in self.asks.remove(id).unwrap_or_default() {
            answer(&mut post);
            self.edit(&place, &message_id, &post).await;
        }
    }

    /// What a failed turn of `session_id` posts.
    async fn failure(&self, session_id: &SessionId, error: &TurnError) -> Post {
        let account = match self.host.sessions.sessions().await {
            Ok(heads) => heads
                .into_iter()
                .find(|head| head.session_id == *session_id)
                .map(|head| head.account_id.to_string()),
            Err(_) => None,
        }
        .unwrap_or_else(|| "its account".into());
        let (text, retry) = match error.class {
            ErrorClass::Auth => (
                format!(
                    "The account {account} needs signing in again on this machine. Prompts \
                     sent meanwhile wait, and run once it is signed in."
                ),
                false,
            ),
            ErrorClass::LimitReached => (
                format!(
                    "The account {account} hit its usage limit. {}",
                    error.message
                ),
                true,
            ),
            ErrorClass::Transient | ErrorClass::Fatal => {
                (first_sentence(&error.message).to_owned(), true)
            }
        };
        Post::Failure {
            session_id: session_id.clone(),
            text,
            retry,
            open: self.open(session_id),
        }
    }

    /// Why a session could not start, as `err` says, in plain words with what fixes it.
    fn not_started(&self, err: &ErrorInfo) -> String {
        let signed_in = self
            .host
            .sessions
            .accounts()
            .iter()
            .any(|account| account.needs_login.is_none());
        if !signed_in {
            "No signed-in account on this machine: an owner can sign one in under Providers in \
             the herder app."
                .into()
        } else if err.code == ErrorCode::Conflict {
            "Every account on this machine is at its usage limit: try again once one resets, or \
             an owner can add one under Providers in the herder app."
                .into()
        } else {
            "A session cannot start on this machine as this bot is set up: an owner can check \
             its accounts, projects and bots in the herder app."
                .into()
        }
    }

    // Helpers.

    fn add_link(&mut self, session_id: &SessionId, place: Place) {
        let threads = self.threads.entry(session_id.clone()).or_default();
        if !threads.contains(&place) {
            threads.push(place.clone());
        }
        self.links.insert(place, session_id.clone());
    }

    /// Where `session_id`'s answers go: its running turn's thread, else every thread of it.
    fn answer_places(&self, session_id: &SessionId) -> Vec<Place> {
        self.turns
            .get(session_id)
            .or_else(|| self.threads.get(session_id))
            .cloned()
            .unwrap_or_default()
    }

    fn has_working(&self, session_id: &SessionId, place: &Place) -> bool {
        self.working
            .get(session_id)
            .is_some_and(|working| working.iter().any(|(at, _, _)| at == place))
    }

    async fn post_working(&mut self, session_id: &SessionId, place: &Place, status: &'static str) {
        let post = Post::Working {
            session_id: session_id.clone(),
            status: status.into(),
        };
        if let Some(message_id) = self.post(place, &post).await {
            self.working.entry(session_id.clone()).or_default().push((
                place.clone(),
                message_id,
                status,
            ));
        }
    }

    async fn post(&self, place: &Place, post: &Post) -> Option<String> {
        match self.bridge.post(place, post).await {
            Ok(message_id) => Some(message_id),
            Err(err) => {
                warn!(bot_id = %self.bot.bot_id, "cannot post: {err}");
                None
            }
        }
    }

    async fn edit(&self, place: &Place, message_id: &str, post: &Post) {
        if let Err(err) = self.bridge.edit(place, message_id, post).await {
            warn!(bot_id = %self.bot.bot_id, "cannot edit a message: {err}");
        }
    }

    async fn note(&self, place: &Place, user_id: &str, text: &str) {
        if let Err(err) = self.bridge.ephemeral(place, user_id, text).await {
            warn!(bot_id = %self.bot.bot_id, "cannot post a note: {err}");
        }
    }

    /// Tells `user_id`, once per thread, that the bot does not take this from them.
    async fn refuse(&mut self, place: &Place, user_id: &str) {
        if self.noted.insert((user_id.to_owned(), place.clone())) {
            let text = format!(
                "{} does not take this from you. Ask the owner of its herder to allow you.",
                self.bot.name
            );
            self.note(place, user_id, &text).await;
        }
    }

    fn user(&self, user_id: &str) -> Option<&BotUser> {
        self.bot
            .allow
            .iter()
            .find(|user| user.external_user_id == user_id)
    }

    /// The allowlisted person `user_id`, if they may drive sessions in a direct message or a
    /// channel, as `direct` says.
    fn driver(&self, user_id: &str, direct: bool) -> Option<BotUser> {
        self.user(user_id)
            .filter(|user| if direct { user.dm } else { user.channels })
            .cloned()
    }

    /// The user the journal records a person's actions as: "via ‹bot›, ‹name›".
    fn by(&self, user: &BotUser) -> UserId {
        UserId::new(format!("via {}, {}", self.bot.name, user.display_name))
    }

    /// Whether `by` is one of the bot's people, acting through it.
    fn ours(&self, by: &UserId) -> bool {
        self.bot.allow.iter().any(|user| self.by(user) == *by)
    }

    /// Who did something, for the bot's posts.
    fn who(&self, by: Option<&UserId>) -> String {
        let Some(by) = by else {
            return "herder".into();
        };
        match self.bot.allow.iter().find(|user| self.by(user) == *by) {
            Some(user) => user.display_name.clone(),
            None => (self.host.names)(by).unwrap_or_else(|| "someone".into()),
        }
    }

    /// The link that opens `session_id` in the herder app.
    fn open(&self, session_id: &SessionId) -> String {
        format!(
            "herder://session?host={}&session={session_id}",
            self.host.host_id
        )
    }

    /// The project a new thread starts on, from the channel's binding, a project named at the
    /// start of `message` as "‹name›: …", or the bot's default.
    fn project_for(&self, message: &Message) -> Pick {
        if !message.direct
            && let Some(binding) = self
                .bot
                .channel_bindings
                .iter()
                .find(|binding| binding.channel_id == message.place.channel_id)
        {
            return Pick::Project(binding.project_id.clone(), message.text.clone());
        }
        if let Some((name, rest)) = message.text.split_once(':')
            && !name.trim().is_empty()
            && !name.contains('\n')
        {
            let name = name.trim();
            let named: Vec<_> = self
                .host
                .sessions
                .project_list()
                .into_iter()
                .filter(|project| project.name.eq_ignore_ascii_case(name))
                .map(|project| (project.project_id, project.name))
                .collect();
            let rest = rest.trim().to_owned();
            match named.len() {
                0 => {}
                1 => return Pick::Project(named[0].0.clone(), rest),
                _ => return Pick::Ambiguous(named, rest),
            }
        }
        match &self.bot.defaults.project_id {
            Some(project_id) => Pick::Project(project_id.clone(), message.text.clone()),
            None => Pick::Chat,
        }
    }
}

/// One of the bot's commands, which a message outside a linked thread may be.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Command<'a> {
    Help,
    Status,
    Sessions,
    /// Continue the session whose title has this.
    Continue(&'a str),
    /// Bind the channel to the project of this name, or to none.
    Use(&'a str),
}

impl<'a> Command<'a> {
    /// The command `text` is; `None` for a prompt. `use` takes one word, so a prompt such as
    /// "use the new API" stays one.
    pub(super) fn parse(text: &'a str) -> Option<Self> {
        let text = text.trim();
        let (word, rest) = text
            .split_once(char::is_whitespace)
            .map_or((text, ""), |(word, rest)| (word, rest.trim()));
        match word.to_ascii_lowercase().as_str() {
            "help" if rest.is_empty() => Some(Self::Help),
            "status" if rest.is_empty() => Some(Self::Status),
            "sessions" if rest.is_empty() => Some(Self::Sessions),
            "continue" if !rest.is_empty() => Some(Self::Continue(rest)),
            "use" if !rest.is_empty() && !rest.contains(char::is_whitespace) => {
                Some(Self::Use(rest))
            }
            _ => None,
        }
    }
}

/// What a thread said before a mention, which its prompt carries.
#[derive(Debug, Default)]
struct Context {
    /// What it is, for the prompt: "Earlier in this thread:" or "… channel:".
    heading: &'static str,
    /// Its messages, oldest first, each "‹who›: ‹text›".
    lines: Vec<String>,
    /// Their files, oldest first.
    files: Vec<ChatFile>,
    /// Whether messages between the first and the latest were left out.
    cut: bool,
}

/// `data`, downloaded of `file`, as one of a prompt's `images` when it is one the agent sees,
/// else as one of its `files`.
fn attach(file: &ChatFile, data: Vec<u8>, images: &mut Vec<Image>, files: &mut Vec<PromptFile>) {
    if IMAGE_MEDIA_TYPES.contains(&file.media_type.as_str()) && data.len() <= MAX_IMAGE_BYTES {
        images.push(Image {
            media_type: file.media_type.clone(),
            data: Bytes(data),
        });
    } else {
        files.push(PromptFile {
            name: file.name.clone(),
            data: Bytes(data),
        });
    }
}

/// A session's title, as the bot shows it.
fn title(head: &SessionHead) -> &str {
    head.title.as_deref().unwrap_or("Untitled session")
}

/// The project a new thread starts on.
enum Pick {
    /// This project, with the prompt's text.
    Project(ProjectId, String),
    /// One of these, whose name the message named; with the prompt's text.
    Ambiguous(Vec<(ProjectId, String)>, String),
    /// A chat, about no project.
    Chat,
}

/// The id and post of an approval request or question `body` asks of the user; `None` for
/// anything else.
fn asked(session_id: &SessionId, body: &EventBody) -> Option<(String, Post)> {
    match body {
        EventBody::ApprovalRequested {
            approval_id,
            summary,
            routed_to: Route::User,
            ..
        } => Some((
            approval_id.to_string(),
            Post::Approval {
                session_id: session_id.clone(),
                approval_id: approval_id.clone(),
                summary: summary.clone(),
                resolved: None,
            },
        )),
        EventBody::QuestionAsked {
            question_id,
            text,
            choices,
            multi_select,
            routed_to: Route::User,
            ..
        } => Some((
            question_id.to_string(),
            Post::Question {
                session_id: session_id.clone(),
                question_id: question_id.clone(),
                text: text.clone(),
                choices: choices.clone(),
                multi_select: *multi_select,
                answered: None,
            },
        )),
        _ => None,
    }
}

/// The id of the approval request or question `body` resolves; `None` for anything else.
fn answer_of(body: &EventBody) -> Option<&str> {
    match body {
        EventBody::ApprovalResolved { approval_id, .. } => Some(approval_id.as_str()),
        EventBody::QuestionAnswered { question_id, .. } => Some(question_id.as_str()),
        _ => None,
    }
}

/// Marks the post of an approval request or question resolved as `body` says, by `by`.
fn resolve(post: &mut Post, body: &EventBody, by: &str) {
    match (post, body) {
        (Post::Approval { resolved, .. }, EventBody::ApprovalResolved { decision, .. }) => {
            *resolved = Some((*decision, by.to_owned()));
        }
        (
            Post::Question {
                choices, answered, ..
            },
            EventBody::QuestionAnswered { answer, .. },
        ) => {
            *answered = Some(format!("{} — {by}", answer_text(answer, choices)));
        }
        _ => {}
    }
}

fn place_of(thread: &ThreadLink) -> Place {
    Place {
        channel_id: thread.channel_id.clone(),
        thread_id: thread.thread_id.clone(),
    }
}

/// What the "working…" message says while a session is in `status`; `None` when it says
/// nothing new.
fn working_status(status: SessionStatus) -> Option<&'static str> {
    match status {
        SessionStatus::Running => Some("working…"),
        SessionStatus::WaitingForCapacity => Some("waiting for capacity…"),
        SessionStatus::NeedsYou => Some("waiting for you…"),
        SessionStatus::NeedsLogin => {
            Some("waiting: the account needs signing in again on this machine…")
        }
        _ => None,
    }
}

/// An answer of more than [`ANSWER_LINES`] lines split into its start, posted at once, and its
/// rest, for Full answer; `None` for a shorter one. A code block the start leaves open is
/// closed.
pub fn trim(text: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    if lines.len() <= ANSWER_LINES {
        return None;
    }
    let mut start = lines[..ANSWER_LINES].join("\n");
    let fences = lines[..ANSWER_LINES]
        .iter()
        .filter(|line| line.trim_start().starts_with("```"))
        .count();
    if fences % 2 == 1 {
        start.push_str("\n```");
    }
    start.push_str("\n…");
    Some((start, lines[ANSWER_LINES..].join("\n")))
}

/// The text of `answer` to a question with `choices`.
fn answer_text(answer: &Answer, choices: &[String]) -> String {
    let choice = |index: &u32| {
        choices
            .get(*index as usize)
            .cloned()
            .unwrap_or_else(|| format!("choice {}", index + 1))
    };
    match answer {
        Answer::Text { text } => text.clone(),
        Answer::Choice { index } => choice(index),
        Answer::Choices { indices } => indices.iter().map(choice).collect::<Vec<_>>().join(", "),
    }
}

/// The first sentence of `message`, or its first line.
fn first_sentence(message: &str) -> &str {
    let line = message.trim().lines().next().unwrap_or_default();
    match line.find(". ") {
        Some(end) => &line[..=end],
        None => line,
    }
}
