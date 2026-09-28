//! The one-shot offer faults a disposable smoke worker arms, consumed at their
//! real owner boundary inside `peer::owner_offer`. The ordinary worker neither
//! creates nor receives this state: `runtime::owners` passes no slot. Ports the
//! offer half of v2 `apps/worker/src/terminal/peer/terminal-peer-test-faults.ts`.
//!
//! The fault surface is here rather than behind a hidden environment variable
//! because a fault that can be armed from outside the process is not a test
//! fixture, it is a production switch nobody will remember to turn off.

use std::sync::Mutex;

use super::packet_budget::lock;

/// How the next peer offer this worker handles is made to fail.
///
/// Each is one-shot: it is consumed at its real owner boundary, and a second
/// offer is unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfferFault {
    /// The offer handed to the native peer is not a valid SDP offer.
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

/// The armed fault, if any (v2 `armOfferFault` / `consumeOfferFault`).
#[derive(Debug, Default)]
pub struct OfferFaultSlot {
    next: Mutex<Option<OfferFault>>,
}

impl OfferFaultSlot {
    pub fn arm(&self, fault: OfferFault) {
        *lock(&self.next) = Some(fault);
        tracing::info!(
            fault = fault.as_str(),
            "a terminal peer offer fault was armed"
        );
    }

    pub fn consume(&self) -> Option<OfferFault> {
        lock(&self.next).take()
    }
}
