//! Names in the session list grouped by project.

use crate::session::Session;

/// A session's name under its project's heading, which names the repo already: see
/// [`Session::name`].
pub(super) fn session_name(session: &Session, compact: bool) -> String {
    session.name(compact)
}
