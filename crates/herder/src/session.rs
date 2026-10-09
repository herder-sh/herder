//! `herder session`: create, prompt, wait on, inspect and archive sessions from scripts.
//!
//! Every command connects through the client profile, as the TUI does, and never asks
//! anything interactively. `--json` prints one JSON value on stdout. `wait` reports where the
//! session stopped by its exit code; every other failure exits 1, and a usage error 64.

use std::io::{IsTerminal, Read};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use herder_client_core::{Client, ConnectionState, Machine, SessionSubscription};
use herder_protocol::{
    AccountId, Answer, ApprovalDecision, ApprovalId, CommandBody, CommandResult, Event, EventBody,
    ItemBody, PermissionMode, ProjectId, Provider, PullRequest, QuestionId, Route, Seq, SessionId,
    SessionStatus, Timestamp, TurnError, TurnId,
};
use serde::Serialize;
use tokio::time::Instant;

/// How long a command waits for its machine to answer at all.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// `wait` exit codes besides 0 for `idle`.
const NEEDS_YOU: u8 = 2;
const ERROR: u8 = 3;
const TIMEOUT: u8 = 4;

#[derive(clap::Args)]
#[command(after_help = "Exit codes: 0 done, 1 failed, 64 usage error; \
    `wait` also exits 2 when the session needs you, 3 on its error, 4 on --timeout.")]
pub struct Args {
    /// Paired machine, by name or host id [default: the only paired machine].
    #[arg(long, global = true, value_name = "NAME")]
    machine: Option<String>,
    /// Print JSON on stdout instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Create a session on a new worktree and branch, prompt it with stdin, and print its id.
    ///
    /// With nothing piped on stdin the session starts idle, without a prompt.
    New {
        /// Absolute path of the repository on the machine.
        #[arg(long, value_name = "PATH", required_unless_present_any = ["project", "chat"])]
        repo: Option<String>,
        /// Project to work on, by id, in its first clone on the machine.
        #[arg(long, value_name = "PROJECT", conflicts_with = "repo")]
        project: Option<String>,
        /// Start a chat, about no project, in a folder of its own on the machine.
        #[arg(long, conflicts_with_all = ["repo", "project", "branch"])]
        chat: bool,
        /// Account to run on, by id or label [default: the machine's only account, else the
        /// project's default account, else the first available one].
        #[arg(long, value_name = "ACCOUNT")]
        account: Option<String>,
        /// Run on the account of this provider with the most room left, instead of a named
        /// one.
        #[arg(long, value_name = "PROVIDER", conflicts_with = "account")]
        provider: Option<String>,
        /// Model, in the provider's naming [default: the provider's default].
        #[arg(long, value_name = "MODEL")]
        model: Option<String>,
        /// Permission mode [default: the project's default mode, else ask].
        #[arg(long, value_enum)]
        mode: Option<Mode>,
        /// Branch to create [default: a name herder picks].
        #[arg(long, value_name = "BRANCH")]
        branch: Option<String>,
        /// Keep the session on its account when it hits a limit [default: the machine's
        /// failover setting].
        #[arg(long, conflicts_with = "no_pin")]
        pin: bool,
        /// Let the session rotate to another account when its account hits a limit, even on a
        /// machine that pins sessions.
        #[arg(long)]
        no_pin: bool,
    },
    /// Prompt a session with stdin, queued behind a running turn, or answer what it asks.
    Send {
        /// The session id.
        session: String,
        /// Allow an approval request instead of prompting.
        #[arg(long, value_name = "APPROVAL", group = "reply")]
        approve: Option<String>,
        /// Deny an approval request instead of prompting.
        #[arg(long, value_name = "APPROVAL", group = "reply")]
        deny: Option<String>,
        /// Answer a question instead of prompting: a choice's text or number (from 1), or
        /// free text.
        #[arg(long, num_args = 2, value_names = ["QUESTION", "ANSWER"], group = "reply")]
        answer: Option<Vec<String>>,
    },
    /// Switch a session's account, provider or model using the existing session commands.
    Switch {
        /// The session id.
        session: String,
        /// Target account id or label.
        #[arg(long, required_unless_present_any = ["provider", "model"])]
        account: Option<String>,
        /// Target provider; chooses its account with the most reported quota left.
        #[arg(long)]
        provider: Option<String>,
        /// Target model.
        #[arg(long)]
        model: Option<String>,
    },
    /// Wait until a session is idle (exit 0), needs you (2) or failed (3), or time out (4);
    /// print its status, its last turn's reply and what it asks.
    Wait {
        /// The session id.
        session: String,
        /// Give up after this many seconds [default: never].
        #[arg(long, value_name = "SECONDS")]
        timeout: Option<u64>,
    },
    /// Show a session: status, branch, linked pull requests, what it asks, its last reply.
    Status {
        /// The session id.
        session: String,
    },
    /// List the machine's sessions with their status and linked pull requests.
    List,
    /// Archive a session: stop it and make it read-only; its worktree is removed three days
    /// later, its branches kept.
    Archive {
        /// The session id.
        session: String,
    },
    /// Bring an archived session back: its worktree again, on its branch, and writable.
    Unarchive {
        /// The session id.
        session: String,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Mode {
    /// Reads only; every write or command is refused.
    ReadOnly,
    /// Asks before every write or command.
    Ask,
    /// Edits files freely; asks before commands.
    AutoEdit,
    /// Does anything without asking.
    FullAccess,
}

impl From<Mode> for PermissionMode {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::ReadOnly => PermissionMode::ReadOnly,
            Mode::Ask => PermissionMode::Ask,
            Mode::AutoEdit => PermissionMode::AutoEdit,
            Mode::FullAccess => PermissionMode::FullAccess,
        }
    }
}

pub fn run(args: Args) -> Result<ExitCode> {
    // Read before connecting, so a prompt that cannot be read fails before anything changes.
    let prompt = match &args.command {
        Command::New { .. } => read_stdin()?,
        Command::Send {
            approve: None,
            deny: None,
            answer: None,
            ..
        } => match read_stdin()? {
            Some(prompt) => Some(prompt),
            None => bail!("pipe the prompt on stdin"),
        },
        _ => None,
    };
    let config_dir = herder_tui::config_dir()?
        .into_os_string()
        .into_string()
        .map_err(|_| anyhow::anyhow!("the config dir is not valid UTF-8"))?;
    let runtime = tokio::runtime::Runtime::new().context("starting the tokio runtime")?;
    runtime.block_on(async {
        let client = Client::open(
            config_dir,
            format!("herder-cli/{}", env!("CARGO_PKG_VERSION")),
        )?;
        let machine = pick(&client.machines(), args.machine.as_deref())?;
        let cli = Cli {
            client,
            machine,
            json: args.json,
        };
        cli.run(args.command, prompt).await
    })
}

/// The whole of stdin, trimmed; `None` when it is a terminal or holds only whitespace.
fn read_stdin() -> Result<Option<String>> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Ok(None);
    }
    let mut text = String::new();
    stdin
        .read_to_string(&mut text)
        .context("reading the prompt from stdin")?;
    let text = text.trim();
    Ok((!text.is_empty()).then(|| text.to_owned()))
}

/// The machine named `wanted`, by name or host id, or the only one paired.
fn pick(machines: &[Machine], wanted: Option<&str>) -> Result<Machine> {
    let names = || {
        machines
            .iter()
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match (wanted, machines) {
        (None, []) => bail!("no paired machines; pair one with `herder connect`"),
        (None, [machine]) => Ok(machine.clone()),
        (None, _) => bail!(
            "several machines are paired; pick one with --machine: {}",
            names()
        ),
        (Some(wanted), _) => machines
            .iter()
            .find(|m| m.host_id.as_str() == wanted)
            .or_else(|| machines.iter().find(|m| m.name == wanted))
            .cloned()
            .with_context(|| format!("no paired machine {wanted}; paired: {}", names())),
    }
}

struct Cli {
    client: Client,
    machine: Machine,
    json: bool,
}

impl Cli {
    async fn run(&self, command: Command, prompt: Option<String>) -> Result<ExitCode> {
        self.sync().await?;
        match command {
            Command::New {
                repo,
                project,
                chat,
                account,
                provider,
                model,
                mode,
                branch,
                pin,
                no_pin,
            } => {
                let account_id = match &provider {
                    Some(_) => None,
                    None => self.account(account.as_deref())?,
                };
                let created = self
                    .send(CommandBody::CreateSession {
                        repo,
                        project_id: project.map(ProjectId::new),
                        branch,
                        account_id,
                        provider: provider.map(Provider::from),
                        model,
                        permission_mode: mode.map(Into::into),
                        failover_pin: (pin || no_pin).then_some(pin),
                        chat,
                    })
                    .await?;
                let CommandResult::SessionCreated { session_id } = created else {
                    bail!("the daemon did not create a session: {created:?}");
                };
                if let Some(prompt) = prompt {
                    let (subscription, mut view) = self.load(&session_id).await?;
                    self.prompt(&subscription, &mut view, prompt).await?;
                }
                self.print_id(&session_id)?;
            }
            Command::Send {
                session,
                approve,
                deny,
                answer,
            } => {
                let session_id = SessionId::new(session);
                let (subscription, mut view) = self.load(&session_id).await?;
                let approval = |id: String, decision| CommandBody::AnswerApproval {
                    session_id: session_id.clone(),
                    approval_id: ApprovalId::new(id),
                    decision,
                };
                if let Some(id) = approve {
                    self.send(approval(id, ApprovalDecision::Allow)).await?;
                } else if let Some(id) = deny {
                    self.send(approval(id, ApprovalDecision::Deny)).await?;
                } else if let Some([id, answer]) = answer.as_deref() {
                    let question_id = QuestionId::new(id.as_str());
                    let answer = view.answer(&question_id, answer);
                    self.send(CommandBody::AnswerQuestion {
                        session_id: session_id.clone(),
                        question_id,
                        answer,
                    })
                    .await?;
                } else if let Some(prompt) = prompt {
                    self.prompt(&subscription, &mut view, prompt).await?;
                }
                if self.json {
                    self.print_id(&session_id)?;
                }
            }
            Command::Switch {
                session,
                account,
                provider,
                model,
            } => {
                let session_id = SessionId::new(session);
                let (_, view) = self.load(&session_id).await?;
                let accounts = self.current().map(|m| m.accounts).unwrap_or_default();
                let target = if let Some(account) = account {
                    let id = self.account(Some(&account))?;
                    accounts.iter().find(|a| Some(&a.account_id) == id.as_ref())
                } else if let Some(provider) = &provider {
                    accounts
                        .iter()
                        .filter(|a| a.provider.as_str() == provider)
                        .min_by(|a, b| {
                            let used = |a: &herder_protocol::Account| {
                                a.usage
                                    .iter()
                                    .filter(|w| {
                                        !w.resets_at.is_some_and(|at| at <= Timestamp::now())
                                    })
                                    .map(|w| w.used_percent)
                                    .fold(0.0_f64, f64::max)
                            };
                            used(a)
                                .total_cmp(&used(b))
                                .then_with(|| a.account_id.as_str().cmp(b.account_id.as_str()))
                        })
                } else {
                    None
                };
                if provider.is_some() && target.is_none() {
                    bail!("the machine has no account for the requested provider");
                }
                let mut model = model;
                if let Some(target) = target {
                    if provider
                        .as_deref()
                        .is_some_and(|p| p != target.provider.as_str())
                    {
                        bail!("the account does not belong to the requested provider");
                    }
                    if view.provider.as_ref() == Some(&target.provider) {
                        self.send(CommandBody::SwitchAccount {
                            session_id: session_id.clone(),
                            account_id: target.account_id.clone(),
                        })
                        .await?;
                    } else {
                        self.send(CommandBody::SwitchProvider {
                            session_id: session_id.clone(),
                            account_id: target.account_id.clone(),
                            model: model.take(),
                        })
                        .await?;
                    }
                }
                if let Some(model) = model {
                    self.send(CommandBody::SetModel {
                        session_id: session_id.clone(),
                        model,
                    })
                    .await?;
                }
                if self.json {
                    self.print_id(&session_id)?;
                }
            }
            Command::Wait { session, timeout } => {
                let deadline = timeout.map(|secs| Instant::now() + Duration::from_secs(secs));
                return self.wait(&SessionId::new(session), deadline).await;
            }
            Command::Status { session } => {
                let (_subscription, view) = self.load(&SessionId::new(session)).await?;
                self.print(&view, || view.describe())?;
            }
            Command::List => self.list().await?,
            Command::Archive { session } => {
                let session_id = SessionId::new(session);
                self.send(CommandBody::ArchiveSession {
                    session_id: session_id.clone(),
                })
                .await?;
                if self.json {
                    self.print_id(&session_id)?;
                }
            }
            Command::Unarchive { session } => {
                let session_id = SessionId::new(session);
                self.send(CommandBody::UnarchiveSession {
                    session_id: session_id.clone(),
                })
                .await?;
                if self.json {
                    self.print_id(&session_id)?;
                }
            }
        }
        Ok(ExitCode::SUCCESS)
    }

    /// Waits until the client holds everything the daemon had to say when it was asked: the
    /// session and account lists, and every session subscribed to so far.
    async fn sync(&self) -> Result<()> {
        let synced = self.client.synced(self.machine.host_id.clone());
        match tokio::time::timeout(CONNECT_TIMEOUT, synced).await {
            Ok(result) => Ok(result?),
            Err(_) => match self.current().map(|m| m.connection) {
                Some(ConnectionState::Disconnected { error }) => {
                    bail!("cannot reach {}: {error}", self.machine.name)
                }
                _ => bail!(
                    "cannot reach {}: no answer in {} s",
                    self.machine.name,
                    CONNECT_TIMEOUT.as_secs()
                ),
            },
        }
    }

    /// The machine as the client knows it now.
    fn current(&self) -> Option<Machine> {
        self.client
            .machines()
            .into_iter()
            .find(|m| m.host_id == self.machine.host_id)
    }

    /// The account named `wanted`, by id or label, or the machine's only one; `None` leaves
    /// the choice to the project's default account.
    fn account(&self, wanted: Option<&str>) -> Result<Option<AccountId>> {
        let accounts = self.current().map(|m| m.accounts).unwrap_or_default();
        let names = accounts
            .iter()
            .map(|a| a.account_id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let name = &self.machine.name;
        match (wanted, accounts.as_slice()) {
            (None, [account]) => Ok(Some(account.account_id.clone())),
            (None, []) => bail!("{name} has no accounts"),
            (None, _) => Ok(None),
            (Some(wanted), _) => accounts
                .iter()
                .find(|a| a.account_id.as_str() == wanted)
                .or_else(|| accounts.iter().find(|a| a.label == wanted))
                .map(|a| Some(a.account_id.clone()))
                .with_context(|| format!("{name} has no account {wanted}; it has: {names}")),
        }
    }

    async fn send(&self, command: CommandBody) -> Result<CommandResult> {
        Ok(self
            .client
            .send(self.machine.host_id.clone(), command)
            .await?)
    }

    /// Subscribes to a session and folds everything the daemon holds of it.
    async fn load(&self, session_id: &SessionId) -> Result<(SessionSubscription, View)> {
        let subscription = self
            .client
            .subscribe_session(self.machine.host_id.clone(), session_id.clone())?;
        self.sync().await?;
        let mut view = View::new(session_id.clone());
        if let Some(update) = subscription.next().await {
            view.apply(update.events);
        }
        if view.seq == 0 {
            bail!("{} has no session {session_id}", self.machine.name);
        }
        Ok((subscription, view))
    }

    /// Prompts the session; the daemon queues the prompt behind a running turn.
    ///
    /// The daemon answers before it starts a turn on an idle session, so this waits for the
    /// status change it journals then: a `wait` right after sees the new turn, not the
    /// previous one's end.
    async fn prompt(
        &self,
        subscription: &SessionSubscription,
        view: &mut View,
        text: String,
    ) -> Result<()> {
        let starts = view.turn.is_none()
            && (view.retry_at.is_some()
                || matches!(
                    view.status,
                    SessionStatus::Idle | SessionStatus::NeedsYou | SessionStatus::Error
                ));
        let before = view.status_seq;
        self.send(CommandBody::SendPrompt {
            session_id: view.session_id.clone(),
            text,
            images: Vec::new(),
            files: Vec::new(),
        })
        .await?;
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        while starts && view.status_seq == before {
            match tokio::time::timeout_at(deadline, subscription.next()).await {
                Ok(Some(update)) => view.apply(update.events),
                Ok(None) => bail!("the client stopped"),
                // The prompt was accepted; it just has not started yet.
                Err(_) => break,
            }
        }
        Ok(())
    }

    async fn wait(&self, session_id: &SessionId, deadline: Option<Instant>) -> Result<ExitCode> {
        let (subscription, mut view) = self.load(session_id).await?;
        loop {
            let code = match view.status {
                SessionStatus::Idle => Some(0),
                SessionStatus::NeedsYou => Some(NEEDS_YOU),
                SessionStatus::Error => Some(ERROR),
                SessionStatus::Archived | SessionStatus::Moved => {
                    bail!("session {session_id} is {}", view.status_label())
                }
                _ => None,
            };
            if let Some(code) = code {
                self.print(&view, || view.report())?;
                return Ok(ExitCode::from(code));
            }
            let next = subscription.next();
            let update = match deadline {
                Some(deadline) => match tokio::time::timeout_at(deadline, next).await {
                    Ok(update) => update,
                    Err(_) => {
                        self.print(&view, || view.report())?;
                        return Ok(ExitCode::from(TIMEOUT));
                    }
                },
                None => next.await,
            };
            let Some(update) = update else {
                bail!("the client stopped");
            };
            view.apply(update.events);
        }
    }

    async fn list(&self) -> Result<()> {
        let heads = self.current().map(|m| m.sessions).unwrap_or_default();
        let subscriptions = heads
            .iter()
            .map(|head| {
                self.client
                    .subscribe_session(self.machine.host_id.clone(), head.session_id.clone())
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.sync().await?;
        let mut views = Vec::new();
        for (head, subscription) in heads.iter().zip(&subscriptions) {
            let mut view = View::new(head.session_id.clone());
            if let Some(update) = subscription.next().await {
                view.apply(update.events);
            }
            views.push(view);
        }
        self.print(&views, || table(&views))
    }

    fn print_id(&self, session_id: &SessionId) -> Result<()> {
        #[derive(Serialize)]
        struct Id<'a> {
            session_id: &'a SessionId,
        }
        self.print(&Id { session_id }, || format!("{session_id}\n"))
    }

    /// Prints `value` as JSON with `--json`, else the text `text` makes.
    fn print<T: Serialize>(&self, value: &T, text: impl FnOnce() -> String) -> Result<()> {
        if self.json {
            println!("{}", serde_json::to_string(value)?);
        } else {
            print!("{}", text());
        }
        Ok(())
    }
}

/// A session as folded from its events; its JSON form is what `status`, `wait` and `list`
/// print.
#[derive(Debug, Serialize)]
struct View {
    session_id: SessionId,
    status: SessionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_at: Option<Timestamp>,
    provider: Option<Provider>,
    repo: String,
    /// Absent for a session that works in its folder itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    model: String,
    account_id: Option<AccountId>,
    permission_mode: Option<PermissionMode>,
    /// Primary session of its task, for a child session.
    #[serde(skip_serializing_if = "Option::is_none")]
    parent: Option<SessionId>,
    /// Task label, for a child session.
    #[serde(skip_serializing_if = "Option::is_none")]
    task: Option<String>,
    /// Pull requests linked now, in the order they were linked.
    prs: Vec<PullRequest>,
    /// Approval requests nobody answered yet, oldest first.
    approvals: Vec<OpenApproval>,
    /// Questions nobody answered yet, oldest first.
    questions: Vec<OpenQuestion>,
    /// The latest turn's last assistant message.
    last_message: Option<String>,
    /// Why the latest turn failed, when it did.
    last_error: Option<TurnError>,
    /// Seq of the latest event.
    seq: Seq,
    #[serde(skip)]
    status_seq: Seq,
    #[serde(skip)]
    turn: Option<TurnId>,
}

#[derive(Debug, Serialize)]
struct OpenApproval {
    approval_id: ApprovalId,
    summary: String,
    routed_to: Route,
}

#[derive(Debug, Serialize)]
struct OpenQuestion {
    question_id: QuestionId,
    turn_id: TurnId,
    text: String,
    choices: Vec<String>,
    routed_to: Route,
}

impl View {
    fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            status: SessionStatus::Idle,
            retry_at: None,
            provider: None,
            repo: String::new(),
            branch: None,
            model: String::new(),
            account_id: None,
            permission_mode: None,
            parent: None,
            task: None,
            prs: Vec::new(),
            approvals: Vec::new(),
            questions: Vec::new(),
            last_message: None,
            last_error: None,
            seq: 0,
            status_seq: 0,
            turn: None,
        }
    }

    fn apply(&mut self, events: Vec<Event>) {
        for event in events {
            self.event(event);
        }
    }

    fn event(&mut self, event: Event) {
        self.seq = event.seq;
        match event.body {
            EventBody::SessionCreated {
                repo,
                branch,
                account_id,
                provider,
                model,
                permission_mode,
                parent,
                task,
                ..
            } => {
                self.repo = repo;
                self.branch = branch;
                self.account_id = Some(account_id);
                self.provider = Some(provider);
                self.model = model;
                self.permission_mode = Some(permission_mode);
                self.parent = parent;
                self.task = task;
            }
            EventBody::BranchCheckedOut { branch } => self.branch = Some(branch),
            EventBody::SessionStatusChanged { status, retry_at } => {
                self.status = status;
                self.retry_at = retry_at;
                self.status_seq = event.seq;
            }
            EventBody::TurnStarted { turn_id } => {
                self.turn = Some(turn_id);
                self.last_message = None;
                self.last_error = None;
            }
            EventBody::TurnCompleted { turn_id, .. } | EventBody::TurnInterrupted { turn_id } => {
                self.turn_ended(&turn_id);
            }
            EventBody::TurnFailed { turn_id, error } => {
                self.turn_ended(&turn_id);
                self.last_error = Some(error);
            }
            EventBody::ItemAdded { item } => {
                if let ItemBody::AssistantMessage { text } = item.body {
                    self.last_message = Some(text);
                }
            }
            EventBody::ApprovalRequested {
                approval_id,
                summary,
                routed_to,
                ..
            } => self.approvals.push(OpenApproval {
                approval_id,
                summary,
                routed_to,
            }),
            EventBody::ApprovalEscalated { approval_id, .. } => {
                for approval in &mut self.approvals {
                    if approval.approval_id == approval_id {
                        approval.routed_to = Route::User;
                    }
                }
            }
            EventBody::ApprovalResolved { approval_id, .. } => {
                self.approvals.retain(|a| a.approval_id != approval_id);
            }
            EventBody::QuestionAsked {
                question_id,
                turn_id,
                text,
                choices,
                routed_to,
                ..
            } => self.questions.push(OpenQuestion {
                question_id,
                turn_id,
                text,
                choices,
                routed_to,
            }),
            EventBody::QuestionEscalated { question_id, .. } => {
                for question in &mut self.questions {
                    if question.question_id == question_id {
                        question.routed_to = Route::User;
                    }
                }
            }
            EventBody::QuestionAnswered { question_id, .. } => {
                self.questions.retain(|q| q.question_id != question_id);
            }
            EventBody::ProviderSwitched {
                provider,
                account_id,
                model,
            } => {
                self.provider = Some(provider);
                self.account_id = Some(account_id);
                self.model = model;
            }
            EventBody::ModelSwitched { model } => {
                self.model = model;
            }
            EventBody::AccountSwitched { account_id } => self.account_id = Some(account_id),
            EventBody::PermissionModeChanged { mode } => self.permission_mode = Some(mode),
            EventBody::PrLinked { pr } | EventBody::PrUpdated { pr } => {
                match self.prs.iter_mut().find(|known| known.number == pr.number) {
                    Some(known) => *known = pr,
                    None => self.prs.push(pr),
                }
            }
            EventBody::PrUnlinked { number } => self.prs.retain(|pr| pr.number != number),
            EventBody::ChildSpawned { .. } | EventBody::ChildReported { .. } => {}
            EventBody::TitleChanged { .. }
            | EventBody::SessionForked { .. }
            | EventBody::Unknown => {}
        }
    }

    /// A turn's end clears its questions; the daemon resolves its approvals with events.
    fn turn_ended(&mut self, turn_id: &TurnId) {
        if self.turn.as_ref() == Some(turn_id) {
            self.turn = None;
        }
        self.questions.retain(|q| q.turn_id != *turn_id);
    }

    /// `answer` to the question: one of its choices, by text or by number from 1, else text.
    fn answer(&self, question_id: &QuestionId, answer: &str) -> Answer {
        let choices = self
            .questions
            .iter()
            .find(|q| q.question_id == *question_id)
            .map(|q| q.choices.as_slice())
            .unwrap_or_default();
        let by_text = choices.iter().position(|choice| choice == answer);
        let by_number = answer
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=choices.len()).contains(n))
            .map(|n| n - 1);
        match by_text.or(by_number).and_then(|i| u32::try_from(i).ok()) {
            Some(index) => Answer::Choice { index },
            None => Answer::Text {
                text: answer.to_owned(),
            },
        }
    }

    fn status_label(&self) -> String {
        match self
            .retry_at
            .filter(|_| self.status == SessionStatus::WaitingForCapacity)
        {
            Some(at) => format!("waiting for limit reset · {} UTC", at.strftime("%H:%M")),
            None => status_name(self.status),
        }
    }

    /// What `wait` prints: the status, the last reply, what the session asks.
    fn report(&self) -> String {
        let mut out = format!("{}\n", self.status_label());
        if let Some(error) = &self.last_error {
            out.push_str(&format!("turn failed: {}\n", error.message));
        }
        if let Some(message) = &self.last_message {
            out.push_str(&format!("\n{message}\n"));
        }
        let asks = self.asks();
        if !asks.is_empty() {
            out.push_str(&format!("\n{asks}"));
        }
        out
    }

    /// What `status` prints.
    fn describe(&self) -> String {
        let mut lines = vec![
            ("session", self.session_id.to_string()),
            ("status", self.status_label()),
            ("repo", self.repo.clone()),
            (
                "branch",
                self.branch.clone().unwrap_or_else(|| "none".to_owned()),
            ),
            ("model", self.model.clone()),
        ];
        if let Some(account) = &self.account_id {
            lines.push(("account", account.to_string()));
        }
        if let Some(mode) = self.permission_mode {
            lines.push(("mode", mode_name(mode)));
        }
        if let Some(task) = &self.task {
            lines.push(("task", task.clone()));
        }
        if let Some(parent) = &self.parent {
            lines.push(("parent", parent.to_string()));
        }
        for pr in &self.prs {
            lines.push(("pr", format!("{}  {}", pr_short(pr), pr.url)));
        }
        if let Some(error) = &self.last_error {
            lines.push(("failed", error.message.clone()));
        }
        let mut out: String = lines
            .into_iter()
            .map(|(key, value)| format!("{key:<9} {value}\n"))
            .collect();
        let asks = self.asks();
        if !asks.is_empty() {
            out.push_str(&format!("\n{asks}"));
        }
        if let Some(message) = &self.last_message {
            out.push_str(&format!("\n{message}\n"));
        }
        out
    }

    /// The open approvals and questions, with the ids `send` answers them by.
    fn asks(&self) -> String {
        let mut out = String::new();
        let to = |route| match route {
            Route::Primary => " (for the primary session)",
            Route::User => "",
        };
        for approval in &self.approvals {
            out.push_str(&format!(
                "approval {}{}: {}\n",
                approval.approval_id,
                to(approval.routed_to),
                approval.summary
            ));
        }
        for question in &self.questions {
            out.push_str(&format!(
                "question {}{}: {}\n",
                question.question_id,
                to(question.routed_to),
                question.text
            ));
            for (n, choice) in question.choices.iter().enumerate() {
                out.push_str(&format!("  {}. {choice}\n", n + 1));
            }
        }
        out
    }
}

/// What `list` prints: one row per session.
fn table(views: &[View]) -> String {
    let rows: Vec<[String; 5]> = views
        .iter()
        .map(|view| {
            let prs = view.prs.iter().map(pr_short).collect::<Vec<_>>().join(", ");
            [
                view.session_id.to_string(),
                view.status_label(),
                view.branch.clone().unwrap_or_else(|| "-".to_owned()),
                view.repo.clone(),
                prs,
            ]
        })
        .collect();
    let header = ["SESSION", "STATUS", "BRANCH", "REPO", "PRS"].map(str::to_owned);
    let mut widths = header.clone().map(|h| h.len());
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.len());
        }
    }
    std::iter::once(&header)
        .chain(&rows)
        .map(|row| {
            let line = row
                .iter()
                .zip(widths)
                .map(|(cell, width)| format!("{cell:<width$}"))
                .collect::<Vec<_>>()
                .join("  ");
            format!("{}\n", line.trim_end())
        })
        .collect()
}

/// A pull request in a few words: `#12 open, ci passing`.
fn pr_short(pr: &PullRequest) -> String {
    let (state, ci) = (to_name(&pr.state), to_name(&pr.ci));
    format!("#{} {state}, ci {ci}", pr.number)
}

fn status_name(status: SessionStatus) -> String {
    to_name(&status)
}

fn mode_name(mode: PermissionMode) -> String {
    to_name(&mode)
}

/// The wire name of a unit enum value, as the JSON output spells it.
fn to_name<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        _ => "unknown".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use herder_protocol::{CiStatus, Mergeable, PrState, ReviewStatus, Timestamp};

    use super::*;

    fn event(seq: Seq, body: EventBody) -> Event {
        Event {
            session_id: SessionId::new("s1"),
            seq,
            at: Timestamp::now(),
            by: None,
            body,
        }
    }

    fn pr(number: u64, state: PrState, ci: CiStatus) -> PullRequest {
        PullRequest {
            number,
            url: format!("https://github.com/o/r/pull/{number}"),
            title: "Fix it".into(),
            head_branch: None,
            head_sha: None,
            unresolved_threads: None,
            state,
            ci,
            review: ReviewStatus::None,
            mergeable: Mergeable::Unknown,
        }
    }

    #[test]
    fn linked_pull_requests_follow_their_updates_and_unlinks() {
        let mut view = View::new(SessionId::new("s1"));
        view.apply(vec![
            event(
                1,
                EventBody::PrLinked {
                    pr: pr(7, PrState::Draft, CiStatus::None),
                },
            ),
            event(
                2,
                EventBody::PrLinked {
                    pr: pr(9, PrState::Open, CiStatus::Pending),
                },
            ),
            event(
                3,
                EventBody::PrUpdated {
                    pr: pr(7, PrState::Open, CiStatus::Passing),
                },
            ),
            event(4, EventBody::PrUnlinked { number: 9 }),
        ]);
        assert_eq!(view.prs, [pr(7, PrState::Open, CiStatus::Passing)]);
        assert_eq!(pr_short(&view.prs[0]), "#7 open, ci passing");
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["prs"][0]["number"], 7);
        assert_eq!(json["prs"][0]["ci"], "passing");
        assert_eq!(json["seq"], 4);
    }

    #[test]
    fn an_answer_picks_a_choice_by_text_or_number_else_is_free_text() {
        let mut view = View::new(SessionId::new("s1"));
        let question_id = QuestionId::new("q1");
        view.apply(vec![event(
            1,
            EventBody::QuestionAsked {
                question_id: question_id.clone(),
                turn_id: TurnId::new("t1"),
                text: "Which database?".into(),
                choices: vec!["SQLite".into(), "Postgres".into()],
                routed_to: Route::User,
                reason: None,
            },
        )]);
        let text = |text: &str| Answer::Text { text: text.into() };
        assert_eq!(
            view.answer(&question_id, "Postgres"),
            Answer::Choice { index: 1 }
        );
        assert_eq!(view.answer(&question_id, "1"), Answer::Choice { index: 0 });
        assert_eq!(view.answer(&question_id, "3"), text("3"));
        assert_eq!(view.answer(&question_id, "MySQL"), text("MySQL"));
        assert_eq!(view.answer(&QuestionId::new("q2"), "1"), text("1"));
        assert!(
            view.asks()
                .contains("question q1: Which database?\n  1. SQLite\n")
        );

        // The question goes once its turn ends.
        view.apply(vec![event(
            2,
            EventBody::TurnCompleted {
                turn_id: TurnId::new("t1"),
                usage: None,
            },
        )]);
        assert!(view.questions.is_empty());
    }

    #[test]
    fn limit_wait_reports_the_deadline_until_the_next_status() {
        let mut view = View::new(SessionId::new("s1"));
        view.event(event(
            1,
            EventBody::SessionStatusChanged {
                status: SessionStatus::WaitingForCapacity,
                retry_at: Some("2026-10-03T21:20:00Z".parse().unwrap()),
            },
        ));
        assert!(
            view.describe()
                .contains("waiting for limit reset · 21:20 UTC")
        );
        assert_eq!(
            serde_json::to_value(&view).unwrap()["retry_at"],
            "2026-10-03T21:20:00Z"
        );
        view.event(event(
            2,
            EventBody::SessionStatusChanged {
                status: SessionStatus::Running,
                retry_at: None,
            },
        ));
        assert!(!view.describe().contains("limit reset"));
        assert!(
            serde_json::to_value(&view)
                .unwrap()
                .get("retry_at")
                .is_none()
        );
    }
}
