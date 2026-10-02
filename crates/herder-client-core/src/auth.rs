//! The client side of pairing: the device key, a TLS config that pins the daemon's
//! certificate, and the `herder://pair` link `herder pair` prints.

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, OtherError, SignatureScheme};
use sha2::{Digest, Sha256};
use url::Url;

/// WebSocket upgrade request header an unpaired device sends its pairing code in.
pub const PAIRING_CODE_HEADER: &str = "herder-pairing-code";

/// A client device's Ed25519 key and the self-signed certificate it presents to daemons.
///
/// Generate it once per device and keep it: a new key is a new, unpaired device.
pub struct DeviceKey {
    pem: String,
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
}

impl DeviceKey {
    /// A new key pair with its certificate.
    pub fn generate() -> Result<Self> {
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519)
            .context("generating the device key")?;
        let cert = rcgen::CertificateParams::new(vec!["herder-device".to_owned()])
            .and_then(|params| params.self_signed(&key))
            .context("creating the device certificate")?;
        Self::from_pem(&format!("{}{}", cert.pem(), key.serialize_pem()))
    }

    /// Reads a key saved with [`DeviceKey::to_pem`].
    pub fn from_pem(pem: &str) -> Result<Self> {
        let cert = CertificateDer::from_pem_slice(pem.as_bytes())
            .context("reading the device certificate")?;
        let key =
            PrivateKeyDer::from_pem_slice(pem.as_bytes()).context("reading the device key")?;
        Ok(Self {
            pem: pem.to_owned(),
            cert,
            key,
        })
    }

    /// The certificate and key as PEM, to save; the key is secret.
    pub fn to_pem(&self) -> &str {
        &self.pem
    }

    /// SHA-256 of the certificate, as daemons know the device.
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.cert)
    }
}

/// A TLS config that authenticates as `device` and accepts only the daemon certificate whose
/// SHA-256 is `daemon_fingerprint` (lowercase hex, as `herder pair` prints it).
pub fn client_config(daemon_fingerprint: &str, device: &DeviceKey) -> Result<ClientConfig> {
    let provider = rustls::crypto::ring::default_provider();
    let verifier = PinnedServer {
        fingerprint: daemon_fingerprint.to_ascii_lowercase(),
        algorithms: provider.signature_verification_algorithms,
    };
    ClientConfig::builder_with_provider(Arc::new(provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("configuring TLS")?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_client_auth_cert(vec![device.cert.clone()], device.key.clone_key())
        .context("loading the device key")
}

/// Accepts exactly the daemon certificate with the pinned fingerprint, whatever its name or
/// dates: daemons use self-signed certificates that no CA vouches for.
#[derive(Debug)]
struct PinnedServer {
    fingerprint: String,
    algorithms: WebPkiSupportedAlgorithms,
}

/// rustls reports a verifier's error with `Debug`, so that prints the message too.
#[derive(thiserror::Error)]
#[error(
    "the daemon's certificate fingerprint is {found}, not the paired {expected}; it may be an \
     impostor, or its certificate was replaced and the device must pair again"
)]
struct FingerprintMismatch {
    expected: String,
    found: String,
}

impl fmt::Debug for FingerprintMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl ServerCertVerifier for PinnedServer {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let found = fingerprint(end_entity);
        if found == self.fingerprint {
            return Ok(ServerCertVerified::assertion());
        }
        let mismatch = FingerprintMismatch {
            expected: self.fingerprint.clone(),
            found,
        };
        Err(rustls::Error::InvalidCertificate(CertificateError::Other(
            OtherError(Arc::new(mismatch)),
        )))
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// What a device needs to pair, as the QR code carries it:
/// `herder://pair?host=<addr>&host=<addr>&fp=<sha256 hex>&code=<code>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingUri {
    /// Addresses the daemon may be reached at, as `host:port`; try them in order.
    pub hosts: Vec<String>,
    /// SHA-256 of the daemon's certificate, lowercase hex: the client pins it.
    pub fingerprint: String,
    /// One-time pairing code, sent in [`PAIRING_CODE_HEADER`].
    pub code: String,
}

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
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let url = Url::parse(text).context("not a pairing link")?;
        if url.scheme() != "herder" || url.host_str() != Some("pair") {
            bail!("not a herder://pair link");
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
            bail!("the pairing link lacks the fingerprint or the code");
        };
        if hosts.is_empty() {
            bail!("the pairing link names no daemon address");
        }
        Ok(Self {
            hosts,
            fingerprint,
            code,
        })
    }
}

/// SHA-256 of a DER-encoded certificate, as lowercase hex: how daemons and devices know each
/// other's certificates.
fn fingerprint(cert_der: &[u8]) -> String {
    Sha256::digest(cert_der)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_key_survives_pem() {
        let key = DeviceKey::generate().unwrap();
        let again = DeviceKey::from_pem(key.to_pem()).unwrap();
        assert_eq!(key.fingerprint(), again.fingerprint());
        assert_ne!(
            key.fingerprint(),
            DeviceKey::generate().unwrap().fingerprint()
        );
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
    }
}
