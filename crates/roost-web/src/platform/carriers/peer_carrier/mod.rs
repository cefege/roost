//! One WebRTC carrier's lane framing: the packets a `DirectCommand` becomes,
//! and the logical messages a lane's fragments become.
//!
//! Owned by `platform::carriers`, called by `pump::peer_dial`. The browser's
//! data channels move bytes and know nothing about a carrier's encoding — which
//! is what keeps the packet header out of every WebRTC stack — so the framing
//! lives here, on top of `roost_protocol::terminal_peer`'s queues.
//!
//! Three lanes, three budgets, and one rule each: the control lane is bounded
//! tightly because it carries keystrokes and view state; the terminal lane may
//! carry a whole frame; the history lane may carry a multi-megabyte baseline.
//! Those caps are `roost_protocol`'s, not values chosen here, because the
//! worker's own ports enforce the same three and a browser that disagreed with
//! its worker would fail only on the largest frame.
//!
//! Target-independent on purpose: every counter, budget and refusal is decided
//! by native tests, and the browser supplies only the fragments.

mod attempt;
mod heartbeat;
mod life;

pub use attempt::{PeerCarrier, PeerCarriers};
pub use heartbeat::{HeartbeatMiss, PeerHeartbeat};
pub use life::{GATHERING_SETTLE_AFTER_REFLEXIVE_MS, PeerDeadline, PeerLife};

use std::collections::BTreeMap;

use roost_protocol::terminal_peer::packet_queue::{
    TerminalPeerPacketQueue, TerminalPeerPacketQueueFragment,
};
use roost_protocol::terminal_peer::packets::{
    TerminalPeerPacketAssembler, TerminalPeerPacketErrorCode, TerminalPeerPacketQuota,
};
use roost_protocol::terminal_peer::peer::{
    TERMINAL_PEER_LANE_PRIORITY, TERMINAL_PEER_MAX_HISTORY_READS_PER_PEER,
    TERMINAL_PEER_PACKET_STALL_MS, TerminalPeerChannelWatermarks, TerminalPeerLaneByteCaps,
};

use roost_client_core::client::carriers::PeerLane;

/// Why one peer's lanes refused a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaneFault {
    /// The lane's queue is full, or its own budget is spent.
    ///
    /// Not a protocol fault: the peer is still correct and the caller is
    /// expected to report backpressure rather than retry, because a resend of a
    /// message the queue may already hold is a duplicate on an ORDERED lane.
    Backpressured {
        /// Which lane.
        lane: PeerLane,
    },
    /// The bytes are not a packet this lane can carry.
    Unreadable {
        /// Which lane.
        lane: PeerLane,
        /// What the decoder said.
        detail: String,
    },
    /// The fragment did not continue the message the lane was assembling.
    ///
    /// Distinct from `Unreadable` because the bytes PARSED: an ordered lane that
    /// loses its sequence is carrying from a peer that cannot be trusted with
    /// the fragments it already holds, so the whole peer goes rather than the
    /// one message.
    OutOfOrder {
        /// Which lane.
        lane: PeerLane,
    },
    /// A lane held one fragment without its message completing for longer than
    /// the protocol's packet stall allows.
    Stalled {
        /// Which lane.
        lane: PeerLane,
    },
}

impl std::fmt::Display for LaneFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backpressured { lane } => {
                write!(formatter, "the {} lane is backpressured", lane.label())
            }
            Self::Unreadable { lane, detail } => {
                write!(
                    formatter,
                    "the {} lane is unreadable: {detail}",
                    lane.label()
                )
            }
            Self::OutOfOrder { lane } => {
                write!(
                    formatter,
                    "the {} lane lost its fragment order",
                    lane.label()
                )
            }
            Self::Stalled { lane } => {
                write!(formatter, "the {} lane stopped mid-message", lane.label())
            }
        }
    }
}

impl std::error::Error for LaneFault {}

/// Which refusal a packet error is, decided by WHAT it says rather than by
/// re-reading the lane.
///
/// `FragmentOrder` is the one that retires the peer and `Quota` is the one that
/// does not, and both arrive from the same decoder, so the mapping lives in one
/// place instead of at each call site.
fn classify(lane: PeerLane, code: TerminalPeerPacketErrorCode) -> LaneFault {
    match code {
        TerminalPeerPacketErrorCode::FragmentOrder => LaneFault::OutOfOrder { lane },
        TerminalPeerPacketErrorCode::Quota | TerminalPeerPacketErrorCode::Allocation => {
            LaneFault::Backpressured { lane }
        }
        TerminalPeerPacketErrorCode::FragmentStalled => LaneFault::Stalled { lane },
        other => LaneFault::Unreadable {
            lane,
            detail: other.as_str().to_owned(),
        },
    }
}

/// One lane's retained-byte budget, capped at that lane's own protocol cap.
///
/// The cap is per lane rather than one sum across all three, because the
/// protocol's caps are per lane: a shared sum would let a peer fill the
/// history lane's 64 MiB allowance and then be told the control lane — which
/// is holding a keystroke — is full.
#[derive(Debug)]
pub struct LaneBudget {
    cap_bytes: usize,
    retained: usize,
}

impl LaneBudget {
    /// A budget for one lane, empty.
    #[must_use]
    pub fn new(lane: PeerLane) -> Self {
        Self {
            cap_bytes: TerminalPeerLaneByteCaps::for_lane(lane.packet_lane()),
            retained: 0,
        }
    }

    /// How many bytes this lane retains right now.
    pub fn retained(&self) -> usize {
        self.retained
    }
}

impl TerminalPeerPacketQuota for LaneBudget {
    fn reserve(&mut self, bytes: usize) -> bool {
        let Some(after) = self.retained.checked_add(bytes) else {
            return false;
        };
        if after > self.cap_bytes {
            return false;
        }
        self.retained = after;
        true
    }

    fn release(&mut self, bytes: usize) {
        self.retained = self.retained.saturating_sub(bytes);
    }
}

/// One logical message off a lane, with the carrier's own framing removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneMessage {
    /// Which attempt carried it.
    pub attempt_id: u64,
    /// Which lane.
    pub lane: PeerLane,
    /// The reassembled message.
    pub bytes: Vec<u8>,
}

/// One attempt's three lanes, outbound and inbound.
#[derive(Debug)]
pub struct PeerLanes {
    attempt_id: u64,
    outbound: BTreeMap<u16, TerminalPeerPacketQueue<LaneBudget>>,
    inbound: BTreeMap<u16, TerminalPeerPacketAssembler<LaneBudget>>,
    history_reads: u32,
}

impl PeerLanes {
    /// The three lanes for one attempt, empty.
    ///
    /// `peer_id` is not a parameter: the lanes belong to the ATTEMPT, and the
    /// attempt is what every later message is checked against.
    #[must_use]
    pub fn new(attempt_id: u64) -> Self {
        let mut outbound = BTreeMap::new();
        let mut inbound = BTreeMap::new();
        for lane in PeerLane::ALL {
            outbound.insert(
                lane.stream_id(),
                TerminalPeerPacketQueue::new(lane.packet_lane(), LaneBudget::new(lane)),
            );
            inbound.insert(
                lane.stream_id(),
                TerminalPeerPacketAssembler::new(lane.packet_lane(), LaneBudget::new(lane)),
            );
        }
        Self {
            attempt_id,
            outbound,
            inbound,
            history_reads: 0,
        }
    }

    /// Which attempt these lanes belong to.
    pub fn attempt_id(&self) -> u64 {
        self.attempt_id
    }

    /// Queue one logical message on a lane.
    ///
    /// `Ok(false)` means the lane's queue is full and the message was NOT
    /// retained: backpressure, which the caller reports and does not retry.
    pub fn enqueue(&mut self, lane: PeerLane, message: Vec<u8>) -> Result<bool, LaneFault> {
        if lane == PeerLane::History {
            self.history_reads += 1;
            if self.history_reads > TERMINAL_PEER_MAX_HISTORY_READS_PER_PEER as u32 {
                self.history_reads -= 1;
                return Err(LaneFault::Backpressured { lane });
            }
        }
        self.outbound
            .get_mut(&lane.stream_id())
            .ok_or_else(|| missing_lane(lane))?
            .enqueue(message)
            .map_err(|error| classify(lane, error))
    }

    /// The next fragment this lane may write, or `None` when it has none.
    ///
    /// The fragment borrows the lane's queue, so a packet cannot be written
    /// after the bytes behind it have moved on.
    pub fn next_fragment(
        &mut self,
        lane: PeerLane,
    ) -> Result<Option<TerminalPeerPacketQueueFragment<'_, LaneBudget>>, LaneFault> {
        self.outbound
            .get_mut(&lane.stream_id())
            .ok_or_else(|| missing_lane(lane))?
            .next_fragment()
            .map_err(|error| classify(lane, error))
    }

    /// The complete logical bytes one lane still holds, for the diagnostic
    /// that reports what a carrier is holding and cannot write yet.
    pub fn queued_bytes(&self, lane: PeerLane) -> usize {
        self.outbound
            .get(&lane.stream_id())
            .map_or(0, TerminalPeerPacketQueue::queued_bytes)
    }

    /// Feed one arrived fragment to its lane's assembler.
    ///
    /// `Ok(None)` means the fragment was retained and the message is still
    /// incomplete; `Ok(Some)` is a whole message, ready to decode.
    pub fn push(
        &mut self,
        lane: PeerLane,
        now_ms: u64,
        packet: &[u8],
    ) -> Result<Option<Vec<u8>>, LaneFault> {
        self.inbound
            .get_mut(&lane.stream_id())
            .ok_or_else(|| missing_lane(lane))?
            .push(packet, now_ms)
            .map_err(|error| classify(lane, error))
    }

    /// Retire any lane that held one fragment for longer than the protocol's
    /// packet stall, and name them.
    ///
    /// The assembler applies the stall window itself, so this only says WHEN to
    /// look — which is the host's clock, because the core owns no timer.
    pub fn expired_lanes(&mut self, now_ms: u64) -> Vec<PeerLane> {
        let mut expired = Vec::new();
        for lane in PeerLane::ALL {
            let Some(assembler) = self.inbound.get_mut(&lane.stream_id()) else {
                continue;
            };
            if assembler.expire(now_ms) {
                expired.push(lane);
            }
        }
        expired
    }
}

/// How long a lane may hold one fragment without completing its message.
pub const LANE_STALL_MS: u64 = TERMINAL_PEER_PACKET_STALL_MS;

/// The lanes a sender drains, in the priority order the protocol fixes.
///
/// Control first, then live terminal output, then history backfill: a keystroke
/// must not sit behind a multi-megabyte baseline, which is the whole reason the
/// history lane exists.
#[must_use]
pub fn drain_order() -> [PeerLane; 3] {
    TERMINAL_PEER_LANE_PRIORITY.map(PeerLane::of_packet_lane)
}

/// The retained count at which a lane suspends writing, and the much lower one
/// it resumes at.
#[must_use]
pub fn watermarks(lane: PeerLane) -> (usize, usize) {
    TerminalPeerChannelWatermarks::for_lane(lane.packet_lane())
}

/// The largest logical message this lane may carry.
#[must_use]
pub fn lane_cap(lane: PeerLane) -> usize {
    TerminalPeerLaneByteCaps::for_lane(lane.packet_lane())
}

/// A lane this peer was never opened with.
fn missing_lane(lane: PeerLane) -> LaneFault {
    LaneFault::Unreadable {
        lane,
        detail: "no-such-lane".to_owned(),
    }
}

#[cfg(test)]
mod tests;
