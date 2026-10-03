//! The machines panel, its add-machine dialog and its add-account dialog: their state, keys
//! and reducer.
//!
//! The panel lists every paired machine with its connection, role, accounts, addresses and
//! pinned fingerprint, and renames the selected one on this device or forgets it here. Its add
//! dialog pairs a new one: paste the `herder://pair` link `herder pair`
//! printed, or type its address, fingerprint and code; check the fingerprint; pair. Pasting a
//! link anywhere in the TUI opens the dialog at that check.

use std::net::{IpAddr, SocketAddr};

use herder_client_core::Machine;
use herder_client_core::PairingUri;
use herder_protocol::HostId;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::accounts::{self, AddAccount};
use crate::action::Action;
use crate::app::{App, Effect, Focus};
use crate::terminal::{self, Target};

/// The port a daemon listens on unless configured otherwise, as `herder daemon` has it; added
/// to a typed address without one.
const DEFAULT_PORT: u16 = 7447;

/// The machines panel, shown over the main screen.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MachinePanel {
    /// The machine the user selected; until they move, the first one.
    pub chosen: Option<HostId>,
    /// The add-machine dialog, over the panel.
    pub add: Option<AddMachine>,
    /// The add-account dialog, over the panel.
    pub account: Option<AddAccount>,
    /// A rename or forget of the selected machine, in the panel.
    pub edit: Option<PanelEdit>,
}

/// What the panel does to the selected machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelEdit {
    /// Renaming it: the name typed so far.
    Rename(String),
    /// Asking before forgetting it.
    Forget,
}

impl MachinePanel {
    /// Index of the selected machine in `machines`.
    pub fn selected(&self, machines: &[Machine]) -> Option<usize> {
        self.chosen
            .as_ref()
            .and_then(|chosen| machines.iter().position(|m| m.host_id == *chosen))
            .or((!machines.is_empty()).then_some(0))
    }
}

/// The add-machine dialog.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AddMachine {
    /// What the user entered.
    pub form: Form,
    /// Where pairing stands.
    pub step: Step,
}

/// Where the add-machine dialog stands.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Step {
    /// Entering the link or its fields.
    #[default]
    Edit,
    /// Entering again after a failure.
    Failed(String),
    /// Checking the fingerprint before pairing.
    Confirm(PairingUri),
    /// Waiting for the daemon to accept the code.
    Pairing(PairingUri),
    /// Paired.
    Paired(Box<Machine>),
}

/// The add-machine form: a pasted link, or what `herder pair` prints field by field.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Form {
    /// The `herder://pair` link.
    pub link: String,
    /// The daemon's address, `host[:port]`.
    pub host: String,
    /// SHA-256 of the daemon's certificate.
    pub fingerprint: String,
    /// The one-time pairing code.
    pub code: String,
    /// The field being typed in.
    pub focus: Field,
}

/// A field of the add-machine form, in tab order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Field {
    /// The link.
    #[default]
    Link,
    /// The address.
    Host,
    /// The fingerprint.
    Fingerprint,
    /// The code.
    Code,
}

impl Field {
    const ALL: [Field; 4] = [Field::Link, Field::Host, Field::Fingerprint, Field::Code];

    fn next(self, forward: bool) -> Field {
        let len = Self::ALL.len();
        let at = Self::ALL.iter().position(|f| *f == self).unwrap_or(0);
        let to = if forward { at + 1 } else { at + len - 1 };
        Self::ALL[to % len]
    }
}

impl Form {
    /// Whether nothing is typed in any field.
    fn is_empty(&self) -> bool {
        [&self.link, &self.host, &self.fingerprint, &self.code]
            .iter()
            .all(|field| field.is_empty())
    }

    fn field(&mut self) -> &mut String {
        match self.focus {
            Field::Link => &mut self.link,
            Field::Host => &mut self.host,
            Field::Fingerprint => &mut self.fingerprint,
            Field::Code => &mut self.code,
        }
    }

    /// The pairing link the form describes: the link if one is entered, else the fields.
    pub fn uri(&self) -> Result<PairingUri, String> {
        let link = self.link.trim();
        if !link.is_empty() {
            return link
                .parse()
                .map_err(|err: herder_client_core::Error| err.to_string());
        }
        let host = self.host.trim();
        if host.is_empty() {
            return Err("paste the link, or enter the address, fingerprint and code".to_owned());
        }
        let fingerprint: String = self
            .fingerprint
            .chars()
            .filter(|c| !c.is_whitespace() && *c != ':')
            .collect::<String>()
            .to_ascii_lowercase();
        if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("the fingerprint is 64 hex digits, as `herder pair` prints it".to_owned());
        }
        let code = self.code.trim();
        if code.is_empty() {
            return Err("enter the pairing code".to_owned());
        }
        Ok(PairingUri {
            hosts: vec![with_port(host)],
            fingerprint,
            code: code.to_owned(),
        })
    }
}

/// `host` as `host:port`, adding the default port when it has none.
fn with_port(host: &str) -> String {
    if host.parse::<SocketAddr>().is_ok() {
        return host.to_owned();
    }
    if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        return SocketAddr::new(ip, DEFAULT_PORT).to_string();
    }
    match host.rsplit_once(':') {
        Some((_, port)) if port.parse::<u16>().is_ok() => host.to_owned(),
        _ => format!("{host}:{DEFAULT_PORT}"),
    }
}

/// Input to the machines panel or its add dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// Close the dialog step, or the panel.
    Close,
    /// Select the previous machine, or the previous field.
    Up,
    /// Select the next machine, or the next field.
    Down,
    /// Open the add dialog.
    Add,
    /// Open the add-account dialog for the selected machine.
    AddAccount,
    /// Start renaming the selected machine.
    Rename,
    /// Ask before forgetting the selected machine.
    Forget,
    /// Pick the previous choice.
    Left,
    /// Pick the next choice.
    Right,
    /// Go on: check the form, pair, or finish.
    Submit,
    /// Type a character.
    Char(char),
    /// Delete the last character.
    Backspace,
    /// Clear the field.
    Clear,
}

/// The action a key asks for while the panel is open.
pub fn for_key(key: KeyEvent, panel: &MachinePanel) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if panel.account.is_some() {
        return accounts::input_for_key(key).map(Action::Machines);
    }
    let input = match &panel.edit {
        Some(PanelEdit::Rename(_)) => match key.code {
            KeyCode::Esc => Input::Close,
            KeyCode::Enter => Input::Submit,
            KeyCode::Backspace => Input::Backspace,
            KeyCode::Char('u') if ctrl => Input::Clear,
            KeyCode::Char(c) if !ctrl => Input::Char(c),
            _ => return None,
        },
        Some(PanelEdit::Forget) => match key.code {
            KeyCode::Enter | KeyCode::Char('y') => Input::Submit,
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('n') => Input::Close,
            _ => return None,
        },
        None => return panel_key(key, panel),
    };
    Some(Action::Machines(input))
}

/// The action a key asks for while the panel is open with no rename or forget going on.
fn panel_key(key: KeyEvent, panel: &MachinePanel) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let input = match &panel.add {
        Some(AddMachine {
            step: Step::Edit | Step::Failed(_),
            form,
        }) => match key.code {
            KeyCode::Esc => Input::Close,
            KeyCode::Backspace if form.is_empty() => Input::Close,
            KeyCode::Enter => Input::Submit,
            KeyCode::Tab | KeyCode::Down => Input::Down,
            KeyCode::BackTab | KeyCode::Up => Input::Up,
            KeyCode::Backspace => Input::Backspace,
            KeyCode::Char('u') if ctrl => Input::Clear,
            KeyCode::Char(c) if !ctrl => Input::Char(c),
            _ => return None,
        },
        Some(AddMachine {
            step: Step::Confirm(_) | Step::Paired(_),
            ..
        }) => match key.code {
            KeyCode::Enter => Input::Submit,
            KeyCode::Esc | KeyCode::Backspace => Input::Close,
            _ => return None,
        },
        Some(AddMachine {
            step: Step::Pairing(_),
            ..
        }) => match key.code {
            KeyCode::Esc | KeyCode::Backspace => Input::Close,
            _ => return None,
        },
        None => match key.code {
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('m' | 'q') => Input::Close,
            KeyCode::Char('k') | KeyCode::Up => Input::Up,
            KeyCode::Char('j') | KeyCode::Down => Input::Down,
            KeyCode::Char('a') => Input::Add,
            KeyCode::Char('n') => Input::AddAccount,
            KeyCode::Char('e') => Input::Rename,
            KeyCode::Char('d') => Input::Forget,
            KeyCode::Char('r') => return Some(Action::Reconnect),
            KeyCode::Char('?') => return Some(Action::ToggleHelp),
            _ => return None,
        },
    };
    Some(Action::Machines(input))
}

impl App {
    /// Opens the machines panel, with the add dialog if `add`.
    pub(crate) fn open_machines(&mut self, add: bool) {
        let panel = self.machine_panel.get_or_insert_with(MachinePanel::default);
        if add && panel.add.is_none() {
            panel.add = Some(AddMachine::default());
        }
    }

    /// Carries out one input to the machines panel.
    pub(crate) fn machine_input(&mut self, input: Input) -> Vec<Effect> {
        let Some(panel) = &mut self.machine_panel else {
            return Vec::new();
        };
        if let Some(account) = &mut panel.account {
            match account.input(input) {
                accounts::Outcome::Open => {}
                accounts::Outcome::Closed => panel.account = None,
                accounts::Outcome::Login(new) => {
                    let host_id = account.host_id.clone();
                    self.machine_panel = None;
                    return vec![Effect::AttachTerminal {
                        host_id,
                        target: Target::Login(new),
                    }];
                }
            }
            return Vec::new();
        }
        let selected = panel
            .selected(&self.machines)
            .map(|at| self.machines[at].host_id.clone());
        if let Some(edit) = &mut panel.edit {
            match (edit, input) {
                (_, Input::Close) => panel.edit = None,
                (PanelEdit::Rename(name), Input::Char(c)) => name.push(c),
                (PanelEdit::Rename(name), Input::Backspace) => {
                    name.pop();
                }
                (PanelEdit::Rename(name), Input::Clear) => name.clear(),
                (PanelEdit::Rename(name), Input::Submit) if !name.trim().is_empty() => {
                    let name = name.trim().to_owned();
                    panel.edit = None;
                    return selected
                        .map(|host_id| Effect::RenameMachine { host_id, name })
                        .into_iter()
                        .collect();
                }
                (PanelEdit::Forget, Input::Submit) => {
                    panel.edit = None;
                    panel.chosen = None;
                    return selected.map(Effect::ForgetMachine).into_iter().collect();
                }
                _ => {}
            }
            return Vec::new();
        }
        let Some(add) = &mut panel.add else {
            match input {
                Input::Close => self.machine_panel = None,
                Input::Add => panel.add = Some(AddMachine::default()),
                Input::Rename => {
                    if let Some(at) = panel.selected(&self.machines) {
                        let name = self.machines[at].name.clone();
                        panel.edit = Some(PanelEdit::Rename(name));
                    }
                }
                Input::Forget => {
                    if panel.selected(&self.machines).is_some() {
                        panel.edit = Some(PanelEdit::Forget);
                    }
                }
                Input::AddAccount => {
                    let Some(at) = panel.selected(&self.machines) else {
                        return Vec::new();
                    };
                    let host_id = self.machines[at].host_id.clone();
                    match terminal::refusal(&self.machines, &host_id) {
                        Some(refusal) => {
                            self.notice = Some(format!("adding accounts: {refusal}"));
                        }
                        None => panel.account = Some(AddAccount::new(host_id)),
                    }
                }
                Input::Up | Input::Down => {
                    let step = if input == Input::Up { -1 } else { 1 };
                    if let Some(at) = panel.selected(&self.machines) {
                        let last = self.machines.len() - 1;
                        let at = at.saturating_add_signed(step).min(last);
                        panel.chosen = Some(self.machines[at].host_id.clone());
                    }
                }
                _ => {}
            }
            return Vec::new();
        };
        match (&add.step, input) {
            (Step::Edit | Step::Failed(_), Input::Close) | (Step::Pairing(_), Input::Close) => {
                panel.add = None;
            }
            (Step::Edit | Step::Failed(_), Input::Submit) => add.check(),
            (Step::Edit | Step::Failed(_), Input::Up) => {
                add.form.focus = add.form.focus.next(false)
            }
            (Step::Edit | Step::Failed(_), Input::Down) => {
                add.form.focus = add.form.focus.next(true)
            }
            (Step::Edit | Step::Failed(_), Input::Char(c)) => add.form.field().push(c),
            (Step::Edit | Step::Failed(_), Input::Backspace) => {
                add.form.field().pop();
            }
            (Step::Edit | Step::Failed(_), Input::Clear) => add.form.field().clear(),
            (Step::Confirm(_), Input::Close) => add.step = Step::Edit,
            (Step::Confirm(uri), Input::Submit) => {
                let link = uri.to_string();
                add.step = Step::Pairing(uri.clone());
                return vec![Effect::Pair(link)];
            }
            (Step::Paired(_), Input::Submit | Input::Close) => self.machine_panel = None,
            _ => {}
        }
        Vec::new()
    }

    /// Pasted text for the machines panel: into the add dialog's field, or a pairing link to
    /// open the dialog with. Returns whether it took the text; a link pasted while the composer,
    /// one of its dialogs or the PR link prompt has the keys is left to them.
    pub(crate) fn paste_pairing(&mut self, text: &str) -> bool {
        let text = text.trim();
        let Some(panel) = &mut self.machine_panel else {
            let composing = self.focus == Focus::Composer
                || self.compose.dialog.is_some()
                || self.compose.palette.is_some()
                || self.prs.prompt.is_some();
            if !text.starts_with("herder://") || composing {
                return false;
            }
            let mut add = AddMachine::default();
            add.paste(text);
            self.machine_panel = Some(MachinePanel {
                add: Some(add),
                ..MachinePanel::default()
            });
            return true;
        };
        if let Some(field) = panel.account.as_mut().and_then(AddAccount::field) {
            field.push_str(text.lines().next().unwrap_or(""));
        } else if let Some(add) = &mut panel.add
            && matches!(add.step, Step::Edit | Step::Failed(_))
        {
            add.paste(text);
        }
        true
    }

    /// The outcome of pairing, for the dialog if it still waits for it.
    pub(crate) fn paired(&mut self, result: Result<Box<Machine>, String>) {
        let Some(panel) = &mut self.machine_panel else {
            return;
        };
        let Some(add) = panel
            .add
            .as_mut()
            .filter(|a| matches!(a.step, Step::Pairing(_)))
        else {
            return;
        };
        match result {
            Ok(machine) => {
                panel.chosen = Some(machine.host_id.clone());
                add.step = Step::Paired(machine);
            }
            Err(error) => add.step = Step::Failed(error),
        }
    }
}

impl AddMachine {
    /// A whole link replaces the form and goes straight to checking its fingerprint; other
    /// text goes into the field being typed in, as one line.
    fn paste(&mut self, text: &str) {
        if text.starts_with("herder://") {
            self.form.link = text.to_owned();
            self.form.focus = Field::Link;
            self.check();
        } else {
            self.form
                .field()
                .push_str(text.lines().next().unwrap_or(""));
        }
    }

    /// Moves on to the fingerprint check if the form is complete.
    fn check(&mut self) {
        self.step = match self.form.uri() {
            Ok(uri) => Step::Confirm(uri),
            Err(error) => Step::Failed(error),
        };
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyEvent;

    use herder_protocol::Provider;

    use super::*;
    use crate::app::Msg;
    use crate::fake::machine;

    const FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    fn step(app: &App) -> &Step {
        &app.machine_panel
            .as_ref()
            .unwrap()
            .add
            .as_ref()
            .unwrap()
            .step
    }

    fn link() -> String {
        PairingUri {
            hosts: vec!["127.0.0.1:7447".into()],
            fingerprint: FP.into(),
            code: "ABCDE-FGHJK".into(),
        }
        .to_string()
    }

    #[test]
    fn the_panel_renames_and_forgets_the_selected_machine() {
        let mut app = App::default();
        app.update(Msg::Machines(vec![
            machine("h1", "box", &[]),
            machine("h2", "box", &[]),
        ]));
        press(&mut app, KeyCode::Char('m'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('e'));
        // The name starts as it is; an empty one is not saved.
        for _ in 0.."box".len() {
            press(&mut app, KeyCode::Backspace);
        }
        assert_eq!(press(&mut app, KeyCode::Enter), []);
        typed(&mut app, "laptop");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            [Effect::RenameMachine {
                host_id: HostId::new("h2"),
                name: "laptop".into(),
            }]
        );
        // `d` asks first; `n` keeps the machine, `y` forgets it.
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(press(&mut app, KeyCode::Char('n')), []);
        assert!(app.machine_panel.as_ref().unwrap().edit.is_none());
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            [Effect::ForgetMachine(HostId::new("h2"))]
        );
        assert!(app.machine_panel.is_some());
    }

    #[test]
    fn a_typed_link_is_checked_then_paired() {
        let mut app = App::default();
        press(&mut app, KeyCode::Char('a'));
        typed(&mut app, "herder://nope");
        press(&mut app, KeyCode::Enter);
        assert!(matches!(step(&app), Step::Failed(_)), "{:?}", step(&app));
        app.update(Msg::Key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        )));
        typed(&mut app, &link());
        press(&mut app, KeyCode::Enter);
        let Step::Confirm(uri) = step(&app) else {
            panic!("{:?}", step(&app));
        };
        assert_eq!(uri.fingerprint, FP);
        // Back to the form, and on again.
        press(&mut app, KeyCode::Esc);
        assert_eq!(*step(&app), Step::Edit);
        press(&mut app, KeyCode::Enter);
        assert_eq!(press(&mut app, KeyCode::Enter), [Effect::Pair(link())]);
        assert!(matches!(step(&app), Step::Pairing(_)));
        // Keys other than Esc do nothing while pairing.
        assert_eq!(press(&mut app, KeyCode::Enter), []);

        let new = machine("h9", "laptop", &[]);
        app.update(Msg::Paired(Ok(Box::new(new.clone()))));
        assert_eq!(*step(&app), Step::Paired(Box::new(new)));
        assert_eq!(
            app.machine_panel.as_ref().unwrap().chosen,
            Some(HostId::new("h9"))
        );
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.machine_panel, None);
    }

    #[test]
    fn the_fields_make_a_link() {
        let mut app = App::default();
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Tab);
        typed(&mut app, "box.lan");
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            *step(&app),
            Step::Failed("the fingerprint is 64 hex digits, as `herder pair` prints it".into())
        );
        press(&mut app, KeyCode::Tab);
        typed(&mut app, &FP.to_ascii_uppercase());
        press(&mut app, KeyCode::Down);
        typed(&mut app, "abcde-fghjk");
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            *step(&app),
            Step::Confirm(PairingUri {
                hosts: vec!["box.lan:7447".into()],
                fingerprint: FP.into(),
                code: "abcde-fghjk".into(),
            })
        );
    }

    #[test]
    fn typed_addresses_get_the_default_port() {
        assert_eq!(with_port("box"), "box:7447");
        assert_eq!(with_port("box:9000"), "box:9000");
        assert_eq!(with_port("10.0.0.2"), "10.0.0.2:7447");
        assert_eq!(with_port("fd00::1"), "[fd00::1]:7447");
        assert_eq!(with_port("[fd00::1]"), "[fd00::1]:7447");
        assert_eq!(with_port("[fd00::1]:9000"), "[fd00::1]:9000");
    }

    #[test]
    fn a_failed_pairing_returns_to_the_form_with_the_error() {
        let mut app = App::default();
        app.update(Msg::Paste(format!("{}\n", link())));
        assert!(matches!(step(&app), Step::Confirm(_)));
        press(&mut app, KeyCode::Enter);
        app.update(Msg::Paired(Err("pairing failed: bad code".into())));
        assert_eq!(*step(&app), Step::Failed("pairing failed: bad code".into()));
        // The form keeps the link, to try again.
        let form = &app
            .machine_panel
            .as_ref()
            .unwrap()
            .add
            .as_ref()
            .unwrap()
            .form;
        assert_eq!(form.link, link());
    }

    #[test]
    fn a_pasted_link_opens_the_dialog_but_other_text_does_not() {
        let mut app = crate::fake::tree();
        app.update(Msg::Paste("hello".into()));
        assert_eq!(app.machine_panel, None);
        app.update(Msg::Paste(link()));
        assert!(matches!(step(&app), Step::Confirm(_)));
        // A link pasted while pairing changes nothing.
        press(&mut app, KeyCode::Enter);
        app.update(Msg::Paste(link()));
        assert!(matches!(step(&app), Step::Pairing(_)));
        // Closing while pairing drops the dialog; the late outcome is ignored.
        press(&mut app, KeyCode::Esc);
        app.update(Msg::Paired(Ok(Box::new(machine("h9", "laptop", &[])))));
        assert_eq!(app.machine_panel.as_ref().unwrap().add, None);
    }

    #[test]
    fn pasted_text_goes_into_the_field() {
        let mut app = App::default();
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Tab);
        app.update(Msg::Paste("box.lan:9000\n".into()));
        let form = &app
            .machine_panel
            .as_ref()
            .unwrap()
            .add
            .as_ref()
            .unwrap()
            .form;
        assert_eq!(form.host, "box.lan:9000");
    }

    #[test]
    fn the_panel_selects_machines_and_closes() {
        let mut app = App::default();
        app.update(Msg::Machines(vec![
            machine("h1", "box", &[]),
            machine("h2", "laptop", &[]),
        ]));
        press(&mut app, KeyCode::Char('m'));
        let selected = |app: &App| {
            app.machine_panel
                .as_ref()
                .unwrap()
                .selected(&app.machines)
                .unwrap()
        };
        assert_eq!(selected(&app), 0);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(selected(&app), 1);
        // Keys meant for the session list do not reach it.
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.open, None);
        assert_eq!(press(&mut app, KeyCode::Char('r')), [Effect::Wake]);
        press(&mut app, KeyCode::Char('a'));
        typed(&mut app, "q");
        // In the form, q is text, and Esc closes only the dialog.
        press(&mut app, KeyCode::Esc);
        assert!(app.machine_panel.as_ref().unwrap().add.is_none());
        press(&mut app, KeyCode::Char('q'));
        assert_eq!(app.machine_panel, None);
    }

    #[test]
    fn the_account_dialog_starts_the_selected_machines_login() {
        let mut app = App::default();
        app.update(Msg::Machines(vec![
            machine("h1", "box", &[]),
            machine("h2", "laptop", &[]),
        ]));
        press(&mut app, KeyCode::Char('m'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('n'));
        let account = |app: &App| app.machine_panel.as_ref().unwrap().account.clone().unwrap();
        assert_eq!(account(&app).host_id, HostId::new("h2"));
        // The provider is picked with the arrows or space, wrapping around.
        press(&mut app, KeyCode::Left);
        assert_eq!(*account(&app).provider(), Provider::Cursor);
        typed(&mut app, " ");
        assert_eq!(*account(&app).provider(), Provider::Claude);
        press(&mut app, KeyCode::Right);
        assert_eq!(account(&app).default_config_dir(), "~/.codex-<id>");
        // An id is needed.
        assert_eq!(press(&mut app, KeyCode::Enter), []);
        assert_eq!(account(&app).focus, crate::accounts::Field::Id);
        assert!(account(&app).error.is_some());
        typed(&mut app, "work");
        assert_eq!(account(&app).default_config_dir(), "~/.codex-work");
        press(&mut app, KeyCode::Tab);
        typed(&mut app, "Work");
        press(&mut app, KeyCode::Tab);
        app.update(Msg::Paste("~/.codex-w\nignored".into()));
        press(&mut app, KeyCode::Backspace);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            [Effect::AttachTerminal {
                host_id: HostId::new("h2"),
                target: Target::Login(herder_client_core::NewAccount {
                    account_id: herder_protocol::AccountId::new("work"),
                    provider: Provider::Codex,
                    label: Some("Work".into()),
                    config_dir: Some("~/.codex-".into()),
                }),
            }]
        );
        assert_eq!(app.machine_panel, None);
    }

    #[test]
    fn members_cannot_add_accounts() {
        let mut app = App::default();
        let mut member = machine("h1", "box", &[]);
        member.role = Some(herder_protocol::Role::Member);
        app.update(Msg::Machines(vec![member]));
        press(&mut app, KeyCode::Char('m'));
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.machine_panel.as_ref().unwrap().account, None);
        assert_eq!(
            app.notice.as_deref(),
            Some("adding accounts: terminals are owner-only")
        );
        // Esc on the dialog closes only the dialog.
        let mut app = App::default();
        app.update(Msg::Machines(vec![machine("h1", "box", &[])]));
        press(&mut app, KeyCode::Char('m'));
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Esc);
        let panel = app.machine_panel.as_ref().unwrap();
        assert_eq!(panel.account, None);
    }
}
