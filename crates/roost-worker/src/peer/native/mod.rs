//! The native WebRTC transport both direct peers stand on: one str0m peer
//! connection behind the [`NativePeer`] trait, which the terminal and
//! attachment connections share. Built once by `runtime::owners` through
//! [`str0m_loader`]; called by `peer::connection` and the attachment peer.

mod driver;
mod factory;
mod gather;
mod gather_cache;
mod host_addresses;
mod peer_handle;
mod str0m_peer;
mod stun;

use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use roost_protocol::terminal_peer::sdp::normalize_terminal_peer_sha256_fingerprint;
use tokio::sync::{OnceCell, mpsc};

use crate::uplink::OwnerFuture;

pub use factory::Str0mPeerFactory;

/// One negotiated data channel, created before the remote offer is applied.
/// Its index in [`NativePeerConfig::channels`] names it in every event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeChannelSpec {
    pub id: u16,
    pub label: String,
    pub ordered: bool,
    pub protocol: String,
}

/// What one peer connection is built with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePeerConfig {
    /// A log label; never SDP, never a grant.
    pub name: String,
    /// Normalized `stun:host[:port]` URLs.
    pub stun_urls: Vec<String>,
    pub bind_address: Option<IpAddr>,
    /// Inclusive local UDP port range.
    pub port_range: Option<(u16, u16)>,
    /// The largest message `send` accepts.
    pub max_message_size: usize,
    pub channels: Vec<NativeChannelSpec>,
}

/// The transport's callbacks, as one ordered stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativePeerEvent {
    /// ICE and DTLS are up; the remote
    /// fingerprint is readable from here on.
    Connected,
    /// The connection failed (connection state or ICE state `failed`).
    Failed,
    /// The connection closed underneath its owner.
    Closed,
    /// The remote opened a channel or track nobody negotiated.
    UnsolicitedChannel,
    ChannelOpen(usize),
    ChannelMessage {
        channel: usize,
        binary: bool,
        data: Vec<u8>,
    },
    /// Buffered bytes fell to the channel's low threshold after exceeding it.
    BufferedAmountLow(usize),
    ChannelClosed(usize),
    /// A buffered write failed inside the transport.
    ChannelError(usize),
}

/// The remote DTLS certificate fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeFingerprint {
    pub algorithm: String,
    /// Colon-separated hex, as SDP spells it.
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NativePeerError {
    #[error("the native peer is closed")]
    Closed,
    #[error("the native peer transport is unavailable")]
    Unavailable,
    #[error("the remote description is not a usable offer")]
    InvalidOffer,
    #[error("an offer was already answered on this peer")]
    AlreadyAnswered,
    #[error("no local address could be bound for the peer")]
    NoLocalAddress,
    #[error("a {0}-byte message exceeds the peer's message size")]
    MessageTooLarge(usize),
    #[error("the peer has no channel {0}")]
    UnknownChannel(usize),
    #[error("the native peer transport failed: {0}")]
    Transport(String),
}

pub type NativePeerEvents = mpsc::UnboundedReceiver<NativePeerEvent>;

/// One peer connection plus its data channels, addressed by index.
pub trait NativePeer: Send + Sync + fmt::Debug {
    /// Applies the remote offer, then resolves with the local answer when
    /// candidate gathering completes or `gathering_deadline` passes, carrying
    /// whatever candidates exist by then — possibly none; the caller decides.
    fn answer(
        &self,
        offer_sdp: String,
        gathering_deadline: Duration,
    ) -> OwnerFuture<Result<String, NativePeerError>>;
    fn remote_fingerprint(&self) -> Option<NativeFingerprint>;
    /// `Ok(true)` handed to the transport now, `Ok(false)` accepted but
    /// buffered behind earlier bytes.
    fn send(&self, channel: usize, bytes: &[u8]) -> Result<bool, NativePeerError>;
    /// Accepted bytes the transport has not taken yet.
    fn buffered_amount(&self, channel: usize) -> usize;
    fn set_buffered_amount_low_threshold(&self, channel: usize, bytes: usize);
    fn is_open(&self, channel: usize) -> bool;
    fn close_channel(&self, channel: usize);
    fn close(&self);
}

/// The loaded transport.
pub trait NativePeerFactory: Send + Sync + fmt::Debug {
    fn create(
        &self,
        config: NativePeerConfig,
    ) -> Result<(Arc<dyn NativePeer>, NativePeerEvents), NativePeerError>;
    /// Process teardown; no peer is created after it.
    fn cleanup(&self);
}

/// Loads the transport once per loader.
pub type NativeLoader =
    Arc<dyn Fn() -> OwnerFuture<Result<Arc<dyn NativePeerFactory>, NativePeerError>> + Send + Sync>;

/// The production loader. Every clone shares one load, so the terminal and
/// attachment owners hold one factory.
pub fn str0m_loader() -> NativeLoader {
    let loaded: Arc<OnceCell<Arc<dyn NativePeerFactory>>> = Arc::new(OnceCell::new());
    Arc::new(move || {
        let loaded = Arc::clone(&loaded);
        Box::pin(async move {
            loaded
                .get_or_try_init(|| async {
                    Str0mPeerFactory::load()
                        .map(|factory| Arc::new(factory) as Arc<dyn NativePeerFactory>)
                })
                .await
                .cloned()
        })
    })
}

/// The fingerprint check: a SHA-256 certificate whose digest is
/// the one the offer declared. An unreadable fingerprint does not match.
pub fn remote_fingerprint_matches(peer: &dyn NativePeer, expected_sha256: &str) -> bool {
    let Some(fingerprint) = peer.remote_fingerprint() else {
        return false;
    };
    fingerprint.algorithm.eq_ignore_ascii_case("sha-256")
        && normalize_terminal_peer_sha256_fingerprint(&fingerprint.value).as_deref()
            == Some(expected_sha256)
}
