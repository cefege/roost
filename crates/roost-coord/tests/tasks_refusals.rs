// The task queue's refusals and its boundaries: what the five `Tasks*` methods
// refuse, what they accept at the edge, and the auth level each one enforces.
//
// The bus subscriber here stands in for a Sync socket: `sync_ws::feed` attaches
// to `task_bus` and nothing else, so a publication this file can observe is a
// publication every connected queue view receives. That is the seam the incident
// in `docs/FAILURE-INDEX.md` ("task state changes invisible to other browsers")
// broke, and it is why most of what follows asserts on what ARRIVED rather than
// only on what the handler returned.
//
// `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
// test is its own crate rather than a module of one, so the exemption has to be
// stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod tasks_support;

use std::sync::Arc;
use std::time::Duration;

use roost_coord::coord_core::CoordCore;
use roost_coord::coord_core::boot_facts::BootFacts;
use roost_coord::services::CoordServices;
use roost_coord::sessions::tasks::{
    handle_tasks_cancel, handle_tasks_enqueue, handle_tasks_list, handle_tasks_next_pending,
    handle_tasks_set_state, new_task_id,
};
use sqlx::AssertSqlSafe;
use tasks_support::{
    DEVICE_FP, OTHER_DEVICE_FP, TasksFixture, WORKER_FP, claim_next, device, machine,
};

#[tokio::test]
async fn a_malformed_payload_is_refused_and_stores_nothing() {
    let fixture = TasksFixture::new("bad-payload").await;
    let refused = handle_tasks_enqueue(
        &fixture.core,
        &device(DEVICE_FP),
        roost_proto::TasksEnqueueRequest {
            payload_json: "{not json".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect_err("a payload that is not JSON");
    assert_eq!(refused.code(), connectrpc::ErrorCode::InvalidArgument);
    assert!(refused.message().contains("invalid payloadJson"));

    let count: i64 = sqlx::query_scalar(AssertSqlSafe("SELECT COUNT(*) FROM tasks"))
        .fetch_one(fixture.database.pool())
        .await
        .expect("a count");
    assert_eq!(count, 0, "a refused enqueue wrote no row");
    assert!(fixture.deltas().is_empty());
}

#[tokio::test]
async fn an_enqueue_honours_a_claim_ttl_and_a_completion_check() {
    let fixture = TasksFixture::new("enqueue-shape").await;
    let task = handle_tasks_enqueue(
        &fixture.core,
        &device(DEVICE_FP),
        roost_proto::TasksEnqueueRequest {
            payload_json: r#"{"prompt":"build"}"#.to_owned(),
            completion_check: Some("test -f out.bin".to_owned()),
            claim_ttl_ms: Some(60_000),
            ..Default::default()
        },
    )
    .await
    .expect("an enqueued task")
    .body
    .task
    .into_option()
    .expect("a task");
    assert_eq!(task.claim_ttl_ms, 60_000);
    assert_eq!(task.completion_check.as_deref(), Some("test -f out.bin"));
    assert_eq!(task.state, "pending");
    assert!(task.claimed_at_ms.is_none() && task.claimed_by.is_none());
    assert!(task.finished_at_ms.is_none() && task.result_json.is_none());

    let defaulted = handle_tasks_enqueue(
        &fixture.core,
        &device(DEVICE_FP),
        roost_proto::TasksEnqueueRequest {
            payload_json: "{}".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect("an enqueued task")
    .body
    .task
    .into_option()
    .expect("a task");
    assert_eq!(defaulted.claim_ttl_ms, 900_000, "fifteen minutes");
}

#[tokio::test]
async fn a_state_change_without_a_result_leaves_a_stored_one_in_place() {
    let fixture = TasksFixture::new("result-coalesce").await;
    fixture.enqueue("{}").await;
    let id = claim_next(&fixture).await.id;
    handle_tasks_set_state(
        &fixture.core,
        &device(DEVICE_FP),
        roost_proto::TasksSetStateRequest {
            id: id.clone(),
            state: "running".to_owned(),
            result_json: Some(r#"{"step":1}"#.to_owned()),
            ..Default::default()
        },
    )
    .await
    .expect("a running task");
    let running = handle_tasks_set_state(
        &fixture.core,
        &device(DEVICE_FP),
        roost_proto::TasksSetStateRequest {
            id,
            state: "done".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect("a finished task")
    .body
    .task
    .into_option()
    .expect("a task");
    assert_eq!(
        running.result_json.as_deref(),
        Some(r#"{"step":1}"#),
        "an absent optional field is not an instruction to clear"
    );
    assert!(running.finished_at_ms.is_some());
}

#[tokio::test]
async fn a_non_terminal_state_does_not_stamp_a_finish() {
    let fixture = TasksFixture::new("non-terminal").await;
    fixture.enqueue("{}").await;
    let id = claim_next(&fixture).await.id;
    let running = handle_tasks_set_state(
        &fixture.core,
        &device(DEVICE_FP),
        roost_proto::TasksSetStateRequest {
            id,
            state: "running".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect("a running task")
    .body
    .task
    .into_option()
    .expect("a task");
    assert_eq!(running.state, "running");
    assert!(running.finished_at_ms.is_none());
}

#[tokio::test]
async fn a_worker_credential_cannot_reach_the_queue() {
    let fixture = TasksFixture::new("auth").await;
    for refused in [
        handle_tasks_list(
            &fixture.core,
            &machine(WORKER_FP),
            roost_proto::TasksListRequest::default(),
        )
        .await
        .expect_err("a machine is not a browser"),
        handle_tasks_enqueue(
            &fixture.core,
            &machine(WORKER_FP),
            roost_proto::TasksEnqueueRequest {
                payload_json: "{}".to_owned(),
                ..Default::default()
            },
        )
        .await
        .expect_err("a machine is not a browser"),
        handle_tasks_next_pending(
            &fixture.core,
            &machine(WORKER_FP),
            roost_proto::TasksNextPendingRequest::default(),
        )
        .await
        .expect_err("a machine is not a browser"),
        handle_tasks_set_state(
            &fixture.core,
            &machine(WORKER_FP),
            roost_proto::TasksSetStateRequest {
                id: "11111111-1111-4111-8111-111111111111".to_owned(),
                state: "done".to_owned(),
                ..Default::default()
            },
        )
        .await
        .expect_err("a machine is not a browser"),
        handle_tasks_cancel(
            &fixture.core,
            &machine(WORKER_FP),
            roost_proto::TasksCancelRequest {
                id: "11111111-1111-4111-8111-111111111111".to_owned(),
                ..Default::default()
            },
        )
        .await
        .expect_err("a machine is not a browser"),
    ] {
        assert_eq!(refused.code(), connectrpc::ErrorCode::Unauthenticated);
        assert_eq!(
            refused
                .response_headers()
                .get("x-roost-auth-layer")
                .map(|value| value.as_bytes().as_ref()),
            Some(&b"device"[..]),
            "the marker is how a client tells this from an expired session"
        );
    }
}

#[test]
fn minted_task_ids_are_distinct_and_accepted_by_the_id_brand() {
    let ids: Vec<String> = (0..64).map(|_| new_task_id("epoch-a", 1_000)).collect();
    let unique: std::collections::HashSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "two enqueues never share an id");
    for id in &ids {
        roost_protocol::wire::TaskId::try_from(id.as_str())
            .unwrap_or_else(|error| panic!("{id} is not a task id: {error}"));
    }
    // A second coordinator process, same millisecond, different boot identity.
    let other = new_task_id("epoch-b", 1_000);
    assert!(
        !ids.contains(&other),
        "the boot identity is part of the id, so two coordinators cannot collide"
    );
}

#[tokio::test]
async fn an_unbooted_coordinator_refuses_an_enqueue_rather_than_guessing_a_dashboard() {
    let root = std::env::temp_dir().join(format!(
        "roost-tasks-unbooted-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a scratch directory");
    let database = roost_coord::db::open(&root.join("coord.db"))
        .await
        .expect("a migrated database");
    roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&database, 1_000)
        .await
        .expect("the self-hosted tenant");
    let core = CoordCore::new(Arc::new(CoordServices::new(database)));
    let refused = handle_tasks_enqueue(
        &core,
        &device(DEVICE_FP),
        roost_proto::TasksEnqueueRequest {
            payload_json: "{}".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect_err("an unbooted coordinator has no dashboard to scope the row to");
    assert_eq!(refused.code(), connectrpc::ErrorCode::Internal);
    assert!(
        refused
            .message()
            .contains("coordinator booted without tenant")
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn two_devices_never_claim_the_same_task() {
    let fixture = TasksFixture::new("claim-race").await;
    fixture
        .seed(
            "11111111-1111-4111-8111-111111111111",
            "pending",
            1_000,
            None,
        )
        .await;

    let mine = tokio::spawn({
        let core = fixture.core.clone();
        async move {
            handle_tasks_next_pending(
                &core,
                &device(DEVICE_FP),
                roost_proto::TasksNextPendingRequest::default(),
            )
            .await
        }
    });
    let theirs = tokio::spawn({
        let core = fixture.core.clone();
        async move {
            handle_tasks_next_pending(
                &core,
                &device(OTHER_DEVICE_FP),
                roost_proto::TasksNextPendingRequest::default(),
            )
            .await
        }
    });
    let (mine, theirs) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(10), mine),
        tokio::time::timeout(Duration::from_secs(10), theirs)
    );
    let claimed: Vec<String> = [mine, theirs]
        .into_iter()
        .filter_map(|outcome| {
            outcome
                .expect("the claim finished")
                .expect("the task joined")
                .expect("a claim answer")
                .body
                .task
                .into_option()
                .map(|task| task.claimed_by.unwrap_or_default())
        })
        .collect();
    assert_eq!(
        claimed.len(),
        1,
        "one statement picks one winner: the other device is told the queue is empty"
    );
}
