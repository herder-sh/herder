//! The remote access screen, over the fleet: each way this device reaches the fleet from
//! outside home, with what is set up and the keys that set it up, then each machine's route
//! in use.
//!
//! ```text
//!  remote access · home-hub                                     r ask again
//!
//!  Your VPN                                             i import · d remove
//!  ▶ home-router  vpn.example.net:51820  up
//!
//!  herder's WireGuard on the hub                   w set up · enter use here
//!    on at home.example.net:51900 · used on this device
//!
//!  Cloudflare (account 0123…)              c set up · l relay · h tunnel
//!    relay   ✓ up  https://herder-relay.example.workers.dev
//!    tunnel  … setting up  hub.example.com
//!
//!  Tailscale
//!    box  100.101.1.2:7447
//!
//!  Routes in use                                    o in the fleet ranks them
//!    home-hub  home network · 3 ms
//!    box       system VPN · 38 ms
//! ```

use herder_client_core::{ConnectionState, RouteKind, route_kind};
use herder_protocol::{RemoteAccess as Offer, RouteState, RouteStatus};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::App;
use crate::mouse::Hits;
use crate::remote_access::RemoteAccess;
use crate::secrets::Load;
use crate::ui::{Ui, fit, spread};

pub(super) fn draw(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    screen: &RemoteAccess,
    _hits: &mut Hits,
) {
    let ui = app.ui();
    let compact = app.width < super::NARROW;
    super::clear(frame, area, ui);
    let name = screen
        .hub
        .as_ref()
        .and_then(|hub| app.machines.iter().find(|m| m.host_id == *hub))
        .map_or_else(|| "this device".to_owned(), |m| m.name.clone());
    let right = if screen.sending {
        format!("applying{}", ui.glyphs.ellipsis)
    } else {
        "r ask again".to_owned()
    };
    let area = super::heading(frame, area, ui, "remote access", &name, &right, compact);
    let width = usize::from(area.width);
    let mut lines = Vec::new();
    if let Some(prompt) = &screen.prompt {
        let typed = if prompt.field.secret() {
            "•".repeat(prompt.text.chars().count())
        } else {
            prompt.text.clone()
        };
        lines.push(Line::styled(prompt.field.label(), ui.strong()));
        lines.push(Line::from(vec![
            Span::styled(format!("{typed}{}", ui.glyphs.cursor), ui.text()),
            Span::styled("   enter go on · esc cancel", ui.muted()),
        ]));
        lines.push(Line::default());
    }
    lines.extend(own_vpn(ui, screen, width));
    lines.push(Line::default());
    lines.extend(hub_options(ui, screen, width));
    lines.push(Line::default());
    lines.extend(tailscale(ui, app, width));
    lines.push(Line::default());
    lines.extend(in_use(ui, app, width));
    let buf = frame.buffer_mut();
    for (line, y) in lines.into_iter().zip(area.y..area.bottom()) {
        ratatui::widgets::Widget::render(
            fit(line, width, ui.glyphs),
            Rect::new(area.x, y, area.width, 1),
            buf,
        );
    }
}

/// A section's title, with its keys on the right.
fn title(ui: Ui, text: &str, keys: &str, width: usize) -> Line<'static> {
    spread(
        Line::styled(text.to_owned(), ui.strong()),
        Line::styled(keys.to_owned(), ui.muted()),
        width,
        ui.glyphs,
    )
}

/// An indented line of facts.
fn fact(style: Style, text: String) -> Line<'static> {
    Line::styled(format!("  {text}"), style)
}

/// The WireGuard configs of your own VPN imported on this device.
fn own_vpn(ui: Ui, screen: &RemoteAccess, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![title(ui, "Your VPN", "i import · d remove", width)];
    match &screen.tunnels {
        Load::Asking => lines.push(fact(ui.muted(), "…".to_owned())),
        Load::Failed(why) => lines.push(fact(ui.muted(), format!("cannot read them: {why}"))),
        Load::Loaded(_) => {
            let imported = screen.imported();
            if imported.is_empty() {
                lines.push(fact(
                    ui.muted(),
                    "none: import the WireGuard config of your router's or server's VPN, and \
                     machines behind it are reached through it"
                        .to_owned(),
                ));
            }
            for (at, tunnel) in imported.into_iter().enumerate() {
                let marker = if at == screen.at {
                    ui.glyphs.pointer
                } else {
                    " "
                };
                let mut spans = vec![
                    Span::styled(format!("{marker} "), ui.accent()),
                    Span::styled(tunnel.name.clone(), ui.text()),
                    Span::styled(format!("  {}", tunnel.endpoint), ui.muted()),
                ];
                if tunnel.up {
                    spans.push(Span::styled("  up", ui.accent()));
                }
                lines.push(Line::from(spans));
            }
        }
    }
    lines
}

/// What the hub offers and how it is set: herder's WireGuard, then Cloudflare.
fn hub_options(ui: Ui, screen: &RemoteAccess, width: usize) -> Vec<Line<'static>> {
    let owner = screen.owner;
    let wireguard_keys = if owner {
        "w set up · enter use here"
    } else {
        "enter use here"
    };
    let mut lines = vec![title(
        ui,
        "herder's WireGuard on the hub",
        wireguard_keys,
        width,
    )];
    let offer: &Offer = match &screen.offer {
        Load::Loaded(offer) => offer,
        Load::Asking => {
            lines.push(fact(ui.muted(), "asking the hub…".to_owned()));
            return lines;
        }
        Load::Failed(why) => {
            lines.push(fact(
                ui.muted(),
                format!("{why}: select a connected hub in the fleet to set up what it runs"),
            ));
            return lines;
        }
    };
    lines.push(match &offer.wireguard {
        Some(wireguard) => {
            let used = if wireguard.enrolled {
                "used on this device"
            } else {
                "not used on this device"
            };
            fact(ui.text(), format!("on at {} · {used}", wireguard.endpoint))
        }
        None if owner => fact(
            ui.muted(),
            "off: forward a UDP port on the router to the hub and press w".to_owned(),
        ),
        None => fact(ui.muted(), "off".to_owned()),
    });
    lines.push(Line::default());
    let cloudflare = offer
        .settings
        .as_ref()
        .and_then(|settings| settings.cloudflare.as_ref());
    let heading = match cloudflare {
        Some(cloudflare) => format!("Cloudflare (account {})", cloudflare.account_id),
        None => "Cloudflare".to_owned(),
    };
    let keys = match (owner, cloudflare) {
        (false, _) => "",
        (true, None) => "c set up",
        (true, Some(_)) => "c set up · l relay · h tunnel",
    };
    lines.push(title(ui, &heading, keys, width));
    if offer.relay.is_none() && offer.tunnel.is_none() {
        let text = if owner && cloudflare.is_none() {
            "off: with an account id and an API token, the hub runs a relay or a tunnel in your \
             Cloudflare account"
        } else {
            "off"
        };
        lines.push(fact(ui.muted(), text.to_owned()));
    }
    let hostname = cloudflare.and_then(|c| c.tunnel_hostname.as_deref());
    lines.extend(status(ui, "relay ", offer.relay.as_ref(), None));
    lines.extend(status(ui, "tunnel", offer.tunnel.as_ref(), hostname));
    lines
}

/// What the hub set up in Cloudflare: how far it got, where (`named` until its URL is known),
/// and why it failed.
fn status(
    ui: Ui,
    name: &str,
    status: Option<&RouteStatus>,
    named: Option<&str>,
) -> Vec<Line<'static>> {
    let Some(status) = status else {
        return Vec::new();
    };
    let (mark, state) = match status.state {
        RouteState::Up => (ui.glyphs.check_pass, "up"),
        RouteState::SettingUp => (ui.glyphs.check_pending, "setting up"),
        RouteState::Failed => (ui.glyphs.check_fail, "failed"),
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("  {name}  "), ui.muted()),
        Span::styled(format!("{mark} {state}"), ui.text()),
        Span::styled(
            format!(
                "  {}",
                if status.url.is_empty() {
                    named.unwrap_or_default()
                } else {
                    &status.url
                }
            ),
            ui.muted(),
        ),
    ])];
    if let Some(error) = &status.error {
        lines.push(fact(ui.muted(), format!("        {error}")));
    }
    lines
}

/// The machines this device reaches through a VPN the system runs, such as Tailscale.
fn tailscale(ui: Ui, app: &App, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![title(ui, "Tailscale", "", width)];
    let mut any = false;
    for machine in &app.machines {
        for address in &machine.addresses {
            if route_kind(address.clone()) == RouteKind::SystemVpn {
                any = true;
                lines.push(fact(ui.text(), format!("{}  {address}", machine.name)));
            }
        }
    }
    if !any {
        lines.push(fact(
            ui.muted(),
            "no machine has a Tailscale address: install Tailscale on it and on this device, \
             and its address joins the machine's routes"
                .to_owned(),
        ));
    }
    lines
}

/// Each machine's route in use and its round trip.
fn in_use(ui: Ui, app: &App, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![title(
        ui,
        "Routes in use",
        "o in the fleet ranks them",
        width,
    )];
    let name_width = app
        .machines
        .iter()
        .map(|m| m.name.chars().count())
        .max()
        .unwrap_or(0);
    for machine in &app.machines {
        let route = match (&machine.connection, &machine.address) {
            (ConnectionState::Connected, Some(address)) => {
                let mut route = super::machines::carrier(app, address);
                if let Some(rtt) = machine.quality.last_rtt_ms {
                    route.push_str(&format!(" · {rtt} ms"));
                }
                route
            }
            (ConnectionState::Connected, None) => "connected".to_owned(),
            _ => "not connected".to_owned(),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<name_width$}  ", machine.name), ui.text()),
            Span::styled(route, ui.muted()),
        ]));
    }
    lines
}
