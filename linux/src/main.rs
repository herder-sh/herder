//! The GTK4 desktop app: paired machines and their sessions, over
//! [`herder_client_core::Client`] like the TUI.
//!
//! - `main` (this module): the tokio runtime the client runs on, the application, and the
//!   wiring between the client and the window.
//! - `window`: the main window, drawn from plain [`herder_client_core::Machine`] values.

mod window;

use adw::prelude::*;
use gtk::{gio, glib};
use herder_client_core::Client;

use window::MainWindow;

const APP_ID: &str = "sh.herder.Herder";

fn main() -> glib::ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("herder: starting the tokio runtime: {err}");
            return glib::ExitCode::FAILURE;
        }
    };
    // The client spawns its connections on the runtime, and the GTK callbacks that open it
    // run on this thread.
    let _runtime = runtime.enter();
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(activate);
    app.run()
}

fn activate(app: &adw::Application) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }
    let window = MainWindow::new(Some(app));
    match open_client() {
        Ok(client) => connect(app, &window, client),
        Err(err) => window.show_error(&err),
    }
    window.present();
}

/// Opens this device's profile, the one the TUI uses.
fn open_client() -> Result<Client, String> {
    let config_dir = glib::user_config_dir().join("herder");
    let config_dir = config_dir
        .to_str()
        .ok_or_else(|| format!("the config dir {} is not valid UTF-8", config_dir.display()))?
        .to_owned();
    Client::open(
        config_dir,
        format!("herder-gtk/{}", env!("CARGO_PKG_VERSION")),
    )
    .map_err(|err| err.to_string())
}

/// Shows the client's machines as they change, and wires the app actions to it.
fn connect(app: &adw::Application, window: &MainWindow, client: Client) {
    window.show_machines(&client.machines());
    let changes = client.changes();
    let shown = window.clone();
    let machines = client.clone();
    glib::spawn_future_local(async move {
        while changes.next().await {
            shown.show_machines(&machines.machines());
        }
    });

    let reconnect = gio::SimpleAction::new("reconnect", None);
    let woken = client.clone();
    reconnect.connect_activate(move |_, _| woken.wake());
    app.add_action(&reconnect);
    app.set_accels_for_action("app.reconnect", &["<Control>r"]);

    // A desktop app is not backgrounded; quitting is when the offline cache must be saved.
    app.connect_shutdown(move |_| client.suspend());
}
