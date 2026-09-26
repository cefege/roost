//! The cancel that has to retire a search it beat, and the answer a search page
//! is allowed to carry.
//!
//! Split from `terminal_screen_scrollback.rs` because the page window and the
//! search ledger are two different vocabularies: one is a range of absolute
//! rows, the other is an identity a browser cancels by. The relay that answers
//! them end to end is `terminal_screen_scrollback_search.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use connectrpc::{ConnectError, ErrorCode};
use roost_coord::terminal_screen::scrollback_relay::ScrollbackRelay;
use roost_coord::terminal_screen::scrollback_result::{
    SearchStop, parse_worker_search_result, validate_search_request,
};
use roost_coord::terminal_screen::search_ledger::{
    SEARCH_CANCEL_TOMBSTONE_MAX, SEARCH_CANCEL_TOMBSTONE_TTL_MS, ScrollbackSearchLedger,
    SearchIdentity,
};
use roost_protocol::terminal_search::{
    TERMINAL_SEARCH_MAX_MATCHES, TERMINAL_SEARCH_MAX_ROWS, TERMINAL_SEARCH_PREVIEW_MAX_CODE_POINTS,
    TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS,
};
use serde_json::json;

fn session(tail: &str) -> String {
    format!("00000000-0000-4000-8000-{tail:0>12}")
}

fn session_id(tail: &str) -> roost_protocol::wire::SessionId {
    roost_protocol::wire::SessionId::try_from(session(tail).as_str()).unwrap()
}

fn relay() -> ScrollbackRelay {
    ScrollbackRelay::with_clock(
        Arc::new(roost_coord::coord_core::worker_handle::WorkerRegistry::new()),
        Arc::new(|| 1_000),
    )
}

#[test]
fn a_search_request_is_refused_before_any_frame_reaches_the_worker() {
    let cases: Vec<(ConnectError, ErrorCode)> = vec![
        (
            validate_search_request("", "grid", "needle", 100, 20, None).unwrap_err(),
            ErrorCode::InvalidArgument,
        ),
        (
            validate_search_request(&"s".repeat(65), "grid", "needle", 100, 20, None).unwrap_err(),
            ErrorCode::InvalidArgument,
        ),
        (
            validate_search_request("search", &"e".repeat(65), "needle", 100, 20, None)
                .unwrap_err(),
            ErrorCode::InvalidArgument,
        ),
        (
            validate_search_request(
                "search",
                "grid",
                &"🙂".repeat(TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS + 1),
                100,
                20,
                None,
            )
            .unwrap_err(),
            ErrorCode::InvalidArgument,
        ),
        (
            validate_search_request("search", "grid", "needle", 0, 20, None).unwrap_err(),
            ErrorCode::InvalidArgument,
        ),
        (
            validate_search_request(
                "search",
                "grid",
                "needle",
                TERMINAL_SEARCH_MAX_ROWS + 1,
                20,
                None,
            )
            .unwrap_err(),
            ErrorCode::InvalidArgument,
        ),
        (
            validate_search_request(
                "search",
                "grid",
                "needle",
                100,
                TERMINAL_SEARCH_MAX_MATCHES + 1,
                None,
            )
            .unwrap_err(),
            ErrorCode::InvalidArgument,
        ),
        (
            validate_search_request(
                "search",
                "grid",
                "needle",
                100,
                20,
                Some(9_007_199_254_740_992),
            )
            .unwrap_err(),
            ErrorCode::InvalidArgument,
        ),
    ];
    for (error, code) in cases {
        assert_eq!(error.code, code);
    }
    // The boundary values themselves are admitted, so the bounds are not
    // off-by-one in the refusing direction.
    let boundary = validate_search_request(
        &"s".repeat(64),
        &"e".repeat(64),
        &"🙂".repeat(TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS),
        TERMINAL_SEARCH_MAX_ROWS,
        TERMINAL_SEARCH_MAX_MATCHES,
        None,
    )
    .expect("the declared maxima are admissible");
    assert_eq!(
        boundary.before_row, None,
        "an absent cursor stays absent, not zero"
    );
}

#[test]
fn a_search_page_that_disagrees_with_its_request_is_refused() {
    let request = validate_search_request("search-1", "grid-1", "needle", 100, 20, Some(800))
        .expect("a well-formed request");
    let good = json!({
        "matches": [{ "row": 745, "col": 3, "len": 6, "preview": "a needle here" }],
        "truncated": false,
        "scrollback_total": 1_000,
        "cols": 80,
        "grid_epoch": "grid-1",
        "scanned_start_row": 700,
        "scanned_end_row": 800,
        "history_floor": "evicted",
        "stop_reason": "row_limit",
        "next_before_row": 700,
    });
    let result = parse_worker_search_result(&good, &request).expect("a well-formed answer");
    assert_eq!(result.stop_reason, SearchStop::RowLimit);
    assert_eq!(result.next_before_row, Some(700));
    assert_eq!(result.matches[0].row, 745);

    let mut over = good.clone();
    over["scanned_end_row"] = json!(801);
    assert_eq!(
        parse_worker_search_result(&over, &request)
            .unwrap_err()
            .message
            .unwrap(),
        "malformed scrollback search result",
        "a page that scanned past the cursor the request named would make the SPA page forever"
    );

    let mut outside = good.clone();
    outside["matches"] = json!([{ "row": 699, "col": 0, "len": 1, "preview": "below" }]);
    assert!(
        parse_worker_search_result(&outside, &request).is_err(),
        "a match outside the scanned range is refused"
    );

    let mut too_many = good.clone();
    too_many["matches"] = json!(
        (0..21)
            .map(|i| json!({ "row": 700 + i, "col": 0, "len": 1, "preview": "x" }))
            .collect::<Vec<_>>()
    );
    assert!(
        parse_worker_search_result(&too_many, &request).is_err(),
        "more matches than the request asked for"
    );

    let mut unknown_stop = good.clone();
    unknown_stop["stop_reason"] = json!("unspecified");
    assert!(
        parse_worker_search_result(&unknown_stop, &request).is_err(),
        "an unknown stop reason is refused, not mapped onto complete"
    );

    let mut unknown_floor = good.clone();
    unknown_floor["history_floor"] = json!("unknown");
    assert!(
        parse_worker_search_result(&unknown_floor, &request).is_err(),
        "an unknown floor is refused, not mapped onto none"
    );

    let mut long_preview = good.clone();
    long_preview["matches"][0]["preview"] =
        json!("🙂".repeat(TERMINAL_SEARCH_PREVIEW_MAX_CODE_POINTS + 1));
    assert!(
        parse_worker_search_result(&long_preview, &request).is_err(),
        "a preview past the shared code-point limit is refused"
    );
}

#[test]
fn a_truncated_flag_must_match_the_stop_that_truncated_the_page() {
    let request = validate_search_request("search-1", "grid-1", "needle", 100, 20, None).unwrap();
    let mut lies = json!({
        "matches": [],
        "truncated": true,
        "scrollback_total": 1_000,
        "cols": 80,
        "grid_epoch": "grid-1",
        "scanned_start_row": 0,
        "scanned_end_row": 100,
        "history_floor": "none",
        "stop_reason": "complete",
    });
    assert!(
        parse_worker_search_result(&lies, &request).is_err(),
        "a complete search is not truncated, whatever the flag says"
    );
    lies["truncated"] = json!(false);
    lies["stop_reason"] = json!("deadline");
    assert!(
        parse_worker_search_result(&lies, &request).is_err(),
        "a deadline stop is truncated, so a flag that still says otherwise is refused"
    );
    lies["truncated"] = json!(true);
    assert!(
        parse_worker_search_result(&lies, &request).is_ok(),
        "a deadline stop is truncated"
    );
}

#[test]
fn a_page_the_worker_answered_for_another_epoch_is_refused_unless_it_says_so() {
    let request = validate_search_request("search-1", "grid-1", "needle", 100, 20, None).unwrap();
    let mut changed = json!({
        "matches": [],
        "truncated": false,
        "scrollback_total": 1_000,
        "cols": 80,
        "grid_epoch": "grid-9",
        "scanned_start_row": 0,
        "scanned_end_row": 0,
        "history_floor": "none",
        "stop_reason": "epoch_changed",
    });
    assert!(
        parse_worker_search_result(&changed, &request).is_ok(),
        "an epoch change is the one answer that legitimately names an epoch the request did not"
    );
    changed["stop_reason"] = json!("complete");
    changed["scanned_end_row"] = json!(10);
    assert!(parse_worker_search_result(&changed, &request).is_err());
}

#[test]
fn a_cancel_that_arrives_before_its_search_still_retires_it() {
    let relay = relay();
    let identity = relay.search_identity("browser:tab", &session_id("1"), "search-7");

    relay.record_cancel(&identity);
    assert_eq!(
        relay.live_tombstones(),
        1,
        "the cancel is held before its search exists"
    );

    assert!(
        relay.consume_cancel(&identity),
        "the tombstone retires the search when the search finally arrives"
    );
    assert_eq!(
        relay.live_tombstones(),
        0,
        "consuming the tombstone IS the retirement"
    );
    assert!(
        !relay.consume_cancel(&identity),
        "a second search under the same id is a new search, not a cancelled one"
    );
}

#[test]
fn a_cancel_only_retires_the_search_it_names() {
    let relay = relay();
    let named = relay.search_identity("browser:tab", &session_id("1"), "search-7");
    let other_tab = relay.search_identity("browser:other-tab", &session_id("1"), "search-7");
    let other_session = relay.search_identity("browser:tab", &session_id("2"), "search-7");
    let other_id = relay.search_identity("browser:tab", &session_id("1"), "search-8");

    relay.record_cancel(&named);

    assert!(
        !relay.consume_cancel(&other_tab),
        "one tab's cancel cannot stop another's scan"
    );
    assert!(!relay.consume_cancel(&other_session));
    assert!(!relay.consume_cancel(&other_id));
    assert!(relay.consume_cancel(&named));
}

#[test]
fn the_ledger_is_bounded_and_its_tombstones_expire() {
    let mut ledger = ScrollbackSearchLedger::new();
    for index in 0..SEARCH_CANCEL_TOMBSTONE_MAX {
        ledger.record_cancel(
            &SearchIdentity::new("tab", "s", &format!("search-{index}")),
            1_000,
        );
    }
    assert_eq!(ledger.live_count(1_000), SEARCH_CANCEL_TOMBSTONE_MAX);
    ledger.record_cancel(&SearchIdentity::new("tab", "s", "one-too-many"), 1_000);
    assert_eq!(
        ledger.live_count(1_000),
        SEARCH_CANCEL_TOMBSTONE_MAX,
        "the ledger is bounded: an abandoned search must not pin memory for the life of the coordinator"
    );

    let expired = SearchIdentity::new("tab", "s", "ancient");
    ledger.record_cancel(&expired, 1_000);
    assert!(
        ledger.live_count(1_000 + SEARCH_CANCEL_TOMBSTONE_TTL_MS + 1) < SEARCH_CANCEL_TOMBSTONE_MAX
    );
    assert!(
        !ledger.consume_cancel(&expired, 1_000 + SEARCH_CANCEL_TOMBSTONE_TTL_MS + 1),
        "an expired tombstone does not retire a search that starts after it"
    );
}

#[test]
fn a_search_needs_a_tab_id_and_a_cancel_falls_back_to_the_fingerprint() {
    let refused = ScrollbackRelay::viewer_id("browser-fp", None).unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidArgument);
    assert!(
        refused.message.unwrap().contains("x-roost-tab-id"),
        "the refusal names the header the caller is missing"
    );
    assert_eq!(
        ScrollbackRelay::viewer_id("browser-fp", Some("tab-1")).unwrap(),
        "browser-fp:tab-1"
    );
    assert_eq!(
        ScrollbackRelay::cancel_viewer_id("browser-fp", None),
        "browser-fp",
        "refusing a cancel would leave a search the caller can no longer reach"
    );
}
