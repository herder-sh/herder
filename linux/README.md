# linux

The GTK4 + libadwaita desktop app, built on `herder-client-core`. It opens the same profile as
the TUI (`$XDG_CONFIG_HOME/herder`) and shows, live, the paired machines with their connection
state and a vault's hosts online or offline, and the sessions of all of them or of the one
selected: grouped by project, or by machine with <kbd>Ctrl</kbd>+<kbd>G</kbd> (the TUI's `v`),
each with its status, task children and pull requests. Below 600 sp wide the sidebar and the
list become pages, as on a phone.

It is its own Cargo workspace, so the root workspace builds without GTK. To build it, install
the GTK 4 and libadwaita dev packages (`libgtk-4-dev libadwaita-1-dev` on Debian/Ubuntu), then:

```sh
cd linux
cargo run
```

The tests build the window and need a display: run them under `xvfb-run cargo test`, or with
`GDK_BACKEND=broadway` and `gtk4-broadwayd` running.
