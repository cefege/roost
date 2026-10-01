//! Registering an authenticated peer carrier, and spending the credential on the
//! control lane that proves it.
//!
//! Owned by `pump::peer_lane`. These are the two ends of the handshake: the
//! `Hello` goes out when the browser reports its control lane open and the
//! carrier is registered when the worker's `Ready` comes back. Between them the
//! peer is open and proves nothing, which is why a `CellGrid` in that window is
//! a pre-hello frame and not a grid — `client::carriers::wire` owns that rule.
//!
//! THE `Hello` IS SPENT ONCE AND ON THE LIVE GRANT. The secret is read at the
//! moment it is written (`CarrierLane::live_grant`), because an attempt is traced
//! and logged and a secret that reached any of those is a secret that outlives
//! its grant. A grant that has since been re-minted is not this attempt's
//! credential, and the attempt is refused rather than spent on the new one.

use roost_client_core::ClientEvent;
use roost_client_core::TerminalTransport;
use roost_client_core::client::carriers::{PeerLane, ReadyTuple, wire::encode_hello};

use super::Pump;
use super::open::refuse_attempt;
use super::write::{LaneRefusal, write_control};
use crate::platform::carrier::CarrierIdentity;

/// Register the carrier this attempt's `Ready` earned, and announce it.
///
/// The connection id is MINTED here rather than reused from anything the attempt
/// carried, because the id names ONE connection: a retirement that named an id a
/// reconnect re-minted would take down the connection that replaced it.
pub(super) fn stage_carrier(pump: &Pump, attempt_id: u64, ready: &ReadyTuple) {
    let held = pump.inner.peer_attempts.borrow();
    let Some(attempt) = held.attempt(attempt_id) else {
        tracing::warn!(
            target: "carriers",
            attempt_id,
            "the machine staged a carrier for an attempt this document does not hold"
        );
        return;
    };
    let worker_fp = attempt.worker_fp().to_owned();
    let connection_id =
        CarrierIdentity::mint(worker_fp.clone(), TerminalTransport::Peer).connection_id;
    let admitted =
        pump.inner
            .peer_attempts
            .borrow_mut()
            .authenticate(attempt_id, ready, connection_id);
    let Some((carrier, displaced)) = admitted else {
        tracing::warn!(
            target: "carriers",
            attempt_id,
            worker_fp,
            "an admitted Ready did not admit against this document's attempt"
        );
        refuse_attempt(
            pump,
            &worker_fp,
            Some(attempt_id),
            "identity_mismatch".to_owned(),
        );
        return;
    };
    if let Some(stale) = displaced {
        tracing::warn!(
            target: "carriers",
            attempt_id,
            displaced = stale.attempt_id(),
            worker_fp = stale.worker_fp(),
            "a peer carrier displaced the one this worker presented before"
        );
        super::close(pump, stale.attempt_id(), "peer carrier displaced");
    }
    tracing::info!(
        target: "carriers",
        attempt_id,
        connection_id = carrier.connection_id,
        worker_fp,
        peer_id = ready.peer_id,
        socket_id = ready.socket_id,
        socket_generation = ready.socket_generation,
        sessions = carrier.granted_sessions.len(),
        "peer carrier authenticated and staged"
    );
    pump.dispatch(ClientEvent::CarrierReady(carrier));
}

/// The browser reported one lane open. Spend the credential on the control lane,
/// and flush anything else that turn.
///
/// Only the control lane opens a negotiation: the credential travels on the lane
/// the protocol numbers for control, and a peer that received it on the data lane
/// would be reading a stream the worker never watches for one.
pub(super) fn lane_opened(pump: &Pump, attempt_id: u64, lane: PeerLane) {
    pump.inner
        .peer
        .borrow_mut()
        .mark_lane_open(attempt_id, lane);
    if lane != PeerLane::Control {
        super::write::flush(pump, attempt_id);
        return;
    }
    spend_hello(pump, attempt_id);
}

/// Write this attempt's `Hello`, once.
///
/// A control lane that reports open twice does not get a second `Hello`: the
/// handshake is spent once, and a worker that saw two would answer one attempt
/// twice.
fn spend_hello(pump: &Pump, attempt_id: u64) {
    let now_ms = pump.inner.core.borrow().clock().now_ms();
    let attempt = {
        let held = pump.inner.peer_attempts.borrow();
        let Some(carrier) = held.attempt(attempt_id) else {
            return;
        };
        carrier.attempt().clone()
    };
    let worker_fp = attempt.worker_fp.clone();
    let fresh = pump
        .inner
        .peer_attempts
        .borrow_mut()
        .attempt_mut(attempt_id)
        .is_some_and(|carrier| carrier.life_mut().hello_sent(now_ms));
    if !fresh {
        tracing::debug!(
            target: "carriers",
            attempt_id,
            worker_fp,
            "the control lane reopened; the handshake is already spent"
        );
        return;
    }
    let granted = pump
        .inner
        .core
        .borrow()
        .store()
        .direct
        .live_grant(&worker_fp, now_ms)
        .cloned();
    let Some(grant) = granted else {
        tracing::warn!(
            target: "carriers",
            attempt_id,
            worker_fp,
            "the live grant was gone before the Hello could be spent"
        );
        refuse_attempt(
            pump,
            &worker_fp,
            Some(attempt_id),
            "grant_unavailable".to_owned(),
        );
        return;
    };
    if grant.grant_id != attempt.grant_id {
        tracing::warn!(
            target: "carriers",
            attempt_id,
            worker_fp,
            "the grant was re-minted; this attempt's credential is dead"
        );
        refuse_attempt(
            pump,
            &worker_fp,
            Some(attempt_id),
            "grant_unavailable".to_owned(),
        );
        return;
    }
    let hello = encode_hello(
        &grant.grant_id,
        &grant.secret,
        &grant.tab_id,
        &grant.device_fingerprint,
        &attempt.peer_id,
        &attempt.worker_epoch,
    );
    match write_control(pump, attempt_id, hello) {
        Ok(true) => tracing::info!(
            target: "carriers",
            attempt_id,
            worker_fp,
            peer_id = attempt.peer_id,
            "peer control lane open; the credential is spent on its Hello"
        ),
        Ok(false) => tracing::warn!(
            target: "carriers",
            attempt_id,
            worker_fp,
            "the peer control lane refused the Hello; the queue was full"
        ),
        Err(refusal) => report_write_refusal(attempt_id, &worker_fp, refusal),
    }
}

/// Why a peer write did not go out, named once so every call site reads the same.
pub(super) fn report_write_refusal(attempt_id: u64, worker_fp: &str, refusal: LaneRefusal) {
    tracing::warn!(
        target: "carriers",
        attempt_id,
        worker_fp,
        refusal = ?refusal,
        "a peer carrier write was refused rather than dropped"
    );
}
