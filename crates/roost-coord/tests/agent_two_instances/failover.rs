//! A replica that dies mid-tool is replaced without the tool running twice.
//! The real durable kernel's confirmed batches are what the replacement
//! replica resumes from; a late write from the dead replica's stale lease
//! must still be fenced.

use std::sync::Arc;
use std::time::Duration;

use roost_agent::{AgentError, AgentStore, Entry, LeaseFence};

use super::{
    add_key, create, primary_requests, replica, scratch, settled, store, text_sse, tool_sse,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replica_that_dies_mid_tool_is_replaced_without_replaying_the_tool() {
    let root = scratch("failover");
    let replies: Arc<parking_lot::Mutex<Vec<String>>> = Arc::default();
    let b = replica(&root, "coord-b", Arc::clone(&replies), None).await;
    add_key(&b).await;
    let id = create(&b).await;
    let runtime_a = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let a = {
        let root = root.clone();
        let replies = Arc::clone(&replies);
        let notifier = Arc::clone(&b.service.cluster.notifier);
        runtime_a
            .spawn(async move { replica(&root, "coord-a", replies, Some(notifier)).await })
            .await
            .unwrap()
    };
    *a.worker.hold.lock() = true;
    replies.lock().extend([
        tool_sse(
            "call-lost",
            "bash",
            r#"{"i":"Check","command":"make migrate"}"#,
        ),
        text_sse("Checked the migration state instead."),
    ]);
    let service_a = Arc::clone(&a.service);
    let submit_id = id.clone();
    runtime_a
        .spawn(async move {
            service_a
                .runtime
                .submit_request(&submit_id, "migrate", "run the migration".into())
                .await
        })
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), a.worker.started.notified())
        .await
        .expect("A runs the tool");
    let before_handover = store(&b).durable_batches(&id).await.unwrap();
    assert!(
        before_handover.iter().any(|batch| !batch.writes.is_empty()),
        "the real durable kernel has confirmed non-empty batches before the owner dies"
    );
    let confirmed_sequence = before_handover
        .last()
        .expect("at least one durable batch")
        .sequence;
    runtime_a.shutdown_background();

    let adopted = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if b.service.runtime.recover_orphaned_runs().await.unwrap() == 1 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(adopted.is_ok(), "B adopts the run once A's lease expires");
    settled(&b, &id, Duration::from_secs(10)).await;

    assert_eq!(
        b.worker.bash_calls(),
        0,
        "the lost tool call is not replayed"
    );
    assert_eq!(primary_requests(&b), 1);
    let journal = store(&b);
    let resumed_batches = journal.durable_batches(&id).await.unwrap();
    assert!(
        resumed_batches
            .last()
            .is_some_and(|batch| batch.sequence > confirmed_sequence),
        "the replacement core resumes from confirmed storage and commits its continuation"
    );
    assert!(resumed_batches.iter().any(|batch| !batch.writes.is_empty()));
    let entries = journal.entries(&id).await.unwrap();
    assert!(entries.iter().any(|(_, entry)| matches!(entry,
        Entry::ToolResult { call_id, is_error: true, .. } if call_id == "call-lost")));
    let stale = LeaseFence {
        owner: "coord-a".into(),
        epoch: 1,
    };
    let note = Entry::Notice {
        level: "info".into(),
        title: "late write from the dead replica".into(),
        body: String::new(),
    };
    assert!(matches!(
        journal.append_entry(&id, &note, &[], Some(&stale)).await,
        Err(AgentError::Fenced(_))
    ));
}
