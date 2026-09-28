//! Handler-owned pipeline sampling batches target scopes without leaking them:
//! each worker is sampled once per cache window, and an uncached target waits
//! for its own bounded batch instead of receiving another target's evidence.
//!
//! Ports `apps/coord/tests/terminal/screen/worker-terminal-pipeline-cache.test.ts`
//! against `terminal_screen::pipeline_cache::WorkerTerminalPipelineSnapshotCache`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_screen_pipeline_support;

use std::collections::BTreeMap;
use std::time::Duration;

use roost_coord::terminal_screen::pipeline_cache::{
    WORKER_TERMINAL_PIPELINE_CACHE_MS, WorkerTerminalPipelineSnapshotCache,
};
use roost_coord::terminal_screen::pipeline_request::{
    TerminalPipelineSnapshotErrorCode, WorkerTerminalPipelineSnapshotResult,
};
use roost_coord::terminal_screen::pipeline_snapshot::TerminalPipelineDiagnosticTarget;
use roost_proto::WTerminalPipelineSnapshot;
use roost_protocol::wire::WorkerFp;
use terminal_screen_pipeline_support::{
    FakeWorkers, TARGET_WORKER_FP, accept_only, pipeline_reply, request_targets, resolve_pipeline,
    settle, target,
};

fn worker_fp() -> WorkerFp {
    WorkerFp::try_from(TARGET_WORKER_FP).unwrap()
}

fn scope(
    target: TerminalPipelineDiagnosticTarget,
) -> BTreeMap<WorkerFp, Vec<TerminalPipelineDiagnosticTarget>> {
    BTreeMap::from([(worker_fp(), vec![target])])
}

fn snapshot_of(
    results: BTreeMap<WorkerFp, WorkerTerminalPipelineSnapshotResult>,
) -> WTerminalPipelineSnapshot {
    match results.into_values().next() {
        Some(WorkerTerminalPipelineSnapshotResult::Ok { snapshot, .. }) => snapshot,
        other => panic!("expected an ok result, got {other:?}"),
    }
}

fn session_ids(snapshot: &WTerminalPipelineSnapshot) -> Vec<&str> {
    snapshot
        .sessions
        .iter()
        .map(|session| session.session_id.as_str())
        .collect()
}

/// Answers the `index`th request the fake worker received with every target it asked for.
fn answer_request(workers: &FakeWorkers, index: usize) {
    let request = workers.requests()[index].clone();
    let reply = pipeline_reply(&request.request_id, &request_targets(&request));
    assert!(resolve_pipeline(&workers.relay, reply, TARGET_WORKER_FP));
}

// v2: "batches concurrent target scopes and reuses only their own cached records".
#[tokio::test]
async fn batches_concurrent_target_scopes_and_reuses_only_their_own_cached_records() {
    let workers = FakeWorkers::new();
    workers.connect(
        TARGET_WORKER_FP,
        "pipeline-cache-test-connection",
        true,
        accept_only(),
    );
    let cache = WorkerTerminalPipelineSnapshotCache::new(workers.relay.clone());
    let (scope_a, scope_b) = (
        scope(target("session-a", "")),
        scope(target("session-b", "")),
    );

    let (first, second, ()) = tokio::join!(
        cache.collect(&scope_a, Some(1_000)),
        cache.collect(&scope_b, Some(1_000)),
        async {
            settle().await;
            let requests = workers.requests();
            assert_eq!(requests.len(), 1);
            assert_eq!(
                request_targets(&requests[0]),
                vec![target("session-a", ""), target("session-b", "")]
            );
            answer_request(&workers, 0);
        }
    );

    assert_eq!(session_ids(&snapshot_of(first)), vec!["session-a"]);
    assert_eq!(session_ids(&snapshot_of(second)), vec!["session-b"]);
    let cached = cache.collect(&scope_a, Some(1_000)).await;
    assert_eq!(session_ids(&snapshot_of(cached)), vec!["session-a"]);
    assert_eq!(workers.requests().len(), 1);
    cache.dispose();
}

// v2: "waits for an uncached target batch instead of projecting another target's sample".
#[tokio::test(start_paused = true)]
async fn waits_for_an_uncached_target_batch_instead_of_projecting_another_targets_sample() {
    let workers = FakeWorkers::new();
    workers.connect(
        TARGET_WORKER_FP,
        "pipeline-cache-test-connection",
        true,
        accept_only(),
    );
    let cache = WorkerTerminalPipelineSnapshotCache::new(workers.relay.clone());
    let (scope_a, scope_b) = (
        scope(target("session-a", "")),
        scope(target("session-b", "")),
    );

    let (first, ()) = tokio::join!(cache.collect(&scope_a, Some(1_000)), async {
        settle().await;
        answer_request(&workers, 0);
    });
    assert_eq!(session_ids(&snapshot_of(first)), vec!["session-a"]);

    let (second, ()) = tokio::join!(cache.collect(&scope_b, Some(1_000)), async {
        settle().await;
        assert_eq!(workers.requests().len(), 1);
        tokio::time::advance(Duration::from_millis(WORKER_TERMINAL_PIPELINE_CACHE_MS)).await;
        settle().await;
        let requests = workers.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(request_targets(&requests[1]), vec![target("session-b", "")]);
        answer_request(&workers, 1);
    });

    let snapshot = snapshot_of(second);
    assert_eq!(session_ids(&snapshot), vec!["session-b"]);
    assert_eq!(snapshot.dropped_records, 0);
    cache.dispose();
}

// v2 header: "a generation replacement starts fresh" (`collectWorker`'s
// `entry.worker !== worker` arm), which the v2 suite leaves unexercised.
#[tokio::test]
async fn a_replaced_generation_is_sampled_afresh() {
    let workers = FakeWorkers::new();
    workers.connect(
        TARGET_WORKER_FP,
        "pipeline-cache-generation-1",
        true,
        accept_only(),
    );
    let cache = WorkerTerminalPipelineSnapshotCache::new(workers.relay.clone());
    let scope_a = scope(target("session-a", ""));
    let (first, ()) = tokio::join!(cache.collect(&scope_a, Some(1_000)), async {
        settle().await;
        answer_request(&workers, 0);
    });
    assert_eq!(session_ids(&snapshot_of(first)), vec!["session-a"]);

    workers.connect(
        TARGET_WORKER_FP,
        "pipeline-cache-generation-2",
        true,
        accept_only(),
    );
    let (second, ()) = tokio::join!(cache.collect(&scope_a, Some(1_000)), async {
        settle().await;
        assert_eq!(workers.requests().len(), 2);
        answer_request(&workers, 1);
    });

    let snapshot = snapshot_of(second);
    assert_eq!(snapshot.request_id, workers.requests()[1].request_id);
    cache.dispose();
}

// v2 `dispose()`: every waiting collection settles offline instead of hanging.
#[tokio::test]
async fn dispose_settles_a_waiting_collection_offline() {
    let workers = FakeWorkers::new();
    workers.connect(
        TARGET_WORKER_FP,
        "pipeline-cache-test-connection",
        true,
        accept_only(),
    );
    let cache = WorkerTerminalPipelineSnapshotCache::new(workers.relay.clone());
    let scope_a = scope(target("session-a", ""));

    let (waiting, ()) = tokio::join!(cache.collect(&scope_a, Some(1_000)), async {
        settle().await;
        assert_eq!(workers.requests().len(), 1);
        cache.dispose();
    });

    let result = waiting.into_values().next();
    assert!(
        matches!(result, Some(WorkerTerminalPipelineSnapshotResult::Error { code: TerminalPipelineSnapshotErrorCode::Offline, ref message, .. }) if message == "pipeline cache disposed"),
        "got {result:?}"
    );
}
