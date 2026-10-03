//! The GTK4 desktop app: paired machines and their sessions, over
//! [`herder_client_core::Client`] like the TUI.
//!
//! - `main` (this module): the tokio runtime the client runs on, the application, and the
//!   wiring between the client and the window.
//! - `lists`: the session lists, built from plain [`herder_client_core::Machine`] values and
//!   what each session's subscription said.
//! - `session`: one session's transcript and state, folded from its subscription.
//! - `window`: the main window, drawing the machines and those lists.
//! - `session_view`, `transcript`, `tools`, `markdown`: the open session, its transcript and
//!   composer.
//! - `theme`: the colour tokens and the rules that use them.

#[cfg(test)]
mod e2e;
mod lists;
mod markdown;
#[cfg(test)]
mod screenshots;
mod session;
mod session_view;
mod theme;
mod tools;
mod transcript;
#[cfg(test)]
mod view_tests;
mod window;

use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use herder_client_core::{Client, Machine};

use lists::SessionKey;
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
    theme::load();
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

/// Shows the client's machines and their sessions as they change, and wires the app actions
/// to it.
fn connect(app: &adw::Application, window: &MainWindow, client: Client) {
    wire(window, &client);

    let reconnect = gio::SimpleAction::new("reconnect", None);
    let woken = client.clone();
    reconnect.connect_activate(move |_, _| woken.wake());
    app.add_action(&reconnect);
    app.set_accels_for_action("app.reconnect", &["<Control>r"]);
    app.set_accels_for_action("win.group-by-machine", &["<Control>g"]);

    // A desktop app is not backgrounded; quitting is when the offline cache must be saved.
    app.connect_shutdown(move |_| client.suspend());
}

/// Shows the client's machines and their sessions in `window` as they change, and sends the
/// session view's commands through it.
fn wire(window: &MainWindow, client: &Client) {
    let sender = client.clone();
    window.set_sender(Rc::new(move |host_id, command| {
        let client = sender.clone();
        Box::pin(async move {
            client
                .send(host_id, command)
                .await
                .map(|_| ())
                .map_err(|err| err.to_string())
        })
    }));
    let mut subscriptions = Subscriptions::default();
    let mut refresh = {
        let window = window.clone();
        let client = client.clone();
        move || {
            let machines = client.machines();
            window.show_machines(&machines);
            subscriptions.sync(&client, &window, &machines);
        }
    };
    refresh();
    let changes = client.changes();
    glib::spawn_future_local(async move {
        while changes.next().await {
            refresh();
        }
    });
}

/// One task per listed session, folding its updates into the window.
#[derive(Default)]
struct Subscriptions(HashMap<SessionKey, glib::JoinHandle<()>>);

impl Subscriptions {
    /// Subscribes to every listed session and drops the ones no longer listed.
    fn sync(&mut self, client: &Client, window: &MainWindow, machines: &[Machine]) {
        let listed = lists::keys(machines);
        let tasks = &mut self.0;
        tasks.retain(|key, task| {
            let keep = listed.contains(key);
            if !keep {
                // Dropping the subscription with its task unsubscribes.
                task.abort();
            }
            keep
        });
        for key in listed {
            if tasks.contains_key(&key) {
                continue;
            }
            let Ok(subscription) =
                client.subscribe_session(key.host_id.clone(), key.session_id.clone())
            else {
                // The machine is gone; the next machine list drops the session.
                continue;
            };
            let window = window.clone();
            let task_key = key.clone();
            let task = glib::spawn_future_local(async move {
                while let Some(update) = subscription.next().await {
                    window.apply(&task_key, &update);
                }
            });
            tasks.insert(key, task);
        }
    }
}
