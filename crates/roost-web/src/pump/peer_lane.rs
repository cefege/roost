//! Performing one peer-lifecycle action the direct-carrier state machine
//! decided on.
//!
//! Owned by `pump`, driven by `pump::effects` for the one `Effect::Carrier`
//! arm. It is the HOST half of `roost_client_core::client::carriers`: the core
//! owns WHEN an attempt opens, hands over, authenticates, stages and closes;
//! this owns what the browser's WebRTC stack does about it, and reports what it
//! saw back as `ClientEvent`s.
//!
//! THE ONE RULE THIS FILE EXISTS TO KEEP: an action this document performs is
//! REPORTED, never dropped. A dropped `OpenTransport` is indistinguishable from
//! a browser that gathered candidates forever, so the session stays on Sync with
//! nothing anywhere to say why — the same silence `pump::carriers` refuses for a
//! command.
//!
//! WHICH IS WHY `perform` HAS NO CATCH-ALL. Every arm of `CarrierEffect` is
//! handled here, and a new arm is a compile error rather than a `tracing::warn!`
//! nobody reads: the core owns this vocabulary and the host's job is to speak it,
//! not to decline it. The arms are spread across the siblings below — `open`,
//! `negotiate`, `stage`, `drain` — and this file is the dispatch and the two
//! endings every path shares, which are a close and a retry.
mod deadlines;
mod drain;
mod liveness;
mod negotiate;
mod open;
mod stage;
mod write;

use std::rc::Rc;

use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;

use roost_client_core::ClientEvent;
use roost_client_core::TerminalToken;
use roost_client_core::client::carriers::{CarrierEffect, PeerTransport, SignallingInput};

pub(super) use drain::install_tick;

use super::Pump;

/// How often the peer tick drains the browser's event sink and reads the
/// protocol's own deadlines.
///
/// The sweep's cadence rather than a faster one of its own: nothing here can end
/// an attempt sooner than the slowest window the protocol fixes, and a second
/// interval at a different rate is a second number to keep honest.
pub(super) const PEER_TICK_INTERVAL_MS: i32 = 250;

/// Queue and write one already-decided command on the peer carrier presenting
/// `token`.
///
/// Re-exported as a `String` refusal because the call site lives in
/// `pump::carriers`, which REPORTS a refusal rather than judging it: what the
/// lane said is its own log line, and the route only learns that nothing
/// presented the generation.
pub(super) fn write_direct(
    pump: &Pump,
    token: &TerminalToken,
    bytes: Vec<u8>,
) -> Result<(), String> {
    write::write_direct(pump, token, bytes).map_err(|refusal| refusal.to_string())
}

/// Perform one peer-lifecycle action.
pub(super) fn perform(pump: &Pump, action: CarrierEffect) {
    match action {
        CarrierEffect::Core(effect) => super::effects::perform(pump, effect),
        CarrierEffect::OpenTransport { attempt } => open::open_transport(pump, attempt),
        CarrierEffect::NegotiateOffer {
            attempt_id,
            offer_sdp,
        } => negotiate::negotiate_offer(pump, attempt_id, offer_sdp),
        CarrierEffect::ApplyAnswer {
            attempt_id,
            answer_sdp,
        } => negotiate::apply_answer(pump, attempt_id, &answer_sdp),
        CarrierEffect::StageCarrier { attempt_id, ready } => {
            stage::stage_carrier(pump, attempt_id, &ready);
        }
        CarrierEffect::CloseAttempt { attempt_id, reason } => close(pump, attempt_id, &reason),
        CarrierEffect::RetryAt { at_ms } => retry_at(pump, at_ms),
        CarrierEffect::Faulted {
            worker_fp,
            fault,
            detail,
        } => {
            // `warn`, not `error`: a fault that hands the session to Sync is a
            // working state, and the core has already recorded the fault on the
            // machine. What the HOST adds is the fact that its own transport
            // was not involved in the rule that fired.
            tracing::warn!(
                target: "carriers",
                worker_fp,
                fault = fault.as_str(),
                detail,
                "direct carrier faulted; the session falls back to the other transport"
            );
        }
        CarrierEffect::Fallback {
            session_id,
            transport,
        } => {
            tracing::info!(
                target: "carriers",
                session_id,
                transport = ?transport,
                "the elected direct route is gone; the named transport takes the session"
            );
        }
    }
}

/// The peer and every channel on it go, and the document's peer count drops
/// with them.
///
/// The count is re-declared to the core afterwards rather than left for the next
/// `RetryAt`: the cap in `Signalling::start` reads it, and a count that only
/// refreshes on a retry is a count that lets the document allocate past its own
/// cap on the way to the retry.
///
/// The lanes and the announced connection go with it, which is what makes a byte
/// from a retired attempt un-routable rather than merely late: a frame that
/// arrives afterwards finds no attempt and is discarded, and the routes the
/// connection was serving are retired under the id it announced.
pub(super) fn close(pump: &Pump, attempt_id: u64, reason: &str) {
    let announced = pump
        .inner
        .peer_attempts
        .borrow_mut()
        .retire_attempt(attempt_id)
        .and_then(|carrier| carrier.connection_id().map(str::to_owned));
    let remaining = {
        let mut peer = pump.inner.peer.borrow_mut();
        peer.close(attempt_id, reason);
        peer.open_count() as u32
    };
    tracing::info!(
        target: "carriers",
        attempt_id,
        reason,
        peers_held = remaining,
        "peer carrier closed"
    );
    declare_environment(pump);
    if let Some(connection_id) = announced {
        pump.dispatch(ClientEvent::CarrierLost { connection_id });
        super::direct_history::lose_reads_off_route(pump, reason);
    }
}

/// Come back at the instant the machine asked for, and report it as the input
/// it named.
///
/// The delay is computed against the host's clock rather than used as a
/// duration, because `RetryAt` carries an INSTANT: a delay that is right when it
/// is scheduled is wrong by however long the closure below it took.
fn retry_at(pump: &Pump, at_ms: u64) {
    schedule(pump, at_ms, deliver_retry_due(at_ms));
}

/// The one thing a scheduled carrier wake-up reports.
fn deliver_retry_due(at_ms: u64) -> impl Fn(&Pump, String) + 'static {
    move |pump, worker_fp| {
        pump.dispatch(ClientEvent::CarrierTransportObserved {
            worker_fp,
            observation: SignallingInput::RetryDue { now_ms: at_ms },
        });
    }
}

/// Arm one retry for every machine the lane holds, at the same instant.
///
/// Per machine rather than once for the lane: `SignallingInput::RetryDue` names
/// one worker, and a single wake-up that fed it to the first machine would leave
/// the other worker's credential waiting out a retry that was never reported.
fn schedule<F>(pump: &Pump, at_ms: u64, deliver: F)
where
    F: Fn(&Pump, String) + 'static,
{
    let workers: Vec<String> = pump
        .inner
        .core
        .borrow()
        .store()
        .direct
        .workers()
        .map(str::to_owned)
        .collect();
    let Some(window) = web_sys::window() else {
        tracing::warn!(target: "carriers", "no browser window; the retry was not scheduled");
        return;
    };
    let now_ms = pump.inner.core.borrow().clock().now_ms();
    let delay = i32::try_from(at_ms.saturating_sub(now_ms)).unwrap_or(i32::MAX);
    // One shared handle and one self-freeing closure per worker. A
    // `Closure::once` dropped at the end of this loop is freed before the timer
    // fires, and the browser then throws "closure invoked after being dropped"
    // instead of delivering the retry — so every fault's cooldown never ended.
    let deliver = Rc::new(deliver);
    for worker_fp in workers {
        let reported = worker_fp.clone();
        let timer = Closure::once_into_js({
            let pump = pump.clone();
            let deliver = Rc::clone(&deliver);
            move || deliver(&pump, reported)
        });
        if window
            .set_timeout_with_callback_and_timeout_and_arguments_0(timer.unchecked_ref(), delay)
            .is_err()
        {
            tracing::warn!(
                target: "carriers",
                worker_fp,
                at_ms,
                "the browser refused the carrier retry timer"
            );
        }
    }
}

/// Re-declare what this document can currently do, after its peers moved.
///
/// The same declaration `Pump::new` makes, because it is the same fact: one
/// place that knows how to state it means the availability half cannot drift
/// between the boot call and the close call.
fn declare_environment(pump: &Pump) {
    pump.declare_carrier_environment();
}
