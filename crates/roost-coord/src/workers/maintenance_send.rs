//! The keeper-update maintenance command, written to the worker's current
//! routable generation with its pending-RPC correlation kept, so a lost
//! transport rejects this exact maintenance step instead of reading as a
//! completed update. Called by `deploy::keeper_update`; correlates through
//! `services.scrollback.pending()`. Ports `sendKeeperUpdatePreparation` of
//! apps/coord/src/workers/worker-send-maintenance.ts; its Windows half,
//! `sendWindowsUpdateBroker`, is not carried (the signed updater is paused).

use std::time::Duration;

use connectrpc::{ConnectError, ErrorCode};
use roost_protocol::keeper_update::validate_keeper_coordinator_open_session_ids;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use serde_json::Value;

use crate::terminal_screen::scrollback_relay::ScrollbackRelay;
use crate::workers::send::{SendOutcome, current_routable_worker, send_frame_through};

/// One keeper-update preparation, as the worker link carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperUpdatePreparation {
    /// The journaled envelope, absent exactly on the maintenance path.
    pub journaled_update_json: Option<String>,
    /// `source` or `target`, empty on the maintenance path.
    pub direction: String,
    pub maintenance: bool,
    pub force_live: bool,
    /// The coordinator's open-session proof the admission rests on.
    pub coordinator_open_session_ids: Vec<String>,
}

/// Hand `preparation` to the worker and await its proof, or the refusal.
///
/// The proof is re-validated here because it is this frame's contract: a
/// non-canonical id list must never reach a worker that decides a keeper's
/// fate by it. A frame the socket did not take rejects the correlation at
/// once rather than leaving the caller to its deadline.
pub async fn send_keeper_update_preparation(
    relay: &ScrollbackRelay,
    worker_fp: &WorkerFp,
    preparation: KeeperUpdatePreparation,
    deadline: Duration,
) -> Result<Value, ConnectError> {
    validate_keeper_coordinator_open_session_ids(
        "coordinator_open_session_ids",
        &preparation.coordinator_open_session_ids,
    )
    .map_err(|error| ConnectError::new(ErrorCode::Internal, error.to_string()))?;
    let handle = current_routable_worker(relay.workers(), worker_fp)
        .ok_or_else(|| ConnectError::new(ErrorCode::Unavailable, "worker offline"))?;
    let mut pending = relay
        .pending()
        .create_fresh(Some(worker_fp.as_str()), relay.now_ms())?;
    let frame = CoordWorkerDownstream::KeeperUpdatePrepare(roost_proto::DKeeperUpdatePrepare {
        request_id: pending.request_id().to_owned(),
        journaled_update_json: preparation.journaled_update_json,
        direction: preparation.direction,
        maintenance: preparation.maintenance,
        coordinator_open_session_ids: preparation.coordinator_open_session_ids,
        force_live: preparation.force_live,
        ..Default::default()
    });
    if let SendOutcome::Refused(refusal) = send_frame_through(relay.workers(), &handle, frame) {
        let message = refusal.to_string();
        relay.pending().reject_unavailable(
            pending.request_id(),
            &message,
            Some(worker_fp.as_str()),
        );
        tracing::warn!(%worker_fp, %refusal, "maintenance send: the keeper update was not sent");
        return Err(ConnectError::new(ErrorCode::Unavailable, message));
    }
    tracing::info!(%worker_fp, "a keeper update preparation reached the worker");
    match tokio::time::timeout(deadline, pending.settle()).await {
        Ok(reply) => reply,
        Err(_) => Err(ConnectError::new(
            ErrorCode::DeadlineExceeded,
            "the worker did not answer the keeper update preparation in time",
        )),
    }
}
