//! Whether this process shares diagnostics, as the app's one switch sets it.

use std::sync::RwLock;

/// What an app that shares diagnostics gives the client core, with [`set_diagnostics`]: its
/// random install id and the write-only client keys its build was made with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostics {
    /// A random id the app made once for this install, and names nothing else: what
    /// PostHog counts as one install. A new one makes the install look new.
    pub install_id: String,
    /// The Sentry DSN traces and errors go to; none goes to Sentry without one.
    pub sentry_dsn: Option<String>,
    /// The PostHog project token measurements go to; none goes to PostHog without one.
    pub posthog_token: Option<String>,
}

static DIAGNOSTICS: RwLock<Option<Diagnostics>> = RwLock::new(None);

/// Turns sharing diagnostics on, with `Some`, or off, with `None`, for the whole process:
/// every [`crate::Client`] in it. It is off until an app turns it on; turning it off stops
/// sending at once, dropping whatever was not sent yet.
pub fn set_diagnostics(diagnostics: Option<Diagnostics>) {
    // A writer that panicked left a whole value behind, as an assignment cannot tear.
    *DIAGNOSTICS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = diagnostics;
}

/// The diagnostics in effect, if the process shares them.
pub(crate) fn diagnostics() -> Option<Diagnostics> {
    DIAGNOSTICS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// The trace context a command carries: the app's, while the process shares diagnostics, and
/// none otherwise.
pub(crate) fn shared_trace(trace: String) -> Option<String> {
    diagnostics().map(|_| trace)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRACE: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

    // One test, as the switch is the whole process's.
    #[test]
    fn diagnostics_are_off_until_set_and_off_again_once_cleared() {
        assert_eq!(diagnostics(), None);
        assert_eq!(shared_trace(TRACE.to_owned()), None);
        let on = Diagnostics {
            install_id: "0b7f1c2e-4d4b-4e0e-9a51-3f6f1b2a9c10".to_owned(),
            sentry_dsn: None,
            posthog_token: None,
        };
        set_diagnostics(Some(on.clone()));
        assert_eq!(diagnostics(), Some(on));
        assert_eq!(shared_trace(TRACE.to_owned()).as_deref(), Some(TRACE));
        set_diagnostics(None);
        assert_eq!(diagnostics(), None);
        assert_eq!(shared_trace(TRACE.to_owned()), None);
    }
}
