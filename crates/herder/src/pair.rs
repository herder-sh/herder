//! `herder pair`: mint a one-time pairing code on the running daemon, list or revoke devices.

use std::path::PathBuf;

use anyhow::{Result, bail};
use herder_client_core::PairingUri;
use herder_daemon::auth::DeviceRole;
use herder_daemon::auth::control::{self, OwnInfo, PairingInfo, Request, Response};
use herder_protocol::{DeviceId, Role, Timestamp, UserInfo};
use qrcode::QrCode;
use qrcode::render::unicode::Dense1x2;

#[derive(clap::Args)]
pub struct Args {
    /// User the new device acts as [default: your login name].
    #[arg(long, value_name = "NAME", conflicts_with_all = ["list", "revoke", "host"])]
    user: Option<String>,
    /// Role of a new user [default: owner for a daemon's first user, member after].
    #[arg(long, value_enum, conflicts_with_all = ["list", "revoke"])]
    role: Option<RoleArg>,
    /// On a vault, pair the host named NAME to replicate here, and only that: it never reads
    /// other hosts, sessions or devices.
    #[arg(long, value_name = "NAME", conflicts_with_all = ["list", "revoke", "role"])]
    host: Option<String>,
    /// List the users and their paired devices.
    #[arg(long, conflicts_with = "revoke")]
    list: bool,
    /// Unpair a device and disconnect it.
    #[arg(long, value_name = "DEVICE")]
    revoke: Option<String>,
    /// Daemon config file, to find its data dir [default: $XDG_CONFIG_HOME/herder/daemon.toml].
    #[arg(long, value_name = "PATH", env = "HERDER_CONFIG")]
    config: Option<PathBuf>,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum RoleArg {
    /// Full control, including terminals.
    Owner,
    /// Drives sessions; never gets a terminal.
    Member,
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
        Request::Pair {
            user: args.user.unwrap_or_else(default_user),
            role: args.role.map(|role| match role {
                RoleArg::Owner => Role::Owner,
                RoleArg::Member => Role::Member,
            }),
        }
    };
    match control::request(&config.data_dir, &request)? {
        Response::Paired(info) => print_pairing(&info),
        Response::Users { users } => print_users(&users),
        Response::Revoked => {
            if let Request::Revoke { device_id } = request {
                println!("revoked {device_id}");
            }
            Ok(())
        }
        Response::Error { message } => bail!("{message}"),
        Response::Address { .. }
        | Response::Forked(_)
        | Response::ForgotHost(_)
        | Response::Own(_) => {
            bail!("the daemon sent an unexpected answer")
        }
    }
}

/// The user a device pairs as unless told otherwise: the login name.
pub fn default_user() -> String {
    std::env::var("USER").unwrap_or_else(|_| "owner".to_owned())
}

pub fn print_pairing(info: &PairingInfo) -> Result<()> {
    if info.device_role == DeviceRole::Host {
        print_host_pairing(info);
        return Ok(());
    }
    let uri = link(info, &herder_daemon::host_name());
    let qr = QrCode::new(uri.as_bytes())?
        .render::<Dense1x2>()
        .quiet_zone(true)
        .build();
    let left = info.expires_at.duration_since(Timestamp::now()).as_secs();
    let minutes = u64::try_from(left).unwrap_or(0).div_ceil(60);
    println!(
        "Pair a device as {} ({}): scan the code, or enter these in the app.\n",
        info.user,
        role_name(info.role)
    );
    for address in &info.addresses {
        println!("  address      {address}");
    }
    println!("  fingerprint  {}", info.fingerprint);
    println!("  code         {}", info.code);
    println!("\nThe code works once, for the next {minutes} minutes.\n");
    println!("{qr}");
    println!("{uri}");
    println!("\nIn a terminal, run `herder connect '<link>'` with it, or paste it into herder.");
    Ok(())
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
    let mut query = url::form_urlencoded::Serializer::new(String::from("herder://own?"));
    for address in &info.addresses {
        query.append_pair("host", address);
    }
    query
        .append_pair("fp", &info.fingerprint)
        .append_pair("code", &info.code)
        .append_pair("name", name);
    query.finish()
}

/// The `herder://pair` link of `info`, followed by the role it grants and the machine's
/// `name`, so the app says what pairing gets you before it pairs; the link's parser skips both.
fn link(info: &PairingInfo, name: &str) -> String {
    let uri = PairingUri {
        hosts: info.addresses.clone(),
        fingerprint: info.fingerprint.clone(),
        code: info.code.clone(),
    };
    let extra = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("role", role_name(info.role))
        .append_pair("name", name)
        .finish();
    format!("{uri}&{extra}")
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
        println!("no paired devices; run `herder pair` to pair one");
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
    fn the_link_names_the_role_and_the_machine() {
        let info = PairingInfo {
            code: "ABCDE-FGHJK".into(),
            user: "tomas".into(),
            role: Role::Owner,
            device_role: DeviceRole::Client,
            expires_at: Timestamp::now(),
            fingerprint: "ab".repeat(32),
            addresses: vec!["127.0.0.1:7448".into()],
        };
        let link = link(&info, "draugelis");
        assert!(link.ends_with("&role=owner&name=draugelis"), "{link}");
        let uri: PairingUri = link.parse().unwrap();
        assert_eq!(
            (uri.hosts, uri.code.as_str()),
            (info.addresses, "ABCDE-FGHJK")
        );
    }
}
