//! The account's limit windows: read once with `get_usage`, and taken from the
//! `rate_limit_event` lines of a running session.

use std::collections::BTreeMap;

use herder_protocol::{TurnError, UsageWindow};
use serde::Deserialize;
use serde_json::Value;

use super::session::{STOP_TIMEOUT, fatal, gone};
use super::wire::{self, EventWindow, Incoming, Request};
use crate::transport::Transport;

/// The windows `get_usage` answers with: the plan's limits from claude.ai's usage endpoint.
///
/// Sends `initialize`, then `get_usage` with `skip_behaviors`, then closes stdin. No prompt is
/// sent, so no turn runs and no tokens are spent. An account without plan limits, such as an
/// API key, has no windows.
pub(super) async fn read(transport: Transport) -> Result<Vec<UsageWindow>, TurnError> {
    let Transport {
        stdin,
        mut stdout,
        mut exit,
    } = transport;
    let mut answer = None;
    for (id, request) in [
        ("herder-1", Request::Initialize),
        (
            "herder-2",
            Request::GetUsage {
                skip_behaviors: true,
            },
        ),
    ] {
        let line = wire::ControlRequestLine {
            kind: "control_request",
            request_id: id,
            request,
        };
        // Serializing these plain structs cannot fail.
        let line = serde_json::to_string(&line).unwrap_or_default();
        if stdin.send(line).await.is_err() {
            break;
        }
        let response = loop {
            let Some(line) = stdout.recv().await else {
                return Err(fatal(format!(
                    "claude stopped while reading usage: {}",
                    gone(&mut exit).await
                )));
            };
            if let Ok(Incoming::ControlResponse { response }) = serde_json::from_str(&line)
                && response.request_id == id
            {
                break response;
            }
        };
        if let Some(error) = response.error.filter(|_| response.subtype == "error") {
            return Err(fatal(format!("claude refused {id}: {error}")));
        }
        answer = response.response;
    }
    // Closing stdin is how `claude` is asked to exit; it is killed if it does not.
    drop(stdin);
    let _ = tokio::time::timeout(STOP_TIMEOUT, async {
        while stdout.recv().await.is_some() {}
    })
    .await;
    let _ = gone(&mut exit).await;
    let usage: GetUsage = answer
        .and_then(|answer| serde_json::from_value(answer).ok())
        .unwrap_or_default();
    Ok(usage.rate_limits.map(plan_windows).unwrap_or_default())
}

/// The `get_usage` answer, as far as herder reads it.
#[derive(Debug, Default, Deserialize)]
struct GetUsage {
    #[serde(default)]
    rate_limits: Option<RateLimits>,
}

#[derive(Debug, Deserialize)]
struct RateLimits {
    /// Per-model weekly windows, such as Fable's.
    #[serde(default)]
    model_scoped: Vec<ModelWindow>,
    /// `five_hour`, `seven_day`, `seven_day_opus` and the rest, each a [`PlanWindow`] or
    /// null, next to fields that are not windows.
    #[serde(flatten)]
    windows: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize)]
struct PlanWindow {
    /// Percent used, 0 to 100.
    utilization: Option<f64>,
    /// ISO 8601.
    resets_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ModelWindow {
    display_name: String,
    utilization: Option<f64>,
    resets_at: Option<String>,
}

/// The windows with a known utilization: the five-hour and seven-day ones by their own name,
/// and each per-model one as `seven_day_<model>`, the CLI's naming for its Opus and Sonnet
/// windows.
fn plan_windows(limits: RateLimits) -> Vec<UsageWindow> {
    let window = |name: String, utilization: Option<f64>, resets_at: Option<String>| {
        Some(UsageWindow {
            window: name,
            used_percent: utilization?,
            resets_at: resets_at.and_then(|at| at.parse().ok()),
        })
    };
    let mut windows: Vec<UsageWindow> = limits
        .windows
        .into_iter()
        .filter(|(name, _)| name == "five_hour" || name.starts_with("seven_day"))
        .filter_map(|(name, value)| {
            let plan: PlanWindow = serde_json::from_value(value).ok()?;
            window(name, plan.utilization, plan.resets_at)
        })
        .collect();
    for model in limits.model_scoped {
        let name = format!(
            "seven_day_{}",
            model.display_name.to_lowercase().replace(' ', "_")
        );
        if windows.iter().all(|known| known.window != name)
            && let Some(window) = window(name, model.utilization, model.resets_at)
        {
            windows.push(window);
        }
    }
    windows
}

/// The `unifiedWindows` of a `rate_limit_event`: each window's `utilization`, 0 to 1, and its
/// `resetsAt` in Unix seconds.
pub(super) fn event_windows(windows: &BTreeMap<String, EventWindow>) -> Vec<UsageWindow> {
    windows
        .iter()
        .map(|(name, window)| UsageWindow {
            window: name.clone(),
            // Rounded to hundredths of a percent, so 0.07 reads as 7, not 7.000000000000001.
            used_percent: (window.utilization * 10_000.0).round() / 100.0,
            resets_at: window
                .resets_at
                .and_then(|seconds| herder_protocol::Timestamp::from_second(seconds).ok()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use herder_protocol::Timestamp;
    use serde_json::json;

    use super::*;

    #[test]
    fn plan_windows_keep_those_with_a_utilization() {
        // Trimmed from Claude Code 2.1.286's answer to `get_usage`.
        let limits: RateLimits = serde_json::from_value(json!({
            "five_hour": {"utilization": 9, "resets_at": "2026-10-02T20:19:59.921522+00:00", "limit_dollars": null},
            "seven_day": {"utilization": 2.5, "resets_at": "2026-10-08T15:59:59+00:00"},
            "seven_day_opus": null,
            "seven_day_breakdown": null,
            "tangelo": null,
            "extra_usage": {"is_enabled": true, "utilization": 3},
            "limits": [{"kind": "session", "percent": 9}],
            "model_scoped": [{"display_name": "Fable", "utilization": 0, "resets_at": "2026-10-08T16:00:00+00:00"}]
        }))
        .unwrap();
        let at = |s: &str| Some(s.parse::<Timestamp>().unwrap());
        assert_eq!(
            plan_windows(limits),
            [
                UsageWindow {
                    window: "five_hour".into(),
                    used_percent: 9.0,
                    resets_at: at("2026-10-02T20:19:59.921522Z"),
                },
                UsageWindow {
                    window: "seven_day".into(),
                    used_percent: 2.5,
                    resets_at: at("2026-10-08T15:59:59Z"),
                },
                UsageWindow {
                    window: "seven_day_fable".into(),
                    used_percent: 0.0,
                    resets_at: at("2026-10-08T16:00:00Z"),
                },
            ]
        );
    }

    #[test]
    fn event_windows_are_percentages() {
        let windows: BTreeMap<String, EventWindow> = serde_json::from_value(json!({
            "five_hour": {"utilization": 0.62, "resetsAt": 1790953200},
            "seven_day": {"utilization": 0.07}
        }))
        .unwrap();
        let windows = event_windows(&windows);
        assert_eq!(windows[0].window, "five_hour");
        assert!((windows[0].used_percent - 62.0).abs() < 1e-9);
        assert_eq!(
            windows[0].resets_at,
            Some(Timestamp::from_second(1790953200).unwrap())
        );
        assert_eq!(windows[1].resets_at, None);
    }
}
