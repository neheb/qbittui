//! HTTPS verification against a user-supplied certificate (`ca_cert`).

use anyhow::{Context, Result, bail};
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, Error, RootCertStore, SignatureScheme};
use std::path::Path;
use std::sync::Arc;

/// Accepts the server when it presents exactly one of the given certificates. That pins
/// self-signed certificates, which webpki otherwise rejects when they are marked as a CA
/// (the `openssl req -x509` default). Anything else is verified normally, with the given
/// certificates as the only trusted roots, so a private CA works too.
#[derive(Debug)]
struct PinnedOrRoots {
    pinned: Vec<CertificateDer<'static>>,
    roots: Option<Arc<WebPkiServerVerifier>>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedOrRoots {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        if self.pinned.iter().any(|c| c.as_ref() == end_entity.as_ref()) {
            return Ok(ServerCertVerified::assertion());
        }
        match &self.roots {
            Some(v) => v.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now),
            None => Err(Error::InvalidCertificate(CertificateError::UnknownIssuer)),
        }
    }

    // The handshake signatures are always checked, so a pinned certificate still proves
    // the server holds its private key.
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// A rustls config that trusts only the PEM certificate(s) in `path`.
pub fn config_trusting(path: &Path) -> Result<rustls::ClientConfig> {
    let certs = CertificateDer::pem_file_iter(path)
        .with_context(|| format!("reading CA certificate {}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("parsing CA certificate {}", path.display()))?;
    if certs.is_empty() {
        bail!("no PEM certificates found in {}", path.display());
    }

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut store = RootCertStore::empty();
    let (added, _) = store.add_parsable_certificates(certs.iter().cloned());
    let roots = if added > 0 {
        Some(WebPkiServerVerifier::builder_with_provider(Arc::new(store), provider.clone()).build()?)
    } else {
        None
    };
    let verifier = PinnedOrRoots { pinned: certs, roots, provider: provider.clone() };
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth())
}
