//! The command palette (`ctrl+p`, `:`) and the prompt's `/` commands: one list of
//! [`COMMANDS`], filtered as the search is typed, each with the key that does it too.
//!
//! Enter runs the command under the cursor. A command that takes an argument completes to
//! `name ` first, so the argument is typed after it; the line typed is then run as the old
//! `:` palette ran it, so `model opus`, `mode ask` and `archive!` work as they always did.

use herder_protocol::{CommandBody, PermissionMode};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui_textarea::TextArea;

use crate::action::Action;
use crate::app::{App, Effect, Focus, Row};
use crate::compose::{Act, Origin};
use crate::inbox::InboxAction;
use crate::prs::{self, PrAction};
use crate::session::{MODES, SessionKey, mode_name};
use crate::ui::glyphs::Glyphs;

/// What running a command does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Run {
    /// This action; the command takes no argument.
    Act(Action),
    /// A command on the target session, as [`App::session_command`] builds it.
    Session,
    /// Link a PR: by the argument, else through the link prompt.
    Link,
    /// Mouse reporting on or off.
    Mouse,
    /// The glyph set.
    Glyphs,
}

/// A command of the palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Command {
    /// What is typed: `model`.
    pub name: &'static str,
    /// Other names typed for it: the old palette's.
    pub aliases: &'static [&'static str],
    /// Its argument, `<required>` or `[optional]`; empty for none.
    pub args: &'static str,
    /// What it does.
    pub title: &'static str,
    /// The key that does it outside the palette; empty for none.
    pub key: &'static str,
    /// The group it is listed under.
    pub group: &'static str,
    /// What running it does.
    pub run: Run,
}

impl Command {
    /// Whether it cannot run without an argument.
    pub fn needs_args(&self) -> bool {
        self.args.starts_with('<')
    }

    /// What the search matches: its names and title.
    fn text(&self) -> String {
        let mut text = self.name.to_owned();
        for alias in self.aliases {
            text.push(' ');
            text.push_str(alias);
        }
        text.push(' ');
        text.push_str(self.title);
        text
    }
}

const fn command(
    name: &'static str,
    args: &'static str,
    title: &'static str,
    key: &'static str,
    group: &'static str,
    run: Run,
) -> Command {
    Command {
        name,
        aliases: &[],
        args,
        title,
        key,
        group,
        run,
    }
}

const SESSION: &str = "session";
const VIEWS: &str = "go to";
const MACHINES: &str = "machines";
const DISPLAY: &str = "display";
const HERDER: &str = "herder";

/// Every command, by group.
pub const COMMANDS: &[Command] = &[
    command(
        "new",
        "",
        "new session",
        "n",
        SESSION,
        Run::Act(Action::Compose(Act::NewSession)),
    ),
    command(
        "switch",
        "",
        "switch account, provider or model",
        "s",
        SESSION,
        Run::Act(Action::OpenSwitch),
    ),
    command(
        "model",
        "<name>",
        "change the model",
        "",
        SESSION,
        Run::Session,
    ),
    command(
        "mode",
        "<mode>",
        "permission mode: read_only ask auto_edit full_access",
        "",
        SESSION,
        Run::Session,
    ),
    Command {
        aliases: &["interrupt"],
        ..command(
            "stop",
            "",
            "stop the running turn",
            "ctrl+c",
            SESSION,
            Run::Session,
        )
    },
    command(
        "archive",
        "",
        "archive the session",
        "",
        SESSION,
        Run::Session,
    ),
    command(
        "archive!",
        "",
        "archive, discarding its worktree changes",
        "",
        SESSION,
        Run::Session,
    ),
    command(
        "pr",
        "[n|url]",
        "link a pull request",
        "L",
        SESSION,
        Run::Link,
    ),
    command(
        "term",
        "",
        "terminals (owners)",
        "t",
        SESSION,
        Run::Act(Action::Terminals),
    ),
    command(
        "down",
        "[project]",
        "bring the session's compose project down",
        "",
        SESSION,
        Run::Session,
    ),
    command(
        "recover",
        "",
        "recover a session whose host is offline",
        "R",
        SESSION,
        Run::Act(Action::OpenRecover),
    ),
    command(
        "inbox",
        "",
        "everything waiting on you",
        "I",
        VIEWS,
        Run::Act(Action::Inbox(InboxAction::Toggle)),
    ),
    command(
        "prs",
        "",
        "every session's pull requests",
        "P",
        VIEWS,
        Run::Act(Action::Pr(PrAction::ToggleAll)),
    ),
    command(
        "accounts",
        "",
        "accounts and their usage",
        "A",
        VIEWS,
        Run::Act(Action::OpenAccounts),
    ),
    command(
        "fleet",
        "",
        "machines and their connections",
        "m",
        VIEWS,
        Run::Act(Action::OpenMachines),
    ),
    command(
        "add",
        "",
        "add a machine",
        "a",
        MACHINES,
        Run::Act(Action::AddMachine),
    ),
    command(
        "reconnect",
        "",
        "reconnect every machine now",
        "r",
        MACHINES,
        Run::Act(Action::Reconnect),
    ),
    command(
        "group",
        "",
        "group sessions by project or by machine",
        "v",
        DISPLAY,
        Run::Act(Action::Group),
    ),
    command(
        "glyphs",
        "<ascii|unicode>",
        "plain marks for phones and mosh, or symbols",
        "",
        DISPLAY,
        Run::Glyphs,
    ),
    command(
        "mouse",
        "<on|off>",
        "taps and the wheel, or the terminal's selection",
        "",
        DISPLAY,
        Run::Mouse,
    ),
    command(
        "help",
        "",
        "keys",
        "?",
        HERDER,
        Run::Act(Action::ToggleHelp),
    ),
    command(
        "quit",
        "",
        "quit herder",
        "q",
        HERDER,
        Run::Act(Action::Quit),
    ),
];

/// The command `name` names, by its name or an alias.
pub fn find(name: &str) -> Option<&'static Command> {
    COMMANDS
        .iter()
        .find(|command| command.name == name || command.aliases.contains(&name))
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
        let named = |command: &Command| {
            command.name.starts_with(query) || command.aliases.iter().any(|a| a.starts_with(query))
        };
        let mut out: Vec<_> = COMMANDS
            .iter()
            .filter(|c| named(c))
            .map(|c| (None, c))
            .collect();
        out.extend(
            crate::fuzzy::filter(query, COMMANDS, Command::text)
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
    let mut group = "";
    for command in COMMANDS {
        let header = (command.group != group).then_some(command.group);
        group = command.group;
        out.push((header, command));
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
            .filter(|command| line.contains(' ') || !command.needs_args());
        let line = match typed {
            Some(_) => line,
            None => {
                let Some((_, command)) = self.palette_matches().get(self.palette_cursor()).copied()
                else {
                    return Vec::new();
                };
                if command.needs_args() {
                    self.complete(command);
                    return Vec::new();
                }
                command.name.to_owned()
            }
        };
        let palette = self.compose.palette.take();
        match self.run_line(target, &line) {
            Ok(effects) => effects,
            Err(error) => {
                self.compose.palette = palette.map(|mut palette| {
                    palette.search = search_line(&line, "");
                    palette.error = Some(error);
                    palette
                });
                Vec::new()
            }
        }
    }

    /// Runs a command line, `name args…`, for `target`; why not, when it cannot.
    pub(crate) fn run_line(
        &mut self,
        target: Option<SessionKey>,
        line: &str,
    ) -> Result<Vec<Effect>, String> {
        let mut words = line.split_whitespace();
        let Some(name) = words.next() else {
            return Ok(Vec::new());
        };
        let rest: Vec<&str> = words.collect();
        let Some(command) = find(name) else {
            return Err(format!("unknown command: {name}"));
        };
        let usage = || format!("usage: {} {}", command.name, command.args);
        match command.run {
            Run::Act(action) if rest.is_empty() => Ok(self.act(action)),
            Run::Act(_) => Err(usage()),
            Run::Mouse => {
                let on = match rest.as_slice() {
                    ["on"] => true,
                    ["off"] => false,
                    _ => return Err("usage: mouse on|off".to_owned()),
                };
                self.mouse = on;
                Ok(vec![Effect::Mouse(on), Effect::Save])
            }
            Run::Glyphs => {
                let Some(glyphs) = rest.first().and_then(|name| Glyphs::parse(name)) else {
                    return Err("usage: glyphs unicode|ascii".to_owned());
                };
                self.glyphs = Some(glyphs);
                Ok(vec![Effect::Save])
            }
            Run::Link => match rest.as_slice() {
                [] => Ok(self.act(Action::Pr(PrAction::StartLink))),
                [pr] => {
                    let Some(key) = target else {
                        return Err("no session selected".to_owned());
                    };
                    let Some(number) = prs::parse_number(pr) else {
                        return Err("usage: pr <number or URL>".to_owned());
                    };
                    let command = CommandBody::LinkPr {
                        session_id: key.session_id.clone(),
                        number,
                    };
                    Ok(vec![Effect::Send {
                        host_id: key.host_id.clone(),
                        command,
                        origin: Origin::Session(key),
                    }])
                }
                _ => Err("usage: pr <number or URL>".to_owned()),
            },
            Run::Session => {
                let (key, body) = self.session_command(target.as_ref(), command.name, &rest)?;
                Ok(vec![Effect::Send {
                    host_id: key.host_id.clone(),
                    command: body,
                    origin: Origin::Session(key),
                }])
            }
        }
    }

    /// The command a line names for the session it applies to.
    fn session_command(
        &self,
        target: Option<&SessionKey>,
        name: &str,
        args: &[&str],
    ) -> Result<(SessionKey, CommandBody), String> {
        let Some((key, session)) = target.and_then(|key| Some(key).zip(self.sessions.get(key)))
        else {
            return Err("no session selected".to_owned());
        };
        let session_id = session.id.clone();
        let body = match (name, args) {
            ("model", [model]) => CommandBody::SetModel {
                session_id,
                model: (*model).to_owned(),
            },
            ("model", _) => return Err("usage: model <name>".to_owned()),
            ("mode", [mode]) => match parse_mode(mode) {
                Some(mode) => CommandBody::SetPermissionMode { session_id, mode },
                None => return Err(format!("unknown mode {mode}")),
            },
            ("mode", _) => return Err("usage: mode read_only|ask|auto_edit|full_access".into()),
            ("archive" | "archive!", []) => CommandBody::ArchiveSession {
                session_id,
                force: name == "archive!",
            },
            ("stop", []) => CommandBody::Interrupt { session_id },
            ("down", []) => match self.compose_projects(key).as_slice() {
                [project] => CommandBody::ComposeDown {
                    session_id,
                    project: project.clone(),
                },
                [] => return Err("the session has no compose containers".to_owned()),
                projects => return Err(format!("usage: down {}", projects.join("|"))),
            },
            ("down", [project]) => CommandBody::ComposeDown {
                session_id,
                project: (*project).to_owned(),
            },
            ("down", _) => return Err("usage: down [project]".to_owned()),
            _ => return Err(format!("usage: {name}")),
        };
        Ok((key.clone(), body))
    }

    /// The Compose projects of a session's tracked containers, sorted.
    fn compose_projects(&self, key: &SessionKey) -> Vec<String> {
        let mut projects: Vec<String> = self
            .machines
            .iter()
            .filter(|machine| machine.host_id == key.host_id)
            .filter_map(|machine| machine.session_usage.get(&key.session_id))
            .flat_map(|usage| &usage.containers)
            .filter_map(|container| container.compose_project.clone())
            .collect();
        projects.sort_unstable();
        projects.dedup();
        projects
    }
}

/// A permission mode as typed: its name, with `-` for `_` allowed.
fn parse_mode(name: &str) -> Option<PermissionMode> {
    let name = name.replace('-', "_");
    MODES.into_iter().find(|mode| mode_name(*mode) == name)
}
