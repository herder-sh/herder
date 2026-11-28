//! The remote access screen, from the fleet: every way this device reaches the fleet from
//! outside home, and setting up those herder runs itself (`docs/remote-access.md`). Its
//! state, keys and reducer.
//!
//! `w` on a machine in the fleet opens it. Your own VPN's WireGuard configs are this
//! device's, whatever machine is selected: `i` imports one from a file, `d` removes the one
//! selected. On a hub, `get_remote_access` says what it offers: `enter` uses its WireGuard on
//! this device or stops, and its owners set it up: `w` sets herder's WireGuard endpoint, `c`
//! the Cloudflare account and API token, `l` turns the relay on or off and `h` sets the
//! tunnel's hostname. `r` asks again. Routes are ranked, pinned and disabled per machine
//! with `o` in the fleet.

use herder_client_core::{ConnectionState, WireGuardKind, WireGuardTunnel};
use herder_protocol::{
    CloudflareSettings, CommandBody, CommandResult, DEFAULT_WIREGUARD_PORT, HostId,
    RemoteAccess as Offer, RemoteAccessSettings, Role, SecretRef, SecretValue, WireGuardSettings,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::Action;
use crate::app::{App, Effect};
use crate::backup::Known;
use crate::compose::Origin;
use crate::secrets::Load;

/// The hub secret set the Cloudflare API token is kept in, as the apps keep it.
pub const TOKEN_SET: &str = "cloudflare";

/// The token's name in [`TOKEN_SET`].
pub const TOKEN_NAME: &str = "CLOUDFLARE_API_TOKEN";

/// The remote access screen, over the fleet.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteAccess {
    /// The hub whose options are shown; `None` when the selected machine is not one.
    pub hub: Option<HostId>,
    /// Whether this client owns the hub, and so sets it up.
    pub owner: bool,
    /// What the hub offers and, to an owner, how it is set.
    pub offer: Load<Offer>,
    /// The WireGuard configs on this device.
    pub tunnels: Load<Vec<WireGuardTunnel>>,
    /// The imported config selected, by its index among the imported ones.
    pub at: usize,
    /// A value being typed.
    pub prompt: Option<Prompt>,
    /// A command on its way to the hub, so the screen says so.
    pub sending: bool,
}

/// A value the screen asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub field: Field,
    /// What was typed.
    pub text: String,
}

/// What a [`Prompt`] asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Field {
    /// The path of a WireGuard config to import.
    Import,
    /// herder's WireGuard endpoint, `host:port`; empty turns it off.
    Endpoint,
    /// The Cloudflare account id.
    Account,
    /// The Cloudflare API token, for the account typed before; empty keeps the hub's.
    Token { account_id: String },
    /// The managed tunnel's hostname; empty removes the tunnel.
    Hostname,
}

impl Field {
    /// What the prompt says it asks for.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Import => "WireGuard config file",
            Self::Endpoint => "WireGuard endpoint (host:port, empty turns it off)",
            Self::Account => "Cloudflare account id",
            Self::Token { .. } => "Cloudflare API token (empty keeps the hub's)",
            Self::Hostname => "tunnel hostname (empty removes the tunnel)",
        }
    }

    /// Whether what is typed is shown as dots.
    pub fn secret(&self) -> bool {
        matches!(self, Self::Token { .. })
    }
}

/// A command of the screen to a hub.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sent {
    /// `get_remote_access`.
    Get(HostId),
    /// `set_remote_access`, answered with what the hub applied.
    Set(HostId),
    /// `create_secret_set` for the token, before it is put: then the token is, and these
    /// settings follow.
    TokenSet(HostId, Box<CloudflareSettings>, SecretValue),
    /// `put_secret` of the token: these settings follow.
    Token(HostId, Box<CloudflareSettings>),
}

/// Input to the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// Close the prompt, or the screen.
    Close,
    /// Select the previous imported config.
    Up,
    /// Select the next imported config.
    Down,
    /// Ask the hub again.
    Reload,
    /// Import a WireGuard config.
    Import,
    /// Remove the selected imported config.
    Remove,
    /// Use the hub's WireGuard on this device, or stop.
    UseHere,
    /// Set herder's WireGuard endpoint on the hub.
    WireGuard,
    /// Set the hub's Cloudflare account and token.
    Cloudflare,
    /// Turn the Durable Object relay on or off.
    Relay,
    /// Set the managed tunnel's hostname.
    Hostname,
    /// Type a character.
    Char(char),
    /// Delete the last character.
    Backspace,
    /// Clear the prompt.
    Clear,
    /// Go on with what was typed.
    Submit,
}

/// The action a key asks for on the screen.
pub fn for_key(key: KeyEvent, screen: &RemoteAccess) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let input = if screen.prompt.is_some() {
        match key.code {
            KeyCode::Esc => Input::Close,
            KeyCode::Enter => Input::Submit,
            KeyCode::Backspace => Input::Backspace,
            KeyCode::Char('u') if ctrl => Input::Clear,
            KeyCode::Char(c) if !ctrl => Input::Char(c),
            _ => return None,
        }
    } else {
        match key.code {
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('q') => Input::Close,
            KeyCode::Char('k') | KeyCode::Up => Input::Up,
            KeyCode::Char('j') | KeyCode::Down => Input::Down,
            KeyCode::Char('r') => Input::Reload,
            KeyCode::Char('i') => Input::Import,
            KeyCode::Char('d') => Input::Remove,
            KeyCode::Enter | KeyCode::Char(' ') => Input::UseHere,
            KeyCode::Char('w') => Input::WireGuard,
            KeyCode::Char('c') => Input::Cloudflare,
            KeyCode::Char('l') => Input::Relay,
            KeyCode::Char('h') => Input::Hostname,
            KeyCode::Char('?') => return Some(Action::ToggleHelp),
            _ => return None,
        }
    };
    Some(Action::RemoteAccess(input))
}

impl RemoteAccess {
    /// The imported configs: your own VPN's.
    pub fn imported(&self) -> Vec<&WireGuardTunnel> {
        match &self.tunnels {
            Load::Loaded(tunnels) => tunnels
                .iter()
                .filter(|t| t.kind == WireGuardKind::Imported)
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The hub's settings, as it sent them to an owner.
    fn settings(&self) -> RemoteAccessSettings {
        match &self.offer {
            Load::Loaded(offer) => offer.settings.clone().unwrap_or_default(),
            _ => RemoteAccessSettings::default(),
        }
    }
}

/// `get_remote_access` to `hub`.
fn ask(hub: &HostId) -> Effect {
    Effect::Send {
        host_id: hub.clone(),
        command: CommandBody::GetRemoteAccess,
        origin: Origin::RemoteAccess(Sent::Get(hub.clone())),
    }
}

/// `set_remote_access` of `settings` to `hub`.
fn set(hub: &HostId, settings: RemoteAccessSettings) -> Effect {
    Effect::Send {
        host_id: hub.clone(),
        command: CommandBody::SetRemoteAccess {
            settings: Box::new(settings),
        },
        origin: Origin::RemoteAccess(Sent::Set(hub.clone())),
    }
}

/// The settings with herder's WireGuard at `endpoint`, `host:port`, or off when it is empty.
/// It binds the port the endpoint names, as a router forwards it as it is; else the one it
/// bound before, else [`DEFAULT_WIREGUARD_PORT`].
pub fn with_endpoint(
    mut settings: RemoteAccessSettings,
    endpoint: &str,
) -> Result<RemoteAccessSettings, String> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        settings.wireguard = None;
        return Ok(settings);
    }
    let before = settings.wireguard.as_ref().map(|w| w.port);
    let (endpoint, port) = match endpoint.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => match port.parse::<u16>() {
            Ok(port) if port > 0 => (endpoint.to_owned(), port),
            _ => return Err(format!("{port} is not a UDP port")),
        },
        _ => {
            let port = before.unwrap_or(DEFAULT_WIREGUARD_PORT);
            (format!("{endpoint}:{port}"), port)
        }
    };
    settings.wireguard = Some(WireGuardSettings { port, endpoint });
    Ok(settings)
}

impl App {
    /// Opens the remote access screen for the fleet's selected machine: its hub options when
    /// it is a hub this client reaches.
    pub(crate) fn open_remote_access(&mut self) -> Vec<Effect> {
        let Some(panel) = &self.machine_panel else {
            return Vec::new();
        };
        let Some(at) = panel.selected(&self.machines) else {
            return Vec::new();
        };
        let machine = &self.machines[at];
        let hub = (panel.links.get(&machine.host_id) == Some(&Known::Hub) || machine.hub.is_some())
            .then(|| machine.host_id.clone());
        let connected = machine.connection == ConnectionState::Connected;
        let mut effects = vec![Effect::WireGuardTunnels];
        let offer = match &hub {
            Some(hub) if connected => {
                effects.push(ask(hub));
                Load::Asking
            }
            Some(_) => Load::Failed(format!("{} is not connected", machine.name)),
            None => Load::Failed(format!("{} is not a hub", machine.name)),
        };
        let screen = RemoteAccess {
            hub,
            owner: machine.role.is_none_or(|role| role == Role::Owner),
            offer,
            tunnels: Load::Asking,
            at: 0,
            prompt: None,
            sending: false,
        };
        if let Some(panel) = &mut self.machine_panel {
            panel.remote = Some(screen);
        }
        effects
    }

    /// Carries out one input to the remote access screen.
    pub(crate) fn remote_access_input(&mut self, input: Input) -> Vec<Effect> {
        let Some(panel) = &mut self.machine_panel else {
            return Vec::new();
        };
        let Some(screen) = &mut panel.remote else {
            return Vec::new();
        };
        if let Some(prompt) = &mut screen.prompt {
            match input {
                Input::Close => screen.prompt = None,
                Input::Char(c) => prompt.text.push(c),
                Input::Backspace => {
                    prompt.text.pop();
                }
                Input::Clear => prompt.text.clear(),
                Input::Submit => {
                    let prompt = screen.prompt.take().unwrap_or(Prompt {
                        field: Field::Import,
                        text: String::new(),
                    });
                    return submit(screen, prompt, &mut self.notice);
                }
                _ => {}
            }
            return Vec::new();
        }
        let imported = screen.imported().len();
        let ask_for = |field: Field, text: String| Some(Prompt { field, text });
        let owned_hub = |screen: &RemoteAccess, what: &str| -> Result<HostId, String> {
            match &screen.hub {
                None => Err(format!("{what}: select a hub in the fleet first")),
                Some(_) if !screen.owner => Err(format!("{what}: only the hub's owners set it up")),
                Some(_) if !matches!(screen.offer, Load::Loaded(_)) => {
                    Err(format!("{what}: the hub has not said what it offers yet"))
                }
                Some(hub) => Ok(hub.clone()),
            }
        };
        match input {
            Input::Close => panel.remote = None,
            Input::Up => screen.at = screen.at.saturating_sub(1),
            Input::Down => screen.at = (screen.at + 1).min(imported.saturating_sub(1)),
            Input::Reload => {
                let mut effects = vec![Effect::WireGuardTunnels];
                if let Some(hub) = &screen.hub {
                    screen.offer = Load::Asking;
                    effects.push(ask(hub));
                }
                return effects;
            }
            Input::Import => screen.prompt = ask_for(Field::Import, String::new()),
            Input::Remove => {
                if let Some(tunnel) = screen.imported().get(screen.at) {
                    return vec![Effect::RemoveWireGuard(tunnel.id.clone())];
                }
                self.notice = Some("remote access: no config of your VPN to remove".to_owned());
            }
            Input::UseHere => {
                let offered = match &screen.offer {
                    Load::Loaded(offer) => offer.wireguard.as_ref(),
                    _ => None,
                };
                match (&screen.hub, offered) {
                    (Some(hub), Some(wireguard)) => {
                        return vec![Effect::SetHubWireGuard {
                            host_id: hub.clone(),
                            enabled: !wireguard.enrolled,
                        }];
                    }
                    _ => {
                        self.notice = Some(
                            "remote access: the hub's WireGuard is off; its owners turn it on \
                             with w"
                                .to_owned(),
                        );
                    }
                }
            }
            Input::WireGuard => match owned_hub(screen, "WireGuard") {
                Ok(_) => {
                    let endpoint = screen
                        .settings()
                        .wireguard
                        .map(|w| w.endpoint)
                        .unwrap_or_default();
                    screen.prompt = ask_for(Field::Endpoint, endpoint);
                }
                Err(why) => self.notice = Some(why),
            },
            Input::Cloudflare => match owned_hub(screen, "Cloudflare") {
                Ok(_) => {
                    let account = screen
                        .settings()
                        .cloudflare
                        .map(|c| c.account_id)
                        .unwrap_or_default();
                    screen.prompt = ask_for(Field::Account, account);
                }
                Err(why) => self.notice = Some(why),
            },
            Input::Relay | Input::Hostname => {
                let what = if input == Input::Relay {
                    "relay"
                } else {
                    "tunnel"
                };
                match owned_hub(screen, what) {
                    Ok(hub) => {
                        let mut settings = screen.settings();
                        let Some(cloudflare) = &mut settings.cloudflare else {
                            self.notice = Some(format!(
                                "{what}: set up the hub's Cloudflare account with c first"
                            ));
                            return Vec::new();
                        };
                        if input == Input::Relay {
                            cloudflare.relay = !cloudflare.relay;
                            screen.sending = true;
                            return vec![set(&hub, settings)];
                        }
                        let hostname = cloudflare.tunnel_hostname.clone().unwrap_or_default();
                        screen.prompt = ask_for(Field::Hostname, hostname);
                    }
                    Err(why) => self.notice = Some(why),
                }
            }
            Input::Char(_) | Input::Backspace | Input::Clear | Input::Submit => {}
        }
        Vec::new()
    }

    /// Takes the hub's answer to a command of the screen.
    pub(crate) fn remote_access_sent(
        &mut self,
        sent: Sent,
        result: Result<CommandResult, String>,
    ) -> Vec<Effect> {
        let Some(screen) = self
            .machine_panel
            .as_mut()
            .and_then(|panel| panel.remote.as_mut())
        else {
            return Vec::new();
        };
        let offered = |result: Result<CommandResult, String>| match result {
            Ok(CommandResult::RemoteAccess { remote_access }) => Ok(*remote_access),
            Ok(other) => Err(format!("unexpected answer {other:?}")),
            Err(why) => Err(why),
        };
        match sent {
            Sent::Get(hub) if screen.hub.as_ref() == Some(&hub) => {
                screen.offer = match offered(result) {
                    Ok(offer) => Load::Loaded(offer),
                    Err(why) => Load::Failed(why),
                };
            }
            Sent::Set(hub) if screen.hub.as_ref() == Some(&hub) => {
                screen.sending = false;
                match offered(result) {
                    Ok(offer) => screen.offer = Load::Loaded(offer),
                    Err(why) => self.notice = Some(format!("remote access: {why}")),
                }
            }
            // The set may be there already; putting the token says if it is not.
            Sent::TokenSet(hub, settings, token) => {
                return vec![Effect::Send {
                    host_id: hub.clone(),
                    command: CommandBody::PutSecret {
                        set: TOKEN_SET.to_owned(),
                        name: TOKEN_NAME.to_owned(),
                        description: "Cloudflare API token the hub sets up its relay and tunnel \
                                      with"
                            .to_owned(),
                        value: token,
                        assertion: None,
                    },
                    origin: Origin::RemoteAccess(Sent::Token(hub, settings)),
                }];
            }
            Sent::Token(hub, settings) => match result {
                Ok(_) => {
                    let mut all = screen.settings();
                    all.cloudflare = Some(*settings);
                    return vec![set(&hub, all)];
                }
                Err(why) => {
                    screen.sending = false;
                    self.notice = Some(format!("keeping the Cloudflare token: {why}"));
                }
            },
            _ => {}
        }
        Vec::new()
    }

    /// Takes this device's WireGuard configs, as they are now.
    pub(crate) fn remote_access_tunnels(&mut self, tunnels: Result<Vec<WireGuardTunnel>, String>) {
        if let Some(screen) = self
            .machine_panel
            .as_mut()
            .and_then(|panel| panel.remote.as_mut())
        {
            screen.tunnels = match tunnels {
                Ok(tunnels) => Load::Loaded(tunnels),
                Err(why) => Load::Failed(why),
            };
            screen.at = screen.at.min(screen.imported().len().saturating_sub(1));
        }
    }
}

/// Carries out what a prompt asked for, with what was typed.
fn submit(screen: &mut RemoteAccess, prompt: Prompt, notice: &mut Option<String>) -> Vec<Effect> {
    let text = prompt.text.trim().to_owned();
    let Some(hub) = screen.hub.clone() else {
        if prompt.field == Field::Import && !text.is_empty() {
            return vec![Effect::ImportWireGuard { path: text }];
        }
        return Vec::new();
    };
    let settings = screen.settings();
    match prompt.field {
        Field::Import if text.is_empty() => Vec::new(),
        Field::Import => vec![Effect::ImportWireGuard { path: text }],
        Field::Endpoint => match with_endpoint(settings, &text) {
            Ok(settings) => {
                screen.sending = true;
                vec![set(&hub, settings)]
            }
            Err(why) => {
                *notice = Some(format!("WireGuard: {why}"));
                screen.prompt = Some(Prompt {
                    field: Field::Endpoint,
                    text,
                });
                Vec::new()
            }
        },
        Field::Account if text.is_empty() => {
            *notice = Some("Cloudflare: the account id is on the account's overview".to_owned());
            Vec::new()
        }
        Field::Account => {
            screen.prompt = Some(Prompt {
                field: Field::Token { account_id: text },
                text: String::new(),
            });
            Vec::new()
        }
        Field::Token { account_id } => {
            let before = settings.cloudflare.clone();
            let cloudflare = CloudflareSettings {
                account_id,
                api_token: SecretRef {
                    set: Some(TOKEN_SET.to_owned()),
                    name: TOKEN_NAME.to_owned(),
                },
                // A first set-up runs the relay, as the apps' does.
                relay: before.as_ref().is_none_or(|c| c.relay),
                tunnel_hostname: before.and_then(|c| c.tunnel_hostname),
            };
            if text.is_empty() {
                screen.sending = true;
                let mut all = settings;
                all.cloudflare = Some(cloudflare);
                return vec![set(&hub, all)];
            }
            let Some(token) = SecretValue::new(text) else {
                *notice = Some("Cloudflare: that token is too long".to_owned());
                return Vec::new();
            };
            screen.sending = true;
            vec![Effect::Send {
                host_id: hub.clone(),
                command: CommandBody::CreateSecretSet {
                    set: TOKEN_SET.to_owned(),
                    assertion: None,
                },
                origin: Origin::RemoteAccess(Sent::TokenSet(hub, Box::new(cloudflare), token)),
            }]
        }
        Field::Hostname => {
            let mut settings = settings;
            if let Some(cloudflare) = &mut settings.cloudflare {
                cloudflare.tunnel_hostname = (!text.is_empty()).then_some(text);
            }
            screen.sending = true;
            vec![set(&hub, settings)]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_endpoint_binds_the_port_it_names_else_the_one_before_else_the_default() {
        let set = |endpoint: &str, before: Option<u16>| {
            let settings = RemoteAccessSettings {
                wireguard: before.map(|port| WireGuardSettings {
                    port,
                    endpoint: format!("old.example.net:{port}"),
                }),
                cloudflare: None,
            };
            with_endpoint(settings, endpoint).map(|s| s.wireguard)
        };
        let wireguard = |endpoint: &str, port| {
            Ok(Some(WireGuardSettings {
                port,
                endpoint: endpoint.to_owned(),
            }))
        };
        assert_eq!(
            set("home.example.net:51820", None),
            wireguard("home.example.net:51820", 51820)
        );
        assert_eq!(
            set("home.example.net", None),
            wireguard("home.example.net:51900", 51900)
        );
        assert_eq!(
            set("home.example.net", Some(4500)),
            wireguard("home.example.net:4500", 4500)
        );
        assert_eq!(set("  ", Some(4500)), Ok(None));
        assert!(set("home.example.net:udp", None).is_err());
    }
}
