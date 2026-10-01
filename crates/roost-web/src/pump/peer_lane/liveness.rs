//! What the browser measured about a live peer, and the protocol's own
//! heartbeat that keeps the measurement honest.
//!
//! Owned by `pump::peer_lane`, driven by the tick. It answers two questions a
//! route diagnostic asks and refuses to guess at: which kind of address the pair
//! is, and how far away it is. Every value it publishes was READ from the
//! browser's own `getStats` report — there is no default candidate and no zero
//! round trip anywhere in this file, because a reader cannot tell a fabricated
//! zero from a measured one and would call a dead peer healthy.
//!
//! THE HEARTBEAT USES THE PROTOCOL'S OWN WINDOWS. A read starts once per
//! `TERMINAL_PEER_HEARTBEAT_INTERVAL_MS` and has `TERMINAL_PEER_PROBE_DEADLINE_MS`
//! to come back with a paired path. The answer is the browser's own candidate
//! pair, because the browser exposes no per-lane round trip and a relay pair is
//! paired without being one of the three candidate kinds this protocol spells —
//! so the rule reads whether a pair EXISTS rather than what kind it is.

use roost_client_core::client::carriers::PeerTelemetry;

use super::Pump;
use crate::platform::carriers::PeerCarrier;
use crate::platform::peer::PeerMeasurement;

/// Start a stats read on every attempt whose heartbeat interval has elapsed.
pub(super) fn start_due_reads(pump: &Pump, now_ms: u64) {
    let due = {
        let mut held = pump.inner.peer_attempts.borrow_mut();
        let mut due = Vec::new();
        for attempt_id in held.attempt_ids() {
            let Some(carrier) = held.attempt_mut(attempt_id) else {
                continue;
            };
            let life = carrier.life_mut();
            if life.read_is_due(now_ms) && life.begin_read(now_ms) {
                due.push(attempt_id);
            }
        }
        due
    };
    for attempt_id in due {
        if pump.inner.peer.borrow().measure_attempt(attempt_id) {
            continue;
        }
        // The read cannot start for a peer this document does not hold, so the
        // outstanding flag is cleared rather than left to expire into a fault
        // about a peer that is already gone.
        if let Some(carrier) = pump
            .inner
            .peer_attempts
            .borrow_mut()
            .attempt_mut(attempt_id)
        {
            carrier.life_mut().read_unconfirmed();
        }
    }
}

/// One report settled: advance the attempt's clock and publish the measurement.
pub(super) fn measurement_arrived(
    pump: &Pump,
    attempt_id: u64,
    measurement: PeerMeasurement,
    now_ms: u64,
) {
    {
        let mut held = pump.inner.peer_attempts.borrow_mut();
        let Some(carrier) = held.attempt_mut(attempt_id) else {
            return;
        };
        if measurement.paired {
            carrier.life_mut().read_confirmed(now_ms);
        } else {
            carrier.life_mut().read_unconfirmed();
        }
    }
    publish(pump, attempt_id, measurement);
}

/// What this document may observe about a worker, in the diagnostic's spelling.
///
/// The round trip is published for BOTH `rtt_ms` and `worker_control_rtt_ms`
/// because the browser measures exactly one round trip per ICE candidate pair and
/// all three ordered streams share that pair: a control-lane frame's round trip
/// IS the pair's round trip. v2 published the same number in both fields, from a
/// probe the far end measured.
///
/// `probe_age_ms` is `None`, and that is the honest value rather than a missing
/// one: the client core's wire vocabulary carries no encoder for a content-free
/// `transport_probe`, so no probe is running and there is no answered probe to
/// measure an age from.
fn publish(pump: &Pump, attempt_id: u64, measurement: PeerMeasurement) {
    let Some((worker_fp, peer_id, queued)) = pump
        .inner
        .peer_attempts
        .borrow()
        .attempt(attempt_id)
        .map(|carrier| {
            let queued: u64 = PeerCarrier::write_order()
                .into_iter()
                .map(|lane| carrier.queued_bytes(lane) as u64)
                .sum();
            (
                carrier.worker_fp().to_owned(),
                carrier.attempt().peer_id.clone(),
                queued,
            )
        })
    else {
        return;
    };
    let browser_holding = pump.inner.peer.borrow().buffered_bytes(attempt_id);
    let buffered_bytes = queued.saturating_add(browser_holding);
    pump.inner
        .core
        .borrow_mut()
        .store_mut()
        .direct
        .record_telemetry(
            &worker_fp,
            PeerTelemetry {
                peer_id: Some(peer_id),
                candidate_type: measurement.candidate_type,
                probe_age_ms: None,
                rtt_ms: measurement.round_trip_ms,
                worker_control_rtt_ms: measurement.round_trip_ms,
                buffered_bytes: Some(buffered_bytes),
            },
        );
    tracing::debug!(
        target: "carriers",
        attempt_id,
        worker_fp,
        candidate_type = measurement.candidate_type.as_str(),
        rtt_ms = measurement.round_trip_ms,
        buffered_bytes,
        "peer telemetry recorded from the browser's own report"
    );
}
