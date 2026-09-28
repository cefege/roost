//! Typed terminal-pipeline diagnostics routed over the worker link: direct
//! request framing, target-worker correlation, and the reply-matches-request
//! check a collected sample passes before it is reported.
//!
//! Ports `apps/coord/tests/terminal/screen/worker-terminal-pipeline-snapshot.test.ts`
//! against `terminal_screen::pipeline_request`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_screen_pipeline_support;

use std::collections::BTreeMap;
use std::sync::Arc;

use roost_coord::terminal_screen::pipeline_request::{
    TerminalPipelineSnapshotErrorCode, WorkerTerminalPipelineSnapshotResult,
    collect_worker_terminal_pipeline_snapshots,
};
use roost_coord::terminal_screen::pipeline_snapshot::{
    TERMINAL_PIPELINE_DIAG_MAX_TARGETS, TerminalPipelineDiagnosticTarget,
};
use roost_protocol::wire::WorkerFp;
use terminal_screen_pipeline_support::{
    FakeWorkers, SPOOFED_WORKER_FP, TARGET_WORKER_FP, accept_only, pipeline_reply, request_targets,
    resolve_pipeline, target,
};

fn by_worker(
    targets: Vec<TerminalPipelineDiagnosticTarget>,
) -> BTreeMap<WorkerFp, Vec<TerminalPipelineDiagnosticTarget>> {
    BTreeMap::from([(WorkerFp::try_from(TARGET_WORKER_FP).unwrap(), targets)])
}

fn target_result(
    results: &BTreeMap<WorkerFp, WorkerTerminalPipelineSnapshotResult>,
) -> &WorkerTerminalPipelineSnapshotResult {
    results
        .get(&WorkerFp::try_from(TARGET_WORKER_FP).unwrap())
        .expect("a result for the target worker")
}

fn error_code(result: &WorkerTerminalPipelineSnapshotResult) -> TerminalPipelineSnapshotErrorCode {
    match result {
        WorkerTerminalPipelineSnapshotResult::Error { code, .. } => *code,
        WorkerTerminalPipelineSnapshotResult::Ok { .. } => {
            panic!("expected an error result, got {result:?}")
        }
    }
}

// v2: "sends a bounded direct request and accepts only the target worker reply".
#[tokio::test]
async fn sends_a_bounded_direct_request_and_accepts_only_the_target_worker_reply() {
    let workers = FakeWorkers::new();
    let pending_before = workers.relay.pending().pending_count();
    let targets: Vec<_> = (0..=TERMINAL_PIPELINE_DIAG_MAX_TARGETS)
        .map(|index| target(&format!("session-{index:02}"), ""))
        .collect();
    workers.connect(
        TARGET_WORKER_FP,
        "pipeline-snapshot-test-connection-1",
        true,
        Arc::new(|relay, request| {
            let received = request_targets(request);
            assert_eq!(received.len(), TERMINAL_PIPELINE_DIAG_MAX_TARGETS);
            assert!(!resolve_pipeline(
                relay,
                pipeline_reply(&request.request_id, &received),
                SPOOFED_WORKER_FP
            ));
            assert!(resolve_pipeline(
                relay,
                pipeline_reply(&request.request_id, &received),
                TARGET_WORKER_FP
            ));
            1
        }),
    );

    let results = collect_worker_terminal_pipeline_snapshots(
        &workers.relay,
        &by_worker(targets.clone()),
        Some(100),
    )
    .await;

    let received = request_targets(&workers.requests()[0]);
    assert_eq!(
        received,
        targets[..TERMINAL_PIPELINE_DIAG_MAX_TARGETS].to_vec()
    );
    let WorkerTerminalPipelineSnapshotResult::Ok { snapshot, .. } = target_result(&results) else {
        panic!("expected an ok result, got {results:?}");
    };
    assert_eq!(snapshot.sessions[0].session_id, "session-00");
    assert_eq!(snapshot.sessions[0].view_id, "");
    assert_eq!(workers.relay.pending().pending_count(), pending_before);
}

// v2: "ignores a mismatched reply before the matching response arrives".
#[tokio::test]
async fn ignores_a_mismatched_reply_before_the_matching_response_arrives() {
    let workers = FakeWorkers::new();
    let session = [target("session", "")];
    workers.connect(
        TARGET_WORKER_FP,
        "pipeline-snapshot-test-connection-2",
        true,
        Arc::new(move |relay, request| {
            assert!(!resolve_pipeline(
                relay,
                pipeline_reply("other-request", &session),
                TARGET_WORKER_FP
            ));
            assert!(resolve_pipeline(
                relay,
                pipeline_reply(&request.request_id, &session),
                TARGET_WORKER_FP
            ));
            1
        }),
    );

    let results = collect_worker_terminal_pipeline_snapshots(
        &workers.relay,
        &by_worker(vec![target("session", "")]),
        Some(100),
    )
    .await;

    assert!(matches!(
        target_result(&results),
        WorkerTerminalPipelineSnapshotResult::Ok { .. }
    ));
}

// v2: "rejects an unaccounted partial response".
#[tokio::test]
async fn rejects_an_unaccounted_partial_response() {
    let workers = FakeWorkers::new();
    workers.connect(
        TARGET_WORKER_FP,
        "pipeline-snapshot-test-connection-3",
        true,
        Arc::new(|relay, request| {
            let first = request_targets(request)[..1].to_vec();
            assert!(resolve_pipeline(
                relay,
                pipeline_reply(&request.request_id, &first),
                TARGET_WORKER_FP
            ));
            1
        }),
    );

    let results = collect_worker_terminal_pipeline_snapshots(
        &workers.relay,
        &by_worker(vec![
            target("first-session", ""),
            target("second-session", ""),
        ]),
        Some(100),
    )
    .await;

    assert_eq!(
        error_code(target_result(&results)),
        TerminalPipelineSnapshotErrorCode::RpcError
    );
}

// v2: "does not send to an unready worker".
#[tokio::test]
async fn does_not_send_to_an_unready_worker() {
    let workers = FakeWorkers::new();
    workers.connect(
        TARGET_WORKER_FP,
        "pipeline-snapshot-test-connection-4",
        false,
        Arc::new(|_, _| panic!("unready worker must not receive a pipeline request")),
    );

    let results = collect_worker_terminal_pipeline_snapshots(
        &workers.relay,
        &by_worker(vec![target("session", "")]),
        Some(100),
    )
    .await;

    assert_eq!(
        error_code(target_result(&results)),
        TerminalPipelineSnapshotErrorCode::Offline
    );
}

// v2: "does not dispatch an empty worker target group".
#[tokio::test]
async fn does_not_dispatch_an_empty_worker_target_group() {
    let workers = FakeWorkers::new();
    workers.connect(
        TARGET_WORKER_FP,
        "pipeline-snapshot-test-connection-5",
        true,
        Arc::new(|_, _| panic!("empty group must not reach the worker")),
    );

    let results = collect_worker_terminal_pipeline_snapshots(
        &workers.relay,
        &by_worker(Vec::new()),
        Some(100),
    )
    .await;

    assert!(results.is_empty());
}

// The error codes below are v2's `requestWorkerTerminalPipelineSnapshot` arms
// (`sent === 0` → send_failed, a DeadlineExceeded pending → timeout), which the
// v2 suite leaves unexercised.
#[tokio::test]
async fn a_dropped_write_is_send_failed_and_leaves_nothing_pending() {
    let workers = FakeWorkers::new();
    workers.connect(
        TARGET_WORKER_FP,
        "pipeline-snapshot-test-connection-6",
        true,
        Arc::new(|_, _| 0),
    );

    let results = collect_worker_terminal_pipeline_snapshots(
        &workers.relay,
        &by_worker(vec![target("session", "")]),
        Some(100),
    )
    .await;

    assert_eq!(
        error_code(target_result(&results)),
        TerminalPipelineSnapshotErrorCode::SendFailed
    );
    assert_eq!(workers.relay.pending().pending_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_silent_worker_times_out_within_the_bounded_wait() {
    let workers = FakeWorkers::new();
    workers.connect(
        TARGET_WORKER_FP,
        "pipeline-snapshot-test-connection-7",
        true,
        accept_only(),
    );

    let results = collect_worker_terminal_pipeline_snapshots(
        &workers.relay,
        &by_worker(vec![target("session", "")]),
        Some(100),
    )
    .await;

    let WorkerTerminalPipelineSnapshotResult::Error {
        code, response_ms, ..
    } = target_result(&results)
    else {
        panic!("expected a timeout, got {results:?}");
    };
    assert_eq!(*code, TerminalPipelineSnapshotErrorCode::Timeout);
    assert!(
        *response_ms >= 100 && *response_ms < 200,
        "waited {response_ms} ms"
    );
    assert_eq!(workers.relay.pending().pending_count(), 0);
}
