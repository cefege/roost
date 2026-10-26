//! Two coordinator replicas over one database: a conversation's run has one
//! owner at a time, input admitted on either replica reaches it, an abort on
//! either stops it, and a replica that dies mid-tool is replaced without the
//! tool running twice.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_cluster_support;
mod db_support;
#[path = "agent_two_instances/failover.rs"]
mod failover;
#[path = "agent_two_instances/inbox.rs"]
mod inbox;

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agent_cluster_support::{
    MODEL, Replica, add_key, replica, settled, text_sse, tool_sse, user_texts,
};
use roost_agent::{AgentStore, ChatSink, Entry, NewConversation};
use roost_coord::agent::store::CoordAgentStore;
use roost_protocol::wire::agent_chat::{AgentRunState, ChatEvent, ModelRef, UsageTotals};

fn scratch(label: &str) -> std::path::PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "roost-agent-cluster-{label}-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

async fn pair(label: &str) -> (Replica, Replica, std::path::PathBuf) {
    let root = scratch(label);
    let replies = Arc::default();
    let a = replica(&root, "coord-a", Arc::clone(&replies), None).await;
    let b = replica(
        &root,
        "coord-b",
        replies,
        Some(Arc::clone(&a.service.cluster.notifier)),
    )
    .await;
    add_key(&a).await;
    (a, b, root)
}

async fn create(replica: &Replica) -> String {
    replica
        .service
        .runtime
        .create_conversation(NewConversation {
            worker_fp: "worker-fp".into(),
            worker_label: "desktop".into(),
            worker_os: "linux".into(),
            cwd: "/repo".into(),
            title: Some("cluster".into()),
            model: Some(ModelRef {
                provider: "anthropic".into(),
                model_id: MODEL.into(),
            }),
            thinking_level: Some("off".into()),
        })
        .await
        .unwrap()
        .id
}

fn store(replica: &Replica) -> CoordAgentStore {
    CoordAgentStore::new(replica.db.clone())
}

/// The agent loop's own model requests: the ones that offer tools. Judges and
/// classifiers a turn also makes are not counted.
fn primary_bodies(replica: &Replica) -> Vec<serde_json::Value> {
    replica
        .fake
        .bodies
        .lock()
        .iter()
        .filter(|body| {
            body["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty())
        })
        .cloned()
        .collect()
}

fn primary_requests(replica: &Replica) -> usize {
    primary_bodies(replica).len()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_boot_recovery_asks_the_model_once_per_interrupted_conversation() {
    let (a, b, _root) = pair("recover").await;
    let mut ids = Vec::new();
    for index in 0..3 {
        let id = create(&a).await;
        let journal = store(&a);
        let user = journal
            .append_entry(
                &id,
                &Entry::User {
                    text: format!("interrupted {index}"),
                    loop_repeat: false,
                    item_id: None,
                },
                &[],
                None,
            )
            .await
            .unwrap();
        let intent = Entry::RunIntent {
            active: true,
            through_seq: user,
            aborted: false,
            yielded: false,
        };
        journal.append_entry(&id, &intent, &[], None).await.unwrap();
        let mut record = journal.conversation(&id).await.unwrap().unwrap();
        record.run_state = AgentRunState::Running;
        journal
            .save_conversation(&record, record.revision)
            .await
            .unwrap();
        ids.push(id);
    }
    a.fake
        .replies
        .lock()
        .extend((0..3).map(|index| text_sse(&format!("recovered {index}"))));

    tokio::join!(a.service.recover(), b.service.recover());
    for id in &ids {
        settled(&a, id, Duration::from_secs(10)).await;
    }
    let requests = primary_requests(&a) + primary_requests(&b);
    assert_eq!(
        requests, 3,
        "each interrupted run is resumed by exactly one replica"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn input_admitted_on_one_replica_reaches_the_run_another_owns() {
    let (a, b, _root) = pair("steer").await;
    let id = create(&a).await;
    *a.worker.hold.lock() = true;
    a.fake.replies.lock().extend([
        tool_sse("call-wait", "bash", r#"{"i":"Wait","command":"sleep 1"}"#),
        text_sse("Both requests handled."),
    ]);
    a.service
        .runtime
        .submit_request(&id, "first-request", "first".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), a.worker.started.notified())
        .await
        .expect("A runs the tool");

    b.service
        .runtime
        .submit_request(&id, "second-request", "second".into())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    a.worker.release.notify_one();
    settled(&a, &id, Duration::from_secs(10)).await;

    assert_eq!(primary_requests(&b), 0, "B never runs A's conversation");
    let bodies = primary_bodies(&a);
    assert_eq!(bodies.len(), 2, "one tool round and one reply: {bodies:#?}");
    assert!(user_texts(&bodies[1]).contains(&"second".to_owned()));
    let transcript = a.service.runtime.transcript(&id).await.unwrap();
    let replies = serde_json::to_string(&transcript.items).unwrap();
    assert_eq!(replies.matches("Both requests handled.").count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_abort_on_one_replica_stops_the_run_another_owns() {
    let (a, b, _root) = pair("abort").await;
    let id = create(&a).await;
    *a.worker.hold.lock() = true;
    a.fake.replies.lock().push(tool_sse(
        "call-wait",
        "bash",
        r#"{"i":"Wait","command":"sleep 60"}"#,
    ));
    a.service
        .runtime
        .submit_request(&id, "long-request", "run for a minute".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), a.worker.started.notified())
        .await
        .expect("A runs the tool");

    let asked = Instant::now();
    assert!(b.service.runtime.abort(&id).await.unwrap());
    settled(&a, &id, Duration::from_secs(2)).await;
    assert!(asked.elapsed() < Duration::from_secs(2));
    assert_eq!(primary_requests(&a), 1, "the aborted run asks nothing more");
    let entries = store(&a).entries(&id).await.unwrap();
    let last_intent = entries.iter().rev().find_map(|(_, entry)| match entry {
        Entry::RunIntent {
            active, aborted, ..
        } => Some((*active, *aborted)),
        _ => None,
    });
    assert_eq!(last_intent, Some((false, true)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retried_submit_is_admitted_once() {
    let (a, b, _root) = pair("dedup").await;
    let id = create(&a).await;
    a.fake.replies.lock().push(text_sse("Hello once."));
    for _ in 0..2 {
        b.service
            .runtime
            .submit_request(&id, "same-request", "hello".into())
            .await
            .unwrap();
    }
    settled(&b, &id, Duration::from_secs(10)).await;
    let users = store(&a)
        .entries(&id)
        .await
        .unwrap()
        .into_iter()
        .filter(|(_, entry)| matches!(entry, Entry::User { text, .. } if text == "hello"))
        .count();
    assert_eq!(users, 1);
    assert_eq!(primary_requests(&a) + primary_requests(&b), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn durable_chat_events_relay_across_replicas_and_continue_after_restart() {
    let (a, b, root) = pair("chat-cursors").await;
    let conversation_id = create(&a).await;
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let _subscription = b.buses.agent_chat_bus.subscribe(move |update| {
        let _ = sender.send(update.clone());
    });
    let (snapshot_cursor, _) = b.service.sink.snapshot(&conversation_id).await.unwrap();

    a.fake.replies.lock().push(text_sse("Cursor stream."));
    a.service
        .runtime
        .submit_request(&conversation_id, "chat-run", "hello".into())
        .await
        .unwrap();
    settled(&a, &conversation_id, Duration::from_secs(10)).await;
    tokio::time::sleep(Duration::from_millis(80)).await;
    let (finished_cursor, _) = a.service.sink.snapshot(&conversation_id).await.unwrap();
    let mut cursors = Vec::new();
    while cursors.last().copied().unwrap_or(snapshot_cursor) < finished_cursor {
        let update = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
            .await
            .unwrap()
            .expect("replica B receives chat batch");
        assert_eq!(update.conversation_id, conversation_id);
        cursors.push(update.seq);
    }
    assert_eq!(cursors.first().copied(), Some(snapshot_cursor + 1));
    assert!(cursors.windows(2).all(|window| window[1] == window[0] + 1));

    a.service.leave_cluster().await;
    let restarted = replica(
        &root,
        "coord-a-restarted",
        Arc::default(),
        Some(Arc::clone(&b.service.cluster.notifier)),
    )
    .await;
    let (before_restart_event, _) = restarted
        .service
        .sink
        .snapshot(&conversation_id)
        .await
        .unwrap();
    restarted
        .service
        .sink
        .events(
            &conversation_id,
            vec![ChatEvent::Usage {
                usage: UsageTotals::default(),
            }],
        )
        .await;
    let update = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .expect("restarted replica chat batch reaches B");
    assert_eq!(update.seq, before_restart_event + 1);
    restarted.service.leave_cluster().await;
    b.service.leave_cluster().await;
}
