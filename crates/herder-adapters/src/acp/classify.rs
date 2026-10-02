//! Classifying an ACP agent's JSON-RPC errors into [`ErrorClass`]es.
//!
//! ACP has one dedicated code, `-32000` (authentication required); everything else, usage limits
//! included, arrives as an internal error whose message is the provider's. So beyond the code
//! this matches message text, conservatively: `limit_reached` may trigger failover, so only
//! wording that names an account's usage limit, credits or quota counts, and a plain rate limit
//! is `transient`. What matches nothing is `fatal`.
//!
//! Known wording, by agent:
//! - Grok 1.0.46 (strings in its binary, not seen over ACP): "You hit your free usage limit.",
//!   "You hit your weekly limit.", "You've hit the rate limit for your plan.", "out of credits",
//!   "spending limit", "usage balance exhausted".
//! - Cursor (forum reports, not seen over ACP): "You've hit your usage limit".
//! - OpenCode passes its model provider's message through as an internal error: for example
//!   "You exceeded your current quota" (OpenAI), "usage limit reached" (Anthropic plans).

use herder_protocol::ErrorClass;

/// ACP's "authentication required" error code.
const AUTH_REQUIRED: i32 = -32000;

/// Wording of an exhausted account: usage limits, credits, quota.
const LIMIT: &[&str] = &[
    "usage limit",
    "weekly limit",
    "monthly limit",
    "daily limit",
    "limit for your plan",
    "out of credits",
    "insufficient credits",
    "credit balance is too low",
    "spending limit",
    "usage balance exhausted",
    "insufficient balance",
    "exceeded your current quota",
    "insufficient_quota",
    "payment required",
];

/// Wording of a missing or expired login.
const AUTH: &[&str] = &[
    "authentication required",
    "not authenticated",
    "unauthorized",
    "unauthenticated",
    "not logged in",
    "login required",
    "invalid api key",
    "invalid x-api-key",
    "token expired",
    "please log in",
];

/// Wording of a failure that may pass: rate limits, overload, network.
const TRANSIENT: &[&str] = &[
    "rate limit",
    "rate_limit",
    "too many requests",
    "overloaded",
    "timed out",
    "timeout",
    "temporarily unavailable",
    "service unavailable",
    "bad gateway",
    "connection reset",
    "connection refused",
    "network error",
    "econnreset",
];

/// The class of an agent error with JSON-RPC `code` and the text of its message and data.
pub(super) fn classify(code: i32, text: &str) -> ErrorClass {
    let text = text.to_lowercase();
    let any = |phrases: &[&str]| phrases.iter().any(|phrase| text.contains(phrase));
    if code == AUTH_REQUIRED {
        ErrorClass::Auth
    } else if any(LIMIT) {
        ErrorClass::LimitReached
    } else if any(AUTH) {
        ErrorClass::Auth
    } else if any(TRANSIENT) {
        ErrorClass::Transient
    } else {
        ErrorClass::Fatal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTERNAL: i32 = -32603;

    #[test]
    fn known_wording_is_classified() {
        let cases = [
            (AUTH_REQUIRED, "Authentication required", ErrorClass::Auth),
            (
                INTERNAL,
                "You hit your weekly limit.",
                ErrorClass::LimitReached,
            ),
            (
                INTERNAL,
                "You hit your free usage limit.",
                ErrorClass::LimitReached,
            ),
            (
                INTERNAL,
                "You've hit the rate limit for your plan.",
                ErrorClass::LimitReached,
            ),
            (
                INTERNAL,
                "You've hit your usage limit",
                ErrorClass::LimitReached,
            ),
            (
                INTERNAL,
                "You exceeded your current quota, please check your plan",
                ErrorClass::LimitReached,
            ),
            (INTERNAL, "401 Unauthorized", ErrorClass::Auth),
            (
                INTERNAL,
                "Rate limit exceeded, retry in 2s",
                ErrorClass::Transient,
            ),
            (INTERNAL, "Overloaded", ErrorClass::Transient),
            (-32602, "Invalid params: model not found", ErrorClass::Fatal),
        ];
        for (code, message, class) in cases {
            assert_eq!(classify(code, message), class, "{message}");
        }
    }
}
