//! The secrets screen, from the fleet: a vault's sets of secrets, the secrets in each, the
//! hosts each set is granted to and the latest fetches, for the vault's owners. Its state,
//! keys and reducer.
//!
//! `s` on a vault in the fleet opens it: `list_secrets` and `list_secret_audit` to the vault.
//! There the owner creates (`n`) and deletes (`d`) sets, adds (`a`) or replaces (`e`) a secret
//! and deletes it (`d`), and grants or revokes a set to the vault's hosts (`s`), as the vault
//! lists them to clients. Each change is sent to the vault, and the lists asked for again.
//!
//! A value is typed into a masked field, and is never shown, kept or logged: the form holds it
//! only until it is sent, as [`Masked`], whose `Debug` hides it, and the screen draws a mark
//! per character. No answer carries a value back, so replacing one is the only way to change it.

use std::fmt;

use herder_client_core::{ConnectionState, Machine};
use herder_protocol::{
    CommandBody, CommandResult, HostId, MAX_SECRET_DESCRIPTION_CHARS, Role, SecretAccess,
    SecretInfo, SecretSet, SecretValue, is_valid_secret_name, is_valid_secret_set_name,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::Action;
use crate::app::{App, Effect};
use crate::backup::Known;
use crate::compose::Origin;

/// The secrets screen, over the fleet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Secrets {
    /// The vault.
    pub vault: HostId,
    /// Its sets and secrets, as it last listed them.
    pub list: Load<(Vec<SecretSet>, Vec<SecretInfo>)>,
    /// Its latest fetches, newest first.
    pub audit: Load<Vec<SecretAccess>>,
    /// The row the user selected; until they move, the first one.
    pub chosen: Option<Pick>,
    /// A dialog over the screen.
    pub dialog: Option<Dialog>,
}

/// A list the vault was asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Load<T> {
    /// Asked; no answer yet.
    Asking,
    /// The answer.
    Loaded(T),
    /// Why there is none.
    Failed(String),
}

/// A row of the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    /// A set.
    Set(String),
    /// A secret of a set.
    Secret {
        /// Its set.
        set: String,
        /// Its name.
        name: String,
    },
}

impl Pick {
    /// The set the row belongs to.
    pub fn set(&self) -> &str {
        match self {
            Pick::Set(set) | Pick::Secret { set, .. } => set,
        }
    }
}

/// A dialog over the secrets screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dialog {
    /// Naming a new set.
    NewSet(Form<String>),
    /// Adding or replacing a secret.
    Put(Form<PutSecret>),
    /// Granting a set to the vault's hosts, or revoking it.
    Hosts {
        /// The set.
        set: String,
        /// Index of the selected host in [`Secrets::hosts`].
        selected: usize,
        /// A grant or revoke is on its way.
        sending: bool,
        /// Why the last one failed.
        error: Option<String>,
    },
    /// Asking before deleting a set or a secret.
    Delete {
        /// What to delete.
        pick: Pick,
        /// The delete is on its way.
        sending: bool,
        /// Why it failed.
        error: Option<String>,
    },
}

/// A dialog's fields, and how sending them went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Form<T> {
    /// What the user typed.
    pub fields: T,
    /// The command is on its way.
    pub sending: bool,
    /// Why the last try failed.
    pub error: Option<String>,
}

impl<T> Form<T> {
    fn new(fields: T) -> Self {
        Self {
            fields,
            sending: false,
            error: None,
        }
    }
}

/// The fields of a secret to put.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PutSecret {
    /// Its set.
    pub set: String,
    /// Its name; fixed when replacing.
    pub name: String,
    /// What it is for.
    pub description: String,
    /// Its value, until it is sent.
    pub value: Masked,
    /// The field being typed in.
    pub focus: Field,
    /// It replaces a secret the set has.
    pub replacing: bool,
}

impl PutSecret {
    fn fields(&self) -> &'static [Field] {
        if self.replacing {
            &[Field::Description, Field::Value]
        } else {
            &[Field::Name, Field::Description, Field::Value]
        }
    }

    fn field(&mut self) -> &mut String {
        match self.focus {
            Field::Name => &mut self.name,
            Field::Description => &mut self.description,
            Field::Value => &mut self.value.0,
        }
    }

    fn step(&mut self, forward: bool) {
        let fields = self.fields();
        let at = fields.iter().position(|f| *f == self.focus).unwrap_or(0);
        let to = if forward {
            at + 1
        } else {
            at + fields.len() - 1
        };
        self.focus = fields[to % fields.len()];
    }
}

/// A field of the secret form, in tab order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    /// The name.
    Name,
    /// The description.
    Description,
    /// The value.
    Value,
}

/// A value being typed: its `Debug` never shows it, so the app's state can be logged.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Masked(String);

impl Masked {
    /// How many characters are typed, all a screen may know of it.
    pub fn len(&self) -> usize {
        self.0.chars().count()
    }

    /// Whether nothing is typed.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Masked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Masked(‹hidden›)")
    }
}

/// A command sent for the secrets screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sent {
    /// `list_secrets` to the vault.
    List(HostId),
    /// `list_secret_audit` to the vault.
    Audit(HostId),
    /// A change to the vault's secrets, and what to say once it is made.
    Change {
        /// The vault.
        vault: HostId,
        /// What was done, for the status line.
        done: String,
    },
}

/// Input to the secrets screen or its dialogs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// Close the dialog, or the screen.
    Close,
    /// Select the previous row, host or field.
    Up,
    /// Select the next row, host or field.
    Down,
    /// Select the first row.
    Top,
    /// Select the last row.
    Bottom,
    /// Name a new set.
    NewSet,
    /// Add a secret to the selected set.
    Add,
    /// Replace the selected secret.
    Replace,
    /// Grant the selected set to hosts, or revoke it.
    Hosts,
    /// Ask before deleting the selected set or secret.
    Delete,
    /// Ask the vault for its lists again.
    Reload,
    /// Go on: open the selected row, save, toggle a grant, or delete.
    Submit,
    /// Type a character.
    Char(char),
    /// Delete the last character.
    Backspace,
    /// Clear the field.
    Clear,
}

/// The action a key asks for while the screen is open.
pub fn for_key(key: KeyEvent, secrets: &Secrets) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let input = match &secrets.dialog {
        Some(Dialog::NewSet(_) | Dialog::Put(_)) => match key.code {
            KeyCode::Esc => Input::Close,
            KeyCode::Enter => Input::Submit,
            KeyCode::Tab | KeyCode::Down => Input::Down,
            KeyCode::BackTab | KeyCode::Up => Input::Up,
            KeyCode::Backspace => Input::Backspace,
            KeyCode::Char('u') if ctrl => Input::Clear,
            KeyCode::Char(c) if !ctrl => Input::Char(c),
            _ => return None,
        },
        Some(Dialog::Hosts { .. }) => match key.code {
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('q') => Input::Close,
            KeyCode::Enter | KeyCode::Char(' ') => Input::Submit,
            KeyCode::Char('k') | KeyCode::Up => Input::Up,
            KeyCode::Char('j') | KeyCode::Down => Input::Down,
            _ => return None,
        },
        Some(Dialog::Delete { .. }) => match key.code {
            KeyCode::Enter | KeyCode::Char('y') => Input::Submit,
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('n') => Input::Close,
            _ => return None,
        },
        None => match key.code {
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Left | KeyCode::Char('q') => Input::Close,
            KeyCode::Char('k') | KeyCode::Up => Input::Up,
            KeyCode::Char('j') | KeyCode::Down => Input::Down,
            KeyCode::Char('g') | KeyCode::Home | KeyCode::PageUp => Input::Top,
            KeyCode::Char('G') | KeyCode::End | KeyCode::PageDown => Input::Bottom,
            KeyCode::Char('n') => Input::NewSet,
            KeyCode::Char('a') => Input::Add,
            KeyCode::Char('e') => Input::Replace,
            KeyCode::Char('s') => Input::Hosts,
            KeyCode::Char('d') => Input::Delete,
            KeyCode::Char('r') => Input::Reload,
            KeyCode::Enter => Input::Submit,
            KeyCode::Char('?') => return Some(Action::ToggleHelp),
            _ => return None,
        },
    };
    Some(Action::Secrets(input))
}

impl Secrets {
    fn new(vault: HostId) -> Self {
        Self {
            vault,
            list: Load::Asking,
            audit: Load::Asking,
            chosen: None,
            dialog: None,
        }
    }

    /// Whether a form is open, whose fields take typed keys.
    pub fn types(&self) -> bool {
        matches!(self.dialog, Some(Dialog::NewSet(_) | Dialog::Put(_)))
    }

    /// Pasted text, into the field being typed in: its first line, as a value is one.
    pub fn paste(&mut self, text: &str) {
        let line = text.lines().next().unwrap_or("");
        match &mut self.dialog {
            Some(Dialog::NewSet(form)) if !form.sending => form.fields.push_str(line),
            Some(Dialog::Put(form)) if !form.sending => form.fields.field().push_str(line),
            _ => {}
        }
    }

    /// Every row: each set, then its secrets.
    pub fn rows(&self) -> Vec<Pick> {
        let Load::Loaded((sets, secrets)) = &self.list else {
            return Vec::new();
        };
        sets.iter()
            .flat_map(|set| {
                std::iter::once(Pick::Set(set.name.clone())).chain(
                    secrets
                        .iter()
                        .filter(|secret| secret.set == set.name)
                        .map(|secret| Pick::Secret {
                            set: secret.set.clone(),
                            name: secret.name.clone(),
                        }),
                )
            })
            .collect()
    }

    /// Index of the selected row in `rows`: the chosen one, else the first.
    pub fn selected(&self, rows: &[Pick]) -> Option<usize> {
        match &self.chosen {
            Some(chosen) => rows.iter().position(|row| row == chosen),
            None => (!rows.is_empty()).then_some(0),
        }
    }

    /// The selected row.
    pub fn pick(&self) -> Option<Pick> {
        let rows = self.rows();
        self.selected(&rows).map(|at| rows[at].clone())
    }

    /// The set named `name`, as last listed.
    pub fn set(&self, name: &str) -> Option<&SecretSet> {
        match &self.list {
            Load::Loaded((sets, _)) => sets.iter().find(|set| set.name == name),
            _ => None,
        }
    }

    /// The secret `name` of `set`, as last listed.
    pub fn secret(&self, set: &str, name: &str) -> Option<&SecretInfo> {
        match &self.list {
            Load::Loaded((_, secrets)) => secrets.iter().find(|s| s.set == set && s.name == name),
            _ => None,
        }
    }

    /// The hosts a set can be granted to: the vault's, then any the set is granted to that
    /// the vault no longer lists. Each with its name, if the vault lists it.
    pub fn hosts(&self, vault: Option<&Machine>, set: &str) -> Vec<(HostId, Option<String>)> {
        let mut hosts: Vec<(HostId, Option<String>)> = vault
            .map(|vault| {
                vault
                    .hosts
                    .iter()
                    .map(|host| (host.host_id.clone(), Some(host.host_name.clone())))
                    .collect()
            })
            .unwrap_or_default();
        for granted in self
            .set(set)
            .map(|s| s.hosts.as_slice())
            .unwrap_or_default()
        {
            if !hosts.iter().any(|(host, _)| host == granted) {
                hosts.push((granted.clone(), None));
            }
        }
        hosts
    }
}

/// The commands that ask `vault` for its secrets and its latest fetches.
fn reload(vault: &HostId) -> Vec<Effect> {
    vec![
        Effect::Send {
            host_id: vault.clone(),
            command: CommandBody::ListSecrets,
            origin: Origin::Secrets(Sent::List(vault.clone())),
        },
        Effect::Send {
            host_id: vault.clone(),
            command: CommandBody::ListSecretAudit { before: None },
            origin: Origin::Secrets(Sent::Audit(vault.clone())),
        },
    ]
}

/// A change to send to `vault`, saying `done` once made.
fn change(vault: &HostId, command: CommandBody, done: String) -> Effect {
    Effect::Send {
        host_id: vault.clone(),
        command,
        origin: Origin::Secrets(Sent::Change {
            vault: vault.clone(),
            done,
        }),
    }
}

impl App {
    /// Opens the secrets screen for the selected machine, or says why it cannot.
    pub(crate) fn open_secrets(&mut self) -> Vec<Effect> {
        let Some(panel) = &self.machine_panel else {
            return Vec::new();
        };
        let Some(at) = panel.selected(&self.machines) else {
            return Vec::new();
        };
        let machine = &self.machines[at];
        let known = panel.links.get(&machine.host_id);
        let refusal = if machine.role.is_some_and(|role| role != Role::Owner) {
            Some("secrets: only the vault's owners can manage them".to_owned())
        } else if machine.connection != ConnectionState::Connected {
            Some("secrets: the machine is not connected".to_owned())
        } else if matches!(known, Some(Known::Linked(_) | Known::Unlinked)) {
            Some(format!(
                "secrets: {} is not a vault; secrets live on a vault, so select it and press s",
                machine.name
            ))
        } else {
            None
        };
        if let Some(refusal) = refusal {
            self.notice = Some(refusal);
            return Vec::new();
        }
        let vault = machine.host_id.clone();
        if let Some(panel) = &mut self.machine_panel {
            panel.secrets = Some(Secrets::new(vault.clone()));
        }
        reload(&vault)
    }

    /// Whether the fleet's selected machine is a vault, as it said.
    pub(crate) fn on_vault(&self) -> bool {
        self.machine_panel.as_ref().is_some_and(|panel| {
            panel.selected(&self.machines).is_some_and(|at| {
                panel.links.get(&self.machines[at].host_id) == Some(&Known::Vault)
            })
        })
    }

    fn secrets_mut(&mut self) -> Option<&mut Secrets> {
        self.machine_panel.as_mut()?.secrets.as_mut()
    }

    /// Carries out one input to the secrets screen.
    pub(crate) fn secrets_input(&mut self, input: Input) -> Vec<Effect> {
        let Some(secrets) = self.machine_panel.as_ref().and_then(|p| p.secrets.as_ref()) else {
            return Vec::new();
        };
        let vault = secrets.vault.clone();
        let machine = self.machines.iter().find(|m| m.host_id == vault).cloned();
        let Some(panel) = &mut self.machine_panel else {
            return Vec::new();
        };
        let Some(secrets) = &mut panel.secrets else {
            return Vec::new();
        };
        if secrets.dialog.is_none() && input == Input::Close {
            panel.secrets = None;
            return Vec::new();
        }
        match secrets.dialog.take() {
            None => secrets.screen_input(input, &vault),
            Some(dialog) => {
                let (dialog, effects) = dialog_input(secrets, dialog, input, machine.as_ref());
                secrets.dialog = dialog;
                effects
            }
        }
        .unwrap_or_else(|notice| {
            self.notice = Some(notice);
            Vec::new()
        })
    }

    /// The vault's answer to a command the secrets screen sent.
    pub(crate) fn secrets_sent(
        &mut self,
        sent: Sent,
        result: Result<CommandResult, String>,
    ) -> Vec<Effect> {
        let vault = match &sent {
            Sent::List(vault) | Sent::Audit(vault) | Sent::Change { vault, .. } => vault.clone(),
        };
        let Some(secrets) = self.secrets_mut().filter(|s| s.vault == vault) else {
            return Vec::new();
        };
        match (sent, result) {
            (
                Sent::List(_),
                Ok(CommandResult::Secrets {
                    sets,
                    secrets: list,
                }),
            ) => {
                secrets.list = Load::Loaded((sets, list));
            }
            (Sent::List(_), other) => secrets.list = Load::Failed(failure(other)),
            (Sent::Audit(_), Ok(CommandResult::SecretAudit { entries })) => {
                secrets.audit = Load::Loaded(entries);
            }
            (Sent::Audit(_), other) => secrets.audit = Load::Failed(failure(other)),
            (Sent::Change { done, .. }, Ok(_)) => {
                // Granting stays open, to grant or revoke more hosts.
                match &mut secrets.dialog {
                    Some(Dialog::Hosts { sending, error, .. }) => {
                        *sending = false;
                        *error = None;
                    }
                    _ => secrets.dialog = None,
                }
                self.notice = Some(done);
                return reload(&vault);
            }
            (Sent::Change { .. }, Err(why)) => match &mut secrets.dialog {
                Some(
                    Dialog::NewSet(Form { sending, error, .. })
                    | Dialog::Put(Form { sending, error, .. })
                    | Dialog::Hosts { sending, error, .. }
                    | Dialog::Delete { sending, error, .. },
                ) => {
                    *sending = false;
                    *error = Some(why);
                }
                None => self.notice = Some(why),
            },
        }
        Vec::new()
    }
}

/// Why a list did not come: the vault's refusal, or an answer of the wrong kind.
fn failure(result: Result<CommandResult, String>) -> String {
    match result {
        Ok(other) => format!("unexpected answer {other:?}"),
        Err(why) => why,
    }
}

impl Secrets {
    /// An input with no dialog open: what to send, or a notice for the status line.
    fn screen_input(&mut self, input: Input, vault: &HostId) -> Result<Vec<Effect>, String> {
        let rows = self.rows();
        let at = self.selected(&rows);
        let last = rows.len().saturating_sub(1);
        let go = |to: usize| rows.get(to).cloned();
        let pick = at.map(|at| rows[at].clone());
        match input {
            Input::Up => self.chosen = at.and_then(|at| go(at.saturating_sub(1))),
            Input::Down => self.chosen = at.and_then(|at| go((at + 1).min(last))),
            Input::Top => self.chosen = go(0),
            Input::Bottom => self.chosen = go(last),
            Input::Reload => {
                self.list = Load::Asking;
                self.audit = Load::Asking;
                return Ok(reload(vault));
            }
            Input::NewSet if matches!(self.list, Load::Loaded(_)) => {
                self.dialog = Some(Dialog::NewSet(Form::new(String::new())));
            }
            Input::Add => {
                let Some(pick) = pick else {
                    return Err("secrets: create a set first, with n".to_owned());
                };
                self.dialog = Some(Dialog::Put(Form::new(PutSecret {
                    set: pick.set().to_owned(),
                    name: String::new(),
                    description: String::new(),
                    value: Masked::default(),
                    focus: Field::Name,
                    replacing: false,
                })));
            }
            Input::Replace | Input::Submit if matches!(pick, Some(Pick::Secret { .. })) => {
                let Some(Pick::Secret { set, name }) = pick else {
                    return Ok(Vec::new());
                };
                let description = self
                    .secret(&set, &name)
                    .map(|s| s.description.clone())
                    .unwrap_or_default();
                self.dialog = Some(Dialog::Put(Form::new(PutSecret {
                    set,
                    name,
                    description,
                    value: Masked::default(),
                    focus: Field::Value,
                    replacing: true,
                })));
            }
            Input::Replace => return Err("secrets: select a secret to replace".to_owned()),
            Input::Hosts | Input::Submit => {
                if let Some(pick) = pick {
                    self.dialog = Some(Dialog::Hosts {
                        set: pick.set().to_owned(),
                        selected: 0,
                        sending: false,
                        error: None,
                    });
                }
            }
            Input::Delete => {
                if let Some(pick) = pick {
                    self.dialog = Some(Dialog::Delete {
                        pick,
                        sending: false,
                        error: None,
                    });
                }
            }
            Input::Close | Input::NewSet | Input::Char(_) | Input::Backspace | Input::Clear => {}
        }
        Ok(Vec::new())
    }
}

/// An input to `dialog`, open over `secrets`: the dialog after it, if still open, and what to
/// send, or a notice.
type Next = (Option<Dialog>, Result<Vec<Effect>, String>);

fn dialog_input(
    secrets: &Secrets,
    dialog: Dialog,
    input: Input,
    machine: Option<&Machine>,
) -> Next {
    let vault = &secrets.vault;
    let busy = match &dialog {
        Dialog::NewSet(form) => form.sending,
        Dialog::Put(form) => form.sending,
        Dialog::Hosts { sending, .. } | Dialog::Delete { sending, .. } => *sending,
    };
    // Keys wait while a change is on its way, but for closing.
    if busy && input != Input::Close {
        return (Some(dialog), Ok(Vec::new()));
    }
    if input == Input::Close {
        return (None, Ok(Vec::new()));
    }
    match dialog {
        Dialog::NewSet(mut form) => {
            match input {
                Input::Char(c) => form.fields.push(c),
                Input::Backspace => {
                    form.fields.pop();
                }
                Input::Clear => form.fields.clear(),
                Input::Submit => {
                    let set = form.fields.trim().to_owned();
                    if !is_valid_secret_set_name(&set) {
                        form.error = Some(
                            "a set's name is lowercase letters, digits and single hyphens, \
                             such as prod or coingate-prod"
                                .to_owned(),
                        );
                        return (Some(Dialog::NewSet(form)), Ok(Vec::new()));
                    }
                    form.sending = true;
                    form.error = None;
                    let done = format!("created the set {set}");
                    let command = CommandBody::CreateSecretSet { set };
                    return (
                        Some(Dialog::NewSet(form)),
                        Ok(vec![change(vault, command, done)]),
                    );
                }
                _ => {}
            }
            (Some(Dialog::NewSet(form)), Ok(Vec::new()))
        }
        Dialog::Put(mut form) => {
            match input {
                Input::Char(c) => form.fields.field().push(c),
                Input::Backspace => {
                    form.fields.field().pop();
                }
                Input::Clear => form.fields.field().clear(),
                Input::Down => form.fields.step(true),
                Input::Up => form.fields.step(false),
                Input::Submit => return put(vault, form),
                _ => {}
            }
            (Some(Dialog::Put(form)), Ok(Vec::new()))
        }
        Dialog::Hosts {
            set,
            mut selected,
            sending,
            error,
        } => {
            let hosts = secrets.hosts(machine, &set);
            let last = hosts.len().saturating_sub(1);
            selected = selected.min(last);
            let mut effects = Vec::new();
            let mut sending = sending;
            match input {
                Input::Up => selected = selected.saturating_sub(1),
                Input::Down => selected = (selected + 1).min(last),
                Input::Submit => {
                    if let Some((host_id, name)) = hosts.get(selected) {
                        let name = name.clone().unwrap_or_else(|| host_id.to_string());
                        let granted = secrets.set(&set).is_some_and(|s| s.hosts.contains(host_id));
                        let (command, done) = if granted {
                            (
                                CommandBody::RevokeSecretSet {
                                    set: set.clone(),
                                    host_id: host_id.clone(),
                                    host_use: false,
                                },
                                format!("revoked {set} from {name}"),
                            )
                        } else {
                            (
                                CommandBody::GrantSecretSet {
                                    set: set.clone(),
                                    host_id: host_id.clone(),
                                    host_use: false,
                                },
                                format!("granted {set} to {name}"),
                            )
                        };
                        sending = true;
                        effects.push(change(vault, command, done));
                    }
                }
                _ => {}
            }
            let dialog = Dialog::Hosts {
                set,
                selected,
                sending,
                error: if effects.is_empty() { error } else { None },
            };
            (Some(dialog), Ok(effects))
        }
        Dialog::Delete { pick, .. } => {
            if input != Input::Submit {
                return (
                    Some(Dialog::Delete {
                        pick,
                        sending: false,
                        error: None,
                    }),
                    Ok(Vec::new()),
                );
            }
            let (command, done) = match &pick {
                Pick::Set(set) => (
                    CommandBody::DeleteSecretSet { set: set.clone() },
                    format!("deleted the set {set}"),
                ),
                Pick::Secret { set, name } => (
                    CommandBody::DeleteSecret {
                        set: set.clone(),
                        name: name.clone(),
                    },
                    format!("deleted {set}/{name}"),
                ),
            };
            let dialog = Dialog::Delete {
                pick,
                sending: true,
                error: None,
            };
            (Some(dialog), Ok(vec![change(vault, command, done)]))
        }
    }
}

/// Sends the secret `form` describes, taking its value out of the form; or says what is wrong
/// with it.
fn put(vault: &HostId, mut form: Form<PutSecret>) -> Next {
    let fields = &mut form.fields;
    let name = fields.name.trim().to_owned();
    let description = fields.description.trim().to_owned();
    let error = if !is_valid_secret_name(&name) {
        Some("a secret's name is an environment variable's: A-Z, 0-9 and _, such as API_KEY")
    } else if description.is_empty() {
        Some("say what the secret is for: agents read it to pick one")
    } else if description.chars().count() > MAX_SECRET_DESCRIPTION_CHARS {
        Some("the description is one line of at most 200 characters")
    } else if fields.value.is_empty() {
        Some("enter the value")
    } else {
        None
    };
    if let Some(error) = error {
        form.error = Some(error.to_owned());
        return (Some(Dialog::Put(form)), Ok(Vec::new()));
    }
    // From here the value is the command's alone: the form forgets it, sent or refused.
    let value = SecretValue::new(std::mem::take(&mut fields.value.0));
    let Some(value) = value else {
        fields.focus = Field::Value;
        form.error = Some("the value is too long, or holds a NUL; enter it again".to_owned());
        return (Some(Dialog::Put(form)), Ok(Vec::new()));
    };
    let set = fields.set.clone();
    let done = if fields.replacing {
        format!("replaced {set}/{name}")
    } else {
        format!("saved {set}/{name}")
    };
    let command = CommandBody::PutSecret {
        set,
        name,
        description,
        value,
    };
    form.sending = true;
    form.error = None;
    (
        Some(Dialog::Put(form)),
        Ok(vec![change(vault, command, done)]),
    )
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyEvent;

    use super::*;
    use crate::app::Msg;
    use crate::fake;

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    fn screen(app: &App) -> &Secrets {
        app.machine_panel
            .as_ref()
            .and_then(|panel| panel.secrets.as_ref())
            .expect("the secrets screen is open")
    }

    /// The one command `effects` sends to the vault, answered with `result`.
    fn answer(app: &mut App, effects: Vec<Effect>, result: Result<CommandResult, String>) {
        let origin = sent(effects).1;
        app.update(Msg::Sent { origin, result });
    }

    /// The one command `effects` sends to the vault, with what for.
    fn sent(effects: Vec<Effect>) -> (CommandBody, Origin) {
        let [
            Effect::Send {
                host_id,
                command,
                origin,
            },
        ] = <[Effect; 1]>::try_from(effects).expect("one command")
        else {
            panic!("expected one command");
        };
        assert_eq!(host_id, HostId::new("v"));
        (command, origin)
    }

    fn commands(effects: &[Effect]) -> Vec<CommandBody> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Send { command, .. } => Some(command.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn s_on_the_vault_asks_it_for_its_secrets_and_their_fetches() {
        let mut app = fake::backups();
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        let effects = press(&mut app, KeyCode::Char('s'));
        assert_eq!(
            commands(&effects),
            [
                CommandBody::ListSecrets,
                CommandBody::ListSecretAudit { before: None }
            ]
        );
        assert_eq!(screen(&app).list, Load::Asking);
        // r asks again.
        let mut app = fake::secrets();
        let effects = press(&mut app, KeyCode::Char('r'));
        assert_eq!(effects.len(), 2);
        assert_eq!(screen(&app).list, Load::Asking);
    }

    #[test]
    fn a_machine_that_is_not_a_vault_says_so() {
        let mut app = fake::backups();
        assert_eq!(press(&mut app, KeyCode::Char('s')), []);
        assert!(app.machine_panel.as_ref().unwrap().secrets.is_none());
        assert_eq!(
            app.notice.as_deref(),
            Some(
                "secrets: devbox is not a vault; secrets live on a vault, so select it and press s"
            )
        );
    }

    #[test]
    fn a_vault_without_a_secrets_key_says_why() {
        let mut app = fake::secrets();
        let effects = press(&mut app, KeyCode::Char('r'));
        let refusal = "this vault has no secrets key: set [vault] secrets_key_file";
        for effect in effects {
            let Effect::Send { origin, .. } = effect else {
                continue;
            };
            app.update(Msg::Sent {
                origin,
                result: Err(refusal.to_owned()),
            });
        }
        assert_eq!(screen(&app).list, Load::Failed(refusal.to_owned()));
        assert!(screen(&app).rows().is_empty());
    }

    #[test]
    fn rows_are_each_set_then_its_secrets() {
        let app = fake::secrets();
        let secret = |set: &str, name: &str| Pick::Secret {
            set: set.into(),
            name: name.into(),
        };
        assert_eq!(
            screen(&app).rows(),
            [
                Pick::Set("coingate-prod".into()),
                secret("coingate-prod", "API_KEY"),
                secret("coingate-prod", "DB_URL"),
                Pick::Set("personal".into()),
                secret("personal", "GITHUB_TOKEN"),
                Pick::Set("staging".into()),
            ]
        );
    }

    #[test]
    fn n_creates_a_set_once_its_name_is_valid() {
        let mut app = fake::secrets();
        press(&mut app, KeyCode::Char('n'));
        typed(&mut app, "Bad Name");
        assert_eq!(press(&mut app, KeyCode::Enter), []);
        let Some(Dialog::NewSet(form)) = &screen(&app).dialog else {
            panic!("the new-set dialog is open");
        };
        assert!(form.error.is_some());
        app.update(Msg::Key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        )));
        typed(&mut app, "ci");
        let effects = press(&mut app, KeyCode::Enter);
        assert_eq!(
            commands(&effects),
            [CommandBody::CreateSecretSet { set: "ci".into() }]
        );
        // Applied: the dialog closes and the lists are asked for again.
        let (_, origin) = sent(effects);
        let effects = app.update(Msg::Sent {
            origin,
            result: Ok(CommandResult::Applied),
        });
        assert_eq!(screen(&app).dialog, None);
        assert_eq!(app.notice.as_deref(), Some("created the set ci"));
        assert_eq!(
            commands(&effects),
            [
                CommandBody::ListSecrets,
                CommandBody::ListSecretAudit { before: None }
            ]
        );
    }

    #[test]
    fn a_adds_a_secret_to_the_selected_set_and_forgets_its_value_once_sent() {
        let mut app = fake::secrets();
        // On personal's GITHUB_TOKEN: the new secret goes to personal.
        for _ in 0..4 {
            press(&mut app, KeyCode::Char('j'));
        }
        press(&mut app, KeyCode::Char('a'));
        typed(&mut app, "NPM_TOKEN");
        press(&mut app, KeyCode::Tab);
        typed(&mut app, "publishes packages to npm");
        press(&mut app, KeyCode::Tab);
        typed(&mut app, "npm_s3cr3t");
        let Some(Dialog::Put(form)) = &screen(&app).dialog else {
            panic!("the secret dialog is open");
        };
        assert_eq!(form.fields.value.len(), 10);
        assert!(!format!("{:?}", app.machine_panel).contains("npm_s3cr3t"));
        let effects = press(&mut app, KeyCode::Enter);
        assert_eq!(
            commands(&effects),
            [CommandBody::PutSecret {
                set: "personal".into(),
                name: "NPM_TOKEN".into(),
                description: "publishes packages to npm".into(),
                value: SecretValue::new("npm_s3cr3t").unwrap(),
            }]
        );
        // Sent: the form keeps no value, and the effect's own Debug hides it.
        let Some(Dialog::Put(form)) = &screen(&app).dialog else {
            panic!("the secret dialog waits for the vault");
        };
        assert!(form.fields.value.is_empty());
        assert!(!format!("{effects:?}").contains("npm_s3cr3t"));
        // Refused: the value is typed again.
        answer(&mut app, effects, Err("not_found: no set personal".into()));
        let Some(Dialog::Put(form)) = &screen(&app).dialog else {
            panic!("the secret dialog stays open");
        };
        assert_eq!(form.error.as_deref(), Some("not_found: no set personal"));
        assert!(form.fields.value.is_empty());
        assert_eq!(form.fields.name, "NPM_TOKEN");
    }

    #[test]
    fn a_secret_needs_a_valid_name_a_description_and_a_value() {
        let mut app = fake::secrets();
        press(&mut app, KeyCode::Char('a'));
        typed(&mut app, "lower");
        assert_eq!(press(&mut app, KeyCode::Enter), []);
        app.update(Msg::Key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        )));
        typed(&mut app, "OK");
        press(&mut app, KeyCode::Tab);
        typed(&mut app, "something");
        // No value yet.
        assert_eq!(press(&mut app, KeyCode::Enter), []);
        let Some(Dialog::Put(form)) = &screen(&app).dialog else {
            panic!("the secret dialog is open");
        };
        assert_eq!(form.error.as_deref(), Some("enter the value"));
    }

    #[test]
    fn enter_on_a_secret_replaces_its_value_keeping_its_name() {
        let mut app = fake::secrets();
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        let Some(Dialog::Put(form)) = &screen(&app).dialog else {
            panic!("the secret dialog is open");
        };
        assert!(form.fields.replacing);
        assert_eq!(form.fields.focus, Field::Value);
        assert_eq!(form.fields.description, "coingate merchant api");
        // The name is fixed: Tab goes between the description and the value only.
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Tab);
        typed(&mut app, "new-value");
        let effects = press(&mut app, KeyCode::Enter);
        assert_eq!(
            commands(&effects),
            [CommandBody::PutSecret {
                set: "coingate-prod".into(),
                name: "API_KEY".into(),
                description: "coingate merchant api".into(),
                value: SecretValue::new("new-value").unwrap(),
            }]
        );
        answer(&mut app, effects, Ok(CommandResult::Applied));
        assert_eq!(screen(&app).dialog, None);
        assert_eq!(
            app.notice.as_deref(),
            Some("replaced coingate-prod/API_KEY")
        );
    }

    #[test]
    fn d_deletes_the_selected_secret_or_set_after_asking() {
        let mut app = fake::secrets();
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('d'));
        // n keeps it.
        assert_eq!(press(&mut app, KeyCode::Char('n')), []);
        assert_eq!(screen(&app).dialog, None);
        press(&mut app, KeyCode::Char('d'));
        let effects = press(&mut app, KeyCode::Char('y'));
        assert_eq!(
            commands(&effects),
            [CommandBody::DeleteSecret {
                set: "coingate-prod".into(),
                name: "DB_URL".into(),
            }]
        );
        answer(&mut app, effects, Ok(CommandResult::Applied));
        assert_eq!(screen(&app).dialog, None);

        press(&mut app, KeyCode::Char('G'));
        press(&mut app, KeyCode::Char('d'));
        let effects = press(&mut app, KeyCode::Enter);
        assert_eq!(
            commands(&effects),
            [CommandBody::DeleteSecretSet {
                set: "staging".into()
            }]
        );
        // A set with secrets is refused: the vault says why, in the dialog.
        answer(&mut app, effects, Err("the set holds secrets".into()));
        assert!(matches!(
            &screen(&app).dialog,
            Some(Dialog::Delete { error: Some(why), sending: false, .. }) if why == "the set holds secrets"
        ));
    }

    #[test]
    fn s_grants_a_set_to_a_host_of_the_vault_or_revokes_it() {
        let mut app = fake::secrets();
        // coingate-prod is granted to devbox, not laptop.
        press(&mut app, KeyCode::Char('s'));
        let effects = press(&mut app, KeyCode::Enter);
        assert_eq!(
            commands(&effects),
            [CommandBody::RevokeSecretSet {
                set: "coingate-prod".into(),
                host_id: HostId::new("devbox"),
                host_use: false,
            }]
        );
        // Keys wait for the vault.
        assert_eq!(press(&mut app, KeyCode::Char('j')), []);
        answer(&mut app, effects, Ok(CommandResult::Applied));
        assert_eq!(
            app.notice.as_deref(),
            Some("revoked coingate-prod from devbox")
        );
        // The dialog stays, for the next host.
        press(&mut app, KeyCode::Char('j'));
        let effects = press(&mut app, KeyCode::Char(' '));
        assert_eq!(
            commands(&effects),
            [CommandBody::GrantSecretSet {
                set: "coingate-prod".into(),
                host_id: HostId::new("laptop"),
                host_use: false,
            }]
        );
        answer(&mut app, effects, Ok(CommandResult::Applied));
        assert!(matches!(screen(&app).dialog, Some(Dialog::Hosts { .. })));
        press(&mut app, KeyCode::Esc);
        assert_eq!(screen(&app).dialog, None);
        press(&mut app, KeyCode::Esc);
        assert!(app.machine_panel.as_ref().unwrap().secrets.is_none());
        assert!(app.machine_panel.is_some());
    }
}
