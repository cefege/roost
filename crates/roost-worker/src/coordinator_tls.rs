//! The TLS a worker speaks to an `https` coordinator: rustls over the ring
//! provider, trusting Mozilla's root store compiled into the binary. Called by
//! the boot-time Connect client (`bootstrap_redeem::activation`) and by the
//! link dial (`link_dial`), so both legs to one coordinator trust the same
//! roots. Depends on `rustls` and `webpki-roots` and nothing in this crate.

use std::sync::Arc;

/// The client configuration both coordinator legs use.
///
/// The roots are compiled in rather than read from the OS: a worker runs on
/// macOS, on any Linux, and under service managers whose environment may name
/// no CA bundle, and one fixed store behaves the same on every one of them.
/// The provider is named rather than taken from the process default because
/// more than one rustls provider is linked into this binary, and with two the
/// process default is a panic rather than a choice. No ALPN is set: the
/// Connect client sets its own, and the link's WebSocket upgrade needs
/// HTTP/1.1, which is what a handshake without ALPN negotiates.
pub fn coordinator_tls_config() -> Result<Arc<rustls::ClientConfig>, rustls::Error> {
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::coordinator_tls_config;

    #[test]
    fn the_configuration_offers_no_alpn_so_the_link_upgrade_stays_on_http1() {
        let config = coordinator_tls_config().expect("the ring provider supports TLS 1.2 and 1.3");
        assert!(
            config.alpn_protocols.is_empty(),
            "an ALPN list here would offer h2 to the WebSocket upgrade, which needs HTTP/1.1"
        );
    }
}
