//! `herder update`: replace this binary with a release from GitHub
//! ([`herder_daemon::update::release`]).

use std::io::{IsTerminal, Write};

use anyhow::{Context, Result, bail};
use herder_daemon::update::release;
use semver::Version;

use herder_daemon::service::{self, Service};

pub struct Args {
    pub version: Option<String>,
    pub yes: bool,
    pub allow_downgrade: bool,
}

pub fn run(args: Args) -> Result<()> {
    let current = release::current()?;
    let target = release::this_target()?;
    let base = release::download_base();

    let wanted = match &args.version {
        Some(version) => release::parse_version(version)?,
        None => release::latest(&base)?,
    };
    if !should_install(&current, &wanted, args.allow_downgrade)? {
        println!("herder {current} is already installed");
        return Ok(());
    }

    let url = release::asset_url(&base, &wanted, target);
    println!("downloading {url}");
    let binary = release::download(&url, &wanted, target)?;

    let exe = std::env::current_exe().context("finding the herder binary")?;
    release::replace_exe(&exe, &binary)?;
    println!("updated {} from {current} to {wanted}", exe.display());

    let vault = crate::vault::installed_service();
    let services = to_restart(service::is_active(), vault);
    if services.is_empty() {
        return Ok(());
    }
    let names: Vec<&str> = services.iter().map(|service| service.name).collect();
    let question = match names.as_slice() {
        [name] => format!("the {name} is running the old version; restart it now?"),
        names => format!(
            "the {} are running the old version; restart them now?",
            names.join(" and the ")
        ),
    };
    if args.yes || confirm(&question)? {
        for service in &services {
            service::restart(service)?;
        }
    } else {
        for service in &services {
            println!(
                "restart the {} later with: {}",
                service.name, service.installer
            );
        }
    }
    Ok(())
}

/// The services running this binary that an update restarts onto the new one: the daemon's
/// while it runs, and an installed vault's, so the vault never runs another version than the
/// daemon next to it.
fn to_restart(daemon_active: bool, vault: Option<Service>) -> Vec<Service> {
    daemon_active
        .then(Service::daemon)
        .into_iter()
        .chain(vault)
        .collect()
}

/// Whether to install `wanted` over `current`: newer installs, equal is a no-op, older needs
/// `allow_downgrade`.
fn should_install(current: &Version, wanted: &Version, allow_downgrade: bool) -> Result<bool> {
    if wanted == current {
        return Ok(false);
    }
    if wanted < current && !allow_downgrade {
        bail!(
            "{wanted} is older than the installed {current}; \
             pass --allow-downgrade to install it anyway"
        );
    }
    Ok(true)
}

fn confirm(question: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        return Ok(false);
    }
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn an_update_restarts_an_installed_vault_with_the_daemon() {
        let vault = || Some(herder_daemon::vault::install::service(Path::new("/v")));
        let labels = |services: Vec<Service>| -> Vec<&str> {
            services.iter().map(|service| service.label).collect()
        };
        assert_eq!(
            labels(to_restart(true, vault())),
            ["sh.herder.daemon", "sh.herder.vault"]
        );
        assert_eq!(labels(to_restart(false, vault())), ["sh.herder.vault"]);
        assert_eq!(labels(to_restart(true, None)), ["sh.herder.daemon"]);
        assert!(to_restart(false, None).is_empty());
    }

    #[test]
    fn newer_installs_equal_skips_older_needs_flag() {
        assert!(should_install(&v("0.1.0"), &v("0.2.0"), false).unwrap());
        assert!(should_install(&v("0.9.0"), &v("0.10.0"), false).unwrap());
        assert!(!should_install(&v("0.2.0"), &v("0.2.0"), false).unwrap());
        assert!(should_install(&v("0.2.0-rc.1"), &v("0.2.0"), false).unwrap());

        let err = should_install(&v("0.2.0"), &v("0.1.0"), false).unwrap_err();
        assert!(err.to_string().contains("--allow-downgrade"), "{err}");
        assert!(should_install(&v("0.2.0"), &v("0.1.0"), true).unwrap());
    }
}
