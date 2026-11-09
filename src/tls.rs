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
        if self
            .pinned
            .iter()
            .any(|c| c.as_ref() == end_entity.as_ref())
        {
            return Ok(ServerCertVerified::assertion());
        }
        match &self.roots {
            Some(v) => {
                v.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
            }
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
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
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
        Some(
            WebPkiServerVerifier::builder_with_provider(Arc::new(store), provider.clone())
                .build()?,
        )
    } else {
        None
    };
    let verifier = PinnedOrRoots {
        pinned: certs,
        roots,
        provider: provider.clone(),
    };
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Client, Tls};
    use crate::testutil::serve;
    use rustls::pki_types::PrivateKeyDer;

    const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/testdata");

    fn pem(name: &str) -> std::path::PathBuf {
        Path::new(DATA).join(name)
    }

    /// A WebUI serving `cert` (with its key) over HTTPS.
    fn https_webui(cert: &str, key: &str) -> String {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let chain = CertificateDer::pem_file_iter(pem(cert))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let key = PrivateKeyDer::from_pem_file(pem(key)).unwrap();
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .unwrap();
        let (base, _rx) = serve(Some(Arc::new(config)), |_, path| match path {
            "/api/v2/app/webapiVersion" => (200, "2.11.4"),
            _ => (200, "Ok."),
        });
        base
    }

    fn login(base: &str, ca_cert: &str) -> Result<()> {
        let tls = Tls {
            ca_cert: Some(pem(ca_cert)),
            insecure: false,
        };
        Client::new(base, "admin", "x", &tls)?.login()
    }

    #[test]
    fn a_pinned_self_signed_certificate_is_trusted() {
        let base = https_webui("selfsigned.pem", "selfsigned.key");
        login(&base, "selfsigned.pem").unwrap();
    }

    #[test]
    fn a_certificate_signed_by_the_given_ca_is_trusted() {
        let base = https_webui("leaf.pem", "leaf.key");
        login(&base, "ca.pem").unwrap();
    }

    #[test]
    fn any_other_certificate_is_rejected() {
        let base = https_webui("selfsigned.pem", "selfsigned.key");
        let err = login(&base, "other.pem").unwrap_err();
        assert!(
            format!("{err:#}").contains("invalid peer certificate"),
            "{err:#}"
        );

        // Pinning the leaf alone works, but trusting a different self-signed cert does not.
        let base = https_webui("leaf.pem", "leaf.key");
        login(&base, "leaf.pem").unwrap();
        assert!(login(&base, "selfsigned.pem").is_err());
    }

    #[test]
    fn insecure_accepts_anything() {
        let base = https_webui("selfsigned.pem", "selfsigned.key");
        let tls = Tls {
            ca_cert: None,
            insecure: true,
        };
        Client::new(&base, "admin", "x", &tls)
            .unwrap()
            .login()
            .unwrap();
    }

    #[test]
    fn unreadable_or_empty_ca_files_are_errors() {
        let err = config_trusting(&pem("missing.pem")).unwrap_err();
        assert!(
            err.to_string().starts_with("reading CA certificate"),
            "{err}"
        );
        let err = config_trusting(&pem("selfsigned.key")).unwrap_err();
        assert!(
            err.to_string().starts_with("no PEM certificates found"),
            "{err}"
        );
    }
}
