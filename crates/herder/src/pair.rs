//! `herder pair`: pair a host with the vault running here, list or revoke devices; and `herder
//! claim`, which gives the running daemon a new owner.
//!
//! People and their devices join through the app (`herder claim`, invites) and `herder login`,
//! never through a code minted here.

use std::path::PathBuf;

use anyhow::{Result, bail};
use herder_client_core::OwnUri;
use herder_daemon::auth::control::{self, OwnInfo, PairingInfo, Request, Response};
use herder_protocol::{DeviceId, Role, Timestamp, UserInfo};
use qrcode::QrCode;
use qrcode::render::unicode::Dense1x2;

#[derive(clap::Args)]
#[group(required = true, multiple = false, args = ["host", "list", "revoke"])]
pub struct Args {
    /// On a vault, pair the host named NAME to replicate here, and only that: it never reads
    /// other hosts, sessions or devices.
    #[arg(long, value_name = "NAME")]
    host: Option<String>,
    /// List the users and their paired devices.
    #[arg(long)]
    list: bool,
    /// Unpair a device and disconnect it.
    #[arg(long, value_name = "DEVICE")]
    revoke: Option<String>,
    /// Daemon config file, to find its data dir [default: $XDG_CONFIG_HOME/herder/daemon.toml].
    #[arg(long, value_name = "PATH", env = "HERDER_CONFIG")]
    config: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<()> {
    let config = herder_daemon::Config::load(args.config.as_deref())?;
    let request = if args.list {
        Request::Users
    } else if let Some(device) = args.revoke {
        Request::Revoke {
            device_id: DeviceId::new(device),
        }
    } else if let Some(host) = args.host {
        Request::PairHost { host }
    } else {
        bail!("pass --host, --list or --revoke");
    };
    match control::request(&config.data_dir, &request)? {
        Response::Paired(info) => {
            print_host_pairing(&info);
            Ok(())
        }
        Response::Users { users } => print_users(&users),
        Response::Revoked => {
            if let Request::Revoke { device_id } = request {
                println!("revoked {device_id}");
            }
            Ok(())
        }
        Response::Error { message } => bail!("{message}"),
        Response::Address { .. } | Response::ForgotHost(_) | Response::Own(_) => {
            bail!("the daemon sent an unexpected answer")
        }
    }
}

/// `herder claim`: opens the window in which the running daemon takes a new owner, and shows
/// how to approve it from the app.
pub fn claim(config: Option<PathBuf>) -> Result<()> {
    let config = herder_daemon::Config::load(config.as_deref())?;
    match control::request(&config.data_dir, &Request::Claim)? {
        Response::Own(info) => print_own(&info),
        Response::Error { message } => bail!("{message}"),
        _ => bail!("the daemon sent an unexpected answer"),
    }
}

/// Shows how to become an owner of this daemon, which has none or ran `herder claim`: the
/// `herder://own` link as a QR code, and the code the app should show too.
pub fn print_own(info: &OwnInfo) -> Result<()> {
    let uri = own_link(info, &herder_daemon::host_name());
    let qr = QrCode::new(uri.as_bytes())?
        .render::<Dense1x2>()
        .quiet_zone(true)
        .build();
    let left = info.expires_at.duration_since(Timestamp::now()).as_secs();
    let minutes = u64::try_from(left).unwrap_or(0).div_ceil(60);
    println!(
        "Become this machine's owner: scan the code with the herder app, or open the link.
"
    );
    println!("{qr}");
    println!(
        "{uri}
"
    );
    println!(
        "  Code on your phone should read {}",
        info.code.replace('-', " · ")
    );
    println!(
        "
The first person to approve within {minutes} minutes owns this machine."
    );
    Ok(())
}

/// The `herder://own` link of `info`, with the machine's `name` for the app to show.
fn own_link(info: &OwnInfo, name: &str) -> String {
    OwnUri {
        hosts: info.addresses.clone(),
        fingerprint: info.fingerprint.clone(),
        code: info.code.clone(),
        name: Some(name.to_owned()),
    }
    .to_string()
}

fn print_host_pairing(info: &PairingInfo) {
    let left = info.expires_at.duration_since(Timestamp::now()).as_secs();
    let minutes = u64::try_from(left).unwrap_or(0).div_ceil(60);
    let address = info.addresses.first().map_or("<address>", String::as_str);
    println!(
        "Pair the host {} to replicate to this vault. In its daemon.toml:\n",
        info.user
    );
    println!("  [vault]");
    println!("  address = \"{address}\"");
    println!("  fingerprint = \"{}\"", info.fingerprint);
    println!("  pairing_code = \"{}\"", info.code);
    if info.addresses.len() > 1 {
        println!(
            "\nThe vault also listens on {}.",
            info.addresses[1..].join(", ")
        );
    }
    println!(
        "\nThen restart its daemon. The code works once, for the next {minutes} minutes. The \
         host may only replicate its own sessions: it reads nothing on the vault."
    );
}

fn print_users(users: &[UserInfo]) -> Result<()> {
    let rows: Vec<_> = users
        .iter()
        .flat_map(|user| user.devices.iter().map(move |device| (user, device)))
        .collect();
    if rows.is_empty() {
        println!("no paired devices");
        return Ok(());
    }
    let user_width = rows
        .iter()
        .map(|(user, _)| user.name.len())
        .max()
        .unwrap_or(0)
        .max(4);
    let client_width = rows
        .iter()
        .map(|(_, device)| device.client.len())
        .max()
        .unwrap_or(0)
        .max(6);
    println!(
        "{:<26}  {:<user_width$}  {:<6}  {:<6}  {:<client_width$}  {:<20}  LAST SEEN",
        "DEVICE", "USER", "ROLE", "ACCESS", "CLIENT", "PAIRED"
    );
    for (user, device) in rows {
        let last_seen = if device.online {
            "online".to_owned()
        } else {
            device
                .last_seen
                .map_or_else(|| "-".to_owned(), |at| at.to_string())
        };
        println!(
            "{:<26}  {:<user_width$}  {:<6}  {:<6}  {:<client_width$}  {:<20}  {last_seen}",
            device.device_id,
            user.name,
            role_name(user.role),
            access(device.host_only),
            device.client,
            device.paired_at.to_string(),
        );
    }
    Ok(())
}

/// What a device may do: `client` reads everything, `host` only replicates to a vault.
fn access(host_only: bool) -> &'static str {
    if host_only { "host" } else { "client" }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Owner => "owner",
        Role::Member => "member",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_own_link_names_the_machine_and_parses_back() {
        let info = OwnInfo {
            code: "K7Q-4PD".into(),
            expires_at: Timestamp::now(),
            fingerprint: "ab".repeat(32),
            addresses: vec!["10.0.4.17:7447".into(), "[fd00::1]:7447".into()],
        };
        let link = own_link(&info, "new server");
        assert!(
            link.starts_with("herder://own?host=10.0.4.17%3A7447&host="),
            "{link}"
        );
        assert!(link.ends_with("&code=K7Q-4PD&name=new+server"), "{link}");
        let uri: OwnUri = link.parse().unwrap();
        assert_eq!(
            (uri.hosts, uri.code, uri.name),
            (info.addresses, info.code, Some("new server".into()))
        );
    }
}
