//! The terminal-input sender: a refusal before the write is definite and
//! correlates nothing, admission is transport only, and the typed result -- or
//! a rejection, or the hop deadline -- is the only verdict.
//!
//! Ports `apps/coord/tests/terminal/terminal-hop-deadline.test.ts` (the sender
//! half), `apps/coord/tests/workers/worker-ws-transport-send-failure.test.ts`
//! (the sender's reaction to a dropped write) and the typed cases of
//! `apps/coord/tests/pending-rpc-drop.test.ts`. `unwrap`/`expect` are denied
//! outside `#[cfg(test)]`; an integration test is its own crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod workers_send_support;

use std::time::Duration;

use connectrpc::ErrorCode;
use roost_coord::terminal_screen::typed_results::TypedWorkerResult;
use roost_coord::workers::hop_deadline::{HopDeadline, INPUT_CONTROL_TIMEOUT_MS};
use roost_coord::workers::terminal_send::{TerminalInputSend, send_terminal_input_request};
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use workers_send_support::{
    OTHER_FP, TerminalLink, WORKER_FP, accepted_input, message_of, session, worker,
};

fn batch(data: &[u8]) -> TerminalInputSend {
    TerminalInputSend {
        session_id: session(),
        input_seq: 1,
        data: data.to_vec(),
        device_fingerprint: "device-1".to_owned(),
        tab_id: "tab-1".to_owned(),
        browser_connection_id: "sync-1".to_owned(),
        input_route_epoch: "route-epoch-1".to_owned(),
    }
}

fn fresh() -> HopDeadline {
    HopDeadline::start(INPUT_CONTROL_TIMEOUT_MS)
}

// v2 worker-send.ts `unsentTerminalWorkerRequest("worker offline", false)`.
#[tokio::test]
async fn an_offline_worker_is_refused_before_anything_is_correlated() {
    let link = TerminalLink::offline();
    let request = send_terminal_input_request(&link.relay, &worker(), batch(b"a"), fresh());
    assert!(!request.is_admitted() && !request.is_expired());
    assert_eq!(request.request_id(), None);
    assert_eq!(link.pending_count(), 0);
    let error = request.result().await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Unavailable);
    assert_eq!(message_of(&error), "worker offline");
}

// v2 "a budget too short to survive the hop is refused rather than half-spent".
#[tokio::test(start_paused = true)]
async fn a_budget_too_short_to_survive_the_hop_is_refused_as_expired_and_never_written() {
    let link = TerminalLink::routable();
    let deadline = fresh();
    tokio::time::advance(Duration::from_millis(4_200)).await;
    let request = send_terminal_input_request(&link.relay, &worker(), batch(b"a"), deadline);
    assert!(
        request.is_expired(),
        "the refusal is the definite pre-send kind"
    );
    assert!(!request.is_admitted());
    assert!(link.frames().is_empty(), "nothing reached the socket");
    assert_eq!(link.pending_count(), 0, "and nothing was correlated");
    let error = request.result().await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Unavailable);
    assert_eq!(
        message_of(&error),
        "terminal input budget expired before send"
    );
}

// v2 "a healthy remaining budget still sends, and the worker slice is strictly
// smaller", with the actor fields the input route is fenced on.
#[tokio::test(start_paused = true)]
async fn a_healthy_budget_sends_the_actor_and_a_strictly_smaller_worker_slice() {
    let link = TerminalLink::routable();
    let deadline = fresh();
    tokio::time::advance(Duration::from_millis(1_000)).await;
    let request = send_terminal_input_request(&link.relay, &worker(), batch(b"ls"), deadline);
    assert!(request.is_admitted() && !request.is_expired());
    let frames = link.frames();
    let [CoordWorkerDownstream::InputRequest(sent)] = frames.as_slice() else {
        panic!("exactly one input request, got {frames:?}");
    };
    assert_eq!(Some(sent.request_id.as_str()), request.request_id());
    assert_eq!(
        sent.budget_ms, 3_250,
        "4000 ms left less the 750 ms reserve"
    );
    assert_eq!(sent.session_id, session().as_str());
    assert_eq!(sent.data, b"ls");
    assert_eq!(
        (
            sent.device_fingerprint.as_str(),
            sent.tab_id.as_str(),
            sent.browser_connection_id.as_str(),
            sent.input_route_epoch.as_str(),
        ),
        ("device-1", "tab-1", "sync-1", "route-epoch-1")
    );
}

// v2 worker-send.ts: the promise resolves exclusively from WInputResult.
#[tokio::test]
async fn the_typed_input_result_is_the_verdict() {
    let link = TerminalLink::routable();
    let request = send_terminal_input_request(&link.relay, &worker(), batch(b"ab"), fresh());
    let request_id = request.request_id().unwrap().to_owned();
    let result = accepted_input(&request_id, 2);
    assert!(
        link.relay
            .pending()
            .resolve_typed(TypedWorkerResult::Input(result.clone()), Some(WORKER_FP))
    );
    assert_eq!(request.result().await.unwrap(), result);
    assert_eq!(link.pending_count(), 0);
}

// v2 worker-frame-dispatch.ts resolves with the AUTHENTICATED fingerprint, so a
// reply from another worker under the same id settles nothing.
#[tokio::test]
async fn a_result_from_another_worker_leaves_the_request_waiting() {
    let link = TerminalLink::routable();
    let request = send_terminal_input_request(&link.relay, &worker(), batch(b"a"), fresh());
    let request_id = request.request_id().unwrap().to_owned();
    let pending = link.relay.pending();
    let foreign = TypedWorkerResult::Input(accepted_input(&request_id, 1));
    assert!(!pending.resolve_typed(foreign.clone(), Some(OTHER_FP)));
    assert_eq!(link.pending_count(), 1, "the foreign reply was refused");
    assert!(pending.resolve_typed(foreign, Some(WORKER_FP)));
    assert!(request.result().await.is_ok());
}

// A typed result of the wrong kind under the right id is refused, not misread.
#[tokio::test]
async fn a_result_of_another_kind_is_refused_rather_than_read_as_input() {
    let link = TerminalLink::routable();
    let request = send_terminal_input_request(&link.relay, &worker(), batch(b"a"), fresh());
    let wrong = roost_proto::WTerminalPipelineSnapshot {
        request_id: request.request_id().unwrap().to_owned(),
        ..Default::default()
    };
    assert!(
        link.relay
            .pending()
            .resolve_typed(TypedWorkerResult::PipelineSnapshot(wrong), Some(WORKER_FP))
    );
    let error = request.result().await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Internal);
    assert!(message_of(&error).contains("terminal-pipeline-snapshot"));
}

// v2 worker-ws-transport-send-failure: a write the transport refuses is not
// admission. worker-send.ts rejects the correlation Unavailable at once.
#[tokio::test]
async fn a_dropped_write_is_unadmitted_and_rejects_retryably_at_once() {
    let link = TerminalLink::dropping();
    let request = send_terminal_input_request(&link.relay, &worker(), batch(b"a"), fresh());
    assert!(!request.is_admitted());
    assert!(
        !request.is_expired(),
        "a dropped write is not a budget expiry"
    );
    assert!(request.request_id().is_some(), "the id was allocated first");
    assert_eq!(link.pending_count(), 0, "the entry left with the refusal");
    let error = request.result().await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Unavailable);
    assert_eq!(
        message_of(&error),
        "worker transport dropped terminal input"
    );
}

// v2 pending-rpcs.ts timer: an unanswered admitted request expires as
// DeadlineExceeded at what the hop deadline had left when it was SENT -- never
// a fresh budget, and not extended by a caller that awaits the result late.
#[tokio::test(start_paused = true)]
async fn an_unanswered_request_expires_at_what_the_hop_deadline_had_left() {
    let link = TerminalLink::routable();
    let deadline = fresh();
    tokio::time::advance(Duration::from_millis(1_000)).await;
    let request = send_terminal_input_request(&link.relay, &worker(), batch(b"a"), deadline);
    let sent_at = tokio::time::Instant::now();
    tokio::time::advance(Duration::from_millis(1_500)).await;
    let error = request.result().await.unwrap_err();
    assert_eq!(error.code, ErrorCode::DeadlineExceeded);
    assert_eq!(message_of(&error), "worker did not reply within 4000ms");
    assert_eq!(sent_at.elapsed(), Duration::from_millis(4_000));
    assert_eq!(link.pending_count(), 0, "the expired entry left the table");
}

// v2 pending-rpc-drop.test.ts "rejects only the dropped worker's RPCs": a
// worker close fails an in-flight input fast and retryably.
#[tokio::test]
async fn a_worker_close_rejects_the_in_flight_input_as_unavailable() {
    let link = TerminalLink::routable();
    let request = send_terminal_input_request(&link.relay, &worker(), batch(b"a"), fresh());
    assert_eq!(
        link.relay
            .pending()
            .reject_all_for_worker(WORKER_FP, "worker disconnected"),
        1
    );
    let error = request.result().await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Unavailable);
    assert!(message_of(&error).contains("worker disconnected"));
}
