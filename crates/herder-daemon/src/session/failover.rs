//! Reactive failover: which account a session rotates to when its turn hits a usage limit.
//!
//! Only a turn that fails with `limit_reached` triggers it, never usage percentages alone. The
//! session then rotates to the best available account ([`best`]) and retries the failed turn's
//! prompt there, once ([`super::actor`]). Every account takes part; none opts in. An account is
//! available when it is of the session's own provider, is not the failing one, has an adapter,
//! and is not limited: no window of its usage ([`crate::usage`]) is at 100% before it resets,
//! it has not hit a limit since its reset time, and it has not failed to log in since it last
//! worked ([`Limits`]). Accounts whose usage is known come before those whose usage is not,
//! which may be used up or logged out without herder knowing. Among them the one with the most
//! quota left is best, then by id; the session keeps its model, so a failover never changes provider
//! or model. A session pinned to its account (created with `failover_pin`, else by
//! [`FailoverConfig::pin`]) never fails over.
//!
//! A `create_session` naming a provider instead of an account starts on its best available
//! account the same way.

use std::collections::{HashMap, HashSet};
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

/// Accounts failover passes over: those that hit a limit, until they may be chosen again, and
/// those whose CLI is logged out.
#[derive(Debug, Default)]
pub(crate) struct Limits {
    until: Mutex<HashMap<AccountId, Timestamp>>,
    logged_out: Mutex<HashSet<AccountId>>,
}

impl Limits {
    /// `account_id` hit its limit at `now`: it is passed over until the latest reset of its
    /// windows at 100%, or for [`UNKNOWN_RESET`] when none says. A hit only ever extends the
    /// wait: one without a reset time must not cut short a known later one, such as a weekly
    /// window's.
    pub(crate) fn hit(&self, account_id: &AccountId, windows: &[UsageWindow], now: Timestamp) {
        let until = windows
            .iter()
            .filter(|window| window.used_percent >= 100.0)
            .filter_map(|window| window.resets_at)
            .filter(|resets_at| *resets_at > now)
            .max()
            .unwrap_or_else(|| now + UNKNOWN_RESET);
        let mut limits = lock(&self.until);
        let entry = limits.entry(account_id.clone()).or_insert(until);
        *entry = (*entry).max(until);
    }

    /// `account_id` failed to log in: it is passed over until it works again.
    pub(crate) fn logged_out(&self, account_id: &AccountId) {
        lock(&self.logged_out).insert(account_id.clone());
    }

    /// `account_id` worked: a turn on it completed, or it reported its usage.
    pub(crate) fn worked(&self, account_id: &AccountId) {
        lock(&self.logged_out).remove(account_id);
    }

    /// Whether `account_id` is logged out, or hit a limit that has not reset by `now`.
    fn limited(&self, account_id: &AccountId, now: Timestamp) -> bool {
        lock(&self.logged_out).contains(account_id)
            || lock(&self.until)
                .get(account_id)
                .is_some_and(|until| *until > now)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // Every update is a single insert or remove that leaves it consistent, even mid-panic.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What an account is picked from.
pub(crate) struct Choice<'a> {
    pub(crate) accounts: &'a Accounts,
    pub(crate) adapters: &'a Adapters,
    pub(crate) usage: &'a Windows,
    pub(crate) limits: &'a Limits,
    pub(crate) now: Timestamp,
}

/// The available account of `provider` with known usage and the most quota left, other than
/// `except`, if any is available.
pub(crate) fn best(
    choice: &Choice<'_>,
    provider: &Provider,
    except: Option<&AccountId>,
) -> Option<AccountId> {
    choice.adapters.get(provider)?;
    choice
        .accounts
        .iter()
        .filter(|(id, account)| {
            account.provider == *provider
                && Some(*id) != except
                && !choice.limits.limited(id, choice.now)
        })
        .filter_map(|(id, _)| {
            let usage = choice.usage.get(id);
            Some((usage.is_none(), left(usage, choice.now)?, id))
        })
        // Known usage first, then most quota left; ids break ties, as the map is ordered by id.
        .min_by(|(a_unknown, a, _), (b_unknown, b, _)| {
            a_unknown.cmp(b_unknown).then(b.total_cmp(a))
        })
        .map(|(_, _, id)| id.clone())
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

    fn accounts(entries: &[(&str, Provider)]) -> Accounts {
        entries
            .iter()
            .map(|(id, provider)| {
                let account = AccountConfig {
                    provider: provider.clone(),
                    label: id.to_string(),
                    config_dir: None,
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
        fn new(entries: &[(&str, Provider)]) -> Self {
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
            best(&choice, provider, Some(&AccountId::new(failing))).map(|id| id.to_string())
        }

        fn any(&self, provider: Provider) -> Option<String> {
            let choice = Choice {
                accounts: &self.accounts,
                adapters: &self.adapters,
                usage: &self.usage,
                limits: &self.limits,
                now: self.now,
            };
            best(&choice, &provider, None).map(|id| id.to_string())
        }

        fn usage(&mut self, id: &str, windows: Vec<UsageWindow>) {
            self.usage.insert(AccountId::new(id), windows);
        }
    }

    #[test]
    fn the_same_providers_account_with_most_quota_left_comes_first() {
        let mut case = Case::new(&[
            ("a", Provider::Claude),
            ("b", Provider::Claude),
            ("c", Provider::Claude),
            ("d", Provider::Claude),
        ]);
        // Unknown usage counts as untouched; ties go by id.
        assert_eq!(case.next("a").as_deref(), Some("b"));
        assert_eq!(case.any(Provider::Claude).as_deref(), Some("a"));
        case.usage("a", vec![window(50.0, "2026-10-02T15:00:00Z")]);
        case.usage("b", vec![window(80.0, "2026-10-02T15:00:00Z")]);
        case.usage("c", vec![window(10.0, "2026-10-02T15:00:00Z")]);
        case.usage("d", vec![window(30.0, "2026-10-02T15:00:00Z")]);
        assert_eq!(case.next("a").as_deref(), Some("c"));
        // Every account takes part, without opting in.
        assert_eq!(case.next("c").as_deref(), Some("d"));
        assert_eq!(case.any(Provider::Claude).as_deref(), Some("c"));
        // A window used up excludes the account until it resets.
        case.usage("c", vec![window(100.0, "2026-10-02T15:00:00Z")]);
        assert_eq!(case.next("a").as_deref(), Some("d"));
        case.usage("c", vec![window(100.0, "2026-10-02T11:00:00Z")]);
        assert_eq!(case.next("a").as_deref(), Some("c"));
    }

    #[test]
    fn accounts_of_other_providers_are_never_chosen() {
        let mut case = Case::new(&[
            ("claude-a", Provider::Claude),
            ("codex", Provider::Codex),
            ("cursor", Provider::Cursor),
        ]);
        assert_eq!(case.next("claude-a"), None);
        assert_eq!(case.next("codex"), None);
        assert_eq!(case.any(Provider::Grok), None);
        // Without its provider's adapter, a session has nowhere to go.
        case.accounts = accounts(&[
            ("claude-a", Provider::Claude),
            ("claude-b", Provider::Claude),
        ]);
        case.adapters = adapters(&[Provider::Codex]);
        assert_eq!(case.next("claude-a"), None);
        assert_eq!(case.any(Provider::Claude), None);
    }

    #[test]
    fn an_account_that_hit_its_limit_waits_for_its_reset() {
        let mut case = Case::new(&[
            ("a", Provider::Claude),
            ("b", Provider::Claude),
            ("c", Provider::Claude),
        ]);
        // The reset of its used-up window, as reported.
        let used_up = [window(100.0, "2026-10-02T14:00:00Z")];
        case.limits.hit(&AccountId::new("b"), &used_up, case.now);
        // No reset time known: thirty minutes.
        case.limits.hit(&AccountId::new("a"), &[], case.now);
        assert_eq!(case.next("c"), None);
        assert_eq!(case.any(Provider::Claude).as_deref(), Some("c"));
        case.now = at("2026-10-02T12:31:00Z");
        assert_eq!(case.next("c").as_deref(), Some("a"));
        assert_eq!(case.next("a").as_deref(), Some("c"));
        case.now = at("2026-10-02T14:00:01Z");
        assert_eq!(case.next("a").as_deref(), Some("b"));
    }

    #[test]
    fn accounts_of_known_usage_come_before_unknown_ones() {
        let mut case = Case::new(&[
            ("a", Provider::Claude),
            ("b", Provider::Claude),
            ("c", Provider::Claude),
        ]);
        // "a" was never read, perhaps because its probe cannot log in; "b" has room left.
        case.usage("b", vec![window(90.0, "2026-10-02T15:00:00Z")]);
        assert_eq!(case.next("c").as_deref(), Some("b"));
        assert_eq!(case.any(Provider::Claude).as_deref(), Some("b"));
        // Unknown ones still take over when no known one is available.
        case.usage("b", vec![window(100.0, "2026-10-02T15:00:00Z")]);
        assert_eq!(case.next("c").as_deref(), Some("a"));
    }

    #[test]
    fn a_logged_out_account_waits_until_it_works_again() {
        let case = Case::new(&[("a", Provider::Claude), ("b", Provider::Claude)]);
        case.limits.logged_out(&AccountId::new("b"));
        assert_eq!(case.next("a"), None);
        assert_eq!(case.any(Provider::Claude).as_deref(), Some("a"));
        case.limits.worked(&AccountId::new("b"));
        assert_eq!(case.next("a").as_deref(), Some("b"));
    }

    #[test]
    fn a_later_hit_never_shortens_the_wait() {
        let mut case = Case::new(&[("a", Provider::Claude), ("b", Provider::Claude)]);
        let weekly = [window(100.0, "2026-10-08T12:00:00Z")];
        case.limits.hit(&AccountId::new("b"), &weekly, case.now);
        // A hit without a reset time keeps the weekly wait, not thirty minutes.
        case.now = at("2026-10-02T13:00:00Z");
        case.limits.hit(&AccountId::new("b"), &[], case.now);
        case.now = at("2026-10-02T14:00:00Z");
        assert_eq!(case.next("a"), None);
        case.now = at("2026-10-08T12:00:01Z");
        assert_eq!(case.next("a").as_deref(), Some("b"));
    }
}
