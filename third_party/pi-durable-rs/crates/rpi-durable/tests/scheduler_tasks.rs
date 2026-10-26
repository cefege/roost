//! Scheduler task lifecycle tests (port of
//! `packages/durable/test/harness-tasks.test.ts` @ `abe508e1b`).
//!
//! Every case drives the real [`rpi_durable::harness::TaskScheduler`] through a
//! Harness opened over memory storage. Upstream `describe`/`it` anchors are
//! cited per test.

mod scheduler_support;

use std::sync::Arc;

use rpi_durable::harness::{BlockedReason, TaskAbort, TaskInspectionState};
use rpi_durable::storage::MemoryStorage;
use rpi_durable::types::{Storage, TaskStatus};

use scheduler_support::{
    Spec, add_task, all_tasks, context, open_with, open_with_storage, root, terminal_status,
};

/// upstream: "continues one invocation through checkpoint progress".
#[tokio::test]
async fn continues_progress_and_completes() {
    let opened = open_with(all_tasks()).await;
    let conversation = root(&opened.harness).await;
    let id = add_task(
        &opened.harness,
        conversation,
        Spec::owned("test.count", serde_json::json!({ "phases": 3 })),
    )
    .await;

    let settled = opened.harness.wait_for_task(id, &context()).await.unwrap();
    assert_eq!(settled.result, serde_json::json!({ "count": 3 }));
    assert_eq!(terminal_status(&settled.record), Some("completed"));
    opened.harness.close(&context()).await.unwrap();
}

#[tokio::test]
async fn wake_reports_running_invocation_as_active() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let opened = open_with(vec![Arc::new(scheduler_support::BlockingTask {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    }) as rpi_durable::harness::AnyTask])
    .await;
    let conversation = root(&opened.harness).await;
    let id = add_task(
        &opened.harness,
        conversation,
        Spec::owned("test.blocking", serde_json::Value::Null),
    )
    .await;
    entered.notified().await;
    assert!(opened.harness.wake().unwrap().active);
    release.notify_one();
    let settled = opened.harness.wait_for_task(id, &context()).await.unwrap();
    assert_eq!(terminal_status(&settled.record), Some("completed"));
    opened.harness.close(&context()).await.unwrap();
}

#[tokio::test]
async fn wake_ignores_blocked_pending_tasks() {
    let opened = open_with(all_tasks()).await;
    let conversation = root(&opened.harness).await;
    let id = add_task(
        &opened.harness,
        conversation,
        Spec::owned("test.missing", serde_json::Value::Null),
    )
    .await;
    let wake = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let wake = opened.harness.wake().unwrap();
            if !wake.active {
                break wake;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("reconciliation settles");
    assert_eq!(wake.at, None);
    let inspection = opened.harness.inspect(&context()).await.unwrap();
    assert!(matches!(
        &inspection
            .tasks
            .iter()
            .find(|task| task.record.id == id)
            .unwrap()
            .state,
        TaskInspectionState::Blocked {
            reason: BlockedReason::MissingTask,
            ..
        }
    ));
    opened.harness.close(&context()).await.unwrap();
}

/// upstream: "faults a ... throwing phase".
#[tokio::test]
async fn throwing_phase_faults() {
    let opened = open_with(all_tasks()).await;
    let conversation = root(&opened.harness).await;
    let id = add_task(
        &opened.harness,
        conversation,
        Spec::owned("test.throw", serde_json::json!({})),
    )
    .await;

    let settled = opened.harness.wait_for_task(id, &context()).await.unwrap();
    assert_eq!(terminal_status(&settled.record), Some("faulted"));
    opened.harness.close(&context()).await.unwrap();
}

/// upstream: "faults a phase without durable progress".
#[tokio::test]
async fn no_progress_faults() {
    let opened = open_with(all_tasks()).await;
    let conversation = root(&opened.harness).await;
    let id = add_task(
        &opened.harness,
        conversation,
        Spec::owned("test.noprogress", serde_json::json!({})),
    )
    .await;

    let settled = opened.harness.wait_for_task(id, &context()).await.unwrap();
    assert_eq!(terminal_status(&settled.record), Some("faulted"));
    opened.harness.close(&context()).await.unwrap();
}

/// upstream: "keeps a committed terminal outcome when handler throws afterwards".
#[tokio::test]
async fn committed_terminal_survives_later_throw() {
    let opened = open_with(all_tasks()).await;
    let conversation = root(&opened.harness).await;
    let id = add_task(
        &opened.harness,
        conversation,
        Spec::owned("test.terminal-throw", serde_json::json!({})),
    )
    .await;

    let settled = opened.harness.wait_for_task(id, &context()).await.unwrap();
    assert_eq!(terminal_status(&settled.record), Some("completed"));
    assert_eq!(settled.result, serde_json::json!({ "kept": true }));
    opened.harness.close(&context()).await.unwrap();
}

/// upstream: "resumes a waiting task once every task in `on` terminal (any outcome)".
#[tokio::test]
async fn waits_all_settled() {
    let opened = open_with(all_tasks()).await;
    let conversation = root(&opened.harness).await;
    let first = add_task(
        &opened.harness,
        conversation,
        Spec::owned("test.count", serde_json::json!({ "phases": 1 })),
    )
    .await;
    let second = add_task(
        &opened.harness,
        conversation,
        Spec::owned("test.count", serde_json::json!({ "phases": 2 })),
    )
    .await;
    let mut waiter = Spec::owned(
        "test.wait",
        serde_json::json!({ "on": [first.get(), second.get()], "policy": "allSettled" }),
    );
    waiter.checkpoint = serde_json::json!({ "phase": "wait", "waited": false });
    let waiter_id = add_task(&opened.harness, conversation, waiter).await;

    let settled = opened
        .harness
        .wait_for_task(waiter_id, &context())
        .await
        .unwrap();
    assert_eq!(terminal_status(&settled.record), Some("completed"));
    assert_eq!(settled.result, serde_json::json!({ "done": true }));
    opened.harness.close(&context()).await.unwrap();
}

/// upstream: "stamps startedAt at first run / endedAt at terminal; keeps them through reopen".
#[tokio::test]
async fn stamps_times_and_survives_reopen() {
    let storage: Arc<dyn Storage> = Arc::new(MemoryStorage::new());
    let opened = open_with_storage(Arc::clone(&storage), all_tasks()).await;
    let conversation = root(&opened.harness).await;
    let id = add_task(
        &opened.harness,
        conversation,
        Spec::owned("test.count", serde_json::json!({ "phases": 2 })),
    )
    .await;

    let settled = opened.harness.wait_for_task(id, &context()).await.unwrap();
    let started = settled.record.started_at;
    let ended = settled.record.ended_at;
    assert!(started.is_some(), "startedAt stamped");
    assert!(ended.is_some(), "endedAt stamped");

    // Reopen a second Harness over the same Storage (a crash-free restart).
    let reopened = open_with_storage(Arc::clone(&storage), all_tasks()).await;
    let record = reopened
        .harness
        .get_task(id, &context())
        .await
        .unwrap()
        .expect("task record");
    assert_eq!(record.started_at, started);
    assert_eq!(record.ended_at, ended);
    assert_eq!(terminal_status(&record), Some("completed"));
    reopened.harness.close(&context()).await.unwrap();
}

/// upstream: "keeps memos until terminal".
#[tokio::test]
async fn keeps_memos_across_phases() {
    let opened = open_with(all_tasks()).await;
    let conversation = root(&opened.harness).await;
    let id = add_task(
        &opened.harness,
        conversation,
        Spec::owned("test.memo", serde_json::json!({})),
    )
    .await;

    let settled = opened.harness.wait_for_task(id, &context()).await.unwrap();
    assert_eq!(settled.result, serde_json::json!(7));
    assert!(
        settled.record.memos.is_none(),
        "memos retired with the outcome"
    );
    opened.harness.close(&context()).await.unwrap();
}

/// upstream: "resumes abort work after ... abort"; commit the abort mark and run the handler.
#[tokio::test]
async fn abort_runs_handler_to_terminal() {
    let opened = open_with(all_tasks()).await;
    let conversation = root(&opened.harness).await;
    let id = add_task(
        &opened.harness,
        conversation,
        Spec::owned("test.abort", serde_json::json!({})),
    )
    .await;

    let result = opened.harness.abort_task(id, &context()).await.unwrap();
    assert_eq!(result, TaskAbort::Marked);
    let settled = opened.harness.wait_for_task(id, &context()).await.unwrap();
    assert_eq!(terminal_status(&settled.record), Some("aborted"));
    opened.harness.close(&context()).await.unwrap();
}

/// upstream: "an abort handler that returns without an outcome faults the task".
#[tokio::test]
async fn abort_without_outcome_faults() {
    let opened = open_with(all_tasks()).await;
    let conversation = root(&opened.harness).await;
    let id = add_task(
        &opened.harness,
        conversation,
        Spec::owned("test.abort-no-outcome", serde_json::json!({})),
    )
    .await;

    let _ = opened.harness.abort_task(id, &context()).await.unwrap();
    let settled = opened.harness.wait_for_task(id, &context()).await.unwrap();
    assert_eq!(terminal_status(&settled.record), Some("faulted"));
    opened.harness.close(&context()).await.unwrap();
}

/// upstream: "marks the sibling of a failed task in a `failFast` wait".
#[tokio::test]
async fn fail_fast_marks_siblings_and_settles() {
    let opened = open_with(all_tasks()).await;
    let conversation = root(&opened.harness).await;
    let id = add_task(
        &opened.harness,
        conversation,
        Spec::owned("test.fanout", serde_json::json!({})),
    )
    .await;

    let settled = opened.harness.wait_for_task(id, &context()).await.unwrap();
    assert_eq!(terminal_status(&settled.record), Some("completed"));
    assert_eq!(settled.result, serde_json::json!({ "fanout": true }));
    // The slow sibling is abort-marked by the cascade, so the scope drains.
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        opened.harness.wait_for_idle(&context()),
    )
    .await
    .expect("idle")
    .unwrap();
    opened.harness.close(&context()).await.unwrap();
}

/// upstream: "blocked tasks ... missing definition stays pending/live until registration".
#[tokio::test]
async fn inspect_reports_missing_definition() {
    let opened = open_with(Vec::new()).await;
    let conversation = root(&opened.harness).await;
    let id = add_task(
        &opened.harness,
        conversation,
        Spec::owned("test.count", serde_json::json!({ "phases": 1 })),
    )
    .await;

    let inspection = opened.harness.inspect(&context()).await.unwrap();
    let entry = inspection
        .tasks
        .iter()
        .find(|task| task.record.id == id)
        .expect("live task");
    assert!(matches!(
        entry.state,
        TaskInspectionState::Blocked {
            reason: BlockedReason::MissingTask,
            ..
        }
    ));
    assert_eq!(entry.record.status(), TaskStatus::Pending);
    opened.harness.close(&context()).await.unwrap();
}
