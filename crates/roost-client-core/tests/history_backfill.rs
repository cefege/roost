//! Page geometry for the scrollback pager: which absolute rows one demand wave
//! asks for, and — the part that matters most — the floor it may never reach
//! under.
//!
//! The retained floor is the history-corruption tripwire named in
//! `docs/v3-gate-baselines.md`: a pager that ignores it passes every functional
//! spec and still loses scrollback, because the worker answers each request
//! short, the short answer names the same floor, and the next derivation asks
//! again. Every bound here is reproducible from its arguments alone, so no
//! renderer is needed to pin it.

use roost_client_core::terminal::history::{HistoryRange, HistoryScrollTarget};
use roost_client_core::terminal::history_backfill::{
    BACKFILL_FETCH_ROWS, DemandBounds, find_demand_bounds, scroll_demand_bounds,
};

/// A reader scroll target: the whole missing interval, plus the part of it the
/// read-ahead window exposed.
fn target(start: u32, end: u32, focus_row: u32, visible_end: u32) -> HistoryScrollTarget {
    HistoryScrollTarget {
        missing: HistoryRange { start, end },
        in_window: HistoryRange {
            start: focus_row,
            end: visible_end,
        },
        focus_row,
    }
}

fn gap(start: u32, end: u32) -> HistoryRange {
    HistoryRange { start, end }
}

#[test]
fn a_steady_scroll_up_page_ends_at_the_painted_edge_and_extends_a_page_older() {
    // Reader at 4934 with 4935+ painted: the window exposes that blank edge.
    assert_eq!(
        scroll_demand_bounds(&target(0, 4935, 4434, 4935), 0, 4935),
        Some(DemandBounds {
            focus: 4685,
            start: 4685,
            end: 4935
        })
    );
}

#[test]
fn a_deep_gap_is_bounded_to_the_page_ending_at_the_readers_newest_missing_row() {
    assert_eq!(
        scroll_demand_bounds(&target(0, 12_000, 9500, 10_001), 0, 12_000),
        Some(DemandBounds {
            focus: 9751,
            start: 9751,
            end: 10_001
        })
    );
}

#[test]
fn an_interval_whose_newer_edge_is_inside_the_window_stretches_newer_to_a_full_page() {
    // Parked on the OLDEST row of a hole that runs newer: under a page exists
    // older than the exposed edge, so the sliver is not what gets fetched.
    let bounds = scroll_demand_bounds(&target(100, 500, 100, 102), 0, 0);
    assert_eq!(
        bounds,
        Some(DemandBounds {
            focus: 100,
            start: 100,
            end: 350
        })
    );
    assert_eq!(bounds.map(|b| b.end - b.start), Some(BACKFILL_FETCH_ROWS));
}

#[test]
fn a_short_interval_yields_only_its_own_rows() {
    assert_eq!(
        scroll_demand_bounds(&target(750, 760, 750, 756), 0, 500),
        Some(DemandBounds {
            focus: 750,
            start: 750,
            end: 760
        })
    );
}

#[test]
fn a_retained_floor_clamps_the_older_edge_and_the_focus_a_demand_may_name() {
    assert_eq!(
        scroll_demand_bounds(&target(100, 200, 100, 151), 120, 0),
        Some(DemandBounds {
            focus: 120,
            start: 120,
            end: 200
        })
    );
    // A floor at the interval's newer edge leaves nothing fetchable.
    assert_eq!(
        scroll_demand_bounds(&target(100, 200, 100, 151), 200, 0),
        None
    );
}

#[test]
fn a_page_never_spans_the_painted_base_the_readers_own_side_of_it_wins() {
    // The live-stack layout that stalled the pager: head spacer [0, 171), one
    // gap element [171, 671), reader dragged to the very top. [0, 250) covers
    // two placeholders and the renderer refuses it, so the page stops at 171.
    assert_eq!(
        scroll_demand_bounds(&target(0, 671, 0, 33), 0, 171),
        Some(DemandBounds {
            focus: 0,
            start: 0,
            end: 171
        })
    );
    // Same interval, reader parked ABOVE the base: taking the head side would
    // leave its visible rows blank for a whole wave, so the gap side wins.
    assert_eq!(
        scroll_demand_bounds(&target(0, 671, 0, 201), 0, 171),
        Some(DemandBounds {
            focus: 171,
            start: 171,
            end: 421
        })
    );
}

#[test]
fn a_find_page_takes_the_base_side_its_match_row_sits_on_focus_included() {
    let below = find_demand_bounds(gap(0, 671), 100, 0, 171);
    assert_eq!(
        below,
        Some(DemandBounds {
            focus: 100,
            start: 0,
            end: 171
        })
    );
    let above = find_demand_bounds(gap(0, 671), 200, 0, 171);
    assert_eq!(
        above,
        Some(DemandBounds {
            focus: 200,
            start: 171,
            end: 421
        })
    );
    // A page the focus row is missing from would report a false success:
    // the wave answers with the focus row, and find scrolls to it.
    for bounds in [below, above].into_iter().flatten() {
        assert!(bounds.focus >= bounds.start);
        assert!(bounds.focus < bounds.end);
    }
}

#[test]
fn a_find_page_advances_forward_from_its_focus() {
    assert_eq!(
        find_demand_bounds(gap(0, 12_000), 10_000, 0, 12_000),
        Some(DemandBounds {
            focus: 10_000,
            start: 10_000,
            end: 10_250
        })
    );
    // Under a page of rows older than the match: the bounded head page instead.
    assert_eq!(
        find_demand_bounds(gap(0, 300), 100, 0, 300),
        Some(DemandBounds {
            focus: 100,
            start: 0,
            end: 250
        })
    );
}

#[test]
fn no_backfill_request_ever_names_a_row_below_the_proven_retained_floor() {
    // Every floor a worker can have proven, against every interval shape a
    // reader can expose. The invariant is not "usually true": a request under
    // the floor is a round trip whose answer is short, and the next derivation
    // asks again, so one such request is an infinite loop over dropped history.
    for floor in [0u32, 1, 7, 120, 499, 500, 671, 4_935] {
        for (start, end, focus, visible_end) in [
            (0u32, 12_000u32, 9_500u32, 10_001u32),
            (0, 671, 0, 201),
            (0, 671, 0, 33),
            (100, 200, 100, 151),
            (750, 760, 750, 756),
            (0, 5_000, 4_934, 4_935),
            (0, 300, 150, 200),
        ] {
            if let Some(bounds) =
                scroll_demand_bounds(&target(start, end, focus, visible_end), floor, 0)
            {
                assert!(
                    bounds.start >= floor,
                    "a scroll demand named rows below the proven floor: {bounds:?} under {floor}"
                );
            }
        }
        for (start, end, focus) in [(0u32, 12_000u32, 10_000u32), (0, 671, 100), (0, 300, 100)] {
            if let Some(bounds) = find_demand_bounds(gap(start, end), focus, floor, 0) {
                assert!(
                    bounds.start >= floor,
                    "a find demand named rows below the proven floor: {bounds:?} under {floor}"
                );
            }
        }
    }
}

#[test]
fn paging_stops_rather_than_retrying_forever_when_the_floor_is_reached() {
    // A reader parked at the top of a trimmed history, where the whole missing
    // interval sits UNDER the floor. Every derivation must answer "the pager owes
    // nothing", so the wave count stays at zero for ever instead of climbing.
    let floor = 500;
    let mut waves = 0u32;
    // Re-derive as a reader at the top would, twenty times over: each attempt
    // sees the same short answer, and the floor never moves.
    for _ in 0..20 {
        let exposed = target(0, 480, 0, 32);
        if let Some(bounds) = scroll_demand_bounds(&exposed, floor, 0) {
            waves += 1;
            assert!(bounds.start >= floor);
        }
    }
    assert_eq!(
        waves, 0,
        "a pager at the floor kept asking for dropped rows"
    );

    // A match below the floor is the same story: it cannot be fetched, and a
    // page asking for it comes back short with the floor it already proved.
    assert_eq!(find_demand_bounds(gap(0, 480), 100, floor, 0), None);
    // One row above the floor is still reachable, so the stop is not a blanket.
    assert!(find_demand_bounds(gap(0, 900), 500, floor, 0).is_some());
}
