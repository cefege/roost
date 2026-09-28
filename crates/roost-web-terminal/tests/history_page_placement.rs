//! Where a page of history rows may land, and how the painted set stays bounded
//! while the scroll space keeps describing the whole session.
//!
//! The painted set is deliberately NOT contiguous: a viewport-only checkpoint
//! reserves the interval it pushed as an unpainted gap, and a page fills that
//! gap later. That is the only arrangement in which a repainted TUI block cannot
//! be frozen into history — because the renderer never infers which rows left
//! the grid, it waits to be told.

use std::sync::Arc;

use roost_protocol::cell::{CellRow, CellSpan};
use roost_web_terminal::painted_history::{
    MAX_HELD_SCROLLBACK_ROWS, PaintedHistory, page_is_contiguous, plan_eviction,
};

fn row(index: u32) -> CellRow {
    CellRow {
        index,
        spans: Arc::from([CellSpan {
            text: format!("row {index}"),
            fg: 7,
            bg: 256,
            flags: 0,
            fg_rgb: None,
            bg_rgb: None,
            columns: 8,
            link_uri: None,
            link_key: None,
        }]),
    }
}

fn page(start: u32, count: u32) -> Vec<CellRow> {
    (start..start + count).map(row).collect()
}

fn painted(indices: &[u32]) -> PaintedHistory {
    let mut store = PaintedHistory::new();
    let rows: Vec<CellRow> = indices.iter().copied().map(row).collect();
    store.insert_page(&rows);
    store
}

#[test]
fn the_held_row_cap_is_two_thousand() {
    assert_eq!(MAX_HELD_SCROLLBACK_ROWS, 2000);
}

#[test]
fn a_page_is_admitted_only_when_it_is_exactly_the_interval_it_claims() {
    let rows = page(10, 4);
    assert!(page_is_contiguous(&rows, 10, 14));
    assert!(
        !page_is_contiguous(&rows, 10, 15),
        "short of the interval it claims"
    );
    assert!(!page_is_contiguous(&rows, 11, 14), "offset by one");
    assert!(!page_is_contiguous(&rows, 14, 10), "inverted");

    let mut out_of_order = page(10, 3);
    out_of_order.swap(0, 2);
    assert!(
        !page_is_contiguous(&out_of_order, 10, 13),
        "an out-of-order page would leave the painted set claiming a coverage it lacks"
    );
}

#[test]
fn a_page_inserted_above_the_head_lands_in_sorted_position() {
    let mut store = painted(&[100, 101, 200]);
    store.insert_page(&page(50, 2));
    assert_eq!(store.indices(), &[50, 51, 100, 101, 200]);
    assert_eq!(store.len(), 5);
    assert_eq!(store.row_at(51).map(|row| row.index), Some(51));
    assert!(store.row_at(52).is_none());
}

#[test]
fn a_reserved_gap_between_two_painted_intervals_is_a_first_class_state() {
    let store = painted(&[0, 1, 2, 50, 51]);
    let missing = store
        .missing_range_at(100, 30)
        .expect("row 30 is unpainted");
    assert_eq!((missing.start, missing.end), (3, 50));
    assert!(store.missing_range_at(100, 0).is_none(), "row 0 is painted");
    assert!(store.missing_range_at(100, 100).is_none(), "past the total");
}

#[test]
fn every_missing_interval_inside_a_requested_range_comes_back_ascending() {
    let store = painted(&[5, 9]);
    let gaps = store.missing_ranges(20, 0, 12);
    let shaped: Vec<(u32, u32)> = gaps.iter().map(|gap| (gap.start, gap.end)).collect();
    assert_eq!(shaped, vec![(0, 5), (6, 9), (10, 12)]);
}

#[test]
fn a_range_is_painted_only_when_every_row_of_it_is() {
    let store = painted(&[0, 1, 2, 5]);
    assert!(store.has_range(10, 0, 3));
    assert!(!store.has_range(10, 0, 4));
    assert!(!store.has_range(10, 3, 6), "row 3 and 4 are both unpainted");
    assert!(
        !store.has_range(10, 0, 0),
        "an empty range is never a request"
    );
}

#[test]
fn eviction_moves_the_painted_base_to_one_past_the_last_dropped_row() {
    let mut store = painted(&[0, 1, 2, 10, 11, 12]);
    // One PAST the last dropped row, not the first still-painted one: the head
    // gap an eviction leaves begins at `dropped + 1`, and it only collapses
    // while its start equals the painted base.
    assert_eq!(store.evict_leading(3), Some(3));
    assert_eq!(store.indices(), &[10, 11, 12]);
    assert_eq!(store.evict_leading(0), Some(10));
    assert!(
        store.evict_leading(9).is_none(),
        "more than the store holds"
    );
}

#[test]
fn a_store_inside_the_cap_evicts_nothing() {
    let store = painted(&[0, 1, 2, 3]);
    assert!(plan_eviction(&store, MAX_HELD_SCROLLBACK_ROWS, 250).is_none());
    assert!(
        plan_eviction(&store, 2, 0).is_none(),
        "no leading block means no step"
    );
}

#[test]
fn eviction_drops_no_more_than_the_leading_block_holds() {
    let store = painted(&[0, 1, 2, 3, 4, 5]);
    let step = plan_eviction(&store, 4, 6).expect("two rows over the cap");
    assert_eq!(step.rows, 2);
    assert_eq!(step.next_base, Some(2));
    assert!(
        !step.removes_block,
        "the block keeps the four rows that stay"
    );
}

#[test]
fn eviction_takes_the_whole_block_when_the_cap_clears_it_exactly() {
    let store = painted(&[0, 1, 2, 3]);
    let step = plan_eviction(&store, 0, 4).expect("the cap asks for all of it");
    assert_eq!(step.rows, 4);
    assert_eq!(step.next_base, Some(4));
    assert!(step.removes_block);
}

#[test]
fn eviction_never_reaches_past_the_end_of_the_leading_block() {
    let store = painted(&[0, 1, 2, 3, 4]);
    let step = plan_eviction(&store, 0, 2).expect("the cap asks for more than one block");
    assert_eq!(
        step.rows, 2,
        "one step drops one block, then the loop asks again"
    );
    assert_eq!(step.next_base, Some(2));
    assert!(step.removes_block);
}

#[test]
fn a_pre_paid_demand_exposes_the_gap_above_the_window_and_not_the_visible_one() {
    // The reader's window shows rows 100..110; the row it is about to reach is
    // 90. A demand widened upward names the interval the reader is scrolling
    // TOWARD, so the blank sliver on screen and the next several screens behind
    // it arrive in one round trip.
    let store = painted(&[89, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109]);
    let target = store
        .missing_range_at_scroll(200, 2000.0, 0.0, 200.0, 20.0, 10)
        .expect("rows 90..100 are unpainted");
    assert_eq!(target.missing.start, 90);
    assert_eq!(target.missing.end, 100);
    assert_eq!(target.focus_row, 90);
    // The window is fully painted, so the part of the demand inside it is the
    // gap's own lower edge — `in_window.end` is the WINDOW's end, not the gap's.
    assert_eq!((target.in_window.start, target.in_window.end), (90, 100));
}

#[test]
fn a_fully_painted_window_demands_nothing() {
    let store = painted(&(0..200).collect::<Vec<u32>>());
    assert!(
        store
            .missing_range_at_scroll(200, 0.0, 0.0, 200.0, 20.0, 20)
            .is_none()
    );
    assert!(
        store
            .missing_range_at_scroll(200, 0.0, 0.0, 0.0, 20.0, 20)
            .is_none()
    );
}
