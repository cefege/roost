//! Writing on one attempt's lanes: the `Hello`, a direct command, and the
//! bounded fragment drain every write shares.
//!
//! Owned by `pump::peer_lane`. The browser's data channels move BYTES and know
//! nothing about a carrier's encoding, so the framing is `PeerLanes`' and this
//! owns only the two things above it: which lane a frame belongs on, and how
//! many fragments one turn may commit.
//!
//! THE DRAIN IS BOUNDED TWICE, AND BOTH BOUNDS ARE THE PROTOCOL'S. A turn
//! commits at most `TERMINAL_PEER_MAX_FLUSH_BYTES_PER_TURN` across every lane,
//! and a lane stops while the browser is still holding more than its own high
//! watermark. Lanes are visited in the protocol's priority order — control, then
//! live terminal output, then history backfill — because a keystroke must not sit
//! behind a multi-megabyte baseline, which is the whole reason the history lane
//! exists.

use roost_client_core::TerminalToken;
use roost_client_core::client::carriers::{PeerLane, PeerTransport};
use roost_protocol::terminal_peer::peer::TERMINAL_PEER_MAX_FLUSH_BYTES_PER_TURN;

use super::Pump;
use crate::platform::carriers::{PeerCarrier, watermarks};

/// The lane every client frame belongs on.
///
/// `Control` for everything the client sends: views, input and the handshake. The
/// data and history lanes carry what the WORKER sends, and a peer that received a
/// client frame on either of them would be reading a stream the worker does not
/// watch for one.
const CLIENT_LANE: PeerLane = PeerLane::Control;

/// Why a write did not go out, named so every call site reports the same thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LaneRefusal {
    /// The attempt this write named is not open in this document.
    NoAttempt,
    /// The lane refused the message, or its queue was full.
    Refused(String),
}

impl std::fmt::Display for LaneRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAttempt => write!(formatter, "the attempt is not open in this document"),
            Self::Refused(detail) => write!(formatter, "{detail}"),
        }
    }
}

/// Queue and write one already-encoded client frame on an attempt's control lane.
///
/// `Ok(false)` means the lane's queue was full: backpressure, which the caller
/// reports and does not retry, because a resend of a message the queue may
/// already hold is a duplicate on an ORDERED lane.
pub(super) fn write_control(
    pump: &Pump,
    attempt_id: u64,
    message: Vec<u8>,
) -> Result<bool, LaneRefusal> {
    let queued = {
        let mut held = pump.inner.peer_attempts.borrow_mut();
        let Some(carrier) = held.attempt_mut(attempt_id) else {
            return Err(LaneRefusal::NoAttempt);
        };
        match carrier.enqueue(CLIENT_LANE, message) {
            Ok(queued) => queued,
            Err(fault) => return Err(LaneRefusal::Refused(fault.to_string())),
        }
    };
    if !queued {
        return Ok(false);
    }
    flush(pump, attempt_id);
    Ok(true)
}

/// Queue and write one `SendDirect` on the peer carrier presenting `token`.
///
/// The DECISION to send is the core's (`deliver_direct_command`) and happens in
/// `pump::carriers`; this owns the last step, which is where the token stops
/// being a value and becomes bytes on one lane of one peer.
pub(super) fn write_direct(
    pump: &Pump,
    token: &TerminalToken,
    bytes: Vec<u8>,
) -> Result<(), LaneRefusal> {
    let attempt_id = {
        let held = pump.inner.peer_attempts.borrow();
        let Some(carrier) = held.for_token(token) else {
            return Err(LaneRefusal::NoAttempt);
        };
        carrier.attempt_id()
    };
    write_control(pump, attempt_id, bytes).map(|_| ())
}

/// Commit as many queued fragments as this turn's bounds allow.
///
/// The fragments are taken and committed under the carrier's borrow and written
/// after it is released, because a browser write is a call out of this document
/// and the carrier is behind a `RefCell` the same tick may re-enter.
pub(super) fn flush(pump: &Pump, attempt_id: u64) -> usize {
    let mut committed = 0;
    for lane in PeerCarrier::write_order() {
        if !pump.inner.peer.borrow().lane_is_open(attempt_id, lane) {
            continue;
        }
        let (high_mark, _) = watermarks(lane);
        loop {
            if committed >= TERMINAL_PEER_MAX_FLUSH_BYTES_PER_TURN {
                return committed;
            }
            let next = {
                let mut held = pump.inner.peer_attempts.borrow_mut();
                let Some(carrier) = held.attempt_mut(attempt_id) else {
                    return committed;
                };
                match carrier.next_packet(lane) {
                    Ok(Some(bytes)) => bytes,
                    Ok(None) => break,
                    Err(fault) => {
                        tracing::warn!(
                            target: "carriers",
                            attempt_id,
                            fault = %fault,
                            "a peer lane refused to frame its next packet"
                        );
                        break;
                    }
                }
            };
            if pump.inner.peer.borrow().buffered_bytes(attempt_id) > high_mark as u64 {
                break;
            }
            if let Err(error) = pump.inner.peer.borrow_mut().send(attempt_id, lane, &next) {
                tracing::warn!(
                    target: "carriers",
                    attempt_id,
                    lane = lane.label(),
                    detail = %error,
                    "the browser refused a peer lane write"
                );
                break;
            }
            committed += next.len();
        }
    }
    committed
}
