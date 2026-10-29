//! The links a person's devices pass each other, as text and as a QR code.
//!
//! A link names one or more machines, each as a group of query parameters: one or more
//! `host`, then `fp`, then `code` for a link that carries one. A group is complete once it has
//! them all; the next parameter after a complete group starts the next group.
//!
//! - `herder://own?host=<addr>&fp=<sha256 hex>&code=<code>&name=<name>`: a fresh daemon, or
//!   one `herder claim` ran on, waiting for an owner to claim it (`claim_ownership`).
//! - `herder://login?host=<addr>&fp=<sha256 hex>&code=<code>`: a terminal's `herder login`,
//!   waiting for an app of the person to approve it (`approve_login`).
//! - `herder://fleet?host=<addr>&fp=<sha256 hex>&host=<addr>&fp=…`: the machines a device
//!   shares with another device of the same person, which signs in to each with its passkey.
//!   It carries no code, so it grants nothing by itself.

use std::fmt;
use std::str::FromStr;

use url::Url;

use crate::Error;

/// The `herder://fleet` link [`crate::Client::share`] makes: machines by address and
/// fingerprint, for another device of the person to sign in to with its passkey
/// ([`crate::Client::sign_in`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetLink {
    /// The machines, in the link's order; never empty.
    pub machines: Vec<FleetMachine>,
}

/// One machine of a [`FleetLink`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetMachine {
    /// Addresses the daemon may be reached at, as `host:port`; try them in order.
    pub hosts: Vec<String>,
    /// SHA-256 of the daemon's certificate, lowercase hex: the client pins it.
    pub fingerprint: String,
}

/// The `herder://login` link `herder login` shows: the machine a terminal waits on, and the
/// code an app of the person approves it with (`approve_login`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginUri {
    /// Addresses the daemon may be reached at, as `host:port`; try them in order.
    pub hosts: Vec<String>,
    /// SHA-256 of the daemon's certificate, lowercase hex.
    pub fingerprint: String,
    /// The login request's code, `XXX-XXX`.
    pub code: String,
}

/// The `herder://own` link a daemon waiting for an owner shows, after `herder service
/// install` or `herder claim`: the machine, and the code that claims it (`claim_ownership`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnUri {
    /// Addresses the daemon may be reached at, as `host:port`; try them in order.
    pub hosts: Vec<String>,
    /// SHA-256 of the daemon's certificate, lowercase hex.
    pub fingerprint: String,
    /// The claim code, as the daemon's terminal shows it; the daemon ignores its case and
    /// separators.
    pub code: String,
    /// The machine's name, for the app to show while it asks; `None` when the link has none.
    pub name: Option<String>,
}

/// One machine's parameters, as a link carries them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Group {
    hosts: Vec<String>,
    fingerprint: Option<String>,
    code: Option<String>,
}

/// Formats the link, `herder://fleet?…`.
impl fmt::Display for FleetLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let groups: Vec<Group> = self
            .machines
            .iter()
            .map(|machine| Group {
                hosts: machine.hosts.clone(),
                fingerprint: Some(machine.fingerprint.clone()),
                code: None,
            })
            .collect();
        format(FLEET, &groups, f)
    }
}

impl FromStr for FleetLink {
    type Err = Error;

    /// Parses a link of one or more machines; fails with [`Error::InvalidLink`], also for a
    /// link with a code, which a fleet link never has.
    fn from_str(text: &str) -> Result<Self, Error> {
        let machines = parse(FLEET, text, false)?
            .into_iter()
            .map(|group| FleetMachine {
                hosts: group.hosts,
                fingerprint: group.fingerprint.unwrap_or_default(),
            })
            .collect();
        Ok(Self { machines })
    }
}

/// Formats the link, `herder://login?…`.
impl fmt::Display for LoginUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let group = Group {
            hosts: self.hosts.clone(),
            fingerprint: Some(self.fingerprint.clone()),
            code: Some(self.code.clone()),
        };
        format(LOGIN, &[group], f)
    }
}

impl FromStr for LoginUri {
    type Err = Error;

    /// Parses a `herder://login` link; fails with [`Error::InvalidLink`].
    fn from_str(text: &str) -> Result<Self, Error> {
        let (hosts, fingerprint, code) = one(LOGIN, text)?;
        Ok(Self {
            hosts,
            fingerprint,
            code,
        })
    }
}

/// Formats the link, `herder://own?…`, with the name last.
impl fmt::Display for OwnUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let group = Group {
            hosts: self.hosts.clone(),
            fingerprint: Some(self.fingerprint.clone()),
            code: Some(self.code.clone()),
        };
        format(OWN, &[group], f)?;
        if let Some(name) = &self.name {
            let name = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("name", name)
                .finish();
            write!(f, "&{name}")?;
        }
        Ok(())
    }
}

impl FromStr for OwnUri {
    type Err = Error;

    /// Parses a `herder://own` link, skipping parameters it does not know; fails with
    /// [`Error::InvalidLink`].
    fn from_str(text: &str) -> Result<Self, Error> {
        let (hosts, fingerprint, code) = one(OWN, text)?;
        // `one` checked it parses.
        let name = Url::parse(text).ok().and_then(|url| {
            url.query_pairs()
                .find(|(key, _)| key == "name")
                .map(|(_, name)| name.into_owned())
        });
        Ok(Self {
            hosts,
            fingerprint,
            code,
            name,
        })
    }
}

/// The kinds of link, as their host part.
const FLEET: &str = "fleet";
const LOGIN: &str = "login";
const OWN: &str = "own";

/// The one machine of a `herder://<kind>` link with a code: its hosts, fingerprint and code.
fn one(kind: &str, text: &str) -> Result<(Vec<String>, String, String), Error> {
    let mut groups = parse(kind, text, true)?;
    if groups.len() != 1 {
        return Err(invalid(&format!(
            "the {kind} link names {} machines, not one",
            groups.len()
        )));
    }
    let group = groups.remove(0);
    Ok((
        group.hosts,
        group.fingerprint.unwrap_or_default(),
        group.code.unwrap_or_default(),
    ))
}

fn format(kind: &str, groups: &[Group], f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let mut url = format!("herder://{kind}?");
    let start = url.len();
    let mut query = url::form_urlencoded::Serializer::for_suffix(&mut url, start);
    for group in groups {
        for host in &group.hosts {
            query.append_pair("host", host);
        }
        if let Some(fingerprint) = &group.fingerprint {
            query.append_pair("fp", fingerprint);
        }
        if let Some(code) = &group.code {
            query.append_pair("code", code);
        }
    }
    query.finish();
    f.write_str(&url)
}

impl Group {
    fn complete(&self, coded: bool) -> bool {
        !self.hosts.is_empty() && self.fingerprint.is_some() && (!coded || self.code.is_some())
    }

    /// The group, once it has everything a link of its kind needs.
    fn finish(self, coded: bool) -> Result<Self, Error> {
        if self.fingerprint.is_none() || (coded && self.code.is_none()) {
            return Err(invalid(if coded {
                "the link lacks the fingerprint or the code"
            } else {
                "the link lacks a fingerprint"
            }));
        }
        if self.hosts.is_empty() {
            return Err(invalid("the link names no daemon address"));
        }
        Ok(self)
    }
}

/// The machines of a `herder://<kind>` link, each with a code when `coded`.
fn parse(kind: &str, text: &str, coded: bool) -> Result<Vec<Group>, Error> {
    let url = Url::parse(text).map_err(|err| invalid(&format!("not a {kind} link: {err}")))?;
    if url.scheme() != "herder" || url.host_str() != Some(kind) {
        return Err(invalid(&format!("not a herder://{kind} link")));
    }
    let mut machines = Vec::new();
    let mut group = Group::default();
    for (key, value) in url.query_pairs() {
        if !matches!(&*key, "host" | "fp" | "code") {
            continue;
        }
        if !coded && key == "code" {
            return Err(invalid(&format!("a {kind} link carries no code")));
        }
        if group.complete(coded) {
            machines.push(std::mem::take(&mut group).finish(coded)?);
        }
        let value = value.into_owned();
        let repeated = match &*key {
            "host" => {
                group.hosts.push(value);
                false
            }
            "fp" => group.fingerprint.replace(value).is_some(),
            _ => group.code.replace(value).is_some(),
        };
        if repeated {
            return Err(invalid(&format!("the link names a machine's {key} twice")));
        }
    }
    machines.push(group.finish(coded)?);
    Ok(machines)
}

fn invalid(message: &str) -> Error {
    Error::InvalidLink {
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(host: &str, fingerprint: &str) -> FleetMachine {
        FleetMachine {
            hosts: vec![host.into()],
            fingerprint: fingerprint.repeat(32),
        }
    }

    #[test]
    fn a_fleet_link_round_trips_and_carries_no_code() {
        let mut first = machine("192.168.1.5:7447", "ab");
        first.hosts.push("[fd00::1]:7447".into());
        let link = FleetLink {
            machines: vec![first, machine("10.0.0.9:7447", "cd")],
        };
        let text = link.to_string();
        assert_eq!(
            text,
            format!(
                "herder://fleet?host=192.168.1.5%3A7447&host=%5Bfd00%3A%3A1%5D%3A7447&fp={}\
                 &host=10.0.0.9%3A7447&fp={}",
                "ab".repeat(32),
                "cd".repeat(32)
            )
        );
        assert_eq!(text.parse::<FleetLink>().unwrap(), link);
        // A group's parameters may come in any order.
        let reordered: FleetLink = "herder://fleet?fp=a&host=c".parse().unwrap();
        assert_eq!(
            reordered.machines,
            [FleetMachine {
                hosts: vec!["c".into()],
                fingerprint: "a".into(),
            }]
        );
        let fp = "ab".repeat(32);
        for text in [
            format!("herder://fleet?host=a&fp={fp}&code=X"),
            format!("herder://fleet?host=a&fp={fp}&fp={fp}"),
            format!("herder://fleet?host=a&fp={fp}&host=b"),
            "herder://fleet?host=a".to_owned(),
            "herder://fleet".to_owned(),
            format!("herder://pair?host=a&fp={fp}&code=X"),
            format!("https://fleet?host=a&fp={fp}"),
        ] {
            assert!(
                matches!(text.parse::<FleetLink>(), Err(Error::InvalidLink { .. })),
                "{text}"
            );
        }
    }

    #[test]
    fn login_and_own_links_round_trip_and_are_not_fleet_links() {
        let login = LoginUri {
            hosts: vec!["10.0.4.17:7447".into(), "[fd00::1]:7447".into()],
            fingerprint: "ab".repeat(32),
            code: "K7Q-4PD".into(),
        };
        let text = login.to_string();
        assert!(
            text.starts_with("herder://login?host=10.0.4.17%3A7447&host="),
            "{text}"
        );
        assert_eq!(text.parse::<LoginUri>().unwrap(), login);
        assert!(text.parse::<FleetLink>().is_err());
        assert!(text.parse::<OwnUri>().is_err());

        let own = OwnUri {
            hosts: vec!["10.0.4.17:7447".into()],
            fingerprint: "cd".repeat(32),
            code: "K7Q-4PD".into(),
            name: Some("new server".into()),
        };
        let text = own.to_string();
        assert!(text.starts_with("herder://own?host="), "{text}");
        assert_eq!(text.parse::<OwnUri>().unwrap(), own);
        assert!(text.parse::<LoginUri>().is_err());
        assert!("herder://login?host=a&fp=b".parse::<LoginUri>().is_err());
        let two = "herder://login?host=a&fp=b&code=c&host=d&fp=e&code=f";
        let err = two.parse::<LoginUri>().unwrap_err();
        assert!(err.to_string().contains("2 machines"), "{err}");

        // As `herder service install` prints it, with parameters it does not know and a
        // code typed loosely, which the daemon normalizes.
        let printed: OwnUri = "herder://own?host=a%3A7447&host=b%3A7447&fp=ab&code=k7q4pd\
                               &name=draugelis&role=owner"
            .parse()
            .unwrap();
        assert_eq!(
            printed,
            OwnUri {
                hosts: vec!["a:7447".into(), "b:7447".into()],
                fingerprint: "ab".into(),
                code: "k7q4pd".into(),
                name: Some("draugelis".into()),
            }
        );
        let unnamed: OwnUri = "herder://own?host=a&fp=b&code=c".parse().unwrap();
        assert_eq!(unnamed.name, None);
        assert!(!unnamed.to_string().contains("name"));
    }
}
