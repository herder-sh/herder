//! Whether this process shares diagnostics, as the app's one switch sets it: through
//! `herder-telemetry`, which sends only while it is on and only what it scrubs.

use herder_telemetry::{Component, Telemetry};

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

/// Turns sharing diagnostics on, with `Some`, or off, with `None`, for the whole process:
/// every [`crate::Client`] in it. It is off until an app turns it on; turning it off stops
/// sending at once, dropping whatever was not sent yet.
pub fn set_diagnostics(diagnostics: Option<Diagnostics>) {
    herder_telemetry::set(diagnostics.map(|diagnostics| Telemetry {
        install_id: diagnostics.install_id,
        sentry_dsn: diagnostics.sentry_dsn,
        posthog_token: diagnostics.posthog_token,
        component: Component::App,
    }));
}

/// The span a command is sent in: continuing the app's trace, when it gave one.
pub(crate) fn command_span(
    command: &herder_protocol::CommandBody,
    app: Option<&str>,
) -> tracing::Span {
    let command = herder_telemetry::variant(command);
    let parent = app
        .and_then(herder_telemetry::sentry_trace)
        .unwrap_or_default();
    tracing::info_span!("command", command, sentry.trace = parent.as_str())
}
