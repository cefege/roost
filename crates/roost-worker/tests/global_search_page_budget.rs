#![cfg(unix)]
//! What a global content search page is allowed to report, over the production
//! scanner and a real grid.
//!
//! The coordinator narrows a worker's answer against the limits IT ASKED FOR:
//! a page whose scanned range is wider than `max_rows_per_session`, or whose
//! `row_limit` stopped somewhere other than that boundary, is rejected whole
//! and every match it carried is lost with it. So the property under test is
//! not "the scan stops" — it is that a page stops AT the caller's boundary and
//! hands back a cursor a caller can actually continue from.
//!
//! `smoke/terminal/global-search.spec.ts` is the browser proof: it prints one
//! marker, buries it under more rows than one page may read, clicks Load more,
//! and waits for the buried marker to appear. Page one must NOT contain it, and
//! page two must. A scan that reads past its budget satisfies the second
//! without ever reporting a cursor that reaches the first.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod browser_command_support;
use browser_command_support::{SESSION, session};

use roost_host::HostPlatform;
use roost_protocol::terminal_search::{
    GLOBAL_TERMINAL_SEARCH_MAX_MATCHES, GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS,
    GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
};
use roost_term::{AlacrittyCore, CellEmitState, TerminalCore};
use roost_worker::browser_commands::search::{BatchSearch, ScrollbackSearch};
use roost_worker::browser_commands::search_scan::GridScanner;
use roost_worker::event_store::{DurableEventKind, Store};
use roost_worker::session::lifecycle::SessionTable;
use roost_worker::session::ring::ScrollbackRing;
use roost_worker::session::types::{SessionIdentity, SessionRecord};
use roost_worker::shell_spec::ShellSpec;
use serde_json::{Value, json};
use std::sync::Arc;

/// Viewport height. The rows it holds are read as part of every page, so a
/// page's newest boundary is older than the last line printed by this much.
const ROWS: u16 = 24;

/// The marker `global-search.spec.ts` types, and the padding it buries it
/// under — more lines than one page is allowed to read.
const MARKER: &str = "global-8a459306-secondary";
const PADDING: usize = 2_105;

fn identity() -> SessionIdentity {
    SessionIdentity {
        session_id: session(SESSION),
        channel_id: 7i64.try_into().expect("a positive id is a channel id"),
        socket_path: "/run/roost/mux-keeper.sock".to_string(),
        cwd: "/home/almalinux/repos/roost".to_string(),
        shell_spec: ShellSpec {
            version: 1,
            platform: HostPlatform::Linux,
            executable: "/bin/bash".to_string(),
            argv: Vec::new(),
            cwd: "/home/almalinux/repos/roost".to_string(),
            env: vec![("TERM".to_string(), "xterm-256color".to_string())],
        },
        session_trace_id: "aabbccdd11223344".try_into().expect("hex is a trace id"),
        spawned_at_ms: 1_700_000_000_000,
    }
}

/// One session whose grid holds the marker and then far more padding than a
/// single page may read — the shape the browser spec builds by typing.
fn table_burying_a_marker() -> Arc<SessionTable> {
    let mut core = AlacrittyCore::new(120, ROWS);
    let mut output = format!("{MARKER}\r\n");
    for index in 0..PADDING {
        output.push_str(&format!("padding-{index:04}\r\n"));
    }
    core.write(output.as_bytes());
    let mut store = Store::new();
    let reservation = store
        .reserve(DurableEventKind::Closed, 64)
        .expect("the store admits a close claim");
    let record = SessionRecord::new(
        identity(),
        reservation,
        Box::new(core),
        CellEmitState::new("epoch-base", "stream-1"),
        ScrollbackRing::new(1024),
    );
    let table = Arc::new(SessionTable::default());
    table.insert(record).expect("the table admits a session");
    table
}

/// One batch page over one session, named by the cursor a caller holds.
async fn page(table: &Arc<SessionTable>, grid_epoch: &str, before_row: Option<u32>) -> Value {
    let answered = GridScanner::new(Arc::clone(table))
        .search_batch(BatchSearch {
            search_id: "search-1".to_string(),
            query: MARKER.to_string(),
            case_sensitive: false,
            max_rows_per_session: GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
            max_matches: GLOBAL_TERMINAL_SEARCH_MAX_MATCHES,
            deadline_ms: GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS,
            sessions: vec![(SESSION.to_string(), grid_epoch.to_string(), before_row)],
        })
        .await
        .expect("a batch page over a held session answers");
    let entries = answered["entries"].as_array().expect("entries is an array");
    assert_eq!(entries.len(), 1, "one session in, one entry out");
    let entry = &entries[0];
    assert_eq!(entry["status"], json!("ok"), "the entry answered: {entry}");
    entry["result"].clone()
}

fn cursor_of(page: &Value) -> u32 {
    page["next_before_row"]
        .as_u64()
        .expect("a truncated page names the row a caller continues at")
        .try_into()
        .expect("a row number fits the page's own numbering")
}

fn scanned_rows(page: &Value) -> u32 {
    let start = page["scanned_start_row"].as_u64().expect("a start row");
    let end = page["scanned_end_row"].as_u64().expect("an end row");
    u32::try_from(end - start).expect("a scanned range fits a row number")
}

/// THE PAGE BOUNDARY IS THE CALLER'S. A browser pages a search with the cursor
/// the previous page named, so a page that reads past the row budget does not
/// merely over-answer: the coordinator cannot use the page at all, and the
/// matches it scanned are discarded with it.
#[tokio::test]
async fn a_page_reads_exactly_the_rows_it_was_given_and_names_where_to_continue() {
    let table = table_burying_a_marker();
    let first = page(&table, "", None).await;

    assert_eq!(
        first["stop_reason"],
        json!("row_limit"),
        "more retained rows than one page may read: {first}"
    );
    assert_eq!(
        cursor_of(&first),
        u32::try_from(first["scanned_start_row"].as_u64().expect("a start row"))
            .expect("a row number fits the page's own numbering"),
        "the cursor a caller continues at is the oldest row the page reached"
    );
    assert_eq!(
        first["history_floor"],
        json!("none"),
        "older rows are still retained, so a caller may continue: {first}"
    );
    assert!(
        first["matches"]
            .as_array()
            .expect("matches is an array")
            .is_empty(),
        "the marker is older than one page, so page one cannot report it: {first}"
    );
}

/// THE PAGE AFTER THE CURSOR FINDS WHAT THE FIRST COULD NOT REACH. This is the
/// browser spec's Load more, and the only reason the cursor above is worth
/// anything: a caller that resumes where it was told finds the buried marker.
#[tokio::test]
async fn the_page_after_the_cursor_reaches_the_rows_the_first_page_could_not() {
    let table = table_burying_a_marker();
    let first = page(&table, "", None).await;
    let grid_epoch = first["grid_epoch"]
        .as_str()
        .expect("a page names the epoch its next page must hold");
    let second = page(&table, grid_epoch, Some(cursor_of(&first))).await;

    assert_eq!(
        second["scanned_end_row"].as_u64().expect("an end row"),
        cursor_of(&first) as u64,
        "a resumed page starts where the cursor said, exclusively: {second}"
    );
    assert_eq!(
        scanned_rows(&second) as u64,
        second["scanned_end_row"].as_u64().expect("an end row"),
        "the last page reaches the floor, so it is as long as its range: {second}"
    );
    let matches = second["matches"].as_array().expect("matches is an array");
    assert_eq!(
        matches.len(),
        1,
        "the marker one page could not reach, found by the one that could: {second}"
    );
    assert_eq!(matches[0]["preview"], json!(MARKER));
    assert!(
        second.get("next_before_row").is_none(),
        "the floor was reached, so there is no page after this one: {second}"
    );
}

/// THE MATCH CAP IS THE CALLER'S TOO. A coordinator narrows a worker's page
/// against the match budget it funded, and a page that reports more matches
/// than it was given is a page it discards whole — so one row holding several
/// occurrences must not push the page over the cap the rest of the scan was
/// keeping.
#[tokio::test]
async fn a_row_with_more_occurrences_than_the_cap_yields_the_cap() {
    let mut core = AlacrittyCore::new(120, ROWS);
    core.write(b"needle needle needle needle\r\n");
    let mut store = Store::new();
    let reservation = store
        .reserve(DurableEventKind::Closed, 64)
        .expect("the store admits a close claim");
    let record = SessionRecord::new(
        identity(),
        reservation,
        Box::new(core),
        CellEmitState::new("epoch-base", "stream-1"),
        ScrollbackRing::new(1024),
    );
    let table = Arc::new(SessionTable::default());
    table.insert(record).expect("the table admits a session");

    let answered = GridScanner::new(table)
        .search_batch(BatchSearch {
            search_id: "search-1".to_string(),
            query: "needle".to_string(),
            case_sensitive: false,
            max_rows_per_session: GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
            max_matches: 2,
            deadline_ms: GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS,
            sessions: vec![(SESSION.to_string(), String::new(), None)],
        })
        .await
        .expect("a batch page over a held session answers");
    let result = &answered["entries"][0]["result"];

    assert_eq!(
        result["matches"]
            .as_array()
            .expect("matches is an array")
            .len(),
        2,
        "one row holding four occurrences reports the two the page was funded for: {result}"
    );
}
