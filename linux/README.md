# linux

The GTK4 + libadwaita desktop app, built on `herder-client-core`. It opens the same profile as
the TUI (`$XDG_CONFIG_HOME/herder`) and shows, live, the paired machines with their connection
state and a vault's hosts online or offline, and the sessions of all of them or of the one
selected: grouped by project, or by machine with <kbd>Ctrl</kbd>+<kbd>G</kbd> (the TUI's `v`),
each with its status, task children and pull requests. Below 600 sp wide the sidebar and the
list become pages, as on a phone.

Activating a session opens it: its transcript, each tool call one row (tool, what it works
on, how long it took and how it went) that expands to its output or diff; over it the
session's pull requests, each with its state, checks, review, mergeability and branch, which
open in the browser, and whose menu copies or unlinks them (the header's menu links another);
and below it the composer (<kbd>Enter</kbd> sends, <kbd>Shift</kbd>+<kbd>Enter</kbd>
adds a line) with the session's account, model and permission mode, each a picker that
switches it. An approval or a question replaces the composer with a card, answered with its
buttons, <kbd>y</kbd> / <kbd>n</kbd> or the digits. Archived and moved sessions, and a vault's,
are read-only. The sidebar's "Pull requests" lists every session's PRs, grouped by project
or by machine, each session opening from its group.

It is its own Cargo workspace, so the root workspace builds without GTK. To build it, install
the GTK 4 and libadwaita dev packages (`libgtk-4-dev libadwaita-1-dev` on Debian/Ubuntu), then:

```sh
cd linux
cargo run
```

The tests build the window and need a display: run them under `xvfb-run cargo test`, or with
`GDK_BACKEND=broadway` and `gtk4-broadwayd` running. One drives a full turn against a real
daemon with the fake adapter. `cargo test screenshots -- --ignored` renders the screenshots in
`docs/screenshots/p8-4/`; under broadway, keep a browser open on the display so it draws.
