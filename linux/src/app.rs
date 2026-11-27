//! The tokio runtime the client runs on, the application, and the wiring between the client
//! and the window. `herder-gtk --demo` opens the demo fleet the Mac app offers, recorded by
//! `apple/scripts/record-demo.sh`, with no profile and no connection.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use herder_client_core::{Client, Demo, FleetMachine, Machine};
use herder_protocol::HostId;

use crate::lists::{self, SessionKey};
use crate::machines::MachineOps;
use crate::theme;
use crate::window::MainWindow;

const APP_ID: &str = "sh.herder.Herder";

/// The demo fleet, as the Mac app bundles it.
const DEMO: &str = include_str!("../../apple/HerderKit/Sources/HerderKit/Resources/demo.json");

pub fn main() -> glib::ExitCode {
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
    let mut args: Vec<String> = std::env::args().collect();
    let demo = args.iter().any(|arg| arg == "--demo");
    args.retain(|arg| arg != "--demo");
    let app = adw::Application::builder()
        .application_id(if demo {
            "sh.herder.Herder.Demo"
        } else {
            APP_ID
        })
        .build();
    app.connect_activate(move |app| activate(app, demo));
    app.run_with_args(&args)
}

fn activate(app: &adw::Application, demo: bool) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }
    theme::load();
    let window = MainWindow::new(Some(app));
    let client = if demo { open_demo() } else { open_client() };
    match client {
        Ok(client) => connect(app, &window, client),
        Err(err) => window.show_error(&err),
    }
    window.present();
}

/// Opens the demo fleet: its machines show as connected, and nothing is sent.
pub(crate) fn open_demo() -> Result<Client, String> {
    let demo: Demo =
        serde_json::from_str(DEMO).map_err(|err| format!("the demo is not one: {err}"))?;
    Ok(Client::open_demo(demo))
}

/// Opens this device's profile, the one the TUI uses.
fn open_client() -> Result<Client, String> {
    let config_dir = profile_dir(|var| std::env::var_os(var), glib::user_config_dir);
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

/// The profile's directory, the TUI's: `$XDG_CONFIG_HOME/herder`. In the Flatpak,
/// `XDG_CONFIG_HOME` is the app's own directory under `~/.var/app`, so it takes the host's,
/// `HOST_XDG_CONFIG_HOME` or else `~/.config`, whose `herder` directory the sandbox may open.
fn profile_dir(
    env: impl Fn(&str) -> Option<OsString>,
    user_config_dir: impl FnOnce() -> PathBuf,
) -> PathBuf {
    let absolute = |var| {
        env(var)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let config = if env("FLATPAK_ID").is_some() {
        absolute("HOST_XDG_CONFIG_HOME")
            .or_else(|| absolute("HOME").map(|home| home.join(".config")))
    } else {
        None
    };
    config.unwrap_or_else(user_config_dir).join("herder")
}

/// Shows the client's machines and their sessions as they change, and wires the app actions
/// to it.
fn connect(app: &adw::Application, window: &MainWindow, client: Client) {
    wire(window, &client);
    window.set_ops(Rc::new(Fleet(client.clone())));

    let reconnect = gio::SimpleAction::new("reconnect", None);
    let woken = client.clone();
    reconnect.connect_activate(move |_, _| woken.wake());
    app.add_action(&reconnect);
    app.set_accels_for_action("app.reconnect", &["<Control>r"]);
    app.set_accels_for_action("win.new-session", &["<Control>n"]);

    // A desktop app is not backgrounded; quitting is when the offline cache must be saved.
    app.connect_shutdown(move |_| client.suspend());
}

/// Shows the client's machines and their sessions in `window` as they change, and sends the
/// session view's commands through it.
pub(crate) fn wire(window: &MainWindow, client: &Client) {
    let sender = client.clone();
    window.set_sender(Rc::new(move |host_id, command| {
        let client = sender.clone();
        Box::pin(async move {
            client
                .send(host_id, command)
                .await
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

/// The machines page's operations, on the client.
struct Fleet(Client);

impl MachineOps for Fleet {
    fn reconnect(&self, host_id: HostId) {
        let client = self.0.clone();
        glib::spawn_future_local(async move {
            if let Err(err) = client.reconnect(host_id).await {
                glib::g_warning!("herder", "reconnecting: {err}");
            }
        });
    }

    fn rename(&self, host_id: HostId, name: String) -> Result<(), String> {
        self.0.rename(host_id, name).map_err(|err| err.to_string())
    }

    fn forget(&self, host_id: HostId) -> Result<(), String> {
        self.0.forget(host_id).map_err(|err| err.to_string())
    }

    fn log_in(
        &self,
        machine: FleetMachine,
        shown: Box<dyn Fn(String)>,
        done: Box<dyn FnOnce(Result<String, String>)>,
    ) {
        let client = self.0.clone();
        let (code_tx, mut code_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        // The sign-in waits on the network and on the approval: on the runtime, with the
        // code and the end handed back to this thread.
        tokio::spawn(async move {
            let result = client
                .log_in(machine.hosts, machine.fingerprint, move |code, _| {
                    let _ = code_tx.send(code.to_owned());
                })
                .await
                .map(|machine| machine.name)
                .map_err(|err| err.to_string());
            let _ = done_tx.send(result);
        });
        glib::spawn_future_local(async move {
            while let Some(code) = code_rx.recv().await {
                shown(code);
            }
        });
        glib::spawn_future_local(async move {
            done(
                done_rx
                    .await
                    .unwrap_or_else(|_| Err("the sign-in stopped".to_owned())),
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |var| {
            vars.iter()
                .find(|(name, _)| *name == var)
                .map(|(_, value)| OsString::from(value))
        }
    }

    fn glib_dir() -> PathBuf {
        PathBuf::from("/home/u/.config")
    }

    #[test]
    fn the_profile_is_in_the_user_config_dir_outside_the_flatpak() {
        let vars = [("HOME", "/home/u"), ("HOST_XDG_CONFIG_HOME", "/elsewhere")];
        assert_eq!(
            profile_dir(env(&vars), glib_dir),
            PathBuf::from("/home/u/.config/herder")
        );
    }

    #[test]
    fn the_flatpak_opens_the_hosts_profile() {
        let sandbox = || PathBuf::from("/home/u/.var/app/sh.herder.Herder/config");
        let vars = [("FLATPAK_ID", "sh.herder.Herder"), ("HOME", "/home/u")];
        assert_eq!(
            profile_dir(env(&vars), sandbox),
            PathBuf::from("/home/u/.config/herder")
        );
        let vars = [
            ("FLATPAK_ID", "sh.herder.Herder"),
            ("HOME", "/home/u"),
            ("HOST_XDG_CONFIG_HOME", "/home/u/conf"),
        ];
        assert_eq!(
            profile_dir(env(&vars), sandbox),
            PathBuf::from("/home/u/conf/herder")
        );
    }
}
