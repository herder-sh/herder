//! The new-session dialog: pick the project, then the machine (one of the project's clones),
//! then the account, model and mode, and create.
//!
//! The first two steps are pickers, filtered as their search is typed; "a repository by path"
//! at the end of the projects lists every machine and leaves the path to type. Opened on a
//! project in the session list, the dialog starts at its machines, on the clone used last.
//! Backspace on an empty search, or on a choice, goes back a step; Esc closes.

use herder_protocol::{CommandBody, HostId, PermissionMode, ProjectId};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui_textarea::TextArea;

use crate::action::Action;
use crate::app::{App, Effect, Row};
use crate::compose::Origin;
use crate::palette::search_line;
use crate::session::MODES;

/// Where the dialog stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Picking the project.
    Project,
    /// Picking the machine, or the project's clone on one.
    Machine,
    /// The account, model and mode.
    Form,
}

/// A field of the form, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
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
    pub const ALL: [Field; 4] = [Field::Repo, Field::Account, Field::Model, Field::Mode];

    fn by(self, step: i8) -> Field {
        let at = Self::ALL.iter().position(|f| *f == self).unwrap_or(0);
        Self::ALL[cycle(at, Self::ALL.len(), step)]
    }

    /// Whether the field is typed into rather than picked.
    pub fn is_text(self) -> bool {
        matches!(self, Field::Repo | Field::Model)
    }

    /// Its label.
    pub fn label(self) -> &'static str {
        match self {
            Field::Repo => "repo",
            Field::Account => "account",
            Field::Model => "model",
            Field::Mode => "mode",
        }
    }
}

/// A row of the first two steps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// A project.
    Project(ProjectId),
    /// No project: any machine, the path typed.
    ByPath,
    /// A machine, with the path of the project's clone there, if any.
    Machine(HostId, Option<String>),
}

/// The new-session dialog.
#[derive(Debug)]
pub struct NewSession {
    /// Where it stands.
    pub step: Step,
    /// The search of the step's picker.
    pub search: TextArea<'static>,
    /// The cursor, by index into [`App::new_session_choices`].
    pub selected: usize,
    /// The picker's scroll, kept between draws.
    pub offset: usize,
    /// The project picked; `None` for a repository by path.
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

impl NewSession {
    /// Whether the focused field is a text field with nothing typed.
    fn field_is_empty(&self) -> bool {
        match self.field {
            Field::Repo => self.repo.is_empty(),
            Field::Model => self.model.is_empty(),
            _ => false,
        }
    }

    /// The step's picker afresh, its cursor at `selected`.
    fn go_to(&mut self, step: Step, selected: usize) {
        self.step = step;
        self.search = search_line("", "type to filter");
        self.selected = selected;
        self.offset = 0;
        self.error = None;
    }
}

/// Input to the dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// Close it.
    Close,
    /// Back a step.
    Back,
    /// Move the picker's cursor by this many rows.
    Move(isize),
    /// The cursor to the first row.
    Top,
    /// The cursor to the last row.
    Bottom,
    /// Pick the row under the cursor, or create.
    Submit,
    /// A key for the search or the focused text field.
    Key(KeyEvent),
    /// Focus the next (1) or previous (-1) field.
    Field(i8),
    /// Focus a field.
    Focus(Field),
    /// Change the focused choice to the next (1) or previous (-1) value.
    Cycle(i8),
    /// Focus a choice and change it: a tapped arrow.
    Choose(Field, i8),
    /// Pick row `at` of the picker: a tapped row.
    Pick(usize),
}

/// The action a key asks for while the dialog is open.
pub fn for_key(key: KeyEvent, dialog: &NewSession) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let input = if dialog.step == Step::Form {
        // Without Tab or arrows, as on a phone: a choice moves with j / k and changes with
        // h / l, Backspace on an empty text field goes back a field, and on a choice a step.
        let text = dialog.field.is_text();
        match key.code {
            KeyCode::Esc => Input::Close,
            KeyCode::Enter => Input::Submit,
            KeyCode::Tab | KeyCode::Down => Input::Field(1),
            KeyCode::BackTab | KeyCode::Up => Input::Field(-1),
            KeyCode::Char('j') if !text => Input::Field(1),
            KeyCode::Char('k') if !text => Input::Field(-1),
            KeyCode::Left | KeyCode::Char('h') if !text => Input::Cycle(-1),
            KeyCode::Right | KeyCode::Char(' ' | 'l') if !text => Input::Cycle(1),
            KeyCode::Backspace if !text => Input::Back,
            KeyCode::Backspace if dialog.field_is_empty() && dialog.field == Field::Repo => {
                Input::Back
            }
            KeyCode::Backspace if dialog.field_is_empty() => Input::Field(-1),
            _ if text => Input::Key(key),
            _ => return None,
        }
    } else {
        match key.code {
            KeyCode::Esc => Input::Close,
            KeyCode::Backspace if dialog.search.is_empty() => Input::Back,
            KeyCode::Enter => Input::Submit,
            KeyCode::Up | KeyCode::BackTab => Input::Move(-1),
            KeyCode::Down | KeyCode::Tab => Input::Move(1),
            KeyCode::Char('p' | 'k') if ctrl => Input::Move(-1),
            KeyCode::Char('n' | 'j') if ctrl => Input::Move(1),
            KeyCode::PageUp => Input::Move(-10),
            KeyCode::PageDown => Input::Move(10),
            KeyCode::Home => Input::Top,
            KeyCode::End => Input::Bottom,
            _ => Input::Key(key),
        }
    };
    Some(Action::NewSession(input))
}

/// `at` moved one place forward (`step` > 0) or back in a ring of `len`.
fn cycle(at: usize, len: usize, step: i8) -> usize {
    match len {
        0 => 0,
        _ if step < 0 => (at + len - 1) % len,
        _ => (at + 1) % len,
    }
}

/// A one-line editor with `placeholder`, holding `text`.
fn line(text: &str, placeholder: &str) -> TextArea<'static> {
    search_line(text, placeholder)
}

impl App {
    /// Opens the dialog: at the selected project's machines, else at the projects, on the
    /// open session's.
    pub(crate) fn new_session(&mut self) {
        self.compose.palette = None;
        let Some(first) = self.machines.first().map(|m| m.host_id.clone()) else {
            return;
        };
        let mut dialog = NewSession {
            step: Step::Project,
            search: line("", "type to filter"),
            selected: 0,
            offset: 0,
            project: None,
            host_id: first,
            field: Field::Repo,
            repo: line("", "/absolute/path/to/repo"),
            account: 0,
            model: line("", "provider default"),
            mode: PermissionMode::Ask,
            error: None,
            sending: false,
        };
        if let Some(project) = self.selected_project() {
            let clone = self.last_used_clone(&project);
            dialog.project = Some(project);
            dialog.step = Step::Machine;
            self.compose.dialog = Some(dialog);
            let selected = self.new_session_choices().iter().position(|choice| {
                matches!((choice, &clone), (Choice::Machine(host, repo), Some(c))
                    if *host == c.host_id && repo.as_deref() == Some(c.repo.as_str()))
            });
            if let Some(dialog) = &mut self.compose.dialog {
                dialog.go_to(Step::Machine, selected.unwrap_or(0));
            }
            return;
        }
        let current = self
            .open
            .clone()
            .or_else(|| self.selected().as_ref().and_then(Row::session).cloned())
            .and_then(|key| self.project_of(&key));
        self.compose.dialog = Some(dialog);
        let selected = self.new_session_choices().iter().position(
            |choice| matches!((choice, &current), (Choice::Project(p), Some(c)) if p == c),
        );
        if let Some(dialog) = &mut self.compose.dialog {
            dialog.selected = selected.unwrap_or(0);
        }
    }

    /// Every project with a clone or a session, by name.
    pub(crate) fn known_projects(&self) -> Vec<ProjectId> {
        let mut projects: Vec<ProjectId> = self
            .machines
            .iter()
            .flat_map(|machine| machine.projects.iter().map(|p| p.project_id.clone()))
            .chain(self.sessions.keys().filter_map(|key| self.project_of(key)))
            .collect();
        projects.sort_by_key(|project| (self.project_name(project), project.clone()));
        projects.dedup();
        projects
    }

    /// The rows of the dialog's picker, every one; see [`App::new_session_choices`].
    fn all_choices(&self, dialog: &NewSession) -> Vec<Choice> {
        match dialog.step {
            Step::Project => self
                .known_projects()
                .into_iter()
                .map(Choice::Project)
                .chain([Choice::ByPath])
                .collect(),
            Step::Machine => match &dialog.project {
                Some(project) => self
                    .clones(project)
                    .into_iter()
                    .map(|clone| Choice::Machine(clone.host_id, Some(clone.repo)))
                    .collect(),
                None => self
                    .machines
                    .iter()
                    .map(|machine| Choice::Machine(machine.host_id.clone(), None))
                    .collect(),
            },
            Step::Form => Vec::new(),
        }
    }

    /// What a row of the picker is searched by.
    pub(crate) fn choice_text(&self, choice: &Choice) -> String {
        match choice {
            Choice::Project(project) => format!("{} {}", self.project_name(project), project),
            Choice::ByPath => "a repository by path".to_owned(),
            Choice::Machine(host_id, repo) => {
                let name = self
                    .machines
                    .iter()
                    .find(|m| m.host_id == *host_id)
                    .map_or(host_id.as_str(), |m| m.name.as_str());
                format!("{name} {}", repo.as_deref().unwrap_or(""))
            }
        }
    }

    /// The rows the dialog's picker shows now, filtered by its search.
    pub fn new_session_choices(&self) -> Vec<Choice> {
        let Some(dialog) = &self.compose.dialog else {
            return Vec::new();
        };
        let all = self.all_choices(dialog);
        let query = dialog.search.lines().join(" ");
        crate::fuzzy::filter(&query, &all, |choice| self.choice_text(choice))
            .into_iter()
            .map(|at| all[at].clone())
            .collect()
    }

    /// Carries out one input to the dialog.
    pub(crate) fn new_session_input(&mut self, input: Input) -> Vec<Effect> {
        let count = self.new_session_choices().len();
        let Some(dialog) = &mut self.compose.dialog else {
            return Vec::new();
        };
        let last = count.saturating_sub(1);
        match input {
            Input::Close => self.compose.dialog = None,
            Input::Back => match dialog.step {
                Step::Project => self.compose.dialog = None,
                Step::Machine => {
                    let project = dialog.project.clone();
                    dialog.go_to(Step::Project, 0);
                    let at = self
                        .new_session_choices()
                        .iter()
                        .position(|choice| match choice {
                            Choice::Project(p) => Some(p) == project.as_ref(),
                            Choice::ByPath => project.is_none(),
                            Choice::Machine(..) => false,
                        });
                    if let Some(dialog) = &mut self.compose.dialog {
                        dialog.selected = at.unwrap_or(0);
                    }
                }
                Step::Form => {
                    let (host, repo) = (dialog.host_id.clone(), dialog.repo.lines().join(""));
                    dialog.go_to(Step::Machine, 0);
                    let at = self.new_session_choices().iter().position(|choice| {
                        matches!(choice, Choice::Machine(h, r)
                            if *h == host && r.as_deref().is_none_or(|r| r == repo))
                    });
                    if let Some(dialog) = &mut self.compose.dialog {
                        dialog.selected = at.unwrap_or(0);
                    }
                }
            },
            Input::Move(by) => {
                dialog.selected = dialog
                    .selected
                    .min(last)
                    .saturating_add_signed(by)
                    .min(last);
            }
            Input::Top => dialog.selected = 0,
            Input::Bottom => dialog.selected = last,
            Input::Key(key) => match dialog.step {
                Step::Form => {
                    match dialog.field {
                        Field::Repo => dialog.repo.input(key),
                        Field::Model => dialog.model.input(key),
                        _ => false,
                    };
                }
                _ => {
                    if dialog.search.input(key) {
                        dialog.selected = 0;
                    }
                }
            },
            Input::Field(step) => dialog.field = dialog.field.by(step),
            Input::Focus(field) => dialog.field = field,
            Input::Cycle(step) => self.cycle_choice(step),
            Input::Choose(field, step) => {
                dialog.field = field;
                self.cycle_choice(step);
            }
            Input::Pick(at) => {
                // A tap on the row the cursor is on picks it; on another, moves there.
                if dialog.selected == at {
                    return self.new_session_submit();
                }
                dialog.selected = at.min(last);
            }
            Input::Submit => return self.new_session_submit(),
        }
        Vec::new()
    }

    /// Picks the row under the cursor, or creates the session from the form.
    fn new_session_submit(&mut self) -> Vec<Effect> {
        let choices = self.new_session_choices();
        let Some(dialog) = &self.compose.dialog else {
            return Vec::new();
        };
        if dialog.step == Step::Form {
            return self.create();
        }
        let Some(choice) = choices.get(dialog.selected.min(choices.len().saturating_sub(1))) else {
            return Vec::new();
        };
        match choice.clone() {
            Choice::Project(project) => {
                let clone = self.last_used_clone(&project);
                if let Some(dialog) = &mut self.compose.dialog {
                    dialog.project = Some(project);
                    dialog.go_to(Step::Machine, 0);
                }
                let at = self.new_session_choices().iter().position(|choice| {
                    matches!((choice, &clone), (Choice::Machine(host, repo), Some(c))
                        if *host == c.host_id && repo.as_deref() == Some(c.repo.as_str()))
                });
                if let Some(dialog) = &mut self.compose.dialog {
                    dialog.selected = at.unwrap_or(0);
                }
            }
            Choice::ByPath => {
                if let Some(dialog) = &mut self.compose.dialog {
                    dialog.project = None;
                    dialog.go_to(Step::Machine, 0);
                }
            }
            Choice::Machine(host_id, repo) => {
                let project = dialog.project.clone();
                let account = self.default_account(&host_id, project.as_ref());
                let mode = self.default_mode(&host_id, project.as_ref());
                // By path, the open session's repo on that machine is a good guess.
                let guess = self
                    .open_session()
                    .filter(|_| self.open.as_ref().is_some_and(|key| key.host_id == host_id))
                    .map(|session| session.repo.clone());
                let Some(dialog) = &mut self.compose.dialog else {
                    return Vec::new();
                };
                let path = repo.clone().or(guess).unwrap_or_default();
                dialog.repo = line(&path, "/absolute/path/to/repo");
                dialog.host_id = host_id;
                dialog.account = account;
                dialog.mode = mode;
                dialog.field = if repo.is_some() {
                    Field::Account
                } else {
                    Field::Repo
                };
                dialog.step = Step::Form;
                dialog.error = None;
            }
        }
        Vec::new()
    }

    fn cycle_choice(&mut self, step: i8) {
        let accounts = self
            .compose
            .dialog
            .as_ref()
            .and_then(|dialog| self.machines.iter().find(|m| m.host_id == dialog.host_id))
            .map_or(0, |m| m.accounts.len());
        let Some(dialog) = &mut self.compose.dialog else {
            return;
        };
        match dialog.field {
            Field::Account => dialog.account = cycle(dialog.account, accounts, step),
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
        let repo = dialog.repo.lines().join("").trim().to_owned();
        let model = dialog.model.lines().join("").trim().to_owned();
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
            repo: Some(repo),
            project_id: None,
            branch: None,
            account_id: Some(account.account_id.clone()),
            provider: None,
            model: (!model.is_empty()).then_some(model),
            permission_mode: Some(dialog.mode),
            failover_pin: None,
        };
        vec![Effect::Send {
            host_id: dialog.host_id.clone(),
            command,
            origin: Origin::NewSession(dialog.host_id.clone()),
        }]
    }

    /// Inserts pasted text into the dialog's search or field.
    pub(crate) fn paste_new_session(&mut self, text: &str) -> bool {
        let Some(dialog) = &mut self.compose.dialog else {
            return false;
        };
        let one_line = text.replace(['\r', '\n'], " ");
        let one_line = one_line.trim();
        match (dialog.step, dialog.field) {
            (Step::Form, Field::Repo) => dialog.repo.insert_str(one_line),
            (Step::Form, Field::Model) => dialog.model.insert_str(one_line),
            (Step::Form, _) => false,
            _ => dialog.search.insert_str(one_line),
        };
        true
    }
}

#[cfg(test)]
mod tests {
    use herder_protocol::{AccountId, CommandResult, SessionId};
    use ratatui::crossterm::event::KeyModifiers;

    use super::*;
    use crate::app::{Focus, Msg};
    use crate::fake::{self, key, type_text};

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn dialog(app: &App) -> &NewSession {
        app.compose.dialog.as_ref().expect("the new-session dialog")
    }

    /// [`fake::tree`] with two accounts on its machine.
    fn app() -> App {
        let mut app = fake::tree();
        let mut machines = app.machines.clone();
        machines[0].accounts = vec![
            fake::account("claude-main", "Main"),
            fake::account("claude-work", "Work"),
        ];
        app.update(Msg::Machines(machines));
        app
    }

    #[test]
    fn the_dialog_goes_project_machine_form_and_creates_the_session() {
        let mut app = app();
        let mut machines = app.machines.clone();
        press(&mut app, KeyCode::Char('n'));
        // The selected session's project, then by path.
        assert_eq!(dialog(&app).step, Step::Project);
        let choices = app.new_session_choices();
        assert_eq!(choices.len(), 2);
        assert!(matches!(choices[0], Choice::Project(_)));
        assert_eq!(choices[1], Choice::ByPath);
        assert_eq!(dialog(&app).selected, 0);
        press(&mut app, KeyCode::Enter);
        assert_eq!(dialog(&app).step, Step::Machine);
        assert_eq!(
            app.new_session_choices(),
            [Choice::Machine(
                HostId::new("h1"),
                Some("/home/ann/src/app".into())
            )]
        );
        press(&mut app, KeyCode::Enter);
        let form = dialog(&app);
        assert_eq!(form.step, Step::Form);
        assert_eq!(form.repo.lines(), ["/home/ann/src/app"]);
        assert_eq!(form.field, Field::Account);

        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Tab);
        type_text(&mut app, "claude-opus");
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Left);
        let create = Effect::Send {
            host_id: HostId::new("h1"),
            command: CommandBody::CreateSession {
                repo: Some("/home/ann/src/app".into()),
                project_id: None,
                branch: None,
                account_id: Some(AccountId::new("claude-work")),
                provider: None,
                model: Some("claude-opus".into()),
                permission_mode: Some(PermissionMode::ReadOnly),
                failover_pin: None,
            },
            origin: Origin::NewSession(HostId::new("h1")),
        };
        assert_eq!(press(&mut app, KeyCode::Enter), [create]);
        // A second Enter while it is on its way sends nothing.
        assert_eq!(press(&mut app, KeyCode::Enter), []);

        app.update(Msg::Sent {
            origin: Origin::NewSession(HostId::new("h1")),
            result: Err("not a git repository".into()),
        });
        assert_eq!(dialog(&app).error.as_deref(), Some("not a git repository"));
        assert!(!dialog(&app).sending);

        press(&mut app, KeyCode::Enter);
        app.update(Msg::Sent {
            origin: Origin::NewSession(HostId::new("h1")),
            result: Ok(CommandResult::SessionCreated {
                session_id: SessionId::new("s5"),
            }),
        });
        assert!(app.compose.dialog.is_none());
        assert_eq!(app.open, None);
        let accounts = machines[0].accounts.clone();
        machines[0] = fake::machine("h1", "box", &["s1", "s2", "s3", "s4", "s5"]);
        machines[0].accounts = accounts;
        app.update(Msg::Machines(machines));
        assert_eq!(app.open, Some(key("h1", "s5")));
        assert_eq!(app.focus, Focus::Composer);
    }

    #[test]
    fn backspace_goes_back_field_by_field_and_step_by_step() {
        let mut app = app();
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Enter);
        let field = |app: &App| dialog(app).field;
        // Without arrows: j / k move between fields, h / l change a choice.
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(field(&app), Field::Model);
        type_text(&mut app, "jk");
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(field(&app), Field::Account);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(field(&app), Field::Repo);
        // A text field types k; Up moves on.
        press(&mut app, KeyCode::Up);
        assert_eq!(field(&app), Field::Mode);
        let before = dialog(&app).mode;
        press(&mut app, KeyCode::Char('l'));
        assert_ne!(dialog(&app).mode, before);
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(dialog(&app).mode, before);
        // On a choice, Backspace goes back to the machines, then to the projects, then closes.
        press(&mut app, KeyCode::Backspace);
        assert_eq!(dialog(&app).step, Step::Machine);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(dialog(&app).step, Step::Project);
        assert_eq!(dialog(&app).selected, 0);
        press(&mut app, KeyCode::Backspace);
        assert!(app.compose.dialog.is_none());
    }

    #[test]
    fn the_search_filters_and_by_path_lists_every_machine() {
        let mut app = app();
        press(&mut app, KeyCode::Char('n'));
        type_text(&mut app, "path");
        assert_eq!(app.new_session_choices(), [Choice::ByPath]);
        type_text(&mut app, "zzz");
        assert!(app.new_session_choices().is_empty());
        // Enter on nothing does nothing.
        assert_eq!(press(&mut app, KeyCode::Enter), []);
        assert_eq!(dialog(&app).step, Step::Project);
        for _ in 0..3 {
            press(&mut app, KeyCode::Backspace);
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.new_session_choices(),
            [Choice::Machine(HostId::new("h1"), None)]
        );
        // A tap on the row under the cursor picks it.
        app.act(Action::NewSession(Input::Pick(0)));
        let form = dialog(&app);
        assert_eq!((form.step, form.field), (Step::Form, Field::Repo));
        // Its path is left to type: the open session's, if any, is a guess.
        assert!(form.repo.is_empty());
    }

    #[test]
    fn the_dialog_needs_an_account_and_a_repo() {
        let mut app = fake::tree();
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Enter);
        assert_eq!(press(&mut app, KeyCode::Enter), []);
        assert_eq!(
            dialog(&app).error.as_deref(),
            Some("this machine has no accounts")
        );
        let mut app = app_by_path();
        assert_eq!(press(&mut app, KeyCode::Enter), []);
        assert_eq!(
            dialog(&app).error.as_deref(),
            Some("enter the repository's path")
        );
        press(&mut app, KeyCode::Esc);
        assert!(app.compose.dialog.is_none());
    }

    /// [`app`] at the form of a new session by path.
    fn app_by_path() -> App {
        let mut app = app();
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Enter);
        app
    }
}
