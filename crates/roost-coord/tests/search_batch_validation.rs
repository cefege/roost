//! The batch-result checks that guard a cursor's row boundary and a page's
//! match identities.
//!
//! Ported from the three `validateGlobalSearchGroupResult` tests at the end of
//! `apps/coord/tests/search/global-search-cursors.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_coord::search::cursor_types::GlobalSearchSessionPosition;
use roost_coord::search::options::GlobalSearchPageLimits;
use roost_coord::search::worker_result::validate_global_search_group_result;
use roost_protocol::terminal_search::{
    GLOBAL_TERMINAL_SEARCH_MAX_MATCHES, GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
    GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
};
use serde_json::json;

const LIMITS: GlobalSearchPageLimits = GlobalSearchPageLimits {
    max_sessions: GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
    max_rows_per_session: GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
    max_matches: GLOBAL_TERMINAL_SEARCH_MAX_MATCHES,
};

fn position() -> GlobalSearchSessionPosition {
    GlobalSearchSessionPosition {
        session_id: "00000000-0000-4000-8000-000000000001".to_owned(),
        worker_fp: "a".repeat(64),
        grid_epoch: "epoch-a".to_owned(),
        before_row: Some(2_048),
    }
}

fn cursor_result(
    scanned_start_row: u64,
    scanned_end_row: u64,
    stop_reason: &str,
    matches: serde_json::Value,
) -> serde_json::Value {
    json!({ "entries": [{
        "status": "ok",
        "session_id": position().session_id,
        "result": {
            "matches": matches,
            "truncated": false,
            "scrollback_total": 3_000,
            "cols": 80,
            "grid_epoch": position().grid_epoch,
            "scanned_start_row": scanned_start_row,
            "scanned_end_row": scanned_end_row,
            "history_floor": "none",
            "stop_reason": stop_reason,
        },
    }] })
}

// "rejects a same-epoch cursor result that skips the requested boundary"
#[test]
fn a_same_epoch_result_that_skips_the_requested_boundary_is_malformed() {
    let raw = cursor_result(0, 1_024, "complete", json!([]));
    assert_eq!(
        validate_global_search_group_result(&raw, &[position()], 8, &LIMITS),
        None
    );
}

// "accepts a same-epoch result stopped by a mid-scan epoch change"
#[test]
fn a_same_epoch_result_stopped_by_a_mid_scan_epoch_change_is_accepted() {
    let raw = cursor_result(1_548, 2_048, "epoch_changed", json!([]));
    assert!(validate_global_search_group_result(&raw, &[position()], 8, &LIMITS).is_some());
}

// "rejects duplicate match identities from a worker batch"
#[test]
fn duplicate_match_identities_in_a_batch_are_malformed() {
    let found = json!({ "row": 2_000, "col": 4, "len": 6, "preview": "needle" });
    let raw = cursor_result(0, 2_048, "complete", json!([found, found]));
    assert_eq!(
        validate_global_search_group_result(&raw, &[position()], 8, &LIMITS),
        None
    );
}

/// v2's `WorkerGlobalSearchResultSchema` and `WorkerSearchScrollbackResultSchema`
/// are `.strict()`: a batch, an entry, or a result naming a field the contract
/// does not is malformed, never half-read.
#[test]
fn a_field_outside_the_contract_makes_the_batch_malformed() {
    let valid = cursor_result(0, 2_048, "complete", json!([]));
    assert!(validate_global_search_group_result(&valid, &[position()], 8, &LIMITS).is_some());
    let mut extra_result = valid.clone();
    extra_result["entries"][0]["result"]["regex"] = json!(true);
    let mut extra_entry = valid.clone();
    extra_entry["entries"][0]["worker"] = json!("a");
    let mut extra_batch = valid;
    extra_batch["complete"] = json!(true);
    for raw in [extra_result, extra_entry, extra_batch] {
        assert_eq!(
            validate_global_search_group_result(&raw, &[position()], 8, &LIMITS),
            None,
            "{raw}"
        );
    }
}
