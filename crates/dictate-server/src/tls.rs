//! Optional HTTPS (design §6): rustls on the ring provider, TLS 1.2 and 1.3,
//! HTTP/1.1 only, no client certificates. The certificate is the operator's
//! (`tailscale cert`, or a self-signed one whose fingerprint the client pins);
//! the daemon never generates key material.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use tokio_rustls::rustls;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::TlsAcceptor;

use crate::token::fingerprint;

/// Build the acceptor for `cert` and `key`, and the leaf certificate's
/// SHA-256 fingerprint for pairing.
///
/// # Errors
///
/// A readable description when either file cannot be loaded or they do not
/// form a usable pair. Never includes key material.
pub fn acceptor(cert: &Path, key: &Path) -> Result<(TlsAcceptor, String), String> {
    let certs = load_certs(cert)?;
    let fingerprint = fingerprint(certs[0].as_ref());
    if let Ok(meta) = std::fs::metadata(key) {
        if meta.permissions().mode() & 0o077 != 0 {
            tracing::warn!(
                key = %key.display(),
                "the TLS private key is readable by other users; chmod 600 it"
            );
        }
    }
    let key = PrivateKeyDer::from_pem_file(key)
        .map_err(|e| format!("reading the private key {}: {e}", key.display()))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS protocol setup: {e}"))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("the TLS certificate and key do not form a usable pair: {e}"))?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok((TlsAcceptor::from(Arc::new(config)), fingerprint))
}

/// The SHA-256 fingerprint of the first certificate in a PEM file, for
/// `dictated --api-token` to print.
///
/// # Errors
///
/// When the file holds no readable certificate.
pub fn certificate_fingerprint(cert: &Path) -> Result<String, String> {
    Ok(fingerprint(load_certs(cert)?[0].as_ref()))
}

fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, String> {
    let certs = CertificateDer::pem_file_iter(path)
        .map_err(|e| format!("reading the certificate {}: {e}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("parsing the certificate {}: {e}", path.display()))?;
    if certs.is_empty() {
        return Err(format!("{} holds no certificate", path.display()));
    }
    Ok(certs)
}
