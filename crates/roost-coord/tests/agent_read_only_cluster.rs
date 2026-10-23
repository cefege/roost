//! A persisted conversation setting reaches another coordinator without waking a run.

mod agent_cluster_support;
mod db_support;

use std::sync::Arc;
use std::time::Duration;

use agent_cluster_support::{MODEL, Replica, add_key, replica};
use parking_lot::Mutex;
use roost_protocol::wire::agent_chat::ModelRef;
use tokio::sync::mpsc;

fn scratch() -> std::path::PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "roost-agent-read-only-cluster-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

async fn pair(root: &std::path::Path) -> (Replica, Replica) {
    let replies = Arc::new(Mutex::new(Vec::new()));
    let first = replica(root, "read-only-a", Arc::clone(&replies), None).await;
    let second = replica(
        root,
        "read-only-b",
        replies,
        Some(Arc::clone(&first.service.cluster.notifier)),
    )
    .await;
    add_key(&first).await;
    (first, second)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_only_update_reaches_peer_summary_without_waking_a_run() {
    let root = scratch();
    let (first, second) = pair(&root).await;
    let before = first
        .service
        .runtime
        .create_conversation(roost_agent::NewConversation {
            worker_fp: "worker-fp".into(),
            worker_label: "desktop".into(),
            worker_os: "linux".into(),
            cwd: "/repo".into(),
            title: Some("cluster read-only".into()),
            model: Some(ModelRef {
                provider: "anthropic".into(),
                model_id: MODEL.into(),
            }),
            thinking_level: Some("off".into()),
        })
        .await
        .unwrap();
    let (updates, mut received) = mpsc::unbounded_channel();
    let _subscription = second
        .buses
        .agent_conversation_bus
        .subscribe(move |update| {
            let _ = updates.send(update.clone());
        });
    let previous_updated_ms = before.updated_ms;

    let changed = first
        .service
        .runtime
        .set_read_only(&before.id, true)
        .await
        .unwrap();
    assert!(changed.updated_ms > previous_updated_ms);

    let peer_update = tokio::time::timeout(Duration::from_secs(5), received.recv())
        .await
        .unwrap()
        .expect("peer receives the refreshed conversation summary");
    assert_eq!(peer_update.conversation_id, before.id);
    let peer_summary = peer_update
        .conversation
        .expect("conversation remains active");
    assert!(peer_summary.updated_ms >= changed.updated_ms);
    let inspection = second.service.runtime.inspect(&before.id).await.unwrap();
    assert!(
        inspection.read_only,
        "peer reads the persisted configuration"
    );

    assert!(first.fake.bodies.lock().is_empty());
    assert!(second.fake.bodies.lock().is_empty());
    first.service.leave_cluster().await;
    second.service.leave_cluster().await;
    std::fs::remove_dir_all(root).unwrap();
}
