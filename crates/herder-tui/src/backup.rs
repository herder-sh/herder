//! Backing a machine up to a vault, from the machines panel: its state, keys and reducer.
//!
//! The client pairs with the machine and with the vault as their owner, once. `b` on a machine
//! then opens the backup dialog: pick one of the vaults paired here, and the client asks that
//! vault for a host-only code (`pair_vault_host`) and hands the machine the vault's address,
//! fingerprint and code (`link_vault`); the machine keeps the vault in its config and
//! replicates there from then on. On a machine that backs up already the dialog stops it:
//! `unlink_vault` on the machine, then `revoke_vault_host` on the vault, so the machine's key
//! no longer opens it.
//!
//! Which machines are vaults, and where each backs up, comes from `get_vault_link`, asked of
//! every connected machine this client owns when the panel opens and when the dialog does.

use std::collections::HashMap;

use herder_client_core::{ConnectionState, Machine};
use herder_protocol::{CommandBody, CommandResult, HostId, LinkedVault, Role};
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::action::Action;
use crate::app::{App, Effect};
use crate::compose::Origin;

/// Longest host name the vault takes as a user name.
const MAX_NAME: usize = 64;

/// What a machine said about backing up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Known {
    /// Asked; no answer yet.
    Asking,
    /// It is a vault, which machines back up to.
    Vault,
    /// It backs up to this vault.
    Linked(LinkedVault),
    /// It backs up nowhere.
    Unlinked,
    /// It could not say, as a daemon from before backups could be linked.
    Unknown(String),
}

/// The backup dialog, over the machines panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backup {
    /// The machine to back up.
    pub host: HostId,
    /// Index of the chosen vault in [`App::vaults`].
    pub selected: usize,
    /// Where it stands.
    pub step: Step,
}

/// Where the backup dialog stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Showing where the machine backs up, or the vaults to pick from.
    Choose,
    /// Asking before stopping.
    ConfirmStop,
    /// Getting a code from `vault`, then handing it to the machine.
    Linking {
        /// The vault.
        vault: HostId,
    },
    /// Stopping, then revoking the machine on its vault.
    Stopping,
    /// Done: what happened.
    Done(String),
    /// Failed: why; the dialog can be tried again.
    Failed(String),
}

/// A command sent for the backup dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sent {
    /// `get_vault_link` to a machine.
    Ask(HostId),
    /// `pair_vault_host` to `vault`, for `host`.
    Code {
        /// The machine to back up.
        host: HostId,
        /// The vault.
        vault: HostId,
    },
    /// `link_vault` to `host`.
    Link {
        /// The machine.
        host: HostId,
        /// The vault it links to.
        vault: HostId,
    },
    /// `unlink_vault` to `host`, which backed up to the vault `vault` is paired here as, if
    /// it is.
    Unlink {
        /// The machine.
        host: HostId,
        /// The vault, as paired here.
        vault: Option<HostId>,
    },
    /// `revoke_vault_host` of `host` to its vault.
    Revoke {
        /// The machine.
        host: HostId,
    },
}

/// Input to the backup dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// Close the dialog, or go back a step.
    Close,
    /// Choose the previous vault.
    Up,
    /// Choose the next vault.
    Down,
    /// Go on: link to the chosen vault, ask before stopping, stop, or close once done.
    Submit,
}

/// The action a key asks for while the dialog is open.
pub fn for_key(key: KeyEvent, backup: &Backup) -> Option<Action> {
    let input = match (&backup.step, key.code) {
        (Step::ConfirmStop, KeyCode::Char('y')) => Input::Submit,
        (Step::ConfirmStop, KeyCode::Char('n')) => Input::Close,
        (_, KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('q')) => Input::Close,
        (_, KeyCode::Enter) => Input::Submit,
        (_, KeyCode::Char('k') | KeyCode::Up) => Input::Up,
        (_, KeyCode::Char('j') | KeyCode::Down) => Input::Down,
        _ => return None,
    };
    Some(Action::Backup(input))
}

/// Whether this client can ask `machine` about backups: it is connected, as its owner.
fn askable(machine: &Machine) -> bool {
    machine.role == Some(Role::Owner) && machine.connection == ConnectionState::Connected
}

impl App {
    /// The vaults paired here that this client owns, other than `host`.
    pub fn vaults(&self, host: &HostId) -> Vec<&Machine> {
        let Some(panel) = &self.machine_panel else {
            return Vec::new();
        };
        self.machines
            .iter()
            .filter(|m| m.host_id != *host && askable(m))
            .filter(|m| panel.links.get(&m.host_id) == Some(&Known::Vault))
            .collect()
    }

    /// The vault `linked` names, if it is paired here: the machine with its fingerprint.
    pub fn paired_vault(&self, linked: &LinkedVault) -> Option<&Machine> {
        self.machines
            .iter()
            .find(|m| m.fingerprint.eq_ignore_ascii_case(&linked.fingerprint))
    }

    /// Asks every connected machine this client owns where it backs up, unless it is being
    /// asked already.
    pub(crate) fn ask_links(&mut self) -> Vec<Effect> {
        let Some(panel) = &mut self.machine_panel else {
            return Vec::new();
        };
        let mut effects = Vec::new();
        for machine in self.machines.iter().filter(|m| askable(m)) {
            if panel.links.get(&machine.host_id) == Some(&Known::Asking) {
                continue;
            }
            panel.links.insert(machine.host_id.clone(), Known::Asking);
            effects.push(Effect::Send {
                host_id: machine.host_id.clone(),
                command: CommandBody::GetVaultLink,
                origin: Origin::Backup(Sent::Ask(machine.host_id.clone())),
            });
        }
        effects
    }

    /// Opens the backup dialog for the selected machine, or says why it cannot be backed up.
    pub(crate) fn open_backup(&mut self) -> Vec<Effect> {
        let Some(panel) = &self.machine_panel else {
            return Vec::new();
        };
        let Some(at) = panel.selected(&self.machines) else {
            return Vec::new();
        };
        let machine = &self.machines[at];
        let refusal = if machine.role.is_some_and(|role| role != Role::Owner) {
            Some("backing up: only the machine's owners can")
        } else if machine.connection != ConnectionState::Connected {
            Some("backing up: the machine is not connected")
        } else {
            None
        };
        if let Some(refusal) = refusal {
            self.notice = Some(refusal.to_owned());
            return Vec::new();
        }
        let host = machine.host_id.clone();
        if let Some(panel) = &mut self.machine_panel {
            panel.backup = Some(Backup {
                host,
                selected: 0,
                step: Step::Choose,
            });
        }
        self.ask_links()
    }

    /// Carries out one input to the backup dialog.
    pub(crate) fn backup_input(&mut self, input: Input) -> Vec<Effect> {
        let Some(backup) = self.machine_panel.as_ref().and_then(|p| p.backup.clone()) else {
            return Vec::new();
        };
        let known = self
            .machine_panel
            .as_ref()
            .and_then(|panel| panel.links.get(&backup.host).cloned());
        let vaults: Vec<HostId> = self
            .vaults(&backup.host)
            .iter()
            .map(|m| m.host_id.clone())
            .collect();
        let name = self
            .machines
            .iter()
            .find(|m| m.host_id == backup.host)
            .map(|m| m.name.clone())
            .unwrap_or_default();
        let mut effects = Vec::new();
        let step = match (&backup.step, input) {
            (Step::Linking { .. } | Step::Stopping, _) => return Vec::new(),
            (Step::ConfirmStop, Input::Close) | (Step::Failed(_), Input::Close) => Step::Choose,
            (_, Input::Close) | (Step::Done(_), Input::Submit) => {
                if let Some(panel) = &mut self.machine_panel {
                    panel.backup = None;
                }
                return Vec::new();
            }
            (Step::Choose, Input::Up) => {
                self.set_backup(|b| b.selected = b.selected.saturating_sub(1));
                return Vec::new();
            }
            (Step::Choose, Input::Down) => {
                let last = vaults.len().saturating_sub(1);
                self.set_backup(|b| b.selected = (b.selected + 1).min(last));
                return Vec::new();
            }
            (Step::Choose, Input::Submit) => match known {
                Some(Known::Linked(_)) => Step::ConfirmStop,
                Some(Known::Unlinked) => {
                    let Some(vault) =
                        vaults.get(backup.selected.min(vaults.len().saturating_sub(1)))
                    else {
                        return Vec::new();
                    };
                    effects.push(Effect::Send {
                        host_id: vault.clone(),
                        command: CommandBody::PairVaultHost {
                            host_name: name.trim().chars().take(MAX_NAME).collect(),
                        },
                        origin: Origin::Backup(Sent::Code {
                            host: backup.host.clone(),
                            vault: vault.clone(),
                        }),
                    });
                    Step::Linking {
                        vault: vault.clone(),
                    }
                }
                _ => return Vec::new(),
            },
            (Step::ConfirmStop, Input::Submit) => {
                let vault = match &known {
                    Some(Known::Linked(linked)) => {
                        self.paired_vault(linked).map(|m| m.host_id.clone())
                    }
                    _ => None,
                };
                effects.push(Effect::Send {
                    host_id: backup.host.clone(),
                    command: CommandBody::UnlinkVault,
                    origin: Origin::Backup(Sent::Unlink {
                        host: backup.host.clone(),
                        vault,
                    }),
                });
                Step::Stopping
            }
            (Step::Failed(_), Input::Submit) => Step::Choose,
            _ => return Vec::new(),
        };
        self.set_backup(|b| b.step = step);
        effects
    }

    fn set_backup(&mut self, change: impl FnOnce(&mut Backup)) {
        if let Some(backup) = self.machine_panel.as_mut().and_then(|p| p.backup.as_mut()) {
            change(backup);
        }
    }

    /// Moves the dialog on, if it is still open for `host`.
    fn backup_step(&mut self, host: &HostId, step: Step) {
        if let Some(backup) = self.machine_panel.as_mut().and_then(|p| p.backup.as_mut())
            && backup.host == *host
        {
            backup.step = step;
        }
    }

    fn name_of(&self, host: &HostId) -> String {
        self.machines
            .iter()
            .find(|m| m.host_id == *host)
            .map_or_else(|| host.to_string(), |m| m.name.clone())
    }

    /// A machine's answer to a command the backup dialog sent.
    pub(crate) fn backup_sent(
        &mut self,
        sent: Sent,
        result: Result<CommandResult, String>,
    ) -> Vec<Effect> {
        match (sent, result) {
            (Sent::Ask(host), result) => {
                if let (
                    Ok(CommandResult::VaultLink {
                        volume: Some(volume),
                        ..
                    }),
                    Some(panel),
                ) = (&result, &mut self.machine_panel)
                {
                    panel.volumes.insert(host.clone(), volume.clone());
                }
                let known = match result {
                    Ok(CommandResult::VaultLink { is_vault: true, .. }) => Known::Vault,
                    Ok(CommandResult::VaultLink {
                        vault: Some(vault), ..
                    }) => Known::Linked(vault),
                    Ok(CommandResult::VaultLink { vault: None, .. }) => Known::Unlinked,
                    Ok(other) => Known::Unknown(format!("unexpected answer {other:?}")),
                    Err(error) => Known::Unknown(error),
                };
                if let Some(panel) = &mut self.machine_panel {
                    panel.links.insert(host, known);
                }
            }
            (Sent::Code { host, vault }, Ok(CommandResult::HostPairing { code, .. })) => {
                let Some(machine) = self.machines.iter().find(|m| m.host_id == vault) else {
                    self.backup_step(&host, Step::Failed("the vault is gone".to_owned()));
                    return Vec::new();
                };
                return vec![Effect::Send {
                    host_id: host.clone(),
                    command: CommandBody::LinkVault {
                        addresses: machine.addresses.clone(),
                        fingerprint: machine.fingerprint.clone(),
                        pairing_code: code,
                    },
                    origin: Origin::Backup(Sent::Link { host, vault }),
                }];
            }
            (Sent::Code { host, vault }, other) => {
                let why = other.err().unwrap_or_else(|| "no code".to_owned());
                let vault = self.name_of(&vault);
                self.backup_step(&host, Step::Failed(format!("{vault} gave no code: {why}")));
            }
            (Sent::Link { host, vault }, Ok(_)) => {
                let (name, vault_name) = (self.name_of(&host), self.name_of(&vault));
                if let Some(machine) = self.machines.iter().find(|m| m.host_id == vault) {
                    let linked = LinkedVault {
                        address: machine.addresses.first().cloned().unwrap_or_default(),
                        fingerprint: machine.fingerprint.clone(),
                    };
                    if let Some(panel) = &mut self.machine_panel {
                        panel.links.insert(host.clone(), Known::Linked(linked));
                    }
                }
                let done = format!("{name} backs up to {vault_name} now.");
                self.backup_step(&host, Step::Done(done));
            }
            (Sent::Link { host, .. }, Err(why)) => {
                self.backup_step(&host, Step::Failed(why));
            }
            (Sent::Unlink { host, vault }, Ok(_)) => {
                if let Some(panel) = &mut self.machine_panel {
                    panel.links.insert(host.clone(), Known::Unlinked);
                }
                let Some(vault) = vault else {
                    let done = format!(
                        "{} stopped backing up. Its vault is not paired here: revoke it there \
                         with `herder pair --revoke`.",
                        self.name_of(&host)
                    );
                    self.backup_step(&host, Step::Done(done));
                    return Vec::new();
                };
                return vec![Effect::Send {
                    host_id: vault,
                    command: CommandBody::RevokeVaultHost {
                        host_id: host.clone(),
                    },
                    origin: Origin::Backup(Sent::Revoke { host }),
                }];
            }
            (Sent::Unlink { host, .. }, Err(why)) => {
                self.backup_step(&host, Step::Failed(why));
            }
            (Sent::Revoke { host }, result) => {
                let name = self.name_of(&host);
                let done = match result {
                    Ok(_) => format!(
                        "{name} stopped backing up, and its key no longer opens the vault. \
                         Its sessions stay there."
                    ),
                    Err(why) => format!("{name} stopped backing up; revoking it failed: {why}"),
                };
                self.backup_step(&host, Step::Done(done));
            }
        }
        Vec::new()
    }
}

/// Where `known` says a machine backs up, in words, with `vault` naming a paired vault.
pub fn describe(known: &Known, vault: Option<&Machine>) -> String {
    match known {
        Known::Asking => "checking…".to_owned(),
        Known::Vault => "this is a vault".to_owned(),
        Known::Unlinked => "none".to_owned(),
        Known::Linked(linked) => match vault {
            Some(vault) => format!("{} · {}", vault.name, linked.address),
            None => linked.address.clone(),
        },
        Known::Unknown(_) => "not known".to_owned(),
    }
}

/// Every link this client knows of, by machine.
pub type Links = HashMap<HostId, Known>;

#[cfg(test)]
mod tests {
    use herder_protocol::{ErrorCode, Timestamp};
    use ratatui::crossterm::event::{KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::Msg;
    use crate::fake;

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn step(app: &App) -> Option<Step> {
        app.machine_panel
            .as_ref()
            .and_then(|panel| panel.backup.as_ref())
            .map(|backup| backup.step.clone())
    }

    /// The one command `effects` sends, with where it goes and what for.
    fn sent(effects: Vec<Effect>) -> (HostId, CommandBody, Origin) {
        let [
            Effect::Send {
                host_id,
                command,
                origin,
            },
        ] = <[Effect; 1]>::try_from(effects).unwrap()
        else {
            panic!("expected one command");
        };
        (host_id, command, origin)
    }

    fn known(app: &App, host: &str) -> Option<Known> {
        let panel = app.machine_panel.as_ref().unwrap();
        panel.links.get(&HostId::new(host)).cloned()
    }

    #[test]
    fn opening_the_panel_asks_every_owned_machine_where_it_backs_up() {
        let app = fake::backups();
        assert_eq!(known(&app, "devbox"), Some(Known::Unlinked));
        assert_eq!(known(&app, "v"), Some(Known::Vault));
        assert!(matches!(known(&app, "laptop"), Some(Known::Linked(_))));
        assert_eq!(app.vaults(&HostId::new("devbox")).len(), 1);
        assert!(app.vaults(&HostId::new("v")).is_empty());
    }

    #[test]
    fn backing_up_gets_a_host_code_from_the_vault_and_hands_it_to_the_machine() {
        let mut app = fake::backups();
        // The panel asks again with the dialog; answer it as before.
        for effect in press(&mut app, KeyCode::Char('b')) {
            let Effect::Send {
                origin, host_id, ..
            } = effect
            else {
                continue;
            };
            let result = Ok(CommandResult::VaultLink {
                is_vault: host_id.as_str() == "v",
                vault: None,
                volume: None,
            });
            app.update(Msg::Sent { origin, result });
        }
        assert_eq!(step(&app), Some(Step::Choose));
        let (to, command, origin) = sent(press(&mut app, KeyCode::Enter));
        assert_eq!(to, HostId::new("v"));
        assert_eq!(
            command,
            CommandBody::PairVaultHost {
                host_name: "devbox".into()
            }
        );
        // Keys wait while it links.
        assert_eq!(press(&mut app, KeyCode::Esc), []);
        let code = Ok(CommandResult::HostPairing {
            code: "ABCDE-FGHJK".into(),
            expires_at: Timestamp::UNIX_EPOCH,
        });
        let (to, command, origin) = sent(app.update(Msg::Sent {
            origin,
            result: code,
        }));
        assert_eq!(to, HostId::new("devbox"));
        assert_eq!(
            command,
            CommandBody::LinkVault {
                addresses: vec!["vault.lan:7447".into()],
                fingerprint: "3f9a".repeat(16),
                pairing_code: "ABCDE-FGHJK".into(),
            }
        );
        app.update(Msg::Sent {
            origin,
            result: Ok(CommandResult::Applied),
        });
        assert_eq!(
            step(&app),
            Some(Step::Done("devbox backs up to vault now.".into()))
        );
        assert!(matches!(known(&app, "devbox"), Some(Known::Linked(_))));
        press(&mut app, KeyCode::Enter);
        assert_eq!(step(&app), None);
        assert!(app.machine_panel.is_some());
    }

    #[test]
    fn stopping_unlinks_the_machine_then_revokes_it_on_the_vault() {
        let mut app = fake::backups();
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('b'));
        // Asked again: the answers stay as they were until they come.
        let panel = app.machine_panel.as_mut().unwrap();
        let linked = fake::backups().machine_panel.unwrap().links;
        panel.links = linked;
        assert_eq!(press(&mut app, KeyCode::Enter), []);
        assert_eq!(step(&app), Some(Step::ConfirmStop));
        // n keeps it; y stops.
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(step(&app), Some(Step::Choose));
        press(&mut app, KeyCode::Enter);
        let (to, command, origin) = sent(press(&mut app, KeyCode::Char('y')));
        assert_eq!(
            (to, command),
            (HostId::new("laptop"), CommandBody::UnlinkVault)
        );
        let (to, command, origin) = sent(app.update(Msg::Sent {
            origin,
            result: Ok(CommandResult::Applied),
        }));
        assert_eq!(
            (to, command),
            (
                HostId::new("v"),
                CommandBody::RevokeVaultHost {
                    host_id: HostId::new("laptop")
                }
            )
        );
        assert_eq!(known(&app, "laptop"), Some(Known::Unlinked));
        app.update(Msg::Sent {
            origin,
            result: Ok(CommandResult::Applied),
        });
        assert!(matches!(step(&app), Some(Step::Done(_))));
    }

    #[test]
    fn a_failed_link_says_why_and_goes_back() {
        let mut app = fake::backups();
        let effects = press(&mut app, KeyCode::Char('b'));
        assert!(!effects.is_empty());
        let panel = app.machine_panel.as_mut().unwrap();
        panel.links = fake::backups().machine_panel.unwrap().links;
        let (_, _, origin) = sent(press(&mut app, KeyCode::Enter));
        app.update(Msg::Sent {
            origin,
            result: Err(format!("{:?}: no", ErrorCode::Forbidden)),
        });
        assert!(matches!(step(&app), Some(Step::Failed(why)) if why.contains("no")));
        press(&mut app, KeyCode::Esc);
        assert_eq!(step(&app), Some(Step::Choose));
    }

    #[test]
    fn members_and_offline_machines_are_not_backed_up() {
        let mut app = fake::backups();
        let mut machines = app.machines.clone();
        machines[0].role = Some(Role::Member);
        app.update(Msg::Machines(machines));
        assert_eq!(press(&mut app, KeyCode::Char('b')), []);
        assert_eq!(step(&app), None);
        assert_eq!(
            app.notice.as_deref(),
            Some("backing up: only the machine's owners can")
        );
    }
}
