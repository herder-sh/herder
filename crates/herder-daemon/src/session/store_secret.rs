//! `store_secret`: an agent stores a file's contents as a secret on its host's vault without
//! ever seeing them.
//!
//! The daemon reads the file itself, on the host: at most [`MAX_SECRET_VALUE_BYTES`] of text
//! without a NUL, as [`SecretValue`] holds. Files of the providers' logins are refused: herder
//! never copies a login token. The value stays in the session's actor while the approval it
//! raises is open ([`actor::Request::StoreSecret`]); neither the approval, nor the tool's
//! result, nor any log carries it, only its path, size and [`fingerprint`].
//!
//! The approval is always the user's. An owner allows it from an app, which takes the value
//! with `get_secret_to_store`, puts it on the vault as that owner with `put_secret`, and says
//! so with `confirm_secret_stored`: the host's link to its vault only reads secrets, and the
//! agent never gets the right to write one. Only then is the file deleted, when the agent
//! asked. A denial, or the turn ending first, stores nothing.

use std::io::Read;
use std::path::{Path, PathBuf};

use herder_protocol::{
    MAX_SECRET_DESCRIPTION_CHARS, MAX_SECRET_VALUE_BYTES, SecretStoreRequest, SecretValue,
    SessionId, is_valid_secret_name, is_valid_secret_set_name,
};
use herder_tasktools::{ErrorCode, StoreSecretInput, StoreSecretOutput, ToolError};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

use super::SessionManager;
use super::actor;

/// The files and folders of the providers' logins under the home folder, which `store_secret`
/// never reads; accounts' own config dirs are refused too.
const LOGIN_PATHS: [&str; 5] = [".claude", ".claude.json", ".codex", ".gemini", ".cursor"];

impl SessionManager {
    /// `store_secret` of `caller`: reads the file, asks the user, and answers once an owner
    /// stored it or the user denied it.
    pub(super) async fn store_secret(
        &self,
        caller: SessionId,
        input: StoreSecretInput,
    ) -> Result<StoreSecretOutput, ToolError> {
        let linked = self
            .inner
            .secrets
            .get()
            .is_some_and(|secrets| secrets.linked());
        if !linked {
            return Err(ToolError::new(
                ErrorCode::Unavailable,
                "this machine is not linked to a vault with Secrets on, so it has nowhere to \
                 store a secret; ask an owner to link it to a vault in the herder app (the \
                 machine's page, Secrets), then try again",
            ));
        }
        check(&input)?;
        let session = self.caller(&caller).await?;
        let path = resolve(&input.from_file, Path::new(&session.worktree))?;
        self.refuse_login(&path)?;
        let value = read(&path, &input.from_file).await?;
        let bytes = value.expose().len() as u64;
        let fingerprint = fingerprint(&value);
        let store = SecretStoreRequest {
            path: input.from_file.clone(),
            bytes,
            fingerprint: fingerprint.clone(),
            set: input.set,
            name: input.name.clone(),
            description: input.description,
            delete_file_after: input.delete_file_after,
        };
        let (done, outcome) = oneshot::channel();
        let request = actor::Request::StoreSecret { store, value, done };
        self.send(caller.clone(), None, request)
            .await
            .map_err(super::tasks::tool_error)?;
        // The turn uses no CPU while it waits for the owner: its slot goes to others meanwhile.
        let parked = self.inner.admission.get().and_then(|a| a.park(&caller));
        let outcome = outcome.await;
        drop(parked);
        let set = outcome.map_err(|_| {
            ToolError::new(
                ErrorCode::NotAllowed,
                "the request to store the secret closed unanswered, as its turn ended; nothing \
                 was stored",
            )
        })??;
        let file_deleted = input.delete_file_after && tokio::fs::remove_file(&path).await.is_ok();
        Ok(StoreSecretOutput {
            set,
            name: input.name,
            bytes,
            fingerprint,
            file_deleted,
        })
    }

    /// Refuses `path` when it is within a provider's login: an account's config dir, or a
    /// provider CLI's default one.
    fn refuse_login(&self, path: &Path) -> Result<(), ToolError> {
        let mut logins: Vec<PathBuf> = self
            .inner
            .accounts_lock()
            .values()
            .filter_map(|account| account.config_dir.clone())
            .collect();
        if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) {
            logins.extend(LOGIN_PATHS.iter().map(|login| Path::new(&home).join(login)));
        }
        let within = logins.iter().any(|login| {
            let login = std::fs::canonicalize(login).unwrap_or_else(|_| login.clone());
            path.starts_with(login)
        });
        if within {
            return Err(ToolError::new(
                ErrorCode::NotAllowed,
                "that file is part of an AI provider's login, which herder never copies",
            ));
        }
        Ok(())
    }
}

/// Refuses a set, name or description the vault would refuse, before the user is asked.
fn check(input: &StoreSecretInput) -> Result<(), ToolError> {
    let invalid = |message: &str| ToolError::new(ErrorCode::InvalidArguments, message);
    if !is_valid_secret_set_name(&input.set) {
        return Err(invalid(
            "`set` is a set's name: lowercase letters, digits and hyphens, such as `personal`",
        ));
    }
    if !is_valid_secret_name(&input.name) {
        return Err(invalid(
            "`name` is an environment variable's name: uppercase letters, digits and \
             underscores, not starting with a digit, such as `ASC_PRIVATE_KEY`",
        ));
    }
    let description = &input.description;
    if description.chars().count() > MAX_SECRET_DESCRIPTION_CHARS || description.contains('\n') {
        return Err(invalid(
            "`description` is one line of at most 200 characters",
        ));
    }
    Ok(())
}

/// The file `from_file` names: `~/` is the home folder, and a relative path is within
/// `worktree`; with its links followed.
fn resolve(from_file: &str, worktree: &Path) -> Result<PathBuf, ToolError> {
    let home = || std::env::var_os("HOME").filter(|home| !home.is_empty());
    let path = match from_file.strip_prefix("~/") {
        Some(rest) => match home() {
            Some(home) => Path::new(&home).join(rest),
            None => {
                return Err(ToolError::new(
                    ErrorCode::InvalidArguments,
                    "this machine has no home folder for `~/`; give the file's absolute path",
                ));
            }
        },
        None => worktree.join(from_file),
    };
    std::fs::canonicalize(&path).map_err(|err| {
        ToolError::new(
            ErrorCode::InvalidArguments,
            format!("cannot open {from_file}: {err}"),
        )
    })
}

/// The value in the file at `path`, which `from_file` named: refused when it is larger than
/// [`MAX_SECRET_VALUE_BYTES`], not text, or holds a NUL. No error carries its contents.
async fn read(path: &Path, from_file: &str) -> Result<SecretValue, ToolError> {
    let path = path.to_owned();
    let read = tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&path)?;
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::other("it is not a file"));
        }
        let mut bytes = Vec::new();
        file.take(MAX_SECRET_VALUE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    })
    .await;
    let bytes = match read {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(err)) => {
            return Err(ToolError::new(
                ErrorCode::InvalidArguments,
                format!("cannot read {from_file}: {err}"),
            ));
        }
        Err(err) => return Err(ToolError::new(ErrorCode::Internal, err.to_string())),
    };
    let refused = |why: &str| {
        ToolError::new(
            ErrorCode::InvalidArguments,
            format!("{from_file} cannot be a secret: {why}"),
        )
    };
    if bytes.len() > MAX_SECRET_VALUE_BYTES {
        return Err(refused("a secret holds at most 64 KiB"));
    }
    let text = String::from_utf8(bytes).map_err(|_| refused("a secret is text, and it is not"))?;
    SecretValue::new(text).ok_or_else(|| refused("a secret is not empty and holds no NUL"))
}

/// The first 8 hex digits of `value`'s SHA-256: enough to tell values apart, too little to
/// recover one.
pub(crate) fn fingerprint(value: &SecretValue) -> String {
    Sha256::digest(value.expose().as_bytes())
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(set: &str, name: &str, description: &str) -> StoreSecretInput {
        StoreSecretInput {
            set: set.into(),
            name: name.into(),
            description: description.into(),
            from_file: "key.p8".into(),
            delete_file_after: false,
        }
    }

    #[test]
    fn check_refuses_what_the_vault_would() {
        assert!(check(&input("personal", "ASC_PRIVATE_KEY", "App Store key")).is_ok());
        for bad in [
            input("Personal", "ASC_PRIVATE_KEY", ""),
            input("personal", "asc_key", ""),
            input("personal", "KEY", "two\nlines"),
            input("personal", "KEY", &"x".repeat(201)),
        ] {
            let err = check(&bad).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidArguments);
        }
    }

    #[tokio::test]
    async fn read_caps_the_value_and_keeps_newlines() {
        let dir = tempfile::tempdir().unwrap();
        let pem = "-----BEGIN PRIVATE KEY-----\nMIGT\n-----END PRIVATE KEY-----\n";
        let key = dir.path().join("key.p8");
        std::fs::write(&key, pem).unwrap();
        assert_eq!(read(&key, "key.p8").await.unwrap().expose(), pem);

        let full = dir.path().join("full");
        std::fs::write(&full, "k".repeat(MAX_SECRET_VALUE_BYTES)).unwrap();
        assert!(read(&full, "full").await.is_ok());
        let large = dir.path().join("large");
        std::fs::write(&large, "k".repeat(MAX_SECRET_VALUE_BYTES + 1)).unwrap();
        let err = read(&large, "large").await.unwrap_err();
        assert!(err.message.contains("64 KiB"), "{}", err.message);
        assert!(!err.message.contains("kkk"));

        let binary = dir.path().join("binary");
        std::fs::write(&binary, [0xff, 0xfe, 0x00]).unwrap();
        assert!(read(&binary, "binary").await.is_err());
        assert!(read(dir.path(), "dir").await.is_err());
    }

    #[test]
    fn resolve_reads_relative_paths_in_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("key.p8"), "k").unwrap();
        let resolved = resolve("key.p8", dir.path()).unwrap();
        assert_eq!(resolved, dir.path().join("key.p8").canonicalize().unwrap());
        assert!(resolve("missing", dir.path()).is_err());
    }

    #[test]
    fn fingerprint_is_short_and_stable() {
        let value = SecretValue::new("secret").unwrap();
        assert_eq!(fingerprint(&value), "2bb80d53");
    }
}
