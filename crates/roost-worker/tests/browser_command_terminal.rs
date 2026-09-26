//! What a browser can ask of a live terminal: a page of retained rows and
//! a bounded search, with the fences that stop either answering for a grid
//! that no longer exists.

mod browser_command_support;
use browser_command_support::{
    DIGEST, EPOCH, FINGERPRINT, Harness, HostPlatform, LocalFiles, MapEnv, OTHER_SESSION,
    SESSION, base64_decode, session, command, dispatch, every_kind, floor, frame_of, harness, only,
};
use roost_worker::browser_commands::Command;
use roost_worker::browser_commands::scrollback_page::GridDescription;
use roost_worker::browser_commands::search::Searches;
use roost_worker::browser_commands::search_cancellation::{MAX_TOMBSTONES, TOMBSTONE_TTL};

use roost_worker::scrollback_read::EpochBinding;
use std::sync::Arc;
use serde_json::json;

/// A PAGE THAT STOPPED AT AN EDGE SAYS WHICH. A client that is told nothing
/// retries forever; a client that is told "evicted" can name the floor it is
/// showing.
#[tokio::test]
async fn a_page_clamped_at_an_edge_names_that_edge() {
    let for_evicted = GridDescription {
        binding: EpochBinding::new(EPOCH),
        retained_floor: 60,
        resize_replay_floor: 0,
        total: 100,
        cols: 80,
    };
    assert_eq!(floor(&for_evicted, 10), "evicted");
    assert_eq!(
        floor(&for_evicted, 80),
        "none",
        "a window above the edge is whole"
    );

    let for_replay = GridDescription {
        binding: EpochBinding::new(EPOCH),
        retained_floor: 60,
        resize_replay_floor: 60,
        total: 100,
        cols: 80,
    };
    assert_eq!(floor(&for_replay, 10), "resize_replay");
}

/// A STALE EPOCH IS REFUSED, never served. Rows from a grid that no longer
/// exists splice into the current one invisibly.
#[tokio::test]
async fn a_page_against_a_stale_epoch_is_refused() {
    let harness = harness();
    let refused = only(
        dispatch(
            &roost_worker::browser_commands::Command::decode(
                FINGERPRINT,
                FINGERPRINT,
                "req-1",
                json!({
                    "kind": "get-scrollback-cells",
                    "request_id": "r",
                    "session_id": SESSION,
                    "grid_epoch": "epoch:previous",
                    "end_row": 100,
                    "max_rows": 10,
                }),
            )
            .expect("a stale epoch is still a canonical frame"),
            &harness.deps,
        )
        .await,
    );
    assert_eq!(
        refused.message(),
        Some("`get-scrollback-cells`: grid epoch changed")
    );
}

#[tokio::test]
async fn a_search_reaches_the_scanner_with_the_frame_bounds_it_carried() {
    let harness = harness();
    let reply = only(dispatch(&command(frame_of("search-scrollback")), &harness.deps).await);
    assert!(reply.is_ok());
    let asked = harness.search.single.lock().expect("held")[0].clone();
    assert_eq!(asked.session_id, SESSION);
    assert_eq!(asked.query, "needle");
    assert_eq!(asked.max_rows, 512);
    assert_eq!(asked.max_matches, 64);
    assert_eq!(asked.grid_epoch, EPOCH);
}

#[tokio::test]
async fn a_batch_search_reaches_the_scanner_with_the_sessions_it_named() {
    let harness = harness();
    let reply = only(dispatch(&command(frame_of("search-scrollback-batch")), &harness.deps).await);
    assert!(reply.is_ok());
    let asked = harness.search.batch.lock().expect("held")[0].clone();
    assert_eq!(asked.sessions.len(), 1);
    assert_eq!(asked.sessions[0].0, SESSION);
    assert_eq!(asked.deadline_ms, 5_000);
    assert_eq!(asked.max_rows_per_session, 512);
}

/// A CANCEL THAT ARRIVES FIRST IS HONOURED. The browser walked away before
/// the search it started reached the worker, and the search must not scan a
/// grid nobody is watching.
#[tokio::test]
async fn a_search_that_was_cancelled_before_it_arrived_is_refused() {
    let harness = harness();
    let cancelled = dispatch(
        &command(frame_of("cancel-scrollback-search")),
        &harness.deps,
    )
    .await;
    assert!(cancelled.is_empty(), "a cancel asks for no answer");
    assert!(
        harness.search.single.lock().expect("held").is_empty(),
        "nothing has been scanned yet"
    );

    let refused = only(dispatch(&command(frame_of("search-scrollback")), &harness.deps).await);
    assert_eq!(
        refused.message(),
        Some("`search-scrollback`: search superseded")
    );
    assert!(
        harness.search.single.lock().expect("held").is_empty(),
        "a superseded search is not scanned at all"
    );
}

/// A CANCEL BELONGS TO THE IDENTITY THAT ABANDONED IT. One tab's cancel must
/// not stop another tab's search on the same session.
#[tokio::test]
async fn a_cancel_only_stops_the_search_its_own_identity_abandoned() {
    let harness = harness();
    dispatch(
        &command(frame_of("cancel-scrollback-search")),
        &harness.deps,
    )
    .await;
    let other_tab = roost_worker::browser_commands::Command::decode(
        "another-browser-document",
        "another-browser-document",
        "req-1",
        frame_of("search-scrollback"),
    )
    .expect("a canonical frame decodes");
    let reply = only(dispatch(&other_tab, &harness.deps).await);
    assert!(reply.is_ok(), "another document's search still runs");
    assert_eq!(harness.search.single.lock().expect("held").len(), 1);
}

#[tokio::test]
async fn a_cancelled_batch_is_refused_before_it_scans() {
    let harness = harness();
    dispatch(
        &command(frame_of("cancel-scrollback-search-batch")),
        &harness.deps,
    )
    .await;
    let refused =
        only(dispatch(&command(frame_of("search-scrollback-batch")), &harness.deps).await);
    assert!(
        refused
            .message()
            .is_some_and(|message| message.contains("superseded"))
    );
    assert!(harness.search.batch.lock().expect("held").is_empty());
}

/// THE BOUND IS ON SEARCHES, and a search at the bound is refused with a
/// reason rather than queued behind work the caller cannot see.
#[test]
fn the_search_bound_refuses_the_ninth_search_with_a_reason() {
    let mut searches = Searches::new();
    for index in 0..8 {
        searches
            .admit(&format!("owner-{index}"), "s", false)
            .unwrap_or_else(|error| panic!("search {index} is admitted: {error}"));
    }
    let refusal = searches
        .admit("owner-8", "s", false)
        .expect_err("the ninth is refused");
    assert!(refusal.message().contains("too many"), "{refusal}");

    // A restart under a key already running replaces rather than competes.
    let ticket = searches
        .admit("owner-0", "s-2", false)
        .expect("a restart replaces the search it supersedes");
    searches.finish(&ticket);
    assert_eq!(searches.active(), 7);
}

/// THE LEDGER IS BOUNDED. A loopback peer that sends only cancels must not be
/// able to grow a table until the worker is out of memory.
#[test]
fn the_cancellation_ledger_is_bounded_and_expires() {
    let held = session(SESSION);
    let mut searches = Searches::new();
    for index in 0..(MAX_TOMBSTONES * 4) {
        searches.cancel("owner", &held, &format!("s-{index}"), 0);
    }
    searches
        .admit("owner-key", "s-0", false)
        .expect("a search whose cancel was evicted runs rather than hanging");

    let mut searches = Searches::new();
    searches.cancel("owner", &held, "s-late", 1_000);
    let long_after = 1_000 + TOMBSTONE_TTL.as_millis() as u64 + 1;
    assert!(
        !searches.consume_cancel("owner", &held, "s-late", long_after),
        "a tombstone past its lifetime stops answering"
    );
}
