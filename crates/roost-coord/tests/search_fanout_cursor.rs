//! Global-search cursor continuation through fake routable worker
//! generations: an offline session is kept for retry, and every cursor session
//! is reauthorized against session close and worker deletion.
//!
//! Ported from `apps/coord/tests/workers/worker-ws-transport-global-search-cursor.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
#[path = "search_support/mod.rs"]
mod support;

use roost_coord::search::cursor_types::GlobalSearchSessionPosition;
use roost_proto::GlobalSearchPartialReason;
use serde_json::json;
use support::{Harness, WORKER_A1, WORKER_A2, ok_entry, search_request, session_id};

// "retains an unvisited offline session for cursor retry"
#[tokio::test]
async fn an_unvisited_offline_session_is_retained_for_retry() {
    let harness = Harness::new("cursor-offline").await;
    let id = session_id(305);
    harness.open_session(&id, WORKER_A1, 0).await;
    let first = harness
        .search(search_request("retry", "offline-retry", None))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        first.partials[0].reason,
        GlobalSearchPartialReason::WorkerUnavailable
    );
    assert!(first.next_cursor.is_some());
    assert_eq!(first.searched_sessions, 0);
    assert_eq!(first.eligible_sessions, 1);

    let worker = harness.install_worker(WORKER_A1);
    let retry = harness.search(search_request("retry", "offline-retry", first.next_cursor));
    let sent = worker.wait_for_kind("search-scrollback-batch", 1).await;
    assert_eq!(
        sent[0].control["sessions"],
        json!([{ "session_id": id, "grid_epoch": "" }])
    );
    harness.respond(
        &sent[0],
        json!({ "entries": [ok_entry(&id, None, json!({}))] }),
    );
    let retried = retry.await.unwrap().unwrap();
    assert!(retried.partials.is_empty());
    assert_eq!(retried.next_cursor, None);
    assert_eq!(retried.searched_sessions, 1);
    assert_eq!(retried.eligible_sessions, 1);
}

// "reauthorizes every cursor session after close or worker deletion"
#[tokio::test]
async fn every_cursor_session_is_reauthorized_after_close_or_worker_deletion() {
    let harness = Harness::new("cursor-reauthorize").await;
    let closed_id = session_id(310);
    let deleted_worker_id = session_id(311);
    harness.open_session(&closed_id, WORKER_A1, 0).await;
    harness.open_session(&deleted_worker_id, WORKER_A2, 0).await;
    harness.install_worker(WORKER_A1);
    harness.install_worker(WORKER_A2);
    let position =
        |session_id: &str, worker_fp: &str, grid_epoch: &str| GlobalSearchSessionPosition {
            session_id: session_id.to_owned(),
            worker_fp: worker_fp.to_owned(),
            grid_epoch: grid_epoch.to_owned(),
            before_row: Some(100),
        };
    let cursor = harness.issue_cursor(
        "reauthorize",
        "needle",
        vec![
            position(&closed_id, WORKER_A1, "epoch-a"),
            position(&deleted_worker_id, WORKER_A2, "epoch-b"),
        ],
        2,
    );
    harness
        .execute(
            "UPDATE sessions SET status = 'closed' WHERE id = $1",
            &closed_id,
        )
        .await;
    harness
        .execute(
            "UPDATE workers SET deleted_at_ms = 1 WHERE fp = $1",
            WORKER_A2,
        )
        .await;

    let response = harness
        .search(search_request("needle", "reauthorize", Some(cursor)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.eligible_sessions, 2);
    assert_eq!(response.searched_sessions, 0);
    let partials: Vec<(String, Option<GlobalSearchPartialReason>)> = response
        .partials
        .iter()
        .map(|partial| (partial.session_id.clone(), partial.reason.as_known()))
        .collect();
    assert_eq!(
        partials,
        vec![
            (closed_id, Some(GlobalSearchPartialReason::SessionClosed)),
            (
                deleted_worker_id,
                Some(GlobalSearchPartialReason::SessionClosed)
            ),
        ]
    );
    assert_eq!(response.next_cursor, None);
}
