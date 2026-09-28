//! The pager against arbitrary gaps: which rows a wave asks for, what it
//! splices, the retained floor a short page proves, and every fence that stops
//! a stale page from painting. Ports "ScrollbackBackfill arbitrary gap paging"
//! from `apps/web/tests/renderer/scrollbackBackfill.test.ts`; the renderer is
//! the `backfill_support` stand-in with absolute painted rows.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod backfill_support;

use backfill_support::{GRID_EPOCH, Harness, Options, Reply, interior_painted, response};
use roost_protocol::terminal_search::ScrollbackHistoryFloor;
use roost_web_terminal::backfill::ScrollbackPageRequest;

fn tail_painted() -> Vec<u32> {
    (500..750).collect()
}

#[test]
fn fills_a_bounded_short_tail_gap_from_its_reader_focus() {
    let mut h = Harness::new(Options {
        total: Some(760),
        painted: tail_painted(),
        focus: Some(755),
        ..Options::default()
    });
    h.respond_with(|request| Reply::Page(response(750, request.end_row, 760)));
    h.scroll();
    assert_eq!(h.host.insertions[0], (750..760).collect::<Vec<_>>());
    assert!(h.host.painted.contains(&755));
}

#[test]
fn pages_head_and_interior_gaps_without_inferring_coverage_from_sb_base() {
    let mut head = Harness::new(Options {
        total: Some(300),
        focus: Some(50),
        ..Options::default()
    });
    head.respond_with(|request| Reply::Page(response(0, request.end_row, 300)));
    head.scroll();
    assert_eq!(head.requested()[0], (250, 250));

    let mut interior = Harness::new(Options {
        total: Some(300),
        painted: interior_painted(),
        focus: Some(150),
        ..Options::default()
    });
    interior.respond_with(|request| Reply::Page(response(100, request.end_row, 300)));
    interior.scroll();
    assert_eq!(
        interior.calls[0],
        ScrollbackPageRequest {
            session_id: "session-1".to_string(),
            end_row: 200,
            max_rows: 100,
            grid_epoch: GRID_EPOCH.to_string(),
        }
    );
    assert!(interior.host.painted.contains(&150));
}

#[test]
fn a_short_interior_response_reports_its_actual_retained_floor_row_and_clears_it() {
    let mut h = Harness::new(Options {
        total: Some(300),
        painted: interior_painted(),
        focus: Some(150),
        ..Options::default()
    });
    h.respond_with(|request| {
        let mut page = response(120, request.end_row, 300);
        page.history_floor = ScrollbackHistoryFloor::Evicted;
        Reply::Page(page)
    });
    h.scroll();
    assert_eq!(
        h.pager.history_floor(),
        Some((120, &ScrollbackHistoryFloor::Evicted))
    );
    // The surviving suffix paints and the pager parks at the floor: the one
    // read it made is the only read, so no impossible row is re-requested.
    assert!(h.host.painted.contains(&150));
    assert!(!h.host.painted.contains(&119));
    assert_eq!(h.calls.len(), 1);

    h.host.anchor.grid_epoch = "test-grid:1".to_string();
    h.full_frame();
    assert_eq!(h.pager.history_floor(), None);
    assert_eq!(h.host.floor_rows, vec![120, 0]);
}

#[test]
fn find_supersedes_an_obsolete_scroll_response_before_it_can_paint() {
    let mut h = Harness::new(Options {
        total: Some(760),
        painted: tail_painted(),
        focus: Some(755),
        ..Options::default()
    });
    let mut calls = 0;
    h.respond_with(move |request| {
        calls += 1;
        if calls == 1 {
            Reply::Pending
        } else {
            Reply::Page(response(0, request.end_row, 760))
        }
    });
    h.scroll();
    h.ensure_row_painted(100);
    h.resolve_oldest(response(750, 756, 760));
    assert_eq!(h.find_results, vec![(100, true)]);
    assert!(!h.host.painted.contains(&755));
    assert!(h.host.painted.contains(&100));
}

#[test]
fn rejects_a_page_addressed_to_another_grid_epoch() {
    let mut h = Harness::new(Options {
        total: Some(300),
        focus: Some(100),
        ..Options::default()
    });
    h.respond_with(|request| {
        let mut page = response(0, request.end_row, 300);
        page.grid_epoch = "other:0".to_string();
        Reply::Page(page)
    });
    h.scroll();
    assert!(h.host.insertions.is_empty());
    let rejected = h.events_named("scrollback.backfill_rejected");
    assert!(
        rejected
            .iter()
            .any(|event| event.fields.get("guard").map(String::as_str) == Some("epoch"))
    );
}

#[test]
fn cancels_suspended_or_rewound_work_and_accepts_monotonic_growth() {
    let mut cancelled = Harness::new(Options {
        total: Some(300),
        focus: Some(100),
        ..Options::default()
    });
    cancelled.respond_with(|_| Reply::Pending);
    cancelled.scroll();
    cancelled.suspend();
    cancelled.resolve_oldest(response(0, 250, 300));
    assert!(cancelled.host.insertions.is_empty());

    let mut rewound = Harness::new(Options {
        total: Some(300),
        focus: Some(100),
        ..Options::default()
    });
    rewound.respond_with(|_| Reply::Pending);
    rewound.scroll();
    rewound.host.anchor.total = 99;
    rewound.full_frame();
    rewound.resolve_oldest(response(0, 250, 300));
    assert!(rewound.host.insertions.is_empty());

    let mut growing = Harness::new(Options {
        total: Some(300),
        focus: Some(100),
        ..Options::default()
    });
    growing.respond_with(|_| Reply::Pending);
    growing.scroll();
    growing.host.anchor.total = 360;
    growing.resolve_oldest(response(0, 250, 360));
    assert!(growing.host.painted.contains(&100));
}

#[test]
fn full_frames_do_not_prefetch_and_ensure_row_painted_resolves_only_after_insertion() {
    let mut h = Harness::new(Options {
        total: Some(300),
        focus: Some(100),
        ..Options::default()
    });
    h.full_frame();
    assert!(h.calls.is_empty());

    h.respond_with(|_| Reply::Pending);
    h.ensure_row_painted(100);
    assert_eq!(h.calls.len(), 1);
    assert!(
        h.find_results.is_empty(),
        "the reveal answered before its page landed"
    );
    h.resolve_oldest(response(0, 250, 300));
    assert_eq!(h.find_results, vec![(100, true)]);
    assert!(h.host.insertions.iter().flatten().any(|row| *row == 100));
}

#[test]
fn a_failed_read_is_retried_once_on_the_cadence_and_then_the_wave_ends() {
    // No reader focus, so the settle owes no scroll demand and only the find
    // wave's own reads are counted.
    let mut h = Harness::new(Options {
        total: Some(300),
        ..Options::default()
    });
    h.respond_with(|_| Reply::Fail);
    h.ensure_row_painted(100);
    assert_eq!(h.calls.len(), 1);
    h.advance(roost_web_terminal::backfill::BACKFILL_RETRY_MS - 1);
    assert_eq!(h.calls.len(), 1);
    assert!(h.find_results.is_empty());
    h.advance(1);
    assert_eq!(h.calls.len(), 2, "the one retry reads the same page again");
    assert_eq!(h.calls[0], h.calls[1]);
    assert_eq!(
        h.find_results,
        vec![(100, false)],
        "a second failure ends the wave"
    );
    assert_eq!(h.pager.request_count(), 2);
}
