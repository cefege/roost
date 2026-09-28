//! The frames the downstream dispatch answers with itself: refusals, the
//! answers a worker without a later-wave owner gives, and the failure frames
//! for an owner that panicked. Called by `runtime::downstream` and its
//! `terminal` arms. Ports the literal frames of v2
//! `apps/worker/src/transport/coord-link-downstream.ts` (`sendImmediateInputResult`,
//! the stream-admission and stream-failure results, the unsupported answers)
//! and `coord-link-direct-terminal.ts` (`sendPeerError`,
//! `sendAttachmentPeerError`, `sendUnavailableAttachmentStatus`, `refusedClaim`).

use roost_proto::buffa::MessageField;
use roost_proto::{
    AttachmentTransferStatus, DAttachmentDirectStatusRequest, DLocalAttachmentPeerOffer,
    DLocalTerminalPeerOffer, DTerminalInputRouteClaim, DTerminalStreamState, DUpdateBroker,
    TerminalInputRouteResult, WAttachmentDirectStatus, WLocalAttachmentPeerError,
    WLocalTerminalPeerError, WTerminalInputRouteResult,
};
use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::coord_worker::{
    CoordWorkerUpstream, TerminalInputStatus, TerminalStreamFailureKind, TerminalStreamResult,
    TerminalStreamStatus, TerminalWritePhase,
};

use crate::uplink::terminal_results::InputResultKey;

pub(super) const INPUT_HANDLER_UNAVAILABLE: &str = "worker input handler is unavailable";
pub(super) const AGENT_PROMPT_HANDLER_UNAVAILABLE: &str =
    "worker agent prompt handler is unavailable";
pub(super) const STREAM_ADMISSION_FULL: &str = "worker terminal-stream admission is full";
pub(super) const LOCAL_TERMINAL_GRANTS_UNSUPPORTED: &str =
    "local terminal grants unsupported by this worker";
pub(super) const LOCAL_ATTACHMENT_GRANTS_UNSUPPORTED: &str =
    "local attachment grants unsupported by this worker";
pub(super) const KEEPER_UPDATE_PREPARE_UNSUPPORTED: &str =
    "keeper update preparation unsupported by this worker";
/// v2 `coord-link-deps.ts:381`: the broker is Windows-only.
pub(super) const UPDATE_BROKER_POSIX: &str =
    "Windows update broker command received on a POSIX worker";
/// v2 `TerminalPeerOfferFailureReason` / `AttachmentPeerOfferFailureReason`
/// for a worker with no peer owner.
const PEER_DISABLED: &str = "disabled";
/// v2 `refusedClaim`'s reason.
const ROUTE_CLAIM_BUSY: &str = "route_claim_busy";
/// v2 `sendUnavailableAttachmentStatus`'s error.
const UPLOAD_NOT_FOUND: &str = "upload_not_found";

pub(super) fn rpc_error(request_id: String, message: impl Into<String>) -> CoordWorkerUpstream {
    CoordWorkerUpstream::RpcError {
        request_id,
        message: message.into(),
        trace_id: None,
    }
}

/// An `input-result` the dispatcher answers without an owner (v2
/// `sendImmediateInputResult`). `None`, logged, when the coordinator's session
/// id is not one the wire record can carry.
pub(super) fn input_result(
    key: &InputResultKey,
    status: TerminalInputStatus,
    reason: &str,
) -> Option<CoordWorkerUpstream> {
    match key.to_result(status, 0, reason) {
        Ok(result) => Some(CoordWorkerUpstream::InputResult(result)),
        Err(error) => {
            tracing::error!(request_id = %key.request_id, %error, "an input-result could not be built");
            None
        }
    }
}

/// The stream request fields a result echoes, held past the await that
/// consumed the request.
#[derive(Debug, Clone)]
pub(super) struct StreamResultKey {
    request_id: String,
    session_id: String,
    stream_id: String,
    enabled: bool,
}

impl From<&DTerminalStreamState> for StreamResultKey {
    fn from(request: &DTerminalStreamState) -> Self {
        Self {
            request_id: request.request_id.clone(),
            session_id: request.session_id.clone(),
            stream_id: request.stream_id.clone(),
            enabled: request.enabled,
        }
    }
}

impl StreamResultKey {
    pub(super) fn request_id(&self) -> &str {
        &self.request_id
    }

    /// A `terminal-stream-result` that touched nothing: no resize sequence, no
    /// geometry, not resized.
    pub(super) fn untouched(
        &self,
        status: TerminalStreamStatus,
        phase: TerminalWritePhase,
        failure: TerminalStreamFailureKind,
        reason: &str,
    ) -> Option<CoordWorkerUpstream> {
        let session_id = match SessionId::try_from(self.session_id.as_str()) {
            Ok(session_id) => session_id,
            Err(error) => {
                tracing::error!(request_id = %self.request_id, %error, "a terminal-stream-result could not be built");
                return None;
            }
        };
        Some(CoordWorkerUpstream::TerminalStreamResult(
            TerminalStreamResult {
                request_id: self.request_id.clone(),
                session_id,
                stream_id: self.stream_id.clone(),
                enabled: self.enabled,
                status,
                channel_resize_seq: 0,
                effective_cols: 0,
                effective_rows: 0,
                resized: false,
                reason: reason.to_owned(),
                phase,
                failure_kind: Some(failure),
            },
        ))
    }
}

/// v2 `refusedClaim`: not accepted, no route epoch, this process's epoch.
pub(super) fn refused_claim(
    request: &DTerminalInputRouteClaim,
    worker_epoch: &str,
) -> CoordWorkerUpstream {
    route_result(
        request.request_id.clone(),
        TerminalInputRouteResult {
            request_id: request.request_id.clone(),
            session_id: request.session_id.clone(),
            revision: request.revision,
            accepted: false,
            latest_revision: 0,
            input_route_epoch: String::new(),
            worker_epoch: worker_epoch.to_owned(),
            reason: ROUTE_CLAIM_BUSY.to_owned(),
            ..Default::default()
        },
    )
}

pub(super) fn route_result(
    request_id: String,
    result: TerminalInputRouteResult,
) -> CoordWorkerUpstream {
    CoordWorkerUpstream::TerminalInputRouteResult(WTerminalInputRouteResult {
        request_id,
        result: MessageField::some(result),
        ..Default::default()
    })
}

pub(super) fn terminal_peer_disabled(
    request: &DLocalTerminalPeerOffer,
    worker_epoch: &str,
) -> CoordWorkerUpstream {
    CoordWorkerUpstream::LocalTerminalPeerError(WLocalTerminalPeerError {
        request_id: request.request_id.clone(),
        connection_generation: request.connection_generation.clone(),
        worker_epoch: worker_epoch.to_owned(),
        peer_id: request.peer_id.clone(),
        reason: PEER_DISABLED.to_owned(),
        ..Default::default()
    })
}

pub(super) fn attachment_peer_disabled(
    request: &DLocalAttachmentPeerOffer,
    worker_epoch: &str,
) -> CoordWorkerUpstream {
    CoordWorkerUpstream::LocalAttachmentPeerError(WLocalAttachmentPeerError {
        request_id: request.request_id.clone(),
        connection_generation: request.connection_generation.clone(),
        worker_epoch: worker_epoch.to_owned(),
        peer_id: request.peer_id.clone(),
        reason: PEER_DISABLED.to_owned(),
        ..Default::default()
    })
}

/// v2 `sendUnavailableAttachmentStatus`: an upload this worker has no record of.
pub(super) fn attachment_status_unavailable(
    request: &DAttachmentDirectStatusRequest,
) -> CoordWorkerUpstream {
    CoordWorkerUpstream::AttachmentDirectStatus(WAttachmentDirectStatus {
        request_id: request.request_id.clone(),
        status: MessageField::some(AttachmentTransferStatus {
            upload_id: request.upload_id.clone(),
            next_seq: 0,
            bytes_received: 0,
            last_chunk_sha256: String::new(),
            committed: false,
            abs_path: String::new(),
            error: UPLOAD_NOT_FOUND.to_owned(),
            ..Default::default()
        }),
        ..Default::default()
    })
}

/// The update broker on a POSIX worker, as v2's RUNNING worker answers it: an
/// action the broker does not define is refused as such
/// (`coord-link-downstream.ts:342-344`); a defined one reaches `onUpdateBroker`,
/// which throws on darwin/linux (`coord-link-deps.ts:379-381`), and the
/// `.catch` answers that message (`coord-link-downstream.ts:362-364`).
pub(super) fn update_broker_refusal(request: DUpdateBroker) -> CoordWorkerUpstream {
    if request.action != "START" && request.action != "STATUS" {
        let message = format!("unsupported updater action: {}", request.action);
        return rpc_error(request.request_id, message);
    }
    rpc_error(request.request_id, UPDATE_BROKER_POSIX)
}
