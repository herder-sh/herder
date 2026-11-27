#!/usr/bin/env python3
"""Creates, or replaces, the "herder connection quality" dashboard in PostHog (EU).

    POSTHOG_PERSONAL_API_KEY=phx_... scripts/posthog-dashboard.py [project id, default 322449]

The personal API key needs the dashboard and insight write scopes; the project token the
apps and daemons send with cannot make dashboards. Each insight reads the events
docs/diagnostics.md lists.
"""

import json
import os
import sys
import urllib.request

HOST = "https://eu.posthog.com"
NAME = "herder connection quality"


def trend(name, event, math="total", prop=None, breakdown=None, display="ActionsLineGraph"):
    series = {"kind": "EventsNode", "event": event, "math": math}
    if prop:
        series["math_property"] = prop
    query = {
        "kind": "TrendsQuery",
        "series": [series],
        "interval": "day",
        "dateRange": {"date_from": "-30d"},
        "trendsFilter": {"display": display},
    }
    if breakdown:
        query["breakdownFilter"] = {"breakdown": breakdown, "breakdown_type": "event"}
    return name, {"kind": "InsightVizNode", "source": query}


INSIGHTS = [
    trend("Connect time p90 (ms) by route", "connected", "p90", "connect_ms", "route"),
    trend("Connections by route", "connected", breakdown="route", display="ActionsBarValue"),
    trend("Failed connection attempts", "connect_failed"),
    trend("Disconnects by cause", "disconnected", breakdown="cause"),
    trend("Round trip p90 (ms) by route", "disconnected", "p90", "average_rtt_ms", "route"),
    trend("Missed pongs", "disconnected", "sum", "missed_pongs", "route"),
    trend("Send latency p90 (ms) by command", "command", "p90", "ms", "command"),
    trend("Commands by outcome", "command", breakdown="outcome"),
    trend("Replay time p90 (ms)", "replayed", "p90", "ms"),
    trend("Replayed events p90", "replayed", "p90", "events"),
    trend("Turn start p90 (ms)", "turn_started", "p90", "start_ms", "cli_started"),
    trend("Feature use: commands", "command", breakdown="command", display="ActionsBarValue"),
    trend("Installs sharing, by component", "diagnostics_on", "dau", breakdown="component"),
    trend("Versions", "diagnostics_on", "dau", breakdown="herder_version"),
]


def call(method, path, body=None):
    request = urllib.request.Request(
        f"{HOST}{path}",
        method=method,
        data=json.dumps(body).encode() if body is not None else None,
        headers={
            "Authorization": f"Bearer {os.environ['POSTHOG_PERSONAL_API_KEY']}",
            "Content-Type": "application/json",
        },
    )
    with urllib.request.urlopen(request) as response:
        text = response.read()
        return json.loads(text) if text else None


def main():
    project = sys.argv[1] if len(sys.argv) > 1 else "322449"
    base = f"/api/projects/{project}"
    for dashboard in call("GET", f"{base}/dashboards/?limit=200")["results"]:
        if dashboard["name"] == NAME and not dashboard.get("deleted"):
            call("PATCH", f"{base}/dashboards/{dashboard['id']}/", {"deleted": True})
    dashboard = call("POST", f"{base}/dashboards/", {
        "name": NAME,
        "description": "Opt-in, anonymous measurements from herder's apps and daemons "
                       "(docs/diagnostics.md).",
    })
    for name, query in INSIGHTS:
        call("POST", f"{base}/insights/", {
            "name": name, "query": query, "dashboards": [dashboard["id"]],
        })
    print(f"{HOST}/project/{project}/dashboard/{dashboard['id']}")


if __name__ == "__main__":
    main()
