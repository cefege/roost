//! The loaded str0m transport: one crypto provider, one certificate per peer,
//! and the negotiated channels made before any remote description is applied.
//! Built by [`super::str0m_loader`]; `create` is called by the peer
//! connections. Ports v2 `apps/worker/src/terminal/peer/terminal-peer-native.ts`
//! (`loadTerminalPeerNative`, `native.cleanup`) and the `new PeerConnection` /
//! `createDataChannel` calls of `terminal-peer-connection.ts`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use str0m::RtcConfig;
use str0m::channel::ChannelConfig;
use str0m::config::CryptoProvider;

use super::peer_handle::Str0mPeer;
use super::str0m_peer::{ChannelSlot, PeerShared};
use super::{NativePeer, NativePeerConfig, NativePeerError, NativePeerEvents, NativePeerFactory};

/// The loaded str0m transport (v2 `TerminalPeerNative` after `preload()`).
#[derive(Debug)]
pub struct Str0mPeerFactory {
    provider: Arc<CryptoProvider>,
    cleaned: AtomicBool,
}

impl Str0mPeerFactory {
    /// Ready when the compiled crypto provider can mint a DTLS certificate,
    /// which every peer needs; anything else is `native_unavailable`.
    pub fn load() -> Result<Self, NativePeerError> {
        let provider = Arc::new(str0m::crypto::from_feature_flags());
        if provider.dtls_provider.generate_certificate().is_none() {
            tracing::warn!("the str0m crypto provider cannot mint a DTLS certificate");
            return Err(NativePeerError::Unavailable);
        }
        tracing::info!("the str0m peer transport is ready");
        Ok(Self {
            provider,
            cleaned: AtomicBool::new(false),
        })
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
        let certificate_started = Instant::now();
        let certificate = self
            .provider
            .dtls_provider
            .generate_certificate()
            .ok_or(NativePeerError::Unavailable)?;
        let certificate_ms =
            u64::try_from(certificate_started.elapsed().as_millis()).unwrap_or(u64::MAX);
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
        let peer: Arc<dyn NativePeer> = Arc::new(Str0mPeer::new(shared));
        Ok((peer, events))
    }

    fn cleanup(&self) {
        if !self.cleaned.swap(true, Ordering::AcqRel) {
            tracing::info!("the str0m peer transport was cleaned up");
        }
    }
}
