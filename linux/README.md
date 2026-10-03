# linux

The GTK4 + libadwaita desktop app, built on `herder-client-core`. It opens the same profile as
the TUI (`$XDG_CONFIG_HOME/herder`) and shows the paired machines with their connection state.

It is its own Cargo workspace, so the root workspace builds without GTK. To build it, install
the GTK 4 and libadwaita dev packages (`libgtk-4-dev libadwaita-1-dev` on Debian/Ubuntu), then:

```sh
cd linux
cargo run
```

The tests build the window and need a display: run them under `xvfb-run cargo test`, or with
`GDK_BACKEND=broadway` and `gtk4-broadwayd` running.
