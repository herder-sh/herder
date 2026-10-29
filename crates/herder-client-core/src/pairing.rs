//! The `herder://pair` link a daemon prints as text and as a QR code, and the multi-machine
//! link a paired device shares.
//!
//! A link names one or more machines, each as a group of query parameters: one or more
//! `host`, then `fp` and `code`. A group is complete once it has all three; the next parameter
//! after a complete group starts the next group. So a link of one machine, as `herder pair`
//! prints it, is a link of one group:
//!
//! `herder://pair?host=<addr>&fp=<sha256 hex>&code=<code>&host=<addr>&host=<addr>&fp=…&code=…`
//!
//! Signing in as a person takes links of one machine in the same form: `herder://login`, which
//! `herder login` shows for an app to approve, and `herder://own`, which a fresh daemon shows
//! for its first owner to claim it.

use std::fmt;
use std::str::FromStr;

use url::Url;

use crate::Error;

/// What a device needs to pair with one machine, as the QR code carries it:
/// `herder://pair?host=<addr>&host=<addr>&fp=<sha256 hex>&code=<code>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingUri {
    /// Addresses the daemon may be reached at, as `host:port`; try them in order.
    pub hosts: Vec<String>,
    /// SHA-256 of the daemon's certificate, lowercase hex: the client pins it.
    pub fingerprint: String,
    /// One-time pairing code, sent as the `pairing_code` of the client's hello.
    pub code: String,
}

/// A `herder://pair` link naming one or more machines, each with its own code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingLink {
    /// The machines, in the link's order; never empty.
    pub machines: Vec<PairingUri>,
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

/// The `herder://own` link `herder service install` shows on a fresh daemon: the machine
/// waiting for its first owner, and the code that claims it (`claim_ownership`).
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

/// Formats the link, `herder://pair?…`.
impl fmt::Display for PairingUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        format(PAIR, std::slice::from_ref(self), f)
    }
}

impl FromStr for PairingUri {
    type Err = Error;

    /// Parses a link of one machine; fails with [`Error::InvalidLink`], also for a link of
    /// several.
    fn from_str(text: &str) -> Result<Self, Error> {
        one(PAIR, text)
    }
}

/// Formats the link, `herder://login?…`.
impl fmt::Display for LoginUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let uri = PairingUri {
            hosts: self.hosts.clone(),
            fingerprint: self.fingerprint.clone(),
            code: self.code.clone(),
        };
        format(LOGIN, std::slice::from_ref(&uri), f)
    }
}

impl FromStr for LoginUri {
    type Err = Error;

    /// Parses a `herder://login` link; fails with [`Error::InvalidLink`].
    fn from_str(text: &str) -> Result<Self, Error> {
        let PairingUri {
            hosts,
            fingerprint,
            code,
        } = one(LOGIN, text)?;
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
        let uri = PairingUri {
            hosts: self.hosts.clone(),
            fingerprint: self.fingerprint.clone(),
            code: self.code.clone(),
        };
        format(OWN, std::slice::from_ref(&uri), f)?;
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
        let PairingUri {
            hosts,
            fingerprint,
            code,
        } = one(OWN, text)?;
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
const PAIR: &str = "pair";
const LOGIN: &str = "login";
const OWN: &str = "own";

/// The one machine of a `herder://<kind>` link.
fn one(kind: &str, text: &str) -> Result<PairingUri, Error> {
    let mut machines = parse(kind, text)?;
    match machines.len() {
        1 => Ok(machines.remove(0)),
        count => Err(invalid(&format!(
            "the {kind} link names {count} machines, not one"
        ))),
    }
}

/// Formats the link, `herder://pair?…`.
impl fmt::Display for PairingLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        format(PAIR, &self.machines, f)
    }
}

impl FromStr for PairingLink {
    type Err = Error;

    /// Parses a link of one or more machines; fails with [`Error::InvalidLink`].
    fn from_str(text: &str) -> Result<Self, Error> {
        Ok(Self {
            machines: parse(PAIR, text)?,
        })
    }
}

fn format(kind: &str, machines: &[PairingUri], f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let mut url = format!("herder://{kind}?");
    let start = url.len();
    let mut query = url::form_urlencoded::Serializer::for_suffix(&mut url, start);
    for machine in machines {
        for host in &machine.hosts {
            query.append_pair("host", host);
        }
        query.append_pair("fp", &machine.fingerprint);
        query.append_pair("code", &machine.code);
    }
    query.finish();
    f.write_str(&url)
}

/// One machine's parameters, as far as read.
#[derive(Default)]
struct Group {
    hosts: Vec<String>,
    fingerprint: Option<String>,
    code: Option<String>,
}

impl Group {
    fn complete(&self) -> bool {
        !self.hosts.is_empty() && self.fingerprint.is_some() && self.code.is_some()
    }

    fn finish(self) -> Result<PairingUri, Error> {
        let (Some(fingerprint), Some(code)) = (self.fingerprint, self.code) else {
            return Err(invalid("the link lacks the fingerprint or the code"));
        };
        if self.hosts.is_empty() {
            return Err(invalid("the link names no daemon address"));
        }
        Ok(PairingUri {
            hosts: self.hosts,
            fingerprint,
            code,
        })
    }
}

fn parse(kind: &str, text: &str) -> Result<Vec<PairingUri>, Error> {
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
        if group.complete() {
            machines.push(std::mem::take(&mut group).finish()?);
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
    machines.push(group.finish()?);
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

    fn machine(host: &str, fingerprint: &str, code: &str) -> PairingUri {
        PairingUri {
            hosts: vec![host.into()],
            fingerprint: fingerprint.repeat(32),
            code: code.into(),
        }
    }

    #[test]
    fn a_pairing_uri_round_trips() {
        let uri = PairingUri {
            hosts: vec!["192.168.1.5:7447".into(), "[fd00::1]:7447".into()],
            fingerprint: "ab".repeat(32),
            code: "ABCDE-FGHJK".into(),
        };
        let text = uri.to_string();
        assert!(
            text.starts_with("herder://pair?host=192.168.1.5%3A7447&host="),
            "{text}"
        );
        assert_eq!(text.parse::<PairingUri>().unwrap(), uri);
        assert!(
            "https://pair?fp=a&code=b&host=c"
                .parse::<PairingUri>()
                .is_err()
        );
        assert!("herder://pair?host=c&fp=a".parse::<PairingUri>().is_err());
        // A group's parameters may come in any order.
        let reordered: PairingUri = "herder://pair?fp=a&code=b&host=c".parse().unwrap();
        assert_eq!(
            (reordered.hosts, reordered.code),
            (vec!["c".into()], "b".into())
        );
    }

    #[test]
    fn a_link_of_several_machines_round_trips() {
        let mut first = machine("192.168.1.5:7447", "ab", "ABCDE-FGHJK");
        first.hosts.push("[fd00::1]:7447".into());
        let link = PairingLink {
            machines: vec![first.clone(), machine("10.0.0.9:7447", "cd", "MNPQR-STVWX")],
        };
        let text = link.to_string();
        assert_eq!(
            text,
            format!(
                "herder://pair?host=192.168.1.5%3A7447&host=%5Bfd00%3A%3A1%5D%3A7447&fp={}\
                 &code=ABCDE-FGHJK&host=10.0.0.9%3A7447&fp={}&code=MNPQR-STVWX",
                "ab".repeat(32),
                "cd".repeat(32)
            )
        );
        assert_eq!(text.parse::<PairingLink>().unwrap(), link);
        // A single-machine parser refuses it rather than pair with the first alone.
        let err = text.parse::<PairingUri>().unwrap_err();
        assert!(err.to_string().contains("2 machines"), "{err}");

        // A single-machine link is a link of one machine.
        let single = PairingLink {
            machines: vec![first.clone()],
        };
        assert_eq!(single.to_string(), first.to_string());
        assert_eq!(first.to_string().parse::<PairingLink>().unwrap(), single);
    }

    #[test]
    fn every_machine_of_a_link_needs_its_address_fingerprint_and_code() {
        let fp = "ab".repeat(32);
        for text in [
            format!("herder://pair?host=a&fp={fp}&code=X&host=b&fp={fp}"),
            format!("herder://pair?host=a&fp={fp}&code=X&fp={fp}&code=Y"),
            format!("herder://pair?host=a&fp={fp}&fp={fp}&code=X"),
            "herder://pair".to_owned(),
        ] {
            assert!(
                matches!(text.parse::<PairingLink>(), Err(Error::InvalidLink { .. })),
                "{text}"
            );
        }
    }

    #[test]
    fn login_and_own_links_round_trip_and_are_not_pairing_links() {
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
        assert!(text.parse::<PairingUri>().is_err());
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
