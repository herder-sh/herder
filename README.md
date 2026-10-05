# herder

**[herder.sh](https://herder.sh)**

herder runs coding agents (Claude Code, Codex, Cursor and OpenCode) across all your
accounts and Linux machines. It drives each vendor's own unmodified CLI and never touches
provider login tokens. A daemon on each machine hosts agent sessions, fails over between
accounts when one hits its limit, and an optional vault keeps sessions durable. You watch
and steer them from a terminal UI or the native apps for macOS and iOS, built on a shared
Rust client core.

**Status:** early. Releases ship, but expect rough edges and breaking changes before 1.0.

## Install

On Linux (x86_64 or arm64):

```sh
curl -fsSL https://herder.sh/install | sh
herder service install   # run the daemon now and at every boot (systemd user service)
herder update            # replace herder with the latest release
```

The script installs a static binary to `~/.local/bin/herder` after checking its sha256.
Set `HERDER_VERSION=1.2.3` to install a specific release.

## Keeping accounts safe

herder logs each account in with the vendor's own CLI, on the machine that runs it, so the
provider sees that machine's network. Providers may flag or suspend subscription accounts
whose traffic looks unusual. To lower the risk:

- Run herder on machines with a residential connection, such as spare PCs at home, rather
  than VPSs or other datacenter addresses.
- Don't log in or run sessions through a VPN or proxy service.
- Don't log the same account in on many machines that work at once. Spread your accounts
  across machines instead.
- Don't use a subscription in third-party tools or proxies alongside herder. herder only
  ever runs it through the vendor's CLI.

herder can't make an account safe from suspension. Check each provider's terms for what
your plan allows.

## Using herder from a phone

Run bare `herder` over SSH or mosh from a phone SSH app. Below 65 columns the TUI shows one
pane at a time, with a bar of buttons for what can be done now over the status line. Every
view works with only these keys:

| Key                  | Does                                                    |
| -------------------- | ------------------------------------------------------- |
| ↑ / ↓                | move the selection, or scroll                           |
| PgUp / PgDn          | page                                                    |
| Home / End           | first / last; End follows the transcript again          |
| Tab / Shift-Tab      | next / previous button of the bar (forms: next field)   |
| Enter                | open, or press the button Tab moved to                  |
| Esc / Backspace      | back, or close a dialog                                 |

So approving a tool call is Tab, Enter. The letter shortcuts (`?` lists them) still work.

**Termius** sends gestures as keys, never as mouse events. In its keyboard settings, map:

- swipe up / down → Up / Down arrow
- two-finger swipe up / down → PgUp / PgDn
- swipe left / right → Shift-Tab / Tab

**Moshi** sends taps and swipes as mouse events once Mouse Mode is on; herder takes them,
so a tap opens a row or presses a button and a swipe scrolls. `:mouse off` hands the mouse
back to the app, to select text.

## Build and test

Requires Rust (the toolchain is pinned in `rust-toolchain.toml`).

```sh
cargo build --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p herder -- --version
```

## Contributing

Read [AGENTS.md](AGENTS.md) before opening a PR.

## License

Apache-2.0. See [LICENSE](LICENSE).
