//! Native handoff: a same-provider account switch carries the CLI's own session over, so the
//! new account's CLI resumes it with full context and tool state instead of a replay.
//!
//! The one file copied is the session's transcript, found by
//! [`herder_adapters::transcript::locate`] in the old account's config dir and written to the
//! same relative path in the new account's: for Claude `projects/<cwd key>/<id>.jsonl`, for
//! Codex `sessions/YYYY/MM/DD/rollout-<timestamp>-<id>.jsonl`. Nothing else in either config
//! dir is read or written, credentials least of all. Directories created on the way are
//! owner-only; an existing copy is replaced, since the old account's is the newer.

use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

use anyhow::{Context, Result, bail};
use herder_adapters::transcript;
use herder_protocol::Provider;

/// Copies `provider`'s transcript of session `native_id` from config dir `from` to config dir
/// `to`; fails when `from` has none.
pub fn carry_over(provider: &Provider, from: &Path, to: &Path, native_id: &str) -> Result<()> {
    let Some(relative) = transcript::locate(provider, from, native_id)
        .with_context(|| format!("looking for the transcript in {}", from.display()))?
    else {
        bail!(
            "{} has no transcript of session {native_id}",
            from.display()
        );
    };
    if from == to {
        return Ok(());
    }
    let target = to.join(&relative);
    if let Some(parent) = target.parent() {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    // Written beside the target and renamed over it, so the CLI never reads half a copy.
    let partial = target.with_extension("jsonl.herder-partial");
    std::fs::copy(from.join(&relative), &partial)
        .with_context(|| format!("copying the transcript to {}", partial.display()))?;
    std::fs::rename(&partial, &target)
        .with_context(|| format!("moving the transcript to {}", target.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// Every file under `dir`, relative to it, sorted.
    fn files(dir: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut pending = vec![dir.to_owned()];
        while let Some(next) = pending.pop() {
            for entry in std::fs::read_dir(next).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    found.push(path.strip_prefix(dir).unwrap().to_owned());
                }
            }
        }
        found.sort();
        found
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn only_the_transcript_moves_to_the_same_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("a"), dir.path().join("b"));
        let transcript = "projects/-work-app/s1.jsonl";
        write(&from.join(transcript), "full history\n");
        write(&from.join(".credentials.json"), "secret");
        write(
            &from.join("projects/-work-app/s0.jsonl"),
            "another session\n",
        );
        write(&to.join(".credentials.json"), "b's own");
        write(&to.join(transcript), "stale\n");

        carry_over(&Provider::Claude, &from, &to, "s1").unwrap();

        assert_eq!(
            files(&to),
            [
                PathBuf::from(".credentials.json"),
                PathBuf::from(transcript)
            ]
        );
        assert_eq!(
            std::fs::read_to_string(to.join(transcript)).unwrap(),
            "full history\n"
        );
        assert_eq!(
            std::fs::read_to_string(to.join(".credentials.json")).unwrap(),
            "b's own"
        );
    }

    #[test]
    fn created_dirs_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("a"), dir.path().join("b"));
        let rollout = "sessions/2026/10/02/rollout-2026-10-02T14-58-50-t1.jsonl";
        write(&from.join(rollout), "{}\n");
        carry_over(&Provider::Codex, &from, &to, "t1").unwrap();
        for created in ["sessions", "sessions/2026/10/02"] {
            let mode = std::fs::metadata(to.join(created))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700, "{created}");
        }
    }

    #[test]
    fn a_missing_transcript_fails() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("a"), dir.path().join("b"));
        assert!(carry_over(&Provider::Claude, &from, &to, "s1").is_err());
        assert!(!to.exists());
    }
}
