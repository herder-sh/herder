//! Driving sessions from the TUI: the composer, approval and question answers, the command
//! palette, the new-session dialog, and what the daemon said back.
//!
//! Keys reach this module through [`for_key`] while an editor or overlay has them, and as
//! [`Act`]s from the shell's key map otherwise. Everything the daemon must do leaves as an
//! [`Effect::Send`]; its answer comes back as [`crate::app::Msg::Sent`].

use std::collections::HashMap;

use herder_protocol::{
    Answer, ApprovalDecision, AttachmentId, CommandBody, CommandResult, HostId,
    IMAGE_NOT_BACKED_UP, Image, MAX_PROMPT_IMAGE_BYTES, PermissionMode, SessionStatus,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;
use ratatui_textarea::{TextArea, WrapMode};

use crate::action::Action;
use crate::app::{App, Effect, Focus};
use crate::attach::{self, Source};
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
    /// Ctrl-V: attach the clipboard's image to the prompt.
    PasteImage,
    /// Take the prompt's last image off.
    Unattach,
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
    /// A step of backing a machine up to a vault.
    Backup(crate::backup::Sent),
    /// Fetching a transcript's image: which one, its file name, and whether to open it or
    /// only save it.
    Image {
        key: SessionKey,
        attachment_id: AttachmentId,
        name: String,
        open: bool,
    },
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
    /// The images the prompt carries, in the order they were attached.
    pub images: Vec<Image>,
    /// Images being loaded.
    pub loading: usize,
    /// Send the prompt once the images being loaded are in.
    pub send_loaded: bool,
    /// The folder the `@` popup lists, as typed, with its entries.
    pub listing: Option<(String, Vec<attach::Entry>)>,
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
            images: Vec::new(),
            loading: 0,
            send_loaded: false,
            listing: None,
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
        self.images.clear();
        self.send_loaded = false;
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
        KeyCode::Char('v') if ctrl => compose(Act::PasteImage),
        // Takes the last image off, then leaves without Esc, which a phone keyboard may lack.
        KeyCode::Backspace if app.compose.editor.is_empty() && !app.compose.images.is_empty() => {
            compose(Act::Unattach)
        }
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
                return self.list_wanted();
            }
            Act::Newline => self.compose.editor.insert_newline(),
            Act::Completion(step) => self.move_completion(isize::from(step)),
            Act::Complete => return self.accept_completion(),
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
            Act::PasteImage => return self.attach(Source::Clipboard, None),
            Act::Unattach => {
                self.compose.images.pop();
            }
        }
        Vec::new()
    }

    /// Loads an image for the prompt, named by `word` of its text if any.
    pub(crate) fn attach(&mut self, source: Source, word: Option<String>) -> Vec<Effect> {
        self.compose.loading += 1;
        vec![Effect::Attach { source, word }]
    }

    /// Folds in an image loaded for the prompt: it joins the prompt and the `@path` word that
    /// named it leaves the text; the prompt goes out if it waited on it. Why it could not
    /// load shows under the prompt.
    pub(crate) fn attached(
        &mut self,
        word: Option<String>,
        result: Result<Image, String>,
    ) -> Vec<Effect> {
        self.compose.loading = self.compose.loading.saturating_sub(1);
        let carried: usize = self.compose.images.iter().map(|i| i.data.0.len()).sum();
        let result = result.and_then(|image| {
            if carried + image.data.0.len() > MAX_PROMPT_IMAGE_BYTES {
                Err(format!(
                    "over {} of images in one prompt",
                    attach::size(MAX_PROMPT_IMAGE_BYTES as u64)
                ))
            } else {
                Ok(image)
            }
        });
        let error = match result {
            Ok(image) => {
                self.compose.images.push(image);
                if let Some(word) = word {
                    self.unword(&word);
                }
                None
            }
            Err(error) => Some(error),
        };
        if let Some(key) = self.open.clone() {
            match error {
                Some(error) => {
                    self.compose.send_loaded = false;
                    self.compose.errors.insert(key, error);
                }
                None => {
                    self.compose.errors.remove(&key);
                }
            }
        }
        if self.compose.loading == 0 && std::mem::take(&mut self.compose.send_loaded) {
            return self.submit();
        }
        Vec::new()
    }

    /// Takes the word `word` out of the prompt's text, with a space next to it.
    fn unword(&mut self, word: &str) {
        let text = text(&self.compose.editor);
        let Some(at) = text.match_indices(word).map(|(at, _)| at).find(|&at| {
            let end = at + word.len();
            text[..at]
                .chars()
                .next_back()
                .is_none_or(char::is_whitespace)
                && text[end..].chars().next().is_none_or(char::is_whitespace)
        }) else {
            return;
        };
        let mut end = at + word.len();
        let mut start = at;
        if text[end..].starts_with(' ') {
            end += 1;
        } else if text[..start].ends_with(' ') {
            start -= 1;
        }
        let pastes = std::mem::take(&mut self.compose.pastes);
        let images = std::mem::take(&mut self.compose.images);
        self.compose
            .set_text(&format!("{}{}", &text[..start], &text[end..]));
        self.compose.pastes = pastes;
        self.compose.images = images;
    }

    /// The `@path` words of `text` that name images, each once.
    fn image_words(text: &str) -> Vec<String> {
        let mut words: Vec<String> = Vec::new();
        for word in text.split_whitespace() {
            if let Some(path) = word.strip_prefix('@')
                && attach::is_image_path(path)
                && !words.iter().any(|known| known == word)
            {
                words.push(word.to_owned());
            }
        }
        words
    }

    /// Inserts pasted text into the editor that has the keys. In the prompt, dropped image
    /// files attach; so does the clipboard's image when a terminal pastes it as nothing.
    pub(crate) fn paste(&mut self, text: &str) -> Vec<Effect> {
        if self.paste_new_session(text) {
            return Vec::new();
        }
        if let Some(palette) = &mut self.compose.palette {
            palette.search.insert_str(text.replace(['\r', '\n'], " "));
            palette.selected = 0;
        } else if self.focus == Focus::Composer {
            if text.is_empty() {
                return self.attach(Source::Clipboard, None);
            }
            if let Some(paths) = attach::dropped_paths(text) {
                return paths
                    .into_iter()
                    .flat_map(|path| self.attach(Source::File(path), None))
                    .collect();
            }
            self.paste_prompt(text);
            return self.list_wanted();
        }
        Vec::new()
    }

    /// Folds in the daemon's answer to a command sent for `origin`.
    pub(crate) fn sent(
        &mut self,
        origin: Origin,
        result: Result<CommandResult, String>,
    ) -> Vec<Effect> {
        match origin {
            Origin::Image {
                key,
                attachment_id,
                name,
                open,
            } => match result {
                Ok(CommandResult::Attachment { data, .. }) => {
                    return vec![Effect::Image {
                        name,
                        data: data.0,
                        open,
                    }];
                }
                Ok(_) => self.session_result(key, Some("the machine sent no image".to_owned())),
                // Never backed up: its chip says so from now on.
                Err(error) if error.starts_with(IMAGE_NOT_BACKED_UP) => {
                    if let Some(session) = self.sessions.get_mut(&key) {
                        session.not_backed_up.insert(attachment_id);
                    }
                    self.notice = Some(IMAGE_NOT_BACKED_UP.to_owned());
                }
                Err(error) => self.session_result(key, Some(error)),
            },
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
            Origin::Backup(sent) => return self.backup_sent(sent, result),
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
        Vec::new()
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

    pub(crate) fn submit(&mut self) -> Vec<Effect> {
        let Some(key) = self.open.clone() else {
            return Vec::new();
        };
        if !self.sessions.contains_key(&key) {
            return Vec::new();
        }
        let typed = text(&self.compose.editor);
        if typed.trim().is_empty() {
            // Providers take no image without a word of text.
            if !self.compose.images.is_empty() {
                let error = "write a line to go with the image".to_owned();
                self.compose.errors.insert(key, error);
            }
            return Vec::new();
        }
        // The prompt waits for its images: the ones loading, and those its `@path`s name.
        if self.compose.loading > 0 {
            self.compose.send_loaded = true;
            return Vec::new();
        }
        let words = Self::image_words(&typed);
        if !words.is_empty() {
            self.compose.send_loaded = true;
            return words
                .into_iter()
                .flat_map(|word| {
                    let path = word.trim_start_matches('@').to_owned();
                    self.attach(Source::File(path), Some(word))
                })
                .collect();
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
        let images = std::mem::take(&mut self.compose.images);
        self.compose.clear();
        let Some(session) = self.sessions.get_mut(&key) else {
            return Vec::new();
        };
        let session_id = session.id.clone();
        // A pending question takes the composer's text as its answer.
        if let Some(question) = session.questions.first() {
            // An answer is text: the images stay for the next prompt.
            self.compose.images = images;
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
            images,
        };
        vec![send(&key, command, Origin::Prompt(key.clone(), prompt))]
    }

    fn ctrl_c(&mut self) -> Vec<Effect> {
        // Clears what is being written first.
        if self.focus == Focus::Composer
            && (!self.compose.editor.is_empty() || !self.compose.images.is_empty())
        {
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
            ("unarchive", []) => CommandBody::UnarchiveSession { session_id },
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

    fn image(bytes: usize) -> Image {
        Image {
            media_type: "image/png".into(),
            data: herder_protocol::Bytes(vec![0; bytes]),
        }
    }

    /// `s2` open, writing in its prompt.
    fn writing() -> App {
        let mut app = open_s2(vec![]);
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(app.focus, Focus::Composer);
        app
    }

    fn prompt(text: &str, images: Vec<Image>) -> Effect {
        send(
            &key("h1", "s2"),
            CommandBody::SendPrompt {
                session_id: SessionId::new("s2"),
                text: text.into(),
                images,
            },
            Origin::Prompt(key("h1", "s2"), text.into()),
        )
    }

    fn error(app: &App) -> Option<&str> {
        app.compose.errors.get(&key("h1", "s2")).map(String::as_str)
    }

    #[test]
    fn ctrl_v_attaches_the_clipboards_image_and_the_prompt_carries_it() {
        let mut app = writing();
        let effects = press_with(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL);
        assert_eq!(
            effects,
            [Effect::Attach {
                source: Source::Clipboard,
                word: None,
            }]
        );
        assert_eq!(app.compose.loading, 1);
        type_text(&mut app, "what is off here?");
        // Sent before the image is in, the prompt waits for it.
        assert_eq!(press(&mut app, KeyCode::Enter), []);
        let effects = app.update(Msg::Attached {
            word: None,
            result: Ok(image(10)),
        });
        assert_eq!(effects, [prompt("what is off here?", vec![image(10)])]);
        assert!(app.compose.images.is_empty());
        assert_eq!(app.compose.loading, 0);

        // A terminal that pastes an image as nothing attaches it too.
        let effects = app.update(Msg::Paste(String::new()));
        assert!(
            matches!(
                &effects[..],
                [Effect::Attach {
                    source: Source::Clipboard,
                    ..
                }]
            ),
            "{effects:?}"
        );
        // Without a clipboard, as over SSH, it says what to do instead.
        let reason = "no clipboard here; attach a file with @path";
        app.update(Msg::Attached {
            word: None,
            result: Err(reason.into()),
        });
        assert_eq!(error(&app), Some(reason));
        assert!(app.compose.images.is_empty());
    }

    #[test]
    fn an_at_path_to_an_image_attaches_before_the_prompt_goes() {
        let mut app = writing();
        type_text(&mut app, "compare @shots/a.png with @b.PNG please");
        let effects = press(&mut app, KeyCode::Enter);
        let attach = |path: &str| Effect::Attach {
            source: Source::File(path.into()),
            word: Some(format!("@{path}")),
        };
        assert_eq!(effects, [attach("shots/a.png"), attach("b.PNG")]);
        let loaded = |app: &mut App, word: &str, bytes| {
            app.update(Msg::Attached {
                word: Some(word.into()),
                result: Ok(image(bytes)),
            })
        };
        assert_eq!(loaded(&mut app, "@shots/a.png", 1), []);
        assert_eq!(text(&app.compose.editor), "compare with @b.PNG please");
        let effects = loaded(&mut app, "@b.PNG", 2);
        assert_eq!(
            effects,
            [prompt("compare with please", vec![image(1), image(2)])]
        );
    }

    #[test]
    fn an_image_that_cannot_attach_says_why_and_keeps_the_prompt() {
        let mut app = writing();
        type_text(&mut app, "see @gone.png");
        press(&mut app, KeyCode::Enter);
        let effects = app.update(Msg::Attached {
            word: Some("@gone.png".into()),
            result: Err("gone.png: no such file".into()),
        });
        assert_eq!(effects, []);
        assert_eq!(error(&app), Some("gone.png: no such file"));
        assert_eq!(text(&app.compose.editor), "see @gone.png");
        assert!(!app.compose.send_loaded);
    }

    #[test]
    fn a_prompts_images_stay_under_the_cap_and_need_a_line_of_text() {
        let mut app = writing();
        let six_mb = 6 * 1024 * 1024;
        for _ in 0..2 {
            app.compose(Act::PasteImage);
            app.update(Msg::Attached {
                word: None,
                result: Ok(image(six_mb)),
            });
        }
        assert_eq!(app.compose.images.len(), 1);
        assert_eq!(error(&app), Some("over 10 MB of images in one prompt"));
        // An image alone is not sent: providers want a word with it.
        assert_eq!(press(&mut app, KeyCode::Enter), []);
        assert_eq!(error(&app), Some("write a line to go with the image"));
        // Backspace on the empty prompt takes the image off, then leaves.
        press(&mut app, KeyCode::Backspace);
        assert!(app.compose.images.is_empty());
        assert_eq!(app.focus, Focus::Composer);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.focus, Focus::Transcript);
    }

    #[test]
    fn dropped_image_files_attach_and_other_paths_paste_as_text() {
        let mut app = writing();
        let effects = app.update(Msg::Paste("'/home/ann/Screen Shot.png' /tmp/b.jpg".into()));
        let file = |path: &str| Effect::Attach {
            source: Source::File(path.into()),
            word: None,
        };
        assert_eq!(
            effects,
            [file("/home/ann/Screen Shot.png"), file("/tmp/b.jpg")]
        );
        assert_eq!(app.compose.loading, 2);
        app.update(Msg::Paste("/tmp/notes.txt".into()));
        assert_eq!(text(&app.compose.editor), "/tmp/notes.txt");
    }

    #[test]
    fn at_completes_folders_and_images_and_picking_an_image_attaches_it() {
        let mut app = writing();
        type_text(&mut app, "look ");
        let effects = press(&mut app, KeyCode::Char('@'));
        assert_eq!(effects, [Effect::List(String::new())]);
        let entry = |name: &str, is_dir| crate::attach::Entry {
            name: name.into(),
            is_dir,
        };
        app.update(Msg::Listed {
            dir: String::new(),
            entries: vec![
                entry(".git", true),
                entry("docs", true),
                entry("shot.png", false),
            ],
        });
        type_text(&mut app, "s");
        let labels: Vec<String> = app.completions().into_iter().map(|c| c.label).collect();
        // The task children by branch, then this machine's files: names starting `s` first.
        assert_eq!(
            labels,
            [
                "@herder/api-docs",
                "@herder/api-tests",
                "@shot.png",
                "@docs/"
            ]
        );
        // A folder completes and lists.
        app.compose.popup = 3;
        let effects = press(&mut app, KeyCode::Tab);
        assert_eq!(effects, [Effect::List("docs/".into())]);
        assert_eq!(text(&app.compose.editor), "look @docs/");
        app.update(Msg::Listed {
            dir: "docs/".into(),
            entries: vec![entry("mock.webp", false)],
        });
        // An image attaches in place of its word.
        let effects = press(&mut app, KeyCode::Enter);
        assert_eq!(
            effects,
            [Effect::Attach {
                source: Source::File("docs/mock.webp".into()),
                word: None,
            }]
        );
        assert_eq!(text(&app.compose.editor), "look ");
    }

    #[test]
    fn o_and_w_fetch_a_prompts_images_to_open_or_save() {
        let attachment = |id: &str, media_type: &str| herder_protocol::Attachment {
            attachment_id: herder_protocol::AttachmentId::new(id),
            media_type: media_type.into(),
            size: 10,
        };
        let message = |id: &str, attachments| {
            added(
                id,
                ItemBody::UserMessage {
                    text: "this".into(),
                    attachments,
                },
            )
        };
        let mut app = open_s2(vec![
            message("i1", vec![attachment("01A", "image/png")]),
            message("i2", vec![attachment("01B", "image/jpeg")]),
            message("i3", Vec::new()),
        ]);
        assert_eq!(app.focus, Focus::Transcript);
        let fetch = |id: &str, name: &str, open| Effect::Send {
            host_id: key("h1", "s2").host_id,
            command: CommandBody::GetAttachment {
                session_id: SessionId::new("s2"),
                attachment_id: herder_protocol::AttachmentId::new(id),
            },
            origin: Origin::Image {
                key: key("h1", "s2"),
                attachment_id: herder_protocol::AttachmentId::new(id),
                name: name.into(),
                open,
            },
        };
        // Without a cursor, the latest prompt with images.
        let effects = press(&mut app, KeyCode::Char('o'));
        assert_eq!(effects, [fetch("01B", "herder-01B.jpg", true)]);
        // The image arrives and opens.
        let origin = |id: &str, name: &str| Origin::Image {
            key: key("h1", "s2"),
            attachment_id: herder_protocol::AttachmentId::new(id),
            name: name.into(),
            open: true,
        };
        let effects = app.update(Msg::Sent {
            origin: origin("01B", "herder-01B.jpg"),
            result: Ok(CommandResult::Attachment {
                media_type: "image/jpeg".into(),
                data: herder_protocol::Bytes(vec![1, 2]),
            }),
        });
        assert_eq!(
            effects,
            [Effect::Image {
                name: "herder-01B.jpg".into(),
                data: vec![1, 2],
                open: true,
            }]
        );
        // With one, the prompt under it; w only saves.
        app.chat.cursor = Some(herder_protocol::ItemId::new("i1"));
        let effects = press(&mut app, KeyCode::Char('w'));
        assert_eq!(effects, [fetch("01A", "herder-01A.png", false)]);
        app.chat.cursor = Some(herder_protocol::ItemId::new("i3"));
        assert_eq!(press(&mut app, KeyCode::Char('o')), []);
        assert_eq!(app.notice.as_deref(), Some("no images here"));

        // One the vault never got, in a recovered session: said so, and marked on its chip.
        let effects = app.update(Msg::Sent {
            origin: origin("01A", "herder-01A.png"),
            result: Err(format!(
                "{IMAGE_NOT_BACKED_UP}: the vault holds no image 01A of session s2"
            )),
        });
        assert_eq!(effects, []);
        assert_eq!(app.notice.as_deref(), Some(IMAGE_NOT_BACKED_UP));
        let session = &app.sessions[&key("h1", "s2")];
        assert!(
            session
                .not_backed_up
                .contains(&herder_protocol::AttachmentId::new("01A"))
        );
        assert!(!app.compose.errors.contains_key(&key("h1", "s2")));
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
                    images: Vec::new(),
                },
                Origin::Prompt(key("h1", "s2"), prompt.clone()),
            )]
        );
        assert!(app.compose.editor.is_empty());
        let session = &app.sessions[&key("h1", "s2")];
        assert_eq!(session.queued, std::slice::from_ref(&prompt));

        // It leaves the queue once its turn starts with it.
        let message = added(
            "i9",
            ItemBody::UserMessage {
                text: prompt,
                attachments: Vec::new(),
            },
        );
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
        assert_eq!(
            run(&mut app, "unarchive"),
            [on_s2(CommandBody::UnarchiveSession { session_id: s2() })]
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
