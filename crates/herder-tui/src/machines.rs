//! The machines panel, its add-machine dialog and its add-account dialog: their state, keys
//! and reducer.
//!
//! The panel lists every paired machine with its connection, role, accounts, addresses and
//! pinned fingerprint, and renames the selected one on this device or forgets it here. Its add
//! dialog signs this terminal in to another: paste the `herder://fleet` link an app of yours
//! shares, or type the machine's address and fingerprint; check the fingerprint; then an app
//! of yours approves the code the dialog shows, as for `herder login`. Pasting a link anywhere
//! in the TUI opens the dialog at that check.
//!
//! An owner edits the selected machine's public addresses in the panel too: where other
//! devices reach it from outside its networks (a port forward, a TCP proxy, a VPN, a tunnel's
//! `wss://` URL). They are the daemon's setting, so every device picks them up from its next
//! connection. They also turn the machine's sharing of diagnostics on or off, its
//! `[telemetry]` setting: off until they do.
//!
//! Anyone ranks the selected machine's routes on this device: `o` lists its addresses in the
//! order they are tried, each named by what carries it; space pins one (the only one dialled),
//! disables it (never dialled) or puts it back in the race, and `J`/`K` move it. On a tunnel
//! behind a login proxy, such as Cloudflare Access, `s` signs in with a browser: the TUI opens
//! the proxy's login page, or shows its URL, and connects once the person signed in there.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};

use herder_client_core::{FleetLink, FleetMachine, Machine, RouteChoice};
use herder_protocol::{CommandBody, CommandResult, HostId, HubVolume, Role};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::accounts::{self, AddAccount};
use crate::action::Action;
use crate::app::{App, Effect, Focus};
use crate::backup::{self, Backup, Links};
use crate::secrets::{self, Secrets};
use crate::terminal;

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
    /// The backup dialog, over the panel.
    pub backup: Option<Backup>,
    /// The secrets screen of the selected hub, over the panel.
    pub secrets: Option<Secrets>,
    /// The usage screen of the selected hub, over the panel.
    pub usage: Option<crate::hub_usage::HubUsage>,
    /// Where each machine this client owns backs up, as it last said.
    pub links: Links,
    /// How full each hub's disk is, as it said with its link when the panel opened.
    pub volumes: HashMap<HostId, HubVolume>,
}

/// What the panel does to the selected machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelEdit {
    /// Renaming it: the name typed so far.
    Rename(String),
    /// Asking before forgetting it.
    Forget,
    /// Editing its public addresses, separated by spaces or commas: `None` until the machine
    /// said what they are.
    Public(Option<String>),
    /// Ranking its routes on this device: the one selected, by its index in its addresses.
    Routes(usize),
}

/// What a route's choice becomes when space is pressed on it: in the race, then the only one,
/// then never, then back in the race.
pub fn next_choice(choice: RouteChoice) -> RouteChoice {
    match choice {
        RouteChoice::Auto => RouteChoice::Pinned,
        RouteChoice::Pinned => RouteChoice::Disabled,
        RouteChoice::Disabled => RouteChoice::Auto,
    }
}

/// The user's choice for `address`, one of `machine`'s addresses.
pub fn choice(machine: &Machine, address: &str) -> RouteChoice {
    if machine.pinned_address.as_deref() == Some(address) {
        RouteChoice::Pinned
    } else if machine.disabled_addresses.iter().any(|a| a == address) {
        RouteChoice::Disabled
    } else {
        RouteChoice::Auto
    }
}

/// A step of editing a machine's public addresses, whose answer comes back to the panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PublicSent {
    /// Asking the machine what they are now.
    Load(HostId),
    /// Asking for its settings, to save them with these addresses.
    Save(HostId, Vec<String>),
    /// Saving them.
    Set(HostId),
}

/// A step of turning a machine's sharing of diagnostics on or off, whose answer comes back to
/// the panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiagnosticsSent {
    /// Asking for its settings, to save them with sharing turned the other way.
    Load(HostId),
    /// Saving them, with sharing on or off.
    Set(HostId, bool),
}

fn send_diagnostics(host_id: HostId, command: CommandBody, sent: DiagnosticsSent) -> Effect {
    Effect::Send {
        host_id,
        command,
        origin: crate::compose::Origin::Diagnostics(sent),
    }
}

/// `text`, public addresses as typed: separated by spaces or commas.
fn public_addresses(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || c == ',')
        .filter(|address| !address.is_empty())
        .map(str::to_owned)
        .collect()
}

fn send_public(host_id: HostId, command: CommandBody, sent: PublicSent) -> Effect {
    Effect::Send {
        host_id,
        command,
        origin: crate::compose::Origin::Public(sent),
    }
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
    /// Where signing in stands.
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
    /// Checking the fingerprint before signing in.
    Confirm(FleetMachine),
    /// Waiting for an app of the person to approve the login: its code, once the machine
    /// gave one.
    SigningIn(FleetMachine, Option<String>),
    /// Signed in.
    SignedIn(Box<Machine>),
}

/// The add-machine form: a pasted link, or the machine's address and fingerprint.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Form {
    /// The `herder://fleet` link, of one machine.
    pub link: String,
    /// The daemon's address, `host[:port]`.
    pub host: String,
    /// SHA-256 of the daemon's certificate.
    pub fingerprint: String,
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
}

impl Field {
    const ALL: [Field; 3] = [Field::Link, Field::Host, Field::Fingerprint];

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
        [&self.link, &self.host, &self.fingerprint]
            .iter()
            .all(|field| field.is_empty())
    }

    fn field(&mut self) -> &mut String {
        match self.focus {
            Field::Link => &mut self.link,
            Field::Host => &mut self.host,
            Field::Fingerprint => &mut self.fingerprint,
        }
    }

    /// The machine the form names: the link's if one is entered, else the fields'.
    pub fn machine(&self) -> Result<FleetMachine, String> {
        let link = self.link.trim();
        if !link.is_empty() {
            let link: FleetLink = link
                .parse()
                .map_err(|err: herder_client_core::Error| err.to_string())?;
            let count = link.machines.len();
            return <[FleetMachine; 1]>::try_from(link.machines)
                .map(|[machine]| machine)
                .map_err(|_| format!("the link names {count} machines; add them one at a time"));
        }
        let host = self.host.trim();
        if host.is_empty() {
            return Err("paste the link, or enter the address and fingerprint".to_owned());
        }
        let fingerprint: String = self
            .fingerprint
            .chars()
            .filter(|c| !c.is_whitespace() && *c != ':')
            .collect::<String>()
            .to_ascii_lowercase();
        if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(
                "the fingerprint is 64 hex digits, as the machine's page in the app shows it"
                    .to_owned(),
            );
        }
        Ok(FleetMachine {
            hosts: vec![with_port(host)],
            fingerprint,
            name: None,
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
    /// Select the first machine.
    Top,
    /// Select the last machine.
    Bottom,
    /// Open the add dialog.
    Add,
    /// Open the add-account dialog for the selected machine.
    AddAccount,
    /// Start renaming the selected machine.
    Rename,
    /// Ask before forgetting the selected machine.
    Forget,
    /// Back the selected machine up to a hub, or stop.
    Backup,
    /// Manage the secrets of the selected hub.
    Secrets,
    /// See what the selected hub's logins used.
    Usage,
    /// Edit the public addresses of the selected machine.
    Public,
    /// Rank the routes of the selected machine on this device.
    Routes,
    /// Turn the selected machine's sharing of diagnostics on or off.
    Diagnostics,
    /// Try the selected route earlier.
    Earlier,
    /// Try the selected route later.
    Later,
    /// Sign in with a browser to the login proxy in front of the selected route.
    SignIn,
    /// Pick the previous choice.
    Left,
    /// Pick the next choice.
    Right,
    /// Go on: check the form, sign in, or finish.
    Submit,
    /// Type a character.
    Char(char),
    /// Delete the last character.
    Backspace,
    /// Clear the field.
    Clear,
    /// Type in a field of the add dialog: a tapped one.
    Focus(Field),
}

/// The action a key asks for while the panel is open.
pub fn for_key(key: KeyEvent, panel: &MachinePanel) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if panel.account.is_some() {
        return accounts::input_for_key(key).map(Action::Machines);
    }
    if let Some(backup) = &panel.backup {
        return backup::for_key(key, backup);
    }
    if let Some(screen) = &panel.secrets {
        return secrets::for_key(key, screen);
    }
    if panel.usage.is_some() {
        return crate::hub_usage::for_key(key);
    }
    let input = match &panel.edit {
        Some(PanelEdit::Rename(_) | PanelEdit::Public(_)) => match key.code {
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
        Some(PanelEdit::Routes(_)) => match key.code {
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('o' | 'q') => Input::Close,
            KeyCode::Char('k') | KeyCode::Up => Input::Up,
            KeyCode::Char('j') | KeyCode::Down => Input::Down,
            KeyCode::Char(' ') | KeyCode::Enter => Input::Submit,
            KeyCode::Char('K') => Input::Earlier,
            KeyCode::Char('J') => Input::Later,
            KeyCode::Char('s') => Input::SignIn,
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
            step: Step::Confirm(_) | Step::SignedIn(_),
            ..
        }) => match key.code {
            KeyCode::Enter => Input::Submit,
            KeyCode::Esc | KeyCode::Backspace => Input::Close,
            _ => return None,
        },
        Some(AddMachine {
            step: Step::SigningIn(..),
            ..
        }) => match key.code {
            KeyCode::Esc | KeyCode::Backspace => Input::Close,
            _ => return None,
        },
        None => match key.code {
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('m' | 'q') => Input::Close,
            KeyCode::Char('k') | KeyCode::Up => Input::Up,
            KeyCode::Char('j') | KeyCode::Down => Input::Down,
            KeyCode::Home | KeyCode::PageUp => Input::Top,
            KeyCode::End | KeyCode::PageDown => Input::Bottom,
            KeyCode::Char('a') => Input::Add,
            KeyCode::Char('n') => Input::AddAccount,
            KeyCode::Char('e') => Input::Rename,
            KeyCode::Char('d') => Input::Forget,
            KeyCode::Char('b') => Input::Backup,
            KeyCode::Char('s') => Input::Secrets,
            KeyCode::Char('u') => Input::Usage,
            KeyCode::Char('p') => Input::Public,
            KeyCode::Char('o') => Input::Routes,
            KeyCode::Char('t') => Input::Diagnostics,
            KeyCode::Char('r') => return Some(Action::Reconnect),
            KeyCode::Char('?') => return Some(Action::ToggleHelp),
            _ => return None,
        },
    };
    Some(Action::Machines(input))
}

/// Carries out one input while ranking `machine`'s routes, `at` the one selected.
fn routes_input(machine: &Machine, at: &mut usize, input: Input) -> Vec<Effect> {
    let addresses = &machine.addresses;
    let last = addresses.len().saturating_sub(1);
    *at = (*at).min(last);
    let host_id = machine.host_id.clone();
    let moved = |to: usize| {
        let mut addresses = addresses.clone();
        addresses.swap(*at, to);
        Effect::SetAddresses {
            host_id: host_id.clone(),
            addresses,
        }
    };
    match input {
        Input::Up => *at = at.saturating_sub(1),
        Input::Down => *at = (*at + 1).min(last),
        Input::Submit => {
            if let Some(address) = addresses.get(*at) {
                return vec![Effect::SetRoute {
                    host_id: host_id.clone(),
                    address: address.clone(),
                    choice: next_choice(choice(machine, address)),
                }];
            }
        }
        Input::Earlier if *at > 0 => {
            let effect = moved(*at - 1);
            *at -= 1;
            return vec![effect];
        }
        Input::Later if *at < last => {
            let effect = moved(*at + 1);
            *at += 1;
            return vec![effect];
        }
        Input::SignIn => {
            if let Some(address) = addresses.get(*at).filter(|address| is_tunnel(address)) {
                return vec![Effect::FrontDoorSignIn {
                    host_id,
                    address: address.clone(),
                }];
            }
        }
        _ => {}
    }
    Vec::new()
}

/// Whether `address` is a tunnel's URL, which a login proxy may sit in front of.
fn is_tunnel(address: &str) -> bool {
    address.starts_with("wss://") || address.starts_with("ws://")
}

/// What the routes list says of `address`'s login proxy: whether to sign in, or that this
/// device is signed in; `None` for an address without one.
pub fn front_door(machine: &Machine, address: &str) -> Option<&'static str> {
    let door = machine
        .front_doors
        .iter()
        .find(|door| door.address == address)?;
    Some(match (door.sign_in, door.expires_at) {
        (true, None) => "sign in with s",
        (true, Some(_)) => "sign in again with s",
        (false, _) => "signed in",
    })
}

impl App {
    /// Opens the machines panel, with the add dialog if `add`, and asks the machines this
    /// client owns where they back up.
    pub(crate) fn open_machines(&mut self, add: bool) -> Vec<Effect> {
        let panel = self.machine_panel.get_or_insert_with(MachinePanel::default);
        if add && panel.add.is_none() {
            panel.add = Some(AddMachine::default());
        }
        self.ask_links()
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
                    let target = terminal::login_or_install(&self.machines, &host_id, new);
                    self.machine_panel = None;
                    return vec![Effect::AttachTerminal { host_id, target }];
                }
                accounts::Outcome::AddEndpoint(command) => {
                    return vec![accounts::send(account.host_id.clone(), command)];
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
                (PanelEdit::Public(Some(text)), Input::Char(c)) => text.push(c),
                (PanelEdit::Public(Some(text)), Input::Backspace) => {
                    text.pop();
                }
                (PanelEdit::Public(Some(text)), Input::Clear) => text.clear(),
                (PanelEdit::Public(Some(text)), Input::Submit) => {
                    let addresses = public_addresses(text);
                    return selected
                        .map(|host_id| {
                            let sent = PublicSent::Save(host_id.clone(), addresses);
                            send_public(host_id, CommandBody::GetSettings, sent)
                        })
                        .into_iter()
                        .collect();
                }
                (PanelEdit::Forget, Input::Submit) => {
                    panel.edit = None;
                    panel.chosen = None;
                    return selected.map(Effect::ForgetMachine).into_iter().collect();
                }
                (PanelEdit::Routes(at), input) => {
                    let Some(machine) = selected
                        .and_then(|host_id| self.machines.iter().find(|m| m.host_id == host_id))
                    else {
                        return Vec::new();
                    };
                    return routes_input(machine, at, input);
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
                Input::Backup => return self.open_backup(),
                Input::Public => {
                    let Some(at) = panel.selected(&self.machines) else {
                        return Vec::new();
                    };
                    let machine = &self.machines[at];
                    if machine.role != Some(Role::Owner) {
                        self.notice =
                            Some("public addresses: only the machine's owners set them".to_owned());
                        return Vec::new();
                    }
                    panel.edit = Some(PanelEdit::Public(None));
                    let host_id = machine.host_id.clone();
                    let sent = PublicSent::Load(host_id.clone());
                    return vec![send_public(host_id, CommandBody::GetSettings, sent)];
                }
                Input::Diagnostics => {
                    let Some(at) = panel.selected(&self.machines) else {
                        return Vec::new();
                    };
                    let machine = &self.machines[at];
                    if machine.role != Some(Role::Owner) {
                        self.notice = Some(
                            "diagnostics: only the machine's owners turn them on or off".to_owned(),
                        );
                        return Vec::new();
                    }
                    let host_id = machine.host_id.clone();
                    let sent = DiagnosticsSent::Load(host_id.clone());
                    return vec![send_diagnostics(host_id, CommandBody::GetSettings, sent)];
                }
                Input::Routes => {
                    let Some(at) = panel.selected(&self.machines) else {
                        return Vec::new();
                    };
                    if self.machines[at].addresses.is_empty() {
                        self.notice = Some("routes: this machine has no addresses".to_owned());
                        return Vec::new();
                    }
                    panel.edit = Some(PanelEdit::Routes(0));
                }
                Input::Secrets => return self.open_secrets(),
                Input::Usage => return self.open_hub_usage(),
                Input::AddAccount => {
                    let Some(at) = panel.selected(&self.machines) else {
                        return Vec::new();
                    };
                    let host_id = self.machines[at].host_id.clone();
                    match terminal::refusal(&self.machines, &host_id) {
                        Some(refusal) => {
                            self.notice = Some(format!("adding accounts: {refusal}"));
                        }
                        None => {
                            panel.account =
                                Some(AddAccount::for_provider(host_id.clone(), 0, &self.machines));
                            return vec![crate::accounts::ask_hub(host_id)];
                        }
                    }
                }
                Input::Up | Input::Down | Input::Top | Input::Bottom => {
                    let step = match input {
                        Input::Up => -1,
                        Input::Down => 1,
                        Input::Top => isize::MIN,
                        _ => isize::MAX,
                    };
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
            (Step::Edit | Step::Failed(_), Input::Close) | (Step::SigningIn(..), Input::Close) => {
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
            (Step::Edit | Step::Failed(_), Input::Focus(field)) => add.form.focus = field,
            (Step::Confirm(_), Input::Close) => add.step = Step::Edit,
            (Step::Confirm(machine), Input::Submit) => {
                let machine = machine.clone();
                add.step = Step::SigningIn(machine.clone(), None);
                return vec![Effect::LogIn(machine)];
            }
            (Step::SignedIn(_), Input::Submit | Input::Close) => self.machine_panel = None,
            _ => {}
        }
        Vec::new()
    }

    /// Pasted text for the machines panel: into the add dialog's field, or a link to open the
    /// dialog with. Returns whether it took the text; a link pasted while the composer,
    /// one of its dialogs or the PR link prompt has the keys is left to them.
    pub(crate) fn paste_link(&mut self, text: &str) -> bool {
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
        if let Some(screen) = &mut panel.secrets {
            screen.paste(text);
        } else if let Some(field) = panel.account.as_mut().and_then(AddAccount::field) {
            field.push_str(text.lines().next().unwrap_or(""));
        } else if let Some(add) = &mut panel.add
            && matches!(add.step, Step::Edit | Step::Failed(_))
        {
            add.paste(text);
        }
        true
    }

    /// The code the machine gave the login, for the dialog to show while it waits.
    pub(crate) fn login_code(&mut self, code: String) {
        if let Some(Step::SigningIn(_, shown)) = self
            .machine_panel
            .as_mut()
            .and_then(|panel| panel.add.as_mut())
            .map(|add| &mut add.step)
        {
            *shown = Some(code);
        }
    }

    /// The outcome of signing in, for the dialog if it still waits for it.
    pub(crate) fn signed_in(&mut self, result: Result<Box<Machine>, String>) {
        let Some(panel) = &mut self.machine_panel else {
            return;
        };
        let Some(add) = panel
            .add
            .as_mut()
            .filter(|a| matches!(a.step, Step::SigningIn(..)))
        else {
            return;
        };
        match result {
            Ok(machine) => {
                panel.chosen = Some(machine.host_id.clone());
                add.step = Step::SignedIn(machine);
            }
            Err(error) => add.step = Step::Failed(error),
        }
    }
}

impl App {
    /// Folds in a machine's answer to a step of editing its public addresses.
    pub(crate) fn public_sent(
        &mut self,
        sent: PublicSent,
        result: Result<CommandResult, String>,
    ) -> Vec<Effect> {
        let Some(panel) = &mut self.machine_panel else {
            return Vec::new();
        };
        let editing = panel
            .selected(&self.machines)
            .map(|at| &self.machines[at].host_id);
        let host_id = match &sent {
            PublicSent::Load(host_id) | PublicSent::Save(host_id, _) | PublicSent::Set(host_id) => {
                host_id
            }
        };
        // The panel moved on: the answer is for an edit no longer open.
        if editing != Some(host_id) || !matches!(panel.edit, Some(PanelEdit::Public(_))) {
            return Vec::new();
        }
        let settings = match result {
            Ok(CommandResult::Settings { settings, .. }) => Some(settings),
            Ok(_) => None,
            Err(error) => {
                if matches!(sent, PublicSent::Load(_)) {
                    panel.edit = None;
                }
                self.notice = Some(format!("public addresses: {error}"));
                return Vec::new();
            }
        };
        match (sent, settings) {
            (PublicSent::Load(_), Some(settings)) => {
                let text = settings.public_addresses.join(" ");
                panel.edit = Some(PanelEdit::Public(Some(text)));
            }
            (PublicSent::Save(host_id, addresses), Some(mut settings)) => {
                settings.public_addresses = addresses;
                let set = CommandBody::SetSettings { settings };
                return vec![send_public(host_id.clone(), set, PublicSent::Set(host_id))];
            }
            (PublicSent::Set(_), _) => {
                panel.edit = None;
                self.notice = Some(
                    "public addresses saved: every device picks them up when it next connects"
                        .to_owned(),
                );
            }
            (_, None) => {
                panel.edit = None;
                self.notice = Some("public addresses: the machine sent no settings".to_owned());
            }
        }
        Vec::new()
    }
}

impl App {
    /// Folds in a machine's answer to a step of turning its diagnostics on or off.
    pub(crate) fn diagnostics_sent(
        &mut self,
        sent: DiagnosticsSent,
        result: Result<CommandResult, String>,
    ) -> Vec<Effect> {
        let settings = match result {
            Ok(CommandResult::Settings { settings, .. }) => settings,
            Ok(_) => {
                self.notice = Some("diagnostics: the machine sent no settings".to_owned());
                return Vec::new();
            }
            Err(error) => {
                self.notice = Some(format!("diagnostics: {error}"));
                return Vec::new();
            }
        };
        match sent {
            DiagnosticsSent::Load(host_id) => {
                let mut settings = settings;
                let enabled = !settings.telemetry.enabled;
                settings.telemetry.enabled = enabled;
                let set = CommandBody::SetSettings { settings };
                vec![send_diagnostics(
                    host_id.clone(),
                    set,
                    DiagnosticsSent::Set(host_id, enabled),
                )]
            }
            DiagnosticsSent::Set(host_id, enabled) => {
                let name = self
                    .machines
                    .iter()
                    .find(|machine| machine.host_id == host_id)
                    .map_or(host_id.as_str(), |machine| machine.name.as_str());
                self.notice = Some(if enabled {
                    format!(
                        "diagnostics on for {name}: it shares traces, errors and anonymous \
                         measurements, never your work; see herder.sh/privacy"
                    )
                } else {
                    format!("diagnostics off for {name}: it sends nothing")
                });
                Vec::new()
            }
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
        self.step = match self.form.machine() {
            Ok(machine) => Step::Confirm(machine),
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
    use crate::terminal::Target;
    use herder_client_core::FleetPerson;

    const FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn the_panel_pins_disables_and_moves_a_machines_routes() {
        let mut app = App::default();
        let mut box1 = machine("h1", "box", &[]);
        box1.addresses = vec!["192.168.1.5:7447".into(), "100.101.1.2:7447".into()];
        app.update(Msg::Machines(vec![box1.clone()]));
        press(&mut app, KeyCode::Char('m'));
        press(&mut app, KeyCode::Char('o'));
        let edit = |app: &App| app.machine_panel.as_ref().and_then(|p| p.edit.clone());
        assert_eq!(edit(&app), Some(PanelEdit::Routes(0)));
        // Space cycles the selected route: only this, then never, then back in the race.
        press(&mut app, KeyCode::Down);
        let set = |choice| Effect::SetRoute {
            host_id: HostId::new("h1"),
            address: "100.101.1.2:7447".into(),
            choice,
        };
        assert_eq!(
            press(&mut app, KeyCode::Char(' ')),
            [set(RouteChoice::Pinned)]
        );
        box1.pinned_address = Some("100.101.1.2:7447".into());
        app.update(Msg::Machines(vec![box1.clone()]));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            [set(RouteChoice::Disabled)]
        );
        box1.pinned_address = None;
        box1.disabled_addresses = vec!["100.101.1.2:7447".into()];
        app.update(Msg::Machines(vec![box1.clone()]));
        assert_eq!(
            press(&mut app, KeyCode::Char(' ')),
            [set(RouteChoice::Auto)]
        );
        // K tries it earlier; at the top it stays.
        assert_eq!(
            press(&mut app, KeyCode::Char('K')),
            [Effect::SetAddresses {
                host_id: HostId::new("h1"),
                addresses: vec!["100.101.1.2:7447".into(), "192.168.1.5:7447".into()],
            }]
        );
        assert_eq!(edit(&app), Some(PanelEdit::Routes(0)));
        assert!(press(&mut app, KeyCode::Char('K')).is_empty());
        press(&mut app, KeyCode::Esc);
        assert_eq!(edit(&app), None);
        assert!(app.machine_panel.is_some());
    }

    #[test]
    fn a_tunnel_behind_a_login_proxy_is_signed_in_to_from_the_routes() {
        use herder_client_core::FrontDoor;

        let mut app = App::default();
        let mut box1 = machine("h1", "box", &[]);
        let tunnel = "wss://box.example.com".to_owned();
        box1.addresses = vec!["192.168.1.5:7447".into(), tunnel.clone()];
        box1.front_doors = vec![FrontDoor {
            address: tunnel.clone(),
            expires_at: None,
            sign_in: true,
        }];
        app.update(Msg::Machines(vec![box1.clone()]));
        press(&mut app, KeyCode::Char('m'));
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(front_door(&box1, &tunnel), Some("sign in with s"));
        assert_eq!(front_door(&box1, "192.168.1.5:7447"), None);
        // Not on an address of the machine's own network.
        assert!(press(&mut app, KeyCode::Char('s')).is_empty());
        press(&mut app, KeyCode::Down);
        assert_eq!(
            press(&mut app, KeyCode::Char('s')),
            [Effect::FrontDoorSignIn {
                host_id: HostId::new("h1"),
                address: tunnel.clone(),
            }]
        );

        // Signed in, then turned away again: sign in again.
        let expires = Some(herder_protocol::Timestamp::from_second(4_000_000_000).unwrap());
        box1.front_doors[0].expires_at = expires;
        box1.front_doors[0].sign_in = false;
        assert_eq!(front_door(&box1, &tunnel), Some("signed in"));
        box1.front_doors[0].sign_in = true;
        assert_eq!(front_door(&box1, &tunnel), Some("sign in again with s"));
    }

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

    fn target() -> FleetMachine {
        FleetMachine {
            hosts: vec!["127.0.0.1:7447".into()],
            fingerprint: FP.into(),
            name: None,
        }
    }

    fn person() -> FleetPerson {
        FleetPerson {
            name: "alice".into(),
            passkeys: vec!["Y3JlZC0x".into()],
        }
    }

    fn link() -> String {
        FleetLink {
            person: person(),
            machines: vec![target()],
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

    /// The settings `h1` answers with, holding `public`.
    fn settings(public: &[&str]) -> Msg {
        let tmp = tempfile::tempdir().unwrap();
        let mut settings = herder_daemon::Config::load_file(&tmp.path().join("none.toml"))
            .unwrap()
            .settings();
        settings.public_addresses = public.iter().map(|a| (*a).to_owned()).collect();
        Msg::Sent {
            origin: crate::compose::Origin::Public(PublicSent::Load(HostId::new("h1"))),
            result: Ok(CommandResult::Settings {
                settings: Box::new(settings),
                restart_required: false,
                data_dir: "/data".into(),
                is_hub: false,
            }),
        }
    }

    #[test]
    fn an_owner_turns_a_machines_diagnostics_on_and_off() {
        let mut app = App::default();
        let mut member = machine("h2", "laptop", &[]);
        member.role = Some(Role::Member);
        app.update(Msg::Machines(vec![machine("h1", "box", &[]), member]));
        press(&mut app, KeyCode::Char('m'));
        let load = Effect::Send {
            host_id: HostId::new("h1"),
            command: CommandBody::GetSettings,
            origin: crate::compose::Origin::Diagnostics(DiagnosticsSent::Load(HostId::new("h1"))),
        };
        for enabled in [true, false] {
            assert_eq!(
                press(&mut app, KeyCode::Char('t')),
                std::slice::from_ref(&load)
            );
            // The machine answers with its settings, sharing the other way.
            let Msg::Sent { result, .. } = settings(&[]) else {
                unreachable!()
            };
            let result = result.map(|result| match result {
                CommandResult::Settings {
                    mut settings,
                    restart_required,
                    data_dir,
                    is_hub,
                } => {
                    settings.telemetry.enabled = !enabled;
                    CommandResult::Settings {
                        settings,
                        restart_required,
                        data_dir,
                        is_hub,
                    }
                }
                other => other,
            });
            let origin =
                crate::compose::Origin::Diagnostics(DiagnosticsSent::Load(HostId::new("h1")));
            let Some(Effect::Send {
                command: CommandBody::SetSettings { settings },
                origin,
                ..
            }) = app.update(Msg::Sent { origin, result }).pop()
            else {
                panic!("expected the settings saved");
            };
            assert_eq!(settings.telemetry.enabled, enabled);
            assert_eq!(
                origin,
                crate::compose::Origin::Diagnostics(DiagnosticsSent::Set(
                    HostId::new("h1"),
                    enabled
                ))
            );

            app.update(Msg::Sent {
                origin: crate::compose::Origin::Diagnostics(DiagnosticsSent::Set(
                    HostId::new("h1"),
                    enabled,
                )),
                result: settings_result(),
            });
            let notice = app.notice.clone().unwrap();
            let wanted = if enabled {
                "diagnostics on for box"
            } else {
                "diagnostics off for box"
            };
            assert!(notice.starts_with(wanted), "{notice}");
        }
        // A member cannot.
        press(&mut app, KeyCode::Char('j'));
        assert!(press(&mut app, KeyCode::Char('t')).is_empty());
        assert!(
            app.notice
                .as_deref()
                .unwrap()
                .contains("only the machine's owners")
        );
    }

    fn settings_result() -> Result<CommandResult, String> {
        let Msg::Sent { result, .. } = settings(&[]) else {
            unreachable!()
        };
        result
    }

    #[test]
    fn an_owner_sets_a_machines_public_addresses_for_every_device() {
        let mut app = App::default();
        let mut member = machine("h2", "laptop", &[]);
        member.role = Some(Role::Member);
        app.update(Msg::Machines(vec![machine("h1", "box", &[]), member]));
        press(&mut app, KeyCode::Char('m'));
        let load = |host: &str| Effect::Send {
            host_id: HostId::new(host),
            command: CommandBody::GetSettings,
            origin: crate::compose::Origin::Public(PublicSent::Load(HostId::new(host))),
        };
        assert_eq!(press(&mut app, KeyCode::Char('p')), [load("h1")]);
        let edit = |app: &App| app.machine_panel.as_ref().unwrap().edit.clone();
        assert_eq!(edit(&app), Some(PanelEdit::Public(None)));
        app.update(settings(&["203.0.113.7:7447"]));
        assert_eq!(
            edit(&app),
            Some(PanelEdit::Public(Some("203.0.113.7:7447".into())))
        );
        typed(&mut app, ", https://box.example.com");
        let Some(Effect::Send {
            command, origin, ..
        }) = press(&mut app, KeyCode::Enter).pop()
        else {
            panic!("expected the settings asked for");
        };
        assert_eq!(command, CommandBody::GetSettings);
        let addresses = vec![
            "203.0.113.7:7447".to_owned(),
            "https://box.example.com".to_owned(),
        ];
        assert_eq!(
            origin,
            crate::compose::Origin::Public(PublicSent::Save(HostId::new("h1"), addresses.clone()))
        );
        // Saved over the settings as they are then, with only the addresses changed.
        let Msg::Sent { result, .. } = settings(&[]) else {
            unreachable!()
        };
        let mut effects = app.update(Msg::Sent { origin, result });
        let Some(Effect::Send {
            command: CommandBody::SetSettings { settings },
            origin,
            ..
        }) = effects.pop()
        else {
            panic!("expected the settings saved");
        };
        assert_eq!(settings.public_addresses, addresses);
        app.update(Msg::Sent {
            origin,
            result: Err("public_addresses: localhost:7447 is this machine itself".into()),
        });
        assert!(
            app.notice
                .as_deref()
                .unwrap()
                .contains("this machine itself")
        );
        assert!(matches!(edit(&app), Some(PanelEdit::Public(Some(_)))));
        app.update(Msg::Sent {
            origin: crate::compose::Origin::Public(PublicSent::Set(HostId::new("h1"))),
            result: Ok(CommandResult::Applied),
        });
        assert_eq!(edit(&app), None);
        assert!(app.notice.as_deref().unwrap().contains("every device"));

        // A member is told, and asks nothing.
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(press(&mut app, KeyCode::Char('p')), []);
        assert!(app.notice.as_deref().unwrap().contains("owners"));
        assert_eq!(edit(&app), None);
    }

    #[test]
    fn a_typed_link_is_checked_then_signed_in_to() {
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
        assert_eq!(*step(&app), Step::Confirm(target()));
        // Back to the form, and on again.
        press(&mut app, KeyCode::Esc);
        assert_eq!(*step(&app), Step::Edit);
        press(&mut app, KeyCode::Enter);
        assert_eq!(press(&mut app, KeyCode::Enter), [Effect::LogIn(target())]);
        assert_eq!(*step(&app), Step::SigningIn(target(), None));
        // Keys other than Esc do nothing while signing in; the code shows once given.
        assert_eq!(press(&mut app, KeyCode::Enter), []);
        app.update(Msg::LoginCode("K7Q-4PD".into()));
        assert_eq!(
            *step(&app),
            Step::SigningIn(target(), Some("K7Q-4PD".into()))
        );

        let new = machine("h9", "laptop", &[]);
        app.update(Msg::SignedIn(Ok(Box::new(new.clone()))));
        assert_eq!(*step(&app), Step::SignedIn(Box::new(new)));
        assert_eq!(
            app.machine_panel.as_ref().unwrap().chosen,
            Some(HostId::new("h9"))
        );
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.machine_panel, None);
    }

    #[test]
    fn the_fields_name_a_machine() {
        let mut app = App::default();
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Tab);
        typed(&mut app, "box.lan");
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            *step(&app),
            Step::Failed(
                "the fingerprint is 64 hex digits, as the machine's page in the app shows it"
                    .into()
            )
        );
        press(&mut app, KeyCode::Tab);
        typed(&mut app, &FP.to_ascii_uppercase());
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            *step(&app),
            Step::Confirm(FleetMachine {
                hosts: vec!["box.lan:7447".into()],
                fingerprint: FP.into(),
                name: None,
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
    fn a_failed_sign_in_returns_to_the_form_with_the_error() {
        let mut app = App::default();
        app.update(Msg::Paste(format!("{}\n", link())));
        assert!(matches!(step(&app), Step::Confirm(_)));
        press(&mut app, KeyCode::Enter);
        app.update(Msg::SignedIn(Err("nobody approved it in time".into())));
        assert_eq!(
            *step(&app),
            Step::Failed("nobody approved it in time".into())
        );
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
        // A link pasted while signing in changes nothing.
        press(&mut app, KeyCode::Enter);
        app.update(Msg::Paste(link()));
        assert!(matches!(step(&app), Step::SigningIn(..)));
        // Closing while signing in drops the dialog; the late outcome is ignored.
        press(&mut app, KeyCode::Esc);
        app.update(Msg::SignedIn(Ok(Box::new(machine("h9", "laptop", &[])))));
        assert_eq!(app.machine_panel.as_ref().unwrap().add, None);
    }

    #[test]
    fn a_link_of_several_machines_is_added_one_at_a_time() {
        let mut app = App::default();
        let two = FleetLink {
            person: person(),
            machines: vec![target(), target()],
        };
        app.update(Msg::Paste(two.to_string()));
        assert_eq!(
            *step(&app),
            Step::Failed("the link names 2 machines; add them one at a time".into())
        );
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
        assert_eq!(*account(&app).provider(), Provider::Opencode);
        typed(&mut app, " ");
        assert_eq!(*account(&app).provider(), Provider::Claude);
        press(&mut app, KeyCode::Right);
        assert_eq!(account(&app).default_config_dir(), "~/.codex-codex");
        // Past how the account runs, a login by default.
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Tab);
        app.update(Msg::Key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        )));
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
    fn a_missing_cli_starts_the_installer() {
        let mut app = App::default();
        let mut host = machine("h1", "box", &[]);
        host.providers = vec![herder_protocol::ProviderStatus {
            provider: Provider::Claude,
            installed: false,
            version: None,
            latest_version: None,
            binary: Some("claude".into()),
            can_install: true,
            can_update: false,
        }];
        app.update(Msg::Machines(vec![host]));
        press(&mut app, KeyCode::Char('m'));
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            [Effect::AttachTerminal {
                host_id: HostId::new("h1"),
                target: Target::Install(Provider::Claude),
            }]
        );
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
