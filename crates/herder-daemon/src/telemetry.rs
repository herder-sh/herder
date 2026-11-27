//! Whether the daemon shares diagnostics: the `[telemetry]` table, applied as the daemon
//! starts and at once whenever an owner changes it ([`crate::settings`]). Off unless turned
//! on; off sends nothing at all.
//!
//! The daemon shares them through the client core's process-wide switch
//! ([`herder_client_core::set_diagnostics`]), with a random install id kept in its data dir
//! and the keys its build was made with: the Sentry DSN, unless `[telemetry] dsn` names
//! another, and the PostHog project token.

use std::path::Path;

use herder_client_core::{Diagnostics, set_diagnostics};
use herder_protocol::TelemetrySettings;
use tracing::{info, warn};

/// The file in the data dir holding the daemon's install id.
const INSTALL_ID_FILE: &str = "telemetry-id";

/// The Sentry DSN this build was made with, if any.
const BUILD_SENTRY_DSN: Option<&str> = option_env!("HERDER_DAEMON_SENTRY_DSN");

/// The PostHog project token this build was made with, if any.
const BUILD_POSTHOG_TOKEN: Option<&str> = option_env!("HERDER_POSTHOG_TOKEN");

/// Turns the daemon's sharing on or off as `settings` say.
pub(crate) fn apply(data_dir: &Path, settings: &TelemetrySettings) {
    let diagnostics = diagnostics(data_dir, settings);
    match &diagnostics {
        Some(_) => info!("the daemon shares diagnostics, as [telemetry] says"),
        None => info!("the daemon shares no diagnostics"),
    }
    set_diagnostics(diagnostics);
}

/// What the daemon shares with, while `settings` turn it on.
fn diagnostics(data_dir: &Path, settings: &TelemetrySettings) -> Option<Diagnostics> {
    if !settings.enabled {
        return None;
    }
    let install_id = match install_id(data_dir) {
        Ok(id) => id,
        Err(err) => {
            // Without an id of its own the daemon would look new on every start.
            warn!("shares no diagnostics: cannot keep an install id: {err:#}");
            return None;
        }
    };
    let set = |value: Option<&str>| value.filter(|value| !value.is_empty()).map(str::to_owned);
    Some(Diagnostics {
        install_id,
        sentry_dsn: set(settings.dsn.as_deref()).or_else(|| set(BUILD_SENTRY_DSN)),
        posthog_token: set(BUILD_POSTHOG_TOKEN),
    })
}

/// The daemon's random install id, made the first time it shares diagnostics.
fn install_id(data_dir: &Path) -> anyhow::Result<String> {
    let path = data_dir.join(INSTALL_ID_FILE);
    match std::fs::read_to_string(&path) {
        Ok(id) if uuid::Uuid::parse_str(id.trim()).is_ok() => return Ok(id.trim().to_owned()),
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    std::fs::create_dir_all(data_dir)?;
    let id = uuid::Uuid::new_v4().to_string();
    std::fs::write(&path, &id)?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_shares_nothing_and_makes_no_install_id() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(diagnostics(tmp.path(), &TelemetrySettings::default()), None);
        let named = TelemetrySettings {
            enabled: false,
            dsn: Some("https://k@o1.ingest.de.sentry.io/2".into()),
        };
        assert_eq!(diagnostics(tmp.path(), &named), None);
        assert!(!tmp.path().join(INSTALL_ID_FILE).exists());
    }

    #[test]
    fn on_shares_with_one_install_id_kept_across_starts() {
        let tmp = tempfile::tempdir().unwrap();
        let on = TelemetrySettings {
            enabled: true,
            dsn: Some("https://k@o1.ingest.de.sentry.io/2".into()),
        };
        let first = diagnostics(tmp.path(), &on).unwrap();
        assert_eq!(
            first.sentry_dsn.as_deref(),
            Some("https://k@o1.ingest.de.sentry.io/2")
        );
        assert!(uuid::Uuid::parse_str(&first.install_id).is_ok());
        let again = diagnostics(tmp.path(), &on).unwrap();
        assert_eq!(again.install_id, first.install_id);
    }

    #[test]
    fn an_empty_dsn_falls_back_to_the_builds_own() {
        let tmp = tempfile::tempdir().unwrap();
        let on = TelemetrySettings {
            enabled: true,
            dsn: Some(String::new()),
        };
        let shared = diagnostics(tmp.path(), &on).unwrap();
        assert_eq!(
            shared.sentry_dsn,
            BUILD_SENTRY_DSN
                .filter(|dsn| !dsn.is_empty())
                .map(str::to_owned)
        );
    }
}
