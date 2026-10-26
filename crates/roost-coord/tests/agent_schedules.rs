//! Scheduled messages (`/schedule`) at the coordinator's SQL store: delivery
//! is exactly-once across two replicas sweeping the same due row, a recurring
//! schedule advances and survives a store reopen, `/schedule cancel` prevents
//! a later sweep from delivering, and conversation deletion cascades its rows.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_cluster_support;
mod db_support;

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agent_cluster_support::{Replica, add_key, replica, settled, text_sse};
use parking_lot::Mutex;
use roost_agent::schedules::ScheduleRow;
use roost_agent::traits::AgentStore;
use roost_agent::{ConversationRecord, Entry, Mode, NewConversation};
use roost_coord::agent::store::CoordAgentStore;
use roost_protocol::wire::agent_chat::{AgentRunState, ModelRef};

fn scratch(label: &str) -> std::path::PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "roost-agent-schedules-{label}-{}-{stamp}",
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
            title: Some("schedules".into()),
            model: Some(ModelRef {
                provider: "anthropic".into(),
                model_id: agent_cluster_support::MODEL.into(),
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

fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

fn conversation_row(id: &str) -> ConversationRecord {
    ConversationRecord {
        id: id.to_owned(),
        title: "title".to_owned(),
        worker_fp: "worker-fp".to_owned(),
        worker_label: "desktop".to_owned(),
        worker_os: "linux".to_owned(),
        cwd: "/work".to_owned(),
        model: None,
        thinking_level: "medium".to_owned(),
        mode: Mode::Normal,
        pre_plan_model: None,
        parent_id: None,
        agent: None,
        advisor: None,
        read_only: false,
        run_state: AgentRunState::Idle,
        error: None,
        created_ms: 1,
        updated_ms: 1,
        revision: 0,
    }
}

/// Model requests the loop itself makes (those offering tools): judges and
/// classifiers a turn also makes are not counted. Mirrors
/// `agent_two_instances::primary_requests`.
fn primary_requests(replica: &Replica) -> usize {
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
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_due_schedule_delivers_exactly_once_across_two_racing_replicas() {
    let (a, b, _root) = pair("dedup").await;
    let id = create(&a).await;
    a.fake.replies.lock().push(text_sse("pong"));
    let row = ScheduleRow {
        id: "s-dedup001".to_owned(),
        text: "ping".to_owned(),
        due_ms: now_ms() - 1_000,
        every_ms: None,
        fired: 0,
        created_ms: now_ms(),
    };
    store(&a).set_schedule(&id, &row).await.unwrap();

    let (fired_a, fired_b) = tokio::join!(
        roost_agent::schedules::deliver_due(&a.service.runtime),
        roost_agent::schedules::deliver_due(&b.service.runtime),
    );
    assert_eq!(
        fired_a.unwrap() + fired_b.unwrap(),
        1,
        "exactly one replica's sweep wins the advance; the other's submit is a duplicate no-op"
    );

    settled(&a, &id, Duration::from_secs(10)).await;
    let users = store(&a)
        .entries(&id)
        .await
        .unwrap()
        .into_iter()
        .filter(|(_, entry)| matches!(entry, Entry::User { text, .. } if text == "ping"))
        .count();
    assert_eq!(
        users, 1,
        "the schedule's text lands as exactly one user turn"
    );
    assert_eq!(
        primary_requests(&a) + primary_requests(&b),
        1,
        "the model is asked exactly once"
    );
    assert!(
        store(&a).schedules(&id).await.unwrap().is_empty(),
        "the fired one-shot row is gone"
    );
}

#[tokio::test]
async fn a_recurring_schedule_advances_its_due_ms_and_survives_reopen() {
    let root = scratch("recur");
    let conversation = conversation_row("recurring");
    let due = now_ms() - 1_800_000; // 30 minutes behind, on a 10-minute cadence
    {
        let db = db_support::open_test_database(&root)
            .await
            .expect("open database");
        let coord_store = CoordAgentStore::new(db);
        coord_store
            .save_conversation(&conversation, 0)
            .await
            .unwrap();
        let row = ScheduleRow {
            id: "s-recur0001".to_owned(),
            text: "tick".to_owned(),
            due_ms: due,
            every_ms: Some(600_000),
            fired: 0,
            created_ms: now_ms(),
        };
        coord_store
            .set_schedule(&conversation.id, &row)
            .await
            .unwrap();
        let advanced = coord_store
            .advance_schedule(&conversation.id, &row.id, due)
            .await
            .unwrap();
        assert!(advanced);
        let rows = coord_store.schedules(&conversation.id).await.unwrap();
        assert_eq!(
            rows.len(),
            1,
            "a recurring schedule's row survives its own fire"
        );
        assert_eq!(rows[0].fired, 1);
        assert!(
            rows[0].due_ms > now_ms(),
            "the next due time is strictly in the future, catching up without bursting"
        );
    }

    let reopened = db_support::open_test_database(&root)
        .await
        .expect("reopen database");
    let reopened_store = CoordAgentStore::new(reopened);
    let rows = reopened_store.schedules(&conversation.id).await.unwrap();
    assert_eq!(rows.len(), 1, "the advanced row persisted across reopen");
    assert_eq!(rows[0].fired, 1);
}

#[tokio::test]
async fn schedule_cancel_removes_the_row_and_a_later_sweep_delivers_nothing() {
    let root = scratch("cancel");
    let db = db_support::open_test_database(&root)
        .await
        .expect("open database");
    let coord_store = CoordAgentStore::new(db);
    let conversation = conversation_row("cancel-target");
    coord_store
        .save_conversation(&conversation, 0)
        .await
        .unwrap();
    let row = ScheduleRow {
        id: "s-cancel001".to_owned(),
        text: "should not send".to_owned(),
        due_ms: now_ms() - 1_000,
        every_ms: None,
        fired: 0,
        created_ms: now_ms(),
    };
    coord_store
        .set_schedule(&conversation.id, &row)
        .await
        .unwrap();

    let removed = coord_store
        .remove_schedule(&conversation.id, &row.id)
        .await
        .unwrap();
    assert!(removed);
    assert!(
        coord_store
            .schedules(&conversation.id)
            .await
            .unwrap()
            .is_empty()
    );

    let due = coord_store.due_schedules(now_ms()).await.unwrap();
    assert!(
        due.iter().all(|(_, candidate)| candidate.id != row.id),
        "a later sweep finds nothing to deliver for the cancelled schedule"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conversation_deletion_removes_its_schedule_rows() {
    let root = scratch("delete");
    let a = replica(&root, "coord-a", Arc::new(Mutex::new(Vec::new())), None).await;
    let id = create(&a).await;
    let row = ScheduleRow {
        id: "s-del0001".to_owned(),
        text: "orphaned".to_owned(),
        due_ms: now_ms() + 3_600_000,
        every_ms: None,
        fired: 0,
        created_ms: now_ms(),
    };
    store(&a).set_schedule(&id, &row).await.unwrap();
    assert_eq!(store(&a).schedules(&id).await.unwrap().len(), 1);

    a.service.runtime.delete_conversation(&id).await.unwrap();

    assert!(
        store(&a).schedules(&id).await.unwrap().is_empty(),
        "deleting the conversation cascades its schedule rows"
    );
}
