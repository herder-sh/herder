//! Driving sessions from the TUI: the composer, approval and question answers, the command
//! palette, the new-session dialog, and what the daemon said back.
//!
//! Keys reach this module through [`for_key`] while an editor or overlay has them, and as
//! [`Act`]s from the shell's key map otherwise. Everything the daemon must do leaves as an
//! [`Effect::Send`]; its answer comes back as [`crate::app::Msg::Sent`].

use std::collections::HashMap;

use herder_protocol::{
    Answer, ApprovalDecision, CommandBody, CommandResult, HostId, PermissionMode, SessionStatus,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;
use ratatui_textarea::{TextArea, WrapMode};

use crate::action::Action;
use crate::app::{App, Effect, Focus};
use crate::prompt::Recall;
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
    /// Move the completion popup's selection down (1) or up (-1).
    Completion(i8),
    /// Accept the popup's completion.
    Complete,
    /// Hide the popup until the text changes.
    HidePopup,
    /// Walk the prompt history back (1) or forward (-1).
    Recall(i8),
    /// Move the approval panel's choice right (1) or left (-1).
    Button(i8),
    /// Answer the pending approval with the panel's choice.
    Confirm,
    /// Show the pending request at full height, or back at its usual.
    Full,
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
    pub palette: Option<crate::palette::Palette>,
    /// The new-session dialog, while open.
    pub dialog: Option<crate::new_session::NewSession>,
    /// The latest failed command of each session, shown in its view until the next command.
    pub errors: HashMap<SessionKey, String>,
    /// Ctrl-C was pressed with no turn to interrupt; a second press quits.
    pub quit_armed: bool,
    /// A created session to open once its machine lists it.
    pub pending_open: Option<SessionKey>,
    /// Prompts sent from here, oldest first, with the session each went to.
    pub history: Vec<(SessionKey, String)>,
    /// Where `↑` / `↓` are in the history.
    pub recall: Option<Recall>,
    /// The completion popup's selection.
    pub popup: usize,
    /// The text the popup was hidden for with Esc; it stays hidden until the text changes.
    pub popup_hidden: Option<String>,
    /// Collapsed pastes in the prompt: the placeholder, and the text it stands for.
    pub pastes: Vec<(String, String)>,
    /// The approval panel's choice: 0 allow, 1 deny.
    pub button: usize,
    /// Whether the pending request shows at full height.
    pub full: bool,
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
            history: Vec::new(),
            recall: None,
            popup: 0,
            popup_hidden: None,
            pastes: Vec::new(),
            button: 0,
            full: false,
        }
    }
}

impl Compose {
    /// Replaces the prompt's text, the cursor at its end.
    pub fn set_text(&mut self, text: &str) {
        self.editor = editor("Write a prompt…");
        self.editor.insert_str(text);
    }

    /// Empties the prompt.
    pub fn clear(&mut self) {
        self.editor = editor("Write a prompt…");
        self.pastes.clear();
        self.recall = None;
        self.popup = 0;
        self.popup_hidden = None;
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
        return Some(crate::new_session::for_key(key, dialog));
    }
    if let Some(palette) = &app.compose.palette {
        return Some(Some(crate::palette::for_key(key, palette)));
    }
    if app.focus != Focus::Composer {
        return None;
    }
    // A pending approval takes the prompt's place, and its keys.
    if app
        .open_session()
        .is_some_and(|session| !session.approvals.is_empty())
    {
        return match key.code {
            KeyCode::Char('y') => compose(Act::Approve(ApprovalDecision::Allow)),
            KeyCode::Char('n') => compose(Act::Approve(ApprovalDecision::Deny)),
            KeyCode::Left | KeyCode::Char('h') => compose(Act::Button(-1)),
            KeyCode::Right | KeyCode::Char('l') => compose(Act::Button(1)),
            KeyCode::Tab | KeyCode::BackTab if !plain => Some(None),
            KeyCode::Enter => compose(Act::Confirm),
            KeyCode::Char('f') => compose(Act::Full),
            KeyCode::Esc | KeyCode::Backspace => compose(Act::Leave),
            KeyCode::PageUp => Some(Some(Action::PageUp)),
            KeyCode::PageDown => Some(Some(Action::PageDown)),
            KeyCode::Tab | KeyCode::BackTab => Some(Some(Action::SwitchPane)),
            _ => Some(None),
        };
    }
    let popup = !app.completions().is_empty();
    let row = app.compose.editor.cursor().0;
    let last = app.compose.editor.lines().len().saturating_sub(1);
    // Digits pick a pending question's answer while nothing is typed.
    if app.compose.editor.is_empty()
        && let KeyCode::Char(digit @ '1'..='9') = key.code
        && plain
        && app
            .open_session()
            .and_then(|session| session.questions.first())
            .is_some_and(|question| !question.choices.is_empty())
    {
        return compose(Act::Choose(u32::from(digit) - u32::from('1')));
    }
    match key.code {
        KeyCode::Esc if popup => compose(Act::HidePopup),
        KeyCode::Up if popup => compose(Act::Completion(-1)),
        KeyCode::Down if popup => compose(Act::Completion(1)),
        KeyCode::Char('p') if popup && ctrl => compose(Act::Completion(-1)),
        KeyCode::Char('n') if popup && ctrl => compose(Act::Completion(1)),
        KeyCode::Tab if popup => compose(Act::Complete),
        KeyCode::Enter if popup && key.modifiers.is_empty() => compose(Act::Complete),
        KeyCode::Up if row == 0 => compose(Act::Recall(1)),
        KeyCode::Down if row == last && app.compose.recall.is_some() => compose(Act::Recall(-1)),
        KeyCode::Esc => compose(Act::Leave),
        // Leaves without Esc, which a phone keyboard may lack.
        KeyCode::Backspace if app.compose.editor.is_empty() => compose(Act::Leave),
        KeyCode::Enter if key.modifiers.is_empty() => compose(Act::Submit),
        KeyCode::Enter => compose(Act::Newline),
        KeyCode::Char('j') if ctrl => compose(Act::Newline),
        // The editor has no pages: they scroll the transcript above it.
        KeyCode::PageUp => Some(Some(Action::PageUp)),
        KeyCode::PageDown => Some(Some(Action::PageDown)),
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
                let writable = self.open.as_ref().is_some_and(|key| {
                    self.read_only(key).is_none()
                        && self
                            .sessions
                            .get(key)
                            .is_some_and(|s| s.status != SessionStatus::Archived)
                });
                if writable {
                    self.focus = Focus::Composer;
                }
            }
            Act::Key(key) => {
                self.edit(key);
                self.compose.popup = 0;
                self.compose.recall = None;
            }
            Act::Newline => self.compose.editor.insert_newline(),
            Act::Completion(step) => self.move_completion(isize::from(step)),
            Act::Complete => {
                if self.accept_completion() {
                    return self.submit();
                }
            }
            Act::HidePopup => {
                self.compose.popup_hidden = Some(self.compose.editor.lines().join("\n"));
            }
            Act::Recall(step) => {
                self.recall(isize::from(step));
            }
            Act::Button(step) => {
                self.compose.button = if step > 0 { 1 } else { 0 };
            }
            Act::Confirm => {
                let decision = if self.compose.button == 0 {
                    ApprovalDecision::Allow
                } else {
                    ApprovalDecision::Deny
                };
                return self.approve(decision);
            }
            Act::Full => self.compose.full = !self.compose.full,
            Act::Submit => return self.submit(),
            Act::Leave => {
                if self.compose.dialog.take().is_none() && self.compose.palette.take().is_none() {
                    self.focus = Focus::Transcript;
                }
            }
            Act::CtrlC => return self.ctrl_c(),
            Act::Approve(decision) => return self.approve(decision),
            Act::Choose(index) => return self.choose(index),
            Act::Palette => self.open_palette(),
            Act::NewSession => self.new_session(),
        }
        Vec::new()
    }

    /// Inserts pasted text into the editor that has the keys.
    pub(crate) fn paste(&mut self, text: &str) {
        if self.paste_new_session(text) {
            return;
        }
        if let Some(palette) = &mut self.compose.palette {
            palette.search.insert_str(text.replace(['\r', '\n'], " "));
            palette.selected = 0;
        } else if self.focus == Focus::Composer {
            self.paste_prompt(text);
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
        self.compose.editor.input(key);
    }

    fn submit(&mut self) -> Vec<Effect> {
        let Some(key) = self.open.clone() else {
            return Vec::new();
        };
        if !self.sessions.contains_key(&key) {
            return Vec::new();
        }
        let typed = text(&self.compose.editor);
        if typed.trim().is_empty() {
            return Vec::new();
        }
        // A `/command` runs; `//` sends a literal `/`.
        if typed.starts_with('/') && !typed.starts_with("//") {
            return match self.slash(Some(&key), &typed) {
                Ok(effects) => {
                    self.compose.clear();
                    self.compose.errors.remove(&key);
                    effects
                }
                Err(error) => {
                    self.compose.errors.insert(key, error);
                    Vec::new()
                }
            };
        }
        let typed = typed
            .strip_prefix('/')
            .filter(|_| typed.starts_with("//"))
            .unwrap_or(&typed);
        let prompt = self.expand_pastes(typed);
        self.compose.clear();
        let Some(session) = self.sessions.get_mut(&key) else {
            return Vec::new();
        };
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
        self.compose.history.push((key.clone(), prompt.clone()));
        let command = CommandBody::SendPrompt {
            session_id,
            text: prompt.clone(),
        };
        vec![send(&key, command, Origin::Prompt(key.clone(), prompt))]
    }

    fn ctrl_c(&mut self) -> Vec<Effect> {
        // Clears what is being written first.
        if self.focus == Focus::Composer && !self.compose.editor.is_empty() {
            self.compose.clear();
            return Vec::new();
        }
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
        self.compose.button = 0;
        self.compose.full = false;
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

    /// The command a palette line names, for the session it applies to.
    pub(crate) fn command(
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
        if app.focus == Focus::Composer {
            // Opening lands in the prompt: leave it for NAVIGATE.
            press(&mut app, KeyCode::Esc);
        }
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
        // The approval panel stands in for the composer; Backspace leaves it.
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
    fn the_palette_and_dialog_close_with_backspace() {
        let mut app = open_s2(vec![]);
        press(&mut app, KeyCode::Char(':'));
        press(&mut app, KeyCode::Backspace);
        assert!(app.compose.palette.is_none());

        // From the list, n opens the dialog; Backspace on its empty search closes it.
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('n'));
        assert!(app.compose.dialog.is_some());
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
        // Any other key disarms it; the next Ctrl-C clears what it typed.
        press(&mut app, KeyCode::Char('x'));
        assert!(!app.compose.quit_armed);
        assert_eq!(ctrl_c(&mut app), []);
        assert!(app.compose.editor.is_empty());
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
