//! `herder login`: sign this terminal in to a daemon as a person.
//!
//! A terminal with a passkey of its own (`herder_tui::passkey`) signs in with it, and makes
//! one when it claims a machine from the `herder://own` link `herder claim` shows, or joins
//! one from a `herder://invite` link: how someone with no herder app becomes a person. A
//! terminal without one shows a code as a `herder://login` link and QR, and waits until an
//! app of the person, or their terminal that has the passkey, approves it. Either way the
//! terminal then gets its own device credential under that person.

use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use herder_client_core::{Client, LoginUri, Machine, OwnUri};
use herder_daemon::auth::control::{self, Request, Response};
use herder_protocol::Timestamp;
use herder_tui::passkey::{self, InviteLink, LocalPasskey, Secret, Way};
use qrcode::QrCode;
use qrcode::render::unicode::Dense1x2;

#[derive(clap::Args)]
pub struct Args {
    /// The `herder://own` link `herder claim` shows, to become that machine's owner from this
    /// terminal; a `herder://invite` link, to join; or a remote daemon's address, `host:port`
    /// [default: the daemon running on this machine].
    #[arg(value_name = "LINK|ADDRESS")]
    target: Option<String>,
    /// SHA-256 of the remote daemon's certificate, as the link `herder claim` shows there names
    /// it; with an address.
    #[arg(long, value_name = "HEX", requires = "target")]
    fingerprint: Option<String>,
    /// Config file of the local daemon, to find its data dir [default:
    /// $XDG_CONFIG_HOME/herder/daemon.toml].
    #[arg(
        long,
        value_name = "PATH",
        env = "HERDER_CONFIG",
        conflicts_with = "target"
    )]
    config: Option<PathBuf>,
}

/// Where `herder login` signs in, and how.
enum Target {
    /// Claim the machine of a `herder://own` link.
    Own(OwnUri),
    /// Join the machines of a `herder://invite` link.
    Invite(InviteLink),
    /// Sign in to a machine that knows the person, or will once they approve.
    Machine {
        hosts: Vec<String>,
        fingerprint: String,
    },
}

impl Target {
    fn of(args: Args) -> Result<Self> {
        match (args.target, args.fingerprint) {
            (Some(link), None) if link.starts_with("herder://own") => Ok(Self::Own(link.parse()?)),
            (Some(link), None) if link.starts_with("herder://invite") => {
                Ok(Self::Invite(link.parse()?))
            }
            (Some(link), _) if link.starts_with("herder://") => {
                bail!("herder login takes a herder://own or herder://invite link, alone")
            }
            (Some(address), Some(fingerprint)) => Ok(Self::Machine {
                hosts: vec![address],
                fingerprint,
            }),
            (Some(_), None) => bail!("an address needs the daemon's --fingerprint"),
            (None, _) => {
                let (hosts, fingerprint) = local(args.config)?;
                Ok(Self::Machine { hosts, fingerprint })
            }
        }
    }
}

pub fn run(args: Args) -> Result<()> {
    let target = Target::of(args)?;
    let config_dir = herder_tui::config_dir()?;
    let profile = config_dir
        .to_str()
        .context("the config dir is not valid UTF-8")?
        .to_owned();
    let passkey = LocalPasskey::load(&config_dir)?;
    let runtime = tokio::runtime::Runtime::new().context("starting the tokio runtime")?;
    let client = {
        // The client's tasks start on the runtime.
        let _entered = runtime.enter();
        Client::open(profile, format!("herder-cli/{}", env!("CARGO_PKG_VERSION")))?
    };
    match target {
        Target::Own(own) => {
            let typed = Typed::ask(passkey.as_ref(), &config_dir)?;
            let way = Way::Claim(own.code);
            let joined = passkey::join(
                &client,
                own.hosts,
                own.fingerprint,
                way,
                typed.secret(passkey.as_ref(), &config_dir),
            );
            let machine = runtime.block_on(joined)?;
            println!("✓ You own {}, with this terminal's passkey.", machine.name);
        }
        Target::Invite(invite) => {
            let typed = Typed::ask(passkey.as_ref(), &config_dir)?;
            // A passkey made for the first machine signs in to the rest.
            let mut passkey = passkey;
            for machine in invite.machines {
                let name = machine.name.unwrap_or_else(|| machine.hosts.join(", "));
                let joined = passkey::join(
                    &client,
                    machine.hosts,
                    machine.fingerprint,
                    Way::Invite(machine.invite),
                    typed.secret(passkey.as_ref(), &config_dir),
                );
                match runtime.block_on(joined) {
                    Ok(machine) => println!("✓ Joined {}.", machine.name),
                    Err(err) => eprintln!("herder: joining {name}: {err:#}"),
                }
                if passkey.is_none() {
                    passkey = LocalPasskey::load(&config_dir)?;
                }
            }
        }
        Target::Machine { hosts, fingerprint } => {
            let machine = match &passkey {
                Some(passkey) => {
                    println!(
                        "Signing in as {} with this terminal's passkey.",
                        passkey.name()
                    );
                    let passphrase = rpassword::prompt_password("Passphrase: ")?;
                    runtime.block_on(passkey::sign_in(
                        &client,
                        hosts,
                        fingerprint,
                        passkey,
                        &passphrase,
                    ))?
                }
                None => runtime.block_on(with_code(&client, hosts, fingerprint))?,
            };
            println!(
                "✓ Signed in to {}. This terminal is listed under your devices.",
                machine.name
            );
        }
    }
    println!("Run `herder` to open it.");
    Ok(())
}

/// What the user typed to sign in with a passkey.
struct Typed {
    /// The name for a new passkey's person.
    name: String,
    passphrase: String,
}

impl Typed {
    /// Asks for the passphrase of the passkey this terminal has, or for the name and the
    /// passphrase of the one it is about to make.
    fn ask(passkey: Option<&LocalPasskey>, config_dir: &Path) -> Result<Self> {
        if let Some(passkey) = passkey {
            println!(
                "Signing in as {} with this terminal's passkey.",
                passkey.name()
            );
            return Ok(Self {
                name: passkey.name().to_owned(),
                passphrase: rpassword::prompt_password("Passphrase: ")?,
            });
        }
        println!(
            "This terminal has no passkey yet. herder makes one and keeps it in {}, sealed \
             under a passphrase.\nYou type the passphrase to approve what the apps approve with \
             Face ID. Nobody can recover it for you.\n",
            config_dir.join("passkey.json").display()
        );
        let suggested = std::env::var("USER").unwrap_or_default();
        print!("Your name [{suggested}]: ");
        std::io::stdout().flush()?;
        let mut name = String::new();
        std::io::stdin().lock().read_line(&mut name)?;
        let name = match name.trim() {
            "" => suggested,
            name => name.to_owned(),
        };
        if name.is_empty() {
            bail!("a person needs a name");
        }
        let passphrase = rpassword::prompt_password(format!(
            "New passphrase (at least {} characters): ",
            passkey::MIN_PASSPHRASE
        ))?;
        if passphrase.chars().count() < passkey::MIN_PASSPHRASE {
            bail!(
                "a passphrase has at least {} characters",
                passkey::MIN_PASSPHRASE
            );
        }
        if rpassword::prompt_password("Again: ")? != passphrase {
            bail!("the passphrases differ");
        }
        Ok(Self { name, passphrase })
    }

    fn secret<'a>(&'a self, passkey: Option<&'a LocalPasskey>, config_dir: &'a Path) -> Secret<'a> {
        match passkey {
            Some(passkey) => Secret::Existing(passkey, &self.passphrase),
            None => Secret::New {
                name: &self.name,
                passphrase: &self.passphrase,
                config_dir,
            },
        }
    }
}

/// Signs in without a passkey: shows the code, and waits until the person approves it.
async fn with_code(
    client: &Client,
    hosts: Vec<String>,
    fingerprint: String,
) -> Result<Machine, herder_client_core::Error> {
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
    println!(
        "No herder app? In the terminal that has your passkey, run `/approve {}` in herder.\n",
        uri.code
    );
    let left = expires_at.duration_since(Timestamp::now()).as_secs();
    let minutes = u64::try_from(left).unwrap_or(0).div_ceil(60);
    println!("… waiting for your approval (up to {minutes} minutes)");
}

/// A code as the mockup shows it, `K7Q · 4PD`.
fn spaced(code: &str) -> String {
    code.replace('-', " · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(target: &str, fingerprint: Option<&str>) -> Result<Target> {
        Target::of(Args {
            target: Some(target.to_owned()),
            fingerprint: fingerprint.map(str::to_owned),
            config: None,
        })
    }

    #[test]
    fn a_link_claims_or_joins_and_an_address_needs_its_fingerprint() {
        let fp = "ab".repeat(32);
        let own = format!("herder://own?host=10.0.4.17%3A7447&fp={fp}&code=K7Q-4PD");
        assert!(matches!(
            target(&own, None),
            Ok(Target::Own(own)) if own.code == "K7Q-4PD"
        ));
        let invite = format!("herder://invite?role=member&host=10.0.4.17%3A7447&fp={fp}&invite=t");
        assert!(matches!(
            target(&invite, None),
            Ok(Target::Invite(invite)) if invite.machines.len() == 1
        ));
        assert!(matches!(
            target("10.0.4.17:7447", Some(&fp)),
            Ok(Target::Machine { hosts, .. }) if hosts == ["10.0.4.17:7447"]
        ));
        // A link names its own fingerprint, an address has none, and only these links sign in.
        assert!(target(&own, Some(&fp)).is_err());
        assert!(target("10.0.4.17:7447", None).is_err());
        assert!(target("herder://fleet?person=a", None).is_err());
    }

    #[test]
    fn codes_are_shown_in_two_spaced_groups() {
        assert_eq!(spaced("K7Q-4PD"), "K7Q · 4PD");
    }
}
