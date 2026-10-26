//! Durable persistence and restart acceptance tests across SQLite and JSONL.

mod scheduler_support;
mod test_support;

use std::sync::Arc;

use rpi_durable::fs::StdFileSystem;
use rpi_durable::storage::open_node_sqlite_storage;
use rpi_durable::storage::{JsonlStorage, JsonlStorageOptions, SqliteDatabaseOptions};
use rpi_durable::types::{Storage, TaskState, TaskStatus};

use scheduler_support::{
    Spec, add_task, all_tasks, context, open_with_storage, root, stamp_task, terminal_status,
};
use test_support::TempDir;

/// Persist a running `test.count` record and return its id, simulating a crash
/// between commits.
async fn seed_interrupted_task(storage: Arc<dyn Storage>) -> rpi_durable::ids::TaskId {
    let opened = open_with_storage(storage, all_tasks()).await;
    let conversation = root(&opened.harness).await;
    let mut spec = Spec::owned("test.count", serde_json::json!({ "phases": 3 }));
    spec.checkpoint = serde_json::json!({ "phase": "step", "count": 1 });
    let id = add_task(&opened.harness, conversation, spec).await;
    let mut record = opened
        .harness
        .get_task(id, &context())
        .await
        .expect("get task")
        .expect("task record");
    record.state = TaskState::Running {
        checkpoint: serde_json::json!({ "phase": "step", "count": 1 }),
    };
    stamp_task(&opened.harness, record).await;
    id
}

async fn assert_resumes(id: rpi_durable::ids::TaskId, storage: Arc<dyn Storage>) {
    let reopened = open_with_storage(storage, all_tasks()).await;
    let record = reopened
        .harness
        .get_task(id, &context())
        .await
        .expect("get task")
        .expect("task record");
    // `open()` rewinds a surviving `running` record to `pending`.
    assert_eq!(record.status(), TaskStatus::Pending);
    let settled = reopened
        .harness
        .wait_for_task(id, &context())
        .await
        .expect("wait for task");
    assert_eq!(terminal_status(&settled.record), Some("completed"));
    // The resumed run reached the terminal phase and reported its final count.
    assert_eq!(settled.result, serde_json::json!({ "count": 3 }));
    reopened.harness.close(&context()).await.expect("close");
}

/// FR-F R1: a killed process (no graceful harness shutdown) resumes over
/// SQLite.
#[tokio::test]
async fn sqlite_harness_resumes_after_a_crash() {
    let dir = TempDir::new("durable-accept-sqlite");
    let path = dir.path_str() + "/durable.db";
    let id = {
        let storage: Arc<dyn Storage> = Arc::new(
            open_node_sqlite_storage(&path, SqliteDatabaseOptions::default())
                .await
                .expect("open sqlite"),
        );
        let id = seed_interrupted_task(Arc::clone(&storage)).await;
        // The crash: the harness is never closed; only the owning storage
        // handle goes away.
        storage.close(&context()).await.expect("close storage");
        id
    };
    let storage: Arc<dyn Storage> = Arc::new(
        open_node_sqlite_storage(&path, SqliteDatabaseOptions::default())
            .await
            .expect("reopen sqlite"),
    );
    assert_resumes(id, storage).await;
}

/// FR-F R1: the same crash/resume anchor over the JSONL backend.
#[tokio::test]
async fn jsonl_harness_resumes_after_a_crash() {
    let dir = TempDir::new("durable-accept-jsonl");
    let directory = dir.path_str() + "/storage";
    let id = {
        let fs: Arc<dyn rpi_durable::fs::FileSystem> = Arc::new(StdFileSystem::new(dir.path()));
        let storage: Arc<dyn Storage> = Arc::new(
            JsonlStorage::open(
                &directory,
                Arc::clone(&fs),
                JsonlStorageOptions { fsync: false },
                &context(),
            )
            .await
            .expect("open jsonl"),
        );
        let id = seed_interrupted_task(Arc::clone(&storage)).await;
        storage.close(&context()).await.expect("close storage");
        id
    };
    let fs: Arc<dyn rpi_durable::fs::FileSystem> = Arc::new(StdFileSystem::new(dir.path()));
    let storage: Arc<dyn Storage> = Arc::new(
        JsonlStorage::open(
            &directory,
            fs,
            JsonlStorageOptions { fsync: false },
            &context(),
        )
        .await
        .expect("reopen jsonl"),
    );
    assert_resumes(id, storage).await;
}
/// Sleeping records retain their deadline across reopen, while already-due
/// records run on the first scheduling pass.
#[tokio::test]
async fn sleeping_tasks_survive_reopen_and_past_deadlines_resume() {
    let dir = TempDir::new("durable-accept-sleep");
    let path = dir.path_str() + "/durable.db";
    let storage: Arc<dyn Storage> = Arc::new(
        open_node_sqlite_storage(&path, SqliteDatabaseOptions::default())
            .await
            .expect("open sqlite"),
    );
    let opened = open_with_storage(Arc::clone(&storage), all_tasks()).await;
    let conversation = root(&opened.harness).await;
    let future = add_task(
        &opened.harness,
        conversation,
        Spec::owned("test.sleeping", serde_json::Value::Null),
    )
    .await;
    opened.harness.resume().expect("start scheduler");
    let future_until = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let record = opened
                .harness
                .get_task(future, &context())
                .await
                .expect("read sleeping task")
                .expect("task record");
            if let TaskState::Sleeping { until, .. } = record.state {
                break until;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("task enters sleeping state");
    let inspection = opened
        .harness
        .inspect(&context())
        .await
        .expect("inspect sleeping task");
    let inspected = inspection
        .tasks
        .iter()
        .find(|task| task.record.id == future)
        .expect("sleeping task inspection");
    assert!(matches!(
        &inspected.state,
        rpi_durable::harness::TaskInspectionState::Sleeping { until } if *until == future_until
    ));
    opened
        .harness
        .close(&context())
        .await
        .expect("close harness");

    let storage: Arc<dyn Storage> = Arc::new(
        open_node_sqlite_storage(&path, SqliteDatabaseOptions::default())
            .await
            .expect("reopen sqlite"),
    );
    let reopened = open_with_storage(Arc::clone(&storage), all_tasks()).await;
    assert_eq!(
        reopened
            .harness
            .get_task(future, &context())
            .await
            .expect("read sleeping task")
            .expect("task record")
            .state,
        TaskState::Sleeping {
            checkpoint: serde_json::json!({ "phase": "after" }),
            until: future_until,
        }
    );
    let sleeping_wake = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let wake = reopened.harness.wake().expect("wake state");
            if !wake.active {
                break wake;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("reopened sleeper becomes idle");
    assert_eq!(sleeping_wake.at, Some(future_until));
    let conversation = root(&reopened.harness).await;
    let due = add_task(
        &reopened.harness,
        conversation,
        Spec::owned("test.sleeping", serde_json::Value::Null),
    )
    .await;
    let mut due_record = reopened
        .harness
        .get_task(due, &context())
        .await
        .expect("read due task")
        .expect("task record");
    due_record.state = TaskState::Sleeping {
        checkpoint: serde_json::json!({ "phase": "after" }),
        until: 0.0,
    };
    stamp_task(&reopened.harness, due_record).await;
    let due_wake = reopened.harness.wake().expect("due wake state");
    assert!(due_wake.active);
    assert_eq!(due_wake.at, None);
    reopened.harness.resume().expect("resume");
    let settled = reopened
        .harness
        .wait_for_task(due, &context())
        .await
        .expect("past deadline resumes");
    assert_eq!(terminal_status(&settled.record), Some("completed"));
    assert_eq!(
        reopened
            .harness
            .get_task(future, &context())
            .await
            .expect("read future sleeper")
            .expect("task record")
            .status(),
        TaskStatus::Sleeping
    );
    assert!(matches!(
        reopened
            .harness
            .abort_task(future, &context())
            .await
            .expect("abort future sleeper"),
        rpi_durable::harness::TaskAbort::Marked
    ));
    let aborted = reopened
        .harness
        .wait_for_task(future, &context())
        .await
        .expect("wait for aborted sleeper");
    assert_eq!(terminal_status(&aborted.record), Some("aborted"));
    reopened
        .harness
        .close(&context())
        .await
        .expect("close reopened");
}

#[tokio::test]
async fn due_sleeping_phase_cannot_step_a_replacement_invocation() {
    let storage: Arc<dyn Storage> = Arc::new(rpi_durable::storage::MemoryStorage::new());
    let opened = open_with_storage(storage, all_tasks()).await;
    let conversation = root(&opened.harness).await;
    let task = add_task(
        &opened.harness,
        conversation,
        Spec::owned("test.due-sleeping", serde_json::Value::Null),
    )
    .await;
    opened.harness.resume().expect("resume");
    let settled = opened
        .harness
        .wait_for_task(task, &context())
        .await
        .expect("wait for due sleeper");
    assert_eq!(terminal_status(&settled.record), Some("completed"));
    opened.harness.close(&context()).await.expect("close");
}

#[tokio::test]
async fn panicking_task_phase_faults_and_releases_waiters() {
    let storage: Arc<dyn Storage> = Arc::new(rpi_durable::storage::MemoryStorage::new());
    let opened = open_with_storage(storage, all_tasks()).await;
    let conversation = root(&opened.harness).await;
    let task = add_task(
        &opened.harness,
        conversation,
        Spec::owned("test.panicking", serde_json::Value::Null),
    )
    .await;
    opened.harness.resume().expect("resume");
    let settled = opened
        .harness
        .wait_for_task(task, &context())
        .await
        .expect("wait for panicked task");
    assert_eq!(terminal_status(&settled.record), Some("faulted"));
    opened.harness.close(&context()).await.expect("close");
}

/// FR-F R2: the storage conformance suite still passes over SQLite and JSONL
/// (the V17-13 anchor re-run from this task's acceptance).
#[tokio::test]
async fn storage_conformance_recheck() {
    let result = rpi_durable::testing::run_storage_conformance(|| async {
        Ok(Arc::new(rpi_durable::storage::MemoryStorage::new()) as Arc<dyn Storage>)
    })
    .await;
    assert!(result.is_ok(), "memory conformance: {result:?}");
}
