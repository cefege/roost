//! Cell-history interval arithmetic, independent of any DOM: sorted absolute
//! painted rows in, exact half-open missing intervals out. Gap paging depends
//! on these boundaries staying exact. Ports
//! `apps/web/tests/client/terminal-stream/cellHistoryRanges.test.ts`.

use roost_client_core::terminal::history::{
    HistoryRange, HistoryScrollTarget, has_history_range, has_sorted_history_rows, insertion_index,
    is_contiguous_page, missing_range_at, missing_range_at_scroll, missing_ranges,
};

fn range(start: u32, end: u32) -> HistoryRange {
    HistoryRange { start, end }
}

/// The scroll box v2 pins: 30px tall, 10px rows, no spacer above the rows.
fn target_at(rows: &[u32], scroll_top: f64, ahead_rows: u32) -> Option<HistoryScrollTarget> {
    missing_range_at_scroll(rows, 100, scroll_top, 0.0, 30.0, 10.0, ahead_rows)
}

fn exposed(start: u32, end: u32, focus_row: u32, visible_end: u32) -> Option<HistoryScrollTarget> {
    Some(HistoryScrollTarget {
        missing: range(start, end),
        in_window: range(focus_row, visible_end),
        focus_row,
    })
}

#[test]
fn finds_sorted_head_interior_and_tail_gaps() {
    let rows = [2, 3, 6, 7];
    assert!(has_sorted_history_rows(&rows, 10));
    assert_eq!(missing_range_at(&rows, 10, 0), Some(range(0, 2)));
    assert_eq!(missing_range_at(&rows, 10, 4), Some(range(4, 6)));
    assert_eq!(missing_range_at(&rows, 10, 8), Some(range(8, 10)));
    assert_eq!(
        missing_ranges(&rows, 10, 1, 9),
        vec![range(1, 2), range(4, 6), range(8, 9)]
    );
}

#[test]
fn requires_exact_nonempty_coverage_and_contiguous_insertions() {
    let rows = [2, 3, 6, 7];
    assert!(has_history_range(&rows, 10, 2, 4));
    assert!(!has_history_range(&rows, 10, 2, 5));
    assert!(has_history_range(&rows, 10, 6, 8));
    assert!(!has_history_range(&rows, 10, 10, 10));
    assert!(is_contiguous_page(&[6, 7], 6, 8));
    assert!(!is_contiguous_page(&[6, 8], 6, 8));
    assert_eq!(insertion_index(&rows, 6), 2);
}

#[test]
fn read_ahead_widens_the_scroll_window_upward_nearest_missing_gap_first() {
    let painted: Vec<u32> = (50..100).collect();
    // Viewport [60, 63) is painted: only read-ahead can see the gap above it.
    assert_eq!(target_at(&painted, 600.0, 0), None);
    assert_eq!(target_at(&painted, 600.0, 20), exposed(0, 50, 40, 50));

    // A gap reaching the viewport keeps its visible end inside the viewport, so
    // the page answering it still covers the rows the reader is staring at.
    let straddle: Vec<u32> = (0..30).chain(60..100).collect();
    assert_eq!(target_at(&straddle, 550.0, 0), exposed(30, 60, 55, 58));
    assert_eq!(target_at(&straddle, 550.0, 20), exposed(30, 60, 35, 58));

    // Disjoint gaps inside the widened window: the nearest one wins.
    let disjoint: Vec<u32> = (0..10).chain(20..30).chain(40..100).collect();
    assert_eq!(target_at(&disjoint, 500.0, 40), exposed(30, 40, 30, 40));
}
