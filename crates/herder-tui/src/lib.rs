//! The ratatui terminal client: paired machines, their sessions, and a live session view.
//!
//! The shape is one event loop over one state:
//!
//! - `app`: the state and its reducer. Every input is an `app::Msg`; the reducer returns
//!   `app::Effect`s for the loop to carry out. No I/O, so it is tested with plain values.
//! - `action`: what keys mean. Keys become `action::Action`s before they touch the state.
//! - `session`: one session as folded from its events: list facts and transcript entries.
//! - `views`: drawing, one module per screen area.
//! - `run` (this module): the terminal, the client, and the tasks that feed the loop.
//!
//! Everything network-related goes through [`herder_client_core::Client`].

mod action;
mod app;
mod compose;
#[cfg(test)]
mod fake;
mod machines;
mod prs;
mod session;
mod views;

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use herder_client_core::Client;
use ratatui::crossterm::event::{self, Event};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use app::{App, Effect, Msg};
use session::SessionKey;

/// Opens the TUI on this device's profile and runs it until the user quits.
pub fn run() -> Result<()> {
    let config_dir = config_dir()?;
    tokio::runtime::Runtime::new()
        .context("starting the tokio runtime")?
        .block_on(run_in(config_dir))
}

async fn run_in(config_dir: PathBuf) -> Result<()> {
    let client = Client::open(
        config_dir,
        format!("herder-tui/{}", env!("CARGO_PKG_VERSION")),
    )?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    forward_machines(&client, tx.clone());

    let mut terminal = ratatui::init();
    let enhanced = enable_input_modes();
    if let Err(err) = forward_input(tx.clone()) {
        restore(enhanced);
        return Err(err);
    }
    let mut app = App::default();
    let mut subscriptions = Subscriptions::default();
    let result = loop {
        if let Err(err) = terminal.draw(|frame| views::draw(frame, &mut app)) {
            break Err(err.into());
        }
        let Some(msg) = rx.recv().await else {
            break Ok(());
        };
        // Fold in everything that queued up while drawing, then draw once.
        let mut quit = false;
        let mut next = Some(msg);
        while let Some(msg) = next {
            for effect in app.update(msg) {
                match effect {
                    Effect::Quit => quit = true,
                    Effect::Wake => client.wake(),
                    Effect::Send {
                        host_id,
                        command,
                        origin,
                    } => send(&client, host_id, command, origin, tx.clone()),
                    Effect::OpenUrl(url) => open_url(url, tx.clone()),
                    Effect::Pair(link) => pair(&client, link, tx.clone()),
                }
            }
            next = rx.try_recv().ok();
        }
        if quit {
            break Ok(());
        }
        subscriptions.sync(&client, &app, &tx);
    };
    restore(enhanced);
    result
}

/// Turns on bracketed paste, so a pasted prompt is one edit and not a key per character, and
/// where the terminal supports it, disambiguated keys, so Shift-Enter is not Enter. Returns
/// whether the keyboard enhancement was pushed.
fn enable_input_modes() -> bool {
    use ratatui::crossterm::event::{
        EnableBracketedPaste, KeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    };
    use ratatui::crossterm::{execute, terminal};
    let mut out = std::io::stdout();
    // Both are conveniences: without them typing still works, only less well.
    let _ = execute!(out, EnableBracketedPaste);
    terminal::supports_keyboard_enhancement().unwrap_or(false)
        && execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .is_ok()
}

/// Undoes [`enable_input_modes`] and restores the terminal.
fn restore(enhanced: bool) {
    use ratatui::crossterm::event::{DisableBracketedPaste, PopKeyboardEnhancementFlags};
    use ratatui::crossterm::execute;
    let mut out = std::io::stdout();
    if enhanced {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(out, DisableBracketedPaste);
    ratatui::restore();
}

/// Sends a command on its own task and feeds the daemon's answer back to the loop.
fn send(
    client: &Client,
    host_id: herder_protocol::HostId,
    command: herder_protocol::CommandBody,
    origin: compose::Origin,
    tx: mpsc::UnboundedSender<Msg>,
) {
    let client = client.clone();
    tokio::spawn(async move {
        let result = client
            .send(&host_id, command)
            .await
            .map_err(|err| err.to_string());
        let _ = tx.send(Msg::Sent { origin, result });
    });
}

/// Opens `url` with the desktop's opener; where there is none, as over SSH, the notice shows
/// the URL to copy instead.
fn open_url(url: String, tx: mpsc::UnboundedSender<Msg>) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let desktop = cfg!(target_os = "macos")
        || ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|var| std::env::var_os(var).is_some_and(|value| !value.is_empty()));
    if !desktop {
        let _ = tx.send(Msg::Notice(url));
        return;
    }
    let failed = tx.clone();
    let spawned = std::thread::Builder::new()
        .name("herder-tui-open".to_owned())
        .spawn(move || {
            let status = std::process::Command::new(opener)
                .arg(&url)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            match status {
                Ok(status) if status.success() => {
                    let _ = tx.send(Msg::Notice(format!("opened {url}")));
                }
                _ => {
                    let _ = tx.send(Msg::Notice(format!("{opener} failed; open {url}")));
                }
            }
        });
    if let Err(err) = spawned {
        let _ = failed.send(Msg::Notice(format!("cannot run {opener}: {err}")));
    }
}

/// Pairs with the daemon of `link` and sends the outcome to the loop.
fn pair(client: &Client, link: String, tx: mpsc::UnboundedSender<Msg>) {
    let client = client.clone();
    tokio::spawn(async move {
        let result = client.pair(link).await.map_err(|err| err.to_string());
        let _ = tx.send(Msg::Paired(result));
    });
}

/// Sends the machines now and after every change.
fn forward_machines(client: &Client, tx: mpsc::UnboundedSender<Msg>) {
    let client = client.clone();
    tokio::spawn(async move {
        let changes = client.changes();
        loop {
            if tx.send(Msg::Machines(client.machines())).is_err() {
                return;
            }
            if changes.next().await.is_none() {
                return;
            }
        }
    });
}

/// Reads terminal input on its own thread, as crossterm's read blocks.
fn forward_input(tx: mpsc::UnboundedSender<Msg>) -> Result<()> {
    std::thread::Builder::new()
        .name("herder-tui-input".to_owned())
        .spawn(move || {
            loop {
                let msg = match event::read() {
                    Ok(Event::Key(key)) => Msg::Key(key),
                    Ok(Event::Resize(..)) => Msg::Resize,
                    Ok(Event::Paste(text)) => Msg::Paste(text),
                    Ok(_) => continue,
                    Err(_) => return,
                };
                if tx.send(msg).is_err() {
                    return;
                }
            }
        })
        .context("starting the input thread")?;
    Ok(())
}

/// One task per subscribed session, forwarding its updates to the loop.
#[derive(Default)]
struct Subscriptions(HashMap<SessionKey, JoinHandle<()>>);

impl Subscriptions {
    /// Subscribes to the sessions the app wants and drops the ones it no longer lists.
    fn sync(&mut self, client: &Client, app: &App, tx: &mpsc::UnboundedSender<Msg>) {
        let wanted = app.wanted();
        self.0.retain(|key, task| {
            let keep = wanted.contains(key);
            if !keep {
                // Dropping the task drops the subscription, which unsubscribes.
                task.abort();
            }
            keep
        });
        for key in wanted {
            if self.0.contains_key(&key) {
                continue;
            }
            let Ok(subscription) = client.subscribe_session(&key.host_id, &key.session_id) else {
                // The machine is gone; the next machine list drops the session.
                continue;
            };
            let tx = tx.clone();
            let task_key = key.clone();
            let task = tokio::spawn(async move {
                while let Some(update) = subscription.next().await {
                    let msg = Msg::Session {
                        key: task_key.clone(),
                        update,
                    };
                    if tx.send(msg).is_err() {
                        return;
                    }
                }
            });
            self.0.insert(key, task);
        }
    }
}

/// The client profile's directory: `$XDG_CONFIG_HOME/herder`, else `~/.config/herder`.
pub fn config_dir() -> Result<PathBuf> {
    let absolute = |var| {
        std::env::var_os(var)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    if let Some(config) = absolute("XDG_CONFIG_HOME") {
        return Ok(config.join("herder"));
    }
    match absolute("HOME") {
        Some(home) => Ok(home.join(".config/herder")),
        None => bail!("cannot find the config dir: HOME is not set to an absolute path"),
    }
}
