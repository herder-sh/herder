//! Account usage: each account's limit windows, as its provider last reported them.
//!
//! Two sources feed it, and every change goes to clients as a fresh account list
//! ([`crate::session::EventSink::accounts_changed`]):
//!
//! - Running sessions: their adapter's `usage_reported` events. Claude reports the five-hour
//!   and seven-day windows with every API response, Codex whenever its limits change.
//! - Probes, for accounts with no session running and for the windows sessions do not report
//!   (Claude's per-model weekly ones): every [`INTERVAL`], and on demand when a client
//!   connects, each account whose provider has a [`Probe`] is read by a short-lived CLI run
//!   under its config dir. Claude answers `get_usage`, Codex `account/rateLimits/read`; neither
//!   runs a turn. Probes run one at a time, and an on-demand one only for an account not read
//!   in the last [`FRESH`]. A probe that fails is retried at the next interval.
//!
//! A report replaces the windows it names and keeps the others, since a session's report covers
//! only some of them. Usage is not stored: a restarted daemon starts empty and probes at once.
//! herder sees only the numbers; the CLI reads its own login to get them.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use herder_adapters::StartRequest;
use herder_adapters::claude::ClaudeAdapter;
use herder_adapters::codex::CodexAdapter;
use herder_protocol::{AccountId, PermissionMode, Provider, TurnError, UsageWindow};
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::session::{AccountConfig, Accounts};

/// How often every account is probed.
pub const INTERVAL: Duration = Duration::from_secs(15 * 60);

/// How recent a probe must be for an on-demand refresh to skip the account.
pub const FRESH: Duration = Duration::from_secs(2 * 60);

/// Longest a probe may run before it is killed.
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

/// How often the poller looks for accounts due a probe.
const TICK: Duration = Duration::from_secs(60);

/// What a probe resolves to: the account's windows, empty when it has no plan limits.
pub type ProbeFuture = Pin<Box<dyn Future<Output = Result<Vec<UsageWindow>, TurnError>> + Send>>;

/// Reads an account's limit windows with a CLI run that exits once it has answered.
pub trait Probe: Send + Sync {
    /// Runs the CLI for `request`, which names the account's config dir.
    fn read(&self, request: StartRequest) -> ProbeFuture;
}

impl Probe for ClaudeAdapter {
    fn read(&self, request: StartRequest) -> ProbeFuture {
        Box::pin(self.read_usage(request))
    }
}

impl Probe for CodexAdapter {
    fn read(&self, request: StartRequest) -> ProbeFuture {
        Box::pin(self.read_usage(request))
    }
}

/// One probe per provider that has one.
pub type Probes = HashMap<Provider, Arc<dyn Probe>>;

/// How the poller runs.
pub struct Config {
    /// Probes by provider; accounts of any other provider are never probed.
    pub probes: Probes,
    /// Working directory of every probe, created if missing; nothing is written there.
    pub dir: PathBuf,
    /// How often every account is probed; [`INTERVAL`] outside tests.
    pub interval: Duration,
    /// How recent a probe must be for an on-demand refresh to skip the account; [`FRESH`]
    /// outside tests.
    pub fresh: Duration,
}

/// Every account's windows, by account.
pub(crate) type Windows = BTreeMap<AccountId, Vec<UsageWindow>>;

/// The windows last reported for each account.
#[derive(Debug, Default)]
pub(crate) struct Usage {
    windows: Mutex<Windows>,
}

impl Usage {
    /// Merges `windows` into the account's. When that changed anything, returns every
    /// account's windows still locked, so what is published from them goes out in the order
    /// the reports came in.
    pub(crate) fn report(
        &self,
        account_id: &AccountId,
        windows: Vec<UsageWindow>,
    ) -> Option<MutexGuard<'_, Windows>> {
        let mut all = self.all();
        let known = all.entry(account_id.clone()).or_default();
        let before = known.clone();
        for window in windows {
            match known.iter_mut().find(|known| known.window == window.window) {
                Some(known) => *known = window,
                None => known.push(window),
            }
        }
        (*known != before).then_some(all)
    }

    /// Every account's windows, each in the order first reported.
    pub(crate) fn all(&self) -> MutexGuard<'_, Windows> {
        // Every update is a single merge that leaves the map consistent, even mid-panic.
        self.windows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Probes `accounts` until `shutdown`: each one at once, then every [`Config::interval`], and
/// those not read within [`Config::fresh`] whenever `wake` is notified. Each answer goes to `report`.
pub(crate) async fn poll(
    config: Config,
    accounts: Accounts,
    wake: Arc<Notify>,
    report: impl Fn(&AccountId, Vec<UsageWindow>),
    shutdown: CancellationToken,
) {
    let mut probed: HashMap<AccountId, Instant> = HashMap::new();
    let mut max_age = config.interval;
    loop {
        for (account_id, account) in &accounts {
            let Some(probe) = config.probes.get(&account.provider) else {
                continue;
            };
            if probed
                .get(account_id)
                .is_some_and(|at| at.elapsed() < max_age)
            {
                continue;
            }
            probed.insert(account_id.clone(), Instant::now());
            let read = tokio::time::timeout(
                PROBE_TIMEOUT,
                probe.read(request(account, config.dir.clone())),
            );
            let result = tokio::select! {
                () = shutdown.cancelled() => return,
                result = read => result,
            };
            match result {
                Ok(Ok(windows)) if windows.is_empty() => {
                    debug!(%account_id, "the account reports no limit windows");
                }
                Ok(Ok(windows)) => report(account_id, windows),
                Ok(Err(error)) => warn!(%account_id, "cannot read usage: {}", error.message),
                Err(_) => warn!(%account_id, "reading usage timed out"),
            }
        }
        max_age = tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(TICK.min(config.interval)) => config.interval,
            () = wake.notified() => config.fresh,
        };
    }
}

/// A probe's run for `account`, in `dir`, with the daemon's environment like a session's.
fn request(account: &AccountConfig, dir: PathBuf) -> StartRequest {
    StartRequest {
        config_dir: account.config_dir.clone(),
        env: std::env::vars().collect(),
        cwd: dir,
        model: None,
        permission_mode: PermissionMode::ReadOnly,
        seed: Vec::new(),
        mcp: None,
        launcher: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use herder_protocol::Timestamp;

    use super::*;

    fn window(name: &str, used_percent: f64) -> UsageWindow {
        UsageWindow {
            window: name.into(),
            used_percent,
            resets_at: Some(Timestamp::from_second(1_790_953_200).unwrap()),
        }
    }

    #[test]
    fn a_report_replaces_the_windows_it_names_and_keeps_the_rest() {
        let usage = Usage::default();
        let id = AccountId::new("claude");
        let first = vec![window("five_hour", 9.0), window("seven_day_fable", 1.0)];
        assert!(usage.report(&id, first).is_some());
        let second = vec![window("five_hour", 12.0), window("seven_day", 3.0)];
        assert!(usage.report(&id, second).is_some());
        assert_eq!(
            usage.all()[&id],
            [
                window("five_hour", 12.0),
                window("seven_day_fable", 1.0),
                window("seven_day", 3.0)
            ]
        );
        // The same numbers again change nothing, so nothing is published.
        assert!(usage.report(&id, vec![window("seven_day", 3.0)]).is_none());
        assert!(!usage.all().contains_key(&AccountId::new("other")));
    }
}
