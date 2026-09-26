//! The search and cancel RPCs end to end, over a real database and a real
//! worker socket generation.
//!
//! The unit surface is in `terminal_screen_scrollback.rs`; what is here is the
//! part only a real call can show: that a search really reaches the worker as a
//! `search-scrollback` frame, that a cancel reaches it as a
//! `cancel-scrollback-search` frame carrying the SAME tab-owned identity, and
//! that a cancel which beats its own search still retires it before any frame
//! is sent.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "terminal_screen_support/mod.rs"]
mod support;

use roost_coord::terminal_screen::rpc::{
    handle_sessions_cancel_scrollback_search, handle_sessions_search_scrollback,
};
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use support::{Harness, WORKER_FP, frame_of, wait_for_frame};

#[tokio::test]
async fn a_search_reaches_the_worker_scoped_to_the_calling_tab() {
    let harness = Harness::new("search").await;
    let core = harness.core.clone();
    let caller = harness.caller();
    let session_id = harness.session_id.clone();
    let handle = tokio::spawn(async move {
        handle_sessions_search_scrollback(
            &core,
            &caller,
            roost_proto::SessionsSearchScrollbackRequest {
                session_id,
                query: "needle".to_owned(),
                case_sensitive: true,
                regex: false,
                max_matches: 20,
                grid_epoch: "browser-grid:4".to_owned(),
                before_row: Some(800),
                max_rows: 100,
                search_id: "search-7".to_owned(),
                ..Default::default()
            },
        )
        .await
    });

    let sent = wait_for_frame(&harness, 1).await;
    let (browser_id, frame, viewer_id, _) = frame_of(&sent);
    assert_eq!(
        browser_id, "browser-fp:tab-1",
        "a search is tab-scoped so one tab cannot stop another's scan"
    );
    assert_eq!(viewer_id, "browser-fp:tab-1");
    match frame {
        ClientControlFrame::SearchScrollback {
            search_id,
            before_row,
            max_rows,
            max_matches,
            query,
            ..
        } => {
            assert_eq!(search_id.as_str(), "search-7");
            assert_eq!(before_row.map(|row| row.as_i64()), Some(800));
            assert_eq!(*max_rows, 100);
            assert_eq!(*max_matches, 20);
            assert_eq!(query.as_str(), "needle");
        }
        other => panic!("a search is a search-scrollback frame, got {other:?}"),
    }

    let worker = WorkerFp::try_from(WORKER_FP).unwrap();
    let pending_id = match &sent {
        CoordWorkerDownstream::BrowserCommand { request_id, .. } => request_id.clone(),
        _ => unreachable!(),
    };
    harness.core.services.scrollback.pending().resolve(
        &pending_id,
        serde_json::json!({
            "matches": [{ "row": 745, "col": 3, "len": 6, "preview": "a needle here" }],
            "truncated": false,
            "scrollback_total": 1_000,
            "cols": 80,
            "grid_epoch": "browser-grid:4",
            "scanned_start_row": 700,
            "scanned_end_row": 800,
            "history_floor": "evicted",
            "stop_reason": "row_limit",
            "next_before_row": 700,
        }),
        Some(worker.as_str()),
    );
    let response = handle
        .await
        .expect("the handler task finished")
        .expect("the page is served")
        .body;
    assert_eq!(response.matches[0].row, 745);
    assert_eq!(response.matches[0].preview, "a needle here");
    assert_eq!(response.next_before_row, Some(700));
    assert_eq!(
        response.history_floor,
        roost_proto::ScrollbackHistoryFloor::Evicted
    );
    assert_eq!(
        response.stop_reason,
        roost_proto::SearchStopReason::RowLimit
    );
    assert!(
        !response.truncated,
        "a row-limit page is not a truncated search"
    );
}

#[tokio::test]
async fn a_search_worker_error_stays_internal_but_an_invalid_regex_is_the_callers() {
    let harness = Harness::new("search-error").await;
    for (message, expected) in [
        (
            "invalid regex: unterminated group",
            connectrpc::ErrorCode::InvalidArgument,
        ),
        ("grid epoch changed", connectrpc::ErrorCode::Internal),
    ] {
        let core = harness.core.clone();
        let caller = harness.caller();
        let session_id = harness.session_id.clone();
        let handle = tokio::spawn(async move {
            handle_sessions_search_scrollback(
                &core,
                &caller,
                roost_proto::SessionsSearchScrollbackRequest {
                    session_id,
                    query: "needle".to_owned(),
                    case_sensitive: true,
                    regex: true,
                    max_matches: 20,
                    grid_epoch: "grid".to_owned(),
                    max_rows: 100,
                    search_id: format!("search-{message}"),
                    ..Default::default()
                },
            )
            .await
        });
        let sent = wait_for_frame(&harness, harness.frames().len() + 1).await;
        let pending_id = match &sent {
            CoordWorkerDownstream::BrowserCommand { request_id, .. } => request_id.clone(),
            _ => unreachable!(),
        };
        let worker = WorkerFp::try_from(WORKER_FP).unwrap();
        assert!(harness.core.services.scrollback.pending().reject(
            &pending_id,
            message,
            Some(worker.as_str())
        ));
        let error = handle
            .await
            .expect("the handler task finished")
            .expect_err("the search failed");
        assert_eq!(error.code, expected, "for {message}");
    }
    assert_eq!(
        harness.core.services.scrollback.pending().pending_count(),
        0,
        "a settled search leaves no correlation entry behind"
    );
}

#[tokio::test]
async fn an_explicit_cancel_forwards_the_same_tab_owned_identity() {
    let harness = Harness::new("cancel").await;
    let core = harness.core.clone();
    let caller = harness.caller();
    let session_id = harness.session_id.clone();

    let search = tokio::spawn({
        let core = core.clone();
        let caller = caller.clone();
        let session_id = session_id.clone();
        async move {
            handle_sessions_search_scrollback(
                &core,
                &caller,
                roost_proto::SessionsSearchScrollbackRequest {
                    session_id,
                    query: "needle".to_owned(),
                    case_sensitive: true,
                    regex: false,
                    max_matches: 20,
                    grid_epoch: "grid".to_owned(),
                    max_rows: 100,
                    search_id: "search-7".to_owned(),
                    ..Default::default()
                },
            )
            .await
        }
    });
    wait_for_frame(&harness, 1).await;

    handle_sessions_cancel_scrollback_search(
        &core,
        &caller,
        roost_proto::SessionsCancelScrollbackSearchRequest {
            session_id: session_id.clone(),
            search_id: "search-7".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect("the cancel is accepted");

    let frames = harness.frames();
    let cancellation = frames
        .iter()
        .find_map(|sent| match frame_of(sent).1 {
            ClientControlFrame::CancelScrollbackSearch {
                request_id,
                search_request_id,
                ..
            } => Some((request_id.clone(), search_request_id.clone())),
            _ => None,
        })
        .expect("the cancel reached the worker");
    assert_eq!(
        cancellation.0, "search-7",
        "the cancel reuses the search id as its own correlation id"
    );
    assert_eq!(cancellation.1.as_str(), "search-7");
    assert_eq!(
        frame_of(&frames[1]).0,
        "browser-fp:tab-1",
        "the cancel carries the same tab scope as the search"
    );

    // The worker aborts the scan and answers with an error; the search fails
    // with it rather than hanging until the deadline.
    let worker = WorkerFp::try_from(WORKER_FP).unwrap();
    let pending_id = match &frames[0] {
        CoordWorkerDownstream::BrowserCommand { request_id, .. } => request_id.clone(),
        _ => unreachable!(),
    };
    assert!(harness.core.services.scrollback.pending().reject(
        &pending_id,
        "scrollback search superseded",
        Some(worker.as_str())
    ));
    let error = search
        .await
        .expect("the handler task finished")
        .expect_err("the search was called off");
    assert_eq!(error.code, connectrpc::ErrorCode::Internal);
    assert_eq!(
        harness.core.services.scrollback.pending().pending_count(),
        0
    );
}

#[tokio::test]
async fn a_cancel_that_arrives_before_its_search_retires_it_without_a_frame() {
    let harness = Harness::new("cancel-first").await;
    let core = harness.core.clone();
    let caller = harness.caller();
    let session_id = harness.session_id.clone();

    handle_sessions_cancel_scrollback_search(
        &core,
        &caller,
        roost_proto::SessionsCancelScrollbackSearchRequest {
            session_id: session_id.clone(),
            search_id: "search-7".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect("the cancel is accepted");
    let after_cancel = harness.frames().len();

    let error = handle_sessions_search_scrollback(
        &core,
        &caller,
        roost_proto::SessionsSearchScrollbackRequest {
            session_id,
            query: "needle".to_owned(),
            case_sensitive: true,
            regex: false,
            max_matches: 20,
            grid_epoch: "grid".to_owned(),
            max_rows: 100,
            search_id: "search-7".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect_err("the search is retired by the tombstone");

    assert_eq!(error.code, connectrpc::ErrorCode::Canceled);
    assert_eq!(
        harness.frames().len(),
        after_cancel,
        "a search the tombstone retired is never forwarded, so no scan runs on the worker"
    );
    assert_eq!(
        harness.core.services.scrollback.pending().pending_count(),
        0,
        "a retired search never allocated a correlation entry to leak"
    );
}

#[tokio::test]
async fn a_search_after_a_consumed_cancel_is_a_new_search() {
    let harness = Harness::new("cancel-consumed").await;
    let core = harness.core.clone();
    let caller = harness.caller();
    let session_id = harness.session_id.clone();
    let owned_session = session_id.clone();
    let request = move || roost_proto::SessionsSearchScrollbackRequest {
        session_id: owned_session.clone(),
        query: "needle".to_owned(),
        case_sensitive: true,
        regex: false,
        max_matches: 20,
        grid_epoch: "grid".to_owned(),
        max_rows: 100,
        search_id: "search-7".to_owned(),
        ..Default::default()
    };

    handle_sessions_cancel_scrollback_search(
        &core,
        &caller,
        roost_proto::SessionsCancelScrollbackSearchRequest {
            session_id: session_id.clone(),
            search_id: "search-7".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect("the cancel is accepted");
    assert!(
        handle_sessions_search_scrollback(&core, &caller, request())
            .await
            .is_err()
    );

    let second = tokio::spawn({
        let core = core.clone();
        let caller = caller.clone();
        async move { handle_sessions_search_scrollback(&core, &caller, request()).await }
    });
    let sent = wait_for_frame(&harness, 2).await;
    assert!(
        matches!(
            frame_of(&sent).1,
            ClientControlFrame::SearchScrollback { .. }
        ),
        "the tombstone was consumed by the first search, so a later one under the same id is forwarded"
    );
    let pending_id = match &sent {
        CoordWorkerDownstream::BrowserCommand { request_id, .. } => request_id.clone(),
        _ => unreachable!(),
    };
    let worker = WorkerFp::try_from(WORKER_FP).unwrap();
    harness.core.services.scrollback.pending().resolve(
        &pending_id,
        serde_json::json!({
            "matches": [],
            "truncated": false,
            "scrollback_total": 0,
            "cols": 80,
            "grid_epoch": "grid",
            "scanned_start_row": 0,
            "scanned_end_row": 0,
            "history_floor": "none",
            "stop_reason": "complete",
        }),
        Some(worker.as_str()),
    );
    let served = second
        .await
        .expect("the handler task finished")
        .expect("the new search is served")
        .body;
    assert_eq!(served.stop_reason, roost_proto::SearchStopReason::Complete);
}
