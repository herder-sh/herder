//! What differs between ACP agents: how to launch one for an account.
//!
//! | Agent    | Command            | Account config dir                         | Verified                                 |
//! | -------- | ------------------ | ------------------------------------------ | ---------------------------------------- |
//! | OpenCode | `opencode acp`     | `XDG_DATA_HOME` (`opencode/auth.json`)     | 1.18.21, credentials path moves          |
//! | Grok     | `grok agent stdio` | `GROK_HOME` (replaces `~/.grok`)           | 1.0.46, config moves; login not tried    |
//! | Cursor   | `agent acp`        | `CURSOR_CONFIG_DIR` and `XDG_CONFIG_HOME`  | no: from Cursor's docs, CLI not available |
//!
//! Each agent must ask before every write or command, so the adapter can apply the session's
//! permission mode: OpenCode is launched with `OPENCODE_PERMISSION={"*":"ask"}`, as by default
//! it runs most tools unasked; Grok and Cursor ask by default over ACP.

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
        }
    }

    /// Grok, `grok agent [-m <model>] stdio`.
    ///
    /// `GROK_HOME` replaces `~/.grok`, where its config and login live.
    pub fn grok() -> Self {
        Self {
            provider: Provider::Grok,
            program: "grok".into(),
            args: strings(&["agent"]),
            model_flag: Some("-m".into()),
            trailing_args: strings(&["stdio"]),
            config_dir_vars: strings(&["GROK_HOME"]),
            launch_env: Vec::new(),
        }
    }

    /// Cursor, `agent [--model <model>] acp`. Unverified: no Cursor CLI was available.
    ///
    /// Cursor's docs say `CURSOR_CONFIG_DIR` moves `cli-config.json`; its Linux login is
    /// `$XDG_CONFIG_HOME/cursor/auth.json`, so both point at the account's config dir. On macOS
    /// the login is in the Keychain and accounts cannot be separated this way.
    pub fn cursor() -> Self {
        Self {
            provider: Provider::Cursor,
            program: "agent".into(),
            args: Vec::new(),
            model_flag: Some("--model".into()),
            trailing_args: strings(&["acp"]),
            config_dir_vars: strings(&["CURSOR_CONFIG_DIR", "XDG_CONFIG_HOME"]),
            launch_env: Vec::new(),
        }
    }

    /// Whether the starting model goes on the command line.
    pub(super) fn passes_model(&self, request: &StartRequest) -> bool {
        self.model_flag.is_some() && request.model.is_some()
    }

    /// The command that runs the agent for `request`: its environment is exactly the request's,
    /// plus the config dir variables when the account has a config dir, and the launch
    /// variables. Stderr is discarded.
    pub fn command(&self, request: &StartRequest) -> Command {
        let mut command = request.command(&self.program);
        command.args(&self.args);
        if let (Some(flag), Some(model)) = (&self.model_flag, &request.model) {
            command.args([flag, model]);
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
        command
    }
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
            mcp: None,
            launcher: Vec::new(),
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
        assert_eq!(cursor.as_std().get_program(), "agent");
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
