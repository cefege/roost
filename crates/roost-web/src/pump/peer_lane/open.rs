//! Opening one attempt's transport, and reading the offer its browser gathered.
//!
//! Owned by `pump::peer_lane`. It is the host half of the negotiation's first
//! two steps: the core names the attempt and the credential it may spend, and
//! this opens the browser's peer, starts its gathering, and hands the offer the
//! coordinator will relay to a worker.
//!
//! THE OFFER IS READ ONCE, AND FILTERED. `local_offer` drops the browser's
//! ICE-TCP candidates before the bytes exist here, so the offer the coordinator
//! relays to the worker is UDP-only (`protocol/spec/direct-terminal.md`). The
//! read happens on whichever comes first — the browser reporting gathering
//! complete, or the protocol's gathering bound — and the core's own rule on an
//! offer with no usable candidate is what refuses an empty one.

use roost_client_core::ClientEvent;
use roost_client_core::client::carriers::{
    PeerAttempt, PeerTransport, SignallingInput, TransportError,
};

use super::Pump;
use crate::platform::carriers::PeerCarrier;

/// Open the transport the core named, and record what it opened.
///
/// The peer id is minted HERE: the core holds no entropy, and the id is the
/// transport's own name for this negotiation (v2 mints it as it constructs the
/// connection). The core adopts it from the offer report.
pub(super) fn open_transport(pump: &Pump, mut attempt: PeerAttempt) {
    let attempt_id = attempt.attempt_id;
    let worker_fp = attempt.worker_fp.clone();
    let now_ms = pump.inner.core.borrow().clock().now_ms();
    let Some(peer_id) = crate::platform::terminal_view_id::mint_peer_id() else {
        tracing::warn!(
            target: "carriers",
            attempt_id,
            worker_fp,
            "this document cannot mint a peer id, so it cannot open a peer"
        );
        refuse_attempt(
            pump,
            &worker_fp,
            Some(attempt_id),
            "native_unavailable".to_owned(),
        );
        return;
    };
    attempt.peer_id = peer_id;
    if let Err(error) = pump.inner.peer.borrow_mut().open(&attempt) {
        tracing::warn!(
            target: "carriers",
            attempt_id,
            worker_fp,
            detail = %error,
            "the browser refused to open this peer transport"
        );
        refuse_attempt(pump, &worker_fp, Some(attempt_id), refusal_reason(&error));
        return;
    }
    register(pump, PeerCarrier::opened(attempt, now_ms));
    pump.declare_carrier_environment();
}

/// Record a newly opened attempt, and close whatever it displaced.
///
/// Displacement is reported rather than ignored: an attempt id is never reused by
/// the core, so a displaced record can only be a peer this document opened and
/// then lost track of, and its lanes would otherwise keep accepting bytes for an
/// attempt nothing holds.
pub(super) fn register(pump: &Pump, carrier: PeerCarrier) {
    let attempt_id = carrier.attempt_id();
    let displaced = pump.inner.peer_attempts.borrow_mut().open(carrier);
    let Some(displaced) = displaced else {
        return;
    };
    tracing::warn!(
        target: "carriers",
        attempt_id,
        displaced = displaced.attempt_id(),
        worker_fp = displaced.worker_fp(),
        "an opened peer attempt displaced one this document already held"
    );
    super::close(pump, displaced.attempt_id(), "peer attempt displaced");
}

/// Read this attempt's filtered offer and hand it to the machine.
///
/// The outcome is a `ClientEvent` in every case, because an offer this host
/// cannot read is the same fact to the machine as an offer with no usable
/// candidate, and the machine owns which of the two it is.
pub(super) fn offer_ready(pump: &Pump, attempt_id: u64) {
    let Some(attempt) = pump.inner.peer.borrow().attempt_of(attempt_id) else {
        tracing::warn!(
            target: "carriers",
            attempt_id,
            "the offer named an attempt this document no longer holds"
        );
        return;
    };
    let worker_fp = attempt.worker_fp.clone();
    let owed = pump
        .inner
        .peer_attempts
        .borrow_mut()
        .attempt_mut(attempt_id)
        .is_some_and(|carrier| {
            let owed = carrier.life().gathering_pending();
            carrier.life_mut().gathering_settled();
            owed
        });
    if !owed {
        return;
    }
    match pump.inner.peer.borrow().local_offer(attempt_id) {
        Ok(offer_sdp) => {
            pump.dispatch(ClientEvent::CarrierTransportObserved {
                worker_fp,
                observation: SignallingInput::OfferReady {
                    attempt_id,
                    peer_id: attempt.peer_id,
                    offer_sdp,
                },
            });
        }
        Err(error) => refuse_attempt(pump, &worker_fp, Some(attempt_id), refusal_reason(&error)),
    }
}

/// The protocol reason a transport refusal is reported under.
///
/// The eight codes the protocol fixes name what the WORKER said, and a browser's
/// own refusal is not one of them: `native_unavailable` is the closest, because
/// the case that matters is a stack that never produced a description at all.
/// Everything else is reported verbatim so the line carries the browser's words.
fn refusal_reason(error: &TransportError) -> String {
    match error {
        TransportError::Unavailable { .. } => "native_unavailable".to_owned(),
        other => other.to_string(),
    }
}

/// Tell the machine this attempt is over, and why.
///
/// `None` names the COORDINATOR case: an offer went out and no answer came back,
/// which is a different repair from a worker that refused one.
pub(super) fn refuse_attempt(
    pump: &Pump,
    worker_fp: &str,
    attempt_id: Option<u64>,
    reason: String,
) {
    pump.dispatch(ClientEvent::CarrierTransportObserved {
        worker_fp: worker_fp.to_owned(),
        observation: SignallingInput::AttemptRefused { attempt_id, reason },
    });
}
