//! The skill library ([`herder_protocol::SkillsStatus`]): one checkout of the owner's skills
//! repository at `<data_dir>/skills`, and the links that hand its enabled skills to each CLI.
//!
//! # The checkout
//!
//! `set_skills_repo` clones the repository in place of the checkout. The daemon pulls when it
//! starts, when a client connects and on `pull_skills`: it fetches and resets the checkout to
//! its upstream, so the checkout is always what was last pushed. `put_skill`, `delete_skill`
//! and `import_skill` pull, change the skill's folder, commit as herder and push, with the git
//! access the daemon's user has, as checkpoints push; a commit that cannot be pushed is reset
//! away and fails the command. Project repositories and worktrees are never touched.
//!
//! Which skills are disabled is this machine's own, kept in `<data_dir>/skills.json`.
//!
//! # Delivery
//!
//! `<data_dir>/skill-links` is herder's own layout of the enabled skills: `enabled`,
//! `claude/.claude/skills` and `cursor/skills` each hold a link per enabled skill to its
//! folder in the checkout, and nothing for a disabled one. Each CLI is pointed at its layout
//! when it starts:
//!
//! | CLI      | How                                                  | A running session sees a change |
//! | -------- | ---------------------------------------------------- | ------------------------------- |
//! | Claude   | `--add-dir <links>/claude`                           | at once: it watches the dir     |
//! | Codex    | `$CODEX_HOME/skills/herder` → `<links>/enabled`      | at its next turn                |
//! | Cursor   | `--plugin-dir <links>/cursor`, a plugin named herder | in sessions started after it    |
//! | OpenCode | `skills.paths` in `OPENCODE_CONFIG_CONTENT`          | in sessions started after it    |
//!
//! Claude 2.1.288 loads `.claude/skills` of every added dir, as plain `/name`, and follows a
//! link per skill but not a link in place of `.claude/skills` itself, hence the link per skill
//! in a real dir. The Codex link is the one thing herder adds to Codex's config dir; Codex's
//! app-server watches its skills. Cursor reads plugins and OpenCode its config only as they
//! start. Grok and Gemini take no extra skills.
//!
//! # A session's skills
//!
//! A session's skills (`session_skills`) are the enabled library skills its CLI loads and the
//! project skills checked in to its worktree: every `<name>/SKILL.md` in a `.claude/skills` or
//! `.agents/skills` dir, at the top of the worktree or in a nested dir such as
//! `web/.claude/skills`, where the session's CLI looks: Claude in `.claude`, Codex in
//! `.agents`, Cursor and OpenCode in both. They are sent when the session's CLI starts, and
//! again whenever the library changes.

#[cfg(test)]
mod tests;

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use herder_protocol::{
    CommandBody, CommandResult, ErrorCode, ErrorInfo, LibrarySkill, MAX_SKILL_BYTES, Provider,
    ProviderReload, SessionId, SessionSkill, SkillFile, SkillReload, SkillSource, SkillsStatus,
    Timestamp, is_valid_skill_name,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::warn;

use crate::hub::Hub;
use crate::worktree::git;

/// How long a clone, fetch or push may take before it is given up.
pub const NETWORK_TIMEOUT: Duration = Duration::from_secs(120);

/// The CLIs herder hands the library to, and when their running sessions see a change.
const RELOAD: [(Provider, SkillReload); 4] = [
    (Provider::Claude, SkillReload::Live),
    (Provider::Codex, SkillReload::NextTurn),
    (Provider::Cursor, SkillReload::NextSession),
    (Provider::Opencode, SkillReload::NextSession),
];

/// Where a skill library publishes what clients see.
pub trait SkillsSink: Send + Sync {
    /// The library changed.
    fn skills_status(&self, status: SkillsStatus);
    /// A session's skills changed.
    fn session_skills(&self, session_id: &SessionId, skills: Vec<SessionSkill>);
}

impl SkillsSink for Hub {
    fn skills_status(&self, status: SkillsStatus) {
        Hub::skills_status(self, status);
    }

    fn session_skills(&self, session_id: &SessionId, skills: Vec<SessionSkill>) {
        Hub::session_skills(self, session_id, skills);
    }
}

/// The skill library of one daemon.
pub struct Skills {
    data_dir: PathBuf,
    /// `<data_dir>/skills`.
    checkout: PathBuf,
    /// `<data_dir>/skill-links`.
    links: PathBuf,
    /// The CLIs on this machine the library reaches.
    providers: Vec<Provider>,
    sink: Arc<dyn SkillsSink>,
    /// Held across every change to the checkout, the links or what is disabled.
    state: Mutex<State>,
    /// The enabled library skills as names and descriptions, as last published; read at every
    /// CLI start, so it never waits for a pull or a push.
    enabled: std::sync::Mutex<Vec<(String, String)>>,
    /// Sessions whose skills were sent: their CLI's provider and project skills.
    sessions: std::sync::Mutex<HashMap<SessionId, (Provider, Vec<SessionSkill>)>>,
}

#[derive(Debug, Default)]
struct State {
    /// What `<data_dir>/skills.json` holds.
    saved: Saved,
    last_pull: Option<Timestamp>,
    pull_error: Option<String>,
}

/// This machine's own settings for the library, in `<data_dir>/skills.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    /// Skills not handed to any CLI here.
    #[serde(default)]
    disabled: BTreeSet<String>,
}

impl Skills {
    /// The library kept in `data_dir`, reaching the CLIs of `providers` and published to
    /// `sink`. Creates the link layout; changes no checkout.
    pub fn open(
        data_dir: &Path,
        providers: &[Provider],
        sink: Arc<dyn SkillsSink>,
    ) -> anyhow::Result<Self> {
        let saved = match fs::read(data_dir.join("skills.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Saved::default(),
            Err(err) => return Err(err.into()),
        };
        let links = data_dir.join("skill-links");
        let skills = Self {
            data_dir: data_dir.to_owned(),
            checkout: data_dir.join("skills"),
            providers: RELOAD
                .iter()
                .map(|(provider, _)| provider.clone())
                .filter(|provider| providers.contains(provider))
                .collect(),
            sink,
            state: Mutex::new(State {
                saved,
                ..State::default()
            }),
            enabled: std::sync::Mutex::new(Vec::new()),
            sessions: std::sync::Mutex::new(HashMap::new()),
            links,
        };
        for dir in skills.link_dirs() {
            fs::create_dir_all(dir)?;
        }
        let plugin = skills.links.join("cursor/.cursor-plugin");
        fs::create_dir_all(&plugin)?;
        fs::write(
            plugin.join("plugin.json"),
            serde_json::json!({
                "name": "herder",
                "description": "The skill library herder keeps on this machine",
            })
            .to_string(),
        )?;
        Ok(skills)
    }

    /// Pulls the library, if one is set, and publishes it; for the daemon's start and each
    /// client that connects. Skipped while another change runs, which publishes when done.
    pub async fn pull(&self) {
        let Ok(mut state) = self.state.try_lock() else {
            return;
        };
        if self.has_checkout() {
            self.pull_locked(&mut state).await;
        }
        self.publish(&state).await;
    }

    /// Answers the six skill library commands.
    pub async fn command(&self, command: CommandBody) -> Result<CommandResult, ErrorInfo> {
        let mut state = self.state.lock().await;
        match command {
            CommandBody::SetSkillsRepo { url } => self.set_repo(&mut state, &url).await?,
            CommandBody::PutSkill { name, files } => {
                check_put(&name, &files)?;
                self.change(&mut state, &name, &format!("Put skill {name}"), |dir| {
                    put(dir, &files)
                })
                .await?;
            }
            CommandBody::DeleteSkill { name } => {
                if !is_valid_skill_name(&name) || !self.checkout.join(&name).is_dir() {
                    return Err(error(
                        ErrorCode::NotFound,
                        format!("the library has no skill {name}"),
                    ));
                }
                self.change(&mut state, &name, &format!("Delete skill {name}"), |dir| {
                    fs::remove_dir_all(dir).map_err(internal)
                })
                .await?;
            }
            CommandBody::ImportSkill { git_url, path } => {
                self.import(&mut state, &git_url, path.as_deref()).await?;
            }
            CommandBody::PullSkills => {
                self.require_checkout()?;
                self.pull_locked(&mut state).await;
            }
            CommandBody::SetSkillEnabled { name, enabled } => {
                if !library(&self.checkout)
                    .iter()
                    .any(|(skill, _)| *skill == name)
                {
                    return Err(error(
                        ErrorCode::NotFound,
                        format!("the library has no skill {name}"),
                    ));
                }
                if enabled {
                    state.saved.disabled.remove(&name);
                } else {
                    state.saved.disabled.insert(name);
                }
                let saved = serde_json::to_vec(&state.saved).map_err(internal)?;
                fs::write(self.data_dir.join("skills.json"), saved).map_err(internal)?;
            }
            _ => {
                return Err(error(ErrorCode::Unsupported, "not a skill library command"));
            }
        }
        self.publish(&state).await;
        Ok(CommandResult::Applied)
    }

    /// Readies the library for a start of `provider`'s CLI, whose config dir is `config_dir`:
    /// the dir to hand it as [`herder_adapters::StartRequest::skills`], if any. For Codex, links
    /// `skills/herder` in its config dir to the enabled skills instead.
    pub fn launch(&self, provider: &Provider, config_dir: Option<&Path>) -> Option<PathBuf> {
        if !self.providers.contains(provider) {
            return None;
        }
        match provider {
            Provider::Claude => Some(self.links.join("claude")),
            Provider::Cursor => Some(self.links.join("cursor")),
            Provider::Opencode => Some(self.links.join("enabled")),
            Provider::Codex => {
                match config_dir {
                    Some(dir) => {
                        if let Err(err) = link_codex(dir, &self.links.join("enabled")) {
                            warn!(
                                "cannot link the skill library into {}: {err:#}",
                                dir.display()
                            );
                        }
                    }
                    None => warn!("Codex has no config dir to link the skill library into"),
                }
                None
            }
            _ => None,
        }
    }

    /// A session's CLI started, as `provider`'s in `worktree`: finds its project skills and
    /// sends its skills, and sends them again whenever the library changes.
    pub async fn session_started(
        &self,
        session_id: &SessionId,
        provider: &Provider,
        worktree: &Path,
    ) {
        let worktree = worktree.to_owned();
        let found = {
            let provider = provider.clone();
            tokio::task::spawn_blocking(move || project_skills(&worktree, &provider)).await
        };
        let project = found.unwrap_or_else(|err| {
            warn!(%session_id, "finding the project's skills panicked: {err}");
            Vec::new()
        });
        let skills = self.session_list(provider, &project);
        self.sessions_lock()
            .insert(session_id.clone(), (provider.clone(), project));
        self.sink.session_skills(session_id, skills);
    }

    /// A session was archived: it has no skills any more.
    pub fn session_archived(&self, session_id: &SessionId) {
        if self.sessions_lock().remove(session_id).is_some() {
            self.sink.session_skills(session_id, Vec::new());
        }
    }

    fn sessions_lock(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<SessionId, (Provider, Vec<SessionSkill>)>> {
        // Every update is one insert or remove, so a poisoned map is consistent.
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The skills of a session of `provider` whose project skills are `project`.
    fn session_list(&self, provider: &Provider, project: &[SessionSkill]) -> Vec<SessionSkill> {
        let mut skills = Vec::new();
        if self.providers.contains(provider) {
            let enabled = self.enabled.lock().unwrap_or_else(PoisonError::into_inner);
            skills.extend(enabled.iter().map(|(name, description)| SessionSkill {
                name: name.clone(),
                description: description.clone(),
                source: SkillSource::Library,
                path: None,
            }));
        }
        skills.extend(project.iter().cloned());
        skills
    }

    fn link_dirs(&self) -> [PathBuf; 3] {
        [
            self.links.join("enabled"),
            self.links.join("claude/.claude/skills"),
            self.links.join("cursor/skills"),
        ]
    }

    fn has_checkout(&self) -> bool {
        self.checkout.join(".git").exists()
    }

    fn require_checkout(&self) -> Result<(), ErrorInfo> {
        if self.has_checkout() {
            Ok(())
        } else {
            Err(error(ErrorCode::NotFound, "no skill library is set"))
        }
    }

    /// Clones `url` in place of the checkout.
    async fn set_repo(&self, state: &mut State, url: &str) -> Result<(), ErrorInfo> {
        if url.trim().is_empty() {
            return Err(error(ErrorCode::BadRequest, "the library needs a git URL"));
        }
        let clone = self.data_dir.join("skills.clone");
        remove_dir(&clone).map_err(internal)?;
        let cloned = network(git(
            &self.data_dir,
            [
                OsStr::new("clone"),
                OsStr::new("--quiet"),
                OsStr::new("--"),
                OsStr::new(url),
                clone.as_os_str(),
            ],
        ))
        .await;
        if let Err(err) = cloned {
            let _ = remove_dir(&clone);
            return Err(error(
                ErrorCode::BadRequest,
                format!("cannot clone {}: {}", redact(url), redact_in(&err, url)),
            ));
        }
        remove_dir(&self.checkout).map_err(internal)?;
        fs::rename(&clone, &self.checkout).map_err(internal)?;
        state.last_pull = Some(Timestamp::now());
        state.pull_error = None;
        Ok(())
    }

    /// Fetches and resets the checkout to its upstream, noting how that went.
    async fn pull_locked(&self, state: &mut State) {
        let pulled = async {
            network(git(
                &self.checkout,
                ["fetch", "--quiet", "--prune", "origin"],
            ))
            .await?;
            self.reset().await
        }
        .await;
        state.last_pull = Some(Timestamp::now());
        state.pull_error = pulled.err();
    }

    /// Resets the checkout to its upstream, tracking the remote branch of the same name once
    /// it has one; to no commit while the remote has none.
    async fn reset(&self) -> Result<(), String> {
        let dir = &self.checkout;
        if !self.has_upstream().await {
            let branch = git(dir, ["symbolic-ref", "--short", "HEAD"])
                .await
                .map_err(|err| err.to_string())?;
            let remote = format!("refs/remotes/origin/{branch}");
            if git(dir, ["rev-parse", "--verify", "--quiet", &remote])
                .await
                .is_ok()
            {
                git(dir, ["branch", "--quiet", "--set-upstream-to", &remote])
                    .await
                    .map_err(|err| err.to_string())?;
            }
        }
        if self.has_upstream().await {
            git(dir, ["reset", "--quiet", "--hard", "@{upstream}"])
                .await
                .map_err(|err| err.to_string())?;
        } else {
            // Best effort: a branch with no commit yet has no ref to delete.
            let _ = git(dir, ["update-ref", "-d", "HEAD"]).await;
            git(dir, ["read-tree", "--empty"])
                .await
                .map_err(|err| err.to_string())?;
        }
        git(dir, ["clean", "--quiet", "-fd"])
            .await
            .map_err(|err| err.to_string())?;
        Ok(())
    }

    async fn has_upstream(&self) -> bool {
        git(
            &self.checkout,
            ["rev-parse", "--verify", "--quiet", "@{upstream}"],
        )
        .await
        .is_ok()
    }

    /// Pulls, applies `apply` to skill `name`'s folder, then commits and pushes what changed
    /// as `message`; resets the change away when it cannot be pushed.
    async fn change(
        &self,
        state: &mut State,
        name: &str,
        message: &str,
        apply: impl FnOnce(&Path) -> Result<(), ErrorInfo>,
    ) -> Result<(), ErrorInfo> {
        self.require_checkout()?;
        self.pull_locked(state).await;
        if let Some(err) = &state.pull_error {
            return Err(error(
                ErrorCode::Internal,
                format!("cannot pull the skill library: {err}"),
            ));
        }
        let applied = async {
            apply(&self.checkout.join(name))?;
            self.commit_and_push(name, message).await
        }
        .await;
        if applied.is_err()
            && let Err(err) = self.reset().await
        {
            warn!("cannot reset the skill library after a failed change: {err}");
        }
        applied
    }

    async fn commit_and_push(&self, name: &str, message: &str) -> Result<(), ErrorInfo> {
        let dir = &self.checkout;
        let git_error = |err: crate::worktree::Error| error(ErrorCode::Internal, err.to_string());
        git(dir, ["add", "--all", "--", name])
            .await
            .map_err(git_error)?;
        if git(dir, ["diff", "--cached", "--quiet"]).await.is_ok() {
            return Ok(());
        }
        git(
            dir,
            [
                "-c",
                "user.name=herder",
                "-c",
                "user.email=herder@localhost",
                "commit",
                "--quiet",
                "--no-gpg-sign",
                "--no-verify",
                "-m",
                message,
            ],
        )
        .await
        .map_err(git_error)?;
        network(git(
            dir,
            ["push", "--quiet", "--set-upstream", "origin", "HEAD"],
        ))
        .await
        .map_err(|err| {
            error(
                ErrorCode::Internal,
                format!("cannot push the skill library: {err}"),
            )
        })
    }

    /// Copies the skill folder `path` of the repository `url` into the library.
    async fn import(
        &self,
        state: &mut State,
        url: &str,
        path: Option<&str>,
    ) -> Result<(), ErrorInfo> {
        self.require_checkout()?;
        let path = path
            .map(|path| path.trim_matches('/'))
            .filter(|path| !path.is_empty());
        if let Some(path) = path
            && !is_valid_path(path)
        {
            return Err(error(
                ErrorCode::BadRequest,
                format!("{path} is not a folder within the repository"),
            ));
        }
        let name = match path {
            Some(path) => path.rsplit('/').next().unwrap_or(path),
            None => repo_name(url),
        }
        .to_owned();
        if !is_valid_skill_name(&name) {
            return Err(error(
                ErrorCode::BadRequest,
                format!("{name} is not a valid skill name"),
            ));
        }
        let clone = self.data_dir.join("skills.import");
        remove_dir(&clone).map_err(internal)?;
        let cloned = network(git(
            &self.data_dir,
            [
                OsStr::new("clone"),
                OsStr::new("--quiet"),
                OsStr::new("--depth"),
                OsStr::new("1"),
                OsStr::new("--"),
                OsStr::new(url),
                clone.as_os_str(),
            ],
        ))
        .await;
        let imported = async {
            cloned.map_err(|err| {
                error(
                    ErrorCode::BadRequest,
                    format!("cannot clone {}: {}", redact(url), redact_in(&err, url)),
                )
            })?;
            let source = match path {
                Some(path) => clone.join(path),
                None => clone.clone(),
            };
            if !source.join("SKILL.md").is_file() {
                return Err(error(
                    ErrorCode::BadRequest,
                    format!("{} has no SKILL.md", path.unwrap_or("the repository's top")),
                ));
            }
            let message = format!("Import skill {name} from {}", redact(url));
            self.change(state, &name, &message, |dir| {
                remove_dir(dir).map_err(internal)?;
                copy_dir(&source, dir).map_err(internal)
            })
            .await
        }
        .await;
        if let Err(err) = remove_dir(&clone) {
            warn!("cannot remove {}: {err}", clone.display());
        }
        imported
    }

    /// Brings the links in line with the library, then publishes the library and every
    /// session's skills.
    async fn publish(&self, state: &State) {
        let library = library(&self.checkout);
        let enabled: Vec<(String, String)> = library
            .iter()
            .filter(|(name, _)| !state.saved.disabled.contains(name))
            .cloned()
            .collect();
        for dir in self.link_dirs() {
            if let Err(err) = sync_links(&dir, &self.checkout, &enabled) {
                warn!("cannot link the skill library in {}: {err}", dir.display());
            }
        }
        *self.enabled.lock().unwrap_or_else(PoisonError::into_inner) = enabled;
        let dir = &self.checkout;
        let (repo, head) = if self.has_checkout() {
            let repo = git(dir, ["remote", "get-url", "origin"]).await.ok();
            let head = git(dir, ["rev-parse", "--verify", "--quiet", "HEAD"])
                .await
                .ok();
            (repo.map(|repo| redact(&repo)), head)
        } else {
            (None, None)
        };
        let status = SkillsStatus {
            repo,
            head,
            last_pull: state.last_pull,
            pull_error: state.pull_error.clone(),
            skills: library
                .into_iter()
                .map(|(name, description)| {
                    let enabled = !state.saved.disabled.contains(&name);
                    LibrarySkill {
                        providers: if enabled {
                            self.providers.clone()
                        } else {
                            Vec::new()
                        },
                        name,
                        description,
                        enabled,
                    }
                })
                .collect(),
            reload: RELOAD
                .iter()
                .filter(|(provider, _)| self.providers.contains(provider))
                .map(|(provider, reload)| ProviderReload {
                    provider: provider.clone(),
                    reload: *reload,
                })
                .collect(),
        };
        self.sink.skills_status(status);
        let sessions: Vec<_> = self
            .sessions_lock()
            .iter()
            .map(|(session_id, (provider, project))| {
                (session_id.clone(), self.session_list(provider, project))
            })
            .collect();
        for (session_id, skills) in sessions {
            self.sink.session_skills(&session_id, skills);
        }
    }
}

/// Runs a git command that may reach a remote, giving up after [`NETWORK_TIMEOUT`].
async fn network(
    run: impl Future<Output = Result<String, crate::worktree::Error>>,
) -> Result<(), String> {
    match tokio::time::timeout(NETWORK_TIMEOUT, run).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) => Err(err.to_string()),
        Err(_) => Err(format!(
            "git did not finish within {} s",
            NETWORK_TIMEOUT.as_secs()
        )),
    }
}

/// Refuses a `put_skill` the protocol refuses, before anything is touched.
fn check_put(name: &str, files: &[SkillFile]) -> Result<(), ErrorInfo> {
    if !is_valid_skill_name(name) {
        return Err(error(
            ErrorCode::BadRequest,
            format!("{name} is not a valid skill name"),
        ));
    }
    if let Some(file) = files.iter().find(|file| !is_valid_path(&file.path)) {
        return Err(error(
            ErrorCode::BadRequest,
            format!("{} is not a valid path in a skill", file.path),
        ));
    }
    if !files.iter().any(|file| file.path == "SKILL.md") {
        return Err(error(ErrorCode::BadRequest, "a skill needs a SKILL.md"));
    }
    let bytes: usize = files.iter().map(|file| file.data.0.len()).sum();
    if bytes > MAX_SKILL_BYTES {
        return Err(error(
            ErrorCode::BadRequest,
            format!("a skill's files may have {MAX_SKILL_BYTES} bytes together"),
        ));
    }
    Ok(())
}

/// Whether `path` is `/`-separated with no empty, `.`, `..` or `.git` component.
fn is_valid_path(path: &str) -> bool {
    !path.contains(['\\', '\0'])
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".." && part != ".git")
}

/// Replaces the folder `dir` with exactly `files`.
fn put(dir: &Path, files: &[SkillFile]) -> Result<(), ErrorInfo> {
    remove_dir(dir).map_err(internal)?;
    for file in files {
        let path = dir.join(&file.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(internal)?;
        }
        fs::write(&path, &file.data.0).map_err(internal)?;
        let mode = if file.executable { 0o755 } else { 0o644 };
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).map_err(internal)?;
    }
    Ok(())
}

/// Copies the folder `from` to `to`, files with their permissions, leaving out `.git` and
/// links, which could point anywhere.
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if entry.file_name() == ".git" || kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Removes `dir` and everything in it, if it is there.
fn remove_dir(dir: &Path) -> std::io::Result<()> {
    match fs::remove_dir_all(dir) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// The skills of the library checked out at `checkout`, as names and descriptions, ordered by
/// name: every folder with a valid skill name and a `SKILL.md`.
fn library(checkout: &Path) -> Vec<(String, String)> {
    let Ok(entries) = fs::read_dir(checkout) else {
        return Vec::new();
    };
    let mut skills: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if !is_valid_skill_name(&name) || !entry.file_type().ok()?.is_dir() {
                return None;
            }
            let text = fs::read_to_string(entry.path().join("SKILL.md")).ok()?;
            Some((name, description(&text)))
        })
        .collect();
    skills.sort();
    skills
}

/// The `description` in the front matter of a `SKILL.md`; empty when it has none. Takes a
/// plain, quoted, folded (`>`) or literal (`|`) value, the last two joined into one line.
fn description(skill_md: &str) -> String {
    let mut lines = skill_md.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return String::new();
    }
    let front: Vec<&str> = lines.take_while(|line| line.trim_end() != "---").collect();
    let Some(at) = front
        .iter()
        .position(|line| line.starts_with("description:"))
    else {
        return String::new();
    };
    let value = front[at]["description:".len()..].trim();
    let block = value.is_empty() || value.starts_with(['>', '|']);
    if !block {
        return unquote(value).to_owned();
    }
    front[at + 1..]
        .iter()
        .take_while(|line| line.starts_with([' ', '\t']) || line.trim().is_empty())
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|value| value.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

/// Makes `dir` hold a link to `<checkout>/<name>` for each of `enabled`, and nothing else.
fn sync_links(dir: &Path, checkout: &Path, enabled: &[(String, String)]) -> std::io::Result<()> {
    let wanted: BTreeSet<&str> = enabled.iter().map(|(name, _)| name.as_str()).collect();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let keep = entry
            .file_name()
            .to_str()
            .is_some_and(|name| wanted.contains(name))
            && entry.file_type()?.is_symlink();
        if !keep {
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                fs::remove_dir_all(path)?;
            } else {
                fs::remove_file(path)?;
            }
        }
    }
    for name in wanted {
        let link = dir.join(name);
        if fs::symlink_metadata(&link).is_err() {
            symlink(checkout.join(name), link)?;
        }
    }
    Ok(())
}

/// Links `<config_dir>/skills/herder` to `enabled`, replacing a link that points elsewhere;
/// anything else in its place is the user's and is left alone.
fn link_codex(config_dir: &Path, enabled: &Path) -> std::io::Result<()> {
    let skills = config_dir.join("skills");
    fs::create_dir_all(&skills)?;
    let link = skills.join("herder");
    match fs::symlink_metadata(&link) {
        Ok(meta) if meta.file_type().is_symlink() => {
            if fs::read_link(&link)? == enabled {
                return Ok(());
            }
            fs::remove_file(&link)?;
        }
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("{} is not herder's", link.display()),
            ));
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    symlink(enabled, link)
}

/// Dirs never searched for project skills: they hold no checked-in project files.
const SKIPPED_DIRS: [&str; 3] = [".git", "node_modules", "target"];

/// How deep below the worktree project skills are looked for.
const MAX_DEPTH: usize = 8;

/// The project skills in `worktree` that `provider`'s CLI loads, ordered by path.
fn project_skills(worktree: &Path, provider: &Provider) -> Vec<SessionSkill> {
    let kinds: &[&str] = match provider {
        Provider::Claude => &[".claude"],
        Provider::Codex => &[".agents"],
        _ => &[".claude", ".agents"],
    };
    let mut skills = Vec::new();
    find_project_skills(worktree, Path::new(""), kinds, 0, &mut skills);
    skills.sort_by(|a, b| a.path.cmp(&b.path));
    skills
}

fn find_project_skills(
    worktree: &Path,
    relative: &Path,
    kinds: &[&str],
    depth: usize,
    skills: &mut Vec<SessionSkill>,
) {
    let dir = worktree.join(relative);
    for kind in kinds {
        let found = relative.join(kind).join("skills");
        let Ok(entries) = fs::read_dir(worktree.join(&found)) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let Ok(text) = fs::read_to_string(entry.path().join("SKILL.md")) else {
                continue;
            };
            skills.push(SessionSkill {
                description: description(&text),
                source: SkillSource::Project,
                path: Some(found.join(&name).to_string_lossy().into_owned()),
                name,
            });
        }
    }
    if depth >= MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
        if is_dir && !name.starts_with('.') && !SKIPPED_DIRS.contains(&name.as_str()) {
            find_project_skills(worktree, &relative.join(&name), kinds, depth + 1, skills);
        }
    }
}

/// The repository's name in `url`, without `.git`: a skill imported from its top is named so.
fn repo_name(url: &str) -> &str {
    let url = url.trim_end_matches('/');
    let last = url.rsplit(['/', ':']).next().unwrap_or(url);
    last.strip_suffix(".git").unwrap_or(last)
}

/// `url` without any credentials it holds, as `https://user:token@host/...` does.
fn redact(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let end = rest.find('/').unwrap_or(rest.len());
    match rest[..end].rsplit_once('@') {
        Some((_, host)) => format!("{scheme}://{host}{}", &rest[end..]),
        None => url.to_owned(),
    }
}

/// `message` with `url` in it redacted.
fn redact_in(message: &str, url: &str) -> String {
    message.replace(url, &redact(url))
}

fn error(code: ErrorCode, message: impl Into<String>) -> ErrorInfo {
    ErrorInfo {
        code,
        message: message.into(),
    }
}

fn internal(err: impl std::fmt::Display) -> ErrorInfo {
    error(ErrorCode::Internal, err.to_string())
}
