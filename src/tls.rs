//! Self-signed TLS for the SMTP listeners.
//!
//! A fresh certificate is minted once at startup (CN/SAN `localhost`) and shared
//! by every connection — both implicit-TLS ports (SMTPS) and STARTTLS upgrades.
//! Being self-signed, clients must disable certificate verification (or trust
//! the cert); that's expected for a local mock.

use std::sync::Arc;

use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::crypto::ring;
use tokio_rustls::rustls::pki_types::PrivatePkcs8KeyDer;

/// Build a [`TlsAcceptor`] backed by a freshly generated self-signed cert.
pub fn self_signed_acceptor() -> anyhow::Result<TlsAcceptor> {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])?;
    let cert_der = certified.cert.der().clone();
    let key_der = PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der());

    // Pin the crypto provider explicitly rather than relying on a process-global
    // default, so behavior is independent of init order (notably under tests).
    let config = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der.into())?;

    Ok(TlsAcceptor::from(Arc::new(config)))
}
