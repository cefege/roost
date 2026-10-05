//! The loaded str0m transport: one crypto provider, one DTLS certificate
//! shared by every peer until it ages out, one gathering cache, and the
//! negotiated channels made before any remote description is applied.
//! Built by [`super::str0m_loader`]; `create` is called by the peer
//! connections. Ports v2 `apps/worker/src/terminal/peer/terminal-peer-native.ts`
//! (`loadTerminalPeerNative`, `native.cleanup`) and the `new PeerConnection` /
//! `createDataChannel` calls of `terminal-peer-connection.ts`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use str0m::RtcConfig;
use str0m::channel::ChannelConfig;
use str0m::config::{CryptoProvider, DtlsCert};

use super::gather_cache::GatherCache;
use super::peer_handle::Str0mPeer;
use super::str0m_peer::{ChannelSlot, PeerShared};
use super::{NativePeer, NativePeerConfig, NativePeerError, NativePeerEvents, NativePeerFactory};

/// How long one DTLS certificate is reused. DTLS-SRTP binds a connection to
/// the fingerprint its SDP carried, not to a fresh key per peer, so sharing it
/// costs no security; regenerating daily bounds how long one key is exposed.
const DTLS_CERT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// The loaded str0m transport (v2 `TerminalPeerNative` after `preload()`).
#[derive(Debug)]
pub struct Str0mPeerFactory {
    provider: Arc<CryptoProvider>,
    /// The certificate every new peer presents, and when it was generated.
    certificate: Mutex<(Instant, DtlsCert)>,
    gather_cache: Arc<GatherCache>,
    cleaned: AtomicBool,
}

impl Str0mPeerFactory {
    /// Ready when the compiled crypto provider can mint a DTLS certificate,
    /// which every peer needs; anything else is `native_unavailable`. The
    /// certificate minted here is the one the first peers present.
    pub fn load() -> Result<Self, NativePeerError> {
        let provider = Arc::new(str0m::crypto::from_feature_flags());
        let Some(certificate) = provider.dtls_provider.generate_certificate() else {
            tracing::warn!("the str0m crypto provider cannot mint a DTLS certificate");
            return Err(NativePeerError::Unavailable);
        };
        tracing::info!("the str0m peer transport is ready");
        Ok(Self {
            provider,
            certificate: Mutex::new((Instant::now(), certificate)),
            gather_cache: Arc::new(GatherCache::default()),
            cleaned: AtomicBool::new(false),
        })
    }

    /// The certificate a peer created at `now` presents, and the milliseconds
    /// this call spent generating it: zero while the current one is younger
    /// than [`DTLS_CERT_MAX_AGE`].
    fn certificate_at(&self, now: Instant) -> Result<(DtlsCert, u64), NativePeerError> {
        let mut held = self
            .certificate
            .lock()
            .map_err(|_| NativePeerError::Unavailable)?;
        if now.saturating_duration_since(held.0) < DTLS_CERT_MAX_AGE {
            return Ok((held.1.clone(), 0));
        }
        let started = Instant::now();
        let certificate = self
            .provider
            .dtls_provider
            .generate_certificate()
            .ok_or(NativePeerError::Unavailable)?;
        let certificate_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        tracing::info!(certificate_ms, "the peer DTLS certificate was regenerated");
        *held = (now, certificate.clone());
        Ok((certificate, certificate_ms))
    }
}

impl NativePeerFactory for Str0mPeerFactory {
    fn create(
        &self,
        config: NativePeerConfig,
    ) -> Result<(Arc<dyn NativePeer>, NativePeerEvents), NativePeerError> {
        if self.cleaned.load(Ordering::Acquire) {
            return Err(NativePeerError::Unavailable);
        }
        let (certificate, certificate_ms) = self.certificate_at(Instant::now())?;
        let mut rtc = RtcConfig::new()
            .set_crypto_provider(Arc::clone(&self.provider))
            .set_dtls_cert(certificate)
            .build(Instant::now());
        let slots = config
            .channels
            .iter()
            .map(|spec| {
                ChannelSlot::new(rtc.direct_api().create_data_channel(ChannelConfig {
                    label: spec.label.clone(),
                    ordered: spec.ordered,
                    negotiated: Some(spec.id),
                    protocol: spec.protocol.clone(),
                    ..ChannelConfig::default()
                }))
            })
            .collect();
        let (shared, events) = PeerShared::new(config, rtc, slots);
        tracing::debug!(peer = %shared.config.name, certificate_ms, "a native peer was created");
        let peer: Arc<dyn NativePeer> =
            Arc::new(Str0mPeer::new(shared, Arc::clone(&self.gather_cache)));
        Ok((peer, events))
    }

    fn cleanup(&self) {
        if !self.cleaned.swap(true, Ordering::AcqRel) {
            tracing::info!("the str0m peer transport was cleaned up");
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::time::Instant;

    use super::{DTLS_CERT_MAX_AGE, Str0mPeerFactory};

    #[test]
    fn peers_created_within_the_max_age_share_one_certificate_and_generate_nothing() {
        let factory = Str0mPeerFactory::load().unwrap();
        let now = Instant::now();
        let (first, _) = factory.certificate_at(now).unwrap();
        let (second, second_ms) = factory.certificate_at(now).unwrap();
        assert_eq!(first.certificate, second.certificate, "one fingerprint");
        assert_eq!(second_ms, 0, "the second peer generated nothing");
    }

    #[test]
    fn an_aged_certificate_is_regenerated_once_and_then_reused() {
        let factory = Str0mPeerFactory::load().unwrap();
        let now = Instant::now();
        let (original, _) = factory.certificate_at(now).unwrap();
        let aged = now + DTLS_CERT_MAX_AGE;
        let (renewed, _) = factory.certificate_at(aged).unwrap();
        assert_ne!(original.certificate, renewed.certificate);
        let (reused, reused_ms) = factory.certificate_at(aged).unwrap();
        assert_eq!(renewed.certificate, reused.certificate);
        assert_eq!(reused_ms, 0);
    }
}
