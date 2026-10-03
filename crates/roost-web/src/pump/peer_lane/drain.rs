//! How every open peer's events reach the core: the bounded event sink is
//! drained on the next task after it rings, the protocol's deadlines are read on
//! the scheduled tick, and one arrived byte at a time becomes a `ClientEvent`.
//!
//! Owned by `pump::peer_lane`. The drain is never a handler inside a browser
//! callback, because a callback may fire while the pump is already borrowed:
//! every fact the browser produced is pushed into a bounded sink by
//! `platform::peer`, and this is where the sink is emptied, in arrival order,
//! with nothing held across a write.
//!
//! THE FENCE THIS FILE EXISTS TO KEEP: a byte is settled against the ATTEMPT that
//! named it. An event for an attempt this document no longer holds is discarded
//! and logged, because the alternative — folding it against whatever attempt
//! took that id's place — paints a retired generation's grid into a live
//! replica. The attempt id is never reused by the core, so "no attempt" is a
//! fact about a dead negotiation and never about a replacement.

use std::rc::Rc;

use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;

use roost_client_core::ClientEvent;
use roost_client_core::client::carriers::wire::decode_server_frame;
use roost_client_core::client::carriers::{DirectInbound, PeerLane, ReadyTuple, SignallingInput};
use roost_client_core::client::local::door::LoopbackReady;

use super::deadlines;
use super::liveness;
use super::open::{offer_ready, refuse_attempt};
use super::stage::lane_opened;
use super::{PEER_TICK_INTERVAL_MS, Pump};
use crate::platform::peer::PeerEvent;

/// Install the tick the browser drives this document's peers on, and the
/// notify that drains what the browser reports as soon as it reports it.
pub(in crate::pump) fn install_tick(pump: &Pump) {
    // Left to the tick, a frame waits up to a whole tick and two viewers of one
    // session paint it a tick apart. Sync and loopback drain on the next task
    // for the same reason, as v2 handled every data-channel message on arrival.
    let notify: Rc<dyn Fn()> = {
        let pump = pump.clone();
        Rc::new(move || schedule_drain(&pump))
    };
    pump.inner.peer.borrow().notify_on_event(notify);
    let Some(window) = web_sys::window() else {
        tracing::warn!(target: "carriers", "no browser window; the peer tick was not installed");
        return;
    };
    let tick = Closure::<dyn FnMut()>::new({
        let pump = pump.clone();
        move || tick(&pump)
    });
    if window
        .set_interval_with_callback_and_timeout_and_arguments_0(
            tick.as_ref().unchecked_ref(),
            PEER_TICK_INTERVAL_MS,
        )
        .is_err()
    {
        tracing::warn!(target: "carriers", "the browser refused the peer tick");
    }
    pump.inner.listeners.borrow_mut().push(Box::new(tick));
}

/// Drain on the next task, never inside the callback that recorded the event.
fn schedule_drain(pump: &Pump) {
    let pump = pump.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let now_ms = pump.inner.core.borrow().clock().now_ms();
        drain_events(&pump, now_ms);
        deadlines::retire_overflowed(&pump);
    });
}

/// One pass over every open peer: probe, drain, and read the deadlines.
pub(super) fn tick(pump: &Pump) {
    let now_ms = pump.inner.core.borrow().clock().now_ms();
    liveness::start_due_probes(pump, now_ms);
    drain_events(pump, now_ms);
    deadlines::retire_overflowed(pump);
    deadlines::read_deadlines(pump, now_ms);
    liveness::read_heartbeat_deadlines(pump, now_ms);
}

/// Everything the browser reported since the last drain, in arrival order.
fn drain_events(pump: &Pump, now_ms: u64) {
    let events = pump.inner.peer.borrow().drain_events();
    for event in events {
        match event {
            PeerEvent::Gathered { attempt_id } => offer_ready(pump, attempt_id),
            PeerEvent::LaneOpen { attempt_id, lane } => lane_opened(pump, attempt_id, lane),
            PeerEvent::LaneFailed {
                attempt_id,
                lane,
                reason,
            } => {
                tracing::warn!(
                    target: "carriers",
                    attempt_id,
                    lane = lane.label(),
                    reason,
                    "a peer lane ended; the carrier cannot carry on it"
                );
                fault_attempt(pump, attempt_id, "ice_failed");
            }
            PeerEvent::IceFailed { attempt_id, reason } => {
                tracing::warn!(
                    target: "carriers",
                    attempt_id,
                    reason,
                    "the browser reported that this peer's ICE is gone"
                );
                ice_failed(pump, attempt_id);
            }
            PeerEvent::Bytes {
                attempt_id,
                lane,
                bytes,
            } => settle_bytes(pump, attempt_id, lane, &bytes, now_ms),
            PeerEvent::Measured {
                attempt_id,
                measurement,
            } => liveness::measurement_arrived(pump, attempt_id, measurement, now_ms),
        }
    }
}

/// One arrived fragment, settled against the attempt that named it.
fn settle_bytes(pump: &Pump, attempt_id: u64, lane: PeerLane, bytes: &[u8], now_ms: u64) {
    let Some(known) = pump
        .inner
        .peer_attempts
        .borrow()
        .attempt(attempt_id)
        .map(|carrier| (carrier.is_authenticated(), carrier.worker_fp().to_owned()))
    else {
        tracing::debug!(
            target: "carriers",
            attempt_id,
            lane = lane.label(),
            bytes = bytes.len(),
            "a peer frame arrived for an attempt this document no longer holds"
        );
        return;
    };
    let (authenticated, worker_fp) = known;
    // The credential is spent on the control lane and the worker answers there;
    // anything on a data lane before that is a peer that skipped the handshake.
    if !authenticated && lane != PeerLane::Control {
        tracing::warn!(
            target: "carriers",
            attempt_id,
            worker_fp,
            lane = lane.label(),
            "a peer sent data before its Hello; the carrier is refused"
        );
        fault_attempt(pump, attempt_id, "ice_failed");
        return;
    }
    let assembled = match pump
        .inner
        .peer_attempts
        .borrow_mut()
        .attempt_mut(attempt_id)
    {
        Some(carrier) => carrier.push(lane, now_ms, bytes),
        None => return,
    };
    let message = match assembled {
        Ok(message) => message,
        Err(fault) => {
            tracing::warn!(
                target: "carriers",
                attempt_id,
                worker_fp,
                lane = lane.label(),
                fault = %fault,
                "a peer lane refused an arrived fragment; the carrier is retired"
            );
            fault_attempt(pump, attempt_id, "ice_failed");
            return;
        }
    };
    let Some(message) = message else {
        return;
    };
    deliver(
        pump,
        attempt_id,
        &worker_fp,
        authenticated,
        &message,
        now_ms,
    );
}

/// One complete logical message off a lane, translated by the core's own rules.
fn deliver(
    pump: &Pump,
    attempt_id: u64,
    worker_fp: &str,
    authenticated: bool,
    message: &[u8],
    now_ms: u64,
) {
    let inbound = match decode_server_frame(message, authenticated) {
        Ok(inbound) => inbound,
        Err(error) => {
            // Reported and dropped rather than fatal: `decode_server_frame`
            // refuses arms the client core has no rule for, and a host that
            // retired a carrier on one would take a live session down over a
            // frame the core simply has no use for.
            tracing::warn!(
                target: "carriers",
                attempt_id,
                worker_fp,
                error = %error,
                "a peer frame did not decode; it was reported and dropped"
            );
            return;
        }
    };
    match inbound {
        DirectInbound::Ready(ready) => {
            let tuple = ready_tuple(&ready);
            if tuple.socket_generation == 0 || tuple.socket_id.is_empty() {
                tracing::warn!(
                    target: "carriers",
                    attempt_id,
                    worker_fp,
                    "a peer proved no socket generation to fence against"
                );
                refuse_attempt(
                    pump,
                    worker_fp,
                    Some(attempt_id),
                    "identity_mismatch".to_owned(),
                );
                return;
            }
            pump.dispatch(ClientEvent::CarrierTransportObserved {
                worker_fp: worker_fp.to_owned(),
                observation: SignallingInput::PeerAuthenticated {
                    attempt_id,
                    ready: tuple,
                },
            });
        }
        DirectInbound::PreHelloFrame => {
            tracing::warn!(
                target: "carriers",
                attempt_id,
                worker_fp,
                "a peer sent a frame before its Ready; the carrier is refused"
            );
            fault_attempt(pump, attempt_id, "identity_mismatch");
        }
        DirectInbound::Closed { reason } => {
            tracing::info!(
                target: "carriers",
                attempt_id,
                worker_fp,
                reason = %if reason.is_empty() { "the worker sent none" } else { reason.as_str() },
                "the worker closed the peer carrier"
            );
            fault_attempt(pump, attempt_id, "ice_failed");
        }
        DirectInbound::TransportProbeResult(result) => {
            liveness::probe_answered(pump, attempt_id, &result, now_ms);
        }
        DirectInbound::Scrollback(answer) => {
            super::super::direct_history::answered(pump, answer);
        }
        frame => {
            let token = pump
                .inner
                .peer_attempts
                .borrow()
                .attempt(attempt_id)
                .and_then(|carrier| carrier.token().cloned());
            let Some(token) = token else {
                return;
            };
            // The generation is stamped here and nowhere else: the only thing
            // that knows which peer a frame came off is this drain.
            if let Some(frame) = frame.as_sync_frame(token.socket_generation) {
                pump.dispatch(ClientEvent::DirectFrameReceived { token, frame });
            }
        }
    }
}

/// The tuple a `Ready` claims, in the shape the admission rule reads.
fn ready_tuple(ready: &LoopbackReady) -> ReadyTuple {
    ReadyTuple {
        worker_fp: ready.worker_fingerprint.clone(),
        worker_epoch: ready.worker_epoch.clone(),
        peer_id: ready.peer_id.clone(),
        socket_generation: ready.socket_generation,
        socket_id: ready.socket_id.clone(),
        session_ids: ready.session_ids.clone(),
    }
}

/// Tell the machine this attempt is over because its peer died.
///
/// The close is the CORE's to emit: reporting the fault here produces its
/// `CloseAttempt`, and a host that closed as well would be a second teardown
/// whose two paths could disagree about which routes went.
pub(super) fn fault_attempt(pump: &Pump, attempt_id: u64, reason: &str) {
    let Some(worker_fp) = pump
        .inner
        .peer_attempts
        .borrow()
        .attempt(attempt_id)
        .map(|carrier| carrier.worker_fp().to_owned())
    else {
        return;
    };
    tracing::info!(
        target: "carriers",
        attempt_id,
        worker_fp,
        reason,
        "a peer carrier ended; the session falls back to the other transport"
    );
    refuse_attempt(pump, &worker_fp, Some(attempt_id), reason.to_owned());
}

/// Tell the machine its ICE is gone, which it records as its own fault.
pub(super) fn ice_failed(pump: &Pump, attempt_id: u64) {
    let Some(worker_fp) = pump
        .inner
        .peer_attempts
        .borrow()
        .attempt(attempt_id)
        .map(|carrier| carrier.worker_fp().to_owned())
    else {
        return;
    };
    pump.dispatch(ClientEvent::CarrierTransportObserved {
        worker_fp,
        observation: SignallingInput::IceFailed { attempt_id },
    });
}
