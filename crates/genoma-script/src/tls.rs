//! TLS for the script's network objects (`g.websocket`, `g.http`): rustls
//! with ring, the server checked against the system's certificates, read
//! once for the whole process.

use std::sync::{Arc, OnceLock};

use rustls::pki_types::CertificateDer;

/// The system's certificates, read the first time anything asks.
pub(crate) fn roots() -> Result<&'static [CertificateDer<'static>], String> {
    static ROOTS: OnceLock<Result<Vec<CertificateDer<'static>>, String>> = OnceLock::new();
    ROOTS
        .get_or_init(|| {
            let found = rustls_native_certs::load_native_certs();
            for e in &found.errors {
                log::warn!("tls: system certificates: {e}");
            }
            if found.certs.is_empty() {
                return Err("no system certificates to check a server with".into());
            }
            Ok(found.certs)
        })
        .as_ref()
        .map(|v| v.as_slice())
        .map_err(Clone::clone)
}

/// The cryptography: ring, one instance shared.
pub(crate) fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    static PROVIDER: OnceLock<Arc<rustls::crypto::CryptoProvider>> = OnceLock::new();
    PROVIDER.get_or_init(|| Arc::new(rustls::crypto::ring::default_provider())).clone()
}

/// A client's configuration, made once.
pub(crate) fn client_config() -> Result<Arc<rustls::ClientConfig>, String> {
    static CONFIG: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut store = rustls::RootCertStore::empty();
            let (added, _) = store.add_parsable_certificates(roots()?.iter().cloned());
            if added == 0 {
                return Err("no usable system certificates".into());
            }
            let config = rustls::ClientConfig::builder_with_provider(provider())
                .with_safe_default_protocol_versions()
                .map_err(|e| e.to_string())?
                .with_root_certificates(store)
                .with_no_client_auth();
            Ok(Arc::new(config))
        })
        .clone()
}
