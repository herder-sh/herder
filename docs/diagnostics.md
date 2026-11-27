# Diagnostics

What herder shares while its person turns on sharing diagnostics, and nothing while they do
not: the apps' one switch (asked once on first launch, then on the You page), a daemon's
`[telemetry] enabled` in `daemon.toml`, or `t` on a machine in the terminal UI's machines
panel. `site/privacy.html` says the same for everyone; this page lists every field.

Everything leaves through `herder-telemetry` (Rust: the daemon and the apps' client core) or
`AppScrub` (Swift: the apps), which rebuild each event from what may go rather than strip it.
Strings go only when they are herder's own words: lowercase letters, digits and underscores
(`system_vpn`, `send_prompt`), or code names (types, functions) in stack traces.

Every event carries: a random install id (Sentry `user.id`, PostHog `distinct_id`), the
herder version, `component` (`app` or `daemon`), the OS and its version, the processor
architecture, and in Sentry the device family and model. GeoIP is off in PostHog and no person
profiles are made; set both projects to discard IP addresses too (Sentry: Security & Privacy →
Prevent Storing of IP Addresses; PostHog: Project settings → Discard client IP data).

## Sentry (EU): traces, errors, crashes

Sampled at the tail: every operation is recorded, and a trace is sent when its trace id falls
in the 5% picked (the first 8 hex digits under 5% of `u32::MAX`, the same in every process), or
when any operation in it took a second or more.

| Span | Where | Data |
| --- | --- | --- |
| `command` (`app.command`) | app, each command about a session | `command` |
| `command` | client core, each `send` | `command` |
| `connect` | client core, each connection attempt | `route`, `outcome` (`ok`, `failed`) |
| `handshake` | client core and daemon, the hello | `route` on the daemon (`direct`, `tunnel`, `hub`) |
| `command` | daemon, applying a command, in the sender's trace | `command` |
| `replay` | daemon, a subscription's replay | `events` |
| `turn.start` | daemon, a prompt until the CLI has it, in the sender's trace | |
| `hub.relay` | hub, a relay stream until both ends are answered | `outcome` |

`route` is one of `home_network`, `own_vpn`, `direct`, `system_vpn`, `hub_wire_guard`,
`tunnel`, `hub`. `command` is a command's kind, such as `send_prompt` or `open_terminal`.

Errors: a daemon's and the client core's `error`-level log events and panics, as the module
that logged them, the error's type and the stack's functions and lines. The apps: crashes,
app hangs (2 s), watchdog terminations, slow starts, slow and frozen frames, screen loads on
the iPhone (by the view controller's type), and on the iPhone 5% of sessions' profiles. Never
an error's message or value, a file name or path, a breadcrumb, request data, the device's
name, locale or time zone.

## PostHog (EU): measurements

| Event | From | Properties |
| --- | --- | --- |
| `diagnostics_on` | both | |
| `connected` | client core | `route`, `connect_ms`, `reconnect` |
| `connect_failed` | client core | `attempt`, `connect_ms` |
| `disconnected` | client core | `route`, `cause` (`lost`, `dead`, `dropped`, `asked`), `connected_ms`, `average_rtt_ms`, `max_rtt_ms`, `missed_pongs`, `suspended` |
| `command` | client core | `command`, `ms` (until the daemon answered), `outcome` (`ok`, `rejected`, `failed`) |
| `replayed` | daemon | `events`, `ms`, `from_start` |
| `turn_started` | daemon | `start_ms` (prompt taken until the CLI has it), `cli_started` |

Besides these: `component`, `herder_version`, `arch`, and PostHog's `$os`, `$os_version`,
`$lib`, `$lib_version`, `$geoip_disable` (true).

The dashboard ("herder connection quality") is made by `scripts/posthog-dashboard.py`.

## Keys

All write-only client keys, given at build time, never in the repository: `SENTRY_DSN_IOS`,
`SENTRY_DSN_MACOS` (the apps' Info.plist), `SENTRY_DSN_DAEMON` (the `herder` binary; a
daemon's `[telemetry] dsn` overrides it), `POSTHOG_PROJECT_TOKEN` (both). `SENTRY_AUTH_TOKEN`
and `SENTRY_ORG` upload the apps' dSYMs in the release.
