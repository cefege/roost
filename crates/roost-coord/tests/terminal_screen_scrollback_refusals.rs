//! The refusals the two scrollback search methods share, end to end: a caller
//! that never had a tab, a session that never was, and the one worker error
//! that is the caller's fault rather than the coordinator's.
//!
//! Each of these is asserted at the point that matters -- that nothing reached
//! the worker and that no correlation entry was left behind -- because a
//! refusal that leaks a pending entry is a leak per abandoned request.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "terminal_screen_support/mod.rs"]
mod support;

use roost_coord::terminal_screen::rpc::handle_sessions_search_scrollback;
use support::{Harness, session};

#[tokio::test]
async fn a_search_with_no_tab_id_is_refused_before_any_command_reaches_the_worker() {
    let harness = Harness::new("no-tab").await;
    let mut caller = harness.caller();
    caller.tab_id = None;

    let error = handle_sessions_search_scrollback(
        &harness.core,
        &caller,
        roost_proto::SessionsSearchScrollbackRequest {
            session_id: harness.session_id.clone(),
            query: "needle".to_owned(),
            case_sensitive: true,
            regex: false,
            max_matches: 20,
            grid_epoch: "grid".to_owned(),
            max_rows: 100,
            search_id: "no-tab".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect_err("a tab-less search is refused");

    assert_eq!(error.code, connectrpc::ErrorCode::InvalidArgument);
    assert!(error.message.unwrap().contains("x-roost-tab-id"));
    assert!(
        harness.frames().is_empty(),
        "nothing was sent to the worker"
    );
    assert_eq!(
        harness.core.services.scrollback.pending().pending_count(),
        0
    );
}

#[tokio::test]
async fn a_search_for_an_unknown_session_is_not_found_and_sends_nothing() {
    let harness = Harness::new("unknown").await;
    let error = handle_sessions_search_scrollback(
        &harness.core,
        &harness.caller(),
        roost_proto::SessionsSearchScrollbackRequest {
            session_id: session("9"),
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
    .expect_err("an unknown session is refused");

    assert_eq!(error.code, connectrpc::ErrorCode::NotFound);
    assert!(harness.frames().is_empty());
}
