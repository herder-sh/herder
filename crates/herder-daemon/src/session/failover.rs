//! Reactive failover: which account a session moves to when its turn hits a usage limit.
//!
//! Only a turn that fails with `limit_reached` triggers it, never usage percentages alone. The
//! session then moves to the first eligible account ([`next`]) and retries the failed turn's
//! prompt there, once ([`super::actor`]). An account is eligible when it opted in
//! (`failover = true`), is not the failing one, has an adapter, and is not limited: no window of
//! its usage ([`crate::usage`]) is at 100% before it resets, and it has not hit a limit since
//! its reset time ([`Limits`]). Only accounts of the session's own provider qualify, most quota
//! left first, then by id; the session keeps its model, so a failover never changes provider or
//! model. A session pinned to its account (created with `failover_pin`, else by
//! [`FailoverConfig::pin`]) never fails over.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use herder_protocol::{AccountId, Provider, Timestamp, UsageWindow};
use serde::Deserialize;

use super::{Accounts, Adapters};
use crate::usage::Windows;

/// How long an account that hit its limit is passed over when its provider gives no reset time.
pub const UNKNOWN_RESET: Duration = Duration::from_secs(30 * 60);

/// How sessions fail over: the `[failover]` table.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FailoverConfig {
    /// Whether sessions stay on their account when it hits a limit.
    pub pin: bool,
}

impl FailoverConfig {
    /// The settings as clients see them.
    pub fn settings(&self) -> herder_protocol::FailoverSettings {
        herder_protocol::FailoverSettings { pin: self.pin }
    }
}

/// When each account that hit a limit may be chosen again.
#[derive(Debug, Default)]
pub(crate) struct Limits(Mutex<HashMap<AccountId, Timestamp>>);

impl Limits {
    /// `account_id` hit its limit at `now`: it is passed over until the latest reset of its
    /// windows at 100%, or for [`UNKNOWN_RESET`] when none says.
    pub(crate) fn hit(&self, account_id: &AccountId, windows: &[UsageWindow], now: Timestamp) {
        let until = windows
            .iter()
            .filter(|window| window.used_percent >= 100.0)
            .filter_map(|window| window.resets_at)
            .filter(|resets_at| *resets_at > now)
            .max()
            .unwrap_or_else(|| now + UNKNOWN_RESET);
        self.lock().insert(account_id.clone(), until);
    }

    /// Whether `account_id` hit a limit that has not reset by `now`.
    fn limited(&self, account_id: &AccountId, now: Timestamp) -> bool {
        self.lock()
            .get(account_id)
            .is_some_and(|until| *until > now)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<AccountId, Timestamp>> {
        // Every update is a single insert that leaves the map consistent, even mid-panic.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What failover picks from.
pub(crate) struct Choice<'a> {
    pub(crate) accounts: &'a Accounts,
    pub(crate) adapters: &'a Adapters,
    pub(crate) usage: &'a Windows,
    pub(crate) limits: &'a Limits,
    pub(crate) now: Timestamp,
}

/// The account a session of `provider` on `failing` moves to, if any is eligible.
pub(crate) fn next(
    choice: &Choice<'_>,
    provider: &Provider,
    failing: &AccountId,
) -> Option<AccountId> {
    choice.adapters.get(provider)?;
    choice
        .accounts
        .iter()
        .filter(|(id, account)| {
            account.provider == *provider
                && account.failover
                && *id != failing
                && !choice.limits.limited(id, choice.now)
        })
        .filter_map(|(id, _)| Some((left(choice.usage.get(id), choice.now)?, id)))
        // Most quota left first; ids break ties, as the map is ordered by id.
        .min_by(|(a, _), (b, _)| b.total_cmp(a))
        .map(|(_, id)| id.clone())
}

/// Quota left on an account with `windows`: its fullest window's share left, in percent, or
/// `None` when a window is used up and has not reset by `now`. Windows that reset are empty.
fn left(windows: Option<&Vec<UsageWindow>>, now: Timestamp) -> Option<f64> {
    let mut used: f64 = 0.0;
    for window in windows.into_iter().flatten() {
        if window.resets_at.is_some_and(|resets_at| resets_at <= now) {
            continue;
        }
        if window.used_percent >= 100.0 {
            return None;
        }
        used = used.max(window.used_percent);
    }
    Some(100.0 - used)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use herder_adapters::fake::FakeAdapter;

    use super::*;
    use crate::session::AccountConfig;

    fn at(time: &str) -> Timestamp {
        time.parse().unwrap()
    }

    fn window(used_percent: f64, resets_at: &str) -> UsageWindow {
        UsageWindow {
            window: "five_hour".into(),
            used_percent,
            resets_at: Some(at(resets_at)),
        }
    }

    fn accounts(entries: &[(&str, Provider, bool)]) -> Accounts {
        entries
            .iter()
            .map(|(id, provider, failover)| {
                let account = AccountConfig {
                    provider: provider.clone(),
                    label: id.to_string(),
                    config_dir: None,
                    failover: *failover,
                };
                (AccountId::new(*id), account)
            })
            .collect()
    }

    fn adapters(providers: &[Provider]) -> Adapters {
        let mut adapters = Adapters::new();
        for provider in providers {
            adapters.register(provider.clone(), Arc::new(FakeAdapter::new("/nowhere")));
        }
        adapters
    }

    struct Case {
        accounts: Accounts,
        adapters: Adapters,
        usage: Windows,
        limits: Limits,
        now: Timestamp,
    }

    impl Case {
        fn new(entries: &[(&str, Provider, bool)]) -> Self {
            Self {
                accounts: accounts(entries),
                adapters: adapters(&[Provider::Claude, Provider::Codex, Provider::Cursor]),
                usage: Windows::new(),
                limits: Limits::default(),
                now: at("2026-10-02T12:00:00Z"),
            }
        }

        fn next(&self, failing: &str) -> Option<String> {
            let choice = Choice {
                accounts: &self.accounts,
                adapters: &self.adapters,
                usage: &self.usage,
                limits: &self.limits,
                now: self.now,
            };
            let provider = &self.accounts[&AccountId::new(failing)].provider;
            next(&choice, provider, &AccountId::new(failing)).map(|id| id.to_string())
        }

        fn usage(&mut self, id: &str, windows: Vec<UsageWindow>) {
            self.usage.insert(AccountId::new(id), windows);
        }
    }

    #[test]
    fn the_same_providers_account_with_most_quota_left_comes_first() {
        let mut case = Case::new(&[
            ("a", Provider::Claude, true),
            ("b", Provider::Claude, true),
            ("c", Provider::Claude, true),
            ("d", Provider::Claude, false),
        ]);
        // Unknown usage counts as untouched; ties go by id.
        assert_eq!(case.next("a").as_deref(), Some("b"));
        case.usage("b", vec![window(80.0, "2026-10-02T15:00:00Z")]);
        case.usage("c", vec![window(10.0, "2026-10-02T15:00:00Z")]);
        assert_eq!(case.next("a").as_deref(), Some("c"));
        // A window used up excludes the account until it resets.
        case.usage("c", vec![window(100.0, "2026-10-02T15:00:00Z")]);
        assert_eq!(case.next("a").as_deref(), Some("b"));
        case.usage("c", vec![window(100.0, "2026-10-02T11:00:00Z")]);
        assert_eq!(case.next("a").as_deref(), Some("c"));
    }

    #[test]
    fn accounts_of_other_providers_are_never_chosen() {
        let mut case = Case::new(&[
            ("claude-a", Provider::Claude, true),
            ("claude-b", Provider::Claude, false),
            ("codex", Provider::Codex, true),
            ("cursor", Provider::Cursor, true),
        ]);
        assert_eq!(case.next("claude-a"), None);
        assert_eq!(case.next("codex"), None);
        // Without its provider's adapter, a session has nowhere to go.
        case.accounts = accounts(&[
            ("claude-a", Provider::Claude, true),
            ("claude-b", Provider::Claude, true),
        ]);
        case.adapters = adapters(&[Provider::Codex]);
        assert_eq!(case.next("claude-a"), None);
    }

    #[test]
    fn an_account_that_hit_its_limit_waits_for_its_reset() {
        let mut case = Case::new(&[
            ("a", Provider::Claude, true),
            ("b", Provider::Claude, true),
            ("c", Provider::Claude, true),
        ]);
        // The reset of its used-up window, as reported.
        let used_up = [window(100.0, "2026-10-02T14:00:00Z")];
        case.limits.hit(&AccountId::new("b"), &used_up, case.now);
        // No reset time known: thirty minutes.
        case.limits.hit(&AccountId::new("a"), &[], case.now);
        assert_eq!(case.next("c"), None);
        case.now = at("2026-10-02T12:31:00Z");
        assert_eq!(case.next("c").as_deref(), Some("a"));
        assert_eq!(case.next("a").as_deref(), Some("c"));
        case.now = at("2026-10-02T14:00:01Z");
        assert_eq!(case.next("a").as_deref(), Some("b"));
    }
}
