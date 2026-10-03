//! Session titles a small model generates from the conversation.
//!
//! Once [`super::SessionManager::generate_titles`] runs, the daemon titles each session by
//! running its provider's unmodified CLI once, the way [`TitleCli`] says (`claude -p --model
//! haiku`, `codex exec`), with [`INSTRUCTION`] and the conversation so far on stdin and the
//! title on stdout. The run uses the titling account's config dir like a session's CLI,
//! never its credentials, and runs in a scope of the session's with the session's limits
//! ([`crate::resources::Scopes::launch_aside`]), in the session's worktree; the account's usage
//! is refreshed after it.
//!
//! - Automatically, as `auto`: once the session's first prompt is journaled, and again when
//!   its [`REFRESH_AFTER`]th turn ends, unless a user chose the title (`user` or
//!   `ai_requested`); a title they choose while a run is under way wins over its result.
//! - On `retitle_session`, as `ai_requested` `by` the user who asked: from the whole
//!   conversation so far, replacing any title, unless a user renamed the session meanwhile.
//!
//! The conversation is the prompts and the agent's replies, each cut to [`MESSAGE_CHARS`],
//! condensed to [`BUDGET`] tokens the way a handoff is ([`crate::handoff::transcript`]). A run
//! that fails, times out or answers no usable title is logged and changes nothing.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use herder_protocol::{
    AccountId, EventBody, Item, ItemBody, MAX_TITLE_CHARS, Provider, SessionId, SessionStatus,
    TitleSource, UserId, clean_title,
};
use herder_store::Session;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, warn};

use super::{AccountConfig, Inner};
use crate::handoff;
use crate::resources::processes;

/// The turn whose end refreshes an automatic title.
pub const REFRESH_AFTER: u64 = 3;

/// Longest a title run may take before it is killed.
pub const TIMEOUT: Duration = Duration::from_secs(60);

/// Characters of each message the model sees, from its start.
pub const MESSAGE_CHARS: usize = 1_000;

/// Tokens the conversation is condensed to.
pub const BUDGET: usize = 4_000;

/// What the model is asked, followed by the conversation.
pub const INSTRUCTION: &str = "Write a title for the coding session below: 3 to 6 words that \
    name its task, in the language the user writes in. Answer with the title alone on one \
    line, with no quotes, no trailing period and no explanation.";

/// How sessions are titled: the `[titles]` table, resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TitlesConfig {
    /// Whether sessions get generated titles at all.
    pub enabled: bool,
    /// Provider whose CLI titles; the account's when absent.
    pub provider: Option<Provider>,
    /// Model, in that provider's naming; its [`TitleCli::model`] when absent.
    pub model: Option<String>,
    /// Account the CLI runs on; the session's own when absent, else, when `provider` is not
    /// the session's, that provider's available account with the most room left.
    pub account: Option<AccountId>,
}

impl Default for TitlesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: None,
            model: None,
            account: None,
        }
    }
}

/// How a provider's CLI answers one prompt: `program` with `args`, then `--model <model>`; the
/// prompt on stdin, the answer on stdout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TitleCli {
    /// The executable, looked up on `PATH` when it is a bare name.
    pub program: PathBuf,
    /// Arguments before `--model`.
    pub args: Vec<String>,
    /// The variable the account's config dir is handed in, as for its sessions.
    pub config_dir_env: String,
    /// The provider's small model, used unless [`TitlesConfig::model`] names one.
    pub model: String,
}

impl TitleCli {
    /// `claude -p` on haiku, with no tools, nothing the user customized, and no session kept.
    pub fn claude(program: PathBuf) -> Self {
        Self {
            program,
            args: [
                "-p",
                "--output-format",
                "text",
                "--tools",
                "",
                "--safe-mode",
                "--no-session-persistence",
            ]
            .map(str::to_owned)
            .to_vec(),
            config_dir_env: "CLAUDE_CONFIG_DIR".to_owned(),
            model: "haiku".to_owned(),
        }
    }

    /// `codex exec` on its small model, read-only, with no session kept.
    pub fn codex(program: PathBuf) -> Self {
        Self {
            program,
            args: [
                "exec",
                "--sandbox",
                "read-only",
                "--skip-git-repo-check",
                "--ephemeral",
                "--color",
                "never",
            ]
            .map(str::to_owned)
            .to_vec(),
            config_dir_env: "CODEX_HOME".to_owned(),
            model: "gpt-6-luna".to_owned(),
        }
    }
}

/// The CLI each provider titles with.
pub type TitleClis = HashMap<Provider, TitleCli>;

/// What titles sessions, once set.
pub(super) struct Titler {
    pub(super) config: TitlesConfig,
    pub(super) clis: TitleClis,
}

/// Why a title is generated.
#[derive(Clone, Debug)]
enum Ask {
    /// Unasked; it never replaces a title a user chose.
    Auto,
    /// At `by`'s `retitle_session`, when the session's title was `seen`.
    Requested {
        by: UserId,
        seen: (Option<String>, Option<TitleSource>),
    },
}

/// One title run, resolved.
struct Run {
    cli: TitleCli,
    model: String,
    account_id: AccountId,
    account: AccountConfig,
}

/// Whether sessions get automatic titles.
pub(super) fn enabled(inner: &Inner) -> bool {
    inner
        .titler
        .get()
        .is_some_and(|titler| titler.config.enabled)
}

/// Titles `session_id` automatically in the background, unless a user chose its title.
pub(super) fn auto(inner: &Arc<Inner>, session_id: SessionId) {
    if enabled(inner) {
        tokio::spawn(generate(Arc::clone(inner), session_id, Ask::Auto));
    }
}

/// Starts titling `session` from its conversation at `by`'s request; refused as unsupported
/// when titles are off or no CLI can title it.
pub(super) fn request(inner: &Arc<Inner>, by: UserId, session: &Session) -> Result<(), String> {
    if !enabled(inner) {
        return Err("title generation is off on this daemon".to_owned());
    }
    resolve(inner, session)?;
    let ask = Ask::Requested {
        by,
        seen: (session.title.clone(), session.title_source),
    };
    tokio::spawn(generate(Arc::clone(inner), session.session_id.clone(), ask));
    Ok(())
}

/// The CLI, model and account that title `session`.
fn resolve(inner: &Inner, session: &Session) -> Result<Run, String> {
    let titler = inner
        .titler
        .get()
        .ok_or("title generation is off on this daemon")?;
    let config = &titler.config;
    let account_id = match (&config.account, &config.provider) {
        (Some(account_id), _) => account_id.clone(),
        (None, Some(provider)) if *provider != session.provider => inner
            .available_account(provider, None)
            .ok_or_else(|| format!("no {} account can title sessions", provider.as_str()))?,
        _ => session.account_id.clone(),
    };
    let account = inner
        .account(&account_id)
        .ok_or_else(|| format!("account {account_id} is not configured"))?;
    let cli = titler.clis.get(&account.provider).ok_or_else(|| {
        format!(
            "herder cannot title sessions with {}",
            account.provider.as_str()
        )
    })?;
    Ok(Run {
        model: config.model.clone().unwrap_or_else(|| cli.model.clone()),
        cli: cli.clone(),
        account_id,
        account,
    })
}

/// Generates a title for `session_id` as `ask` says and journals it; logs why when it cannot.
async fn generate(inner: Arc<Inner>, session_id: SessionId, ask: Ask) {
    let Some(session) = live(&inner, &session_id).await else {
        return;
    };
    if matches!(ask, Ask::Auto) && chosen(&session) {
        return;
    }
    let run = match resolve(&inner, &session) {
        Ok(run) => run,
        Err(why) => return debug!(%session_id, "not titling the session: {why}"),
    };
    let events = match inner.journal.all(session_id.clone()).await {
        Ok(events) => events,
        Err(err) => return warn!(%session_id, "cannot read the conversation to title: {err:#}"),
    };
    let items = events.into_iter().filter_map(|event| match event.body {
        EventBody::ItemAdded { item } => Some(item),
        _ => None,
    });
    let prompt = format!(
        "{INSTRUCTION}\n\n<conversation>\n{}\n</conversation>\n",
        conversation(items)
    );
    let launcher = match inner.scopes.get() {
        Some(scopes) => {
            let limits = scopes.limits(session.parent.is_some());
            scopes.launch_aside(&session_id, &limits)
        }
        None => Vec::new(),
    };
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    // Marks the run as the session's, for archive to find.
    env.insert(processes::SESSION_ENV.to_owned(), session_id.to_string());
    let cwd = PathBuf::from(&session.worktree);
    let answer = tokio::select! {
        answer = answer(&run, &launcher, env, &cwd, &prompt) => answer,
        () = inner.shutdown.cancelled() => return,
    };
    inner.refresh_usage.notify_one();
    let title = match answer {
        Ok(output) => match title(&output) {
            Some(title) => title,
            None => return warn!(%session_id, "the title model answered no title: {output:?}"),
        },
        Err(why) => {
            return warn!(%session_id, account_id = %run.account_id, "cannot title the session: {why}");
        }
    };
    let (by, source) = match &ask {
        Ask::Auto => (None, TitleSource::Auto),
        Ask::Requested { by, .. } => (Some(by.clone()), TitleSource::AiRequested),
    };
    let _titling = inner.titling.lock().await;
    let Some(session) = live(&inner, &session_id).await else {
        return;
    };
    let current = (session.title.clone(), session.title_source);
    let superseded = match &ask {
        Ask::Auto => chosen(&session),
        Ask::Requested { seen, .. } => {
            session.title_source == Some(TitleSource::User) && current != *seen
        }
    };
    if superseded || current == (Some(title.clone()), Some(source)) {
        return;
    }
    let body = EventBody::TitleChanged { title, source };
    if let Err(err) = inner.journal.record(session_id.clone(), by, body).await {
        warn!(%session_id, "cannot journal a title: {err:#}");
    }
}

/// The session, while its title may still change.
async fn live(inner: &Inner, session_id: &SessionId) -> Option<Session> {
    match inner.journal.session(session_id.clone()).await {
        Ok(session) => session.filter(|session| {
            !matches!(
                session.status,
                SessionStatus::Archived | SessionStatus::Moved
            )
        }),
        Err(err) => {
            warn!(%session_id, "cannot read the session to title: {err:#}");
            None
        }
    }
}

/// Whether a user chose the session's title.
fn chosen(session: &Session) -> bool {
    matches!(
        session.title_source,
        Some(TitleSource::User | TitleSource::AiRequested)
    )
}

/// Runs `run`'s CLI behind `launcher` in `cwd` with `env`, `prompt` on stdin, until it exits
/// or [`TIMEOUT`] passes; its stdout once it exited 0.
async fn answer(
    run: &Run,
    launcher: &[std::ffi::OsString],
    env: BTreeMap<String, String>,
    cwd: &Path,
    prompt: &str,
) -> Result<String, String> {
    let cli = &run.cli;
    let mut command = match launcher.split_first() {
        Some((program, args)) => {
            let mut command = Command::new(program);
            command.args(args).arg(&cli.program);
            command
        }
        None => Command::new(&cli.program),
    };
    command
        .args(&cli.args)
        .args(["--model", &run.model])
        .env_clear()
        .envs(env)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Its own group, so a timeout kills whatever it started too.
        .process_group(0)
        .kill_on_drop(true);
    if let Some(dir) = &run.account.config_dir {
        command.env(&cli.config_dir_env, dir);
    }
    let mut child = command
        .spawn()
        .map_err(|err| format!("starting {}: {err}", cli.program.display()))?;
    let group = child
        .id()
        .and_then(|pid| i32::try_from(pid).ok())
        .map(Pid::from_raw);
    if let Some(mut stdin) = child.stdin.take() {
        // A CLI that exits without reading it all fails below, by its status.
        let _ = stdin.write_all(prompt.as_bytes()).await;
    }
    match tokio::time::timeout(TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) if output.status.success() => {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        Ok(Ok(output)) => Err(format!("it failed ({})", output.status)),
        Ok(Err(err)) => Err(format!("waiting for it: {err}")),
        Err(_) => {
            if let Some(group) = group {
                // The group is gone already when its last process exited meanwhile.
                let _ = killpg(group, Signal::SIGKILL);
            }
            Err(format!("it timed out after {} s", TIMEOUT.as_secs()))
        }
    }
}

/// The prompts and replies among `items` as the model reads them: each cut to
/// [`MESSAGE_CHARS`], condensed to [`BUDGET`].
fn conversation(items: impl IntoIterator<Item = Item>) -> String {
    let cut = |text: &mut String| {
        if let Some((end, _)) = text.char_indices().nth(MESSAGE_CHARS) {
            text.truncate(end);
            text.push_str(" […]");
        }
    };
    let messages = items
        .into_iter()
        .filter_map(|mut item| {
            match &mut item.body {
                ItemBody::UserMessage { text, attachments } => {
                    attachments.clear();
                    cut(text);
                }
                ItemBody::AssistantMessage { text } => cut(text),
                _ => return None,
            }
            Some(item)
        })
        .collect();
    handoff::transcript(messages, BUDGET)
        .into_iter()
        .filter_map(|item| match item.body {
            ItemBody::UserMessage { text, .. } => Some(format!("User: {text}")),
            ItemBody::AssistantMessage { text } => Some(format!("Agent: {text}")),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The title in a model's `output`: its first non-empty line, without the quotes, emphasis or
/// trailing period models add, cut to [`MAX_TITLE_CHARS`].
fn title(output: &str) -> Option<String> {
    let line = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let undecorate = |line: &str| -> String {
        line.trim_matches(|c: char| c.is_whitespace() || "\"'`*#“”‘’".contains(c))
            .trim_end_matches('.')
            .to_owned()
    };
    let line = undecorate(line);
    let line = undecorate(line.strip_prefix("Title:").unwrap_or(&line));
    let line: String = line.chars().take(MAX_TITLE_CHARS).collect();
    clean_title(&line).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use herder_protocol::{ItemId, TurnId};

    use super::*;

    fn item(turn: &str, body: ItemBody) -> Item {
        Item {
            agent_message: None,
            parent_call_id: None,
            id: ItemId::new(ulid::Ulid::new().to_string()),
            turn_id: TurnId::new(turn),
            body,
        }
    }

    fn user(turn: &str, text: &str) -> Item {
        item(
            turn,
            ItemBody::UserMessage {
                text: text.into(),
                attachments: Vec::new(),
            },
        )
    }

    fn agent(turn: &str, text: &str) -> Item {
        item(turn, ItemBody::AssistantMessage { text: text.into() })
    }

    #[test]
    fn a_title_is_the_first_line_without_decoration() {
        assert_eq!(
            title("Fix flaky login test\n").as_deref(),
            Some("Fix flaky login test")
        );
        assert_eq!(
            title("\n  \"Fix flaky login test.\"  \nBecause...").as_deref(),
            Some("Fix flaky login test")
        );
        assert_eq!(
            title("**Title: Add dark mode**").as_deref(),
            Some("Add dark mode")
        );
        assert_eq!(title("# “Speed up CI”").as_deref(), Some("Speed up CI"));
        let long = "word ".repeat(40);
        let long = title(&long).unwrap();
        assert!(long.chars().count() <= MAX_TITLE_CHARS && long.starts_with("word word"));
        for none in ["", " \n \n", "\"\"", "..."] {
            assert_eq!(title(none), None, "{none:?}");
        }
    }

    #[test]
    fn the_conversation_is_the_prompts_and_replies_cut_to_size() {
        let long = "y".repeat(MESSAGE_CHARS + 50);
        let items = vec![
            user("t1", "Fix the flaky login test."),
            item(
                "t1",
                ItemBody::ToolCall {
                    name: "Bash".into(),
                    input: serde_json::json!({"command": "cargo test"}),
                },
            ),
            item(
                "t1",
                ItemBody::Reasoning {
                    text: "thinking".into(),
                },
            ),
            agent("t1", &long),
        ];
        assert_eq!(
            conversation(items),
            format!(
                "User: Fix the flaky login test.\n\nAgent: {} […]",
                "y".repeat(MESSAGE_CHARS)
            )
        );
    }

    #[test]
    fn a_long_conversation_is_condensed_keeping_the_opening_request() {
        let mut items = vec![user("t0", "The opening request.")];
        for turn in 1..100 {
            let turn = format!("t{turn}");
            items.push(user(&turn, &"p".repeat(MESSAGE_CHARS)));
            items.push(agent(&turn, &"r".repeat(MESSAGE_CHARS)));
        }
        let text = conversation(items);
        assert!(text.starts_with("User: The opening request.\n\nUser: [herder:"));
        assert!(text.len() / 4 <= BUDGET);
    }
}
