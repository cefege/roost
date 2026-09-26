//! The history page RPC end to end, over a real database and a real worker
//! socket generation: the window it names, and the order its rows arrive in.
//!
//! The unit surface is in `terminal_screen_scrollback.rs`; what is here is what
//! only a real call can show -- that a page really reaches the worker as a
//! `get-scrollback-cells` frame naming the call's own half-open window.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "terminal_screen_support/mod.rs"]
mod support;

use roost_coord::terminal_screen::rpc::handle_sessions_get_scrollback_cells;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::control::ClientControlFrame;
use support::{Harness, WORKER_FP, frame_of, wait_for_frame};

#[tokio::test]
async fn a_page_request_reaches_the_worker_as_a_bounded_window() {
    let harness = Harness::new("cells").await;
    let core = harness.core.clone();
    let caller = harness.caller();
    let session_id = harness.session_id.clone();
    let handle = tokio::spawn(async move {
        handle_sessions_get_scrollback_cells(
            &core,
            &caller,
            roost_proto::SessionsGetScrollbackCellsRequest {
                session_id,
                end_row: 500,
                max_rows: 1_000,
                grid_epoch: "browser-grid:4".to_owned(),
                ..Default::default()
            },
        )
        .await
    });

    let sent = wait_for_frame(&harness, 1).await;
    let (browser_id, frame, viewer_id, request_id) = frame_of(&sent);
    assert_eq!(
        browser_id, "browser-fp",
        "a page carries the caller's fingerprint, not a tab scope"
    );
    assert_eq!(viewer_id, "browser-fp");
    match frame {
        ClientControlFrame::GetScrollbackCells {
            session_id,
            grid_epoch,
            end_row,
            max_rows,
            ..
        } => {
            assert_eq!(session_id.as_str(), harness.session_id);
            assert_eq!(grid_epoch, "browser-grid:4");
            assert_eq!(*end_row, 500);
            assert_eq!(*max_rows, 1_000);
        }
        other => panic!("a page is a get-scrollback-cells frame, got {other:?}"),
    }

    // Settle the correlation entry from the worker side, then let the page land.
    let worker = WorkerFp::try_from(WORKER_FP).unwrap();
    let pending_id = request_id.to_owned();
    assert!(
        harness.core.services.scrollback.pending().resolve(
            &pending_id,
            serde_json::json!({
                "rows": [
                    { "index": 400, "spans": [] },
                    { "index": 401, "spans": [] },
                ],
                "cols": 80,
                "total": 900,
                "start_row": 400,
                "end_row": 500,
                "grid_epoch": "worker-grid:9",
                "history_floor": "resize_replay",
            }),
            Some(worker.as_str()),
        ),
        "the worker's rpc-ok settles the page"
    );
    let outcome = handle.await.expect("the handler task finished");
    let response = match outcome {
        Ok(response) => response.body,
        Err(error) => panic!(
            "the page is served, refused with {:?}: {error}",
            error.message
        ),
    };
    assert_eq!(
        response
            .rows
            .iter()
            .map(|row| row.index)
            .collect::<Vec<_>>(),
        vec![400, 401]
    );
    assert_eq!(
        (
            response.start_row,
            response.end_row,
            response.cols,
            response.scrollback_total
        ),
        (400, 500, 80, 900)
    );
    assert_eq!(response.grid_epoch, "worker-grid:9");
    assert_eq!(
        response.history_floor,
        roost_proto::ScrollbackHistoryFloor::ResizeReplay,
        "the floor the worker named survives the relay"
    );
}

#[tokio::test]
async fn a_page_whose_rows_are_out_of_order_is_refused_rather_than_relayed() {
    let harness = Harness::new("cells-order").await;
    let core = harness.core.clone();
    let caller = harness.caller();
    let session_id = harness.session_id.clone();
    let handle = tokio::spawn(async move {
        handle_sessions_get_scrollback_cells(
            &core,
            &caller,
            roost_proto::SessionsGetScrollbackCellsRequest {
                session_id,
                end_row: 500,
                max_rows: 1_000,
                grid_epoch: "browser-grid:4".to_owned(),
                ..Default::default()
            },
        )
        .await
    });
    let sent = wait_for_frame(&harness, 1).await;
    let (_, _, _, request_id) = frame_of(&sent);
    let worker = WorkerFp::try_from(WORKER_FP).unwrap();
    harness.core.services.scrollback.pending().resolve(
        request_id,
        serde_json::json!({
            "rows": [
                { "index": 401, "spans": [] },
                { "index": 400, "spans": [] },
            ],
            "cols": 80,
            "total": 900,
            "start_row": 400,
            "end_row": 500,
            "grid_epoch": "worker-grid:9",
            "history_floor": "none",
        }),
        Some(worker.as_str()),
    );
    let error = handle
        .await
        .expect("the handler task finished")
        .expect_err("the page is refused");
    assert_eq!(error.code, connectrpc::ErrorCode::Internal);
    let message = error.message.unwrap_or_default();
    assert!(
        message.contains("ascending contiguous run"),
        "the refusal names the order the proto promised: {message}"
    );
}
