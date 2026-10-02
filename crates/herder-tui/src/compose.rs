//! Driving sessions from the TUI: the composer, approval and question answers, the command
//! palette, the new-session dialog, and what the daemon said back.
//!
//! Keys reach this module through [`for_key`] while an editor or overlay has them, and as
//! [`Act`]s from the shell's key map otherwise. Everything the daemon must do leaves as an
//! [`Effect::Send`]; its answer comes back as [`crate::app::Msg::Sent`].

use std::collections::HashMap;

use herder_protocol::{
    Answer, ApprovalDecision, CommandBody, CommandResult, HostId, PermissionMode, ProjectId,
    SessionStatus,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;
use ratatui_textarea::{TextArea, WrapMode};

use crate::action::Action;
use crate::app::{App, Effect, Focus, Row};
use crate::session::{MODES, SessionKey, mode_name};

/// A user action of this module; see [`Action::Compose`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// Focus the composer of the open session.
    Write,
    /// A key for the editor that has them: composer, palette or dialog field.
    Key(KeyEvent),
    /// A line break in the composer.
    Newline,
    /// Send the composer, run the palette, or create the dialog's session.
    Submit,
    /// Close the overlay, or leave the composer.
    Leave,
    /// Ctrl-C: interrupt the open session's turn; when none runs, quit on the second press.
    CtrlC,
    /// Answer the open session's oldest pending approval.
    Approve(ApprovalDecision),
    /// Answer the open session's oldest pending question with a choice, from 0.
    Choose(u32),
    /// Open the command palette.
    Palette,
    /// Open the new-session dialog.
    NewSession,
    /// Move to the next (1) or previous (-1) dialog field.
    Field(i8),
    /// Change the dialog's focused choice to the next (1) or previous (-1) value.
    Cycle(i8),
}

/// What a command was sent for, so its answer lands in the right place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// A command on a session; a failure shows in its view.
    Session(SessionKey),
    /// A prompt; a failure also takes it off the queued list.
    Prompt(SessionKey, String),
    /// The new-session dialog's create.
    NewSession(HostId),
}

/// The composer and overlays, and what the daemon last said about each session.
#[derive(Debug)]
pub struct Compose {
    /// The open session's prompt being written.
    pub editor: TextArea<'static>,
    /// The command palette, while open.
    pub palette: Option<Palette>,
    /// The new-session dialog, while open.
    pub dialog: Option<NewSession>,
    /// The latest failed command of each session, shown in its view until the next command.
    pub errors: HashMap<SessionKey, String>,
    /// Ctrl-C was pressed with no turn to interrupt; a second press quits.
    pub quit_armed: bool,
    /// A created session to open once its machine lists it.
    pub pending_open: Option<SessionKey>,
}

impl Default for Compose {
    fn default() -> Self {
        Self {
            editor: editor("Write a prompt…"),
            palette: None,
            dialog: None,
            errors: HashMap::new(),
            quit_armed: false,
            pending_open: None,
        }
    }
}

/// The `:` command line.
#[derive(Debug)]
pub struct Palette {
    /// The command being typed.
    pub input: TextArea<'static>,
    /// Why the last command was not run.
    pub error: Option<String>,
    /// Session the command applies to.
    target: Option<SessionKey>,
}

/// The palette's commands, for its hint and the help.
pub const COMMANDS: &str = "model <name> · mode read_only|ask|auto_edit|full_access · archive[!] · interrupt · new · \
     down [project]";

/// The new-session dialog.
#[derive(Debug)]
pub struct NewSession {
    /// Project the session starts from; the machine field then picks among its clones.
    pub project: Option<ProjectId>,
    /// Machine to create on.
    pub host_id: HostId,
    /// Field with focus.
    pub field: Field,
    /// Repository path on the machine.
    pub repo: TextArea<'static>,
    /// Index of the account in the machine's accounts.
    pub account: usize,
    /// Model; empty for the provider's default.
    pub model: TextArea<'static>,
    /// Starting permission mode.
    pub mode: PermissionMode,
    /// Why the last create failed.
    pub error: Option<String>,
    /// A create is on its way.
    pub sending: bool,
}

/// A field of the new-session dialog, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    /// Which machine.
    Machine,
    /// Repository path.
    Repo,
    /// Which account.
    Account,
    /// Model.
    Model,
    /// Permission mode.
    Mode,
}

impl Field {
    const ALL: [Field; 5] = [
        Field::Machine,
        Field::Repo,
        Field::Account,
        Field::Model,
        Field::Mode,
    ];

    fn by(self, step: i8) -> Field {
        let at = Self::ALL.iter().position(|f| *f == self).unwrap_or(0);
        Self::ALL[cycle(at, Self::ALL.len(), step)]
    }

    /// Whether the field is typed into rather than picked.
    pub fn is_text(self) -> bool {
        matches!(self, Field::Repo | Field::Model)
    }
}

impl NewSession {
    /// Whether the focused field is a text field with nothing typed.
    fn field_is_empty(&self) -> bool {
        match self.field {
            Field::Repo => self.repo.is_empty(),
            Field::Model => self.model.is_empty(),
            _ => false,
        }
    }
}

/// `at` moved one place forward (`step` > 0) or back in a ring of `len`.
fn cycle(at: usize, len: usize, step: i8) -> usize {
    match len {
        0 => 0,
        _ if step < 0 => (at + len - 1) % len,
        _ => (at + 1) % len,
    }
}

/// A one-line editor with `placeholder`.
fn line(placeholder: &str) -> TextArea<'static> {
    let mut input = TextArea::default();
    input.set_cursor_line_style(Style::new());
    input.set_placeholder_text(placeholder);
    input
}

/// A wrapping multi-line editor with `placeholder`.
fn editor(placeholder: &str) -> TextArea<'static> {
    let mut input = line(placeholder);
    input.set_wrap_mode(WrapMode::WordOrGlyph);
    input
}

/// The text of an editor, lines joined.
fn text(input: &TextArea<'_>) -> String {
    input.lines().join("\n")
}

/// The action a key asks for while an overlay or the composer has the keys; `None` when they
/// do not, so the shell's key map applies.
pub fn for_key(key: KeyEvent, app: &App) -> Option<Option<Action>> {
    let compose = |act| Some(Some(Action::Compose(act)));
    let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if let Some(dialog) = &app.compose.dialog {
        // Without Tab or arrows, as on a phone: a choice field moves with j / k and changes
        // with h / l, Backspace on an empty text field goes back a field, and Backspace on a
        // choice closes the dialog.
        let text = dialog.field.is_text();
        return match key.code {
            KeyCode::Esc => compose(Act::Leave),
            KeyCode::Enter => compose(Act::Submit),
            KeyCode::Tab | KeyCode::Down => compose(Act::Field(1)),
            KeyCode::BackTab | KeyCode::Up => compose(Act::Field(-1)),
            KeyCode::Char('j') if !text => compose(Act::Field(1)),
            KeyCode::Char('k') if !text => compose(Act::Field(-1)),
            KeyCode::Left | KeyCode::Char('h') if !text => compose(Act::Cycle(-1)),
            KeyCode::Right | KeyCode::Char(' ' | 'l') if !text => compose(Act::Cycle(1)),
            KeyCode::Backspace if !text => compose(Act::Leave),
            KeyCode::Backspace if dialog.field_is_empty() => compose(Act::Field(-1)),
            _ if text => compose(Act::Key(key)),
            _ => Some(None),
        };
    }
    if let Some(palette) = &app.compose.palette {
        return match key.code {
            KeyCode::Esc => compose(Act::Leave),
            KeyCode::Backspace if palette.input.is_empty() => compose(Act::Leave),
            KeyCode::Enter => compose(Act::Submit),
            _ => compose(Act::Key(key)),
        };
    }
    if app.focus != Focus::Composer {
        return None;
    }
    match key.code {
        KeyCode::Esc => compose(Act::Leave),
        // Leaves without Esc, which a phone keyboard may lack.
        KeyCode::Backspace if app.compose.editor.is_empty() => compose(Act::Leave),
        KeyCode::Enter if key.modifiers.is_empty() => compose(Act::Submit),
        KeyCode::Enter => compose(Act::Newline),
        KeyCode::Char('j') if ctrl => compose(Act::Newline),
        KeyCode::Tab | KeyCode::BackTab if plain => Some(Some(Action::SwitchPane)),
        _ => compose(Act::Key(key)),
    }
}

impl App {
    /// Carries out one [`Act`].
    pub(crate) fn compose(&mut self, act: Act) -> Vec<Effect> {
        if act != Act::CtrlC {
            self.compose.quit_armed = false;
        }
        match act {
            Act::Write => {
                if self
                    .open_session()
                    .is_some_and(|s| s.status != SessionStatus::Archived)
                {
                    self.focus = Focus::Composer;
                }
            }
            Act::Key(key) => self.edit(key),
            Act::Newline => self.compose.editor.insert_newline(),
            Act::Submit => return self.submit(),
            Act::Leave => {
                if self.compose.dialog.take().is_none() && self.compose.palette.take().is_none() {
                    self.focus = Focus::Transcript;
                }
            }
            Act::CtrlC => return self.ctrl_c(),
            Act::Approve(decision) => return self.approve(decision),
            Act::Choose(index) => return self.choose(index),
            Act::Palette => {
                let target = match self.focus {
                    Focus::Sessions => self.selected().as_ref().and_then(Row::session).cloned(),
                    _ => self.open.clone(),
                };
                self.compose.palette = Some(Palette {
                    input: line(""),
                    error: None,
                    target,
                });
            }
            Act::NewSession => self.new_session(),
            Act::Field(step) => {
                if let Some(dialog) = &mut self.compose.dialog {
                    dialog.field = dialog.field.by(step);
                }
            }
            Act::Cycle(step) => self.cycle(step),
        }
        Vec::new()
    }

    /// Inserts pasted text into the editor that has the keys.
    pub(crate) fn paste(&mut self, text: &str) {
        if let Some(dialog) = &mut self.compose.dialog {
            let one_line = text.replace(['\r', '\n'], " ");
            match dialog.field {
                Field::Repo => dialog.repo.insert_str(one_line.trim()),
                Field::Model => dialog.model.insert_str(one_line.trim()),
                _ => false,
            };
        } else if let Some(palette) = &mut self.compose.palette {
            palette.input.insert_str(text.replace(['\r', '\n'], " "));
        } else if self.focus == Focus::Composer {
            self.compose.editor.insert_str(text.replace("\r\n", "\n"));
        }
    }

    /// Folds in the daemon's answer to a command sent for `origin`.
    pub(crate) fn sent(&mut self, origin: Origin, result: Result<CommandResult, String>) {
        match origin {
            Origin::Session(key) => self.session_result(key, result.err()),
            Origin::Prompt(key, text) => {
                if result.is_err()
                    && let Some(session) = self.sessions.get_mut(&key)
                    && let Some(at) = session.queued.iter().position(|q| *q == text)
                {
                    session.queued.remove(at);
                }
                self.session_result(key, result.err());
            }
            Origin::NewSession(host_id) => match result {
                Ok(CommandResult::SessionCreated { session_id }) => {
                    self.compose.dialog = None;
                    self.compose.pending_open = Some(SessionKey {
                        host_id,
                        session_id,
                    });
                    self.open_pending();
                }
                Ok(_) => self.compose.dialog = None,
                Err(error) => {
                    if let Some(dialog) = &mut self.compose.dialog {
                        dialog.error = Some(error);
                        dialog.sending = false;
                    }
                }
            },
        }
    }

    /// Opens the session created from the dialog once its machine lists it.
    pub(crate) fn open_pending(&mut self) {
        let Some(key) = &self.compose.pending_open else {
            return;
        };
        if !self.sessions.contains_key(key) {
            return;
        }
        let key = self.compose.pending_open.take();
        self.chosen = self
            .rows()
            .into_iter()
            .find(|row| row.session() == key.as_ref());
        self.open = key;
        self.scroll = Default::default();
        self.focus = Focus::Composer;
    }

    fn session_result(&mut self, key: SessionKey, error: Option<String>) {
        match error {
            Some(error) => {
                self.compose.errors.insert(key, error);
            }
            None => {
                self.compose.errors.remove(&key);
            }
        }
    }

    fn edit(&mut self, key: KeyEvent) {
        if let Some(dialog) = &mut self.compose.dialog {
            match dialog.field {
                Field::Repo => dialog.repo.input(key),
                Field::Model => dialog.model.input(key),
                _ => false,
            };
        } else if let Some(palette) = &mut self.compose.palette {
            palette.input.input(key);
        } else {
            self.compose.editor.input(key);
        }
    }

    fn submit(&mut self) -> Vec<Effect> {
        if self.compose.dialog.is_some() {
            return self.create();
        }
        if let Some(palette) = self.compose.palette.take() {
            return self.run(palette);
        }
        let Some(key) = self.open.clone() else {
            return Vec::new();
        };
        let Some(session) = self.sessions.get_mut(&key) else {
            return Vec::new();
        };
        let prompt = text(&self.compose.editor);
        if prompt.trim().is_empty() {
            return Vec::new();
        }
        self.compose.editor = editor("Write a prompt…");
        let session_id = session.id.clone();
        // A pending question takes the composer's text as its answer.
        if let Some(question) = session.questions.first() {
            let command = CommandBody::AnswerQuestion {
                session_id,
                question_id: question.id.clone(),
                answer: Answer::Text { text: prompt },
            };
            return vec![send(&key, command, Origin::Session(key.clone()))];
        }
        if session.turn.is_some() {
            session.queued.push(prompt.clone());
        }
        let command = CommandBody::SendPrompt {
            session_id,
            text: prompt.clone(),
        };
        vec![send(&key, command, Origin::Prompt(key.clone(), prompt))]
    }

    fn ctrl_c(&mut self) -> Vec<Effect> {
        if let Some(key) = &self.open
            && let Some(session) = self.sessions.get(key)
            && session.turn.is_some()
        {
            self.compose.quit_armed = false;
            let command = CommandBody::Interrupt {
                session_id: session.id.clone(),
            };
            return vec![send(key, command, Origin::Session(key.clone()))];
        }
        if self.compose.quit_armed {
            return vec![Effect::Quit];
        }
        self.compose.quit_armed = true;
        Vec::new()
    }

    fn approve(&mut self, decision: ApprovalDecision) -> Vec<Effect> {
        let Some((key, session)) = self.open.as_ref().zip(self.open_session()) else {
            return Vec::new();
        };
        let Some(approval) = session.approvals.first() else {
            return Vec::new();
        };
        let command = CommandBody::AnswerApproval {
            session_id: session.id.clone(),
            approval_id: approval.id.clone(),
            decision,
        };
        vec![send(key, command, Origin::Session(key.clone()))]
    }

    fn choose(&mut self, index: u32) -> Vec<Effect> {
        let Some((key, session)) = self.open.as_ref().zip(self.open_session()) else {
            return Vec::new();
        };
        let Some(question) = session.questions.first() else {
            return Vec::new();
        };
        if usize::try_from(index).map_or(true, |at| at >= question.choices.len()) {
            return Vec::new();
        }
        let command = CommandBody::AnswerQuestion {
            session_id: session.id.clone(),
            question_id: question.id.clone(),
            answer: Answer::Choice { index },
        };
        vec![send(key, command, Origin::Session(key.clone()))]
    }

    /// Runs a palette command, or reopens the palette with why it cannot.
    fn run(&mut self, mut palette: Palette) -> Vec<Effect> {
        let line = text(&palette.input);
        let mut words = line.split_whitespace();
        let Some(name) = words.next() else {
            return Vec::new();
        };
        let rest: Vec<&str> = words.collect();
        if name == "new" {
            self.new_session();
            return Vec::new();
        }
        match self.command(palette.target.as_ref(), name, &rest) {
            Ok((key, body)) => vec![send(&key, body, Origin::Session(key.clone()))],
            Err(error) => {
                palette.error = Some(error);
                self.compose.palette = Some(palette);
                Vec::new()
            }
        }
    }

    /// The command a palette line names, for the session it applies to.
    fn command(
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
            ("interrupt", []) => CommandBody::Interrupt { session_id },
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
            _ => return Err(format!("unknown command: {name}")),
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

    fn new_session(&mut self) {
        self.compose.palette = None;
        let selected = self.selected();
        let host_id = match &selected {
            Some(Row::Machine(host_id)) => Some(host_id.clone()),
            Some(Row::Session { key, .. }) => Some(key.host_id.clone()),
            Some(Row::Project(_)) | None => None,
        };
        let mut repo = line("/absolute/path/to/repo");
        // From a project, on the clone used last.
        let project = self.selected_project();
        if let Some(clone) = project.as_ref().and_then(|p| self.last_used_clone(p)) {
            repo.insert_str(&clone.repo);
            self.compose.dialog = Some(NewSession {
                project,
                host_id: clone.host_id,
                field: Field::Machine,
                repo,
                account: 0,
                model: line("provider default"),
                mode: PermissionMode::Ask,
                error: None,
                sending: false,
            });
            return;
        }
        let Some(host_id) = host_id.or_else(|| self.machines.first().map(|m| m.host_id.clone()))
        else {
            return;
        };
        let known = self
            .open
            .as_ref()
            .or(selected.as_ref().and_then(Row::session))
            .and_then(|key| self.sessions.get(key))
            .filter(|session| !session.repo.is_empty());
        if let Some(session) = known {
            repo.insert_str(&session.repo);
        }
        self.compose.dialog = Some(NewSession {
            project: None,
            host_id,
            field: Field::Repo,
            repo,
            account: 0,
            model: line("provider default"),
            mode: PermissionMode::Ask,
            error: None,
            sending: false,
        });
    }

    fn cycle(&mut self, step: i8) {
        let clones = self
            .compose
            .dialog
            .as_ref()
            .and_then(|dialog| dialog.project.as_ref())
            .map(|project| self.clones(project));
        let Some(dialog) = &mut self.compose.dialog else {
            return;
        };
        match dialog.field {
            Field::Machine if let Some(clones) = clones => {
                let at = clones
                    .iter()
                    .position(|c| c.host_id == dialog.host_id)
                    .unwrap_or(0);
                if let Some(clone) = clones.get(cycle(at, clones.len(), step)) {
                    dialog.host_id = clone.host_id.clone();
                    dialog.repo = line("/absolute/path/to/repo");
                    dialog.repo.insert_str(&clone.repo);
                    dialog.account = 0;
                }
            }
            Field::Machine => {
                let at = self
                    .machines
                    .iter()
                    .position(|m| m.host_id == dialog.host_id)
                    .unwrap_or(0);
                if let Some(machine) = self.machines.get(cycle(at, self.machines.len(), step)) {
                    dialog.host_id = machine.host_id.clone();
                    dialog.account = 0;
                }
            }
            Field::Account => {
                let accounts = self
                    .machines
                    .iter()
                    .find(|m| m.host_id == dialog.host_id)
                    .map_or(0, |m| m.accounts.len());
                dialog.account = cycle(dialog.account, accounts, step);
            }
            Field::Mode => {
                let at = MODES.iter().position(|m| *m == dialog.mode).unwrap_or(0);
                dialog.mode = MODES[cycle(at, MODES.len(), step)];
            }
            Field::Repo | Field::Model => {}
        }
    }

    fn create(&mut self) -> Vec<Effect> {
        let Some(dialog) = &mut self.compose.dialog else {
            return Vec::new();
        };
        if dialog.sending {
            return Vec::new();
        }
        let account = self
            .machines
            .iter()
            .find(|m| m.host_id == dialog.host_id)
            .and_then(|m| m.accounts.get(dialog.account));
        let repo = text(&dialog.repo).trim().to_owned();
        let model = text(&dialog.model).trim().to_owned();
        let Some(account) = account else {
            dialog.error = Some("this machine has no accounts".to_owned());
            return Vec::new();
        };
        if repo.is_empty() {
            dialog.error = Some("enter the repository's path".to_owned());
            dialog.field = Field::Repo;
            return Vec::new();
        }
        dialog.error = None;
        dialog.sending = true;
        let command = CommandBody::CreateSession {
            repo,
            branch: None,
            account_id: account.account_id.clone(),
            model: (!model.is_empty()).then_some(model),
            permission_mode: dialog.mode,
        };
        vec![Effect::Send {
            host_id: dialog.host_id.clone(),
            command,
            origin: Origin::NewSession(dialog.host_id.clone()),
        }]
    }
}

/// A command for a session's machine.
fn send(key: &SessionKey, command: CommandBody, origin: Origin) -> Effect {
    Effect::Send {
        host_id: key.host_id.clone(),
        command,
        origin,
    }
}

/// A permission mode as typed: its name, with `-` for `_` allowed.
fn parse_mode(name: &str) -> Option<PermissionMode> {
    let name = name.replace('-', "_");
    MODES.into_iter().find(|mode| mode_name(*mode) == name)
}

#[cfg(test)]
mod tests {
    use herder_protocol::{
        Answerer, ApprovalId, ApprovalOutcome, EventBody, ItemBody, QuestionId, SessionId, TurnId,
    };
    use ratatui::crossterm::event::KeyModifiers;

    use super::*;
    use crate::app::Msg;
    use crate::fake::{self, added, approval, key, question, started, type_text, update};

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn press_with(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Vec<Effect> {
        app.update(Msg::Key(KeyEvent::new(code, modifiers)))
    }

    fn on_s2(command: CommandBody) -> Effect {
        send(&key("h1", "s2"), command, Origin::Session(key("h1", "s2")))
    }

    /// [`fake::tree`] with `s2` open and `bodies` fed to it from seq 3.
    fn open_s2(bodies: Vec<EventBody>) -> App {
        let mut app = fake::tree();
        fake::feed(&mut app, "h1", "s2", update("s2", 3, bodies, Vec::new()));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.open, Some(key("h1", "s2")));
        app
    }

    #[test]
    fn the_composer_sends_a_prompt_and_queues_it_behind_a_running_turn() {
        let mut app = open_s2(vec![started("turn-1")]);
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(app.focus, Focus::Composer);
        // Keys type, even the shell's own: q does not quit.
        type_text(&mut app, "quick");
        press_with(&mut app, KeyCode::Enter, KeyModifiers::ALT);
        type_text(&mut app, "fix");
        press_with(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(app.compose.editor.lines(), ["quick", "fix", ""]);
        press(&mut app, KeyCode::Backspace);

        let prompt = "quick\nfix".to_owned();
        let effects = press(&mut app, KeyCode::Enter);
        assert_eq!(
            effects,
            [send(
                &key("h1", "s2"),
                CommandBody::SendPrompt {
                    session_id: SessionId::new("s2"),
                    text: prompt.clone(),
                },
                Origin::Prompt(key("h1", "s2"), prompt.clone()),
            )]
        );
        assert!(app.compose.editor.is_empty());
        let session = &app.sessions[&key("h1", "s2")];
        assert_eq!(session.queued, std::slice::from_ref(&prompt));

        // It leaves the queue once its turn starts with it.
        let message = added("i9", ItemBody::UserMessage { text: prompt });
        fake::feed(&mut app, "h1", "s2", update("s2", 4, vec![message], vec![]));
        assert!(app.sessions[&key("h1", "s2")].queued.is_empty());

        // An empty composer sends nothing; Esc leaves it.
        assert_eq!(press(&mut app, KeyCode::Enter), []);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Transcript);
    }

    #[test]
    fn backspace_on_an_empty_composer_leaves_it_so_y_answers_without_esc() {
        let mut app = open_s2(vec![started("turn-1"), approval("a1", "Bash: ls")]);
        press(&mut app, KeyCode::Char('i'));
        type_text(&mut app, "y");
        press(&mut app, KeyCode::Backspace);
        // The first Backspace erases, the next leaves.
        assert_eq!(app.focus, Focus::Composer);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.focus, Focus::Transcript);
        let effects = press(&mut app, KeyCode::Char('y'));
        assert!(
            matches!(
                effects.as_slice(),
                [Effect::Send {
                    command: CommandBody::AnswerApproval { .. },
                    ..
                }]
            ),
            "{effects:?}"
        );
    }

    #[test]
    fn the_palette_and_dialog_close_with_backspace_and_move_with_letters() {
        let mut app = open_s2(vec![]);
        press(&mut app, KeyCode::Char(':'));
        press(&mut app, KeyCode::Backspace);
        assert!(app.compose.palette.is_none());

        // In the transcript n denies; from the list it opens the dialog.
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Char('n'));
        let field = |app: &App| app.compose.dialog.as_ref().map(|d| d.field);
        // The repo comes filled in from the open session.
        assert_eq!(field(&app), Some(Field::Repo));
        press(&mut app, KeyCode::Up);
        assert_eq!(field(&app), Some(Field::Machine));
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(field(&app), Some(Field::Mode));
        press(&mut app, KeyCode::Char('k'));
        // In a text field, letters type; Backspace on it empty goes back a field.
        type_text(&mut app, "jk");
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(field(&app), Some(Field::Model));
        press(&mut app, KeyCode::Backspace);
        assert_eq!(field(&app), Some(Field::Account));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(field(&app), Some(Field::Model));
        press(&mut app, KeyCode::Down);
        assert_eq!(field(&app), Some(Field::Mode));
        let mode = |app: &App| app.compose.dialog.as_ref().map(|d| d.mode);
        let before = mode(&app);
        press(&mut app, KeyCode::Char('l'));
        assert_ne!(mode(&app), before);
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(mode(&app), before);
        press(&mut app, KeyCode::Backspace);
        assert!(app.compose.dialog.is_none());
    }

    #[test]
    fn a_prompt_to_an_idle_session_is_not_queued_and_a_refused_one_shows_why() {
        let mut app = open_s2(vec![]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus, Focus::Composer);
        type_text(&mut app, "hello");
        let effects = press(&mut app, KeyCode::Enter);
        assert_eq!(effects.len(), 1);
        assert!(app.sessions[&key("h1", "s2")].queued.is_empty());

        let origin = Origin::Prompt(key("h1", "s2"), "hello".into());
        app.update(Msg::Sent {
            origin: origin.clone(),
            result: Err("the session is archived and read-only".into()),
        });
        assert_eq!(
            app.compose.errors[&key("h1", "s2")],
            "the session is archived and read-only"
        );
        app.update(Msg::Sent {
            origin,
            result: Ok(CommandResult::Applied),
        });
        assert!(app.compose.errors.is_empty());
    }

    #[test]
    fn a_refused_queued_prompt_leaves_the_queue() {
        let mut app = open_s2(vec![started("turn-1")]);
        press(&mut app, KeyCode::Char('i'));
        type_text(&mut app, "next");
        press(&mut app, KeyCode::Enter);
        app.update(Msg::Sent {
            origin: Origin::Prompt(key("h1", "s2"), "next".into()),
            result: Err("gone".into()),
        });
        assert!(app.sessions[&key("h1", "s2")].queued.is_empty());
    }

    #[test]
    fn y_and_n_answer_the_oldest_pending_approval() {
        let mut app = open_s2(vec![
            started("turn-1"),
            approval("a1", "Bash: rm -rf target"),
            approval("a2", "Edit src/lib.rs"),
        ]);
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            [on_s2(CommandBody::AnswerApproval {
                session_id: SessionId::new("s2"),
                approval_id: ApprovalId::new("a1"),
                decision: ApprovalDecision::Allow,
            })]
        );
        let resolved = EventBody::ApprovalResolved {
            approval_id: ApprovalId::new("a1"),
            decision: ApprovalOutcome::Allow,
            answered_by: Answerer::User,
        };
        fake::feed(
            &mut app,
            "h1",
            "s2",
            update("s2", 6, vec![resolved], vec![]),
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('n')),
            [on_s2(CommandBody::AnswerApproval {
                session_id: SessionId::new("s2"),
                approval_id: ApprovalId::new("a2"),
                decision: ApprovalDecision::Deny,
            })]
        );
        // In the session list, y and n are not answers.
        press(&mut app, KeyCode::Esc);
        assert_eq!(press(&mut app, KeyCode::Char('y')), []);
    }

    #[test]
    fn a_question_takes_a_choice_or_the_composer_text_and_clears_with_its_turn() {
        let mut app = open_s2(vec![
            started("turn-1"),
            question("q1", "Which database?", &["SQLite", "Postgres"]),
        ]);
        // Only listed choices are answers.
        assert_eq!(press(&mut app, KeyCode::Char('3')), []);
        let answer = |answer| {
            on_s2(CommandBody::AnswerQuestion {
                session_id: SessionId::new("s2"),
                question_id: QuestionId::new("q1"),
                answer,
            })
        };
        assert_eq!(
            press(&mut app, KeyCode::Char('2')),
            [answer(Answer::Choice { index: 1 })]
        );
        press(&mut app, KeyCode::Char('i'));
        type_text(&mut app, "neither");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            [answer(Answer::Text {
                text: "neither".into()
            })]
        );

        let ended = EventBody::TurnCompleted {
            turn_id: TurnId::new("turn-1"),
        };
        fake::feed(&mut app, "h1", "s2", update("s2", 5, vec![ended], vec![]));
        let session = &app.sessions[&key("h1", "s2")];
        assert!(session.questions.is_empty());
        assert_eq!(session.turn, None);
    }

    #[test]
    fn ctrl_c_interrupts_a_running_turn_and_quits_on_a_second_idle_press() {
        let ctrl_c = |app: &mut App| press_with(app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        let mut app = open_s2(vec![started("turn-1")]);
        press(&mut app, KeyCode::Char('i'));
        let interrupt = on_s2(CommandBody::Interrupt {
            session_id: SessionId::new("s2"),
        });
        assert_eq!(ctrl_c(&mut app), std::slice::from_ref(&interrupt));
        assert_eq!(ctrl_c(&mut app), [interrupt]);

        let ended = EventBody::TurnInterrupted {
            turn_id: TurnId::new("turn-1"),
        };
        fake::feed(&mut app, "h1", "s2", update("s2", 4, vec![ended], vec![]));
        assert_eq!(ctrl_c(&mut app), []);
        assert!(app.compose.quit_armed);
        // Any other key disarms it.
        press(&mut app, KeyCode::Char('x'));
        assert!(!app.compose.quit_armed);
        assert_eq!(ctrl_c(&mut app), []);
        assert_eq!(ctrl_c(&mut app), [Effect::Quit]);
    }

    #[test]
    fn down_brings_down_the_sessions_compose_project() {
        let mut app = open_s2(vec![]);
        let run = |app: &mut App, line: &str| {
            press(app, KeyCode::Char(':'));
            type_text(app, line);
            press(app, KeyCode::Enter)
        };
        assert_eq!(run(&mut app, "down"), []);
        let palette = app.compose.palette.take().unwrap();
        assert_eq!(
            palette.error.as_deref(),
            Some("the session has no compose containers")
        );

        fake::with_resources(&mut app, fake::host_resources(1), true);
        let down = |project: &str| {
            [on_s2(CommandBody::ComposeDown {
                session_id: SessionId::new("s2"),
                project: project.into(),
            })]
        };
        assert_eq!(run(&mut app, "down"), down("app"));
        assert_eq!(run(&mut app, "down web"), down("web"));

        // With several projects, the user names one.
        let mut machines = app.machines.clone();
        let usage = machines[0]
            .session_usage
            .get_mut(&SessionId::new("s2"))
            .unwrap();
        usage.containers[1].compose_project = Some("cache".into());
        app.update(Msg::Machines(machines));
        assert_eq!(run(&mut app, "down"), []);
        let palette = app.compose.palette.as_ref().unwrap();
        assert_eq!(palette.error.as_deref(), Some("usage: down app|cache"));
    }

    #[test]
    fn the_palette_switches_model_and_mode_and_archives() {
        let mut app = open_s2(vec![]);
        let run = |app: &mut App, line: &str| {
            press(app, KeyCode::Char(':'));
            type_text(app, line);
            press(app, KeyCode::Enter)
        };
        let s2 = || SessionId::new("s2");
        assert_eq!(
            run(&mut app, "model claude-sonnet"),
            [on_s2(CommandBody::SetModel {
                session_id: s2(),
                model: "claude-sonnet".into()
            })]
        );
        assert!(app.compose.palette.is_none());
        assert_eq!(
            run(&mut app, "mode auto-edit"),
            [on_s2(CommandBody::SetPermissionMode {
                session_id: s2(),
                mode: PermissionMode::AutoEdit
            })]
        );
        assert_eq!(
            run(&mut app, "archive!"),
            [on_s2(CommandBody::ArchiveSession {
                session_id: s2(),
                force: true
            })]
        );

        // A bad command keeps the palette open with why.
        assert_eq!(run(&mut app, "mode yolo"), []);
        let palette = app.compose.palette.as_ref().unwrap();
        assert_eq!(palette.error.as_deref(), Some("unknown mode yolo"));
        press(&mut app, KeyCode::Esc);
        assert!(app.compose.palette.is_none());
        assert_eq!(app.focus, Focus::Transcript);

        // From the session list, the palette acts on the selected session.
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(
            run(&mut app, "interrupt"),
            [send(
                &key("h1", "s1"),
                CommandBody::Interrupt {
                    session_id: SessionId::new("s1")
                },
                Origin::Session(key("h1", "s1"))
            )]
        );
    }

    #[test]
    fn the_dialog_creates_a_session_and_opens_it_once_listed() {
        let mut app = fake::tree();
        let mut machines = app.machines.clone();
        machines[0].accounts = vec![
            fake::account("claude-main", "Main"),
            fake::account("claude-work", "Work"),
        ];
        app.update(Msg::Machines(machines.clone()));

        press(&mut app, KeyCode::Char('n'));
        let dialog = app.compose.dialog.as_ref().unwrap();
        assert_eq!(dialog.field, Field::Repo);
        assert_eq!(dialog.repo.lines(), ["/home/ann/src/app"]);

        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Tab);
        type_text(&mut app, "claude-opus");
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Left);
        let effects = press(&mut app, KeyCode::Enter);
        assert_eq!(
            effects,
            [Effect::Send {
                host_id: HostId::new("h1"),
                command: CommandBody::CreateSession {
                    repo: "/home/ann/src/app".into(),
                    branch: None,
                    account_id: herder_protocol::AccountId::new("claude-work"),
                    model: Some("claude-opus".into()),
                    permission_mode: PermissionMode::ReadOnly,
                },
                origin: Origin::NewSession(HostId::new("h1")),
            }]
        );
        // A second Enter while it is on its way sends nothing.
        assert_eq!(press(&mut app, KeyCode::Enter), []);

        app.update(Msg::Sent {
            origin: Origin::NewSession(HostId::new("h1")),
            result: Err("not a git repository".into()),
        });
        let dialog = app.compose.dialog.as_ref().unwrap();
        assert_eq!(dialog.error.as_deref(), Some("not a git repository"));
        assert!(!dialog.sending);

        press(&mut app, KeyCode::Enter);
        app.update(Msg::Sent {
            origin: Origin::NewSession(HostId::new("h1")),
            result: Ok(CommandResult::SessionCreated {
                session_id: SessionId::new("s5"),
            }),
        });
        assert!(app.compose.dialog.is_none());
        assert_eq!(app.open, None);
        machines[0] = fake::machine("h1", "box", &["s1", "s2", "s3", "s4", "s5"]);
        app.update(Msg::Machines(machines));
        assert_eq!(app.open, Some(key("h1", "s5")));
        assert_eq!(app.focus, Focus::Composer);
    }

    #[test]
    fn the_dialog_needs_an_account_and_a_repo() {
        let mut app = fake::tree();
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(press(&mut app, KeyCode::Enter), []);
        let dialog = app.compose.dialog.as_ref().unwrap();
        assert_eq!(
            dialog.error.as_deref(),
            Some("this machine has no accounts")
        );
        press(&mut app, KeyCode::Esc);
        assert!(app.compose.dialog.is_none());
    }

    #[test]
    fn a_paste_is_one_edit_and_never_sends() {
        let mut app = open_s2(vec![]);
        press(&mut app, KeyCode::Char('i'));
        app.update(Msg::Paste("line one\r\nline two".into()));
        assert_eq!(app.compose.editor.lines(), ["line one", "line two"]);
        // Outside an editor a paste does nothing.
        press(&mut app, KeyCode::Esc);
        app.update(Msg::Paste("ignored".into()));
        assert_eq!(app.compose.editor.lines(), ["line one", "line two"]);
    }
}
