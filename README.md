# herder

herder runs coding agents — Claude Code, Codex, Cursor, and later others — across many
Linux machines. It drives each vendor's own unmodified CLI; it never touches provider
login tokens. A daemon on each machine hosts agent sessions, a vault keeps them durable,
and you watch and steer them from a terminal UI or native apps for macOS, iOS, Linux and
Android, all built on one shared Rust client core.

**Status:** pre-alpha, under construction. Nothing works yet.

## Install

On Linux (x86_64 or arm64):

```sh
curl -fsSL https://raw.githubusercontent.com/herder-sh/herder/main/install.sh | sh
herder service install   # run the daemon now and at every boot (systemd user service)
herder update            # replace herder with the latest release
```

The script installs a static binary to `~/.local/bin/herder` after checking its sha256.
Set `HERDER_VERSION=1.2.3` to install a specific release.

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
