//! What keeps an authenticated peer honest: the protocol's own transport probe
//! as its heartbeat, the browser's `getStats` for the one fact only the browser
//! knows, and the telemetry both publish.
//!
//! Owned by `pump::peer_lane`, driven by the tick. The heartbeat runs while the
//! page is visible and the worker's authenticated peer either serves a view
//! from an ACTIVE attempt — v2's `TerminalPeerOwner.heartbeat` guard — or is
//! held ready by pre-warm, and the miss that exhausts `HEARTBEAT_MISS_LIMIT` is reported to the
//! core as `ProbeMissed`, whose fault closes the attempt. Every
//! published value was measured: the round trip is a probe's, the candidate kind
//! is the browser's report, and nothing here defaults a zero a reader could not
//! tell from a real one.

use roost_client_core::client::carriers::{
    PeerPhase, PeerTelemetry, SignallingInput, encode_transport_probe,
};
use roost_client_core::sync::inbound::TransportProbeResult;
use roost_client_core::{ClientEvent, TerminalToken};

use super::Pump;
use super::write::write_control;
use crate::platform::carriers::{HeartbeatMiss, PeerCarrier, PeerHeartbeat};
use crate::platform::peer::PeerMeasurement;
use crate::platform::terminal_view_id::mint_probe_request_id;
use crate::platform::visibility::page_visible;

/// Advance every authenticated attempt's heartbeat, and send the probes and
/// stats reads that are due.
pub(super) fn start_due_probes(pump: &Pump, now_ms: u64) {
    let visible = page_visible();
    let (probes, reads) = {
        let core = pump.inner.core.borrow();
        let lane = &core.store().direct;
        let mut held = pump.inner.peer_attempts.borrow_mut();
        let (mut probes, mut reads) = (Vec::new(), Vec::new());
        for attempt_id in held.attempt_ids() {
            let Some(carrier) = held.attempt_mut(attempt_id) else {
                continue;
            };
            let worker_fp = carrier.worker_fp().to_owned();
            let snapshot = lane.snapshot(&worker_fp);
            // A pre-warmed peer is probed with no view on it: a warm peer that
            // died unnoticed is exactly the one the next pane is staged on.
            let serving = visible
                && snapshot.has_carrier
                && (snapshot.prewarmed
                    || (snapshot.phase == PeerPhase::Active && snapshot.active_views > 0));
            let Some(heartbeat) = carrier.heartbeat_mut() else {
                continue;
            };
            heartbeat.set_running(serving, now_ms);
            if heartbeat.is_due(now_ms) {
                probes.push((attempt_id, worker_fp));
            }
            if heartbeat.begin_stats_read(now_ms) {
                reads.push(attempt_id);
            }
        }
        (probes, reads)
    };
    for (attempt_id, worker_fp) in probes {
        send_probe(pump, attempt_id, &worker_fp, now_ms);
    }
    for attempt_id in reads {
        // A read the browser cannot start is simply not measured: the candidate
        // kind keeps its last value and the next interval asks again.
        let _ = pump.inner.peer.borrow().measure_attempt(attempt_id);
    }
}

impl Pump {
    /// Send one heartbeat probe now on the peer presenting `token`, outside the
    /// heartbeat's schedule, for a reader that wants a fresh measurement (v2
    /// `TerminalPeerConnection.probe`). `false` when no attempt presents it.
    pub fn probe_peer_route(&self, token: &TerminalToken, now_ms: u64) -> bool {
        let target = {
            let held = self.inner.peer_attempts.borrow();
            held.attempt_ids().into_iter().find_map(|attempt_id| {
                let carrier = held.attempt(attempt_id)?;
                (carrier.token() == Some(token))
                    .then(|| (attempt_id, carrier.worker_fp().to_owned()))
            })
        };
        let Some((attempt_id, worker_fp)) = target else {
            return false;
        };
        send_probe(self, attempt_id, &worker_fp, now_ms);
        true
    }
}

/// One heartbeat probe out on the attempt's control lane.
fn send_probe(pump: &Pump, attempt_id: u64, worker_fp: &str, now_ms: u64) {
    let Some(request_id) = mint_probe_request_id() else {
        tracing::warn!(
            target: "carriers",
            attempt_id,
            "no crypto.randomUUID: the peer heartbeat cannot mint a probe id"
        );
        settle_miss(pump, attempt_id, now_ms, |heartbeat, now| {
            heartbeat.probe_refused(now)
        });
        return;
    };
    let recorded = with_heartbeat(pump, attempt_id, |heartbeat| {
        heartbeat.probe_sent(&request_id, now_ms)
    })
    .unwrap_or(false);
    if !recorded {
        return;
    }
    let bytes = encode_transport_probe(&request_id, worker_fp);
    match write_control(pump, attempt_id, bytes) {
        Ok(true) => {}
        Ok(false) | Err(_) => {
            tracing::warn!(
                target: "carriers",
                attempt_id,
                "the control lane refused the peer heartbeat probe"
            );
            settle_miss(pump, attempt_id, now_ms, |heartbeat, now| {
                heartbeat.probe_refused(now)
            });
        }
    }
}

/// A probe answer arrived on the attempt that sent it.
pub(super) fn probe_answered(
    pump: &Pump,
    attempt_id: u64,
    result: &TransportProbeResult,
    now_ms: u64,
) {
    let settled = with_heartbeat(pump, attempt_id, |heartbeat| {
        heartbeat.answered(result, now_ms)
    })
    .unwrap_or(false);
    if !settled {
        tracing::debug!(
            target: "carriers",
            attempt_id,
            request_id = %result.request_id,
            "a probe answer for no probe this peer is waiting on"
        );
        return;
    }
    publish(pump, attempt_id, now_ms);
}

/// Every heartbeat probe whose deadline passed, and every qualification the
/// passing of time withdrew.
pub(super) fn read_heartbeat_deadlines(pump: &Pump, now_ms: u64) {
    let attempt_ids = pump.inner.peer_attempts.borrow().attempt_ids();
    for attempt_id in attempt_ids {
        settle_miss(pump, attempt_id, now_ms, |heartbeat, now| {
            heartbeat.lapsed(now)
        });
        let stale = with_heartbeat(pump, attempt_id, |heartbeat| {
            heartbeat.qualification_changed(now_ms)
        })
        .unwrap_or(false);
        if stale {
            publish(pump, attempt_id, now_ms);
        }
    }
}

/// One stats report settled: the candidate kind the browser selected.
pub(super) fn measurement_arrived(
    pump: &Pump,
    attempt_id: u64,
    measurement: PeerMeasurement,
    now_ms: u64,
) {
    let known = with_heartbeat(pump, attempt_id, |heartbeat| {
        heartbeat.stats_arrived(measurement.candidate_type, now_ms);
    });
    if known.is_some() {
        publish(pump, attempt_id, now_ms);
    }
}

/// Apply one possible miss — a lapsed deadline or a refused write share this
/// path — and report the peer's death when it is the last one tolerated.
fn settle_miss<F, M>(pump: &Pump, attempt_id: u64, now_ms: u64, decide: F)
where
    F: FnOnce(&mut PeerHeartbeat, u64) -> M,
    M: Into<Option<HeartbeatMiss>>,
{
    let Some(miss) = with_heartbeat(pump, attempt_id, |heartbeat| {
        decide(heartbeat, now_ms).into()
    })
    .flatten() else {
        return;
    };
    publish(pump, attempt_id, now_ms);
    let Some(worker_fp) = pump
        .inner
        .peer_attempts
        .borrow()
        .attempt(attempt_id)
        .map(|carrier| carrier.worker_fp().to_owned())
    else {
        return;
    };
    match miss {
        HeartbeatMiss::Tolerated => tracing::warn!(
            target: "carriers",
            attempt_id,
            worker_fp,
            "a peer heartbeat probe went unanswered; the miss is tolerated"
        ),
        HeartbeatMiss::Exhausted => {
            tracing::warn!(
                target: "carriers",
                attempt_id,
                worker_fp,
                "terminal peer heartbeat missed; the peer is closed"
            );
            pump.dispatch(ClientEvent::CarrierTransportObserved {
                worker_fp,
                observation: SignallingInput::ProbeMissed { attempt_id },
            });
        }
    }
}

/// Run `step` on an attempt's heartbeat, when the attempt holds one.
fn with_heartbeat<T>(
    pump: &Pump,
    attempt_id: u64,
    step: impl FnOnce(&mut PeerHeartbeat) -> T,
) -> Option<T> {
    pump.inner
        .peer_attempts
        .borrow_mut()
        .attempt_mut(attempt_id)
        .and_then(PeerCarrier::heartbeat_mut)
        .map(step)
}

/// What this document may observe about a worker, in the diagnostic's spelling.
///
/// The probe's round trip is published for BOTH `rtt_ms` and
/// `worker_control_rtt_ms`: the probe rides the control lane, so the round trip
/// it measured IS the control lane's, as v2 published it from the same probe.
fn publish(pump: &Pump, attempt_id: u64, now_ms: u64) {
    let Some((worker_fp, peer_id, queued, candidate_type, reading)) = pump
        .inner
        .peer_attempts
        .borrow()
        .attempt(attempt_id)
        .and_then(|carrier| {
            let heartbeat = carrier.heartbeat()?;
            let queued: u64 = PeerCarrier::write_order()
                .into_iter()
                .map(|lane| carrier.queued_bytes(lane) as u64)
                .sum();
            Some((
                carrier.worker_fp().to_owned(),
                carrier.attempt().peer_id.clone(),
                queued,
                heartbeat.candidate_type(),
                heartbeat.reading(now_ms),
            ))
        })
    else {
        return;
    };
    let browser_holding = pump.inner.peer.borrow().buffered_bytes(attempt_id);
    let buffered_bytes = queued.saturating_add(browser_holding);
    let _ = with_heartbeat(pump, attempt_id, |heartbeat| {
        heartbeat.mark_published(reading.liveness_qualified);
    });
    pump.inner
        .core
        .borrow_mut()
        .store_mut()
        .direct
        .record_telemetry(
            &worker_fp,
            PeerTelemetry {
                peer_id: Some(peer_id),
                candidate_type,
                last_probe_at_ms: reading.last_probe_at_ms,
                liveness_qualified: reading.liveness_qualified,
                rtt_ms: reading.rtt_ms,
                worker_control_rtt_ms: reading.rtt_ms,
                buffered_bytes: Some(buffered_bytes),
            },
        );
    tracing::debug!(
        target: "carriers",
        attempt_id,
        worker_fp,
        candidate_type = candidate_type.as_str(),
        rtt_ms = reading.rtt_ms,
        qualified = reading.liveness_qualified,
        buffered_bytes,
        "peer telemetry recorded"
    );
}
