//! `herder connect`: pair this device with the daemons a `herder://pair` link names, as
//! `herder pair` printed it there or another device shared it, as the TUI's add-machine
//! dialog does.

use anyhow::{Context, Result, bail};
use herder_client_core::{Client, PairResult};

pub fn run(link: &str) -> Result<()> {
    let config_dir = herder_tui::config_dir()?
        .into_os_string()
        .into_string()
        .map_err(|_| anyhow::anyhow!("the config dir is not valid UTF-8"))?;
    let runtime = tokio::runtime::Runtime::new().context("starting the tokio runtime")?;
    let results = runtime.block_on(async {
        let client = Client::open(
            config_dir,
            format!("herder-cli/{}", env!("CARGO_PKG_VERSION")),
        )?;
        client.pair(link.trim().to_owned()).await
    })?;
    let mut failed = 0;
    for result in &results {
        match result {
            PairResult::Paired { machine } => {
                println!("paired with {} ({})\n", machine.name, machine.host_id);
                println!("  addresses    {}", machine.addresses.join(", "));
                println!("  fingerprint  {}\n", machine.fingerprint);
            }
            PairResult::Failed { addresses, error } => {
                failed += 1;
                eprintln!("pairing failed for {}: {error}\n", addresses.join(", "));
            }
        }
    }
    if failed == results.len() {
        bail!("paired with no machine");
    }
    println!("Run `herder` to open it.");
    if failed > 0 {
        bail!("{failed} of {} machines did not pair", results.len());
    }
    Ok(())
}
