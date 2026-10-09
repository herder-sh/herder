//! `fake_daemon --demo`: a small fleet doing what herder does today, with invented content, for
//! the screenshots on herder.sh (the `screenshots` workflow).
//!
//! Three machines, `devbox`, `studio` and `laptop`, each with a clone of `acme/api`, `acme/web`
//! and `acme/infra` (one bare origin per repository, which git reaches as GitHub), and Claude
//! and Codex accounts on a scripted adapter: each prompt plays the script in `fixtures/demo` that
//! [`SCRIPTS`] names for its account, else a short reply. Once they run, the demo sets up:
//!
//! - on `devbox`, a session whose Claude subagents work in parallel, a Codex session waiting on
//!   an approval, a session that hit the Work account's limit and failed over to Personal, a
//!   primary with three children it spawned over herder's MCP server, and a finished session;
//! - a session that started on `devbox` and was handed off to `studio`, onto its Team account,
//!   where it finished the work;
//! - a finished session on `laptop`.
//!
//! [`Demo::start`] returns once all of it is in place, with the link a device paired with every
//! machine shares, as `--share` prints.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use herder_adapters::{
    Adapter, AdapterCommand, AdapterEvent, AdapterSession, Capabilities, StartFuture, StartRequest,
};
use herder_client_core::{Client, ConnectionState, PairResult, PairingUri};
use herder_daemon::auth::{Auth, PAIRING_TTL};
use herder_daemon::login::Logins;
use herder_daemon::mcp;
use herder_daemon::projects::{Discovery, OnSessionsChanged, Overrides, ProjectsConfig};
use herder_daemon::resources::{Admission, ReadHost, Reading, ResourcesConfig};
use herder_daemon::session::{AccountConfig, Accounts, Adapters, EventSink, SessionManager, Setup};
use herder_daemon::settings::Settings;
use herder_daemon::terminal::Terminals;
use herder_daemon::worktree::{Worktrees, checkpoint};
use herder_daemon::ws::{Host, Server, Tls};
use herder_daemon::{Config, Hub, session};
use herder_protocol::{
    AccountId, CommandBody, CommandResult, EventBody, HostId, Item, ItemBody, ItemId,
    PermissionMode, Provider, SessionId, SessionStatus, Timestamp, TurnId,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// How long any step of setting the demo up may take.
const TIMEOUT: Duration = Duration::from_secs(60);

const AGENTS: &str =
    "Audit how the webhook handlers retry failed deliveries and fix anything unsafe.";
const LEDGER: &str =
    "Move invoices onto the new ledger tables, with a migration that is safe to rerun.";
const DATES: &str = "Upgrade date-fns to v4 and fix whatever breaks.";
const RETENTION: &str =
    "Roll out the 30-day log retention policy: Terraform, the Loki Helm values and the runbook.";
const TERRAFORM: &str = "Set the log bucket lifecycle to 30 days in terraform/modules/logs and \
                         plan it against production. Report the plan's summary.";
const HELM: &str = "Lower Loki's retention_period to 30 days in every values file under \
                    charts/loki and check the rendered manifests.";
const RUNBOOK: &str = "Write docs/runbooks/logs.md: how to restore logs older than 30 days from \
                       the cold storage archive.";
const FLAKY: &str = "The checkout e2e test fails about one CI run in five. Find out why.";
const SAFARI: &str = "Carry on here: fix it and run the checkout suite in WebKit until it passes \
                      20 times in a row.";

/// The script each prompt plays, by account; other prompts get a short reply.
const SCRIPTS: &[(&str, &str, &str)] = &[
    ("claude-personal", AGENTS, "agents.jsonl"),
    ("claude-work", LEDGER, "limit.jsonl"),
    ("claude-personal", LEDGER, "failover.jsonl"),
    ("codex", DATES, "approval.jsonl"),
    ("claude-personal", RETENTION, "task.jsonl"),
    ("claude-personal", TERRAFORM, "child-terraform.jsonl"),
    ("claude-personal", HELM, "child-helm.jsonl"),
    ("claude-personal", RUNBOOK, "child-runbook.jsonl"),
    ("claude-personal", FLAKY, "handoff-devbox.jsonl"),
    ("claude-team", SAFARI, "handoff-studio.jsonl"),
    ("codex", FINISHED[0].4, "done.jsonl"),
    ("claude-personal", FINISHED[1].4, "done.jsonl"),
    ("codex", FINISHED[2].4, "done.jsonl"),
];

/// The finished sessions that fill out the lists: machine, account, project, title, prompt.
const FINISHED: &[(&str, &str, &str, &str, &str)] = &[
    (
        "devbox",
        "codex",
        "api",
        "Rate-limit the public API per key",
        "Add per-key rate limits to the public API.",
    ),
    (
        "laptop",
        "claude-personal",
        "web",
        "Tidy the README badges",
        "Replace the broken README badges.",
    ),
    (
        "studio",
        "codex",
        "infra",
        "Pin the Terraform providers",
        "Pin every Terraform provider to a minor version.",
    ),
];

/// The repositories every machine has a clone of, as `acme/<name>`.
const REPOS: &[&str] = &["api", "web", "infra"];

/// An account: its id, provider and label.
type Account = (&'static str, &'static str, &'static str);

/// The machines: name, cores, memory in GiB, accounts.
const MACHINES: &[(&str, u32, u64, &[Account])] = &[
    (
        "devbox",
        32,
        128,
        &[
            ("claude-work", "claude", "Work"),
            ("claude-personal", "claude", "Personal"),
            ("codex", "codex", "Codex"),
        ],
    ),
    (
        "studio",
        24,
        96,
        &[
            ("claude-team", "claude", "Team"),
            ("codex", "codex", "Codex"),
        ],
    ),
    (
        "laptop",
        12,
        36,
        &[("claude-personal", "claude", "Personal")],
    ),
];

/// The running demo; [`Demo::stop`] stops it and removes everything it created.
pub struct Demo {
    /// The link a device paired with every machine shares.
    pub link: String,
    daemons: Vec<Daemon>,
    _tmp: TempDir,
}

impl Demo {
    /// Starts the machines and sets the demo up on them.
    pub async fn start() -> Result<Self> {
        let tmp = tempfile::tempdir()?;
        let origins = origins(&tmp.path().join("origins"))?;
        let mut daemons = Vec::new();
        for &(name, cores, memory, accounts) in MACHINES {
            let dir = tmp.path().join(name);
            daemons.push(Daemon::start(&dir, name, cores, memory, accounts, &origins).await?);
        }
        let profile = tmp.path().join("profile");
        std::fs::create_dir(&profile)?;
        let client = Client::open(profile.display().to_string(), "demo".into())?;
        for daemon in &daemons {
            for result in client.pair(daemon.link.clone()).await? {
                if let PairResult::Failed { error, .. } = result {
                    bail!("the demo device did not pair: {error}");
                }
            }
        }
        let changes = client.changes();
        tokio::time::timeout(TIMEOUT, async {
            while !client
                .machines()
                .iter()
                .all(|machine| machine.connection == ConnectionState::Connected)
            {
                if !changes.next().await {
                    break;
                }
            }
        })
        .await
        .context("the demo device did not connect")?;
        let demo = Scenario {
            client: &client,
            daemons: &daemons,
        };
        demo.set_up().await?;
        let shared = client.share().await?;
        if let Some(skipped) = shared.skipped.first() {
            bail!("{} was not shared: {}", skipped.host_id, skipped.error);
        }
        Ok(Self {
            link: shared.link.to_string(),
            daemons,
            _tmp: tmp,
        })
    }

    /// Stops every machine and waits until they have.
    pub async fn stop(self) -> Result<()> {
        for daemon in self.daemons {
            daemon.shutdown.cancel();
            daemon.server.await?;
        }
        Ok(())
    }
}

/// A session of the demo, as its machine's journal has it.
#[cfg(test)]
pub struct Scene {
    pub host: String,
    pub title: String,
    pub status: SessionStatus,
    pub events: Vec<EventBody>,
}

#[cfg(test)]
impl Demo {
    /// Every session of every machine.
    pub async fn scenes(&self) -> Result<Vec<Scene>> {
        let mut scenes = Vec::new();
        for daemon in &self.daemons {
            for head in daemon.sessions.sessions().await? {
                let events: Vec<EventBody> = daemon
                    .sessions
                    .read_since(&head.session_id, 0, usize::MAX)
                    .await?
                    .into_iter()
                    .map(|event| event.body)
                    .collect();
                let mut title = String::new();
                let mut status = SessionStatus::Idle;
                for body in &events {
                    match body {
                        EventBody::TitleChanged { title: new, .. } => title.clone_from(new),
                        EventBody::SessionStatusChanged { status: new, .. } => status = *new,
                        _ => {}
                    }
                }
                scenes.push(Scene {
                    host: daemon.id.to_string(),
                    title,
                    status,
                    events,
                });
            }
        }
        Ok(scenes)
    }
}

/// Sets the demo's sessions up through a paired client, as a user would.
struct Scenario<'a> {
    client: &'a Client,
    daemons: &'a [Daemon],
}

impl Scenario<'_> {
    async fn set_up(&self) -> Result<()> {
        // A turn that fails over between accounts, and the turn before a handoff, first: both
        // finish, and the handoff forks once its turn is checkpointed.
        let ledger = self
            .session(
                "devbox",
                "claude-work",
                "api",
                "ledger-invoices",
                "Move invoices onto the ledger",
            )
            .await?;
        self.prompt("devbox", &ledger, LEDGER).await?;
        let flaky = self
            .session(
                "devbox",
                "claude-personal",
                "web",
                "fix-flaky-checkout",
                "Fix the flaky checkout e2e test",
            )
            .await?;
        self.prompt("devbox", &flaky, FLAKY).await?;
        for (name, account, repo, title, prompt) in FINISHED {
            let branch = title.to_lowercase().replace(' ', "-");
            let session = self.session(name, account, repo, &branch, title).await?;
            self.prompt(name, &session, prompt).await?;
        }

        let agents = self
            .session(
                "devbox",
                "claude-personal",
                "api",
                "webhook-retries",
                "Audit webhook retries",
            )
            .await?;
        self.prompt("devbox", &agents, AGENTS).await?;
        let dates = self
            .session(
                "devbox",
                "codex",
                "web",
                "date-fns-v4",
                "Upgrade date-fns to v4",
            )
            .await?;
        self.prompt("devbox", &dates, DATES).await?;

        let retention = self
            .session(
                "devbox",
                "claude-personal",
                "infra",
                "log-retention",
                "Roll out 30-day log retention",
            )
            .await?;
        self.prompt("devbox", &retention, RETENTION).await?;
        let devbox = self.daemon("devbox")?;
        devbox
            .until(&retention, |events| {
                events
                    .iter()
                    .any(|body| matches!(body, EventBody::TurnStarted { .. }))
            })
            .await?;
        for (task, prompt) in [
            ("Terraform: 30-day lifecycle on the log bucket", TERRAFORM),
            ("Helm: Loki retention to 30 days", HELM),
            ("Runbook: restoring logs from cold storage", RUNBOOK),
        ] {
            devbox.spawn(&retention, task, prompt).await?;
        }

        devbox.until(&ledger, finished(1)).await?;
        devbox.until(&flaky, finished(1)).await?;
        let forked = self
            .client
            .fork_session(
                HostId::new("devbox"),
                flaky.clone(),
                HostId::new("studio"),
                Some(AccountId::new("claude-team")),
            )
            .await?;
        let CommandResult::SessionForked { session_id, .. } = forked else {
            bail!("studio did not fork the session: {forked:?}");
        };
        // The work goes on on studio, so the original is put away.
        self.client
            .send(
                HostId::new("devbox"),
                CommandBody::ArchiveSession { session_id: flaky },
            )
            .await?;
        self.prompt("studio", &session_id, SAFARI).await?;
        // Its journal starts with the original's finished turn.
        self.daemon("studio")?
            .until(&session_id, finished(2))
            .await?;
        // The children report to the primary before the device looks.
        devbox
            .until(&retention, |events| {
                events
                    .iter()
                    .any(|body| matches!(body, EventBody::ChildReported { .. }))
            })
            .await?;
        Ok(())
    }

    fn daemon(&self, name: &str) -> Result<&Daemon> {
        self.daemons
            .iter()
            .find(|daemon| daemon.id.as_str() == name)
            .with_context(|| format!("no machine {name}"))
    }

    /// Creates a session on `account` of machine `host`, in its clone of `acme/<repo>`, on a new
    /// `branch`, titled `title`.
    async fn session(
        &self,
        host: &str,
        account: &str,
        repo: &str,
        branch: &str,
        title: &str,
    ) -> Result<SessionId> {
        let daemon = self.daemon(host)?;
        let created = self
            .client
            .send(
                daemon.id.clone(),
                CommandBody::CreateSession {
                    repo: Some(daemon.repos[repo].clone()),
                    project_id: None,
                    branch: Some(branch.into()),
                    account_id: Some(AccountId::new(account)),
                    provider: None,
                    model: None,
                    permission_mode: Some(PermissionMode::AutoEdit),
                    failover_pin: None,
                },
            )
            .await?;
        let CommandResult::SessionCreated { session_id } = created else {
            bail!("{host} did not create a session: {created:?}");
        };
        self.client
            .send(
                daemon.id.clone(),
                CommandBody::RenameSession {
                    session_id: session_id.clone(),
                    title: title.into(),
                },
            )
            .await?;
        Ok(session_id)
    }

    async fn prompt(&self, host: &str, session_id: &SessionId, text: &str) -> Result<()> {
        self.client
            .send(
                HostId::new(host),
                CommandBody::SendPrompt {
                    session_id: session_id.clone(),
                    text: text.into(),
                    images: Vec::new(),
                    files: Vec::new(),
                },
            )
            .await?;
        Ok(())
    }
}

/// Whether a session is idle after `turns` finished turns.
fn finished(turns: usize) -> impl Fn(&[EventBody]) -> bool {
    move |events| {
        let completed = events
            .iter()
            .filter(|body| matches!(body, EventBody::TurnCompleted { .. }))
            .count();
        let status = events.iter().rev().find_map(|body| match body {
            EventBody::SessionStatusChanged { status, .. } => Some(*status),
            _ => None,
        });
        completed >= turns && status == Some(SessionStatus::Idle)
    }
}

/// A running machine.
struct Daemon {
    id: HostId,
    link: String,
    /// Its clone of each repository, by name.
    repos: HashMap<&'static str, String>,
    sessions: SessionManager,
    data_dir: PathBuf,
    shutdown: CancellationToken,
    server: JoinHandle<()>,
}

impl Daemon {
    async fn start(
        dir: &Path,
        name: &str,
        cores: u32,
        memory_gib: u64,
        accounts: &[Account],
        origins: &Path,
    ) -> Result<Self> {
        let shutdown = CancellationToken::new();
        std::fs::create_dir_all(dir.join("tls"))?;
        let mut repos = HashMap::new();
        for repo in REPOS {
            repos.insert(*repo, clone(origins, repo, &dir.join("src"))?);
        }
        let tls = Tls::load_or_create(&dir.join("tls"), name)?;
        let auth = Arc::new(Auth::open(dir)?);
        let hub = Arc::new(Hub::default());
        let scripted = Arc::new(Scripted {
            dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/demo"),
        });
        let mut adapters = Adapters::new();
        adapters.register(Provider::Claude, scripted.clone());
        adapters.register(Provider::Codex, scripted);
        let mut configs = Accounts::new();
        for (account, provider, label) in accounts {
            configs.insert(
                AccountId::new(*account),
                AccountConfig {
                    provider: match *provider {
                        "codex" => Provider::Codex,
                        _ => Provider::Claude,
                    },
                    label: (*label).into(),
                    config_dir: Some(dir.join("accounts").join(account)),
                    fallback: false,
                },
            );
        }
        let turns = AtomicU64::new(0);
        let sessions_changed = Arc::new(tokio::sync::Notify::new());
        let setup = Setup {
            store: herder_store::Store::open(dir.join("herder.db"))?,
            adapters,
            accounts: configs,
            sink: Arc::new(OnSessionsChanged {
                next: Arc::clone(&hub) as Arc<dyn EventSink>,
                notify: Arc::clone(&sessions_changed),
            }),
            turn_ids: Box::new(move || {
                TurnId::new(format!("turn-{}", turns.fetch_add(1, Ordering::SeqCst) + 1))
            }),
            worktrees: Worktrees::new(dir.join("worktrees")),
            attachments: dir.join("attachments"),
        };
        let sessions = SessionManager::open(setup, shutdown.clone()).await?;
        sessions.checkpoint_turns(checkpoint::Config {
            dir: dir.join("checkpoints"),
            keep: checkpoint::KEEP,
            push_timeout: TIMEOUT,
        })?;
        // The MCP socket's path must stay short, so it goes in the machine's own directory.
        sessions.serve_mcp(mcp::Config {
            data_dir: dir.to_owned(),
            herder: PathBuf::from("herder"),
        })?;
        let resources = ResourcesConfig {
            max_turns: Some(16),
            ..ResourcesConfig::default()
        };
        let reading = Reading {
            memory_total: memory_gib << 30,
            memory_available: (memory_gib << 30) * 3 / 5,
            load_1m: f64::from(cores) / 5.0,
            cpu_percent: 18.0,
            pressure: None,
        };
        let admission = Arc::new(Admission::new(
            resources.budget(cores),
            Box::new(Steady(reading)),
        ));
        sessions.admit_turns(Arc::clone(&admission))?;
        let config_file = dir.join("daemon.toml");
        std::fs::write(&config_file, "[resources]\nmax_turns = 16\n")?;
        let settings = Arc::new(Settings::new(
            &Config::load_file(&config_file)?,
            Some(Arc::clone(&admission)),
            shutdown.clone(),
        ));
        tokio::spawn({
            let (hub, shutdown) = (Arc::clone(&hub), shutdown.clone());
            async move { admission.run(&hub, shutdown).await }
        });
        let host = Host {
            id: HostId::new(name),
            name: name.into(),
        };
        tokio::spawn(
            Discovery {
                host: host.id.clone(),
                config: Arc::new(Overrides::new(
                    config_file.clone(),
                    dir.join("project-icons"),
                    // Every machine has every project, sessions or not.
                    ProjectsConfig {
                        dir: dir.join("src"),
                        ..ProjectsConfig::default()
                    },
                )),
                hub: Arc::clone(&hub),
                sessions: sessions.clone(),
                sessions_changed,
                data_dir: dir.join("data"),
            }
            .run(shutdown.clone()),
        );
        sessions.fork_from(session::fork::Forks {
            host: host.id.clone(),
            vault: None,
        })?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let link = PairingUri {
            hosts: vec![listener.local_addr()?.to_string()],
            fingerprint: tls.fingerprint().to_owned(),
            code: auth.mint("sam", None, PAIRING_TTL)?.code,
        };
        let terminals = Terminals::new(Arc::clone(&hub), PathBuf::from("/bin/sh"));
        let logins = Logins::new(HashMap::new(), config_file, sessions.clone());
        let server = Server::new(
            tls,
            auth,
            hub,
            sessions.clone(),
            terminals,
            logins,
            host.clone(),
        );
        server.manage_settings(settings)?;
        let server = tokio::spawn(server.run(vec![listener], shutdown.clone()));
        Ok(Self {
            id: host.id,
            link: link.to_string(),
            repos,
            sessions,
            data_dir: dir.to_owned(),
            shutdown,
            server,
        })
    }

    /// Waits until the events of `session_id` satisfy `done`.
    async fn until(
        &self,
        session_id: &SessionId,
        done: impl Fn(&[EventBody]) -> bool,
    ) -> Result<()> {
        tokio::time::timeout(TIMEOUT, async {
            loop {
                let events: Vec<EventBody> = self
                    .sessions
                    .read_since(session_id, 0, usize::MAX)
                    .await?
                    .into_iter()
                    .map(|event| event.body)
                    .collect();
                if done(&events) {
                    return Ok(());
                }
                if let Some(EventBody::SessionStatusChanged {
                    status: SessionStatus::Error,
                    ..
                }) = events
                    .iter()
                    .rev()
                    .find(|body| matches!(body, EventBody::SessionStatusChanged { .. }))
                {
                    bail!("session {session_id} on {} failed: {events:?}", self.id);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .with_context(|| format!("waiting on session {session_id} on {}", self.id))?
    }

    /// Spawns a child of `primary` through herder's MCP server, as the primary's CLI would.
    async fn spawn(&self, primary: &SessionId, task: &str, prompt: &str) -> Result<()> {
        let (mut input, shim_input) = tokio::io::duplex(1 << 16);
        let (shim_output, output) = tokio::io::duplex(1 << 16);
        let (dir, session) = (self.data_dir.clone(), primary.clone());
        let shim =
            tokio::spawn(async move { mcp::shim(&dir, &session, shim_input, shim_output).await });
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "spawn", "arguments": { "task": task, "prompt": prompt } },
        });
        input.write_all(format!("{request}\n").as_bytes()).await?;
        let mut line = String::new();
        tokio::time::timeout(TIMEOUT, BufReader::new(output).read_line(&mut line))
            .await
            .context("spawn did not answer")??;
        drop(input);
        let response: Value = serde_json::from_str(&line)?;
        ensure!(
            response["result"]["isError"] != json!(true) && response.get("error").is_none(),
            "spawn failed: {response}"
        );
        shim.await??;
        Ok(())
    }
}

/// A host that always reads the same.
struct Steady(Reading);

impl ReadHost for Steady {
    fn read(&self) -> Result<Reading> {
        Ok(self.0.clone())
    }
}

/// A bare origin per repository in [`REPOS`], each with a commit that holds a README.
fn origins(dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    for repo in REPOS {
        let seed = dir.join(format!("{repo}-seed"));
        std::fs::create_dir(&seed)?;
        git(&seed, &["init", "--quiet", "--initial-branch=main"])?;
        std::fs::write(seed.join("README.md"), format!("# acme/{repo}\n"))?;
        git(&seed, &["add", "README.md"])?;
        git(&seed, &["commit", "--quiet", "-m", "Initial commit"])?;
        let bare = dir.join(format!("{repo}.git"));
        git(
            dir,
            &["clone", "--quiet", "--bare", &path(&seed)?, &path(&bare)?],
        )?;
    }
    Ok(dir.to_owned())
}

/// Clones the origin of `repo` into `<dir>/<repo>`, with GitHub's `acme/<repo>` as its origin,
/// which git rewrites to the bare one so checkpoints reach it; returns the clone's path.
fn clone(origins: &Path, repo: &str, dir: &Path) -> Result<String> {
    std::fs::create_dir_all(dir)?;
    let bare = path(&origins.join(format!("{repo}.git")))?;
    let clone = dir.join(repo);
    git(dir, &["clone", "--quiet", &bare, &path(&clone)?])?;
    let github = format!("git@github.com:acme/{repo}.git");
    git(&clone, &["remote", "set-url", "origin", &github])?;
    git(
        &clone,
        &["config", &format!("url.{bare}.insteadOf"), &github],
    )?;
    path(&clone)
}

fn path(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .with_context(|| format!("{} is not UTF-8", path.display()))
}

fn git(dir: &Path, args: &[&str]) -> Result<()> {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=herder",
            "-c",
            "user.email=herder@example.com",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .status()?;
    ensure!(status.success(), "git {args:?} failed");
    Ok(())
}

/// Plays the script [`SCRIPTS`] names for each prompt, with the account the session's config
/// dir names.
struct Scripted {
    dir: PathBuf,
}

impl Adapter for Scripted {
    fn accepts_images(&self) -> bool {
        true
    }

    fn start(&self, request: StartRequest) -> StartFuture {
        let account = request
            .config_dir
            .as_deref()
            .and_then(Path::file_name)
            .and_then(OsStr::to_str)
            .unwrap_or_default()
            .to_owned();
        let dir = self.dir.clone();
        Box::pin(async move {
            let (commands, received) = mpsc::unbounded_channel();
            let (events, rx) = mpsc::channel(64);
            tokio::spawn(play(dir, account, request.cwd, received, events));
            Ok(AdapterSession {
                capabilities: Capabilities {
                    native_model_switch: true,
                    native_permission_mode_switch: true,
                    reports_usage: true,
                    native_resume: false,
                },
                commands,
                events: rx,
            })
        })
    }
}

/// One step of a demo script: an event to emit, a pause, a wait for an answer or an interrupt,
/// or a file to write in the worktree.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Step {
    Emit(AdapterEvent),
    SleepMs(u64),
    Hold,
    Write { path: PathBuf, text: String },
}

/// Runs a session: each prompt plays its script, until the daemon shuts the session down.
async fn play(
    dir: PathBuf,
    account: String,
    cwd: PathBuf,
    mut commands: mpsc::UnboundedReceiver<AdapterCommand>,
    events: mpsc::Sender<AdapterEvent>,
) {
    let mut error = None;
    while let Some(command) = commands.recv().await {
        match command {
            AdapterCommand::SendPrompt { turn_id, text, .. } => {
                let steps = match script(&dir, &account, &text, &turn_id) {
                    Ok(steps) => steps,
                    Err(err) => {
                        error = Some(herder_protocol::TurnError {
                            class: herder_protocol::ErrorClass::Fatal,
                            message: format!("{err:#}"),
                        });
                        break;
                    }
                };
                if !run(steps, &turn_id, &cwd, &mut commands, &events).await {
                    break;
                }
            }
            AdapterCommand::Shutdown => break,
            _ => {}
        }
    }
    // The reader may be gone already; there is no one left to tell.
    let _ = events.send(AdapterEvent::Exited { error }).await;
}

/// Plays `steps` of turn `turn_id`; `false` once the session is to stop.
async fn run(
    steps: Vec<Step>,
    turn_id: &TurnId,
    cwd: &Path,
    commands: &mut mpsc::UnboundedReceiver<AdapterCommand>,
    events: &mpsc::Sender<AdapterEvent>,
) -> bool {
    for step in steps {
        match step {
            Step::Emit(event) => {
                if events.send(event).await.is_err() {
                    return false;
                }
            }
            Step::SleepMs(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
            Step::Write { path, text } => {
                let path = cwd.join(path);
                let written = path
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| std::fs::write(&path, text));
                if written.is_err() {
                    return false;
                }
            }
            Step::Hold => loop {
                match commands.recv().await {
                    Some(
                        AdapterCommand::AnswerApproval { .. }
                        | AdapterCommand::AnswerQuestion { .. },
                    ) => break,
                    Some(AdapterCommand::Interrupt) => {
                        let turn_id = turn_id.clone();
                        return events
                            .send(AdapterEvent::TurnInterrupted { turn_id })
                            .await
                            .is_ok();
                    }
                    Some(AdapterCommand::Shutdown) | None => return false,
                    Some(_) => {}
                }
            },
        }
    }
    true
}

/// The steps a prompt plays: its script with `$turn` replaced by the turn's id and each
/// `$in_<n>m` by the time `n` minutes from now, else a short reply.
fn script(dir: &Path, account: &str, text: &str, turn_id: &TurnId) -> Result<Vec<Step>> {
    let Some((_, _, file)) = SCRIPTS
        .iter()
        .find(|(scripted, prompt, _)| *scripted == account && *prompt == text)
    else {
        return Ok(reply(turn_id));
    };
    let path = dir.join(file);
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let raw = times(&raw.replace("$turn", turn_id.as_str()))?;
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| serde_json::from_str(line).with_context(|| format!("{file}: {line}")))
        .collect()
}

/// `raw` with each `$in_<n>m` replaced by the time `n` minutes from now.
fn times(raw: &str) -> Result<String> {
    let now = Timestamp::now().as_second();
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find("$in_") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 4..];
        let digits = after.find('m').context("`$in_` without `m`")?;
        let minutes: i64 = after[..digits].parse()?;
        out.push_str(&Timestamp::from_second(now + minutes * 60)?.to_string());
        rest = &after[digits + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// A turn that answers in a line and ends.
fn reply(turn_id: &TurnId) -> Vec<Step> {
    let item = Item {
        agent_message: None,
        follow_up: None,
        parent_call_id: None,
        id: ItemId::new(format!("{turn_id}-reply")),
        turn_id: turn_id.clone(),
        body: ItemBody::AssistantMessage {
            text: "Done.".into(),
        },
    };
    vec![
        Step::Emit(AdapterEvent::TurnStarted {
            turn_id: turn_id.clone(),
        }),
        Step::Emit(AdapterEvent::ItemCompleted { item }),
        Step::Emit(AdapterEvent::TurnCompleted {
            turn_id: turn_id.clone(),
            usage: None,
        }),
    ]
}
