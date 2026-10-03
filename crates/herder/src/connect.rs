//! `herder connect`: pair this device with a daemon from the `herder://pair` link that
//! `herder pair` printed there, as the TUI's add-machine dialog does.

use anyhow::{Context, Result};
use herder_client_core::Client;

pub fn run(link: &str) -> Result<()> {
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
        client.pair(link.trim().to_owned()).await
    })?;
    println!("paired with {} ({})\n", machine.name, machine.host_id);
    println!("  addresses    {}", machine.addresses.join(", "));
    println!("  fingerprint  {}", machine.fingerprint);
    println!("\nRun `herder` to open it.");
    Ok(())
}
