//! `herder vpn`: the WireGuard configs herder's own connections run through, from the user's
//! own WireGuard server (`docs/remote-access.md`, Own VPN). The TUI and the apps on this
//! device share them, through the client profile.

use std::io::Read as _;
use std::path::PathBuf;

use anyhow::{Context, Result};
use herder_client_core::Client;

#[derive(clap::Subcommand)]
pub enum Command {
    /// Import a WireGuard config, as a router exports it, under a name: away from home,
    /// herder reaches your machines' home addresses through it. Nothing else on this device
    /// uses it.
    Import {
        /// What to call it, such as `home`.
        name: String,
        /// The `.conf` file; `-` reads it from standard input.
        file: PathBuf,
    },
    /// List the imported configs.
    List,
    /// Remove an imported config and its key.
    Remove {
        /// Its id, as `herder vpn list` shows it.
        id: String,
    },
}

pub fn run(command: Command) -> Result<()> {
    let profile = herder_tui::config_dir()?
        .to_str()
        .context("the config dir is not valid UTF-8")?
        .to_owned();
    let runtime = tokio::runtime::Runtime::new().context("starting the tokio runtime")?;
    let client = {
        // The client's tasks start on the runtime.
        let _entered = runtime.enter();
        Client::open(profile, format!("herder-cli/{}", env!("CARGO_PKG_VERSION")))?
    };
    match command {
        Command::Import { name, file } => {
            let config = if file.as_os_str() == "-" {
                let mut text = String::new();
                std::io::stdin()
                    .read_to_string(&mut text)
                    .context("reading the config")?;
                text
            } else {
                std::fs::read_to_string(&file)
                    .with_context(|| format!("reading {}", file.display()))?
            };
            let tunnel = client.import_wireguard(name, config)?;
            println!(
                "Imported {} ({}): herder reaches home addresses in {} through {}.",
                tunnel.name,
                tunnel.id,
                tunnel.allowed_ips.join(", "),
                tunnel.endpoint
            );
            if file.as_os_str() != "-" {
                println!(
                    "The key is kept in herder's profile; you can delete {}.",
                    file.display()
                );
            }
        }
        Command::List => {
            let tunnels = client.wireguard_tunnels()?;
            if tunnels.is_empty() {
                println!("No WireGuard config imported. Add one with `herder vpn import`.");
            }
            for tunnel in tunnels {
                let up = if tunnel.up { "up" } else { "down" };
                println!(
                    "{}\t{}\t{}\t{}\t{up}",
                    tunnel.id,
                    tunnel.name,
                    tunnel.endpoint,
                    tunnel.allowed_ips.join(",")
                );
            }
        }
        Command::Remove { id } => {
            client.remove_wireguard(id.clone())?;
            println!("Removed {id}.");
        }
    }
    drop(client);
    drop(runtime);
    Ok(())
}
