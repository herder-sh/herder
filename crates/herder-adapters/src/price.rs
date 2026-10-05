//! herder's built-in API price table, for turns whose CLI reports tokens but no cost.
//!
//! Prices are the providers' list prices for their API, in US dollars per million tokens, as
//! published when this release was cut; the table is updated with releases and has no user
//! overrides. A model it does not know gets no cost: its tokens still count, its cost does not.
//!
//! Models are matched on their bare id: a `provider/` prefix (OpenCode's `anthropic/...`), a
//! `[1m]`-style context suffix and a trailing `-YYYYMMDD` snapshot date are dropped first, so
//! `anthropic/claude-haiku-4-5-20251001` prices as `claude-haiku-4-5`.

use herder_protocol::TurnUsage;

/// What a model's tokens cost, in US dollars per million.
struct Price {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

/// A Claude model: cache writes cost 1.25× input (the 5-minute cache).
const fn claude(input: f64, output: f64, cache_read: f64) -> Price {
    Price {
        input,
        output,
        cache_read,
        cache_write: input * 1.25,
    }
}

/// An OpenAI model: cached input costs a tenth of input, and there is no cache-write charge.
const fn openai(input: f64, output: f64) -> Price {
    Price {
        input,
        output,
        cache_read: input / 10.0,
        cache_write: 0.0,
    }
}

const PRICES: &[(&str, Price)] = &[
    ("claude-fable-5-1", claude(10.0, 50.0, 0.25)),
    ("claude-fable-5", claude(10.0, 50.0, 1.0)),
    ("claude-opus-5-5", claude(4.0, 20.0, 0.2)),
    ("claude-opus-5", claude(5.0, 25.0, 0.5)),
    ("claude-opus-4-8", claude(5.0, 25.0, 0.5)),
    ("claude-opus-4-7", claude(5.0, 25.0, 0.5)),
    ("claude-opus-4-6", claude(5.0, 25.0, 0.5)),
    ("claude-opus-4-5", claude(5.0, 25.0, 0.5)),
    ("claude-opus-4-1", claude(15.0, 75.0, 1.5)),
    ("claude-opus-4", claude(15.0, 75.0, 1.5)),
    ("claude-sonnet-5-5", claude(2.0, 10.0, 0.2)),
    ("claude-sonnet-5", claude(2.0, 10.0, 0.2)),
    ("claude-sonnet-4-6", claude(3.0, 15.0, 0.3)),
    ("claude-sonnet-4-5", claude(3.0, 15.0, 0.3)),
    ("claude-sonnet-4", claude(3.0, 15.0, 0.3)),
    ("claude-haiku-4-5", claude(1.0, 5.0, 0.1)),
    ("gpt-5", openai(1.25, 10.0)),
    ("gpt-5-codex", openai(1.25, 10.0)),
    ("gpt-5.1", openai(1.25, 10.0)),
    ("gpt-5.1-codex", openai(1.25, 10.0)),
    ("gpt-5-mini", openai(0.25, 2.0)),
    ("gpt-5.1-codex-mini", openai(0.25, 2.0)),
];

/// What `usage` costs on `model` at its API prices; `None` for a model the table lacks.
pub fn estimate(model: &str, usage: &TurnUsage) -> Option<f64> {
    let model = bare(model);
    let (_, price) = PRICES.iter().find(|(id, _)| *id == model)?;
    let cost = usage.input as f64 * price.input
        + usage.output as f64 * price.output
        + usage.cache_read as f64 * price.cache_read
        + usage.cache_write as f64 * price.cache_write;
    Some(cost / 1_000_000.0)
}

/// `usage` with its cost estimated from the table, when the CLI gave none.
pub(crate) fn fill(mut usage: TurnUsage, model: Option<&str>) -> TurnUsage {
    if usage.cost_usd.is_none() {
        usage.cost_usd = model.and_then(|model| estimate(model, &usage));
        usage.cost_estimated = usage.cost_usd.is_some();
    }
    usage
}

/// What was spent since `spent`, given the running `total` a CLI reports; `spent` moves on to
/// `total`. A total that went down, as after the CLI restarted its count, counts as nothing.
pub(crate) fn spent_since(spent: &mut f64, total: f64) -> f64 {
    let since = (total - *spent).max(0.0);
    *spent = total;
    since
}

/// `model` without a provider prefix, context suffix or snapshot date.
fn bare(model: &str) -> &str {
    let model = model.rsplit_once('/').map_or(model, |(_, id)| id);
    let model = model.split_once('[').map_or(model, |(id, _)| id);
    match model.rsplit_once('-') {
        Some((id, date)) if date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) => id,
        _ => model,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage() -> TurnUsage {
        TurnUsage {
            input: 1_000_000,
            output: 100_000,
            cache_read: 2_000_000,
            cache_write: 400_000,
            ..TurnUsage::default()
        }
    }

    #[test]
    fn a_known_model_is_priced_per_token_kind() {
        // 1M × $1 + 0.1M × $5 + 2M × $0.10 + 0.4M × $1.25.
        let cost = estimate("claude-haiku-4-5", &usage()).unwrap();
        assert!((cost - 2.2).abs() < 1e-9, "{cost}");
    }

    #[test]
    fn prefixes_suffixes_and_snapshot_dates_are_ignored() {
        let plain = estimate("claude-sonnet-4-5", &usage());
        assert!(plain.is_some());
        for model in [
            "claude-sonnet-4-5-20250929",
            "anthropic/claude-sonnet-4-5",
            "claude-sonnet-4-5[1m]",
        ] {
            assert_eq!(estimate(model, &usage()), plain, "{model}");
        }
        // A version is not a date, so `gpt-5.1` is not `gpt-5`.
        assert_ne!(
            estimate("gpt-5-mini", &usage()),
            estimate("gpt-5", &usage())
        );
    }

    #[test]
    fn fill_estimates_a_missing_cost() {
        let filled = fill(usage(), Some("claude-haiku-4-5"));
        assert!(filled.cost_estimated);
        assert!((filled.cost_usd.unwrap() - 2.2).abs() < 1e-9);
    }

    #[test]
    fn an_unknown_model_keeps_its_tokens_and_gets_no_cost() {
        for model in [Some("gpt-6-luna"), None] {
            let filled = fill(usage(), model);
            assert_eq!(filled, usage(), "{model:?}");
        }
    }

    #[test]
    fn spent_since_takes_the_difference_of_running_totals() {
        let mut spent = 0.0;
        assert_eq!(spent_since(&mut spent, 0.25), 0.25);
        assert_eq!(spent_since(&mut spent, 0.75), 0.5);
        assert_eq!(spent_since(&mut spent, 0.5), 0.0);
        assert_eq!(spent, 0.5);
    }

    #[test]
    fn fill_keeps_the_cli_s_own_cost() {
        let reported = TurnUsage {
            cost_usd: Some(0.5),
            ..usage()
        };
        assert_eq!(fill(reported.clone(), Some("claude-haiku-4-5")), reported);
    }
}
