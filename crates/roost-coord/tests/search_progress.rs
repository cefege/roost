//! The two progress rules the fan-out and the cursor owner enforce together:
//! a page that rescans nothing at the row it was given does not hand that row
//! back, and a search must carry a tab id.
//!
//! Ported from `apps/coord/tests/search/global-search-progress.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "search_support/mod.rs"]
mod support;

use connectrpc::ErrorCode;
use roost_coord::search::cursor_types::GlobalSearchSessionPosition;
use roost_proto::GlobalSearchPartialReason;
use serde_json::json;
use support::{Harness, WORKER_A1, search_request, session_id};

fn cursor_position(id: &str, grid_epoch: &str) -> GlobalSearchSessionPosition {
    GlobalSearchSessionPosition {
        session_id: id.to_owned(),
        worker_fp: WORKER_A1.to_owned(),
        grid_epoch: grid_epoch.to_owned(),
        before_row: Some(512),
    }
}

// "ends a session that rescans nothing at the row it was given"
#[tokio::test]
async fn a_session_that_rescans_nothing_at_its_row_ends_as_a_deadline_partial() {
    let harness = Harness::new("progress-stalled").await;
    let id = session_id(420);
    harness.open_session(&id, WORKER_A1, 0).await;
    let worker = harness.install_worker(WORKER_A1);
    let cursor = harness.issue_cursor(
        "stalled-deadline",
        "needle",
        vec![cursor_position(&id, "epoch-stall")],
        1,
    );
    let answer = harness.search(search_request("needle", "stalled-deadline", Some(cursor)));
    let sent = worker.wait_for_kind("search-scrollback-batch", 1).await;
    assert_eq!(
        sent[0].control["sessions"],
        json!([{ "session_id": id, "grid_epoch": "epoch-stall", "before_row": 512 }])
    );
    harness.respond(
        &sent[0],
        json!({ "entries": [{ "status": "ok", "session_id": id, "result": {
        "matches": [], "truncated": true, "scrollback_total": 4_000, "cols": 80,
        "grid_epoch": "epoch-stall", "scanned_start_row": 512, "scanned_end_row": 512,
        "history_floor": "none", "stop_reason": "deadline",
    } }] }),
    );
    let response = answer.await.unwrap().unwrap();
    assert_eq!(response.partials.len(), 1);
    assert_eq!(response.partials[0].session_id, id);
    assert_eq!(
        response.partials[0].reason,
        GlobalSearchPartialReason::Deadline
    );
    // No cursor: another page would repeat this exact request forever.
    assert_eq!(response.next_cursor, None);
    assert_eq!(response.searched_sessions, 1);
}

// "keeps paging a session whose page advanced past the requested row"
#[tokio::test]
async fn a_session_whose_page_advanced_keeps_paging() {
    let harness = Harness::new("progress-advancing").await;
    let id = session_id(421);
    harness.open_session(&id, WORKER_A1, 0).await;
    let worker = harness.install_worker(WORKER_A1);
    let cursor = harness.issue_cursor(
        "advancing-deadline",
        "needle",
        vec![cursor_position(&id, "epoch-advance")],
        1,
    );
    let answer = harness.search(search_request("needle", "advancing-deadline", Some(cursor)));
    let sent = worker.wait_for_kind("search-scrollback-batch", 1).await;
    harness.respond(
        &sent[0],
        json!({ "entries": [{ "status": "ok", "session_id": id, "result": {
        "matches": [{ "row": 300, "col": 1, "len": 6, "preview": "needle" }],
        "truncated": true, "scrollback_total": 4_000, "cols": 80,
        "grid_epoch": "epoch-advance", "scanned_start_row": 200, "scanned_end_row": 512,
        "history_floor": "none", "stop_reason": "deadline",
    } }] }),
    );
    let response = answer.await.unwrap().unwrap();
    assert!(response.next_cursor.is_some());
    assert_eq!(response.matches.len(), 1);
}

// "rejects a search whose request carries no tab id"
#[tokio::test]
async fn a_search_without_a_tab_id_is_refused() {
    let harness = Harness::new("progress-no-tab").await;
    harness.open_session(&session_id(430), WORKER_A1, 0).await;
    harness.install_worker(WORKER_A1);
    let refusal = harness
        .spawn_search(
            "global-browser",
            None,
            search_request("needle", "no-tab", None),
        )
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(refusal.code, ErrorCode::InvalidArgument);
    assert!(
        refusal
            .message
            .unwrap_or_default()
            .contains("x-roost-tab-id")
    );
}
