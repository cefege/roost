//! The deadlines an open peer can run out of, read against one tick's clock.
//!
//! Owned by `pump::peer_lane`, driven by the tick beside the event drain. Every
//! window here is the PROTOCOL's number rather than a host's, because the core
//! owns no timer and a host that chose its own would be a second set of limits:
//! two sets of limits is how a peer that is alive gets retired and a peer that is
//! dead gets waited on.
//!
//! EVERY DEADLINE ENDS THE ATTEMPT BY BEING REPORTED, NOT BY CLOSING. The report
//! is what makes the core emit its own `CloseAttempt`, and one closer means the
//! routes a carrier was serving are retired by the same code path whether the
//! attempt died on a lane, on a handshake, or on a queue overflow.

use roost_client_core::client::carriers::PeerLane;

use super::Pump;
use super::drain::{fault_attempt, ice_failed};
use crate::platform::carriers::PeerDeadline;

/// The attempts whose events were dropped for want of queue room.
///
/// An event the host never saw is exactly the silence this layer refuses to
/// leave, and a carrier whose frames were DISCARDED cannot be treated as one that
/// carried them: the retire is the point, not the count.
pub(super) fn retire_overflowed(pump: &Pump) {
    let overflowed = pump.inner.peer.borrow().take_overflowed();
    if overflowed.attempt_ids.is_empty() {
        return;
    }
    tracing::warn!(
        target: "carriers",
        dropped = overflowed.dropped,
        attempts = overflowed.attempt_ids.len(),
        "the browser's peer event queue overflowed; its attempts are retired"
    );
    for attempt_id in overflowed.attempt_ids {
        ice_failed(pump, attempt_id);
    }
}

/// Every deadline that has run out, named by the attempt it belongs to.
pub(super) fn read_deadlines(pump: &Pump, now_ms: u64) {
    for (attempt_id, deadline) in lapsed(pump, now_ms) {
        match deadline {
            // A read, not a fault: the offer is whatever the browser gathered,
            // and the core's own rule on an offer with no usable candidate is
            // what refuses it.
            PeerDeadline::GatheringLapsed => super::open::offer_ready(pump, attempt_id),
            PeerDeadline::HelloUnanswered => {
                tracing::warn!(
                    target: "carriers",
                    attempt_id,
                    "the worker did not prove its tuple inside the handshake window"
                );
                fault_attempt(pump, attempt_id, "identity_mismatch");
            }
        }
    }
    retire_stalled_lanes(pump, now_ms);
}

/// The whole table read once, so the carriers are not re-borrowed per deadline.
fn lapsed(pump: &Pump, now_ms: u64) -> Vec<(u64, PeerDeadline)> {
    let held = pump.inner.peer_attempts.borrow();
    let mut lapsed = Vec::new();
    for attempt_id in held.attempt_ids() {
        let Some(carrier) = held.attempt(attempt_id) else {
            continue;
        };
        lapsed.extend(
            carrier
                .life()
                .lapsed(now_ms)
                .into_iter()
                .map(|deadline| (attempt_id, deadline)),
        );
    }
    lapsed
}

/// Lanes that held one fragment without completing its message for longer than
/// the protocol's packet stall allows.
///
/// The whole peer goes rather than the one lane: an ordered lane that has lost
/// its sequence is carrying from a peer that cannot be trusted with the fragments
/// it already holds.
fn retire_stalled_lanes(pump: &Pump, now_ms: u64) {
    let stalled: Vec<(u64, Vec<PeerLane>)> = {
        let mut held = pump.inner.peer_attempts.borrow_mut();
        let mut stalled = Vec::new();
        for attempt_id in held.attempt_ids() {
            let Some(carrier) = held.attempt_mut(attempt_id) else {
                continue;
            };
            let lanes = carrier.stalled_lanes(now_ms);
            if !lanes.is_empty() {
                stalled.push((attempt_id, lanes));
            }
        }
        stalled
    };
    for (attempt_id, lanes) in stalled {
        let named = lanes
            .iter()
            .map(|lane| lane.label())
            .collect::<Vec<_>>()
            .join(", ");
        tracing::warn!(
            target: "carriers",
            attempt_id,
            lanes = named,
            "a peer lane stopped mid-message; the carrier is retired"
        );
        fault_attempt(pump, attempt_id, "ice_failed");
    }
}
