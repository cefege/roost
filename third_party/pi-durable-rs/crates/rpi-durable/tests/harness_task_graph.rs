//! Harness task-graph integration tests (port of
//! `packages/durable/test/harness-task-graph.test.ts` @ `abe508e1b`).
//!
//! Scoped to what does not need the scheduler: tasks are created directly on the
//! Session line, and the graph is observed through the Harness composition.

mod harness_support;

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use harness_support::{context, open_recording};
use rpi_durable::JsonValue;
use rpi_durable::harness::{HarnessImpl, RootOptions};
use rpi_durable::ids::{ConversationId, TaskId};
use rpi_durable::session::{TaskCreate, TaskOwnership, commit_fn};
use rpi_durable::types::{TaskOutcome, TaskRecord, TaskState};

async fn root(harness: &Arc<HarnessImpl>) -> ConversationId {
    harness
        .root(&context(), RootOptions::default())
        .await
        .expect("root")
        .id()
}

async fn create_task(
    harness: &Arc<HarnessImpl>,
    conversation: ConversationId,
    kind: &str,
) -> TaskId {
    let kind = kind.to_string();
    let change = commit_fn(move |tx| {
        let kind = kind.clone();
        Box::pin(async move {
            tx.create_task(TaskCreate {
                kind,
                version: 1,
                input: JsonValue::Null,
                checkpoint: serde_json::json!({ "phase": "prepare" }),
                ownership: TaskOwnership::Conversation(conversation),
                conversation_id: Some(conversation),
                background: false,
            })
            .await
        })
    });
    harness
        .session()
        .commit(change, &context())
        .await
        .expect("create task")
}

async fn record(harness: &Arc<HarnessImpl>, id: TaskId) -> TaskRecord {
    harness
        .session()
        .storage()
        .task(id, &context())
        .await
        .expect("task")
        .expect("present")
}

async fn set_state(harness: &Arc<HarnessImpl>, id: TaskId, state: TaskState) {
    let record = record(harness, id).await;
    let change = commit_fn(move |tx| {
        let next = TaskRecord {
            state: state.clone(),
            ..record.clone()
        };
        Box::pin(async move { tx.set_task(next) })
    });
    harness
        .session()
        .commit(change, &context())
        .await
        .expect("set task");
}

fn node(harness_value: &JsonValue, id: TaskId) -> JsonValue {
    harness_value["tasks"][id.get().to_string()].clone()
}

#[tokio::test]
async fn follows_a_live_task_through_status_changes() {
    let (harness, _scheduler) = open_recording().await;
    let context = context();
    let conversation = root(&harness).await;
    let task = create_task(&harness, conversation, "pi.generation").await;
    let graph = harness.task_graph(&context).await.expect("graph");
    let pending = node(&graph.value(), task);
    assert_eq!(pending["state"]["status"], "pending");
    assert_eq!(pending["kind"], "pi.generation");
    assert_eq!(pending["conversationId"], conversation.get());

    set_state(
        &harness,
        task,
        TaskState::Waiting {
            checkpoint: serde_json::json!({ "phase": "tools" }),
            on: vec![],
            policy: rpi_durable::types::JoinPolicy::AllSettled,
        },
    )
    .await;
    let waiting = node(&graph.value(), task);
    assert_eq!(waiting["state"]["status"], "waiting");
    assert_eq!(waiting["state"]["phase"], "tools");
    set_state(
        &harness,
        task,
        TaskState::Sleeping {
            checkpoint: serde_json::json!({ "phase": "retry" }),
            until: 61_000.0,
        },
    )
    .await;
    let sleeping = node(&graph.value(), task);
    assert_eq!(sleeping["state"]["status"], "sleeping");
    assert_eq!(sleeping["state"]["phase"], "retry");
    assert_eq!(sleeping["state"]["until"], 61_000.0);

    set_state(
        &harness,
        task,
        TaskState::Terminal {
            outcome: TaskOutcome::Completed {
                result: JsonValue::Null,
            },
        },
    )
    .await;
    // A terminal task leaves the graph.
    assert!(node(&graph.value(), task).is_null());
    graph.dispose();
}

#[tokio::test]
async fn abort_marks_and_owned_conversations_appear_in_the_graph() {
    let (harness, _scheduler) = open_recording().await;
    let context = context();
    let conversation = root(&harness).await;
    let task = create_task(&harness, conversation, "pi.generation").await;
    // A conversation owned by the task in the same commit.
    let change = commit_fn(move |tx| {
        Box::pin(async move {
            Ok(tx
                .create_conversation(rpi_durable::session::ConversationOwnership::Task(task))
                .await?
                .id)
        })
    });
    let owned = harness
        .session()
        .commit(change, &context)
        .await
        .expect("owned conversation");
    let record = record(&harness, task).await;
    let marked = TaskRecord {
        abort_requested: true,
        ..record
    };
    let change = commit_fn(move |tx| {
        let marked = marked.clone();
        Box::pin(async move { tx.set_task(marked) })
    });
    harness
        .session()
        .commit(change, &context)
        .await
        .expect("mark abort");
    let graph = harness.task_graph(&context).await.expect("graph");
    let value = node(&graph.value(), task);
    assert_eq!(value["abortRequested"], true);
    assert_eq!(value["conversations"], serde_json::json!([owned.get()]));
    graph.dispose();
}

#[tokio::test]
async fn watch_reports_the_latest_committed_revision() {
    let (harness, _scheduler) = open_recording().await;
    let context = context();
    let conversation = root(&harness).await;
    let task = create_task(&harness, conversation, "pi.generation").await;
    let watch = harness.watch_task_graph(&context).await.expect("watch");
    let log: Arc<Mutex<Vec<JsonValue>>> = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&log);
    let listener = rpi_durable::session::watch_listener(move |value, _ops, _context| {
        let captured = Arc::clone(&captured);
        Box::pin(async move {
            captured
                .lock()
                .expect("log")
                .push(value.map(JsonValue::Object).unwrap_or(JsonValue::Null));
            Ok(())
        })
    });
    watch.start(listener).expect("start");
    set_state(
        &harness,
        task,
        TaskState::Running {
            checkpoint: serde_json::json!({ "phase": "request" }),
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let latest = log
        .lock()
        .expect("log")
        .last()
        .cloned()
        .unwrap_or(JsonValue::Null);
    assert_eq!(node(&latest, task)["state"]["status"], "running");
    assert_eq!(node(&latest, task)["state"]["phase"], "request");
    let _ = watch.stop().await;
}

#[tokio::test]
async fn a_noop_commit_publishes_no_revision() {
    let (harness, _scheduler) = open_recording().await;
    let context = context();
    let conversation = root(&harness).await;
    create_task(&harness, conversation, "pi.generation").await;
    let graph = harness.task_graph(&context).await.expect("graph");
    let value = graph.value();
    let change = commit_fn(|_tx| Box::pin(async move { Ok::<(), rpi_durable::SessionError>(()) }));
    harness
        .session()
        .commit(change, &context)
        .await
        .expect("noop");
    assert_eq!(graph.value(), value);
    graph.dispose();
}
