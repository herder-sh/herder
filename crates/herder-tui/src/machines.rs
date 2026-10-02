//! The machines panel and the add-machine dialog: their state, keys and reducer.
//!
//! The panel lists every paired machine with its connection, role, addresses and pinned
//! fingerprint. Its add dialog pairs a new one: paste the `herder://pair` link `herder pair`
//! printed, or type its address, fingerprint and code; check the fingerprint; pair. Pasting a
//! link anywhere in the TUI opens the dialog at that check.

use std::net::{IpAddr, SocketAddr};

use herder_client_core::Machine;
use herder_client_core::auth::PairingUri;
use herder_protocol::HostId;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::Action;
use crate::app::{App, Effect, Focus};

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
    Paired(Machine),
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
                .map_err(|err: anyhow::Error| format!("{err:#}"));
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
    let input = match &panel.add {
        Some(AddMachine {
            step: Step::Edit | Step::Failed(_),
            ..
        }) => match key.code {
            KeyCode::Esc => Input::Close,
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
            KeyCode::Esc => Input::Close,
            _ => return None,
        },
        Some(AddMachine {
            step: Step::Pairing(_),
            ..
        }) => match key.code {
            KeyCode::Esc => Input::Close,
            _ => return None,
        },
        None => match key.code {
            KeyCode::Esc | KeyCode::Char('m' | 'q') => Input::Close,
            KeyCode::Char('k') | KeyCode::Up => Input::Up,
            KeyCode::Char('j') | KeyCode::Down => Input::Down,
            KeyCode::Char('a') => Input::Add,
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
        let Some(add) = &mut panel.add else {
            match input {
                Input::Close => self.machine_panel = None,
                Input::Add => panel.add = Some(AddMachine::default()),
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
                chosen: None,
                add: Some(add),
            });
            return true;
        };
        if let Some(add) = &mut panel.add
            && matches!(add.step, Step::Edit | Step::Failed(_))
        {
            add.paste(text);
        }
        true
    }

    /// The outcome of pairing, for the dialog if it still waits for it.
    pub(crate) fn paired(&mut self, result: Result<Machine, String>) {
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
        app.update(Msg::Paired(Ok(new.clone())));
        assert_eq!(*step(&app), Step::Paired(new));
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
        app.update(Msg::Paired(Ok(machine("h9", "laptop", &[]))));
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
}
