//! The window a scrollback page request names, as the worker narrows it.
//!
//! This owns one process-level property: the narrowing of `end_row` is a CLAMP
//! against the grid the reader is being answered from, not a validation that
//! happens before the reader is consulted. A reader walking backwards from the
//! end of the scrollback does not know the total yet, so it sends the largest
//! safe integer a JavaScript `number` can hold — the smoke harness's
//! retained-marker scan does exactly that — and the answer it is owed is the
//! last page the grid holds. Refusing the sentinel instead makes every "scan
//! the whole retained scrollback" reader fail before it reaches the one reader
//! in this crate whose entire job is to answer it.
//!
//! So the fences that are NOT the clamp are pinned here too, because a clamp is
//! exactly the kind of change that invites swallowing the other two: a negative
//! index has no clamp to mean anything by, and an empty request is the reader's
//! own refusal rather than this file's. Each is asked twice — of the frame the
//! worker's front door admits, and of the narrowing itself — because they are
//! two admissions and a change to one is not a change to the other.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod browser_command_support;

use browser_command_support::{EPOCH, FakeGrid, SESSION, harness};
use roost_protocol::wire::control::ClientControlFrame;
use roost_worker::browser_commands::scrollback_page;
use roost_worker::browser_commands::{Answered, Command, Refusal};
use serde_json::{Value, json};

/// What `Number.MAX_SAFE_INTEGER` is once it has crossed the wire. Spelled as
/// the literal the JavaScript client sends rather than as `i64::MAX`, because
/// the value IS the contract here — a reader that sent `i64::MAX` and one that
/// sent this are the same reader, and only one of them is the one that broke.
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// One page request as the narrowing sees it.
fn page_request(end_row: i64, max_rows: i64) -> Command {
    Command::new(
        "browser",
        "viewer",
        "req-window",
        ClientControlFrame::GetScrollbackCells {
            request_id: "r".to_owned(),
            session_id: browser_command_support::session(SESSION),
            grid_epoch: EPOCH.to_owned(),
            end_row,
            max_rows,
            trace_id: None,
        },
    )
}

/// The same request as it crosses the worker's front door, which admits a frame
/// to its OWN bounds before anything narrows it a second time.
fn page_request_over_the_wire(end_row: i64, max_rows: i64) -> Result<Command, String> {
    Command::decode(
        "browser",
        "viewer",
        "req-window",
        json!({
            "kind": "get-scrollback-cells",
            "request_id": "r",
            "session_id": SESSION,
            "grid_epoch": EPOCH,
            "end_row": end_row,
            "max_rows": max_rows,
        }),
    )
}

/// The page a command was answered with, insisting it was a success.
fn page(answered: Result<Answered, Refusal>) -> Value {
    match answered {
        Ok(Answered::Reply(reply)) => {
            assert!(reply.is_ok(), "a served page is an ok reply");
            assert_eq!(
                reply.request_id(),
                "req-window",
                "correlated to its request"
            );
            reply.data().expect("an ok reply carries a payload").clone()
        }
        Ok(Answered::Silent) => panic!("a page request is answered, not ignored"),
        Err(refusal) => panic!("a page request was refused: {}", refusal.message()),
    }
}

/// The message a command was refused with.
fn refused(answered: Result<Answered, Refusal>) -> String {
    match answered {
        Ok(Answered::Reply(reply)) => panic!("expected a refusal, got a reply: {reply:?}"),
        Ok(Answered::Silent) => panic!("expected a refusal, got silence"),
        Err(refusal) => refusal.message(),
    }
}

/// A READER THAT DOES NOT KNOW THE TOTAL STILL GETS A PAGE. The sentinel is
/// what the JS client sends on its first backwards walk, and the page it is
/// owed ends at the grid's own last row rather than failing.
#[tokio::test]
async fn a_reader_that_does_not_know_the_total_gets_the_last_page() {
    let harness = harness();
    let answered =
        scrollback_page::execute(&page_request(MAX_SAFE_INTEGER, 20), &harness.deps).await;
    let page = page(answered);

    let total = page["total"].as_u64().expect("a page names its total");
    assert!(
        total > 0,
        "a grid with no rows cannot tell a clamp from a served window"
    );
    assert_eq!(
        page["end_row"].as_u64(),
        Some(total),
        "the page ends at the last row the grid holds"
    );
    assert_eq!(
        page["start_row"].as_u64(),
        Some(total - 20),
        "and starts one window back from it"
    );
    assert_eq!(
        page["rows"].as_array().map(Vec::len),
        Some(20),
        "the window asked for is the window served"
    );
    assert_eq!(page["grid_epoch"], json!(EPOCH));
    assert_eq!(page["history_floor"], json!("none"));
}

/// THE SAME READER, AS THE WIRE CARRIES IT. The sentinel has to survive the
/// front door too, or the clamp downstream is never reached and the reader
/// fails with a decode cause instead.
#[tokio::test]
async fn the_sentinel_survives_the_front_door_and_is_still_served() {
    let harness = harness();
    let decoded = page_request_over_the_wire(MAX_SAFE_INTEGER, 20)
        .expect("a reader that does not know the total is a canonical frame");
    let page = page(scrollback_page::execute(&decoded, &harness.deps).await);
    assert_eq!(page["end_row"], page["total"]);
    assert_eq!(page["rows"].as_array().map(Vec::len), Some(20));
}

/// A NEGATIVE INDEX IS NOT A CLAMP. No end of a scrollback lies below row zero,
/// so there is nothing for a clamp to mean and the request is refused with a
/// cause the reader can act on.
#[tokio::test]
async fn a_negative_end_row_is_still_refused() {
    let harness = harness();
    let message = refused(scrollback_page::execute(&page_request(-1, 20), &harness.deps).await);
    assert_eq!(
        message, "`get-scrollback-cells`: end_row is negative",
        "a negative index keeps its own refusal, not the reader's"
    );
    let admission = page_request_over_the_wire(-1, 20)
        .expect_err("the front door refuses it before the narrowing does");
    assert!(
        admission.contains("end_row"),
        "the front door names the field: {admission}"
    );
}

/// AN EMPTY REQUEST IS THE READER'S REFUSAL. `page_for` owns that answer and
/// its reason; a second one in the narrowing would be two answers to one empty
/// request.
#[tokio::test]
async fn an_empty_page_request_is_refused_by_the_reader() {
    let harness = harness();
    let message =
        refused(scrollback_page::execute(&page_request(MAX_SAFE_INTEGER, 0), &harness.deps).await);
    assert_eq!(
        message, "`get-scrollback-cells`: the request named no rows",
        "zero rows is refused with the reader's reason, not a narrowing one"
    );
    let admission = page_request_over_the_wire(MAX_SAFE_INTEGER, 0)
        .expect_err("the front door refuses it before the narrowing does");
    assert!(
        admission.contains("max_rows"),
        "the front door names the field: {admission}"
    );
}

/// A WINDOW INSIDE THE GRID IS SERVED WHOLE. The clamp is a ceiling, not a
/// rewrite: a reader that names rows the grid holds gets exactly those rows.
#[tokio::test]
async fn a_window_inside_the_grid_is_served_whole() {
    let harness = harness();
    let page = page(scrollback_page::execute(&page_request(50, 10), &harness.deps).await);
    assert_eq!(page["start_row"], json!(40));
    assert_eq!(page["end_row"], json!(50));
    assert_eq!(page["rows"].as_array().map(Vec::len), Some(10));
    assert_eq!(page["cols"], json!(80));
}

/// A PAGE THAT SERVED EVERYTHING MUST NOT CLAIM IT HIT THE FLOOR.
///
/// The floor is the one thing a reader uses to decide whether paging further is
/// pointless, so a page that reports `Evicted` when it never reached the edge
/// stops a reader early and leaves scrollback it could still have had. This is
/// the case where it went wrong: `page_for` clamps `max_rows` to the page
/// ceiling (2 000) and derives `start_row` from the clamped value, so the
/// wanted window has to be read off the PAGE. Re-deriving it from the raw
/// request asked for a window thousands of rows lower, and any retained floor
/// between the two turned into a false `Evicted`.
///
/// The numbers are chosen so the two readings disagree: the served window is
/// rows 18 000–20 000, the floor is 15 000, and the wrongly-derived window
/// reaches down to 12 000 — below the floor — so the broken code reports
/// `Evicted` and the correct code reports `None`.
#[tokio::test]
async fn a_page_that_served_everything_does_not_claim_it_reached_the_floor() {
    let mut harness = harness();
    harness.deps.grid = std::sync::Arc::new(FakeGrid::evicting(20_000, 15_000));

    let page = page(scrollback_page::execute(&page_request(20_000, 8_000), &harness.deps).await);

    assert_eq!(
        page["start_row"].as_u64(),
        Some(18_000),
        "the page is the window the CEILING allowed, not the window the request asked for"
    );
    assert_eq!(
        page["history_floor"],
        json!("none"),
        "a page whose every row is above the retained floor did not reach that floor, \
         and telling the reader otherwise stops it paging for scrollback it could have had"
    );
}
