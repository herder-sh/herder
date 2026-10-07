//! What differs between ACP agents: how to launch one for an account.
//!
//! | Agent    | Command            | Account config dir                         | Images | Verified                                 |
//! | -------- | ------------------ | ------------------------------------------ | ------ | ---------------------------------------- |
//! | OpenCode | `opencode acp`     | `XDG_DATA_HOME` (`opencode/auth.json`)     | yes    | 1.18.21, credentials path moves          |
//! | Grok     | `grok agent stdio` | `GROK_HOME` (replaces `~/.grok`)           | no     | 1.0.46 logged out, config and sessions move |
//! | Cursor   | `cursor-agent acp` | `CURSOR_CONFIG_DIR` and `XDG_CONFIG_HOME`  | no     | no: from Cursor's docs, CLI not available |
//!
//! herder's skill library reaches Cursor as a plugin, `--plugin-dir <dir>`, and OpenCode as one
//! of its `skills.paths`, set through `OPENCODE_CONFIG_CONTENT`, which OpenCode merges over its
//! other config. Grok takes no extra skills.
//!
//! Images is what each agent advertised as `promptCapabilities.image` in the recordings, and
//! for Cursor, unverified, no.
//!
//! A `$name` skill mention is rewritten the way T3 Code does: `/name` for Cursor, plain text
//! naming the skill for OpenCode, which picks skills itself. Grok's is left as typed, as no
//! way to invoke a Grok skill from a prompt is known.
//!
//! Each agent must ask before every write or command, so the adapter can apply the session's
//! permission mode: OpenCode is launched with `OPENCODE_PERMISSION={"*":"ask"}`, as by default
//! it runs most tools unasked; Grok and Cursor ask by default over ACP.
//!
//! None of them reports limit windows over ACP, so no account of theirs shows usage. OpenCode
//! has none to report either: `opencode stats` totals tokens and cost from its own sessions,
//! not the model provider's quota, so its limits surface only as turn errors.
//!
//! An agent may also take a login from the environment, which the daemon passes on whole; when
//! the account has a config dir those variables are removed, so the account's own login is the
//! only one the agent can use.

use std::collections::HashSet;
use std::path::Path;
use std::process::Stdio;

use herder_protocol::Provider;
use tokio::process::Command;

use crate::StartRequest;

/// How to launch one ACP agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProfile {
    /// Provider sessions of this agent belong to.
    pub provider: Provider,
    /// Program to run, looked up on the `PATH` of the session's environment.
    pub program: String,
    /// Arguments before the model flag.
    pub args: Vec<String>,
    /// Flag that sets the starting model, followed by the model; when absent the model is set
    /// through the agent's `model` config option.
    pub model_flag: Option<String>,
    /// Arguments after the model flag.
    pub trailing_args: Vec<String>,
    /// Variables set to the account's config dir.
    pub config_dir_vars: Vec<String>,
    /// Variables every launch sets, to make the agent ask before every write or command.
    pub launch_env: Vec<(String, String)>,
    /// Variables that hold a login outside the config dir, removed when the account has one.
    pub login_env: Vec<String>,
    /// How the agent is given herder's skill library ([`StartRequest::skills`]).
    pub skills: SkillsLaunch,
    /// Whether the agent is known to take images with a prompt, which it advertises in
    /// `initialize`; what the adapter says until an agent started and told it.
    pub images: bool,
    /// How a `$name` mention of one of the agent's skills is written for it.
    pub skill_mention: SkillMention,
}

/// How an agent is told to use a skill a prompt mentions as `$name`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillMention {
    /// `/name`, which the agent expands like a typed command.
    Slash,
    /// Plain text naming the skill, for an agent that picks skills itself.
    Named,
    /// `$name`, as typed.
    AsTyped,
}

impl SkillMention {
    /// `text` with each `$name` mention of a skill in `skills` written this way.
    pub(super) fn rewrite(self, text: &str, skills: &HashSet<String>) -> String {
        match self {
            Self::Slash => crate::rewrite_skill_mentions(text, skills, |name| format!("/{name}")),
            Self::Named => {
                crate::rewrite_skill_mentions(text, skills, |name| format!("the {name} skill"))
            }
            Self::AsTyped => text.to_owned(),
        }
    }
}

/// How an ACP agent is given herder's skill library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkillsLaunch {
    /// It takes none.
    None,
    /// As the value of this flag, before the trailing arguments.
    Flag(String),
    /// As one of `skills.paths` in `OPENCODE_CONFIG_CONTENT`.
    OpencodeConfig,
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

impl AgentProfile {
    /// OpenCode, `opencode acp`.
    ///
    /// Credentials live in `$XDG_DATA_HOME/opencode/auth.json`, with OpenCode's sessions;
    /// `OPENCODE_CONFIG_DIR` only adds a config dir and does not move them. `opencode acp`
    /// has no model flag; the model is a config option.
    pub fn opencode() -> Self {
        Self {
            provider: Provider::Opencode,
            program: "opencode".into(),
            args: strings(&["acp"]),
            model_flag: None,
            trailing_args: Vec::new(),
            config_dir_vars: strings(&["XDG_DATA_HOME"]),
            launch_env: vec![("OPENCODE_PERMISSION".into(), r#"{"*":"ask"}"#.into())],
            login_env: Vec::new(),
            skills: SkillsLaunch::OpencodeConfig,
            images: true,
            skill_mention: SkillMention::Named,
        }
    }

    /// Grok, `grok agent [-m <model>] stdio`.
    ///
    /// `GROK_HOME` replaces `~/.grok`, where its config, sessions and login (`auth.json`) live.
    /// Grok falls back to `XAI_API_KEY` (or the older `GROK_CODE_XAI_API_KEY`) when signed out,
    /// which would run a signed-out account on someone else's key.
    pub fn grok() -> Self {
        Self {
            provider: Provider::Grok,
            program: "grok".into(),
            args: strings(&["agent"]),
            model_flag: Some("-m".into()),
            trailing_args: strings(&["stdio"]),
            config_dir_vars: strings(&["GROK_HOME"]),
            launch_env: Vec::new(),
            login_env: strings(&["XAI_API_KEY", "GROK_CODE_XAI_API_KEY"]),
            skills: SkillsLaunch::None,
            images: false,
            skill_mention: SkillMention::AsTyped,
        }
    }

    /// Cursor, `cursor-agent [--model <model>] acp`. Unverified: no Cursor CLI was available.
    ///
    /// Cursor installs its CLI as both `agent` and `cursor-agent`; Grok installs an `agent`
    /// too, which can come first on `PATH`, so herder runs the name only Cursor uses.
    ///
    /// Cursor's docs say `CURSOR_CONFIG_DIR` moves `cli-config.json`; its Linux login is
    /// `$XDG_CONFIG_HOME/cursor/auth.json`, so both point at the account's config dir. On macOS
    /// the login is in the Keychain and accounts cannot be separated this way.
    pub fn cursor() -> Self {
        Self {
            provider: Provider::Cursor,
            program: "cursor-agent".into(),
            args: Vec::new(),
            model_flag: Some("--model".into()),
            trailing_args: strings(&["acp"]),
            config_dir_vars: strings(&["CURSOR_CONFIG_DIR", "XDG_CONFIG_HOME"]),
            launch_env: Vec::new(),
            login_env: Vec::new(),
            skills: SkillsLaunch::Flag("--plugin-dir".into()),
            images: false,
            skill_mention: SkillMention::Slash,
        }
    }

    /// Whether the starting model goes on the command line.
    pub(super) fn passes_model(&self, request: &StartRequest) -> bool {
        self.model_flag.is_some() && request.model.is_some()
    }

    /// The command that runs the agent for `request`: its environment is exactly the request's,
    /// plus the config dir variables and minus the login variables when the account has a
    /// config dir, and the launch variables, with herder's skill library. Stderr is discarded.
    pub fn command(&self, request: &StartRequest) -> Command {
        let mut command = request.command(&self.program);
        command.args(&self.args);
        if let (Some(flag), Some(model)) = (&self.model_flag, &request.model) {
            command.args([flag, model]);
        }
        if let (SkillsLaunch::Flag(flag), Some(dir)) = (&self.skills, &request.skills) {
            command.arg(flag).arg(dir);
        }
        command
            .args(&self.trailing_args)
            .env_clear()
            .envs(&request.env)
            .envs(
                request
                    .config_dir
                    .iter()
                    .flat_map(|dir| self.config_dir_vars.iter().map(move |var| (var, dir))),
            )
            .envs(self.launch_env.iter().map(|(var, value)| (var, value)))
            .current_dir(&request.cwd)
            .stderr(Stdio::null());
        if request.config_dir.is_some() {
            for var in &self.login_env {
                command.env_remove(var);
            }
        }
        if let (SkillsLaunch::OpencodeConfig, Some(dir)) = (&self.skills, &request.skills) {
            let config = opencode_config(request.env.get(OPENCODE_CONFIG_CONTENT), dir);
            command.env(OPENCODE_CONFIG_CONTENT, config);
        }
        command
    }
}

const OPENCODE_CONFIG_CONTENT: &str = "OPENCODE_CONFIG_CONTENT";

/// `OPENCODE_CONFIG_CONTENT` with `dir` added to its `skills.paths`, keeping whatever config
/// `existing`, the variable as the environment had it, held; an `existing` that is not a JSON
/// object is replaced.
fn opencode_config(existing: Option<&String>, dir: &Path) -> String {
    let mut config = existing
        .and_then(|config| serde_json::from_str::<serde_json::Value>(config).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    let dir = serde_json::Value::from(dir.to_string_lossy());
    if let Some(config) = config.as_object_mut() {
        let skills = config
            .entry("skills")
            .and_modify(|skills| {
                if !skills.is_object() {
                    *skills = serde_json::json!({});
                }
            })
            .or_insert_with(|| serde_json::json!({}));
        if let Some(skills) = skills.as_object_mut() {
            match skills
                .get_mut("paths")
                .and_then(serde_json::Value::as_array_mut)
            {
                Some(paths) => paths.push(dir),
                None => {
                    skills.insert("paths".into(), serde_json::json!([dir]));
                }
            }
        }
    }
    config.to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsStr;
    use std::path::PathBuf;

    use herder_protocol::PermissionMode;

    use super::*;

    fn request(model: Option<&str>) -> StartRequest {
        StartRequest {
            config_dir: Some(PathBuf::from("/accounts/work")),
            env: BTreeMap::from([("PATH".into(), "/usr/bin".into())]),
            cwd: PathBuf::from("/worktrees/s1"),
            model: model.map(Into::into),
            permission_mode: PermissionMode::Ask,
            seed: Vec::new(),
            resume: None,
            mcp: None,
            launcher: Vec::new(),
            skills: None,
        }
    }

    fn args(command: &Command) -> Vec<&OsStr> {
        command.as_std().get_args().collect()
    }

    fn env(command: &Command) -> Vec<(&OsStr, Option<&OsStr>)> {
        command.as_std().get_envs().collect()
    }

    #[test]
    fn model_flag_goes_between_the_arguments() {
        let grok = AgentProfile::grok();
        assert_eq!(
            args(&grok.command(&request(Some("grok-4.5")))),
            ["agent", "-m", "grok-4.5", "stdio"]
        );
        assert_eq!(args(&grok.command(&request(None))), ["agent", "stdio"]);
        let cursor = AgentProfile::cursor().command(&request(Some("gpt-5")));
        assert_eq!(cursor.as_std().get_program(), "cursor-agent");
        assert_eq!(args(&cursor), ["--model", "gpt-5", "acp"]);
        assert_eq!(
            args(&AgentProfile::opencode().command(&request(Some("opencode/big-pickle")))),
            ["acp"]
        );
    }

    #[test]
    fn environment_is_the_request_plus_the_account_dir() {
        let command = AgentProfile::opencode().command(&request(None));
        let mut vars = env(&command);
        vars.sort();
        assert_eq!(
            vars,
            [
                (
                    OsStr::new("OPENCODE_PERMISSION"),
                    Some(OsStr::new(r#"{"*":"ask"}"#))
                ),
                (OsStr::new("PATH"), Some(OsStr::new("/usr/bin"))),
                (
                    OsStr::new("XDG_DATA_HOME"),
                    Some(OsStr::new("/accounts/work"))
                ),
            ]
        );
        assert_eq!(
            command.as_std().get_current_dir(),
            Some(std::path::Path::new("/worktrees/s1"))
        );
        let cursor = AgentProfile::cursor().command(&request(None));
        let vars: Vec<_> = env(&cursor)
            .into_iter()
            .filter(|(_, value)| *value == Some(OsStr::new("/accounts/work")))
            .map(|(var, _)| var)
            .collect();
        assert_eq!(vars, ["CURSOR_CONFIG_DIR", "XDG_CONFIG_HOME"]);
    }

    #[test]
    fn without_a_config_dir_no_config_dir_variable_is_set() {
        let request = StartRequest {
            config_dir: None,
            ..request(None)
        };
        let command = AgentProfile::cursor().command(&request);
        let mut vars = env(&command);
        vars.sort();
        assert_eq!(vars, [(OsStr::new("PATH"), Some(OsStr::new("/usr/bin")))]);
    }

    #[test]
    fn grok_drops_an_api_key_login_only_for_an_account_with_its_own_dir() {
        let with_key = |config_dir: Option<&str>| StartRequest {
            config_dir: config_dir.map(PathBuf::from),
            env: BTreeMap::from([
                ("PATH".into(), "/usr/bin".into()),
                ("XAI_API_KEY".into(), "xai-key".into()),
                ("GROK_CODE_XAI_API_KEY".into(), "xai-key".into()),
            ]),
            ..request(None)
        };
        let grok = AgentProfile::grok();
        let command = grok.command(&with_key(Some("/accounts/work")));
        let set: Vec<_> = env(&command)
            .into_iter()
            .filter(|(_, value)| value.is_some())
            .map(|(var, _)| var)
            .collect();
        assert_eq!(set, ["GROK_HOME", "PATH"]);
        let command = grok.command(&with_key(None));
        let mut vars: Vec<_> = env(&command).into_iter().map(|(var, _)| var).collect();
        vars.sort();
        assert_eq!(vars, ["GROK_CODE_XAI_API_KEY", "PATH", "XAI_API_KEY"]);
    }

    #[test]
    fn skills_reach_cursor_as_a_plugin_and_opencode_through_its_config() {
        let with_skills = |env: BTreeMap<String, String>| StartRequest {
            env,
            skills: Some(PathBuf::from("/data/skill-links/cursor")),
            ..request(Some("gpt-5"))
        };
        let path = || BTreeMap::from([("PATH".to_owned(), "/usr/bin".to_owned())]);
        let cursor = AgentProfile::cursor().command(&with_skills(path()));
        assert_eq!(
            args(&cursor),
            [
                "--model",
                "gpt-5",
                "--plugin-dir",
                "/data/skill-links/cursor",
                "acp"
            ]
        );
        let config = |command: &Command| -> serde_json::Value {
            let value = env(command)
                .into_iter()
                .find(|(var, _)| *var == "OPENCODE_CONFIG_CONTENT")
                .and_then(|(_, value)| value)
                .unwrap();
            serde_json::from_str(value.to_str().unwrap()).unwrap()
        };
        let opencode = AgentProfile::opencode();
        let command = opencode.command(&with_skills(path()));
        assert_eq!(args(&command), ["acp"]);
        assert_eq!(
            config(&command),
            serde_json::json!({"skills": {"paths": ["/data/skill-links/cursor"]}})
        );
        // Config the user set through the variable is kept.
        let mut env = path();
        env.insert(
            "OPENCODE_CONFIG_CONTENT".into(),
            r#"{"theme":"dark","skills":{"paths":["/mine"]}}"#.into(),
        );
        assert_eq!(
            config(&opencode.command(&with_skills(env))),
            serde_json::json!({
                "theme": "dark",
                "skills": {"paths": ["/mine", "/data/skill-links/cursor"]}
            })
        );
        let grok = AgentProfile::grok().command(&with_skills(path()));
        assert_eq!(args(&grok), ["agent", "-m", "gpt-5", "stdio"]);
    }

    #[test]
    fn command_runs_behind_the_launcher_and_directly_without_one() {
        let grok = AgentProfile::grok();
        let launched = StartRequest {
            launcher: crate::testing::launcher(),
            ..request(Some("grok-4.5"))
        };
        let direct = grok.command(&request(Some("grok-4.5")));
        assert_eq!(direct.as_std().get_program(), "grok");
        crate::testing::assert_behind_launcher(&direct, &grok.command(&launched));
    }
}
