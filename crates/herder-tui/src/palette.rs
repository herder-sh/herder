//! The command palette (`ctrl+p`, `:`): the prompt's `/` commands ([`crate::prompt::COMMANDS`])
//! in a dialog, filtered as the search is typed, each with the key that does it too.
//!
//! Enter runs the command under the cursor. A command that takes an argument completes to
//! `name ` first, so the argument is typed after it; the line typed then runs as the prompt
//! runs `/name args`, so `model opus`, `mode ask` and `archive!` work as the old `:` palette's
//! words did. Commands on a session apply to the selected one in the session list, else the
//! open one.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui_textarea::TextArea;

pub use crate::prompt::Command;

use crate::action::Action;
use crate::app::{App, Effect, Focus, Row};
use crate::prompt::COMMANDS;
use crate::session::SessionKey;

/// The key that does `command` outside the palette; empty for none.
pub fn key_of(command: &Command) -> &'static str {
    match command.name {
        "new" => "n",
        "switch" => "s",
        "stop" => "ctrl+c",
        "pr" => "L",
        "term" => "t",
        "fork" => "F",
        "rename" => "E",
        "retitle" => "R",
        "inbox" => "I",
        "prs" => "P",
        "accounts" => "A",
        "fleet" => "m",
        "add" => "a",
        "reconnect" => "r",
        "group" => "v",
        "help" => "?",
        "quit" => "q",
        _ => "",
    }
}

/// The group `command` is listed under.
fn group_of(command: &Command) -> &'static str {
    match command.name {
        "inbox" | "prs" | "accounts" | "fleet" => "go to",
        "add" | "reconnect" => "machines",
        "thinking" | "details" | "glyphs" | "mouse" | "group" => "display",
        "help" | "quit" => "herder",
        _ => "session",
    }
}

/// The groups, in the order the palette lists them.
const GROUPS: [&str; 5] = ["session", "go to", "machines", "display", "herder"];

/// Whether `command` cannot run without an argument.
pub fn needs_args(command: &Command) -> bool {
    !command.args.is_empty() && !command.args.starts_with('[')
}

/// What the search matches: its name and what it does.
fn text(command: &Command) -> String {
    format!("{} {}", command.name, command.does)
}

/// The command `name` names; the old palette's `interrupt` and `archive!` too.
pub fn find(name: &str) -> Option<&'static Command> {
    let name = match name {
        "interrupt" => "stop",
        "archive!" => "archive",
        name => name,
    };
    COMMANDS.iter().find(|command| command.name == name)
}

/// The commands a search keeps, with the group heading each starts, best match first; with
/// no search, `suggested` first, then every command by group. A search with an argument
/// keeps the command it names.
pub fn matches(
    query: &str,
    suggested: &[&'static str],
) -> Vec<(Option<&'static str>, &'static Command)> {
    let query = query.trim_start();
    if let Some((name, _)) = query.split_once(' ')
        && let Some(command) = find(name)
    {
        return vec![(None, command)];
    }
    if !query.trim().is_empty() {
        // Names the query starts first, as typed names are what the old palette took; then
        // the rest, best match first.
        let query = query.trim();
        let named = |command: &Command| command.name.starts_with(query);
        let mut out: Vec<_> = COMMANDS
            .iter()
            .filter(|c| named(c))
            .map(|c| (None, c))
            .collect();
        out.extend(
            crate::fuzzy::filter(query, COMMANDS, text)
                .into_iter()
                .map(|at| &COMMANDS[at])
                .filter(|command| !named(command))
                .map(|command| (None, command)),
        );
        return out;
    }
    let mut out = Vec::new();
    for (at, name) in suggested.iter().filter_map(|name| find(name)).enumerate() {
        out.push(((at == 0).then_some("suggested"), name));
    }
    for group in GROUPS {
        let mut first = true;
        for command in COMMANDS.iter().filter(|c| group_of(c) == group) {
            out.push((first.then_some(group), command));
            first = false;
        }
    }
    out
}

/// The command palette.
#[derive(Debug)]
pub struct Palette {
    /// The search, or the command line being typed.
    pub search: TextArea<'static>,
    /// The cursor, by index into [`App::palette_matches`].
    pub selected: usize,
    /// The list's scroll, kept between draws.
    pub offset: usize,
    /// Why the last command was not run.
    pub error: Option<String>,
    /// The session commands apply to.
    pub target: Option<SessionKey>,
}

/// Input to the palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// Close it.
    Close,
    /// Move the cursor by this many commands.
    Move(isize),
    /// The cursor to the first command.
    Top,
    /// The cursor to the last command.
    Bottom,
    /// Complete the search to the command under the cursor.
    Complete,
    /// Run the command under the cursor, or the line typed.
    Submit,
    /// A key for the search.
    Key(KeyEvent),
    /// Row `at`: a tap moves the cursor there, or runs the command already under it.
    Pick(usize),
}

/// The action a key asks for while the palette is open.
pub fn for_key(key: KeyEvent, palette: &Palette) -> Action {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let input = match key.code {
        KeyCode::Esc => Input::Close,
        KeyCode::Backspace if palette.search.is_empty() => Input::Close,
        KeyCode::Enter => Input::Submit,
        KeyCode::Tab => Input::Complete,
        KeyCode::Up => Input::Move(-1),
        KeyCode::Down => Input::Move(1),
        KeyCode::Char('p' | 'k') if ctrl => Input::Move(-1),
        KeyCode::Char('n' | 'j') if ctrl => Input::Move(1),
        KeyCode::PageUp => Input::Move(-10),
        KeyCode::PageDown => Input::Move(10),
        KeyCode::Home => Input::Top,
        KeyCode::End => Input::Bottom,
        _ => Input::Key(key),
    };
    Action::Palette(input)
}

/// A one-line editor for a search.
pub fn search_line(text: &str, placeholder: &str) -> TextArea<'static> {
    let mut input = TextArea::from([text.to_owned()]);
    input.set_placeholder_text(placeholder);
    input.move_cursor(ratatui_textarea::CursorMove::End);
    input
}

impl App {
    /// The session the palette's commands apply to: the selected one in the session list,
    /// else the open one.
    fn palette_target(&self) -> Option<SessionKey> {
        match self.focus {
            Focus::Sessions => self.selected().as_ref().and_then(Row::session).cloned(),
            _ => self.open.clone(),
        }
    }

    /// Opens the palette.
    pub(crate) fn open_palette(&mut self) {
        self.compose.palette = Some(Palette {
            search: search_line("", "type a command"),
            selected: 0,
            offset: 0,
            error: None,
            target: self.palette_target(),
        });
    }

    /// Opens the palette on a `rename` line holding the target session's name, to edit and
    /// run; nothing without a session.
    pub(crate) fn open_rename(&mut self) {
        let Some(target) = self.palette_target() else {
            return;
        };
        let name = self
            .sessions
            .get(&target)
            .map(|session| session.name(false))
            .unwrap_or_default();
        self.open_palette();
        if let Some(palette) = &mut self.compose.palette {
            palette.search = search_line(&format!("rename {name}"), "");
        }
    }

    /// Asks AI to title the selected or open session again.
    pub(crate) fn retitle(&mut self) -> Vec<Effect> {
        let target = self.palette_target();
        self.slash(target.as_ref(), "retitle").unwrap_or_default()
    }

    /// What the palette suggests first: stopping a running turn, switching, a new session,
    /// and the inbox while something waits.
    pub fn suggested(&self) -> Vec<&'static str> {
        let target = self
            .compose
            .palette
            .as_ref()
            .and_then(|palette| palette.target.clone())
            .or_else(|| self.open.clone());
        let session = target.as_ref().and_then(|key| self.sessions.get(key));
        let mut out = Vec::new();
        if session.is_some_and(|session| session.turn.is_some()) {
            out.push("stop");
        }
        if session.is_some() {
            out.push("switch");
        }
        out.push("new");
        if !self.waiting().is_empty() {
            out.push("inbox");
        }
        out
    }

    /// The commands the palette lists now.
    pub fn palette_matches(&self) -> Vec<(Option<&'static str>, &'static Command)> {
        let Some(palette) = &self.compose.palette else {
            return Vec::new();
        };
        let query = palette.search.lines().join(" ");
        matches(&query, &self.suggested())
    }

    /// Carries out one input to the palette.
    pub(crate) fn palette_input(&mut self, input: Input) -> Vec<Effect> {
        let count = self.palette_matches().len();
        let Some(palette) = &mut self.compose.palette else {
            return Vec::new();
        };
        let last = count.saturating_sub(1);
        match input {
            Input::Close => self.compose.palette = None,
            Input::Move(by) => {
                palette.selected = palette
                    .selected
                    .min(last)
                    .saturating_add_signed(by)
                    .min(last);
            }
            Input::Top => palette.selected = 0,
            Input::Bottom => palette.selected = last,
            Input::Key(key) => {
                if palette.search.input(key) {
                    palette.selected = 0;
                    palette.error = None;
                }
            }
            Input::Complete => {
                if let Some((_, command)) = self.palette_matches().get(self.palette_cursor()) {
                    self.complete(command);
                }
            }
            Input::Submit => return self.palette_submit(),
            Input::Pick(at) if at == palette.selected.min(last) => return self.palette_submit(),
            Input::Pick(at) => palette.selected = at.min(last),
        }
        Vec::new()
    }

    /// The palette's cursor, within its matches.
    fn palette_cursor(&self) -> usize {
        let count = self.palette_matches().len();
        self.compose
            .palette
            .as_ref()
            .map_or(0, |palette| palette.selected.min(count.saturating_sub(1)))
    }

    /// Fills the search with `command`'s name, ready for its argument.
    fn complete(&mut self, command: &Command) {
        if let Some(palette) = &mut self.compose.palette {
            let text = if command.args.is_empty() {
                command.name.to_owned()
            } else {
                format!("{} ", command.name)
            };
            palette.search = search_line(&text, "");
            palette.selected = 0;
            palette.error = None;
        }
    }

    /// Runs what the palette has: the line typed when it names a command with its
    /// argument, else the command under the cursor, completing one that needs an argument.
    fn palette_submit(&mut self) -> Vec<Effect> {
        let Some(palette) = &self.compose.palette else {
            return Vec::new();
        };
        let line = palette.search.lines().join(" ").trim().to_owned();
        let target = palette.target.clone();
        let typed = line
            .split_whitespace()
            .next()
            .and_then(find)
            .filter(|command| line.contains(' ') || !needs_args(command));
        let line = match typed {
            Some(_) => line,
            None => {
                let Some((_, command)) = self.palette_matches().get(self.palette_cursor()).copied()
                else {
                    return Vec::new();
                };
                if needs_args(command) {
                    self.complete(command);
                    return Vec::new();
                }
                command.name.to_owned()
            }
        };
        let palette = self.compose.palette.take();
        // The old palette's `interrupt` is the prompt's `stop`.
        let run = if line == "interrupt" {
            "stop".to_owned()
        } else {
            line.clone()
        };
        match self.slash(target.as_ref(), &run) {
            Ok(effects) => effects,
            Err(error) => {
                // The palette's lines have no `/`.
                let error = error.replacen("usage: /", "usage: ", 1);
                self.compose.palette = palette.map(|mut palette| {
                    palette.search = search_line(&line, "");
                    palette.error = Some(error);
                    palette
                });
                Vec::new()
            }
        }
    }
}
