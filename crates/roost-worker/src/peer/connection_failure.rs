//! Why a terminal peer connection closed, and the reason that cause reaches the
//! browser as. Raised by `peer::connection`, logged by `peer::owner` in
//! `terminal peer closed`, and mapped onto the offer refusal vocabulary for
//! `peer::owner_offer`. Depends on the packet port's fatal reasons.

use super::owner::TerminalPeerOfferFailure;
use super::packet_lanes::DATA_CHANNEL_CLOSED;

/// Why a terminal peer connection closed. `IceFailed` and
/// `ConnectionSuperseded` are v2's `TerminalPeerConnectionFailureReason`; the
/// rest name the cause a log reader needs and reach the wire as `ice_failed`,
/// so the browser's vocabulary is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionFailure {
    /// ICE lost connectivity, gathering produced no candidate, or the native
    /// peer could not be built or driven.
    IceFailed,
    ConnectionSuperseded,
    /// The transport ended underneath the connection: the browser closed DTLS
    /// or SCTP gave up on the association.
    PeerClosed,
    /// The browser closed one of the fixed data channels, which it does when
    /// it retires the peer — a missed heartbeat, a closed tab, a route elsewhere.
    ChannelClosed,
    /// The packet port failed for any other reason — a channel error, a
    /// refused frame, a stalled lane; the port's own log line names which.
    PortFailed,
    /// The browser's DTLS fingerprint is not the one its offer named.
    FingerprintMismatch,
    /// The browser opened a channel or media section the offer never named.
    UnsolicitedChannel,
}

impl ConnectionFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IceFailed => "ice_failed",
            Self::ConnectionSuperseded => "connection_superseded",
            Self::PeerClosed => "peer_closed",
            Self::ChannelClosed => "channel_closed",
            Self::PortFailed => "port_failed",
            Self::FingerprintMismatch => "fingerprint_mismatch",
            Self::UnsolicitedChannel => "unsolicited_channel",
        }
    }

    /// The cause behind the packet port's fatal `reason`.
    pub(super) fn from_port_failure(reason: &str) -> Self {
        if reason == DATA_CHANNEL_CLOSED {
            Self::ChannelClosed
        } else {
            Self::PortFailed
        }
    }
}

impl From<ConnectionFailure> for TerminalPeerOfferFailure {
    fn from(failure: ConnectionFailure) -> Self {
        match failure {
            ConnectionFailure::ConnectionSuperseded => Self::ConnectionSuperseded,
            ConnectionFailure::IceFailed
            | ConnectionFailure::PeerClosed
            | ConnectionFailure::ChannelClosed
            | ConnectionFailure::PortFailed
            | ConnectionFailure::FingerprintMismatch
            | ConnectionFailure::UnsolicitedChannel => Self::IceFailed,
        }
    }
}
