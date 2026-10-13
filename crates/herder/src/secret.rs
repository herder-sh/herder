//! `herder secret run`: run a program with secrets from the vault in its environment, from a
//! session herder runs. The session's agent never sees a value: the daemon hands the values
//! to this process over its MCP socket, with the session's token, and they go to the program
//! as environment variables, never on a command line.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use herder_protocol::{SecretError, SecretErrorCode, SecretRef, SessionId};

#[derive(clap::Subcommand)]
pub enum Command {
    /// Run a program with secrets in its environment, each as the variable NAME.
    Run(RunArgs),
}

#[derive(clap::Args)]
pub struct RunArgs {
    /// The secrets: NAME, or set/NAME when more than one set granted to this host has NAME.
    #[arg(required = true, value_name = "SECRET")]
    secrets: Vec<String>,
    /// The program and its arguments, after `--`.
    #[arg(last = true, required = true, value_name = "COMMAND")]
    command: Vec<OsString>,
    /// The session to fetch them for [default: the session herder runs this in].
    #[arg(
        long,
        value_name = "ID",
        env = "HERDER_SESSION_ID",
        hide_env_values = true
    )]
    session: Option<String>,
    /// The daemon's data dir [default: the one herder runs this session from].
    #[arg(long, value_name = "PATH", env = herder_daemon::mcp::DATA_DIR_ENV)]
    data_dir: Option<PathBuf>,
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Run(args) => run_with_secrets(args),
    }
}

fn run_with_secrets(args: RunArgs) -> Result<()> {
    let (Some(session), Some(data_dir)) = (args.session, args.data_dir) else {
        bail!(
            "herder secret run works only in a session herder runs, which sets \
             HERDER_SESSION_ID and {}",
            herder_daemon::mcp::DATA_DIR_ENV
        );
    };
    let secrets = args
        .secrets
        .iter()
        .map(|secret| secret.parse::<SecretRef>().map_err(anyhow::Error::msg))
        .collect::<Result<Vec<_>>>()?;
    for (i, secret) in secrets.iter().enumerate() {
        if let Some(other) = secrets[..i].iter().find(|other| other.name == secret.name) {
            bail!(
                "{other} and {secret} would both be ${}; run them in separate commands",
                secret.name
            );
        }
    }
    let session = SessionId::new(session);
    let fetched = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the tokio runtime")?
        .block_on(herder_daemon::mcp::fetch_secrets(
            &data_dir, &session, &secrets,
        ))?;
    let fetched = fetched.map_err(|error| anyhow::Error::msg(refusal(&error)))?;
    let (program, arguments) = args
        .command
        .split_first()
        .context("name the program to run after `--`")?;
    let err = std::process::Command::new(program)
        .args(arguments)
        .envs(
            fetched
                .iter()
                .map(|secret| (&secret.secret.name, secret.value.expose())),
        )
        .exec();
    Err(anyhow::Error::new(err).context(format!("cannot run {}", program.to_string_lossy())))
}

/// What to tell the agent when the secrets were refused.
fn refusal(error: &SecretError) -> String {
    match error.code {
        SecretErrorCode::Ambiguous => {
            let candidates: Vec<String> =
                error.candidates.iter().map(ToString::to_string).collect();
            let secret = error
                .secret
                .as_ref()
                .map_or_else(|| "the secret".to_owned(), ToString::to_string);
            format!(
                "{secret} is in more than one set granted to this host; name one of {}",
                candidates.join(", ")
            )
        }
        SecretErrorCode::NoVault => format!(
            "{}; link this host to a vault in a herder app to use secrets",
            error.message
        ),
        _ => error.message.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ambiguous_secret_lists_what_to_name_instead() {
        let error = SecretError {
            code: SecretErrorCode::Ambiguous,
            message: "more than one".into(),
            secret: Some("TOKEN".parse().unwrap()),
            candidates: vec!["dev/TOKEN".parse().unwrap(), "prod/TOKEN".parse().unwrap()],
        };
        assert_eq!(
            refusal(&error),
            "TOKEN is in more than one set granted to this host; name one of dev/TOKEN, \
             prod/TOKEN"
        );
    }
}
