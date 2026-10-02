//! The daemon's self-signed TLS certificate, created on first start and reused afterwards.
//!
//! Clients cannot validate it against a CA; they pin its [`Tls::fingerprint`] instead. Every
//! client must present a certificate of its own, its device key: the handshake only proves the
//! client holds that certificate's key, and [`crate::auth::Auth`] decides afterwards whether the
//! device is paired.

use std::fs::{self, File};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use rustls::client::danger::HandshakeSignatureValid;
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, ServerConfig, SignatureScheme};
use sha2::{Digest, Sha256};
use tokio_rustls::TlsAcceptor;

use crate::data_dir::write_private;

const CERT_FILE: &str = "cert.pem";
const KEY_FILE: &str = "key.pem";

/// The daemon's TLS identity.
#[derive(Clone)]
pub struct Tls {
    config: Arc<ServerConfig>,
    fingerprint: String,
}

impl Tls {
    /// Loads the certificate and key from `dir`, generating them the first time.
    ///
    /// `host_name` goes into the certificate for display; clients pin the fingerprint, not
    /// the name.
    pub fn load_or_create(dir: &Path, host_name: &str) -> Result<Self> {
        let cert_path = dir.join(CERT_FILE);
        let key_path = dir.join(KEY_FILE);
        if !cert_path.exists() {
            create(dir, host_name)?;
        }
        let cert = fs::read(&cert_path)
            .map_err(anyhow::Error::from)
            .and_then(|pem| Ok(CertificateDer::from_pem_slice(&pem)?))
            .with_context(|| format!("reading {}", cert_path.display()))?;
        let key = fs::read(&key_path)
            .map_err(anyhow::Error::from)
            .and_then(|pem| Ok(PrivateKeyDer::from_pem_slice(&pem)?))
            .with_context(|| format!("reading {}", key_path.display()))?;
        let fingerprint = fingerprint(&cert);
        let provider = rustls::crypto::ring::default_provider();
        let devices = Arc::new(DeviceCerts {
            algorithms: provider.signature_verification_algorithms,
        });
        let config = ServerConfig::builder_with_provider(Arc::new(provider))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .context("configuring TLS")?
            .with_client_cert_verifier(devices)
            .with_single_cert(vec![cert], key)
            .with_context(|| format!("loading the TLS key pair from {}", dir.display()))?;
        Ok(Self {
            config: Arc::new(config),
            fingerprint,
        })
    }

    /// SHA-256 of the certificate's DER encoding, as lowercase hex.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub(crate) fn acceptor(&self) -> TlsAcceptor {
        TlsAcceptor::from(Arc::clone(&self.config))
    }
}

/// Requires a client certificate and checks the client signed the handshake with its key, but
/// trusts no issuer: devices use self-signed certificates and are known by fingerprint.
#[derive(Debug)]
struct DeviceCerts {
    algorithms: WebPkiSupportedAlgorithms,
}

impl ClientCertVerifier for DeviceCerts {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        // Whether this certificate belongs to a paired device is decided after the handshake,
        // so an unpaired device still reaches the hello and is told why it was refused.
        Ok(ClientCertVerified::assertion())
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

/// SHA-256 of a DER-encoded certificate, as lowercase hex.
pub fn fingerprint(cert_der: &[u8]) -> String {
    Sha256::digest(cert_der)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Generates the key pair and certificate; the key lands first, so a present certificate
/// always has its key.
fn create(dir: &Path, host_name: &str) -> Result<()> {
    if dir.join(KEY_FILE).exists() {
        bail!(
            "{} exists without {}; remove it to generate a new certificate",
            dir.join(KEY_FILE).display(),
            CERT_FILE
        );
    }
    let names = vec![host_name.to_owned(), "localhost".to_owned()];
    let generated =
        rcgen::generate_simple_self_signed(names).context("generating the TLS certificate")?;
    write_private(
        dir,
        KEY_FILE,
        generated.signing_key.serialize_pem().as_bytes(),
    )?;
    write_private(dir, CERT_FILE, generated.cert.pem().as_bytes())?;
    File::open(dir)
        .and_then(|dir| dir.sync_all())
        .with_context(|| format!("syncing {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn certificate_is_created_once_and_reused() {
        let tmp = tempfile::tempdir().unwrap();
        let first = Tls::load_or_create(tmp.path(), "box").unwrap();
        assert_eq!(first.fingerprint().len(), 64);
        let key_mode = fs::metadata(tmp.path().join(KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(key_mode & 0o777, 0o600);
        let second = Tls::load_or_create(tmp.path(), "box").unwrap();
        assert_eq!(first.fingerprint(), second.fingerprint());
    }

    #[test]
    fn a_key_without_a_certificate_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        Tls::load_or_create(tmp.path(), "box").unwrap();
        fs::remove_file(tmp.path().join(CERT_FILE)).unwrap();
        assert!(Tls::load_or_create(tmp.path(), "box").is_err());
    }
}
