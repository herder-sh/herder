//! Telling repositories apart and reading their `origin` remote.
//!
//! Both only read the file system: the remote comes from the repository's config file, so no
//! `git` process is spawned per repository.

use std::fs;
use std::path::{Path, PathBuf};

/// Whether `dir` is the top of a repository or of a linked worktree.
pub fn is_repo(dir: &Path) -> bool {
    dir.join(".git").symlink_metadata().is_ok()
}

/// The repository's `remote.origin.url`, as `git config --get remote.origin.url` gives it;
/// `None` when it has none or its config cannot be read.
pub fn origin(repo: &Path) -> Option<String> {
    let config = fs::read_to_string(common_dir(repo)?.join("config")).ok()?;
    origin_url(&config)
}

/// The directory holding the repository's config: `.git` itself, or for a linked worktree
/// or submodule, where its `.git` file points, following `commondir`.
fn common_dir(repo: &Path) -> Option<PathBuf> {
    let dot_git = repo.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let link = fs::read_to_string(&dot_git).ok()?;
    let git_dir = repo.join(link.trim().strip_prefix("gitdir:")?.trim());
    match fs::read_to_string(git_dir.join("commondir")) {
        Ok(common) => Some(git_dir.join(common.trim())),
        Err(_) => Some(git_dir),
    }
}

/// The last `url` of the `[remote "origin"]` section in a git config file.
pub(crate) fn origin_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    let mut url = None;
    for line in config.lines() {
        let mut line = line.trim();
        if let Some(header) = line.strip_prefix('[') {
            let Some((section, rest)) = header.split_once(']') else {
                continue;
            };
            in_origin = is_origin(section);
            line = rest.trim();
        }
        if !in_origin {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("url")
        {
            url = Some(value_of(value));
        }
    }
    url.filter(|url| !url.is_empty())
}

/// Whether a section header, without its brackets, names the `origin` remote: as
/// `remote "origin"`, or in the old `remote.origin` form.
fn is_origin(section: &str) -> bool {
    let section = section.trim();
    match section.split_once(char::is_whitespace) {
        Some((name, sub)) => name.eq_ignore_ascii_case("remote") && sub.trim() == "\"origin\"",
        None => section.eq_ignore_ascii_case("remote.origin"),
    }
}

/// A config value as git reads it: quotes removed, escapes applied, and an unquoted `#` or
/// `;` starting a comment.
fn value_of(raw: &str) -> String {
    let mut value = String::new();
    let mut quoted = false;
    let mut chars = raw.trim().chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' => match chars.next() {
                Some('n') => value.push('\n'),
                Some('t') => value.push('\t'),
                Some(other) => value.push(other),
                None => {}
            },
            '#' | ';' if !quoted => break,
            c => value.push(c),
        }
    }
    value.trim_end().to_owned()
}
