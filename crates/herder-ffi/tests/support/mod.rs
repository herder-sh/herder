//! A daemon for the bindings' tests and samples: in-process, on localhost, with one account
//! on the fake adapter replaying `fixtures/hello.jsonl`, another replaying `fixtures/hold.jsonl`,
//! a third replaying `fixtures/approval.jsonl`, and a git repository to create a session on,
//! with a project skill. Its skill library is a bare repository of its own with
//! [`DEMO_SKILLS`], and [`FakeDaemon::skill_source`] is a repository to import a skill from.
//! It admits [`MAX_TURNS`] turns at once on a host that always has room otherwise, and an
//! owner may change that limit. Adding an OpenCode account runs [`fake_login`], which logs in
//! at once. Its owner, [`OWNER`], and a member, [`MEMBER`], are people with recorded passkeys,
//! there before anyone signs in, so a device signs in as either with its passkey.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, ensure};
use herder_adapters::fake::FakeAdapter;
use herder_client_core::{FleetLink, FleetMachine};
use herder_daemon::auth::Auth;
use herder_daemon::auth::authenticator;
use herder_daemon::login::{LoginProgram, LoginStatus, Logins};
use herder_daemon::resources::{Admission, ReadHost, Reading, ResourcesConfig};
use herder_daemon::session::{AccountConfig, Accounts, Adapters, SessionManager, Setup};
use herder_daemon::settings::Settings;
use herder_daemon::skills::{Skills, SkillsSink};
use herder_daemon::terminal::Terminals;
use herder_daemon::worktree::Worktrees;
use herder_daemon::ws::{Host, Server, Tls};
use herder_daemon::{Config, Hub, session};
use herder_protocol::{AccountId, CommandBody, HostId, Provider, Role, TurnId};
use herder_store::Store;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// The account sessions run on.
pub const ACCOUNT: &str = "fake";

/// An account whose turn runs until it is interrupted, for prompts to queue behind it.
pub const HOLD_ACCOUNT: &str = "hold";

/// An account whose turn waits on an approval, for the apps to show a request.
pub const APPROVAL_ACCOUNT: &str = "approval";

/// An account whose turn asks a question, for the apps to answer with an image.
pub const QUESTION_ACCOUNT: &str = "question";

/// The daemon's owner, by the recorded passkey of that name: [`FakeDaemon::passkey`].
pub const OWNER: &str = "tomas";

/// A member of the daemon, by the recorded passkey of that name: [`FakeDaemon::member_passkey`].
pub const MEMBER: &str = "guest";

/// Turns the daemon runs at once until an owner changes it.
pub const MAX_TURNS: u32 = 4;

/// The skills the library starts with, as names and descriptions.
pub const DEMO_SKILLS: [(&str, &str); 2] = [
    (
        "release-notes",
        "Write release notes from the commits since the last tag.",
    ),
    (
        "review-pr",
        "Review a pull request for correctness, tests and style.",
    ),
];

/// The project skill checked in to the repository, at `.claude/skills/deploy`.
pub const PROJECT_SKILL: (&str, &str) = ("deploy", "Deploy the app to staging.");

/// The skill [`FakeDaemon::skill_source`] holds, in a folder of that name.
pub const IMPORTED_SKILL: (&str, &str) = ("changelog", "Keep CHANGELOG.md up to date.");

/// A host with room for every turn: 8 cores, 16 GiB, half of it available.
struct Roomy;

impl ReadHost for Roomy {
    fn read(&self) -> Result<Reading> {
        Ok(Reading {
            memory_total: 16 << 30,
            memory_available: 8 << 30,
            load_1m: 1.0,
            cpu_percent: 12.0,
            pressure: None,
        })
    }
}

/// A running daemon; [`FakeDaemon::stop`] stops it and removes everything it created.
pub struct FakeDaemon {
    /// The `herder://fleet` link of the daemon, which a device signs in to with a passkey.
    pub link: String,
    /// The owner's passkey, [`OWNER`], as `Authenticator::export` gives it.
    // The fake_daemon example prints the passkeys; the tests sign people in through `auth`.
    #[allow(dead_code)]
    pub passkey: String,
    /// The member's passkey, [`MEMBER`], likewise.
    #[allow(dead_code)]
    pub member_passkey: String,
    /// Who is on the daemon, for a test to sign people in to it.
    pub auth: Arc<Auth>,
    /// Absolute path of a git repository with one commit, holding [`PROJECT_SKILL`].
    pub repo: String,
    /// Absolute path of a git repository with [`IMPORTED_SKILL`] in a folder of its name.
    pub skill_source: String,
    shutdown: CancellationToken,
    server: JoinHandle<()>,
    _tmp: TempDir,
}

impl FakeDaemon {
    /// Starts the daemon of host `name` on the current tokio runtime.
    pub async fn start(name: &str) -> Result<Self> {
        let tmp = tempfile::tempdir()?;
        let dir = tmp.path().join("daemon");
        let app = repo(&tmp.path().join("app"), &[PROJECT_SKILL], ".claude/skills/")?;
        let library = library(tmp.path())?;
        let skill_source = repo(&tmp.path().join("imports"), &[IMPORTED_SKILL], "")?;
        std::fs::create_dir_all(dir.join("tls"))?;

        let tls = Tls::load_or_create(&dir.join("tls"), name)?;
        let auth = Arc::new(Auth::open(&dir)?);
        let hub = Arc::new(Hub::default());
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        let mut adapters = Adapters::new();
        let mut accounts = Accounts::new();
        for (account, label, script) in [
            (ACCOUNT, "Fake", "hello.jsonl"),
            (HOLD_ACCOUNT, "Hold", "hold.jsonl"),
            (APPROVAL_ACCOUNT, "Approval", "approval.jsonl"),
            (QUESTION_ACCOUNT, "Question", "question.jsonl"),
        ] {
            // Each account has a provider of its own, as a provider has one script.
            let provider = Provider::Other(account.into());
            adapters.register(
                provider.clone(),
                Arc::new(FakeAdapter::new(fixtures.join(script))),
            );
            accounts.insert(
                AccountId::new(account),
                AccountConfig {
                    provider,
                    label: label.into(),
                    config_dir: Some(dir.join(account)),
                    fallback: false,
                },
            );
        }
        let turns = AtomicU64::new(0);
        let setup = Setup {
            store: Store::open(dir.join("herder.db"))?,
            adapters,
            accounts,
            sink: Arc::clone(&hub) as Arc<dyn session::EventSink>,
            turn_ids: Box::new(move || {
                TurnId::new(format!("turn-{}", turns.fetch_add(1, Ordering::SeqCst) + 1))
            }),
            worktrees: Worktrees::new(dir.join("worktrees")),
            attachments: dir.join("attachments"),
            chats: dir.join("chats"),
        };
        let shutdown = CancellationToken::new();
        let sessions = SessionManager::open(setup, shutdown.clone()).await?;
        let resources = ResourcesConfig {
            max_turns: Some(MAX_TURNS),
            ..ResourcesConfig::default()
        };
        let admission = Arc::new(Admission::new(resources.budget(8), Box::new(Roomy)));
        sessions.admit_turns(Arc::clone(&admission))?;
        let skills = Arc::new(Skills::open(
            &dir,
            &sessions.providers(),
            Arc::clone(&hub) as Arc<dyn SkillsSink>,
        )?);
        skills
            .command(CommandBody::SetSkillsRepo { url: library })
            .await
            .map_err(|err| anyhow::anyhow!("cannot set the skill library: {}", err.message))?;
        sessions.deliver_skills(skills)?;
        // The config file holds the turn limit, so the app can change it and the settings.
        let config_file = dir.join("daemon.toml");
        std::fs::write(
            &config_file,
            format!("[resources]\nmax_turns = {MAX_TURNS}\n"),
        )?;
        let settings = Arc::new(Settings::new(
            &Config::load_file(&config_file)?,
            Some(Arc::clone(&admission)),
            shutdown.clone(),
        ));
        sessions.use_mcp_servers(settings.mcp_servers())?;
        tokio::spawn({
            let (hub, shutdown) = (Arc::clone(&hub), shutdown.clone());
            async move { admission.run(&hub, shutdown).await }
        });
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let link = FleetLink {
            machines: vec![FleetMachine {
                hosts: vec![listener.local_addr()?.to_string()],
                fingerprint: tls.fingerprint().to_owned(),
            }],
        }
        .to_string();
        let (passkey, member_passkey) = (
            enroll(&auth, OWNER, Role::Owner),
            enroll(&auth, MEMBER, Role::Member),
        );
        let terminals = Terminals::new(Arc::clone(&hub), PathBuf::from("/bin/sh"));
        let logins = Logins::new(
            HashMap::from([(Provider::Opencode, fake_login())]),
            dir.join("daemon.toml"),
            sessions.clone(),
        );
        let host = Host {
            id: HostId::new(name),
            name: name.into(),
        };
        sessions.fork_from(session::fork::Forks {
            host: host.id.clone(),
        })?;
        let server = Server::new(
            tls,
            Arc::clone(&auth),
            hub,
            sessions,
            terminals,
            logins,
            host,
        );
        server.manage_settings(settings)?;
        let server = tokio::spawn(server.run(vec![listener], shutdown.clone()));
        Ok(Self {
            link,
            passkey,
            member_passkey,
            auth,
            repo: app,
            skill_source,
            shutdown,
            server,
            _tmp: tmp,
        })
    }

    /// Stops the daemon and waits until it has.
    pub async fn stop(self) -> Result<()> {
        self.shutdown.cancel();
        Ok(self.server.await?)
    }
}

/// A login that succeeds at once: it marks the account's config dir logged in, which its status
/// check then reports. Give it a config dir, or it logs in under the user's own home.
fn fake_login() -> LoginProgram {
    let script = |script: &str| vec!["-c".to_owned(), script.to_owned()];
    LoginProgram {
        program: PathBuf::from("/bin/sh"),
        args: script(r#"touch "$XDG_DATA_HOME/logged-in""#),
        config_env: vec!["XDG_DATA_HOME".into()],
        status: LoginStatus {
            args: script(r#"[ -e "$XDG_DATA_HOME/logged-in" ]"#),
            logged_in_field: None,
        },
    }
}

/// Puts the person `name` on `auth` as `role`, with their recorded passkey, as though a device
/// of theirs had signed in; their passkey, exported for a client to sign with. They are the
/// same person on every fake daemon, so one passkey signs in to all of them.
pub fn enroll(auth: &Auth, name: &str, role: Role) -> String {
    let (passkey, _) = authenticator::enroll_known(auth, name, role, &format!("{name}-enrolled"));
    passkey.export()
}

/// A git repository with one commit holding `skills`, each a `SKILL.md` in `<prefix><name>`.
fn repo(dir: &Path, skills: &[(&str, &str)], prefix: &str) -> Result<String> {
    std::fs::create_dir(dir)?;
    git(dir, &["init", "--quiet", "--initial-branch=main"])?;
    for (name, description) in skills {
        let folder = dir.join(format!("{prefix}{name}"));
        std::fs::create_dir_all(&folder)?;
        std::fs::write(
            folder.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\n{description}\n"),
        )?;
    }
    git(dir, &["add", "--all"])?;
    git(dir, &["commit", "--quiet", "--allow-empty", "-m", "init"])?;
    dir.to_str()
        .map(str::to_owned)
        .context("the repository path is not UTF-8")
}

/// A bare repository in `tmp` with [`DEMO_SKILLS`], for the skill library to push to.
fn library(tmp: &Path) -> Result<String> {
    let work = repo(&tmp.join("skills-work"), &DEMO_SKILLS, "")?;
    let bare = tmp.join("skills.git");
    git(
        tmp,
        &["clone", "--quiet", "--bare", &work, &bare.to_string_lossy()],
    )?;
    bare.to_str()
        .map(str::to_owned)
        .context("the library path is not UTF-8")
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
