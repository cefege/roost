//! What a browser-to-worker control frame is ALLOWED to ask for: the search
//! page budgets, the capture evidence cap, the trace-id shape, and the
//! positions a frame names.
//!
//! Every bound is pinned at its endpoint and one entry past it, because these
//! are the limits that stop one client's demand from becoming another
//! machine's work. Which kinds exist, and how each is decoded, is the other
//! half, in `control_frames.rs`.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use serde_json::json;

use roost_protocol::terminal_capture::TERMINAL_CAPTURE_EVIDENCE_MAX_CHARS;
use roost_protocol::terminal_search::{
    GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS, GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
    TERMINAL_SEARCH_MAX_MATCHES, TERMINAL_SEARCH_MAX_ROWS,
};
use roost_protocol::wire::control::ClientControlFrame;

use support::{RECORDING, frame_of};

#[test]
fn a_batch_deadline_is_the_contract_value_and_not_a_range() {
    let mut batch = frame_of("search-scrollback-batch");
    assert!(ClientControlFrame::parse(batch.clone()).is_ok());
    for deadline in [
        GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS - 1,
        GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS + 1,
        0,
    ] {
        batch["deadline_ms"] = json!(deadline);
        assert!(
            ClientControlFrame::parse(batch.clone()).is_err(),
            "a deadline of {deadline} was admitted"
        );
    }
}

#[test]
fn a_page_refuses_more_rows_or_matches_than_the_contract_funds() {
    let mut search = frame_of("search-scrollback");
    search["max_rows"] = json!(TERMINAL_SEARCH_MAX_ROWS + 1);
    assert!(ClientControlFrame::parse(search.clone()).is_err());
    search["max_rows"] = json!(0);
    assert!(ClientControlFrame::parse(search.clone()).is_err());
    search["max_rows"] = json!(TERMINAL_SEARCH_MAX_ROWS);
    search["max_matches"] = json!(TERMINAL_SEARCH_MAX_MATCHES + 1);
    assert!(ClientControlFrame::parse(search.clone()).is_err());
    search["max_matches"] = json!(0);
    assert!(ClientControlFrame::parse(search).is_err());

    let mut batch = frame_of("search-scrollback-batch");
    batch["max_rows_per_session"] = json!(GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION + 1);
    assert!(ClientControlFrame::parse(batch).is_err());
}

#[test]
fn a_search_string_past_its_bound_is_refused() {
    let mut search = frame_of("search-scrollback");
    search["query"] = json!("e".repeat(257));
    assert!(ClientControlFrame::parse(search.clone()).is_err());
    search["query"] = json!("e".repeat(256));
    assert!(ClientControlFrame::parse(search.clone()).is_ok());
    search["grid_epoch"] = json!("e".repeat(65));
    assert!(ClientControlFrame::parse(search.clone()).is_err());
    search["grid_epoch"] = json!("epoch:1");
    search["search_id"] = json!("");
    assert!(ClientControlFrame::parse(search).is_err());
}

#[test]
fn a_history_page_epoch_is_the_callers_own_string_while_a_search_epoch_is_bounded() {
    // Two different fields with two different producers: a history page echoes
    // the epoch the browser holds, a search epoch is written by the find
    // controller. Unifying their bounds would refuse pages a browser can send.
    let mut page = frame_of("get-scrollback-cells");
    page["grid_epoch"] = json!("e".repeat(512));
    assert!(ClientControlFrame::parse(page).is_ok());
}

#[test]
fn a_capture_frame_defaults_its_evidence_and_refuses_more_than_the_cap() {
    let frame = ClientControlFrame::parse(frame_of("diag-terminal-capture"))
        .expect("a capture without evidence is valid");
    match frame {
        ClientControlFrame::DiagTerminalCapture {
            browser_evidence_json,
            coordinator_evidence_json,
            ..
        } => {
            assert_eq!(browser_evidence_json, "");
            assert_eq!(coordinator_evidence_json, "");
        }
        other => panic!("expected a capture, got {other:?}"),
    }

    let at_cap = "e".repeat(TERMINAL_CAPTURE_EVIDENCE_MAX_CHARS);
    let over_cap = format!("{at_cap}e");
    let mut capture = frame_of("diag-terminal-capture");
    capture["browser_evidence_json"] = json!(at_cap);
    assert!(ClientControlFrame::parse(capture.clone()).is_ok());
    capture["browser_evidence_json"] = json!(over_cap);
    assert!(ClientControlFrame::parse(capture).is_err());
}

#[test]
fn a_capture_frame_names_an_action_and_a_reason_this_build_implements() {
    for action in ["start", "capture", "stop"] {
        let mut capture = frame_of("diag-terminal-capture");
        capture["action"] = json!(action);
        assert!(ClientControlFrame::parse(capture).is_ok());
    }
    for reason in [
        "manual",
        "history_identity",
        "viewport_model",
        "worker_emission",
        "pre_repair",
    ] {
        let mut capture = frame_of("diag-terminal-capture");
        capture["reason"] = json!(reason);
        assert!(ClientControlFrame::parse(capture).is_ok());
    }
    let mut unknown_action = frame_of("diag-terminal-capture");
    unknown_action["action"] = json!("resume");
    assert!(ClientControlFrame::parse(unknown_action).is_err());
    let mut unknown_reason = frame_of("diag-terminal-capture");
    unknown_reason["reason"] = json!("because");
    assert!(ClientControlFrame::parse(unknown_reason).is_err());
}

#[test]
fn a_capture_frame_names_its_recording_and_capture_as_uuids() {
    let mut capture = frame_of("diag-terminal-capture");
    capture["recording_id"] = json!("not-a-uuid");
    assert!(ClientControlFrame::parse(capture.clone()).is_err());
    capture["recording_id"] = json!(RECORDING);
    capture["capture_id"] = json!("17");
    assert!(ClientControlFrame::parse(capture).is_err());
}

#[test]
fn a_trace_id_is_carried_through_and_is_held_to_its_shape() {
    let mut attach = frame_of("attach");
    attach["trace_id"] = json!("deadbeef");
    let frame = ClientControlFrame::parse(attach.clone()).expect("a traced frame parses");
    match &frame {
        ClientControlFrame::Attach { trace_id, .. } => {
            assert_eq!(trace_id.as_ref().map(|id| id.as_str()), Some("deadbeef"));
        }
        other => panic!("expected an attach, got {other:?}"),
    }
    assert!(ClientControlFrame::parse(attach.clone()).is_ok());
    attach["trace_id"] = json!("short");
    assert!(ClientControlFrame::parse(attach).is_err());
}

#[test]
fn a_negative_where_a_position_is_named_is_refused() {
    let mut cursor = frame_of("cursor-pos");
    cursor["col"] = json!(-1);
    assert!(ClientControlFrame::parse(cursor.clone()).is_err());
    cursor["col"] = json!(0);
    cursor["row"] = json!(-1);
    assert!(ClientControlFrame::parse(cursor.clone()).is_err());
    cursor["row"] = json!(0);
    assert!(ClientControlFrame::parse(cursor).is_ok());

    let mut chunk = frame_of("read-file-chunk");
    chunk["offset"] = json!(-1);
    assert!(ClientControlFrame::parse(chunk.clone()).is_err());
    chunk["offset"] = json!(0);
    chunk["len"] = json!(0);
    assert!(ClientControlFrame::parse(chunk).is_err());
}
