//! The coordinator link's terminal-stream port: [`StreamOwner`] maps a
//! `terminalStreamState` frame onto `SessionManager::apply_terminal_stream_state`
//! (`session::terminal_control`) and its outcome onto the wire result. Built by
//! `runtime::owners`; called by `runtime::downstream`. Ports
//! `onTerminalStreamState` of `apps/worker/src/transport/coord-link-deps.ts:250-287`.

use std::sync::Arc;
use std::time::Instant;

use roost_proto::DTerminalStreamState;
use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::coord_worker::{
    TerminalSnapshotRequest, TerminalStreamResult, TerminalStreamStatus,
};

use super::lifecycle::SessionManager;
use super::terminal_state::{StreamIntent, StreamRequestBudget, WorkerStreamResult};
use crate::link_ports::TerminalStreamPort;
use crate::uplink::terminal_results::bounded_terminal_reason;
use crate::uplink::{LinkFence, OwnerFuture, RequestBudget};

/// The coordinator link's stream owner: the `TerminalStreamPort` over one
/// `SessionManager`. Built once by the composition root.
#[derive(Debug, Clone)]
pub struct StreamOwner {
    manager: Arc<SessionManager>,
}

impl StreamOwner {
    pub fn new(manager: Arc<SessionManager>) -> Self {
        Self { manager }
    }
}

impl TerminalStreamPort for StreamOwner {
    fn apply_stream_state(
        &self,
        request: DTerminalStreamState,
        budget: RequestBudget,
        fence: LinkFence,
    ) -> OwnerFuture<Option<TerminalStreamResult>> {
        let Ok(session_id) = SessionId::try_from(request.session_id.as_str()) else {
            tracing::warn!(request_id = %request.request_id, "a terminal-stream request named a session id no result can carry");
            return Box::pin(std::future::ready(None));
        };
        let request_id = request.request_id.clone();
        let intent = StreamIntent {
            request_id: request.request_id,
            session_id: session_id.clone(),
            stream_id: request.stream_id,
            enabled: request.enabled,
            cols: request.cols,
            rows: request.rows,
            budget: Some(Arc::new(LinkBudget { budget, fence })),
        };
        let operation = self.manager.apply_terminal_stream_state(intent);
        Box::pin(async move { Some(wire_result(request_id, session_id, operation.await)) })
    }

    fn request_snapshot(&self, request: TerminalSnapshotRequest) {
        self.manager
            .request_terminal_snapshot(&request.session_id, &request.stream_id);
    }
}

/// v2's coordinator budget: the frame-receipt deadline and the link fence.
#[derive(Debug)]
struct LinkBudget {
    budget: RequestBudget,
    fence: LinkFence,
}

impl StreamRequestBudget for LinkBudget {
    fn is_current_connection(&self) -> bool {
        self.fence.is_current()
    }

    fn expired(&self) -> bool {
        self.budget.expired(Instant::now())
    }
}

/// `terminal-stream-result` exactly as `coord-link-deps.ts:259-285` builds it.
fn wire_result(
    request_id: String,
    session_id: SessionId,
    result: WorkerStreamResult,
) -> TerminalStreamResult {
    let phase = result.phase();
    let channel_resize_seq = result.channel_resize_seq();
    let (status, stream_id, enabled, cols, rows, resized, failure, reason) = match result {
        WorkerStreamResult::Committed {
            stream_id,
            enabled,
            cols,
            rows,
            resized,
            ..
        } => (
            TerminalStreamStatus::Committed,
            stream_id,
            enabled,
            cols,
            rows,
            resized,
            None,
            None,
        ),
        WorkerStreamResult::Rejected {
            stream_id,
            enabled,
            cols,
            rows,
            failure,
            reason,
            ..
        } => (
            TerminalStreamStatus::Rejected,
            stream_id,
            enabled,
            cols,
            rows,
            false,
            Some(failure),
            Some(reason),
        ),
        WorkerStreamResult::Ambiguous {
            stream_id,
            enabled,
            cols,
            rows,
            failure,
            reason,
            ..
        } => (
            TerminalStreamStatus::Ambiguous,
            stream_id,
            enabled,
            cols,
            rows,
            false,
            Some(failure),
            Some(reason),
        ),
    };
    TerminalStreamResult {
        request_id,
        session_id,
        stream_id,
        enabled,
        status,
        channel_resize_seq,
        effective_cols: cols,
        effective_rows: rows,
        resized,
        reason: bounded_terminal_reason(reason.as_deref()),
        phase,
        failure_kind: failure,
    }
}
