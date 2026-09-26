// The task queue's five Connect methods, over a real migrated database and the
// real `task_bus`, driven the way a browser and a worker drive them.
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

use roost_coord::events::bus_messages::TaskBusMsgKind;
use roost_coord::sessions::tasks::{
    handle_tasks_cancel, handle_tasks_list, handle_tasks_next_pending, handle_tasks_set_state,
};
use tasks_support::{
    DEVICE_FP, OTHER_DEVICE_FP, TasksFixture, WORKER_FP, claim_next, device, list, message_of,
    stored_state,
};

#[tokio::test]
async fn a_task_change_reaches_a_sync_subscriber() {
    let fixture = TasksFixture::new("sync-subscriber").await;
    let id = fixture.enqueue(r#"{"prompt":"build"}"#).await;

    // Claim it, from the device that will finish it.
    let claimed = handle_tasks_next_pending(
        &fixture.core,
        &device(DEVICE_FP),
        roost_proto::TasksNextPendingRequest::default(),
    )
    .await
    .expect("a claim")
    .body
    .task
    .into_option()
    .expect("a claimed task");
    assert_eq!(claimed.state, "claimed");
    assert_eq!(claimed.claimed_by.as_deref(), Some(DEVICE_FP));
    assert!(claimed.claimed_at_ms.is_some(), "the claim is stamped");

    handle_tasks_set_state(
        &fixture.core,
        &device(DEVICE_FP),
        roost_proto::TasksSetStateRequest {
            id: id.clone(),
            state: "done".to_owned(),
            result_json: Some(r#"{"ok":true}"#.to_owned()),
            ..Default::default()
        },
    )
    .await
    .expect("a finished task");

    let deltas = fixture.deltas();
    let kinds: Vec<TaskBusMsgKind> = deltas.iter().map(|delta| delta.kind).collect();
    assert_eq!(
        kinds,
        vec![
            TaskBusMsgKind::Created,
            TaskBusMsgKind::State,
            TaskBusMsgKind::State
        ],
        "every mutation reaches the subscriber, and the first is the only `created`"
    );
    // The delta a browser folds is the row as STORED, not the row it asked for.
    let finished = &deltas[2].task;
    assert_eq!(finished.state, "done");
    assert_eq!(finished.result_json.as_deref(), Some(r#"{"ok":true}"#));
    assert!(
        finished.finished_at_ms.is_some(),
        "a terminal state is stamped"
    );
    assert_eq!(finished.claimed_by.as_deref(), Some(DEVICE_FP));
}

#[tokio::test]
async fn a_cancelled_task_reaches_the_subscriber_and_stamps_a_finish() {
    let fixture = TasksFixture::new("cancel-delta").await;
    let id = fixture.enqueue("{}").await;
    fixture.forget_deltas();

    let cancelled = handle_tasks_cancel(
        &fixture.core,
        &device(DEVICE_FP),
        roost_proto::TasksCancelRequest {
            id: id.clone(),
            ..Default::default()
        },
    )
    .await
    .expect("a cancelled task")
    .body
    .task
    .into_option()
    .expect("a task");
    assert_eq!(cancelled.state, "cancelled");
    assert!(cancelled.finished_at_ms.is_some());

    let deltas = fixture.deltas();
    assert_eq!(deltas.len(), 1, "the cancel publishes exactly one delta");
    assert_eq!(deltas[0].kind, TaskBusMsgKind::State);
    assert_eq!(deltas[0].task.id, cancelled.id);
}

#[tokio::test]
async fn the_next_pending_claim_takes_the_oldest_task_and_ignores_a_claimed_one() {
    let fixture = TasksFixture::new("claim-order").await;
    fixture
        .seed(
            "22222222-2222-4222-8222-222222222222",
            "pending",
            2_000,
            None,
        )
        .await;
    fixture
        .seed(
            "11111111-1111-4111-8111-111111111111",
            "pending",
            1_000,
            None,
        )
        .await;
    fixture
        .seed(
            "33333333-3333-4333-8333-333333333333",
            "claimed",
            0,
            Some(WORKER_FP),
        )
        .await;

    let first = claim_next(&fixture).await;
    assert_eq!(first.id, "11111111-1111-4111-8111-111111111111");
    let second = claim_next(&fixture).await;
    assert_eq!(second.id, "22222222-2222-4222-8222-222222222222");
    assert_eq!(
        second.claimed_by.as_deref(),
        Some(DEVICE_FP),
        "a task another machine holds is never re-claimed"
    );

    // The queue is drained, and draining is an answer rather than a refusal.
    let empty = handle_tasks_next_pending(
        &fixture.core,
        &device(DEVICE_FP),
        roost_proto::TasksNextPendingRequest::default(),
    )
    .await
    .expect("an empty queue is not an error")
    .body;
    assert!(empty.task.into_option().is_none());
}

#[tokio::test]
async fn a_device_that_did_not_claim_a_task_may_not_report_its_outcome() {
    let fixture = TasksFixture::new("claim-fence").await;
    fixture
        .seed(
            "11111111-1111-4111-8111-111111111111",
            "claimed",
            1_000,
            Some(DEVICE_FP),
        )
        .await;

    let refused = handle_tasks_set_state(
        &fixture.core,
        &device(OTHER_DEVICE_FP),
        roost_proto::TasksSetStateRequest {
            id: "11111111-1111-4111-8111-111111111111".to_owned(),
            state: "done".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect_err("another device cannot finish a claimed task");
    assert_eq!(refused.code, connectrpc::ErrorCode::PermissionDenied);
    assert!(fixture.deltas().is_empty(), "a refusal publishes nothing");

    // The claim holder may.
    handle_tasks_set_state(
        &fixture.core,
        &device(DEVICE_FP),
        roost_proto::TasksSetStateRequest {
            id: "11111111-1111-4111-8111-111111111111".to_owned(),
            state: "done".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect("the claim holder finishes its own task");
    assert_eq!(fixture.deltas().len(), 1);
}

#[tokio::test]
async fn a_pending_task_is_open_to_any_device() {
    let fixture = TasksFixture::new("pending-open").await;
    fixture
        .seed(
            "11111111-1111-4111-8111-111111111111",
            "pending",
            1_000,
            None,
        )
        .await;
    handle_tasks_set_state(
        &fixture.core,
        &device(OTHER_DEVICE_FP),
        roost_proto::TasksSetStateRequest {
            id: "11111111-1111-4111-8111-111111111111".to_owned(),
            state: "failed".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect("a pending task has no claim to fence it");
}

#[tokio::test]
async fn a_finished_task_cannot_be_cancelled() {
    let fixture = TasksFixture::new("terminal").await;
    fixture
        .seed(
            "11111111-1111-4111-8111-111111111111",
            "done",
            1_000,
            Some(DEVICE_FP),
        )
        .await;

    let refused = handle_tasks_cancel(
        &fixture.core,
        &device(DEVICE_FP),
        roost_proto::TasksCancelRequest {
            id: "11111111-1111-4111-8111-111111111111".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect_err("a done task has nothing left to stop");
    assert_eq!(refused.code, connectrpc::ErrorCode::NotFound);
    assert!(message_of(&refused).contains("already terminal"));

    let state = stored_state(&fixture, "11111111-1111-4111-8111-111111111111").await;
    assert_eq!(state, "done", "the refusal did not rewrite the row");
}

#[tokio::test]
async fn an_absent_task_is_not_found_on_both_methods_that_name_one() {
    let fixture = TasksFixture::new("absent").await;
    for refused in [
        handle_tasks_set_state(
            &fixture.core,
            &device(DEVICE_FP),
            roost_proto::TasksSetStateRequest {
                id: "99999999-9999-4999-8999-999999999999".to_owned(),
                state: "done".to_owned(),
                ..Default::default()
            },
        )
        .await
        .expect_err("no such task"),
        handle_tasks_cancel(
            &fixture.core,
            &device(DEVICE_FP),
            roost_proto::TasksCancelRequest {
                id: "99999999-9999-4999-8999-999999999999".to_owned(),
                ..Default::default()
            },
        )
        .await
        .expect_err("no such task"),
    ] {
        assert_eq!(refused.code, connectrpc::ErrorCode::NotFound);
    }
    assert!(fixture.deltas().is_empty());
}

#[tokio::test]
async fn a_state_the_queue_does_not_have_is_refused_before_the_database_is_touched() {
    let fixture = TasksFixture::new("bad-state").await;
    fixture
        .seed(
            "11111111-1111-4111-8111-111111111111",
            "pending",
            1_000,
            None,
        )
        .await;

    let listed = handle_tasks_list(
        &fixture.core,
        &device(DEVICE_FP),
        roost_proto::TasksListRequest {
            state: Some("halfway".to_owned()),
            ..Default::default()
        },
    )
    .await
    .expect_err("`halfway` is not a state");
    assert_eq!(listed.code, connectrpc::ErrorCode::InvalidArgument);

    let set = handle_tasks_set_state(
        &fixture.core,
        &device(DEVICE_FP),
        roost_proto::TasksSetStateRequest {
            id: "11111111-1111-4111-8111-111111111111".to_owned(),
            state: "halfway".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect_err("`halfway` is not a state");
    assert_eq!(set.code, connectrpc::ErrorCode::InvalidArgument);

    let state = stored_state(&fixture, "11111111-1111-4111-8111-111111111111").await;
    assert_eq!(state, "pending", "an unknown state changed nothing");
}

#[tokio::test]
async fn listing_filters_by_state_and_treats_an_empty_filter_as_every_state() {
    let fixture = TasksFixture::new("list-filter").await;
    fixture
        .seed(
            "11111111-1111-4111-8111-111111111111",
            "pending",
            1_000,
            None,
        )
        .await;
    fixture
        .seed("22222222-2222-4222-8222-222222222222", "done", 2_000, None)
        .await;

    let all = list(&fixture, None).await;
    assert_eq!(all.len(), 2);
    let empty_filter = list(&fixture, Some("")).await;
    assert_eq!(
        empty_filter.len(),
        2,
        "an empty filter is the same question as no filter"
    );
    let pending = list(&fixture, Some("pending")).await;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, "11111111-1111-4111-8111-111111111111");
}

#[tokio::test]
async fn a_list_returns_the_oldest_five_hundred_rows() {
    let fixture = TasksFixture::new("list-cap").await;
    for index in 0..505_i64 {
        fixture
            .seed(
                &format!("{index:08x}-0000-4000-8000-000000000000"),
                "pending",
                index,
                None,
            )
            .await;
    }
    let listed = list(&fixture, None).await;
    assert_eq!(listed.len(), 500, "the queue is bounded");
    assert_eq!(
        listed[0].id, "00000000-0000-4000-8000-000000000000",
        "and the bound keeps the OLDEST, which is the claim order"
    );
    assert_eq!(listed[499].id, "000001f3-0000-4000-8000-000000000000");
}
