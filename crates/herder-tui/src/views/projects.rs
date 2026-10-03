//! Names in the session list grouped by project.

use crate::session::Session;

/// A session's name under its project's heading, which names the repo already: its branch,
/// or with `compact` the branch's last part; a child's task, as everywhere.
pub(super) fn session_name(session: &Session, compact: bool) -> String {
    if session.task.is_some() || !session.loaded {
        return session.title();
    }
    let branch = session.branch.as_str();
    match branch.rsplit('/').next() {
        Some(last) if compact => last.to_owned(),
        _ => branch.to_owned(),
    }
}
