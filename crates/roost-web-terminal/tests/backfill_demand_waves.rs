//! One wave at a time: read-ahead pre-payment, coalescing a gesture into the
//! wave in flight, the bounded identical-retry budget and the `BACKFILL_RETRY_MS`
//! cadence after it, and the one-placeholder page. Ports the demand cases of
//! `apps/web/tests/renderer/scrollbackBackfill.test.ts` named as the Guard of
//! FAILURE-INDEX "A demand page is only issued once the rows are already blank".

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod backfill_support;

use backfill_support::capture::fields;
use backfill_support::{Harness, Options, Reply, response};
use roost_client_core::terminal::history_backfill::BACKFILL_FETCH_ROWS;
use roost_web_terminal::SCROLLBACK_BLOCK_ROWS;
use roost_web_terminal::backfill::BACKFILL_RETRY_MS;

/// A reader at row 600 of an unpainted 1000-row history, answered with a page
/// reaching back to row 100: the unpainted prefix can never splice.
fn unspliceable(session_id: &'static str) -> Harness {
    let mut h = Harness::new(Options { total: Some(1000), focus: Some(600), session_id: Some(session_id), ..Options::default() });
    h.respond_with(|request| Reply::Page(response(100, request.end_row, 1000)));
    h
}

#[test]
fn a_fetch_page_is_one_sealed_history_block() {
    assert_eq!(BACKFILL_FETCH_ROWS, SCROLLBACK_BLOCK_ROWS);
}

#[test]
fn a_wheel_step_pre_pays_the_rows_above_the_viewport_and_the_next_step_is_free() {
    let mut h = Harness::new(Options { total: Some(5000), painted: (4935..5000).collect(), focus: Some(4934), ..Options::default() });
    h.respond_with(|request| Reply::Page(response(request.end_row - request.max_rows, request.end_row, 5000)));
    h.scroll();
    // The page ends at the blank edge the reader exposed and extends older, so
    // one round trip serves the viewport plus the rows it is scrolling toward.
    assert_eq!(h.requested(), vec![(4935, 250), (4685, 250), (4435, 250)]);

    h.host.focus = Some(4900);
    h.scroll();
    assert_eq!(h.calls.len(), 3, "the identical next wheel step is already painted");

    h.host.focus = Some(4600);
    h.scroll();
    assert_eq!(h.requested()[3], (4185, 250));
}

#[test]
fn scrolls_during_a_wave_add_no_request_one_coalesce_line_and_one_demand_after() {
    let mut h = Harness::new(Options { total: Some(1000), focus: Some(900), ..Options::default() });
    h.respond_with(|_| Reply::Pending);
    h.scroll();
    h.host.focus = Some(300);
    for _ in 0..3 {
        h.scroll();
    }
    assert_eq!(h.requested(), vec![(901, 250)]);

    h.resolve_oldest(response(651, 901, 1000));
    assert_eq!(h.requested(), vec![(901, 250), (301, 250)]);

    for _ in 0..3 {
        h.scroll();
    }
    // The owed edge reports once per wave; a fling raises ~60 events a second.
    let coalesced = h.events_named("scrollback.demand_coalesced");
    assert_eq!(coalesced.len(), 1);
    assert_eq!(
        coalesced[0].fields,
        fields(&[("sid", "session-1"), ("kind", "scroll"), ("focus", "51"), ("start", "51"), ("end", "301")])
    );
}

#[test]
fn a_page_that_cannot_splice_retries_bounded_and_stays_armed_for_the_reader() {
    let mut h = unspliceable("session-1");
    h.scroll();
    // The identical demand stops after its budget until the retry interval
    // elapses or the reader moves, whichever is first.
    assert_eq!(h.calls.iter().map(|call| call.end_row).collect::<Vec<_>>(), vec![601, 601, 601]);
    assert!(h.host.insertions.is_empty());

    h.scroll();
    assert_eq!(h.calls.len(), 6);
}

#[test]
fn an_exhausted_identical_retry_budget_defers_one_rearm_instead_of_abandoning_the_gap() {
    let mut h = unspliceable("session-1");
    h.scroll();
    let demands: Vec<_> = h
        .events()
        .into_iter()
        .filter(|event| event.message.starts_with("scrollback.demand_"))
        .collect();
    let names: Vec<&str> = demands.iter().map(|event| event.message.as_str()).collect();
    assert_eq!(names, ["scrollback.demand_rearmed", "scrollback.demand_rearmed", "scrollback.demand_retry_deferred"]);
    let delay = BACKFILL_RETRY_MS.to_string();
    assert_eq!(
        demands[2].fields,
        fields(&[
            ("sid", "session-1"), ("focus", "351"), ("start", "351"), ("end", "601"),
            ("retries", "2"), ("delay_ms", delay.as_str()),
        ])
    );
}

#[test]
fn the_readers_own_rows_paint_when_one_page_would_span_the_painted_base() {
    // Head spacer [0, 171), one gap element [171, 671), nothing painted, and a
    // scrollbar drag to the very top that raises ONE scroll event.
    let mut h = Harness::new(Options { total: Some(671), painted_base: Some(171), focus: Some(0), ..Options::default() });
    h.respond_with(|request| Reply::Page(response(request.end_row - request.max_rows, request.end_row, 671)));
    h.scroll();
    // [0, 250) would cover the head spacer AND the gap above it, which the
    // renderer refuses, so the page stops at the base and the reader sees rows.
    assert_eq!(h.requested(), vec![(171, 171)]);
    assert_eq!(h.host.insertions.concat(), (0..171).collect::<Vec<_>>());

    h.host.focus = Some(200);
    h.scroll();
    assert_eq!(h.requested(), vec![(171, 171), (421, 250)]);
    assert!(h.host.painted.contains(&200));
}

#[test]
fn a_spent_budget_keeps_re_deriving_on_the_retry_cadence_one_wave_per_interval() {
    let mut h = unspliceable("session-1");
    h.scroll();
    assert_eq!(h.calls.len(), 3);

    h.advance(BACKFILL_RETRY_MS - 1);
    assert_eq!(h.calls.len(), 3);
    h.advance(1);
    // The reader parked and never scrolled again: the pager, not the next
    // gesture, re-derives the rows it owes.
    assert_eq!(h.calls.len(), 4);

    for _ in 0..3 {
        h.advance(BACKFILL_RETRY_MS);
    }
    assert_eq!(h.calls.len(), 7);
    assert!(h.calls.iter().all(|call| call.end_row == 601));
    let woke = h.events_named("scrollback.demand_retry_woke");
    assert_eq!(woke.len(), 4);
    let expected = fields(&[("sid", "session-1"), ("focus", "351"), ("start", "351"), ("end", "601"), ("armed", "true")]);
    assert!(woke.iter().all(|event| event.fields == expected), "{woke:?}");
}

#[test]
fn suspend_and_dispose_cancel_the_deferred_rearm() {
    let mut suspended = unspliceable("suspended");
    let mut disposed = unspliceable("disposed");
    suspended.scroll();
    disposed.scroll();
    assert_eq!(suspended.calls.len() + disposed.calls.len(), 6);

    suspended.suspend();
    disposed.dispose();
    suspended.advance(BACKFILL_RETRY_MS * 5);
    disposed.advance(BACKFILL_RETRY_MS * 5);
    assert_eq!(suspended.calls.len() + disposed.calls.len(), 6);
}

#[test]
fn a_scroll_event_rearms_at_once_and_supersedes_the_pending_rearm() {
    let mut h = unspliceable("session-1");
    h.scroll();
    assert_eq!(h.calls.len(), 3);
    h.scroll();
    assert_eq!(h.calls.len(), 6);
    // Seven, not eight: the gesture's own chain deferred exactly one wake and
    // the one it replaced is gone.
    h.advance(BACKFILL_RETRY_MS);
    assert_eq!(h.calls.len(), 7);
}
