//! A page the browser receives is a CONTRACT, and the contract is
//! contiguity: `rows[i].index == start_row + i` for every row served. A page
//! that skips a row is not a short page — it is a scrollback with a hole in it
//! that the client splices over silently, which is the failure a multi-megabyte
//! direct scan sees when the ring evicts under it.
//!
//! Two properties are pinned here, and they are separate: a window that starts
//! BELOW the retained floor is clamped up to it, and a row that reads absent
//! mid-page ends the page rather than being dropped from the middle of it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod browser_command_support;
use browser_command_support::{SESSION, command, dispatch, harness, only};
use roost_worker::browser_commands::Boxed;
use roost_worker::browser_commands::Refusal as CommandRefusal;
use roost_worker::browser_commands::scrollback_page::{GridDescription, RetainedGrid};
use roost_worker::scrollback_read::EpochBinding;
use roost_worker::session::retained_grid::CellRowJson;
use serde_json::json;
use std::sync::Arc;

const EPOCH: &str = "epoch-1";
const TOTAL: u32 = 4_000;
const FLOOR: u32 = 900;
/// The one retained row the ring lost between the description and the read.
const LOST_ROW: u32 = 905;

/// A grid whose floor has moved to [`FLOOR`] and that has lost row 905 — a
/// saturated ring, mid-eviction, which is the state a multi-megabyte scan
/// reads the worker in.
struct EvictingGrid;

impl RetainedGrid for EvictingGrid {
    fn describe(
        &self,
        _session_id: roost_protocol::wire::brand::SessionId,
    ) -> Boxed<Result<GridDescription, CommandRefusal>> {
        Box::pin(async move {
            Ok(GridDescription {
                binding: EpochBinding::new(EPOCH),
                retained_floor: FLOOR,
                resize_replay_floor: 0,
                total: TOTAL,
                cols: 80,
                viewport_rows: 24,
            })
        })
    }

    fn row(
        &self,
        _session_id: roost_protocol::wire::brand::SessionId,
        absolute_row: u32,
    ) -> Boxed<Option<CellRowJson>> {
        let present = absolute_row >= FLOOR && absolute_row != LOST_ROW;
        Box::pin(async move {
            present.then(|| {
                CellRowJson::owned(roost_protocol::cell::CellRow {
                    index: absolute_row,
                    spans: std::sync::Arc::from(vec![roost_protocol::cell::CellSpan {
                        text: format!("row {absolute_row}"),
                        fg: 0,
                        bg: 0,
                        flags: 0,
                        fg_rgb: None,
                        bg_rgb: None,
                        columns: 0,
                        link_uri: None,
                        link_key: None,
                    }]),
                })
            })
        })
    }
}

async fn page(end_row: i64, max_rows: i64) -> serde_json::Value {
    let mut harness = harness();
    harness.deps.grid = Arc::new(EvictingGrid);
    only(
        dispatch(
            &command(json!({
                "kind": "get-scrollback-cells",
                "request_id": "r",
                "session_id": SESSION,
                "grid_epoch": "",
                "end_row": end_row,
                "max_rows": max_rows,
            })),
            &harness.deps,
        )
        .await,
    )
    .data()
    .expect("a page is served")
    .clone()
}

/// Every index in a served page names its own place, with no gap and no
/// repeat. This is the assertion the oracle's retained-marker scan makes, and
/// the one a hole fails.
fn assert_contiguous(data: &serde_json::Value) {
    let start = data["start_row"].as_u64().expect("a start row");
    let rows = data["rows"].as_array().expect("rows are an array");
    for (offset, row) in rows.iter().enumerate() {
        assert_eq!(
            row["index"].as_u64(),
            Some(start + offset as u64),
            "every row of a page is the row at its own index, so a client can splice it"
        );
    }
}

/// A window reaching below the retained floor is served from the floor, so its
/// first row is a row the grid holds. Before the fix the page began at 800 and
/// the 100 rows before the floor read absent and were skipped, which is a hole
/// at the front of every page a backwards reader asks for.
#[tokio::test]
async fn a_window_below_the_retained_floor_starts_at_the_floor() {
    let data = page(i64::from(TOTAL), 512).await;
    let start = data["start_row"].as_u64().expect("a start row");
    assert!(
        start >= u64::from(FLOOR),
        "a page never names a row the core no longer holds: started at {start}, floor {FLOOR}"
    );
    assert_contiguous(&data);
}

/// A row that reads absent ENDS the page. Skipping it would leave a hole in the
/// middle of the rows, and a client reading backwards from `end_row` would
/// splice the far side of that hole onto the near side as though the history
/// had always been continuous there.
#[tokio::test]
async fn a_row_the_grid_lost_ends_the_page_instead_of_holeing_it() {
    // Start past the lost row so the walk reaches it.
    let data = page(1_000, 200).await;
    let rows = data["rows"].as_array().expect("rows are an array");
    assert!(
        !rows.is_empty(),
        "the rows before the lost one are still served: a short page is a page"
    );
    assert_contiguous(&data);
    let end = data["end_row"].as_u64().expect("an end row");
    let last = rows
        .last()
        .and_then(|row| row["index"].as_u64())
        .expect("a last row");
    assert_eq!(
        end,
        last + 1,
        "`end_row` is one past the row actually served, so the next page starts there"
    );
}

/// The row the grid lost is 905 and the floor is 900, so a page that both
/// clamped to the floor and stopped at the hole serves 900..=904 and nothing
/// else — and says so with `end_row`, which is what makes the read resumable
/// instead of silently short.
#[tokio::test]
async fn the_served_window_is_exactly_what_the_grid_still_holds() {
    let data = page(1_000, 200).await;
    assert_eq!(data["start_row"].as_u64(), Some(u64::from(FLOOR)));
    assert_eq!(data["end_row"].as_u64(), Some(905));
    assert_eq!(
        data["rows"].as_array().map(Vec::len),
        Some(5),
        "900 through 904: every retained row before the hole, and not one past it"
    );
    assert_contiguous(&data);
}
