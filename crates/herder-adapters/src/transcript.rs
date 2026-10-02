//! Where a CLI keeps a session's transcript, so the daemon can move that one file to another
//! account's config dir and resume the session there natively.
//!
//! | provider | config dir without an account's own                 | transcript, under the config dir                      |
//! | -------- | --------------------------------------------------- | ----------------------------------------------------- |
//! | Claude   | `$CLAUDE_CONFIG_DIR`, else `~/.claude`              | `projects/<cwd key>/<id>.jsonl`                       |
//! | Codex    | `$CODEX_HOME`, else `~/.codex`                      | `sessions/YYYY/MM/DD/rollout-<timestamp>-<id>.jsonl`  |
//!
//! Finding a transcript lists only the directories on its path; no file is opened. Every
//! other provider has none herder can move.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use herder_protocol::Provider;

/// The config dir `provider`'s CLI uses: the account's own, else the CLI's default under
/// `env`, the environment it runs with. `None` for a provider without a transcript herder can
/// move, or without a home to find the default in.
pub fn config_dir(
    provider: &Provider,
    account_dir: Option<&Path>,
    env: &BTreeMap<String, String>,
) -> Option<PathBuf> {
    let (var, default) = match provider {
        Provider::Claude => ("CLAUDE_CONFIG_DIR", ".claude"),
        Provider::Codex => ("CODEX_HOME", ".codex"),
        _ => return None,
    };
    if let Some(dir) = account_dir {
        return Some(dir.to_owned());
    }
    if let Some(dir) = env.get(var).filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    env.get("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| Path::new(home).join(default))
}

/// The transcript of `provider`'s session `native_id` under `config_dir`, relative to it, or
/// `None` when there is none. An id that could name anything but a file is never looked up.
pub fn locate(
    provider: &Provider,
    config_dir: &Path,
    native_id: &str,
) -> io::Result<Option<PathBuf>> {
    if native_id.is_empty()
        || !native_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Ok(None);
    }
    match provider {
        Provider::Claude => {
            let name = format!("{native_id}.jsonl");
            find(config_dir, Path::new("projects"), 1, &|file| file == name)
        }
        Provider::Codex => {
            let suffix = format!("-{native_id}.jsonl");
            find(config_dir, Path::new("sessions"), 3, &|file| {
                file.starts_with("rollout-") && file.ends_with(&suffix)
            })
        }
        _ => Ok(None),
    }
}

/// The first file `matches` accepts exactly `depth` directories below `root.join(dir)`, as a
/// path relative to `root`. A directory that does not exist has none.
fn find(
    root: &Path,
    dir: &Path,
    depth: usize,
    matches: &dyn Fn(&str) -> bool,
) -> io::Result<Option<PathBuf>> {
    let entries = match std::fs::read_dir(root.join(dir)) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let mut names: Vec<_> = entries
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<io::Result<_>>()?;
    names.sort();
    for name in names {
        let path = dir.join(&name);
        let kind = std::fs::symlink_metadata(root.join(&path))?.file_type();
        if depth == 0 {
            if kind.is_file() && name.to_str().is_some_and(matches) {
                return Ok(Some(path));
            }
        } else if kind.is_dir()
            && let Some(found) = find(root, &path, depth - 1, matches)?
        {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(root: &Path, relative: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "{}\n").unwrap();
    }

    #[test]
    fn claude_transcripts_are_found_in_their_project_dir() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "projects/-work-app/0bea1056-aaaa.jsonl");
        touch(dir.path(), "projects/-work-app/other.jsonl");
        touch(dir.path(), "todos/0bea1056-aaaa.jsonl");
        assert_eq!(
            locate(&Provider::Claude, dir.path(), "0bea1056-aaaa").unwrap(),
            Some(PathBuf::from("projects/-work-app/0bea1056-aaaa.jsonl"))
        );
        assert_eq!(
            locate(&Provider::Claude, dir.path(), "missing").unwrap(),
            None
        );
    }

    #[test]
    fn codex_rollouts_are_found_by_their_thread_id() {
        let dir = tempfile::tempdir().unwrap();
        let rollout = "sessions/2026/10/02/rollout-2026-10-02T14-58-50-01a0fc7b.jsonl";
        touch(dir.path(), rollout);
        touch(
            dir.path(),
            "sessions/2026/10/02/rollout-2026-10-02T15-00-00-01a0fc7c.jsonl",
        );
        assert_eq!(
            locate(&Provider::Codex, dir.path(), "01a0fc7b").unwrap(),
            Some(PathBuf::from(rollout))
        );
    }

    #[test]
    fn nothing_is_found_without_a_dir_a_safe_id_or_a_known_provider() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "projects/key/a.jsonl");
        assert_eq!(locate(&Provider::Codex, dir.path(), "a").unwrap(), None);
        for id in ["", "..", "../a", "a/b", "a.jsonl"] {
            assert_eq!(
                locate(&Provider::Claude, dir.path(), id).unwrap(),
                None,
                "{id}"
            );
        }
        assert_eq!(locate(&Provider::Cursor, dir.path(), "a").unwrap(), None);
    }

    #[test]
    fn the_config_dir_is_the_accounts_else_the_clis_default() {
        let env = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect()
        };
        let home = env(&[("HOME", "/home/u")]);
        let own = Some(Path::new("/accounts/work"));
        assert_eq!(
            config_dir(&Provider::Claude, own, &home),
            Some(PathBuf::from("/accounts/work"))
        );
        assert_eq!(
            config_dir(&Provider::Claude, None, &home),
            Some(PathBuf::from("/home/u/.claude"))
        );
        assert_eq!(
            config_dir(&Provider::Codex, None, &home),
            Some(PathBuf::from("/home/u/.codex"))
        );
        assert_eq!(
            config_dir(
                &Provider::Codex,
                None,
                &env(&[("HOME", "/home/u"), ("CODEX_HOME", "/srv/codex")])
            ),
            Some(PathBuf::from("/srv/codex"))
        );
        assert_eq!(config_dir(&Provider::Claude, None, &env(&[])), None);
        assert_eq!(config_dir(&Provider::Cursor, own, &home), None);
    }
}
