//! The `herder://pair` link a daemon prints as text and as a QR code, and the multi-machine
//! link a paired device shares.
//!
//! A link names one or more machines, each as a group of query parameters: one or more
//! `host`, then `fp` and `code`. A group is complete once it has all three; the next parameter
//! after a complete group starts the next group. So a link of one machine, as `herder pair`
//! prints it, is a link of one group:
//!
//! `herder://pair?host=<addr>&fp=<sha256 hex>&code=<code>&host=<addr>&host=<addr>&fp=…&code=…`

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

/// Formats the link, `herder://pair?…`.
impl fmt::Display for PairingUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        format(std::slice::from_ref(self), f)
    }
}

impl FromStr for PairingUri {
    type Err = Error;

    /// Parses a link of one machine; fails with [`Error::InvalidLink`], also for a link of
    /// several.
    fn from_str(text: &str) -> Result<Self, Error> {
        let mut machines = parse(text)?;
        match machines.len() {
            1 => Ok(machines.remove(0)),
            count => Err(invalid(&format!(
                "the pairing link names {count} machines, not one"
            ))),
        }
    }
}

/// Formats the link, `herder://pair?…`.
impl fmt::Display for PairingLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        format(&self.machines, f)
    }
}

impl FromStr for PairingLink {
    type Err = Error;

    /// Parses a link of one or more machines; fails with [`Error::InvalidLink`].
    fn from_str(text: &str) -> Result<Self, Error> {
        Ok(Self {
            machines: parse(text)?,
        })
    }
}

fn format(machines: &[PairingUri], f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let mut url = String::from("herder://pair?");
    let mut query = url::form_urlencoded::Serializer::for_suffix(&mut url, 14);
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
            return Err(invalid(
                "the pairing link lacks the fingerprint or the code",
            ));
        };
        if self.hosts.is_empty() {
            return Err(invalid("the pairing link names no daemon address"));
        }
        Ok(PairingUri {
            hosts: self.hosts,
            fingerprint,
            code,
        })
    }
}

fn parse(text: &str) -> Result<Vec<PairingUri>, Error> {
    let url = Url::parse(text).map_err(|err| invalid(&format!("not a pairing link: {err}")))?;
    if url.scheme() != "herder" || url.host_str() != Some("pair") {
        return Err(invalid("not a herder://pair link"));
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
            return Err(invalid(&format!(
                "the pairing link names a machine's {key} twice"
            )));
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
}
