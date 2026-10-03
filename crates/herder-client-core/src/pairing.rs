//! The `herder://pair` link a daemon prints as text and as a QR code.

use std::fmt;
use std::str::FromStr;

use url::Url;

use crate::Error;

/// What a device needs to pair, as the QR code carries it:
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

/// Formats the link, `herder://pair?…`.
impl fmt::Display for PairingUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut url = String::from("herder://pair?");
        let mut query = url::form_urlencoded::Serializer::for_suffix(&mut url, 14);
        for host in &self.hosts {
            query.append_pair("host", host);
        }
        query.append_pair("fp", &self.fingerprint);
        query.append_pair("code", &self.code);
        query.finish();
        f.write_str(&url)
    }
}

impl FromStr for PairingUri {
    type Err = Error;

    /// Parses a link; fails with [`Error::InvalidLink`].
    fn from_str(text: &str) -> Result<Self, Error> {
        let invalid = |message: &str| Error::InvalidLink {
            message: message.to_owned(),
        };
        let url = Url::parse(text).map_err(|err| invalid(&format!("not a pairing link: {err}")))?;
        if url.scheme() != "herder" || url.host_str() != Some("pair") {
            return Err(invalid("not a herder://pair link"));
        }
        let (mut hosts, mut fingerprint, mut code) = (Vec::new(), None, None);
        for (key, value) in url.query_pairs() {
            match &*key {
                "host" => hosts.push(value.into_owned()),
                "fp" => fingerprint = Some(value.into_owned()),
                "code" => code = Some(value.into_owned()),
                _ => {}
            }
        }
        let (Some(fingerprint), Some(code)) = (fingerprint, code) else {
            return Err(invalid(
                "the pairing link lacks the fingerprint or the code",
            ));
        };
        if hosts.is_empty() {
            return Err(invalid("the pairing link names no daemon address"));
        }
        Ok(Self {
            hosts,
            fingerprint,
            code,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
