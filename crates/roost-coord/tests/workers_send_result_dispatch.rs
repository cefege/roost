//! Typed worker results reaching the pending table through the real frame
//! dispatcher: an input or pipeline result settles exactly its own request,
//! and only from the ready, current, authenticated generation.
//!
//! Ports `apps/coord/tests/workers/worker-ws-result-dispatch.test.ts` and
//! `apps/coord/tests/workers/worker-frame-dispatch-terminal-pipeline.test.ts`
//! (`worker_link::live_frames::handle_rpc`). `unwrap`/`expect` are denied
//! outside `#[cfg(test)]`; an integration test is its own crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod frame_dispatch_support;
mod workers_send_support;
mod workers_support;

use std::collections::BTreeSet;
use std::sync::Arc;

use connectrpc::ErrorCode;
use frame_dispatch_support::{LinkFixture, OTHER_FP, WORKER_FP, rpc_frame, worker};
use roost_coord::coord_core::worker_handle::WorkerHandle;
use roost_coord::worker_link::dispatch::{DispatchOutcome, FrameDispatch};
use roost_coord::worker_link::frame_dispatch::WorkerFrameDispatcher;
use roost_coord::workers::registry::claim_generation;
use roost_proto::{TerminalPipelineSessionSnapshot, WTerminalPipelineSnapshot};
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use roost_protocol::wire::coord_worker::CoordWorkerUpstream as Up;
use roost_protocol::wire::coord_worker::InputResult;
use workers_send_support::accepted_input;

/// A ready, current generation for `worker_fp`, and a dispatcher over it.
fn claim_ready(fixture: &LinkFixture, worker_fp: &str, generation: &str) -> WorkerFrameDispatcher {
    let handle = Arc::new(WorkerHandle::new(
        worker(worker_fp),
        None,
        generation.to_owned(),
        BTreeSet::new(),
        Arc::new(|_: CoordWorkerDownstream| 1_i64),
    ));
    claim_generation(
        &fixture.services.buses,
        &fixture.services.workers,
        Arc::clone(&handle),
    );
    handle.mark_ready();
    fixture.services.worker_dispatcher(handle)
}

fn pipeline(request_id: &str) -> WTerminalPipelineSnapshot {
    WTerminalPipelineSnapshot {
        request_id: request_id.to_owned(),
        ..Default::default()
    }
}

// v2 "settles input and stream results" (the dispatcher half; the read loop's
// bypass is `worker_link_result_lane.rs`).
#[tokio::test]
async fn an_input_result_settles_its_exact_request() {
    let fixture = LinkFixture::new("typed-results-settle").await;
    fixture.mark_ready();
    let dispatcher = fixture.dispatcher();
    let table = fixture.services.scrollback.pending();
    let mut first = table.create_fresh(Some(WORKER_FP), 1_000).unwrap();
    let _other = table.create_fresh(Some(WORKER_FP), 1_000).unwrap();

    let frame = rpc_frame(Up::InputResult(accepted_input(first.request_id(), 1)));
    assert_eq!(
        dispatcher.handle_now(WORKER_FP, frame),
        DispatchOutcome::Handled
    );
    let settled = first.settle_typed::<InputResult>().await.unwrap();
    assert_eq!(settled.request_id, first.request_id());
    assert_eq!(table.pending_count(), 1, "only the named request settled");
}

// v2 "leaves stale, unready, foreign, and unmatched results pending".
#[tokio::test]
async fn unready_foreign_unmatched_and_stale_results_leave_the_request_pending() {
    let fixture = LinkFixture::new("typed-results-fenced").await;
    let target = fixture.dispatcher();
    let table = fixture.services.scrollback.pending();
    let mut early = table.create_fresh(Some(WORKER_FP), 1_000).unwrap();
    let mut late = table.create_fresh(Some(WORKER_FP), 1_000).unwrap();
    let early_result = || rpc_frame(Up::InputResult(accepted_input(early.request_id(), 1)));
    let late_result = || rpc_frame(Up::InputResult(accepted_input(late.request_id(), 1)));

    assert_eq!(
        target.handle_now(WORKER_FP, early_result()),
        DispatchOutcome::Refused
    );
    assert_eq!(
        table.pending_count(),
        2,
        "an unready generation settles nothing"
    );

    fixture.mark_ready();
    let foreign = claim_ready(&fixture, OTHER_FP, "generation-other");
    foreign.handle_now(OTHER_FP, late_result());
    target.handle_now(
        WORKER_FP,
        rpc_frame(Up::InputResult(accepted_input("unmatched", 1))),
    );
    assert_eq!(
        table.pending_count(),
        2,
        "foreign and unmatched replies settle nothing"
    );

    assert_eq!(
        target.handle_now(WORKER_FP, early_result()),
        DispatchOutcome::Handled
    );
    assert!(early.settle_typed::<InputResult>().await.is_ok());

    let _replacement = claim_ready(&fixture, WORKER_FP, "generation-2");
    assert_eq!(
        target.handle_now(WORKER_FP, late_result()),
        DispatchOutcome::Refused
    );
    assert_eq!(
        table.pending_count(),
        1,
        "a superseded generation settles nothing"
    );
    table.reject_all_for_worker(WORKER_FP, "test cleanup");
    let error = late.settle_typed::<InputResult>().await.unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::Unavailable,
        "the late request was never settled"
    );
}

// v2 "requires a ready authenticated target and exact request correlation";
// a requestId-only reply is well-shaped.
#[tokio::test]
async fn a_pipeline_sample_needs_a_ready_authenticated_target_and_its_own_id() {
    let fixture = LinkFixture::new("pipeline-settle").await;
    let target = fixture.dispatcher();
    let table = fixture.services.scrollback.pending();
    let mut pending = table.create_fresh(Some(WORKER_FP), 1_000).unwrap();
    let reply = |id: &str| rpc_frame(Up::TerminalPipelineSnapshot(pipeline(id)));

    target.handle_now(WORKER_FP, reply(pending.request_id()));
    assert_eq!(table.pending_count(), 1, "unready");
    fixture.mark_ready();
    target.handle_now(WORKER_FP, reply("other-request"));
    assert_eq!(table.pending_count(), 1, "another request's id");
    let spoofed = claim_ready(&fixture, OTHER_FP, "generation-spoofed");
    spoofed.handle_now(OTHER_FP, reply(pending.request_id()));
    assert_eq!(table.pending_count(), 1, "another worker's fingerprint");

    assert_eq!(
        target.handle_now(WORKER_FP, reply(pending.request_id())),
        DispatchOutcome::Handled
    );
    let settled = pending
        .settle_typed::<WTerminalPipelineSnapshot>()
        .await
        .unwrap();
    assert_eq!(settled.request_id, pending.request_id());
}

// v2 worker-frame-dispatch.ts `invalid_terminal_pipeline_snapshot`: a sample
// that fails the wire-shape check is dropped before it can settle anything.
#[tokio::test]
async fn a_malformed_pipeline_sample_is_refused_and_settles_nothing() {
    let fixture = LinkFixture::new("pipeline-malformed").await;
    fixture.mark_ready();
    let target = fixture.dispatcher();
    let table = fixture.services.scrollback.pending();
    let pending = table.create_fresh(Some(WORKER_FP), 1_000).unwrap();
    let duplicated = TerminalPipelineSessionSnapshot {
        session_id: "session-a".to_owned(),
        view_id: "view-a".to_owned(),
        ..Default::default()
    };
    let malformed = WTerminalPipelineSnapshot {
        sessions: vec![duplicated.clone(), duplicated],
        ..pipeline(pending.request_id())
    };

    let outcome = target.handle_now(
        WORKER_FP,
        rpc_frame(Up::TerminalPipelineSnapshot(malformed)),
    );
    assert_eq!(outcome, DispatchOutcome::Refused);
    assert_eq!(
        table.pending_count(),
        1,
        "the request still waits for a valid sample"
    );
}
