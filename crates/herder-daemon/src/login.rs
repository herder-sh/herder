//! Logins: adding an account by running its provider's own login in a login terminal.
//!
//! Each login runs in a fresh config dir, handed to the provider's CLI through its config dir
//! variable, on a pseudo-terminal relayed to the owner who asked ([`crate::terminal`]). The
//! owner completes the provider's own flow there; herder never reads what it writes.
//!
//! Owner-only access is enforced before commands get here, by [`crate::auth::authorize`].

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::ErrorKind;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use herder_protocol::{Account, AccountId, ErrorCode, ErrorInfo, Provider};
use portable_pty::CommandBuilder;

use crate::config::{ID_RULE, resolve_path, valid_id};

/// How to log in to one provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginProgram {
    /// The CLI to run.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<String>,
    /// The variable that points the CLI at the account's config dir.
    pub config_env: String,
}

/// The login of every provider herder can add accounts of. Cheap to clone.
#[derive(Clone, Default)]
pub struct Logins {
    programs: Arc<HashMap<Provider, LoginProgram>>,
}

/// An account to add, as the owner asked for it.
pub(crate) struct NewAccount<'a> {
    pub account_id: &'a AccountId,
    pub provider: &'a Provider,
    pub config_dir: Option<&'a str>,
}

impl Logins {
    /// Logins running `programs`; no other provider's accounts can be added.
    pub fn new(programs: HashMap<Provider, LoginProgram>) -> Self {
        Self {
            programs: Arc::new(programs),
        }
    }

    /// The login of `account`, in its config dir, which is created empty. `accounts` are the
    /// daemon's accounts and `logging_in` the accounts with a login running; the new id must
    /// be neither.
    pub(crate) fn command(
        &self,
        account: &NewAccount<'_>,
        accounts: &[Account],
        logging_in: &[AccountId],
    ) -> Result<CommandBuilder, ErrorInfo> {
        self.command_with_env(account, accounts, logging_in, |key| std::env::var_os(key))
    }

    fn command_with_env(
        &self,
        account: &NewAccount<'_>,
        accounts: &[Account],
        logging_in: &[AccountId],
        env: impl Fn(&str) -> Option<OsString>,
    ) -> Result<CommandBuilder, ErrorInfo> {
        let NewAccount {
            account_id,
            provider,
            config_dir,
        } = account;
        if !valid_id(account_id.as_str()) {
            return Err(error(
                ErrorCode::BadRequest,
                format!("account id {:?} {ID_RULE}", account_id.as_str()),
            ));
        }
        if accounts.iter().any(|a| a.account_id == **account_id) || logging_in.contains(account_id)
        {
            return Err(error(
                ErrorCode::Conflict,
                format!("account {account_id} already exists"),
            ));
        }
        let program = self.programs.get(provider).ok_or_else(|| {
            error(
                ErrorCode::Unsupported,
                format!("herder cannot add {} accounts", provider.as_str()),
            )
        })?;
        let default = format!("~/.{}-{account_id}", provider.as_str());
        let dir = resolve_path(Path::new(config_dir.unwrap_or(&default)), &env)
            .map_err(|err| error(ErrorCode::BadRequest, format!("config dir: {err:#}")))?;
        fresh_dir(&dir)?;
        let mut command = CommandBuilder::new(&program.program);
        command.args(&program.args);
        command.cwd(&dir);
        command.env(&program.config_env, &dir);
        Ok(command)
    }
}

/// Creates `dir`, owner-only, unless it is an empty directory already: a login never lands
/// on top of another.
fn fresh_dir(dir: &Path) -> Result<(), ErrorInfo> {
    match std::fs::read_dir(dir).map(|mut entries| entries.next().is_none()) {
        Ok(true) => return Ok(()),
        Ok(false) => {
            return Err(error(
                ErrorCode::Conflict,
                format!(
                    "{} is not empty; an account needs a fresh config dir",
                    dir.display()
                ),
            ));
        }
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
        })
}

fn error(code: ErrorCode, message: String) -> ErrorInfo {
    ErrorInfo { code, message }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logins() -> Logins {
        Logins::new(HashMap::from([(
            Provider::Codex,
            LoginProgram {
                program: PathBuf::from("codex"),
                args: vec!["login".into(), "--device-auth".into()],
                config_env: "CODEX_HOME".into(),
            },
        )]))
    }

    fn try_login(
        home: &Path,
        id: &str,
        provider: Provider,
        config_dir: Option<&str>,
        accounts: &[Account],
    ) -> Result<CommandBuilder, ErrorInfo> {
        let home = home.as_os_str().to_owned();
        let account_id = AccountId::new(id);
        let account = NewAccount {
            account_id: &account_id,
            provider: &provider,
            config_dir,
        };
        let logging_in = [AccountId::new("busy")];
        logins().command_with_env(&account, accounts, &logging_in, move |key| {
            (key == "HOME").then(|| home.clone())
        })
    }

    #[test]
    fn a_login_runs_in_a_fresh_owner_only_config_dir() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().unwrap();
        let command = try_login(home.path(), "codex-2", Provider::Codex, None, &[]).unwrap();
        let dir = home.path().join(".codex-codex-2");
        assert_eq!(command.get_argv(), &["codex", "login", "--device-auth"]);
        assert_eq!(command.get_env("CODEX_HOME"), Some(dir.as_os_str()));
        assert_eq!(command.get_cwd(), Some(&dir.as_os_str().to_owned()));
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);

        // An empty dir is still fresh.
        try_login(
            home.path(),
            "codex-3",
            Provider::Codex,
            Some("~/.codex-codex-2"),
            &[],
        )
        .unwrap();
    }

    #[test]
    fn a_login_never_reuses_an_id_or_a_config_dir_in_use() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("used")).unwrap();
        std::fs::write(home.path().join("used/auth.json"), "").unwrap();
        let existing = [Account {
            account_id: AccountId::new("codex"),
            provider: Provider::Codex,
            label: "Codex".into(),
            usage: Vec::new(),
        }];
        let cases = [
            ("codex", Provider::Codex, None, ErrorCode::Conflict),
            ("busy", Provider::Codex, None, ErrorCode::Conflict),
            ("a b", Provider::Codex, None, ErrorCode::BadRequest),
            ("new", Provider::Gemini, None, ErrorCode::Unsupported),
            ("new", Provider::Codex, Some("rel"), ErrorCode::BadRequest),
            ("new", Provider::Codex, Some("~/used"), ErrorCode::Conflict),
        ];
        for (id, provider, dir, code) in cases {
            let err = try_login(home.path(), id, provider, dir, &existing)
                .err()
                .unwrap_or_else(|| panic!("{id} {dir:?} was accepted"));
            assert_eq!(err.code, code, "{id} {dir:?}: {}", err.message);
        }
        assert!(!home.path().join(".codex-new").exists());
    }
}
