//! The typed terminal-peer control frames one exact worker generation receives:
//! the offer a negotiation sends after its waiter is installed, and the cancel
//! it sends when the waiter goes away. Serializes and fences only; SDP, ICE and
//! grant material never reach a log line here.
//! Ports `apps/coord/src/terminal/direct/worker-send-terminal-peer.ts`. Called by
//! `terminal_direct::peer_negotiations`.

use std::sync::Arc;

use roost_proto::{DLocalTerminalPeerCancel, DLocalTerminalPeerOffer};
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;

use crate::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use crate::workers::send::send_frame_through;

/// Every refusal reason a worker may answer an offer with; anything else is not
/// an answer to this protocol and settles nothing.
pub const TERMINAL_PEER_WORKER_ERROR_REASONS: [&str; 8] = [
    "disabled",
    "native_unavailable",
    "invalid_offer",
    "grant_unavailable",
    "capacity",
    "expired",
    "connection_superseded",
    "ice_failed",
];

/// One offer, as the negotiation owner reserved it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalPeerOfferSend {
    /// The coordinator's correlation id for the typed answer.
    pub request_id: String,
    /// The grant the offer is admitted under.
    pub grant_id: String,
    /// The browser's peer id.
    pub peer_id: String,
    /// The browser device.
    pub device_fingerprint: String,
    /// The browser document.
    pub tab_id: String,
    /// The worker process epoch the offer is fenced to.
    pub worker_epoch: String,
    /// The browser's SDP offer.
    pub offer_sdp: String,
    /// The worker's slice of the answer deadline.
    pub budget_ms: u32,
    /// The STUN servers the worker gathers against.
    pub stun_urls: Vec<String>,
}

/// One cancellation of a reserved offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalPeerCancelSend {
    /// The correlation id of the offer being withdrawn.
    pub request_id: String,
    /// The browser's peer id.
    pub peer_id: String,
    /// The worker process epoch the offer was fenced to.
    pub worker_epoch: String,
}

/// True only while this exact handle is the fingerprint's routable generation
/// on the expected process epoch.
#[must_use]
pub fn is_current_terminal_peer_worker(
    workers: &WorkerRegistry,
    worker: &Arc<WorkerHandle>,
    expected_epoch: &str,
) -> bool {
    worker.is_routable()
        && worker.process_epoch.as_deref() == Some(expected_epoch)
        && workers
            .current(&worker.worker_fp)
            .is_some_and(|current| Arc::ptr_eq(&current, worker))
}

/// Send one offer after its typed waiter is installed.
pub fn send_terminal_peer_offer(
    workers: &WorkerRegistry,
    worker: &Arc<WorkerHandle>,
    message: TerminalPeerOfferSend,
) -> bool {
    if !is_current_terminal_peer_worker(workers, worker, &message.worker_epoch) {
        return false;
    }
    let frame = CoordWorkerDownstream::LocalTerminalPeerOffer(DLocalTerminalPeerOffer {
        request_id: message.request_id,
        connection_generation: worker.connection_generation.clone(),
        worker_epoch: message.worker_epoch,
        grant_id: message.grant_id,
        peer_id: message.peer_id,
        device_fingerprint: message.device_fingerprint,
        tab_id: message.tab_id,
        offer_sdp: message.offer_sdp,
        budget_ms: message.budget_ms,
        stun_urls: message.stun_urls,
        ..Default::default()
    });
    send_frame_through(workers, worker, frame).is_admitted()
}

/// Withdraw an offer, only while its captured generation is still current.
pub fn send_terminal_peer_cancel(
    workers: &WorkerRegistry,
    worker: &Arc<WorkerHandle>,
    message: TerminalPeerCancelSend,
) -> bool {
    if !is_current_terminal_peer_worker(workers, worker, &message.worker_epoch) {
        return false;
    }
    let frame = CoordWorkerDownstream::LocalTerminalPeerCancel(DLocalTerminalPeerCancel {
        request_id: message.request_id,
        connection_generation: worker.connection_generation.clone(),
        worker_epoch: message.worker_epoch,
        peer_id: message.peer_id,
        ..Default::default()
    });
    send_frame_through(workers, worker, frame).is_admitted()
}

/// Whether a worker's refusal reason belongs to the protocol.
#[must_use]
pub fn is_terminal_peer_worker_error_reason(value: &str) -> bool {
    TERMINAL_PEER_WORKER_ERROR_REASONS.contains(&value)
}
