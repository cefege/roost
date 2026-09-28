//! Continuation epoch semantics, strict worker-batch validation, cancel
//! ordering and grouping, per-worker serialization, and pending cleanup for
//! global terminal search, through the real handlers and pending table.
//!
//! Ported from `apps/coord/tests/search/global-search-control.test.ts`. A v2
//! `AbortController.abort()` on the request is a dropped call here.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "search_support/mod.rs"]
mod support;

use connectrpc::ErrorCode;
use roost_coord::search::cancel::handle_sessions_cancel_global_search;
use roost_coord::search::cursor_types::GlobalSearchSessionPosition;
use roost_proto::{
    GlobalSearchPartialReason, SessionsCancelGlobalSearchRequest, SessionsSearchGlobalRequest,
};
use roost_protocol::terminal_search::GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION;
use serde_json::{Value, json};
use support::{Harness, WORKER_A1, WORKER_A2, search_request as request, session_id};

fn result(grid_epoch: &str, overrides: Value) -> Value {
    let mut base = json!({
        "matches": [],
        "truncated": false,
        "scrollback_total": 3_000,
        "cols": 80,
        "grid_epoch": grid_epoch,
        "scanned_start_row": 0,
        "scanned_end_row": 10,
        "history_floor": "none",
        "stop_reason": "complete",
    });
    support::merge(&mut base, overrides);
    base
}

async fn cancel(harness: &Harness, search_id: &str) {
    handle_sessions_cancel_global_search(
        &harness.core,
        &harness.caller(),
        SessionsCancelGlobalSearchRequest {
            search_id: search_id.to_owned(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

// "sends the exact returned epoch and exclusive row, then resets only on epoch change"
#[tokio::test]
async fn continuations_send_the_returned_epoch_and_row_and_reset_only_on_epoch_change() {
    let harness = Harness::new("control-epoch").await;
    let worker = harness.install_worker(WORKER_A1);
    let id = session_id(400);
    harness.open_session(&id, WORKER_A1, 0).await;
    let mut first_request = request("needle", "epoch-pages", None);
    first_request.case_sensitive = true;

    let first = harness.search(first_request.clone());
    let sent = worker.wait_for_kind("search-scrollback-batch", 1).await;
    let rows = u64::from(GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION);
    harness.respond(&sent[0], json!({ "entries": [{ "status": "ok", "session_id": id, "result": result("epoch-one", json!({
        "scanned_start_row": 100, "scanned_end_row": 100 + rows, "next_before_row": 100, "stop_reason": "row_limit",
    })) }] }));
    let first = first.await.unwrap().unwrap();
    assert!(first.next_cursor.is_some());

    let second = harness.search(SessionsSearchGlobalRequest {
        cursor: first.next_cursor,
        ..first_request.clone()
    });
    let sent = worker.wait_for_kind("search-scrollback-batch", 2).await;
    assert_eq!(
        sent[1].control["sessions"],
        json!([{ "session_id": id, "grid_epoch": "epoch-one", "before_row": 100 }])
    );
    harness.respond(&sent[1], json!({ "entries": [{ "status": "ok", "session_id": id, "result": result("epoch-two", json!({
        "truncated": true, "scanned_start_row": 100, "scanned_end_row": 100, "stop_reason": "deadline",
    })) }] }));
    let second = second.await.unwrap().unwrap();
    assert_eq!(
        second.partials[0].reason,
        GlobalSearchPartialReason::Deadline
    );
    assert!(second.next_cursor.is_some());

    let third = harness.search(SessionsSearchGlobalRequest {
        cursor: second.next_cursor,
        ..first_request
    });
    let sent = worker.wait_for_kind("search-scrollback-batch", 3).await;
    assert_eq!(
        sent[2].control["sessions"],
        json!([{ "session_id": id, "grid_epoch": "epoch-two" }])
    );
    harness.respond(&sent[2], json!({ "entries": [{ "status": "ok", "session_id": id, "result": result("epoch-two", json!({})) }] }));
    assert_eq!(third.await.unwrap().unwrap().next_cursor, None);
}

// "turns missing, duplicate, reordered, over-budget, row, and epoch lies into malformed partials"
#[tokio::test]
async fn worker_batch_lies_become_malformed_partials() {
    let harness = Harness::new("control-malformed").await;
    let worker = harness.install_worker(WORKER_A1);
    let first_id = session_id(410);
    let second_id = session_id(411);
    harness.open_session(&first_id, WORKER_A1, 1).await;
    harness.open_session(&second_id, WORKER_A1, 2).await;
    let rows = u64::from(GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION);
    let large = |count: u32| -> Value {
        Value::Array(
            (0..count)
                .map(|col| json!({ "row": 5, "col": col, "len": 1, "preview": "x" }))
                .collect(),
        )
    };
    let ok = |id: &str, overrides: Value| json!({ "status": "ok", "session_id": id, "result": result("epoch", overrides) });
    let malformed = [
        json!({ "entries": [] }),
        json!({ "entries": [ok(&second_id, json!({})), ok(&second_id, json!({}))] }),
        json!({ "entries": [ok(&first_id, json!({})), ok(&second_id, json!({}))] }),
        json!({ "entries": [ok(&second_id, json!({ "scanned_start_row": 1, "scanned_end_row": 1 + rows + 1 })), ok(&first_id, json!({}))] }),
        json!({ "entries": [ok(&second_id, json!({ "matches": large(200) })), ok(&first_id, json!({ "matches": large(56) }))] }),
    ];
    for (index, raw) in malformed.iter().enumerate() {
        let answer = harness.search(request("malformed", &format!("malformed-{index}"), None));
        let sent = worker
            .wait_for_kind("search-scrollback-batch", index + 1)
            .await;
        harness.respond(&sent[index], raw.clone());
        let response = answer.await.unwrap().unwrap();
        assert!(response.matches.is_empty(), "case {index}");
        assert_eq!(response.searched_sessions, 0, "case {index}");
        assert_eq!(response.partials.len(), 2, "case {index}");
        assert!(
            response
                .partials
                .iter()
                .all(|p| p.reason == GlobalSearchPartialReason::MalformedResult),
            "case {index}"
        );
    }

    let cursor = harness.issue_cursor(
        "cursor-epoch-lie",
        "malformed",
        vec![GlobalSearchSessionPosition {
            session_id: second_id.clone(),
            worker_fp: WORKER_A1.to_owned(),
            grid_epoch: "expected-epoch".to_owned(),
            before_row: Some(100),
        }],
        1,
    );
    let answer = harness.search(request("malformed", "cursor-epoch-lie", Some(cursor)));
    let sent = worker
        .wait_for_kind("search-scrollback-batch", malformed.len() + 1)
        .await;
    harness.respond(sent.last().unwrap(), json!({ "entries": [{ "status": "ok", "session_id": second_id, "result": result("wrong-epoch", json!({
        "scanned_start_row": 90, "scanned_end_row": 100, "history_floor": "evicted",
    })) }] }));
    let response = answer.await.unwrap().unwrap();
    assert_eq!(
        response.partials[0].reason,
        GlobalSearchPartialReason::MalformedResult
    );
    assert_eq!(harness.pending(), 0);
}

// "retires before cancel reauthorization, groups once per worker, and clears pending RPCs"
#[tokio::test]
async fn a_cancel_retires_first_groups_per_worker_and_clears_pending_rpcs() {
    let harness = Harness::new("control-cancel").await;
    let worker_a1 = harness.install_worker(WORKER_A1);
    let worker_a2 = harness.install_worker(WORKER_A2);
    let first_id = session_id(420);
    let second_id = session_id(421);
    harness.open_session(&first_id, WORKER_A1, 0).await;
    harness.open_session(&second_id, WORKER_A2, 0).await;
    let pending_before = harness.pending();
    let search = harness.search(request("cancel", "cancel-active", None));
    worker_a1.wait_for_kind("search-scrollback-batch", 1).await;
    worker_a2.wait_for_kind("search-scrollback-batch", 1).await;
    assert_eq!(harness.pending(), pending_before + 2);

    cancel(&harness, "cancel-active").await;
    let cancel_a1 = worker_a1
        .wait_for_kind("cancel-scrollback-search-batch", 1)
        .await;
    let cancel_a2 = worker_a2
        .wait_for_kind("cancel-scrollback-search-batch", 1)
        .await;
    assert_eq!(cancel_a1[0].control["session_ids"], json!([first_id]));
    assert_eq!(cancel_a2[0].control["session_ids"], json!([second_id]));
    assert_eq!(cancel_a1[0].viewer_id, "global-browser:global-tab");
    assert_eq!(search.await.unwrap().unwrap_err().code, ErrorCode::Canceled);
    assert_eq!(harness.pending(), pending_before);

    let searches_before = worker_a1.of_kind("search-scrollback-batch").len();
    cancel(&harness, "cancel-before").await;
    let refused = harness.search(request("cancel", "cancel-before", None));
    assert_eq!(
        refused.await.unwrap().unwrap_err().code,
        ErrorCode::Canceled
    );
    assert_eq!(
        worker_a1.of_kind("search-scrollback-batch").len(),
        searches_before
    );
}

// "serializes same-worker pages, leaves other workers parallel, and charges queue time to the page"
#[tokio::test]
async fn same_worker_pages_serialize_and_queue_time_is_charged_to_the_page() {
    let harness = Harness::new("control-lanes").await;
    let worker_a1 = harness.install_worker(WORKER_A1);
    let worker_a2 = harness.install_worker(WORKER_A2);
    harness.open_session(&session_id(430), WORKER_A1, 0).await;
    harness.open_session(&session_id(431), WORKER_A2, 0).await;

    let first = harness.search(request("first", "lane-first", None));
    worker_a1.wait_for_kind("search-scrollback-batch", 1).await;
    worker_a2.wait_for_kind("search-scrollback-batch", 1).await;

    let queued = harness.spawn_search(
        "second-browser",
        Some("second-tab"),
        request("queued", "lane-queued", None),
    );
    let queued = queued.await.unwrap().unwrap();
    assert_eq!(queued.partials.len(), 2);
    assert!(
        queued
            .partials
            .iter()
            .all(|p| p.reason == GlobalSearchPartialReason::Deadline)
    );
    assert_eq!(worker_a1.of_kind("search-scrollback-batch").len(), 1);
    assert_eq!(worker_a2.of_kind("search-scrollback-batch").len(), 1);

    // The browser hanging up is the v2 abort: the dropped call tombstones the
    // search, cancels its sessions on their workers, and frees its pending RPCs.
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert_eq!(worker_a1.of_kind("cancel-scrollback-search-batch").len(), 1);
    assert_eq!(harness.pending(), 0);
}

// "maps a synchronous send failure to unavailable and releases pending state"
#[tokio::test]
async fn a_send_failure_is_worker_unavailable_and_releases_pending_state() {
    let harness = Harness::new("control-send-failure").await;
    let worker = harness.install_worker(WORKER_A1);
    let id = session_id(440);
    harness.open_session(&id, WORKER_A1, 0).await;
    worker.fail_sends(true);
    let pending_before = harness.pending();
    let response = harness
        .search(request("send-failure", "send-failure", None))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.partials.len(), 1);
    assert_eq!(response.partials[0].session_id, id);
    assert_eq!(
        response.partials[0].reason,
        GlobalSearchPartialReason::WorkerUnavailable
    );
    assert_eq!(harness.pending(), pending_before);
}
