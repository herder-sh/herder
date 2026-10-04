//! Tokens and cost: what each turn used, and a daemon's totals over a period.
//!
//! Plan-limit windows are a different thing and stay in [`crate::UsageWindow`].

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Zoned};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AccountId, Provider, Timestamp};

/// Tokens and cost of one turn, as its provider reported them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TurnUsage {
    /// Input tokens, not counting those read from or written to the prompt cache.
    pub input: u64,
    /// Output tokens, reasoning included.
    pub output: u64,
    /// Input tokens read from the prompt cache.
    pub cache_read: u64,
    /// Input tokens written to the prompt cache.
    pub cache_write: u64,
    /// What the turn costs at the provider's API prices, in US dollars; absent when neither
    /// the provider nor herder's price table knows the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Whether `cost_usd` is herder's estimate from its price table rather than the
    /// provider's own figure.
    pub cost_estimated: bool,
}

/// A period to add usage up over, ending now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum UsagePeriod {
    /// The last 24 hours.
    #[serde(rename = "24h")]
    Day,
    /// The last 7 days.
    #[serde(rename = "7d")]
    Week,
    /// The last 30 days.
    #[serde(rename = "30d")]
    ThirtyDays,
    /// The current calendar month, in UTC, to date.
    #[serde(rename = "month")]
    Month,
}

impl UsagePeriod {
    /// When the period that ends at `now` starts.
    pub fn start(self, now: Timestamp) -> Timestamp {
        let back = |hours: i64| {
            now.checked_sub(SignedDuration::from_hours(hours))
                .unwrap_or(Timestamp::MIN)
        };
        match self {
            Self::Day => back(24),
            Self::Week => back(7 * 24),
            Self::ThirtyDays => back(30 * 24),
            Self::Month => {
                let today = Zoned::new(now, TimeZone::UTC).date();
                Date::new(today.year(), today.month(), 1)
                    .and_then(|first| first.to_zoned(TimeZone::UTC))
                    .map_or(Timestamp::MIN, |first| first.timestamp())
            }
        }
    }
}

/// What one account used with one model over a period, on one daemon.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UsageTotal {
    /// The account the turns ran on.
    pub account_id: AccountId,
    /// The account's provider.
    pub provider: Provider,
    /// The model, in the provider's own naming.
    pub model: String,
    /// Completed turns counted.
    pub turns: u64,
    /// Input tokens, as [`TurnUsage::input`] counts them.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Input tokens read from the prompt cache.
    pub cache_read: u64,
    /// Input tokens written to the prompt cache.
    pub cache_write: u64,
    /// The turns' known costs added up, in US dollars.
    pub cost_usd: f64,
    /// Whether some turn's cost was herder's estimate, or unknown and so left out of
    /// `cost_usd`.
    pub cost_estimated: bool,
}
