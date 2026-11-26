# linux

The herder app for Linux: GTK 4 and libadwaita, on `herder-client-core` directly, laid out and
styled as the Mac app (`apple/`, `docs/apple-design.md`), dark as it is. It opens the same
profile as the TUI (`$XDG_CONFIG_HOME/herder`), so machines added in either show in both.

- **Sidebar:** Board (badged amber with the requests waiting on you), Chats, Machines, then
  every project merged across machines with its live counts (amber waiting, blue running), and
  a dot per machine at the foot.
- **List pane:** the section's requests first, each approval answered with Deny or Allow and a
  question with its choices; then its tasks by day of last activity, each with its state glyph,
  title, what it is doing, project, branch and machine, a task tree's agents folded under it
  and the archived tasks last. The search filters them.
- **Session:** beside the list, its transcript with tool calls that unfold, its pull requests,
  the pending request, and the composer with the account, model and permission mode.
- **Machines:** each machine's connection, address, sessions and herder, to reconnect, rename
  or forget; **Add a Machine** signs this device in from the `herder://fleet` link an app of
  yours shares, or by address and fingerprint, as `herder login` does: an app of yours approves
  it with its passkey.
- **New Session** (<kbd>Ctrl</kbd>+<kbd>N</kbd>): a project on a connected machine, or a chat,
  and the first prompt.

Under 1000 sp the sidebar slides over; under 680 sp the list and the session take turns.

`herder-gtk --demo` opens the demo fleet the Mac app offers (`apple/scripts/record-demo.sh`),
with no profile and no connection.

## Build

It is a member of the root workspace, built only on Linux (elsewhere the binary just says
so). Install the GTK 4 and libadwaita dev packages (`libgtk-4-dev libadwaita-1-dev` on
Debian/Ubuntu, `gtk4 libadwaita` on Arch), then:

```sh
cargo run -p herder-gtk            # or: cargo run -p herder-gtk -- --demo
```

To give it its icon in the launcher, put the binary on your `PATH` and install its desktop
entry and icon:

```sh
install -Dm644 linux/data/sh.herder.Herder.desktop ~/.local/share/applications/sh.herder.Herder.desktop
install -Dm644 linux/data/icons/hicolor/scalable/apps/sh.herder.Herder.svg \
  ~/.local/share/icons/hicolor/scalable/apps/sh.herder.Herder.svg
```

## Test

The window tests need a display: CI runs them under `xvfb-run`. Locally, run
`gtk4-broadwayd :7` and `GDK_BACKEND=broadway BROADWAY_DISPLAY=:7 cargo test -p herder-gtk`.
One drives a full turn against a real daemon with the fake adapter.
`cargo test -p herder-gtk screenshots -- --ignored` renders the demo fleet into
`docs/screenshots/p8-8/`.
