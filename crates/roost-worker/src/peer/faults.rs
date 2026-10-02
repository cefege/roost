//! The faults a disposable smoke worker arms on its peer path: the one-shot
//! offer fault `peer::owner_offer` consumes, the outgoing packet blackhole the
//! port flush reads, and the malformed control packets injected through
//! `peer::packet_test_faults`. The ordinary worker neither creates nor
//! receives this state: `runtime::owners` passes `None` unless built with the
//! `smoke` feature and given fault sockets. Ports the peer half of v2
//! `apps/worker/src/terminal/peer/terminal-peer-test-faults.ts`.
//!
//! The fault surface is here rather than behind a hidden environment variable
//! because a fault that can be armed from outside the process is not a test
//! fixture, it is a production switch nobody will remember to turn off.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use roost_protocol::terminal_peer::peer::TERMINAL_PEER_PACKET_MAGIC;

use super::packet_budget::lock;

/// Every fault a disposable smoke worker arms on its peer path: one per
/// worker, shared by the owner, its connections and their ports (the peer
/// half of v2 `TerminalPeerTestFaultState`).
#[derive(Debug, Default)]
pub struct PeerTestFaults {
    offer: OfferFaultSlot,
    blackhole: AtomicBool,
}

impl PeerTestFaults {
    /// The one-shot offer fault `offer()` consumes.
    pub fn offer(&self) -> &OfferFaultSlot {
        &self.offer
    }

    /// v2 `setPacketBlackhole`: every authenticated port stops sending while
    /// set, and still receives.
    pub fn set_packet_blackhole(&self, blackholed: bool) {
        self.blackhole.store(blackholed, Ordering::SeqCst);
        tracing::info!(blackholed, "the terminal peer packet blackhole was set");
    }

    /// Whether an authenticated port drops what it would send.
    pub fn blackholes_outgoing(&self) -> bool {
        self.blackhole.load(Ordering::SeqCst)
    }

    /// v2 `dispose`: nothing armed outlives the harness that armed it.
    pub fn clear(&self) {
        self.offer.consume();
        self.blackhole.store(false, Ordering::SeqCst);
    }
}

/// Which header field of an injected control packet is malformed (v2
/// `TerminalPeerMalformedPacketKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MalformedPacket {
    /// A first fragment that claims a non-zero offset.
    Offset,
    /// A message that claims zero total bytes.
    Total,
    /// A message id of zero.
    Id,
}

impl MalformedPacket {
    /// The kind a command names, or `None` for any other name.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "offset" => Some(Self::Offset),
            "total" => Some(Self::Total),
            "id" => Some(Self::Id),
            _ => None,
        }
    }

    /// The 17 bytes v2 `malformedTerminalPeerControlPacket` builds: the magic,
    /// then id, total and offset as little-endian u32, then one payload byte.
    pub fn control_packet(self) -> Vec<u8> {
        let id = u32::from(self != Self::Id);
        let total = u32::from(self != Self::Total);
        let offset = u32::from(self == Self::Offset);
        let mut packet = Vec::with_capacity(17);
        for field in [TERMINAL_PEER_PACKET_MAGIC, id, total, offset] {
            packet.extend_from_slice(&field.to_le_bytes());
        }
        packet.push(1);
        packet
    }
}

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
