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
#[cfg(test)]
mod fake;
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
    forward_input(tx.clone())?;

    let mut terminal = ratatui::init();
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
                }
            }
            next = rx.try_recv().ok();
        }
        if quit {
            break Ok(());
        }
        subscriptions.sync(&client, &app, &tx);
    };
    ratatui::restore();
    result
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
fn config_dir() -> Result<PathBuf> {
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
