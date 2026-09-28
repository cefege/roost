//! The terminal-control senders: one input batch, one fenced agent prompt and
//! one snapshot repair, each written to the worker's current routable generation.
//! Ports `sendTerminalInputRequest`, `sendAgentPromptRequest` and
//! `sendTerminalSnapshotRequest` of `apps/coord/src/workers/worker-send.ts:174-379`.
//! `sendTerminalStreamStateRequest` is not ported: its only v2 caller,
//! `terminal-view-stream-controller.ts`, is dropped by the `terminal_view/mod.rs`
//! ruling. Called by the terminal input lane, `agents::prompt_control` and the
//! view hub's repair; correlates through `services.scrollback.pending()`.

use roost_proto::{DAgentPrompt, DInputRequest};
use roost_protocol::wire::coord_worker::{
    CoordWorkerDownstream, InputResult, TerminalSnapshotRequest,
};
use roost_protocol::wire::{SessionId, WorkerFp};

use crate::coord_core::worker_handle::WorkerRegistry;
use crate::terminal_screen::scrollback_relay::ScrollbackRelay;
use crate::terminal_screen::typed_results::TypedResult;
use crate::workers::hop_deadline::{HopDeadline, worker_budget_ms};
use crate::workers::send::{SendOutcome, current_routable_worker, send_frame, send_frame_through};
use crate::workers::terminal_request::TerminalWorkerRequest;

/// One terminal-input batch and the browser identity that wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalInputSend {
    /// The session whose PTY the bytes go to.
    pub session_id: SessionId,
    /// The browser's per-session input sequence.
    pub input_seq: u64,
    /// The bytes, owned: a queued batch cannot change under its caller.
    pub data: Vec<u8>,
    /// The writing device.
    pub device_fingerprint: String,
    /// The writing tab.
    pub tab_id: String,
    /// The Sync connection the write arrived on.
    pub browser_connection_id: String,
    /// The input-route epoch the write was admitted under.
    pub input_route_epoch: String,
}

/// Send one terminal-input batch and correlate the keeper-completed result.
///
/// The result settles only from `InputResult`, after the keeper wrote all the
/// bytes, refused before writing, or reported a partial or unknown write.
#[must_use]
pub fn send_terminal_input_request(
    relay: &ScrollbackRelay,
    worker_fp: &WorkerFp,
    message: TerminalInputSend,
    deadline: HopDeadline,
) -> TerminalWorkerRequest<InputResult> {
    let wording = RefusalWording {
        expired: "terminal input budget expired before send",
        dropped: "worker transport dropped terminal input",
    };
    send_typed(
        relay,
        worker_fp,
        deadline,
        &wording,
        |request_id, budget_ms| {
            CoordWorkerDownstream::InputRequest(DInputRequest {
                request_id,
                session_id: message.session_id.as_str().to_owned(),
                input_seq: message.input_seq,
                data: message.data,
                budget_ms,
                device_fingerprint: message.device_fingerprint,
                tab_id: message.tab_id,
                input_route_epoch: message.input_route_epoch,
                browser_connection_id: message.browser_connection_id,
                ..Default::default()
            })
        },
    )
}

/// One status-fenced agent prompt. The worker, not the coordinator, checks the
/// fence against its live occupant and builds the PTY bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPromptSend {
    /// The session whose agent is prompted.
    pub session_id: SessionId,
    /// The coordinator-minted write sequence.
    pub input_seq: u64,
    /// The status epoch the caller read.
    pub expected_status_epoch: String,
    /// The occupant the caller read.
    pub expected_occupant_id: String,
    /// The status revision the caller read.
    pub expected_revision: u64,
    /// The prompt text, without its submit key.
    pub text: String,
}

/// Send one fenced agent prompt and correlate the keeper-completed result.
#[must_use]
pub fn send_agent_prompt_request(
    relay: &ScrollbackRelay,
    worker_fp: &WorkerFp,
    message: AgentPromptSend,
    deadline: HopDeadline,
) -> TerminalWorkerRequest<InputResult> {
    let wording = RefusalWording {
        expired: "agent prompt budget expired before send",
        dropped: "worker transport dropped agent prompt",
    };
    send_typed(relay, worker_fp, deadline, &wording, |request_id, budget_ms| {
        CoordWorkerDownstream::AgentPrompt(DAgentPrompt {
            request_id,
            session_id: message.session_id.as_str().to_owned(),
            input_seq: message.input_seq,
            expected_status_epoch: message.expected_status_epoch,
            expected_occupant_id: message.expected_occupant_id,
            expected_revision: message.expected_revision,
            text: message.text,
            budget_ms,
            ..Default::default()
        })
    })
}

/// Ask for a full baseline of the currently expected stream. Fire and forget:
/// repeating it replaces any partial same-stream cursor, so it correlates
/// nothing.
pub fn send_terminal_snapshot_request(
    registry: &WorkerRegistry,
    worker_fp: &WorkerFp,
    session_id: &SessionId,
    stream_id: &str,
) -> SendOutcome {
    let frame = CoordWorkerDownstream::TerminalSnapshotRequest(TerminalSnapshotRequest {
        session_id: session_id.clone(),
        stream_id: stream_id.to_owned(),
    });
    let outcome = send_frame(registry, worker_fp, frame);
    if let SendOutcome::Refused(refusal) = &outcome {
        tracing::debug!(%worker_fp, %session_id, stream_id, %refusal,
            "terminal send: a snapshot request was not sent");
    }
    outcome
}

/// v2's per-frame refusal wording, verbatim, because the input lane matches on
/// the expiry text to answer the browser.
struct RefusalWording {
    expired: &'static str,
    dropped: &'static str,
}

/// The shared order every typed sender keeps: route, budget, correlate, write.
///
/// The budget is checked BEFORE the entry opens and the entry opens BEFORE the
/// write, so a refusal at either of the first two leaves nothing to clean up
/// and a worker reply can never beat its own correlation entry.
fn send_typed<T: TypedResult>(
    relay: &ScrollbackRelay,
    worker_fp: &WorkerFp,
    deadline: HopDeadline,
    wording: &RefusalWording,
    frame_for: impl FnOnce(String, u32) -> CoordWorkerDownstream,
) -> TerminalWorkerRequest<T> {
    let Some(handle) = current_routable_worker(relay.workers(), worker_fp) else {
        tracing::debug!(%worker_fp, kind = T::KIND, "terminal send: worker offline");
        return TerminalWorkerRequest::unsent("worker offline", false);
    };
    let Some(budget_ms) = worker_budget_ms(&deadline) else {
        tracing::info!(%worker_fp, kind = T::KIND, "terminal send: the hop budget expired before send");
        return TerminalWorkerRequest::unsent(wording.expired, true);
    };
    let pending = match relay
        .pending()
        .create_fresh(Some(worker_fp.as_str()), relay.now_ms())
    {
        Ok(pending) => pending,
        Err(error) => return TerminalWorkerRequest::uncorrelated(error),
    };
    let frame = frame_for(pending.request_id().to_owned(), budget_ms);
    let admitted = match send_frame_through(relay.workers(), &handle, frame) {
        SendOutcome::Admitted { .. } => true,
        SendOutcome::Refused(refusal) => {
            tracing::warn!(%worker_fp, kind = T::KIND, %refusal,
                "terminal send: the socket did not take the frame");
            relay.pending().reject_unavailable(
                pending.request_id(),
                wording.dropped,
                Some(worker_fp.as_str()),
            );
            false
        }
    };
    TerminalWorkerRequest::from_pending(pending, &deadline, admitted)
}
