//! Scheduler recovery, blocked-definition, and migration tests (port of
//! `packages/durable/test/harness-tasks-recovery.test.ts` @ `abe508e1b`).
//!
//! Fixtures stamp durable records directly (simulating a crash between commits)
//! and then reopen a Harness over the same Storage, exercising `open()`'s
//! running→pending rewind, blocked-definition reporting, and reservation
//! migration.

mod scheduler_support;

use std::sync::Arc;

use rpi_durable::harness::{BlockedReason, Extension, Registry, TaskAbort, TaskInspectionState};
use rpi_durable::storage::MemoryStorage;
use rpi_durable::types::{Storage, TaskState, TaskStatus};

use scheduler_support::{
    DynamicTask, Spec, add_task, all_tasks, context, open_with, open_with_storage, root,
    stamp_task, terminal_status,
};

/// upstream: "resumes ... task interrupted ... across close/reopen"; a surviving
/// `running` record is rewound to `pending` by `open()`.
#[tokio::test]
async fn rewinds_interrupted_running_to_pending() {
    let storage: Arc<dyn Storage> = Arc::new(MemoryStorage::new());
    let opened = open_with_storage(Arc::clone(&storage), all_tasks()).await;
    let conversation = root(&opened.harness).await;
    let id = add_task(
        &opened.harness,
        conversation,
        Spec::owned("test.count", serde_json::json!({ "phases": 5 })),
    )
    .await;

    // Simulate a crash after the record became `running`: stamp it directly
    // without letting the invocation commit progress.
    let mut record = opened
        .harness
        .get_task(id, &context())
        .await
        .unwrap()
        .expect("task record");
    record.state = TaskState::Running {
        checkpoint: serde_json::json!({ "phase": "step", "count": 1 }),
    };
    stamp_task(&opened.harness, record).await;

    let reopened = open_with_storage(Arc::clone(&storage), all_tasks()).await;
    let record = reopened
        .harness
        .get_task(id, &context())
        .await
        .unwrap()
        .expect("task record");
    assert_eq!(record.status(), TaskStatus::Pending);
    // The checkpoint survives the rewind.
    assert_eq!(
        scheduler_support::checkpoint(&record),
        serde_json::json!({ "phase": "step", "count": 1 })
    );
    reopened.harness.close(&context()).await.unwrap();
}

/// upstream: "migration at reservation".
#[tokio::test]
async fn migrates_record_at_reservation() {
    let opened = open_with(all_tasks()).await;
    let conversation = root(&opened.harness).await;
    let mut spec = Spec::owned("test.migrate", serde_json::json!({ "seed": 1 }));
    spec.version = 1;
    spec.checkpoint = serde_json::json!({ "phase": "step", "v": 1 });
    let id = add_task(&opened.harness, conversation, spec).await;

    let settled = opened.harness.wait_for_task(id, &context()).await.unwrap();
    assert_eq!(
        settled.record.version, 2,
        "migrated to the registered version"
    );
    assert_eq!(terminal_status(&settled.record), Some("completed"));
    assert_eq!(settled.result, serde_json::json!({ "v": 2 }));
    opened.harness.close(&context()).await.unwrap();
}

/// upstream: "missing definition stays pending/live until registration".
#[tokio::test]
async fn blocked_until_definition_registered() {
    let opened = open_with(Vec::new()).await;
    let conversation = root(&opened.harness).await;
    let id = add_task(
        &opened.harness,
        conversation,
        Spec::owned("test.dynamic", serde_json::json!({})),
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

    opened
        .registry
        .install(Extension {
            name: "dynamic".into(),
            tasks: vec![Arc::new(DynamicTask)],
            ..Extension::default()
        })
        .expect("install");
    opened.harness.resume().unwrap();
    let settled = opened.harness.wait_for_task(id, &context()).await.unwrap();
    assert_eq!(terminal_status(&settled.record), Some("completed"));
    opened.harness.close(&context()).await.unwrap();
}

/// upstream: "older record without migration blocked".
#[tokio::test]
async fn older_record_without_migration_is_blocked() {
    let opened = open_with(all_tasks()).await;
    let conversation = root(&opened.harness).await;
    let mut spec = Spec::owned("test.old", serde_json::json!({}));
    spec.version = 1;
    spec.checkpoint = serde_json::json!({ "phase": "step", "v": 1 });
    let id = add_task(&opened.harness, conversation, spec).await;

    let inspection = opened.harness.inspect(&context()).await.unwrap();
    let entry = inspection
        .tasks
        .iter()
        .find(|task| task.record.id == id)
        .expect("live task");
    assert!(
        matches!(
            entry.state,
            TaskInspectionState::Blocked {
                reason: BlockedReason::MigrationFailed,
                ..
            }
        ),
        "state: {:?}",
        entry.state
    );
    assert_eq!(entry.record.status(), TaskStatus::Pending);
    opened.harness.close(&context()).await.unwrap();
}

/// upstream: "orphan marked task whose def vanished".
#[tokio::test]
async fn orphans_abort_marked_task_without_definition() {
    let opened = open_with(Vec::new()).await;
    let conversation = root(&opened.harness).await;
    let id = add_task(
        &opened.harness,
        conversation,
        Spec::owned("test.orphan", serde_json::json!({})),
    )
    .await;

    let result = opened.harness.abort_task(id, &context()).await.unwrap();
    assert_eq!(result, TaskAbort::Marked);
    let settled = opened.harness.wait_for_task(id, &context()).await.unwrap();
    assert_eq!(terminal_status(&settled.record), Some("orphaned"));
    opened.harness.close(&context()).await.unwrap();
}
