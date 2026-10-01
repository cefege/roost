//! Handing the coordinator this attempt's offer, and taking back the answer.
//!
//! Owned by `pump::peer_lane`. The coordinator is the SIGNALLING AUTHORITY: an
//! offer never goes to another peer, so this is the one round trip in the
//! negotiation and the one place a `RefCell` of the pump must not be held across
//! an `await` — the answer arrives on a task, and a borrow taken before the call
//! and released after it is a re-entrant panic the first time a data channel
//! fires in between.
//!
//! NOTHING IS HELD ACROSS THE CALL. The attempt is read into owned values, the
//! pump is cloned, and the answer is DISPATCHED rather than returned: a stale
//! answer is fenced by the machine, which refuses any answer naming another
//! attempt, and a host that dropped it before dispatching would be a second
//! place that could refuse it.

use roost_client_core::ClientEvent;
use roost_client_core::client::carriers::PeerTransport;
use roost_client_core::client::carriers::SignallingInput;
use roost_client_core::client::carriers::grant_rpc::NegotiateLocalTerminalPeer;

use super::Pump;
use super::open::refuse_attempt;

/// Send the offer to the coordinator, and report the answer it sends back.
pub(super) fn negotiate_offer(pump: &Pump, attempt_id: u64, offer_sdp: String) {
    let Some(attempt) = pump.inner.peer.borrow().attempt_of(attempt_id) else {
        tracing::warn!(
            target: "carriers",
            attempt_id,
            "the machine asked to negotiate an attempt this document does not hold"
        );
        return;
    };
    let worker_fp = attempt.worker_fp.clone();
    let request = NegotiateLocalTerminalPeer {
        worker_fp: attempt.worker_fp.clone(),
        grant_id: attempt.grant_id.clone(),
        tab_id: attempt.tab_id.clone(),
        peer_id: attempt.peer_id.clone(),
        offer_sdp,
        worker_epoch: attempt.worker_epoch.clone(),
    };
    let opening = pump.clone();
    let reporting = pump.clone();
    wasm_bindgen_futures::spawn_local(async move {
        match opening.rpc().call(&request).await {
            Ok(answer) => {
                tracing::info!(
                    target: "carriers",
                    attempt_id,
                    worker_fp,
                    answer_bytes = answer.answer_sdp.len(),
                    "the coordinator answered this attempt's offer"
                );
                reporting.dispatch(ClientEvent::CarrierTransportObserved {
                    worker_fp,
                    observation: SignallingInput::AnswerReceived { attempt_id, answer },
                });
            }
            Err(error) => {
                tracing::warn!(
                    target: "carriers",
                    attempt_id,
                    worker_fp,
                    detail = %error,
                    "the coordinator did not answer this attempt's offer"
                );
                // No attempt is named: the offer went out and nothing answered it,
                // which is the coordinator's failure rather than a worker's.
                refuse_attempt(&reporting, &worker_fp, None, error.to_string());
            }
        }
    });
}

/// Hand the coordinator's answer to the browser.
///
/// A refusal here is the far end's answer failing to become a connection, and it
/// is reported under the attempt because the machine can then record which
/// negotiation it was rather than that something, somewhere, went wrong.
pub(super) fn apply_answer(pump: &Pump, attempt_id: u64, answer_sdp: &str) {
    let Some(attempt) = pump.inner.peer.borrow().attempt_of(attempt_id) else {
        tracing::warn!(
            target: "carriers",
            attempt_id,
            "the machine applied an answer to an attempt this document does not hold"
        );
        return;
    };
    let worker_fp = attempt.worker_fp.clone();
    if let Err(error) = pump
        .inner
        .peer
        .borrow_mut()
        .accept_answer(attempt_id, answer_sdp)
    {
        tracing::warn!(
            target: "carriers",
            attempt_id,
            worker_fp,
            detail = %error,
            "the browser refused the coordinator's answer"
        );
        refuse_attempt(pump, &worker_fp, Some(attempt_id), error.to_string());
    }
}
