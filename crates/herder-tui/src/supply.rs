//! Supplying a secret an agent asked for with `request_secret`: its state, keys and reducer.
//!
//! The approval panel offers `store` and `decline`. Decline answers the approval as denied.
//! Store opens this dialog, where the owner types or pastes the value into a masked field.
//! Enter then puts it on the host's vault as that vault's owner, as the apps do: the host's
//! `get_vault_link` names the vault, which must be paired here with this user as its owner;
//! `list_secrets` there tells whether the set exists (`create_secret_set` when not) and keeps a
//! replaced secret's description when the agent gave none; `put_secret` stores the value; and
//! `confirm_secret_stored` on the host lets the agent go on, knowing only that it was stored.
//!
//! The value goes from here to the vault alone, never to the host. It is held as [`Masked`],
//! whose `Debug` hides it, only until `put_secret` takes it; a failure after that asks for it
//! again.

use herder_client_core::ConnectionState;
use herder_protocol::{
    ApprovalId, CommandBody, CommandResult, HostId, Role, SecretInfo, SecretRequest, SecretValue,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::Action;
use crate::app::{App, Effect};
use crate::compose::Origin;
use crate::secrets::Masked;
use crate::session::SessionKey;

/// The supply dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Supply {
    /// The session that asked, as its machine lists it.
    pub session: SessionKey,
    /// Its open `request_secret` approval.
    pub approval_id: ApprovalId,
    /// What it asks for.
    pub request: SecretRequest,
    /// The value, until `put_secret` takes it.
    pub value: Masked,
    /// The step under way; `None` while the value is typed.
    pub step: Option<Sent>,
    /// Why the last try failed.
    pub error: Option<String>,
}

/// A command sent for the dialog, which is also the step it is at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sent {
    /// `get_vault_link` to the session's machine.
    Link,
    /// `list_secrets` to the vault.
    List(HostId),
    /// `create_secret_set` to the vault, for the set the request names.
    Create(HostId),
    /// `put_secret` to the vault.
    Put,
    /// `confirm_secret_stored` to the session's machine.
    Confirm,
}

/// Input to the dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// Close the dialog; the request stays open.
    Close,
    /// Store the value.
    Submit,
    /// A typed character of the value.
    Char(char),
    /// Drop the value's last character.
    Backspace,
    /// Drop the whole value.
    Clear,
}

/// The action a key asks for while the dialog is open.
pub fn for_key(key: KeyEvent) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let input = match key.code {
        KeyCode::Esc => Input::Close,
        KeyCode::Enter => Input::Submit,
        KeyCode::Backspace => Input::Backspace,
        KeyCode::Char('u') if ctrl => Input::Clear,
        KeyCode::Char(c) if !ctrl => Input::Char(c),
        _ => return None,
    };
    Some(Action::Supply(input))
}

impl Supply {
    /// Pasted text, as it is: a PEM key keeps its newlines.
    pub fn paste(&mut self, text: &str) {
        if self.step.is_none() {
            self.value.0.push_str(text);
        }
    }

    /// The description to store: the agent's, else the replaced secret's in `listed`.
    fn description(&self, listed: &[SecretInfo], set: &str) -> String {
        if !self.request.description.is_empty() {
            return self.request.description.clone();
        }
        listed
            .iter()
            .find(|secret| secret.set == set && secret.name == self.request.name)
            .map(|secret| secret.description.clone())
            .unwrap_or_default()
    }

    /// Sends `put_secret` to `vault`, taking the value out of the dialog; or says why not.
    fn put(&mut self, vault: HostId, description: String) -> Vec<Effect> {
        let Some(value) = SecretValue::new(std::mem::take(&mut self.value.0)) else {
            return self.failed("the value is longer than 64 KiB, or holds a NUL; enter it again");
        };
        self.step = Some(Sent::Put);
        vec![Effect::Send {
            host_id: vault,
            command: CommandBody::PutSecret {
                set: self.request.set.clone(),
                name: self.request.name.clone(),
                description,
                value,
                assertion: None,
            },
            origin: Origin::Supply(Sent::Put),
        }]
    }

    /// Back to typing, with `why`.
    fn failed(&mut self, why: impl Into<String>) -> Vec<Effect> {
        self.step = None;
        self.error = Some(why.into());
        Vec::new()
    }
}

impl App {
    /// Opens the dialog for `key`'s open `request_secret` approval `approval_id`.
    pub(crate) fn open_supply(
        &mut self,
        key: SessionKey,
        approval_id: ApprovalId,
        request: SecretRequest,
    ) {
        self.supply = Some(Supply {
            session: key,
            approval_id,
            request,
            value: Masked::default(),
            step: None,
            error: None,
        });
    }

    /// Carries out one input to the dialog.
    pub(crate) fn supply_input(&mut self, input: Input) -> Vec<Effect> {
        let Some(dialog) = &mut self.supply else {
            return Vec::new();
        };
        if dialog.step.is_some() {
            return Vec::new();
        }
        match input {
            Input::Close => self.supply = None,
            Input::Char(c) => dialog.value.0.push(c),
            Input::Backspace => {
                dialog.value.0.pop();
            }
            Input::Clear => dialog.value = Masked::default(),
            Input::Submit if dialog.value.is_empty() => {
                dialog.error = Some("enter or paste the value".to_owned());
            }
            Input::Submit => {
                dialog.step = Some(Sent::Link);
                dialog.error = None;
                return vec![Effect::Send {
                    host_id: dialog.session.host_id.clone(),
                    command: CommandBody::GetVaultLink,
                    origin: Origin::Supply(Sent::Link),
                }];
            }
        }
        Vec::new()
    }

    /// The answer to a command the dialog sent: its next step, or why it stopped.
    pub(crate) fn supply_sent(
        &mut self,
        sent: Sent,
        result: Result<CommandResult, String>,
    ) -> Vec<Effect> {
        let vault = match (&sent, &result) {
            (Sent::Link, Ok(CommandResult::VaultLink { vault, .. })) => {
                Some(self.secrets_vault(vault.as_ref()))
            }
            _ => None,
        };
        let Some(dialog) = &mut self.supply else {
            return Vec::new();
        };
        if dialog.step.as_ref() != Some(&sent) {
            return Vec::new();
        }
        let result = match result {
            Ok(result) => result,
            Err(why) => return dialog.failed(why),
        };
        match (sent, result) {
            (Sent::Link, _) => match vault {
                Some(Ok(vault)) => {
                    dialog.step = Some(Sent::List(vault.clone()));
                    vec![Effect::Send {
                        host_id: vault.clone(),
                        command: CommandBody::ListSecrets,
                        origin: Origin::Supply(Sent::List(vault)),
                    }]
                }
                Some(Err(why)) => dialog.failed(why),
                None => dialog.failed("the machine did not say where it keeps secrets"),
            },
            (Sent::List(vault), CommandResult::Secrets { sets, secrets }) => {
                let set = dialog.request.set.clone();
                if sets.iter().any(|s| s.name == set) {
                    let description = dialog.description(&secrets, &set);
                    return dialog.put(vault, description);
                }
                dialog.step = Some(Sent::Create(vault.clone()));
                vec![Effect::Send {
                    host_id: vault.clone(),
                    command: CommandBody::CreateSecretSet {
                        set,
                        assertion: None,
                    },
                    origin: Origin::Supply(Sent::Create(vault)),
                }]
            }
            (Sent::Create(vault), _) => {
                let description = dialog.request.description.clone();
                dialog.put(vault, description)
            }
            (Sent::Put, _) => {
                dialog.step = Some(Sent::Confirm);
                vec![Effect::Send {
                    host_id: dialog.session.host_id.clone(),
                    command: CommandBody::ConfirmSecretStored {
                        session_id: dialog.session.session_id.clone(),
                        approval_id: dialog.approval_id.clone(),
                        set: dialog.request.set.clone(),
                        assertion: None,
                    },
                    origin: Origin::Supply(Sent::Confirm),
                }]
            }
            (Sent::Confirm, _) => {
                self.notice = Some(format!(
                    "stored {}/{}",
                    dialog.request.set, dialog.request.name
                ));
                self.supply = None;
                Vec::new()
            }
            (Sent::List(_), other) => dialog.failed(format!("unexpected answer {other:?}")),
        }
    }

    /// The vault a machine fetches its secrets from, `linked`, as this client reaches it: paired
    /// here, connected, with this user its owner; or why it cannot store there.
    fn secrets_vault(
        &self,
        linked: Option<&herder_protocol::LinkedVault>,
    ) -> Result<HostId, String> {
        let Some(linked) = linked.filter(|linked| linked.secrets) else {
            return Err(
                "the machine is not linked to a vault with Secrets on; link it to one \
                        from the machines panel"
                    .to_owned(),
            );
        };
        let Some(vault) = self.paired_vault(linked) else {
            return Err(format!(
                "the machine's vault, {}, is not paired here; pair with it as its owner",
                linked.address
            ));
        };
        if vault.role != Some(Role::Owner) {
            return Err(format!(
                "only the owners of {} store secrets there; ask one of them",
                vault.name
            ));
        }
        if vault.connection != ConnectionState::Connected {
            return Err(format!("{} is not connected", vault.name));
        }
        Ok(vault.host_id.clone())
    }
}

#[cfg(test)]
mod tests {
    use herder_protocol::{LinkedVault, SecretSet, Timestamp};
    use ratatui::crossterm::event::KeyEvent;

    use super::*;
    use crate::app::Msg;
    use crate::fake;

    const VALUE: &str = "github_pat_SECRETMARKER";

    fn request(description: &str) -> SecretRequest {
        SecretRequest {
            set: "coingate".into(),
            name: "GITHUB_SIEM_PAT".into(),
            description: description.into(),
            replaces: false,
        }
    }

    /// [`fake::chat_secret_request`] with its session open, and `y` on its request, which
    /// opens the dialog.
    fn app() -> App {
        let mut app = fake::chat_secret_request();
        app.open = Some(fake::key("h1", "s2"));
        app.focus = crate::app::Focus::Composer;
        press(&mut app, KeyCode::Char('y'));
        assert!(app.supply.is_some(), "y opens the dialog");
        app
    }

    /// [`app`], for a request with `description`.
    fn asking(description: &str) -> App {
        let mut app = app();
        app.supply.as_mut().unwrap().request = request(description);
        app
    }

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    /// The one command `effects` sends, with where to and what for.
    fn sent(effects: Vec<Effect>) -> (HostId, CommandBody, Origin) {
        let [
            Effect::Send {
                host_id,
                command,
                origin,
            },
        ] = <[Effect; 1]>::try_from(effects).expect("one effect")
        else {
            panic!("expected a command");
        };
        (host_id, command, origin)
    }

    /// Answers the one command `effects` sends with `result`: what the app sends next.
    fn answer(
        app: &mut App,
        effects: Vec<Effect>,
        result: Result<CommandResult, String>,
    ) -> Vec<Effect> {
        let origin = sent(effects).2;
        app.update(Msg::Sent { origin, result })
    }

    fn linked() -> CommandResult {
        CommandResult::VaultLink {
            is_vault: false,
            vault: Some(LinkedVault {
                address: "vault:7447".into(),
                fingerprint: "CD".repeat(32),
                backup: true,
                secrets: true,
                cliproxyapi: None,
            }),
            volume: None,
            local_vault: false,
        }
    }

    fn listed(sets: &[&str], secrets: Vec<SecretInfo>) -> CommandResult {
        CommandResult::Secrets {
            sets: sets
                .iter()
                .map(|name| SecretSet {
                    name: (*name).into(),
                    hosts: Vec::new(),
                    host_use: Vec::new(),
                })
                .collect(),
            secrets,
        }
    }

    #[test]
    fn a_typed_value_goes_to_the_vault_then_the_host_hears_it_was_stored() {
        let mut app = asking("");
        for c in VALUE.chars() {
            press(&mut app, KeyCode::Char(c));
        }
        // What the app's state shows of it.
        assert!(!format!("{:?}", app.supply).contains("SECRETMARKER"));

        let link = press(&mut app, KeyCode::Enter);
        let (host, command, _) = sent(link.clone());
        assert_eq!((host.as_str(), command), ("h1", CommandBody::GetVaultLink));
        // The vault answers as paired here, its fingerprint in any case.
        let list = answer(&mut app, link, Ok(linked()));
        let (host, command, _) = sent(list.clone());
        assert_eq!((host.as_str(), command), ("v", CommandBody::ListSecrets));
        // The set has the secret already: its description stays.
        let held = SecretInfo {
            set: "coingate".into(),
            name: "GITHUB_SIEM_PAT".into(),
            description: "SIEM audit log".into(),
            updated_at: Timestamp::now(),
        };
        let put = answer(&mut app, list, Ok(listed(&["coingate"], vec![held])));
        let (host, command, _) = sent(put.clone());
        let CommandBody::PutSecret {
            set,
            name,
            description,
            value,
            ..
        } = command
        else {
            panic!("expected put_secret");
        };
        assert_eq!(host.as_str(), "v");
        assert_eq!(
            (set.as_str(), name.as_str(), description.as_str()),
            ("coingate", "GITHUB_SIEM_PAT", "SIEM audit log")
        );
        assert_eq!(value.expose(), VALUE);
        assert!(app.supply.as_ref().unwrap().value.is_empty());

        let confirm = answer(&mut app, put, Ok(CommandResult::Applied));
        let (host, command, _) = sent(confirm.clone());
        assert_eq!(host.as_str(), "h1");
        assert_eq!(
            command,
            CommandBody::ConfirmSecretStored {
                session_id: herder_protocol::SessionId::new("s2"),
                approval_id: ApprovalId::new("secret-1"),
                set: "coingate".into(),
                assertion: None,
            }
        );
        assert!(answer(&mut app, confirm, Ok(CommandResult::Applied)).is_empty());
        assert!(app.supply.is_none());
        assert_eq!(
            app.notice.as_deref(),
            Some("stored coingate/GITHUB_SIEM_PAT")
        );
    }

    #[test]
    fn a_pasted_key_keeps_its_lines_and_a_missing_set_is_created() {
        let mut app = asking("GitHub PAT");
        let pem = "-----BEGIN KEY-----\nMIGT\n-----END KEY-----\n";
        app.update(Msg::Paste(pem.into()));
        let link = press(&mut app, KeyCode::Enter);
        let list = answer(&mut app, link, Ok(linked()));
        let create = answer(&mut app, list, Ok(listed(&["personal"], Vec::new())));
        let (host, command, _) = sent(create.clone());
        assert_eq!(host.as_str(), "v");
        assert_eq!(
            command,
            CommandBody::CreateSecretSet {
                set: "coingate".into(),
                assertion: None,
            }
        );
        let put = answer(&mut app, create, Ok(CommandResult::Applied));
        let CommandBody::PutSecret {
            description, value, ..
        } = sent(put).1
        else {
            panic!("expected put_secret");
        };
        assert_eq!(description, "GitHub PAT");
        assert_eq!(value.expose(), pem);
    }

    #[test]
    fn it_stops_where_the_vault_cannot_take_it_and_asks_again() {
        let mut app = asking("");
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.supply.as_ref().unwrap().error.as_deref(),
            Some("enter or paste the value")
        );
        press(&mut app, KeyCode::Char('x'));
        let link = press(&mut app, KeyCode::Enter);
        let unlinked = CommandResult::VaultLink {
            is_vault: false,
            vault: None,
            volume: None,
            local_vault: false,
        };
        assert!(answer(&mut app, link, Ok(unlinked)).is_empty());
        let dialog = app.supply.as_ref().unwrap();
        assert!(dialog.step.is_none());
        assert!(dialog.error.as_deref().unwrap().contains("not linked"));

        // A refused put asks for the value again.
        let link = press(&mut app, KeyCode::Enter);
        let list = answer(&mut app, link, Ok(linked()));
        let put = answer(&mut app, list, Ok(listed(&["coingate"], Vec::new())));
        let refused = answer(&mut app, put, Err("forbidden".into()));
        assert!(refused.is_empty());
        let dialog = app.supply.as_ref().unwrap();
        assert_eq!(dialog.error.as_deref(), Some("forbidden"));
        assert!(dialog.value.is_empty());

        press(&mut app, KeyCode::Esc);
        assert!(app.supply.is_none());
    }
}
