//! The WebRTC terminal peer: the str0m side of a browser reaching this
//! worker's PTYs over a data channel rather than the coordinator link. The door
//! module owns the loopback lane, and this owns the negotiated one. Depends on
//! `roost_protocol::terminal_peer` for the packet framing, the lane priorities,
//! the channel definitions and every bound — and on nothing here.
//!
//! The fault surface is here rather than behind a hidden environment variable
//! because a fault that can be armed from outside the process is not a test
//! fixture, it is a production switch nobody will remember to turn off. The
//! smoke harness arms these on a DISPOSABLE worker executable and
//! `smoke/terminal/stack.ts` refuses to run a faulting executable as a real
//! one; the ordinary worker neither creates nor receives this state.

/// How the next peer offer this worker handles is made to fail.
///
/// Each is one-shot: it is consumed at its real owner boundary, and a second
/// offer is unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfferFault {
    /// The answer handed back is not a valid SDP offer.
    InvalidSdp,
    /// No grant was presented at all.
    MissingGrant,
    /// A grant was presented and found expired.
    ExpiredGrant,
    /// A valid grant was presented for a different browser document.
    IdentityMismatch,
}

impl OfferFault {
    /// The wire name the disposable socket is armed with.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidSdp => "invalid_sdp",
            Self::MissingGrant => "missing_grant",
            Self::ExpiredGrant => "expired_grant",
            Self::IdentityMismatch => "identity_mismatch",
        }
    }

    /// The fault a command names, or `None` for a name this build does not
    /// implement. An unknown name is refused rather than ignored, so a smoke
    /// spec that misspells one fails instead of silently running without it.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "invalid_sdp" => Some(Self::InvalidSdp),
            "missing_grant" => Some(Self::MissingGrant),
            "expired_grant" => Some(Self::ExpiredGrant),
            "identity_mismatch" => Some(Self::IdentityMismatch),
            _ => None,
        }
    }
}
