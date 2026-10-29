//! `herder login`: sign this terminal in to a daemon as a person. It holds no passkey, so it
//! shows a code as a `herder://login` link and QR, and waits until an app of the person
//! approves it; the terminal then gets its own device credential under that person.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use herder_client_core::{Client, LoginUri};
use herder_daemon::auth::control::{self, Request, Response};
use herder_protocol::Timestamp;
use qrcode::QrCode;
use qrcode::render::unicode::Dense1x2;

#[derive(clap::Args)]
pub struct Args {
    /// A remote daemon's address, `host:port` [default: the daemon running on this machine].
    #[arg(value_name = "ADDRESS", requires = "fingerprint")]
    address: Option<String>,
    /// SHA-256 of the remote daemon's certificate, as `herder pair` shows it there.
    #[arg(long, value_name = "HEX", requires = "address")]
    fingerprint: Option<String>,
    /// Config file of the local daemon, to find its data dir [default:
    /// $XDG_CONFIG_HOME/herder/daemon.toml].
    #[arg(
        long,
        value_name = "PATH",
        env = "HERDER_CONFIG",
        conflicts_with = "address"
    )]
    config: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<()> {
    let (hosts, fingerprint) = match (args.address, args.fingerprint) {
        (Some(address), Some(fingerprint)) => (vec![address], fingerprint),
        _ => local(args.config)?,
    };
    let config_dir = herder_tui::config_dir()?
        .into_os_string()
        .into_string()
        .map_err(|_| anyhow::anyhow!("the config dir is not valid UTF-8"))?;
    let runtime = tokio::runtime::Runtime::new().context("starting the tokio runtime")?;
    let machine = runtime.block_on(async {
        let client = Client::open(
            config_dir,
            format!("herder-cli/{}", env!("CARGO_PKG_VERSION")),
        )?;
        let shown_hosts = hosts.clone();
        let shown_fingerprint = fingerprint.clone();
        let shown = move |code: &str, expires_at: Timestamp| {
            let uri = LoginUri {
                hosts: shown_hosts,
                fingerprint: shown_fingerprint,
                code: code.to_owned(),
            };
            show(&uri, expires_at);
        };
        client.log_in(hosts, fingerprint, shown).await
    })?;
    println!(
        "✓ Signed in to {}. This terminal is listed under your devices.",
        machine.name
    );
    println!("Run `herder` to open it.");
    Ok(())
}

/// The addresses and certificate of the daemon running on this machine.
fn local(config: Option<PathBuf>) -> Result<(Vec<String>, String)> {
    let config = herder_daemon::Config::load(config.as_deref())?;
    match control::request(&config.data_dir, &Request::Address)? {
        Response::Address {
            fingerprint,
            addresses,
        } => Ok((addresses, fingerprint)),
        Response::Error { message } => bail!("{message}"),
        _ => bail!("the daemon sent an unexpected answer"),
    }
}

fn show(uri: &LoginUri, expires_at: Timestamp) {
    let link = uri.to_string();
    println!("Sign in to herder as a person.\n");
    match QrCode::new(link.as_bytes()) {
        Ok(qr) => println!("{}", qr.render::<Dense1x2>().quiet_zone(true).build()),
        Err(err) => eprintln!("cannot draw the QR code: {err}"),
    }
    println!("Scan with your phone, or open {link}\n");
    println!("Code on your phone should read {}\n", spaced(&uri.code));
    let left = expires_at.duration_since(Timestamp::now()).as_secs();
    let minutes = u64::try_from(left).unwrap_or(0).div_ceil(60);
    println!("… waiting for your phone (up to {minutes} minutes)");
}

/// A code as the mockup shows it, `K7Q · 4PD`.
fn spaced(code: &str) -> String {
    code.replace('-', " · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_shown_in_two_spaced_groups() {
        assert_eq!(spaced("K7Q-4PD"), "K7Q · 4PD");
    }
}
