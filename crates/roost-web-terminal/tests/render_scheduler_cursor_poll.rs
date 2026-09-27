//! The shared cursor-poll ticker, ported from
//! `apps/web/src/renderer/cursorPollTicker.ts` and its one caller in
//! `cell-terminal-renderer.ts`.
//!
//! The deck keeps every open session mounted, so the interval is per DOCUMENT
//! and not per pane. These cases pin the three facts that shape it: one
//! interval for all panes, a per-pane gate that makes an idle pane cost
//! nothing, and a deadline that re-anchors to now so a throttled tab cannot
//! spin.

use roost_web_terminal::scheduler::{
    CURSOR_POLL_INTERVAL_MS, CursorPollPane, CursorPollReading, CursorPollTicker, CursorPosition,
};

/// A reading from a pane that may do foreground work, at `row`/`col`.
fn reading(foreground_work_allowed: bool, row: u32, col: u32) -> CursorPollReading {
    CursorPollReading {
        foreground_work_allowed,
        cursor_row: row,
        cursor_col: col,
    }
}

#[test]
fn the_first_mounted_pane_arms_one_interval_and_every_other_pane_rides_it() {
    assert_eq!(CURSOR_POLL_INTERVAL_MS, 500);
    let mut ticker = CursorPollTicker::new();
    assert!(!ticker.is_armed());
    assert_eq!(ticker.registered_panes(), 0);

    let first = CursorPollPane::new(1);
    let second = CursorPollPane::new(2);
    assert_eq!(
        ticker.register(first, 1_000),
        Some(1_500),
        "the first registration is the only one that arms"
    );
    assert_eq!(
        ticker.register(second, 1_200),
        None,
        "a second mounted pane must not start a second interval"
    );
    assert_eq!(ticker.registered_panes(), 2);
    assert_eq!(
        ticker.due_at_ms(),
        Some(1_500),
        "and the deadline belongs to the first registration, not the newest pane"
    );
    assert_eq!(
        ticker.register(first, 1_300),
        None,
        "re-registering a mounted pane is one registration, not two"
    );
    assert_eq!(ticker.registered_panes(), 2);
}

#[test]
fn the_last_unmounted_pane_stops_the_interval_and_a_double_release_does_not() {
    let mut ticker = CursorPollTicker::new();
    let first = CursorPollPane::new(1);
    let second = CursorPollPane::new(2);
    ticker.register(first, 0);
    ticker.register(second, 0);

    assert!(!ticker.unregister(second), "a sibling pane still rides the interval");
    assert!(ticker.is_armed());
    assert!(
        !ticker.unregister(second),
        "releasing the same pane twice must not stop an interval a sibling rides"
    );
    assert!(ticker.is_armed());
    assert!(!ticker.unregister(first));
    assert!(
        !ticker.is_armed(),
        "the last pane leaving is the only release that stops the interval"
    );
    assert!(!ticker.unregister(first), "and the stop is total");
}

#[test]
fn a_credential_boundary_removes_every_pane_and_stops_the_interval() {
    let mut ticker = CursorPollTicker::new();
    ticker.register(CursorPollPane::new(1), 0);
    ticker.register(CursorPollPane::new(2), 0);
    ticker.register(CursorPollPane::new(3), 0);
    assert_eq!(ticker.registered_panes(), 3);

    assert!(ticker.reset(), "a credential boundary stops the interval it owned");
    assert_eq!(ticker.registered_panes(), 0);
    assert!(!ticker.is_armed());
    assert!(!ticker.reset(), "and there is nothing left to stop");
    assert_eq!(
        ticker.poll(CursorPollPane::new(1), reading(true, 4, 4)),
        None,
        "no mounted pane survives the boundary, so no cursor position may be published"
    );
}

#[test]
fn a_poll_reports_a_moved_cursor_once_and_nothing_while_it_stays_put() {
    let mut ticker = CursorPollTicker::new();
    let pane = CursorPollPane::new(1);
    ticker.register(pane, 0);

    assert_eq!(
        ticker.poll(pane, reading(true, 0, 0)),
        Some(CursorPosition { row: 0, col: 0 }),
        "the first poll always reports, even for a cursor sitting at the origin"
    );
    assert_eq!(ticker.poll(pane, reading(true, 0, 0)), None);
    assert_eq!(
        ticker.poll(pane, reading(true, 3, 4)),
        Some(CursorPosition { row: 3, col: 4 })
    );
    assert_eq!(ticker.poll(pane, reading(true, 3, 4)), None);
    assert_eq!(
        ticker.poll(pane, reading(true, 3, 5)),
        Some(CursorPosition { row: 3, col: 5 }),
        "a move in either axis is a move"
    );
    assert_eq!(
        ticker.poll(CursorPollPane::new(99), reading(true, 1, 1)),
        None,
        "a pane that is not mounted is not polled"
    );
}

#[test]
fn a_pane_that_may_not_do_foreground_work_keeps_its_reported_position() {
    let mut ticker = CursorPollTicker::new();
    let pane = CursorPollPane::new(1);
    ticker.register(pane, 0);

    assert_eq!(ticker.poll(pane, reading(false, 3, 4)), None);
    assert_eq!(
        ticker.poll(pane, reading(true, 3, 4)),
        Some(CursorPosition { row: 3, col: 4 }),
        "a pane returning to the foreground reports where its cursor actually is"
    );
    assert_eq!(ticker.poll(pane, reading(false, 9, 9)), None);
    assert_eq!(
        ticker.poll(pane, reading(true, 3, 4)),
        None,
        "and a hidden pane that moved does not make the return re-report a position \
         it already published"
    );
}

#[test]
fn a_throttled_timer_re_arms_once_from_now_rather_than_once_per_missed_interval() {
    let mut ticker = CursorPollTicker::new();
    ticker.register(CursorPollPane::new(1), 0);
    assert_eq!(ticker.due_at_ms(), Some(CURSOR_POLL_INTERVAL_MS));

    assert!(!ticker.take_due(CURSOR_POLL_INTERVAL_MS - 1));
    assert!(
        ticker.take_due(CURSOR_POLL_INTERVAL_MS),
        "the deadline is inclusive of its own millisecond"
    );
    assert_eq!(ticker.due_at_ms(), Some(2 * CURSOR_POLL_INTERVAL_MS));

    // A background tab slept through three intervals. One tick consumes all of
    // them and the next deadline is anchored to now, so the ticker cannot spin
    // to "catch up" on every interval it missed.
    assert!(ticker.take_due(9_000));
    assert_eq!(ticker.due_at_ms(), Some(9_500));
    assert!(!ticker.take_due(9_999));
    assert!(ticker.take_due(9_500 + CURSOR_POLL_INTERVAL_MS));
    assert_eq!(ticker.due_at_ms(), Some(9_500 + 2 * CURSOR_POLL_INTERVAL_MS));
}
