//! `herder pair`: mint a one-time pairing code on the running daemon, list or revoke devices.

use std::path::PathBuf;

use anyhow::{Result, bail};
use herder_client_core::PairingUri;
use herder_daemon::auth::DeviceRole;
use herder_daemon::auth::control::{self, DeviceInfo, PairingInfo, Request, Response};
use herder_protocol::{DeviceId, Role, Timestamp};
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
    /// List the paired devices.
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
        Request::Devices
    } else if let Some(device) = args.revoke {
        Request::Revoke {
            device_id: DeviceId::new(device),
        }
    } else if let Some(host) = args.host {
        Request::PairHost { host }
    } else {
        Request::Pair {
            user: args
                .user
                .or_else(|| std::env::var("USER").ok())
                .unwrap_or_else(|| "owner".to_owned()),
            role: args.role.map(|role| match role {
                RoleArg::Owner => Role::Owner,
                RoleArg::Member => Role::Member,
            }),
        }
    };
    match control::request(&config.data_dir, &request)? {
        Response::Paired(info) => print_pairing(&info),
        Response::Devices { devices } => print_devices(&devices),
        Response::Revoked => {
            if let Request::Revoke { device_id } = request {
                println!("revoked {device_id}");
            }
            Ok(())
        }
        Response::Error { message } => bail!("{message}"),
        Response::Recovered(_) | Response::ForgotHost(_) => {
            bail!("the daemon sent an unexpected answer")
        }
    }
}

fn print_pairing(info: &PairingInfo) -> Result<()> {
    if info.device_role == DeviceRole::Host {
        print_host_pairing(info);
        return Ok(());
    }
    let uri = PairingUri {
        hosts: info.addresses.clone(),
        fingerprint: info.fingerprint.clone(),
        code: info.code.clone(),
    }
    .to_string();
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

fn print_devices(devices: &[DeviceInfo]) -> Result<()> {
    if devices.is_empty() {
        println!("no paired devices; run `herder pair` to pair one");
        return Ok(());
    }
    let user_width = devices
        .iter()
        .map(|d| d.user.len())
        .max()
        .unwrap_or(0)
        .max(4);
    let client_width = devices
        .iter()
        .map(|d| d.client.len())
        .max()
        .unwrap_or(0)
        .max(6);
    println!(
        "{:<26}  {:<user_width$}  {:<6}  {:<6}  {:<client_width$}  PAIRED",
        "DEVICE", "USER", "ROLE", "ACCESS", "CLIENT"
    );
    for device in devices {
        println!(
            "{:<26}  {:<user_width$}  {:<6}  {:<6}  {:<client_width$}  {}",
            device.device_id,
            device.user,
            role_name(device.role),
            access(device.device_role),
            device.client,
            device.paired_at
        );
    }
    Ok(())
}

/// What a device may do: `client` reads everything, `host` only replicates to a vault.
fn access(role: DeviceRole) -> &'static str {
    match role {
        DeviceRole::Client => "client",
        DeviceRole::Host => "host",
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Owner => "owner",
        Role::Member => "member",
    }
}
