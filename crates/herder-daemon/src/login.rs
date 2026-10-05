//! Logins: adding an account by running its provider's own login in a login terminal.
//!
//! Each login runs in the account's config dir, a new one herder creates, or one the owner names:
//! empty, or already holding a login, but no other account's. The dir is handed to the
//! provider's CLI through its config dir variables, on a pseudo-terminal relayed to the owner who asked ([`crate::terminal`]). The
//! owner completes the provider's own flow there, a device code or a URL to open and a code to
//! paste back, so it works from another machine; nothing relies on a callback to localhost.
//! herder never reads what the login writes.
//!
//! - Claude: `claude` itself, whose first run in an empty config dir walks through `/login`;
//!   the owner exits it once logged in.
//! - Codex: `codex login --device-auth`.
//! - Cursor: `agent login`.
//! - OpenCode: `opencode auth login`, with `XDG_DATA_HOME` at the config dir, where it writes
//!   `opencode/auth.json`. The owner picks a model provider and pastes its API key or follows
//!   its device flow; a provider whose login waits for a browser callback to localhost only
//!   works on the daemon's own machine.
//!
//! herder never takes a login's exit at its word: quitting `claude` before logging in exits with
//! 0 too, and `claude` keeps running once logged in. It asks the provider's own CLI whether the
//! config dir is logged in, with a check that changes nothing ([`LoginStatus`]): `claude auth
//! status --json`, `codex login status`, `agent status --format json`, `opencode auth list`;
//! every few seconds while the login runs, hanging the login up as soon as it says so, so a dir
//! logged in already is added within seconds, and once more when the login ends, however it
//! ends. Only when it says so is the account added: it is appended to the daemon's config file
//! ([`crate::config::append_account`]), which stays the one list of accounts, then offered to
//! sessions and announced to clients. A login that
//! fails, or that the check finds logged out, adds nothing, and removes the config dir if
//! herder created it. Either way the outcome is the terminal's last line.
//!
//! Owner-only access is enforced before commands get here, by [`crate::auth::authorize`].

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::ErrorKind;
use std::io::Read;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use herder_adapters::acp::AgentProfile;
use herder_protocol::{Account, AccountId, ErrorCode, ErrorInfo, Provider};
use portable_pty::CommandBuilder;
use tracing::{info, warn};

use crate::config::{self, ID_RULE, resolve_path, valid_id};
use crate::session::{AccountConfig, SessionManager};

/// How long the login status check may take.
const STATUS_TIMEOUT: Duration = Duration::from_secs(30);

/// How to log in to one provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginProgram {
    /// The CLI to run.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<String>,
    /// The variables that point the CLI at the account's config dir.
    pub config_env: Vec<String>,
    /// How the same CLI tells whether the login succeeded.
    pub status: LoginStatus,
}

/// A check, by the provider's own CLI, of whether a config dir is logged in; it changes no
/// login and herder reads only its answer, never the credentials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginStatus {
    /// The arguments to the login's program.
    pub args: Vec<String>,
    /// The boolean field of the JSON object the check prints that is `true` when logged in;
    /// without one, exit status 0 means logged in.
    pub logged_in_field: Option<String>,
}

/// The login of each provider herder can add accounts of, running the binary `binaries` names
/// for it, else the provider's own CLI on `PATH`.
pub fn programs(binaries: &HashMap<Provider, PathBuf>) -> HashMap<Provider, LoginProgram> {
    let login = |provider: Provider,
                 default: &str,
                 args: &[&str],
                 config_env: &[&str],
                 (status, field): (&[&str], Option<&str>)| {
        let program = binaries
            .get(&provider)
            .cloned()
            .unwrap_or_else(|| PathBuf::from(default));
        let strings = |values: &[&str]| values.iter().map(|&v| v.to_owned()).collect();
        let program = LoginProgram {
            program,
            args: strings(args),
            config_env: strings(config_env),
            status: LoginStatus {
                args: strings(status),
                logged_in_field: field.map(str::to_owned),
            },
        };
        (provider, program)
    };
    HashMap::from([
        login(
            Provider::Claude,
            "claude",
            &[],
            &["CLAUDE_CONFIG_DIR"],
            (&["auth", "status", "--json"], Some("loggedIn")),
        ),
        login(
            Provider::Codex,
            "codex",
            &["login", "--device-auth"],
            &["CODEX_HOME"],
            (&["login", "status"], None),
        ),
        // `agent status` exits with 0 logged out too.
        login(
            Provider::Cursor,
            &AgentProfile::cursor().program,
            &["login"],
            &["CURSOR_CONFIG_DIR", "XDG_CONFIG_HOME"],
            (&["status", "--format", "json"], Some("isAuthenticated")),
        ),
        // `opencode auth list` has no JSON and exits with 0 logged out too; OpenCode runs its
        // own free models with no login, so an account without one still works.
        login(
            Provider::Opencode,
            &AgentProfile::opencode().program,
            &["auth", "login"],
            &["XDG_DATA_HOME"],
            (&["auth", "list"], None),
        ),
    ])
}

/// Adds accounts through their providers' logins. Cheap to clone. The default adds none.
#[derive(Clone, Default)]
pub struct Logins {
    inner: Option<Arc<Inner>>,
}

struct Inner {
    programs: HashMap<Provider, LoginProgram>,
    /// The daemon's config file, where added accounts are saved.
    config_file: PathBuf,
    sessions: SessionManager,
    /// Held while an account is saved, so two logins ending at once both land.
    saving: Mutex<()>,
}

/// An account to add, as the owner asked for it.
pub(crate) struct NewAccount<'a> {
    pub account_id: &'a AccountId,
    pub provider: &'a Provider,
    pub label: Option<&'a str>,
    pub config_dir: Option<&'a str>,
}

/// A login about to run: the command, and what to do once it exits.
pub(crate) struct Login {
    /// The provider's login, in the account's config dir.
    pub command: CommandBuilder,
    /// Adds the account if the login succeeded.
    pub pending: Pending,
}

/// An account waiting for its login to end.
pub(crate) struct Pending {
    inner: Arc<Inner>,
    account_id: AccountId,
    account: AccountConfig,
    /// The login, whose status check tells whether it succeeded.
    program: LoginProgram,
    /// Whether herder created the config dir, and so may remove it.
    created: bool,
}

impl Logins {
    /// Logins running `programs`, saving accounts to `config_file` and adding them to
    /// `sessions`. No other provider's accounts can be added.
    pub fn new(
        programs: HashMap<Provider, LoginProgram>,
        config_file: PathBuf,
        sessions: SessionManager,
    ) -> Self {
        Self {
            inner: Some(Arc::new(Inner {
                programs,
                config_file,
                sessions,
                saving: Mutex::new(()),
            })),
        }
    }

    pub(crate) async fn set_settings(
        &self,
        account_id: &AccountId,
        label: &str,
        config_dir: Option<&str>,
    ) -> Result<(), ErrorInfo> {
        let inner = self.inner.as_ref().ok_or_else(|| {
            error(
                ErrorCode::Unsupported,
                "account settings are unavailable".into(),
            )
        })?;
        inner
            .sessions
            .configure_account(account_id, |previous, may_change_directory| {
                let _saving = inner.saving.lock().unwrap_or_else(PoisonError::into_inner);
                config::set_account_settings(
                    &inner.config_file,
                    account_id,
                    previous,
                    label,
                    config_dir,
                    may_change_directory,
                )
                .map_err(|err| error(ErrorCode::BadRequest, format!("{err:#}")))
            })
            .await
    }

    /// The login of `account`, in its config dir, which is created empty if it does not exist.
    /// `logging_in` are the accounts with a login running; the new id must be neither one of
    /// them nor an account already.
    pub(crate) fn start(
        &self,
        account: &NewAccount<'_>,
        logging_in: &[AccountId],
    ) -> Result<Login, ErrorInfo> {
        self.start_with_env(account, logging_in, |key| std::env::var_os(key))
    }

    fn start_with_env(
        &self,
        account: &NewAccount<'_>,
        logging_in: &[AccountId],
        env: impl Fn(&str) -> Option<OsString>,
    ) -> Result<Login, ErrorInfo> {
        let NewAccount {
            account_id,
            provider,
            label,
            config_dir,
        } = account;
        let unsupported = || {
            error(
                ErrorCode::Unsupported,
                format!(
                    "herder cannot add {} accounts; add them to the daemon's config",
                    provider.as_str()
                ),
            )
        };
        let inner = self.inner.as_ref().ok_or_else(unsupported)?;
        if !valid_id(account_id.as_str()) {
            return Err(error(
                ErrorCode::BadRequest,
                format!("account id {:?} {ID_RULE}", account_id.as_str()),
            ));
        }
        let accounts: Vec<Account> = inner.sessions.accounts();
        if accounts.iter().any(|a| a.account_id == **account_id) || logging_in.contains(account_id)
        {
            return Err(error(
                ErrorCode::Conflict,
                format!("account {account_id} already exists"),
            ));
        }
        let program = inner.programs.get(provider).ok_or_else(unsupported)?;
        let default = format!("~/.{}-{account_id}", provider.as_str());
        let dir = resolve_path(Path::new(config_dir.unwrap_or(&default)), &env)
            .map_err(|err| error(ErrorCode::BadRequest, format!("config dir: {err:#}")))?;
        if let Some(other) = accounts.iter().find(|a| {
            a.config_dir.as_deref().is_some_and(|other| {
                resolve_path(Path::new(other), &env).is_ok_and(|other| other == dir)
            })
        }) {
            return Err(error(
                ErrorCode::Conflict,
                format!(
                    "{} is the config dir of account {}",
                    dir.display(),
                    other.account_id
                ),
            ));
        }
        let created = make_dir(&dir)?;
        let mut command = CommandBuilder::new(&program.program);
        command.args(&program.args);
        command.cwd(&dir);
        for var in &program.config_env {
            command.env(var, &dir);
        }
        let account = AccountConfig {
            provider: (*provider).clone(),
            label: label.map_or_else(|| account_id.to_string(), str::to_owned),
            config_dir: Some(dir),
        };
        Ok(Login {
            command,
            pending: Pending {
                inner: Arc::clone(inner),
                account_id: (*account_id).clone(),
                account,
                program: program.clone(),
                created,
            },
        })
    }
}

impl Pending {
    /// Whether the provider reports the config dir logged in, so the login can be hung up. Runs
    /// the status check, so it blocks.
    pub(crate) fn check(&self) -> impl Fn() -> bool + Send + 'static {
        let program = self.program.clone();
        let dir = self.account.config_dir.clone().unwrap_or_default();
        move || logged_in(&program, &dir) == Ok(true)
    }

    /// Adds the account if the provider reports the config dir logged in, however the login
    /// ended; returns the outcome, as a line for the login terminal. Runs the status check, so
    /// it blocks.
    pub(crate) fn finish(self, exit_code: Option<i32>) -> String {
        let Pending {
            inner,
            account_id,
            account,
            program,
            created,
        } = self;
        let dir = account.config_dir.clone().unwrap_or_default();
        let exited = match exit_code {
            Some(0) => "the login exited".to_owned(),
            Some(code) => format!("the login exited with {code}"),
            None => "the login ended".to_owned(),
        };
        let failure = match logged_in(&program, &dir) {
            Ok(true) => None,
            Ok(false) => Some(format!(
                "{exited}, but {} reports no login in {}",
                account.provider.as_str(),
                dir.display()
            )),
            Err(err) => Some(format!("{exited}, but {err}")),
        };
        if let Some(failure) = failure {
            if created && let Err(err) = std::fs::remove_dir_all(&dir) {
                warn!(%account_id, "cannot remove {}: {err}", dir.display());
            }
            return line(&format!("{failure}; account {account_id} was not added"));
        }
        let _saving = inner.saving.lock().unwrap_or_else(PoisonError::into_inner);
        if let Err(err) = config::append_account(&inner.config_file, &account_id, &account) {
            warn!(%account_id, "cannot save the account: {err:#}");
            return line(&format!(
                "logged in, but cannot add account {account_id}: {err:#}"
            ));
        }
        if !inner.sessions.add_account(account_id.clone(), account) {
            return line(&format!(
                "account {account_id} was saved, but one with its id already runs; restart the daemon"
            ));
        }
        info!(%account_id, config_file = %inner.config_file.display(), "account added");
        line(&format!("added account {account_id}"))
    }
}

/// Whether `program`'s status check finds `dir` logged in; an error says why it could not tell.
fn logged_in(program: &LoginProgram, dir: &Path) -> Result<bool, String> {
    let name = program.program.display();
    let mut command = Command::new(&program.program);
    command
        .args(&program.status.args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for var in &program.config_env {
        command.env(var, dir);
    }
    let mut child = command
        .spawn()
        .map_err(|err| format!("cannot check its status with {name}: {err}"))?;
    let deadline = Instant::now() + STATUS_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "its status check did not answer within {} s",
                    STATUS_TIMEOUT.as_secs()
                ));
            }
            Err(err) => return Err(format!("cannot wait for its status check: {err}")),
        }
    };
    let Some(field) = &program.status.logged_in_field else {
        return Ok(status.success());
    };
    let mut stdout = String::new();
    if let Some(mut out) = child.stdout.take() {
        out.read_to_string(&mut stdout)
            .map_err(|err| format!("cannot read its status check: {err}"))?;
    }
    let answer: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|_| format!("its status check printed no JSON: {}", stdout.trim()))?;
    Ok(answer.get(field).and_then(serde_json::Value::as_bool) == Some(true))
}

/// `text` as a line of herder's own in a terminal.
fn line(text: &str) -> String {
    format!("\r\nherder: {text}\r\n")
}

/// Creates `dir`, owner-only, unless it is a directory already, empty or holding a login.
/// Returns whether it created it.
fn make_dir(dir: &Path) -> Result<bool, ErrorInfo> {
    match std::fs::read_dir(dir) {
        Ok(_) => return Ok(false),
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => {
            return Err(error(
                ErrorCode::BadRequest,
                format!("config dir {}: {err}", dir.display()),
            ));
        }
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|err| {
            error(
                ErrorCode::Internal,
                format!("cannot create {}: {err}", dir.display()),
            )
        })?;
    Ok(true)
}

fn error(code: ErrorCode, message: String) -> ErrorInfo {
    ErrorInfo { code, message }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use herder_store::Store;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::hub::Hub;
    use crate::session::{self, Accounts, Adapters, Setup};
    use crate::worktree::Worktrees;

    struct Fixture {
        home: tempfile::TempDir,
        logins: Logins,
        sessions: SessionManager,
        hub: Arc<Hub>,
    }

    /// Status checks standing in for the providers' own: codex's answers with its exit status,
    /// claude's with JSON, both from whether the login left `logged-in` in the config dir.
    fn fake_programs() -> HashMap<Provider, LoginProgram> {
        let mut programs = programs(&HashMap::new());
        let fake = |program: &mut LoginProgram, script: &str| {
            program.program = PathBuf::from("/bin/sh");
            program.status.args = vec!["-c".into(), script.into()];
        };
        fake(
            programs.get_mut(&Provider::Codex).unwrap(),
            r#"[ -e "$CODEX_HOME/logged-in" ]"#,
        );
        fake(
            programs.get_mut(&Provider::Claude).unwrap(),
            r#"if [ -e "$CLAUDE_CONFIG_DIR/logged-in" ]; then v=true; else v=false; fi
               echo "{\"loggedIn\": $v, \"authMethod\": \"none\"}""#,
        );
        programs
    }

    /// Logins on a daemon with one codex account, `codex`, in the default location.
    async fn fixture() -> Fixture {
        let home = tempfile::tempdir().unwrap();
        let hub = Arc::new(Hub::default());
        let accounts = Accounts::from([(
            AccountId::new("codex"),
            AccountConfig {
                provider: Provider::Codex,
                label: "Codex".into(),
                config_dir: None,
            },
        )]);
        let setup = Setup {
            store: Store::open(home.path().join("herder.db")).unwrap(),
            adapters: Adapters::new(),
            accounts,
            sink: Arc::clone(&hub) as Arc<dyn session::EventSink>,
            turn_ids: session::ulid_turn_ids(),
            worktrees: Worktrees::new(home.path().join("worktrees")),
            attachments: home.path().join("attachments"),
        };
        let sessions = SessionManager::open(setup, CancellationToken::new())
            .await
            .unwrap();
        let logins = Logins::new(
            fake_programs(),
            home.path().join("daemon.toml"),
            sessions.clone(),
        );
        Fixture {
            home,
            logins,
            sessions,
            hub,
        }
    }

    impl Fixture {
        fn start(
            &self,
            id: &str,
            provider: Provider,
            config_dir: Option<&str>,
        ) -> Result<Login, ErrorInfo> {
            let home = self.home.path().as_os_str().to_owned();
            let account_id = AccountId::new(id);
            let account = NewAccount {
                account_id: &account_id,
                provider: &provider,
                label: None,
                config_dir,
            };
            let logging_in = [AccountId::new("busy")];
            self.logins
                .start_with_env(&account, &logging_in, move |key| {
                    (key == "HOME").then(|| home.clone())
                })
        }
    }

    #[test]
    fn each_provider_logs_in_with_its_own_cli_and_config_dir_variables() {
        let binaries = HashMap::from([(Provider::Codex, PathBuf::from("/opt/codex"))]);
        let programs = programs(&binaries);
        let argv = |provider: &Provider| {
            let program = &programs[provider];
            let mut argv = vec![program.program.to_string_lossy().into_owned()];
            argv.extend(program.args.iter().cloned());
            (argv, program.config_env.clone())
        };
        assert_eq!(
            argv(&Provider::Claude),
            (vec!["claude".into()], vec!["CLAUDE_CONFIG_DIR".into()])
        );
        assert_eq!(
            argv(&Provider::Codex),
            (
                vec!["/opt/codex".into(), "login".into(), "--device-auth".into()],
                vec!["CODEX_HOME".into()]
            )
        );
        assert_eq!(
            argv(&Provider::Cursor),
            (
                vec!["agent".into(), "login".into()],
                vec!["CURSOR_CONFIG_DIR".into(), "XDG_CONFIG_HOME".into()]
            )
        );
        assert_eq!(
            argv(&Provider::Opencode),
            (
                vec!["opencode".into(), "auth".into(), "login".into()],
                vec!["XDG_DATA_HOME".into()]
            )
        );
        assert_eq!(programs.len(), 4);
        let status = |provider: &Provider| {
            let status = &programs[provider].status;
            (status.args.join(" "), status.logged_in_field.as_deref())
        };
        assert_eq!(
            status(&Provider::Claude),
            ("auth status --json".into(), Some("loggedIn"))
        );
        assert_eq!(status(&Provider::Codex), ("login status".into(), None));
        assert_eq!(
            status(&Provider::Cursor),
            ("status --format json".into(), Some("isAuthenticated"))
        );
        assert_eq!(status(&Provider::Opencode), ("auth list".into(), None));
    }

    #[tokio::test]
    async fn a_login_runs_in_a_fresh_owner_only_config_dir() {
        let f = fixture().await;
        let login = f.start("cursor-2", Provider::Cursor, None).unwrap();
        let dir = f.home.path().join(".cursor-cursor-2");
        assert_eq!(login.command.get_argv(), &["agent", "login"]);
        for var in ["CURSOR_CONFIG_DIR", "XDG_CONFIG_HOME"] {
            assert_eq!(login.command.get_env(var), Some(dir.as_os_str()));
        }
        assert_eq!(login.command.get_cwd(), Some(&dir.as_os_str().to_owned()));
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        // An empty dir is taken, and so is one a login was made in already.
        f.start("cursor-3", Provider::Cursor, Some("~/.cursor-cursor-2"))
            .unwrap();
        let kept = f.home.path().join("kept");
        std::fs::create_dir(&kept).unwrap();
        std::fs::write(kept.join("logged-in"), "").unwrap();
        let login = f.start("codex-4", Provider::Codex, Some("~/kept")).unwrap();
        assert!(login.pending.check()());
        assert_eq!(
            login.pending.finish(None),
            "\r\nherder: added account codex-4\r\n"
        );
        assert!(kept.join("logged-in").exists());
    }

    #[tokio::test]
    async fn a_login_never_reuses_an_id_or_a_config_dir_in_use() {
        let f = fixture().await;
        let used = f.home.path().join("used");
        std::fs::create_dir(&used).unwrap();
        f.sessions.add_account(
            AccountId::new("used"),
            AccountConfig {
                provider: Provider::Codex,
                label: "Used".into(),
                config_dir: Some(used),
            },
        );
        let cases = [
            ("codex", Provider::Codex, None, ErrorCode::Conflict),
            ("busy", Provider::Codex, None, ErrorCode::Conflict),
            ("a b", Provider::Codex, None, ErrorCode::BadRequest),
            ("new", Provider::Grok, None, ErrorCode::Unsupported),
            ("new", Provider::Codex, Some("rel"), ErrorCode::BadRequest),
            ("new", Provider::Codex, Some("~/used"), ErrorCode::Conflict),
        ];
        for (id, provider, dir, code) in cases {
            let err = f
                .start(id, provider, dir)
                .err()
                .unwrap_or_else(|| panic!("{id} {dir:?} was accepted"));
            assert_eq!(err.code, code, "{id} {dir:?}: {}", err.message);
        }
        assert!(!f.home.path().join(".codex-new").exists());
        let err = Logins::default()
            .start_with_env(
                &NewAccount {
                    account_id: &AccountId::new("x"),
                    provider: &Provider::Claude,
                    label: None,
                    config_dir: None,
                },
                &[],
                |_| None,
            )
            .err()
            .unwrap();
        assert_eq!(err.code, ErrorCode::Unsupported);
    }

    #[tokio::test]
    async fn a_successful_login_saves_and_adds_the_account() {
        let f = fixture().await;
        let login = f.start("codex-2", Provider::Codex, None).unwrap();
        std::fs::write(f.home.path().join(".codex-codex-2/logged-in"), "").unwrap();
        let line = login.pending.finish(Some(0));
        assert_eq!(line, "\r\nherder: added account codex-2\r\n");
        let ids: Vec<_> = f
            .sessions
            .accounts()
            .into_iter()
            .map(|account| account.account_id)
            .collect();
        assert_eq!(ids, [AccountId::new("codex"), AccountId::new("codex-2")]);
        let saved = std::fs::read_to_string(f.home.path().join("daemon.toml")).unwrap();
        let dir = f.home.path().join(".codex-codex-2");
        assert_eq!(
            saved,
            format!(
                "[[accounts]]\nid = \"codex-2\"\nprovider = \"codex\"\nlabel = \"codex-2\"\n\
                 config_dir = \"{}\"\n",
                dir.display()
            )
        );
        assert!(dir.is_dir());
        // A second account with the id cannot start.
        let err = f.start("codex-2", Provider::Codex, None).err().unwrap();
        assert_eq!(err.code, ErrorCode::Conflict);
    }

    #[tokio::test]
    async fn a_failed_login_adds_nothing_and_removes_the_dir_it_created() {
        let f = fixture().await;
        let login = f.start("codex-2", Provider::Codex, None).unwrap();
        let dir = f.home.path().join(".codex-codex-2");
        std::fs::write(dir.join("partial"), "").unwrap();
        let line = login.pending.finish(Some(1));
        assert!(
            line.contains("exited with 1, but codex reports no login"),
            "{line}"
        );
        assert!(line.contains("account codex-2 was not added"), "{line}");
        assert!(!dir.exists());
        assert_eq!(f.sessions.accounts().len(), 1);
        assert!(!f.home.path().join("daemon.toml").exists());

        // A dir the owner made is kept.
        let own = f.home.path().join("own");
        std::fs::create_dir(&own).unwrap();
        let login = f.start("codex-3", Provider::Codex, Some("~/own")).unwrap();
        login.pending.finish(None);
        assert!(own.is_dir());
    }

    #[tokio::test]
    async fn a_login_that_exits_0_without_logging_in_adds_nothing() {
        let f = fixture().await;
        // Codex says so with its exit status.
        let login = f.start("codex-2", Provider::Codex, None).unwrap();
        let line = login.pending.finish(Some(0));
        let dir = f.home.path().join(".codex-codex-2");
        assert_eq!(
            line,
            format!(
                "\r\nherder: the login exited, but codex reports no login in {}; account \
                 codex-2 was not added\r\n",
                dir.display()
            )
        );
        assert!(!dir.exists());

        // Claude says so in JSON, as `claude` quit before `/login` does.
        let login = f.start("claude-2", Provider::Claude, None).unwrap();
        let line = login.pending.finish(Some(0));
        assert!(line.contains("claude reports no login"), "{line}");
        assert!(!f.home.path().join(".claude-claude-2").exists());
        assert_eq!(f.sessions.accounts().len(), 1);
        assert!(!f.home.path().join("daemon.toml").exists());

        // Logged in, Claude's JSON adds it.
        let login = f.start("claude-3", Provider::Claude, None).unwrap();
        std::fs::write(f.home.path().join(".claude-claude-3/logged-in"), "").unwrap();
        let line = login.pending.finish(Some(0));
        assert_eq!(line, "\r\nherder: added account claude-3\r\n");
    }

    #[tokio::test]
    async fn a_login_hung_up_once_logged_in_adds_the_account() {
        let f = fixture().await;
        let login = f.start("claude-2", Provider::Claude, None).unwrap();
        assert!(!login.pending.check()());
        std::fs::write(f.home.path().join(".claude-claude-2/logged-in"), "").unwrap();
        assert!(login.pending.check()());
        // `claude` keeps running once logged in; hung up, it ends with a signal.
        let line = login.pending.finish(None);
        assert_eq!(line, "\r\nherder: added account claude-2\r\n");
    }

    #[tokio::test]
    async fn a_status_check_that_cannot_run_adds_nothing() {
        let f = fixture().await;
        let mut login = f.start("codex-2", Provider::Codex, None).unwrap();
        login.pending.program.program = PathBuf::from("/nonexistent/codex");
        let line = login.pending.finish(Some(0));
        assert!(
            line.contains("cannot check its status with /nonexistent/codex"),
            "{line}"
        );
        assert!(line.contains("account codex-2 was not added"), "{line}");
        assert_eq!(f.sessions.accounts().len(), 1);
    }
    #[tokio::test]
    async fn account_settings_refresh_runtime_and_reject_unknown_accounts() {
        let f = fixture().await;
        let outbox = Arc::new(crate::hub::Outbox::default());
        f.hub.connect(&outbox, herder_protocol::Role::Owner);
        let path = f.home.path().join("daemon.toml");
        std::fs::write(
            &path,
            "[[accounts]]\nid = 'codex'\nprovider = 'codex'\nlabel = 'Codex'\n",
        )
        .unwrap();
        f.logins
            .set_settings(
                &AccountId::new("codex"),
                "Personal",
                Some("/tmp/herder-personal"),
            )
            .await
            .unwrap();
        let account = f.sessions.accounts().remove(0);
        assert_eq!(account.label, "Personal");
        let Some(herder_protocol::ServerMessage::Accounts { accounts, .. }) = outbox.pop() else {
            panic!("account metadata was not sent to the connected client");
        };
        assert_eq!(accounts, f.sessions.accounts());
        assert_eq!(account.config_dir.as_deref(), Some("/tmp/herder-personal"));
        assert_eq!(
            f.logins
                .set_settings(&AccountId::new("missing"), "Missing", None)
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        assert_eq!(f.sessions.accounts().len(), 1);
    }
    #[tokio::test]
    async fn live_sessions_block_directory_changes_but_not_labels() {
        use herder_protocol::{EventBody, PermissionMode, SessionId, SessionStatus, Timestamp};
        use herder_store::NewEvent;
        let f = fixture().await;
        let path = f.home.path().join("daemon.toml");
        std::fs::write(
            &path,
            "[[accounts]]\nid = 'codex'\nprovider = 'codex'\nlabel = 'Codex'\n",
        )
        .unwrap();
        let mut store = Store::open(f.home.path().join("herder.db")).unwrap();
        let id = SessionId::new("live");
        store
            .append(NewEvent {
                session_id: id.clone(),
                at: Timestamp::now(),
                by: None,
                body: EventBody::SessionCreated {
                    repo: "/test".into(),
                    worktree: "/test-wt".into(),
                    branch: Some("test".into()),
                    provider: Provider::Codex,
                    account_id: AccountId::new("codex"),
                    model: "fake".into(),
                    permission_mode: PermissionMode::Ask,
                    parent: None,
                    parent_host: None,
                    task: None,
                    max_children: None,
                    failover_pin: None,
                },
            })
            .unwrap();
        let account = AccountId::new("codex");
        f.logins
            .set_settings(&account, "Renamed", None)
            .await
            .unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(
            f.logins
                .set_settings(&account, "Renamed", Some("/tmp/new-login"))
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
        assert_eq!(f.sessions.accounts()[0].config_dir, None);
        store
            .append(NewEvent {
                session_id: id,
                at: Timestamp::now(),
                by: None,
                body: EventBody::SessionStatusChanged {
                    status: SessionStatus::Archived,
                    retry_at: None,
                },
            })
            .unwrap();
        f.logins
            .set_settings(&account, "Renamed", Some("/tmp/new-login"))
            .await
            .unwrap();
        assert_eq!(
            f.sessions.accounts()[0].config_dir.as_deref(),
            Some("/tmp/new-login")
        );
    }
}
