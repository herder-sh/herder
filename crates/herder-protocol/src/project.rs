//! Projects: a repository however many clones of it the hosts hold.
//!
//! A project's identity is its `origin` remote URL, normalised by [`ProjectId::from_remote`]
//! so every clone of a repository, on any host and by any URL scheme, gets the same id. A
//! repository without a remote is a local project of the one host that has it, identified
//! by [`ProjectId::local`]. Daemon config can rename a project, declare one by path, add
//! paths to it or merge several remotes into one; the daemon resolves all of that and sends
//! the result. Clients merge the project lists of every paired daemon by id.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AccountId, HostId, PermissionMode};

/// Media types a project icon may have.
pub const PROJECT_ICON_MEDIA_TYPES: [&str; 4] =
    ["image/png", "image/svg+xml", "image/x-icon", "image/jpeg"];

/// Most bytes a project icon may have; larger files are not taken as icons, and larger
/// uploads are refused.
pub const MAX_PROJECT_ICON_BYTES: usize = 512 * 1024;

/// Identifies a project across hosts.
///
/// From a remote, it is the host and repository path, as in `github.com/org/repo`. For a
/// repository without one, it is the host id and absolute path, as in
/// `01J9HOST:/home/dev/scratch`.
#[derive(
    Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct ProjectId(String);

impl ProjectId {
    /// Wraps an existing identifier string.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The identifier as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The id of the repository at a remote URL, or `None` when the URL names no remote
    /// host, such as a local path or a `file://` URL.
    ///
    /// Accepts `scheme://[user@]host[:port]/path` and scp-like `[user@]host:path`. The user,
    /// the port, a trailing `.git` and trailing slashes are dropped, the host is lowercased
    /// and repeated slashes collapse, so `git@github.com:org/repo.git` and
    /// `https://github.com/org/repo` both give `github.com/org/repo`. The path keeps its case.
    pub fn from_remote(url: &str) -> Option<Self> {
        let url = url.trim();
        let (authority, path) = match url.split_once("://") {
            Some((scheme, _)) if scheme.eq_ignore_ascii_case("file") => return None,
            Some((_, rest)) => rest.split_once('/').unwrap_or((rest, "")),
            // Like git, a colon before any slash makes it scp-like; anything else is a path.
            None => match url.split_once(':') {
                Some((authority, path)) if !authority.contains('/') => (authority, path),
                _ => return None,
            },
        };
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        let host = match host.rsplit_once(':') {
            Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => name,
            _ => host,
        };
        let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let mut path = segments.join("/");
        if let Some(stripped) = path.strip_suffix(".git") {
            path = stripped.trim_end_matches('/').to_owned();
        }
        if host.is_empty() || path.is_empty() {
            return None;
        }
        Some(Self(format!("{}/{path}", host.to_ascii_lowercase())))
    }

    /// The id of a repository without a remote: its host and its absolute path on the host.
    pub fn local(host_id: &HostId, path: &str) -> Self {
        Self(format!("{host_id}:{path}"))
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A project as one daemon sees it: its clones on that daemon's host and its settings there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Project {
    /// The project.
    pub project_id: ProjectId,
    /// Display name: the configured name, else the last segment of the id.
    pub name: String,
    /// Absolute paths of the project's clones on this host.
    pub paths: Vec<String>,
    /// Permission mode new sessions of the project start in on this host when none is chosen;
    /// absent when none is configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_permission_mode: Option<PermissionMode>,
    /// Account new sessions of the project use on this host when none is chosen; absent when
    /// none is configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_account: Option<AccountId>,
    /// Shell command run in each new worktree of the project before its session starts;
    /// absent when none is configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_command: Option<String>,
    /// The project's icon, the image an owner uploaded with `set_project_icon`, else an image
    /// file found in its clone on this host, named by the SHA-256 of its bytes as lowercase
    /// hex; absent when it has none. It changes whenever the image does, so clients cache the
    /// icon by it and fetch it with `get_project_icon`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Whether `icon` is an uploaded image rather than one found in the clone;
    /// `set_project_icon` without an image clears the upload.
    #[serde(default)]
    pub icon_uploaded: bool,
    /// Colour drawn behind the project's icon, as `#rrggbb`; absent when none is configured,
    /// so a transparent icon shows through to the client's own background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_background: Option<String>,
}
