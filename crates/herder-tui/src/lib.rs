//! The ratatui terminal client: paired machines, their sessions, and a live session view.
//!
//! The shape is one event loop over one state:
//!
//! - `app`: the state and its reducer. Every input is an `app::Msg`; the reducer returns
//!   `app::Effect`s for the loop to carry out. No I/O, so it is tested with plain values.
//! - `action`: what keys mean. Keys become `action::Action`s before they touch the state.
//! - `session`: one session as folded from its events: list facts and transcript entries.
//! - `views`: drawing, one module per screen area.
//! - `mouse`: taps and swipes, matched against where the last frame drew what.
//! - `run` (this module): the terminal, the client, and the tasks that feed the loop.
//!
//! Everything network-related goes through [`herder_client_core::Client`].

mod account_screen;
mod accounts;
mod action;
mod app;
mod backend;
mod bar;
mod chat;
mod compose;
#[cfg(test)]
mod fake;
mod inbox;
mod machines;
mod mouse;
mod nav;
mod projects;
mod prompt;
mod prs;
mod recover;
mod session;
mod settings;
mod switch;
mod terminal;
pub mod ui;
mod views;

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use herder_client_core::Client;
use ratatui::crossterm::event::{self, Event, MouseEventKind};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use app::{App, Effect, Msg};
use session::SessionKey;
use settings::Settings;
use ui::theme::{Mode, Theme};

/// How long the screen may sit unchanged before it is repainted from scratch, in case the
/// terminal lost track of it: cheap when nothing changed, as mosh then sends nothing.
const IDLE_REPAINT: std::time::Duration = std::time::Duration::from_secs(5);

/// How often the screen is drawn while a spinner turns: the slower glyph set's frame.
const SPINNER_FRAME: std::time::Duration = std::time::Duration::from_millis(120);

/// The theme `tui.json` chose, else the one the terminal suits; with why a chosen theme did
/// not load.
fn theme(config_dir: &std::path::Path) -> (Theme, Option<String>) {
    let look = settings::look(config_dir);
    let env = |key| std::env::var(key).ok();
    let colorterm = env("COLORTERM");
    let mode = look
        .mode
        .unwrap_or_else(|| Mode::detect(env("COLORFGBG").as_deref()));
    let name = Theme::choose(look.theme.as_deref(), colorterm.as_deref());
    match Theme::load(name, mode, config_dir) {
        Ok(theme) => (theme, None),
        Err(err) => {
            let fallback = Theme::choose(None, colorterm.as_deref());
            let theme = Theme::load(fallback, mode, config_dir).unwrap_or_else(|_| Theme::ansi());
            (theme, Some(format!("{err:#}")))
        }
    }
}

/// Opens the TUI on this device's profile and runs it until the user quits.
pub fn run() -> Result<()> {
    let config_dir = config_dir()?;
    tokio::runtime::Runtime::new()
        .context("starting the tokio runtime")?
        .block_on(run_in(config_dir))
}

async fn run_in(config_dir: PathBuf) -> Result<()> {
    let settings = settings::load(&config_dir);
    let (theme, notice) = theme(&config_dir);
    let mut app = App {
        mouse: settings.mouse,
        glyphs: settings.glyphs,
        layout: settings.layout,
        theme,
        notice,
        ..App::default()
    };
    let client = Client::open(
        config_dir
            .to_str()
            .context("the config dir is not valid UTF-8")?
            .to_owned(),
        format!("herder-tui/{}", env!("CARGO_PKG_VERSION")),
    )?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    forward_machines(&client, tx.clone());
    let raw = terminal::RawInput::default();

    // Raw mode, the alternate screen and a panic hook that restores both; drawing goes
    // through the TUI's own backend.
    drop(ratatui::init());
    let mut terminal = ratatui::Terminal::new(backend::Anchored(
        ratatui::backend::CrosstermBackend::new(std::io::stdout()),
    ))?;
    // ratatui's panic hook restores the screen, but not mouse reporting.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = mouse::report(false);
        hook(info);
    }));
    let mut modes = enable_input_modes(app.mouse);
    if let Err(err) = forward_input(tx.clone(), raw.clone()) {
        restore(modes);
        return Err(err);
    }
    let mut subscriptions = Subscriptions::default();
    // The first frame covers what the shell left where the alternate screen is missing.
    let mut repaint = true;
    let mut last_full = std::time::Instant::now();
    let result = loop {
        if repaint {
            last_full = std::time::Instant::now();
        }
        if let Err(err) = paint(&mut terminal, &mut app, std::mem::take(&mut repaint)) {
            break Err(err);
        }
        // An armed leader wakes the loop when it lapses, so its badge goes; while a turn
        // runs, the spinner's next frame is due before the idle repaint.
        let leader = app.leader_left(std::time::Instant::now());
        let idle = if app.animating() {
            SPINNER_FRAME
        } else {
            IDLE_REPAINT
        };
        let wait = leader.map_or(idle, |leader| leader.min(idle));
        let msg = match tokio::time::timeout(wait, rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break Ok(()),
            Err(_) if leader.is_some() => Msg::Tick(std::time::Instant::now()),
            Err(_) => {
                if last_full.elapsed() >= IDLE_REPAINT {
                    repaint = true;
                }
                continue;
            }
        };
        // Fold in everything that queued up while drawing, then draw once.
        let mut quit = false;
        let mut attach = None;
        let mut next = Some(msg);
        while let Some(msg) = next {
            for effect in app.update(msg) {
                match effect {
                    Effect::Quit => quit = true,
                    Effect::Wake => client.wake(),
                    Effect::Repaint => repaint = true,
                    Effect::Send {
                        host_id,
                        command,
                        origin,
                    } => send(&client, host_id, command, origin, tx.clone()),
                    Effect::OpenUrl(url) => open_url(url, tx.clone()),
                    Effect::Pair(link) => pair(&client, link, tx.clone()),
                    Effect::RenameMachine { host_id, name } => {
                        if let Err(err) = client.rename(host_id.clone(), name) {
                            let _ = tx.send(Msg::Notice(format!("renaming: {err}")));
                        }
                    }
                    Effect::ForgetMachine(host_id) => {
                        if let Err(err) = client.forget(host_id.clone()) {
                            let _ = tx.send(Msg::Notice(format!("forgetting: {err}")));
                        }
                    }
                    Effect::AttachTerminal { host_id, target } => attach = Some((host_id, target)),
                    Effect::Mouse(on) => modes.mouse = mouse::report(on).is_ok() && on,
                    Effect::Save => {
                        if let Err(err) = settings::save(&config_dir, Settings::of(&app)) {
                            let _ = tx.send(Msg::Notice(format!("saving the settings: {err}")));
                        }
                    }
                    Effect::Copy(text) => copy(&text),
                }
            }
            next = rx.try_recv().ok();
        }
        if let Some((host_id, target)) = attach.filter(|_| !quit) {
            // The attached program gets plain keys and no mouse reports, and sets its own
            // modes.
            disable_input_modes(modes);
            let ended = terminal::attach(&client, &host_id, target, &raw, &mut terminal).await;
            modes = enable_input_modes(app.mouse);
            app.update(Msg::TerminalEnded(ended));
        }
        if quit {
            break Ok(());
        }
        subscriptions.sync(&client, &app, &tx);
    };
    restore(modes);
    result
}

/// Draws the screen; with `full`, every cell anew, as one synchronized update where the
/// terminal supports that, so the cleared screen never shows.
fn paint(terminal: &mut backend::Tui, app: &mut App, full: bool) -> Result<()> {
    use ratatui::crossterm::execute;
    use ratatui::crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
    if full {
        execute!(std::io::stdout(), BeginSynchronizedUpdate)?;
    }
    let painted = views::paint(terminal, app, full);
    if full {
        execute!(std::io::stdout(), EndSynchronizedUpdate)?;
    }
    Ok(painted?)
}

/// The input modes [`enable_input_modes`] turned on.
#[derive(Clone, Copy, Debug)]
struct Modes {
    /// The keyboard enhancement was pushed.
    enhanced: bool,
    /// Mouse reporting is on.
    mouse: bool,
}

/// Turns off line wrap, so a symbol a terminal draws wider than counted cannot push the end
/// of a row onto the next; turns on bracketed paste, so a pasted prompt is one edit and not a
/// key per character; focus reports, so the screen is repainted when a phone app comes back to the front; where
/// the terminal supports it, disambiguated keys, so Shift-Enter is not Enter; and with
/// `mouse`, mouse reporting, so a phone's taps and swipes reach the TUI.
fn enable_input_modes(mouse: bool) -> Modes {
    use ratatui::crossterm::event::{
        EnableBracketedPaste, EnableFocusChange, KeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    };
    use ratatui::crossterm::{execute, terminal};
    let mut out = std::io::stdout();
    // All are conveniences: without them typing still works, only less well.
    let _ = execute!(
        out,
        terminal::DisableLineWrap,
        EnableBracketedPaste,
        EnableFocusChange
    );
    let enhanced = terminal::supports_keyboard_enhancement().unwrap_or(false)
        && execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .is_ok();
    Modes {
        enhanced,
        mouse: mouse && mouse::report(true).is_ok(),
    }
}

/// Undoes [`enable_input_modes`] and restores the terminal, blank: where the alternate
/// screen is missing, as under mosh, the shell's prompt then starts on an empty screen
/// rather than over the last frame.
fn restore(modes: Modes) {
    use ratatui::crossterm::{cursor, execute, terminal};
    disable_input_modes(modes);
    let _ = execute!(
        std::io::stdout(),
        terminal::Clear(terminal::ClearType::All),
        cursor::MoveTo(0, 0)
    );
    ratatui::restore();
}

/// Undoes [`enable_input_modes`].
fn disable_input_modes(modes: Modes) {
    use ratatui::crossterm::event::{
        DisableBracketedPaste, DisableFocusChange, PopKeyboardEnhancementFlags,
    };
    use ratatui::crossterm::execute;
    let mut out = std::io::stdout();
    if modes.mouse {
        let _ = mouse::report(false);
    }
    if modes.enhanced {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        out,
        DisableBracketedPaste,
        DisableFocusChange,
        ratatui::crossterm::terminal::EnableLineWrap
    );
}

/// Puts `text` on the clipboard with OSC 52, which terminals and SSH apps pass to the
/// device the user sits at; a terminal without it ignores the sequence.
fn copy(text: &str) {
    use base64::Engine as _;
    use std::io::Write as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]52;c;{encoded}\x07");
    let _ = out.flush();
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
            .send(host_id.clone(), command)
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
        let result = client
            .pair(link)
            .await
            .map(Box::new)
            .map_err(|err| err.to_string());
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
            if !changes.next().await {
                return;
            }
        }
    });
}

/// Reads terminal input on its own thread, as crossterm's read blocks: key and mouse events,
/// or raw bytes while a terminal is attached.
fn forward_input(tx: mpsc::UnboundedSender<Msg>, raw: terminal::RawInput) -> Result<()> {
    let wait = std::time::Duration::from_millis(50);
    std::thread::Builder::new()
        .name("herder-tui-input".to_owned())
        .spawn(move || {
            // The row a drag last reported: a drag within the row, as motion, costs no redraw.
            let mut drag_row = None;
            loop {
                if raw.forward(wait) {
                    continue;
                }
                match event::poll(wait) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(_) => return,
                }
                let msg = match event::read() {
                    Ok(Event::Key(key)) => Msg::Key(key),
                    Ok(Event::Resize(..)) => Msg::Resize,
                    Ok(Event::FocusGained) => Msg::Focus,
                    Ok(Event::Paste(text)) => Msg::Paste(text),
                    Ok(Event::Mouse(mouse)) => match mouse.kind {
                        MouseEventKind::Down(_) | MouseEventKind::Up(_) => {
                            drag_row = Some(mouse.row);
                            Msg::Mouse(mouse)
                        }
                        MouseEventKind::Drag(_) if drag_row != Some(mouse.row) => {
                            drag_row = Some(mouse.row);
                            Msg::Mouse(mouse)
                        }
                        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => Msg::Mouse(mouse),
                        _ => continue,
                    },
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
            let Ok(subscription) =
                client.subscribe_session(key.host_id.clone(), key.session_id.clone())
            else {
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
