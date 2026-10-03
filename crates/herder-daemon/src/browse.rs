//! Listing the host's folders, so an owner's client can pick a repository on a machine it is
//! not on. Owner-only access is enforced before commands get here, by
//! [`crate::auth::authorize`].

use std::io;
use std::path::{Path, PathBuf};

use herder_protocol::{CommandResult, DirectoryEntry, ErrorCode, ErrorInfo};

use crate::projects::scan;

/// `path` as a client gave it, absolute or starting with `~/`, made absolute with `~`
/// expanded and without a trailing slash.
pub(crate) fn absolute(path: &str) -> Result<PathBuf, ErrorInfo> {
    let path = crate::config::resolve_path(Path::new(path), &|key| std::env::var_os(key))
        .map_err(|err| error(ErrorCode::BadRequest, format!("{err:#}")))?;
    Ok(path.components().collect())
}

/// The entries of the folder at `path`, ordered by name; blocks on the file system. Entries
/// whose name is not UTF-8 are left out, as the protocol carries strings.
pub(crate) fn list(path: &str) -> Result<CommandResult, ErrorInfo> {
    let dir = absolute(path)?;
    let shown = || dir.display().to_string();
    let entries = std::fs::read_dir(&dir).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => {
            error(ErrorCode::NotFound, format!("{} does not exist", shown()))
        }
        io::ErrorKind::NotADirectory => error(
            ErrorCode::BadRequest,
            format!("{} is not a folder", shown()),
        ),
        _ => error(
            ErrorCode::Internal,
            format!("cannot list {}: {err}", shown()),
        ),
    })?;
    let mut listed = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| {
            error(
                ErrorCode::Internal,
                format!("cannot list {}: {err}", shown()),
            )
        })?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let path = entry.path();
        // Follows symlinks; a dangling one is no folder.
        let is_dir = path.is_dir();
        listed.push(DirectoryEntry {
            is_repo: is_dir && scan::is_repo(&path),
            is_dir,
            name,
        });
    }
    listed.sort_by(|a, b| a.name.cmp(&b.name));
    let path = dir
        .into_os_string()
        .into_string()
        .map_err(|_| error(ErrorCode::BadRequest, "the path is not UTF-8".to_owned()))?;
    Ok(CommandResult::Directory {
        path,
        entries: listed,
    })
}

fn error(code: ErrorCode, message: String) -> ErrorInfo {
    ErrorInfo { code, message }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_lists_its_entries_by_name_and_marks_repositories() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("app/.git")).unwrap();
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::write(root.join("README.md"), "hi").unwrap();
        std::os::unix::fs::symlink(root.join("app"), root.join("link")).unwrap();

        let path = format!("{}/", root.display());
        let Ok(CommandResult::Directory { path, entries }) = list(&path) else {
            panic!("expected a listing");
        };
        assert_eq!(Path::new(&path), root);
        let entry = |name: &str, is_dir, is_repo| DirectoryEntry {
            name: name.into(),
            is_dir,
            is_repo,
        };
        assert_eq!(
            entries,
            [
                entry("README.md", false, false),
                entry("app", true, true),
                entry("link", true, true),
                entry("notes", true, false),
            ]
        );
    }

    #[test]
    fn what_is_not_a_folder_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("file"), "").unwrap();
        let code = |path: &str| list(path).unwrap_err().code;
        assert_eq!(code("relative/dir"), ErrorCode::BadRequest);
        let missing = tmp.path().join("missing");
        assert_eq!(code(missing.to_str().unwrap()), ErrorCode::NotFound);
        let file = tmp.path().join("file");
        assert_eq!(code(file.to_str().unwrap()), ErrorCode::BadRequest);
    }
}
