//! The production retained grid, read the way a browser command reads it: the
//! epoch fence, the two bounds and the slice walk. Each of those is a place
//! where a second, almost-right answer produces a scrollback that looks right
//! and is not. How a row is SPELLED is `tests/cell_row_json.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod browser_command_support;
use browser_command_support::{OTHER_SESSION, SESSION, command, dispatch, harness, only, session};
use roost_host::HostPlatform;
use roost_term::{AlacrittyCore, CellEmitState};
use roost_worker::browser_commands::scrollback_page::RetainedGrid;
use roost_worker::event_store::{DurableEventKind, Store};
use roost_worker::scrollback_read::{
    Request, SCROLLBACK_MAX_ROWS_PER_PAGE, SCROLLBACK_SLICE_ROWS, WalkOutcome, page_for,
    slice_count, walk_page,
};
use roost_worker::session::lifecycle::SessionTable;
use roost_worker::session::retained_grid::SessionGrid;
use roost_worker::session::ring::ScrollbackRing;
use roost_worker::session::types::{SessionIdentity, SessionRecord};
use roost_worker::shell_spec::ShellSpec;
use serde_json::json;
use std::sync::{Arc, Mutex};

/// Viewport height, and the number of lines fed to it. The difference is the
/// history the grid ends up holding, so the two are named rather than threaded
/// around as a bare number.
const ROWS: u16 = 2;
const LINES: usize = 6;

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

/// A record whose core has already produced enough output to have scrolled, so
/// its grid holds real history rather than a viewport and nothing else.
fn record() -> SessionRecord {
    let mut core = AlacrittyCore::new(24, ROWS);
    let mut output = String::new();
    for index in 0..LINES {
        output.push_str(&format!("line{index}\r\n"));
    }
    core.write(output.as_bytes());
    let mut store = Store::new();
    let reservation = store
        .reserve(DurableEventKind::Closed, 64)
        .expect("the store admits a close claim");
    SessionRecord::new(
        identity(),
        reservation,
        Box::new(core),
        CellEmitState::new("epoch-base", "stream-1"),
        ScrollbackRing::new(1024),
    )
}

/// A table holding one live session, and the handle that can reframe it.
fn table() -> (Arc<SessionTable>, Arc<Mutex<SessionRecord>>) {
    let table = Arc::new(SessionTable::default());
    let held = table.insert(record()).expect("the table admits a session");
    (table, held)
}

fn request(grid_epoch: &str, end_row: u32, max_rows: u32) -> Request {
    Request {
        grid_epoch: grid_epoch.to_string(),
        end_row,
        max_rows,
    }
}

/// THE FENCE, at the level that matters: a description taken before a reframe
/// must not be usable to build a page afterwards.
///
/// The rows a spliced read returns would describe a grid that no longer exists,
/// and the splice is invisible — which is why this is a refusal and not a short
/// page.
#[tokio::test]
async fn a_read_against_a_replaced_grid_epoch_is_refused() {
    let (table, held) = table();
    let grid = SessionGrid::new(table);
    let before = grid
        .describe(session(SESSION))
        .await
        .expect("a live session describes");
    let page = page_for(
        &request(before.binding.current(), before.total, 2),
        before.total,
        before.cols,
        &before.binding,
    )
    .expect("the first read binds to the current epoch");
    assert_eq!(page.grid_epoch, before.binding.current());

    // A reframe: a resize, a repair, a reattach. The revision is the only thing
    // that moves, and it is the whole identity.
    held.lock().expect("held").cell_emit.grid_epoch_revision += 1;

    let after = grid
        .describe(session(SESSION))
        .await
        .expect("the session is still live after a reframe");
    assert_ne!(
        before.binding.current(),
        after.binding.current(),
        "a reframe is a new grid, or the fence has nothing to fence"
    );
    assert_eq!(
        page_for(
            &request(page.grid_epoch.as_str(), after.total, 2),
            after.total,
            after.cols,
            &after.binding,
        ),
        Err(roost_worker::scrollback_read::Refusal::StaleEpoch),
        "rows from the previous grid are not served, because splicing two \
         grids produces a scrollback with a hole nobody can see"
    );
}

/// A request for more than one page is CLAMPED. Refusing it would fail a
/// well-formed request, and the client is explicitly allowed to ask for more
/// than a page and issue several per wave.
#[tokio::test]
async fn a_page_larger_than_the_ceiling_is_clamped_not_refused() {
    let (table, _held) = table();
    let grid = SessionGrid::new(table);
    let described = grid
        .describe(session(SESSION))
        .await
        .expect("a live session describes");
    let total = described.total;
    assert!(
        total > 0 && total < SCROLLBACK_MAX_ROWS_PER_PAGE,
        "this grid is smaller than the ceiling, so the clamp is observable"
    );
    let page = page_for(
        &request("", total, SCROLLBACK_MAX_ROWS_PER_PAGE * 10),
        total,
        described.cols,
        &described.binding,
    )
    .expect("an over-large request is clamped, never refused");
    assert_eq!(page.row_count(), total, "everything the grid holds");
    assert_eq!(page.total, total);
    assert!(!page.has_more, "there is nothing above the end");
}

/// THE SLICE BOUNDS THE WALK, NOT THE RESPONSE, and a page at the ceiling must
/// take more than one of them. Every OTHER session's PTY output is stalled for
/// the length of a slice, so a page that quietly became ten slices would stall
/// the whole worker ten times over for one browser's backfill.
#[test]
fn a_full_page_takes_more_than_one_slice() {
    let slices = slice_count(SCROLLBACK_MAX_ROWS_PER_PAGE);
    assert!(
        slices > 1,
        "a page at the ceiling must be walked in several slices"
    );
    assert_eq!(
        slices * SCROLLBACK_SLICE_ROWS,
        SCROLLBACK_MAX_ROWS_PER_PAGE,
        "and the slices tile the page exactly, with no row walked twice"
    );
}

/// A read that loses its authority BETWEEN SLICES must not be presentable as a
/// page. The walk stops at a slice boundary, so a partial result is always a
/// whole number of slices — which is what makes "abandon or resume" a decision a
/// caller can actually take rather than a hole nobody can see.
#[tokio::test]
async fn a_read_stopped_between_slices_leaves_no_half_taken_page() {
    let (table, _held) = table();
    let grid = SessionGrid::new(table);
    let described = grid
        .describe(session(SESSION))
        .await
        .expect("a live session describes");
    let end_row = described.total;
    let page = page_for(
        &request("", end_row, SCROLLBACK_MAX_ROWS_PER_PAGE),
        end_row,
        described.cols,
        &described.binding,
    )
    .expect("a full page");

    let mut checks = 0_u32;
    let outcome = walk_page(
        &page,
        |_| true,
        || {
            checks += 1;
            checks <= 2
        },
    );

    assert!(
        !outcome.is_complete(),
        "a read that lost authority is not a page"
    );
    assert!(
        matches!(outcome, WalkOutcome::Cancelled { .. }),
        "and it says so, rather than looking like a short page: {outcome:?}"
    );
    let taken = outcome.taken().len() as u32;
    assert_eq!(
        taken % SCROLLBACK_SLICE_ROWS,
        0,
        "a cancelled read stops on a slice boundary, never mid-slice"
    );
    assert!(
        taken < page.row_count(),
        "and the part of the page it did not finish is not presented as finished"
    );
}

/// A row the grid no longer holds reads as an empty range rather than as a row
/// under the wrong index, which is the difference between a short page and a
/// silently spliced one.
#[tokio::test]
async fn a_row_the_grid_no_longer_holds_is_not_served() {
    let (table, _held) = table();
    let grid = SessionGrid::new(table);
    let total = grid
        .describe(session(SESSION))
        .await
        .expect("a live session describes")
        .total;
    assert!(total > 0, "the core scrolled, so it holds history");

    assert!(
        grid.row(session(SESSION), total).await.is_none(),
        "the row one past the end is not a row"
    );
    assert!(
        grid.row(session(SESSION), u32::MAX).await.is_none(),
        "nor is one far beyond it"
    );
    let first = grid
        .row(session(SESSION), 0)
        .await
        .expect("the oldest retained row is served");
    assert_eq!(
        first["index"].as_u64(),
        Some(0),
        "under the index the caller asked for"
    );
}

/// AN UNKNOWN SESSION IS A REFUSAL, not an empty page. A browser told "nothing
/// here" about a session this worker has lost cannot tell it apart from a
/// session that has produced no output.
#[tokio::test]
async fn a_session_this_worker_does_not_hold_is_refused() {
    let (table, _held) = table();
    let grid = SessionGrid::new(table);
    let unknown = session(OTHER_SESSION);
    let refusal = grid
        .describe(unknown)
        .await
        .expect_err("an unknown session has no grid to describe");
    assert_eq!(refusal.message(), "`get-scrollback-cells`: unknown session");
    assert!(grid.row(unknown, 0).await.is_none());
}

/// THE COMMAND PATH, end to end, over the production grid. The fake this
/// replaces answers any row index with a synthetic one; this answers with what
/// the core actually holds.
#[tokio::test]
async fn the_page_command_serves_the_rows_the_core_holds() {
    let (table, _held) = table();
    let mut harness = harness();
    harness.deps.grid = Arc::new(SessionGrid::new(table));
    let described = harness
        .deps
        .grid
        .describe(session(SESSION))
        .await
        .expect("a live session describes");
    let total = described.total;
    let epoch = described.binding.current().to_string();

    let reply = only(
        dispatch(
            &command(json!({
                "kind": "get-scrollback-cells",
                "request_id": "r",
                "session_id": SESSION,
                "grid_epoch": "",
                "end_row": total,
                "max_rows": 100,
            })),
            &harness.deps,
        )
        .await,
    );
    let data = reply.data().expect("a page is served");
    assert_eq!(data["total"].as_u64(), Some(u64::from(total)));
    assert_eq!(
        data["rows"].as_array().map(Vec::len),
        Some(total as usize),
        "one row per row the request named"
    );
    assert_eq!(
        data["rows"][0]["index"].as_u64(),
        Some(0),
        "and the first row is the one at the start of the window"
    );
    assert_eq!(
        data["grid_epoch"].as_str(),
        Some(epoch.as_str()),
        "the page says which grid it came from, so the next request can be fenced"
    );
}

/// A span a REAL core produces must carry the same fields as one a hand-built
/// row does, because the browser reads both.
#[tokio::test]
async fn every_span_a_real_core_produces_carries_its_own_fields() {
    let (table, _held) = table();
    let grid = SessionGrid::new(table);
    let row = grid
        .row(session(SESSION), 0)
        .await
        .expect("the oldest retained row is served");
    let spans = row["spans"].as_array().expect("a row carries spans");
    assert!(!spans.is_empty(), "a painted line is not an empty row");
    for span in spans {
        for field in ["text", "fg", "bg", "flags", "columns"] {
            assert!(
                !span[field].is_null(),
                "{field} is present on a span from a real core: {span}"
            );
        }
    }
}

/// The epoch a description reports is the one the emit state stamps on the next
/// frame. It is read from the CORE rather than from the last emitted
/// observation, because a stale origin shifts every absolute index these rows
/// resolve through — worse than a short page, because it is invisible.
#[tokio::test]
async fn the_epoch_a_page_is_bound_to_is_the_one_the_emitter_will_stamp() {
    let (table, _held) = table();
    let grid = SessionGrid::new(table.clone());
    let described = grid
        .describe(session(SESSION))
        .await
        .expect("a live session describes");
    let stamped = table
        .with_record(&session(SESSION), |record| record.cell_emit.grid_epoch())
        .expect("the session is live");
    assert_eq!(
        described.binding.current(),
        stamped,
        "and the fingerprint the page is fenced on is the emitter's own"
    );
}
